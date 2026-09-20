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
//!
//! # Forwarding
//!
//! A frame that arrives is not always this node's business, and is not always
//! only this node's business. [`Node::on_frame`] therefore runs bm_core's two
//! receive stages in bm_core's order: L2's routing policy
//! ([`bm_wire::l2_policy::rx_apply`]) decides which ports the frame is relayed
//! to and whether it also travels up the local stack, and only then is it
//! parsed as BCMP. Both answers come back together in [`Owed`], relay first,
//! because that is the order `bm_l2_process_rx_evt` puts them on the wire in.
//!
//! [`Node::forward_link_local`] is the other half — `bcmp_ll_forward`, which
//! re-floods a link-local *message* as a fresh frame per port rather than
//! relaying the received bytes. Nothing calls it yet; the system-time, config
//! and DFU exchanges that do are still unported.

use bm_wire::addr;
use bm_wire::bcmp::info::{DeviceInfoReply, DeviceInfoRequest};
use bm_wire::bcmp::neighbors::{NeighborTableRequest, PortInfo, encode_neighbor_table_reply};
use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, Heartbeat, MessageType, forward, rx, tx};
use bm_wire::frame::{
    ETHERNET_DESTINATION_OFFSET, ETHERNET_SRC_OFFSET, ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET,
    IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_HOP_LIMIT_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::l2::{self, TxKind};
use bm_wire::l2_policy;
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

/// A frame the node wants transmitted, and the ports it goes out on.
///
/// Mutable because stamping the egress port rewrites it, once per port. The
/// borrow is of whichever buffer the frame lives in: the node's transmit buffer
/// for something it built, or the caller's receive buffer for a frame being
/// relayed.
#[derive(Debug)]
pub struct Outbound<'a> {
    frame: &'a mut [u8],
    mask: u16,
}

impl Outbound<'_> {
    /// The frame as it stands, before any egress port is stamped into it.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        self.frame
    }

    /// Ports to transmit on, bit 0 for port 1.
    ///
    /// `bm_l2_tx` takes the same mask. It is every port for anything the node
    /// built, and the routing policy's egress mask for a relay.
    #[must_use]
    pub fn mask(&self) -> u16 {
        self.mask
    }
}

/// What a received frame obliges the node to put back on the network.
///
/// The two halves come from the two stages of `bm_l2_process_rx_evt`, and the
/// field order is the transmit order: L2 queues the relay before it submits the
/// frame up the stack, so a C node puts the relayed copy on the wire first.
/// [`deliver`] does them in that order.
///
/// The lifetimes are separate because the two frames live in different buffers:
/// `'f` is the caller's receive buffer, `'n` the node's transmit buffer.
#[derive(Debug)]
pub struct Owed<'f, 'n> {
    /// The received frame, already prepared as a forwarded copy, and the ports
    /// it is relayed to. `None` when the routing policy asked for no relay.
    pub relay: Option<Outbound<'f>>,
    /// A frame the node built in answer, or `None` if it owes nothing.
    pub reply: Option<Outbound<'n>>,
}

impl Owed<'_, '_> {
    /// Whether there is nothing to transmit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.relay.is_none() && self.reply.is_none()
    }
}

/// The bytes `bm_wire::bcmp::rx::accept` rewrites, saved so a frame can still be
/// relayed after it has been parsed.
///
/// bm_core copies the frame for forwarding *before* it submits it, so the
/// forwarded copy carries the legacy port bytes and the checksum exactly as
/// they arrived — none of `process_received_message`'s clears. Reproducing that
/// without a second MTU-sized buffer means putting the five bytes back.
#[derive(Debug, Clone, Copy)]
struct Snapshot {
    ports: u8,
    legacy: [u8; 2],
    checksum: [u8; 2],
}

/// Offset of the first byte `clear_ports_legacy` zeroes, from
/// `bm_wire::bcmp::rx`.
const LEGACY_PORT_CLEAR_OFFSET: usize = IPV6_SOURCE_ADDRESS_OFFSET + 4;

impl Snapshot {
    /// `None` for a frame too short to hold all five bytes — which is also too
    /// short for `accept` to have rewritten any of them. `accept` refuses
    /// anything whose IPv6 payload length is under 13, and that needs 67 bytes,
    /// so a frame this cannot snapshot is a frame it has nothing to restore.
    fn take(frame: &[u8]) -> Option<Self> {
        let checksum_offset = BCMP_HEADER_OFFSET + bm_wire::bcmp::CHECKSUM_FIELD_OFFSET;
        Some(Self {
            ports: *frame.get(IPV6_INGRESS_EGRESS_PORTS_OFFSET)?,
            legacy: [
                *frame.get(LEGACY_PORT_CLEAR_OFFSET)?,
                *frame.get(LEGACY_PORT_CLEAR_OFFSET + 1)?,
            ],
            checksum: [
                *frame.get(checksum_offset)?,
                *frame.get(checksum_offset + 1)?,
            ],
        })
    }

    fn restore(self, frame: &mut [u8]) {
        let checksum_offset = BCMP_HEADER_OFFSET + bm_wire::bcmp::CHECKSUM_FIELD_OFFSET;
        frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = self.ports;
        frame[LEGACY_PORT_CLEAR_OFFSET] = self.legacy[0];
        frame[LEGACY_PORT_CLEAR_OFFSET + 1] = self.legacy[1];
        frame[checksum_offset] = self.checksum[0];
        frame[checksum_offset + 1] = self.checksum[1];
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

    /// Mask of every port this node has, `CTX.all_ports_mask` in `l2.c`.
    #[must_use]
    pub fn all_ports_mask(&self) -> u16 {
        l2::all_ports_mask(self.port_count)
    }

    /// Handle a received frame, returning everything it owes the network.
    ///
    /// This is `bm_l2_process_rx_evt`, in its order:
    ///
    /// 1. [`bm_wire::l2_policy::rx_apply`] stamps the ingress port into the
    ///    frame's source address and decides which ports the frame is relayed
    ///    to, and whether it also goes up the local stack;
    /// 2. if it does, the frame is validated as BCMP and answered.
    ///
    /// `frame` is mutated in place, as bm_core mutates it. When a relay is owed
    /// the frame comes back as the C's forwarded copy — the whole ports byte
    /// cleared, everything else as it arrived — and the returned [`Owed::relay`]
    /// borrows it. Anything that does not validate as BCMP is dropped silently,
    /// which is also what bm_core does; a dropped frame can still be relayed,
    /// because the two decisions are made by different layers.
    ///
    /// bm_core's L2 also takes a link-local routing callback, consulted for
    /// link-local multicast that is not `FF02::1`. Nothing in bm_core registers
    /// one — `bm_l2_register_link_local_routing_callback` has no callers — so
    /// this passes `None`, which is what a C node does too: such a frame is
    /// submitted locally and relayed nowhere.
    pub fn on_frame<'f>(
        &mut self,
        now_ms: u32,
        ingress_port: u8,
        frame: &'f mut [u8],
    ) -> Owed<'f, '_> {
        let ingress_mask = port_mask(ingress_port);
        let policy = l2_policy::rx_apply(frame, ingress_mask, self.all_ports_mask(), None);

        // What the C copies for forwarding, it copies here -- before the
        // receive path rewrites any of it.
        let snapshot = Snapshot::take(frame);

        let reply = if policy.should_submit {
            self.submit(now_ms, ingress_port, frame)
        } else {
            None
        };

        let relay = if policy.egress_mask != 0 {
            if let Some(snapshot) = snapshot {
                snapshot.restore(frame);
            }
            l2_policy::prepare_forwarded_copy(frame);
            Some(Outbound {
                frame,
                mask: policy.egress_mask,
            })
        } else {
            None
        };

        Owed { relay, reply }
    }

    /// Validate a frame as BCMP and answer it — `bm_l2_submit` and the BCMP
    /// task, minus the queue between them.
    ///
    /// Borrows `frame` only for the call, so the caller can still relay it.
    fn submit<'s>(
        &'s mut self,
        now_ms: u32,
        ingress_port: u8,
        frame: &mut [u8],
    ) -> Option<Outbound<'s>> {
        let received = rx::accept(frame).ok()?;
        let message_type = received.header.message_type;
        let source = received.src.to_node_id();
        let reply_to = received.dst;

        // Everything the reply needs is copied out of the frame here, so the
        // borrow `accept` took ends before a reply is built.
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

    /// Re-flood a received link-local message out one port, as `bcmp_ll_forward`
    /// does — a fresh frame from this node, carrying the received BCMP header
    /// and body unchanged.
    ///
    /// `bcmp` is the received message, header first, exactly as
    /// [`bm_wire::bcmp::rx::accept`] found it; a caller has it as
    /// `&frame[BCMP_HEADER_OFFSET..]` truncated to the IPv6 payload length.
    /// Call it once per port from [`bm_wire::bcmp::forward::egress_ports`],
    /// transmitting each frame before building the next — there is one transmit
    /// buffer, exactly as bm_core allocates one forward buffer per port.
    ///
    /// Returns `None` if the message does not fit the transmit buffer or is
    /// shorter than a BCMP header, both of which bm_core reports as an error
    /// too.
    pub fn forward_link_local(&mut self, egress_port: u8, bcmp: &[u8]) -> Option<Outbound<'_>> {
        let Self {
            identity,
            port_count,
            tx,
            ..
        } = self;
        let end = MIN_FRAME_WITH_ADDRESSES.checked_add(bcmp.len())?;
        let frame = tx.get_mut(..end)?;

        // bm_ip_tx_new: our own link-local address as the source, the plain
        // FF02::1 as the destination, then the header and body copied in and
        // checksummed against that destination.
        write_frame_headers(
            frame,
            identity.node_id(),
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            bcmp.len(),
        )?;
        forward::serialize_forwarded(frame, bcmp).ok()?;

        // bm_ip_tx_perform, then bm_l2_link_output: the egress port goes into
        // the destination address, the Ethernet MAC is derived from it, and L2
        // reads the port back out and clears it.
        forward::apply_port_specific_destination(frame, egress_port).ok()?;
        let mask = forward::take_egress_port(frame, *port_count).ok()?;

        Some(Outbound { frame, mask })
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
        let all_ports = self.all_ports_mask();
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
            mask: all_ports,
        })
    }

    fn build_device_info_request(&mut self, target_node_id: u64) -> Option<Outbound<'_>> {
        let all_ports = self.all_ports_mask();
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
            mask: all_ports,
        })
    }

    fn build_device_info_reply(&mut self, dst: &BmIpAddr) -> Option<Outbound<'_>> {
        let all_ports = self.all_ports_mask();
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
            mask: all_ports,
        })
    }

    fn build_neighbor_table_reply(&mut self, dst: &BmIpAddr) -> Option<Outbound<'_>> {
        let all_ports = self.all_ports_mask();
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
            mask: all_ports,
        })
    }
}

/// Most ports a neighbour-table reply will describe.
///
/// The port field in the reply is a `u8`, and no Bristlemouth device has more
/// than a handful; this is only the size of a stack array.
const MAX_REPORTED_PORTS: usize = 16;

/// The mask holding just `port`, or nothing for a port the device cannot have.
fn port_mask(port: u8) -> u16 {
    port.checked_sub(1)
        .filter(|bit| *bit < 16)
        .map_or(0, |bit| 1u16 << bit)
}

/// Write the Ethernet and IPv6 headers into `tx` for a `payload_len`-byte BCMP
/// payload.
///
/// Byte for byte what `bm_ip_tx_new` and `bm_ip_tx_perform` in
/// `network/bm_linux.c` produce, including the payload-length field — so a
/// caller that fills the payload afterwards has to know its length up front,
/// which every caller here does.
fn write_frame_headers(
    tx: &mut [u8],
    node_id: u64,
    dst: &BmIpAddr,
    payload_len: usize,
) -> Option<()> {
    if tx.len() < MIN_FRAME_WITH_ADDRESSES {
        return None;
    }
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
    tx[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
        .copy_from_slice(&u16::try_from(payload_len).ok()?.to_be_bytes());
    Some(())
}

/// Write the frame headers into `tx`, let `body` fill the BCMP payload, then
/// put the BCMP header and checksum around it.
///
/// Returns the total frame length. The body is written straight into the
/// transmit buffer rather than into a second one, which is what
/// `bm_wire::bcmp::tx::serialize_in_place` is for.
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
    let body_at = BCMP_HEADER_OFFSET + BCMP_HEADER_LEN;
    let body_len = body(tx.get_mut(body_at..)?).ok()?;
    let payload_len = BCMP_HEADER_LEN + body_len;
    write_frame_headers(tx, node_id, dst, payload_len)?;

    let end = MIN_FRAME_WITH_ADDRESSES + payload_len;
    tx::serialize_in_place(tx.get_mut(..end)?, message_type, 0, body_len).ok()?;
    Some(end)
}

/// Transmit one frame, stamping the egress port into each copy that needs it.
///
/// This is `bm_l2_process_tx_evt` together with `send_global_multicast_packet`:
///
/// * global multicast goes out unstamped — to every port at once when the mask
///   covers every port, otherwise once per port in the mask;
/// * link-local multicast goes out once per port in the mask, with that port
///   stamped into the source address and the checksum patched to match;
/// * anything else is dropped, as bm_core drops it.
///
/// The mask is [`Outbound::mask`], which is every port for a frame the node
/// built and the routing policy's egress mask for a relay.
///
/// # Errors
///
/// Whatever the PHY returns. A failure on one port abandons the rest.
pub async fn transmit<P: Phy>(
    phy: &mut P,
    outbound: Outbound<'_>,
    port_count: u8,
) -> Result<(), P::Error> {
    let Outbound { frame, mask } = outbound;
    let all_ports = l2::all_ports_mask(port_count);
    let ports = || (1..=port_count).filter(move |port| mask & port_mask(*port) != 0);

    match l2::tx_kind(frame) {
        TxKind::GlobalMulticast if mask == all_ports => phy.send(frame, Egress::AllPorts).await,
        TxKind::GlobalMulticast => {
            for port in ports() {
                phy.send(frame, Egress::Port(port)).await?;
            }
            Ok(())
        }
        TxKind::LinkLocalMulticast => {
            for port in ports() {
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

/// Transmit everything a received frame owed, in bm_core's order: the relayed
/// copy first, then the node's own reply.
///
/// # Errors
///
/// Whatever the PHY returns. A failure abandons whatever is left.
pub async fn deliver<P: Phy>(
    phy: &mut P,
    owed: Owed<'_, '_>,
    port_count: u8,
) -> Result<(), P::Error> {
    if let Some(relay) = owed.relay {
        transmit(phy, relay, port_count).await?;
    }
    if let Some(reply) = owed.reply {
        transmit(phy, reply, port_count).await?;
    }
    Ok(())
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
                    let owed = self.on_frame(now, port, &mut rx[..len]);
                    if let Err(error) = deliver(phy, owed, port_count).await {
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
