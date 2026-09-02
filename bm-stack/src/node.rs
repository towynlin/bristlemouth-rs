//! The node: what to say, when to say it, and what to do with what arrives.
//!
//! The interesting half is synchronous. [`Node::on_frame`] and
//! [`Node::on_tick`] take the current time, mutate the node's state, and hand
//! back at most one frame to transmit — no futures, no PHY, no allocator. That
//! is what makes the behaviour testable without an executor, and it is where
//! all of the protocol lives.
//!
//! [`Node::run`] is the thin part on top: it waits on the PHY or the heartbeat
//! ticker, calls one of the two, and transmits whatever came back.

use bm_wire::addr;
use bm_wire::bcmp::info::{DeviceInfoReply, DeviceInfoRequest};
use bm_wire::bcmp::neighbors::{NeighborTableRequest, PortInfo, encode_neighbor_table_reply};
use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, Heartbeat, MessageType, rx, tx};
use bm_wire::frame::{
    ETHERNET_DESTINATION_OFFSET, ETHERNET_SRC_OFFSET, ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET,
    IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_HOP_LIMIT_OFFSET, IPV6_NEXT_HEADER_OFFSET,
    IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET, MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::l2::{self, TxKind};
use bm_wire::neighbor::{HEARTBEAT_PERIOD_S, NeighborTable, heartbeat_for};
use bm_wire::util::BmIpAddr;
use bm_wire::{BmWireError, addr::MAC_LEN};

use crate::port::{Egress, Identity, Phy};

/// Largest frame the node will build or accept.
///
/// 1514 is a 1500-byte Ethernet payload plus the 14-byte header, which is what
/// `bcmp_max_payload_size_bytes` in `bcmp/bcmp.h` works out to.
pub const MTU: usize = 1514;

/// The IPv6 prefix bm_core builds a node's link-local address from.
pub const LINK_LOCAL_PREFIX: u32 = 0xFE80_0000;

/// The hop limit bm_core sets on everything it transmits.
pub const HOP_LIMIT: u8 = 64;

/// A frame the node wants transmitted, borrowed from its transmit buffer.
///
/// Mutable because stamping the egress port rewrites it, once per port.
#[derive(Debug)]
pub struct Outbound<'a> {
    frame: &'a mut [u8],
}

impl Outbound<'_> {
    /// The frame as it stands, before any egress port is stamped into it.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        self.frame
    }
}

/// A Bristlemouth node.
///
/// `NEIGHBORS` is the neighbour-table capacity; it needs to be at least the
/// PHY's port count, since bm_core keeps one neighbour per port.
pub struct Node<I, const NEIGHBORS: usize = 4> {
    identity: I,
    neighbors: NeighborTable<NEIGHBORS>,
    port_count: u8,
    /// Link state per port, bit 0 for port 1. Cached rather than read from the
    /// PHY on demand, so the synchronous half stays free of I/O — the same
    /// arrangement bm_core has, where L2 keeps `enabled_ports_mask` up to date
    /// from link-change callbacks and `bm_l2_get_port_state` only reads it.
    link_mask: u16,
    tx: [u8; MTU],
}

impl<I: Identity, const NEIGHBORS: usize> Node<I, NEIGHBORS> {
    /// A node with an empty neighbour table.
    pub fn new(identity: I, port_count: u8) -> Self {
        Self {
            identity,
            neighbors: NeighborTable::new(),
            port_count,
            link_mask: 0,
            tx: [0u8; MTU],
        }
    }

    /// Record that `port` came up or went down. Ports are 1-based.
    ///
    /// [`Node::run`] does this from the PHY; a caller driving the synchronous
    /// half itself has to keep it current.
    pub fn set_link_up(&mut self, port: u8, up: bool) {
        let Some(bit) = port.checked_sub(1).filter(|b| *b < 16) else {
            return;
        };
        if up {
            self.link_mask |= 1 << bit;
        } else {
            self.link_mask &= !(1 << bit);
        }
    }

    /// Whether `port` is up, as last recorded.
    #[must_use]
    pub fn link_up(&self, port: u8) -> bool {
        port.checked_sub(1)
            .is_some_and(|bit| bit < 16 && self.link_mask & (1 << bit) != 0)
    }

    /// This node's identity.
    pub fn identity(&self) -> &I {
        &self.identity
    }

    /// The neighbours seen so far.
    pub fn neighbors(&self) -> &NeighborTable<NEIGHBORS> {
        &self.neighbors
    }

    /// This node's link-local address, which is the source of everything it
    /// sends.
    #[must_use]
    pub fn link_local(&self) -> BmIpAddr {
        addr::nodeid_to_ip(LINK_LOCAL_PREFIX, self.identity.node_id())
    }

    /// Handle a received frame, returning a reply to transmit if one is owed.
    ///
    /// `frame` is mutated in place, as bm_core mutates it: the receive path
    /// rewrites three bytes of the source address before it checksums.
    /// Anything that does not validate as BCMP is dropped silently, which is
    /// also what bm_core does.
    pub fn on_frame(
        &mut self,
        now_ms: u32,
        ingress_port: u8,
        frame: &mut [u8],
    ) -> Option<Outbound<'_>> {
        let received = rx::accept(frame).ok()?;
        let message_type = received.header.message_type;
        let source = received.src.to_node_id();
        let reply_to = received.dst;

        match message_type {
            MessageType::HEARTBEAT => {
                let heartbeat = Heartbeat::decode(received.payload).ok()?;
                let outcome = self
                    .neighbors
                    .on_heartbeat(now_ms, source, ingress_port, &heartbeat);
                if !outcome.request_info {
                    return None;
                }
                // bm_core asks on the link-local multicast address rather than
                // the one the heartbeat arrived on.
                self.build_device_info_request(source)
            }
            MessageType::DEVICE_INFO_REQUEST => {
                let request = DeviceInfoRequest::decode(received.payload).ok()?;
                if !self.addressed_to_us(request.target_node_id) {
                    return None;
                }
                self.build_device_info_reply(&reply_to)
            }
            MessageType::NEIGHBOR_TABLE_REQUEST => {
                let request = NeighborTableRequest::decode(received.payload).ok()?;
                if !self.addressed_to_us(request.target_node_id) {
                    return None;
                }
                self.build_neighbor_table_reply(&reply_to)
            }
            _ => None,
        }
    }

    /// Handle the periodic tick: age the neighbour table, then emit a heartbeat.
    ///
    /// `uptime_ms` is milliseconds since this node started, which is also the
    /// clock [`Self::on_frame`] is given.
    pub fn on_tick(&mut self, uptime_ms: u32) -> Option<Outbound<'_>> {
        // bm_core checks neighbours and sends a heartbeat on the same timer,
        // in that order.
        self.neighbors.check(uptime_ms, |_| {});
        self.build_heartbeat(uptime_ms)
    }

    fn addressed_to_us(&self, target_node_id: u64) -> bool {
        target_node_id == 0 || target_node_id == self.identity.node_id()
    }

    fn build_heartbeat(&mut self, uptime_ms: u32) -> Option<Outbound<'_>> {
        let Self { identity, tx, .. } = self;
        let heartbeat = heartbeat_for(uptime_ms, HEARTBEAT_PERIOD_S);
        let end = build_frame(
            tx,
            identity.node_id(),
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            |body| {
                heartbeat.encode(body)?;
                Ok(Heartbeat::LEN)
            },
        )?;
        Some(Outbound {
            frame: &mut tx[..end],
        })
    }

    fn build_device_info_request(&mut self, target_node_id: u64) -> Option<Outbound<'_>> {
        let Self { identity, tx, .. } = self;
        let end = build_frame(
            tx,
            identity.node_id(),
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::DEVICE_INFO_REQUEST,
            |body| {
                DeviceInfoRequest { target_node_id }.encode(body)?;
                Ok(DeviceInfoRequest::LEN)
            },
        )?;
        Some(Outbound {
            frame: &mut tx[..end],
        })
    }

    fn build_device_info_reply(&mut self, dst: &BmIpAddr) -> Option<Outbound<'_>> {
        let Self { identity, tx, .. } = self;
        let node_id = identity.node_id();
        let mut info = identity.device_info();
        info.node_id = node_id;
        // Each length field is a single byte. bm_core assigns `strlen()` to a
        // `uint8_t`, which wraps -- a 300-byte name becomes 44 bytes of it.
        // Truncating is the same size ceiling without the surprise.
        let version = identity.version_string();
        let name = identity.device_name();
        let reply = DeviceInfoReply {
            info,
            version_string: &version[..version.len().min(DeviceInfoReply::MAX_STRING_LEN)],
            device_name: &name[..name.len().min(DeviceInfoReply::MAX_STRING_LEN)],
        };
        let end = build_frame(tx, node_id, dst, MessageType::DEVICE_INFO_REPLY, |body| {
            reply.encode(body)
        })?;
        Some(Outbound {
            frame: &mut tx[..end],
        })
    }

    fn build_neighbor_table_reply(&mut self, dst: &BmIpAddr) -> Option<Outbound<'_>> {
        let Self {
            identity,
            neighbors,
            port_count,
            link_mask,
            tx,
        } = self;
        let node_id = identity.node_id();

        // bm_core reports every port with the link state it has and a type it
        // never fills in.
        let mut ports = [PortInfo::default(); MAX_REPORTED_PORTS];
        let ports = &mut ports[..usize::from(*port_count).min(MAX_REPORTED_PORTS)];
        for (index, port) in ports.iter_mut().enumerate() {
            port.state = u8::from(*link_mask & (1 << index) != 0);
        }

        let mut table = [bm_wire::bcmp::NeighborInfo::default(); NEIGHBORS];
        let mut count = 0;
        for neighbor in neighbors.neighbors() {
            table[count] = bm_wire::bcmp::NeighborInfo {
                node_id: neighbor.node_id,
                port: neighbor.port,
                online: u8::from(neighbor.online),
            };
            count += 1;
        }

        let end = build_frame(
            tx,
            node_id,
            dst,
            MessageType::NEIGHBOR_TABLE_REPLY,
            |body| encode_neighbor_table_reply(body, node_id, ports, &table[..count]),
        )?;
        Some(Outbound {
            frame: &mut tx[..end],
        })
    }
}

/// Most ports a neighbour-table reply will describe.
///
/// The port field in the reply is a `u8`, and no Bristlemouth device has more
/// than a handful; this is only the size of a stack array.
const MAX_REPORTED_PORTS: usize = 16;

/// Write the Ethernet and IPv6 headers into `tx`, let `body` fill the BCMP
/// payload, then put the BCMP header and checksum around it.
///
/// Returns the total frame length. The body is written straight into the
/// transmit buffer rather than into a second one, which is what
/// `bm_wire::bcmp::tx::serialize_in_place` is for.
///
/// Byte for byte what `bm_ip_tx_new` and `bm_ip_tx_perform` in
/// `network/bm_linux.c` produce.
fn build_frame<F>(
    tx: &mut [u8],
    node_id: u64,
    dst: &BmIpAddr,
    message_type: MessageType,
    body: F,
) -> Option<usize>
where
    F: FnOnce(&mut [u8]) -> Result<usize, BmWireError>,
{
    let src = addr::nodeid_to_ip(LINK_LOCAL_PREFIX, node_id);

    // Ethernet. bm_core broadcasts anything not multicast, having no neighbour
    // discovery to resolve a unicast address with.
    let dst_mac = if addr::is_multicast(dst) {
        addr::multicast_mac_from_ipv6(dst)
    } else {
        [0xFF; MAC_LEN]
    };
    tx[ETHERNET_DESTINATION_OFFSET..ETHERNET_DESTINATION_OFFSET + MAC_LEN]
        .copy_from_slice(&dst_mac);
    tx[ETHERNET_SRC_OFFSET..ETHERNET_SRC_OFFSET + MAC_LEN]
        .copy_from_slice(&addr::mac_from_nodeid(node_id));
    tx[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());

    // IPv6: version 6, no traffic class, no flow label.
    tx[IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET] = 0x60;
    tx[IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET + 1] = 0x00;
    tx[IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET + 2] = 0x00;
    tx[IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET + 3] = 0x00;
    tx[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
    tx[IPV6_HOP_LIMIT_OFFSET] = HOP_LIMIT;
    tx[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16].copy_from_slice(&src.0);
    tx[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
        .copy_from_slice(&dst.0);

    let body_at = BCMP_HEADER_OFFSET + BCMP_HEADER_LEN;
    let body_len = body(tx.get_mut(body_at..)?).ok()?;
    let payload_len = BCMP_HEADER_LEN + body_len;
    tx[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
        .copy_from_slice(&(payload_len as u16).to_be_bytes());

    let end = MIN_FRAME_WITH_ADDRESSES + payload_len;
    tx::serialize_in_place(tx.get_mut(..end)?, message_type, 0, body_len).ok()?;
    Some(end)
}

/// Transmit one frame, stamping the egress port into each copy that needs it.
///
/// This is `bm_l2_process_tx_evt`: global multicast goes out unstamped, to
/// every port at once; link-local multicast goes out once per port with that
/// port stamped into the source address and the checksum patched to match;
/// anything else is dropped, as bm_core drops it.
///
/// # Errors
///
/// Whatever the PHY returns. A failure on one port abandons the rest.
pub async fn transmit<P: Phy>(
    phy: &mut P,
    outbound: Outbound<'_>,
    port_count: u8,
) -> Result<(), P::Error> {
    let frame = outbound.frame;
    match l2::tx_kind(frame) {
        TxKind::GlobalMulticast => phy.send(frame, Egress::AllPorts).await,
        TxKind::LinkLocalMulticast => {
            for port in 1..=port_count {
                // The stamp is undone when it goes out of scope, so the next
                // port starts from a clean frame.
                let Ok(stamped) = l2::stamp_egress_port(frame, port) else {
                    return Ok(());
                };
                phy.send(&stamped, Egress::Port(port)).await?;
            }
            Ok(())
        }
        TxKind::Dropped => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// The async loop
// ---------------------------------------------------------------------------

impl<I: Identity, const NEIGHBORS: usize> Node<I, NEIGHBORS> {
    /// Run the node until the PHY fails.
    ///
    /// Waits on whichever comes first, a frame or the heartbeat tick, handles
    /// it, and transmits anything owed. The heartbeat interval is bm_core's
    /// `bcmp_heartbeat_s`, and the tick also ages the neighbour table, in that
    /// order, as bm_core does on the same timer.
    ///
    /// Returns rather than panicking when the PHY errors, so the caller can
    /// decide whether that is fatal. It has no other exit.
    ///
    /// # Errors
    ///
    /// The first error the PHY reports, from either direction.
    pub async fn run<P: Phy>(&mut self, phy: &mut P) -> P::Error {
        use embassy_futures::select::{Either, select};
        use embassy_time::{Duration, Instant, Ticker};

        let started = Instant::now();
        let mut ticker = Ticker::every(Duration::from_secs(u64::from(HEARTBEAT_PERIOD_S)));
        let mut rx = [0u8; MTU];
        let port_count = self.port_count;

        loop {
            // Cheap: the driver keeps this as an array it updates when it
            // services a PHY interrupt, so this is a read, not a transfer.
            for port in 1..=port_count {
                let up = phy.link_up(port);
                self.set_link_up(port, up);
            }

            let uptime_ms = |()| -> u32 {
                // Wraps at 49.7 days, which is what bm_core's tick counter
                // does too; `time_remaining` is written to survive it.
                started.elapsed().as_millis() as u32
            };

            match select(phy.receive(&mut rx), ticker.next()).await {
                Either::First(Ok((port, len))) => {
                    let now = uptime_ms(());
                    if let Some(outbound) = self.on_frame(now, port, &mut rx[..len])
                        && let Err(error) = transmit(phy, outbound, port_count).await
                    {
                        return error;
                    }
                }
                Either::First(Err(error)) => return error,
                Either::Second(()) => {
                    let now = uptime_ms(());
                    if let Some(outbound) = self.on_tick(now)
                        && let Err(error) = transmit(phy, outbound, port_count).await
                    {
                        return error;
                    }
                }
            }
        }
    }
}
