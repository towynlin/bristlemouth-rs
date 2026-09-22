//! The node: what to say, when to say it, and what to do with what arrives.
//!
//! [`Node::on_frame`], [`Node::on_tick`] and [`Node::on_expiry`] are
//! synchronous: they take the current time, mutate the node's state, and
//! return at most one frame to transmit — no futures, no PHY, no allocator.
//! All the protocol is there. [`Node::run`] is the thin async part on top: it
//! waits on the PHY or the heartbeat ticker, calls one of the three, and
//! transmits what came back.
//!
//! # Forwarding
//!
//! [`Node::on_frame`] runs bm_core's two receive stages in bm_core's order:
//! L2's routing policy ([`bm_wire::l2_policy::rx_apply`]) decides which ports
//! the frame is relayed to and whether it also travels up the local stack, and
//! only then is it parsed as BCMP. Both answers come back in [`Owed`], relay
//! first, which is the order `bm_l2_process_rx_evt` puts them on the wire in.
//!
//! [`Node::forward_link_local`] is `bcmp_ll_forward`: it re-floods a
//! link-local *message* as a fresh frame per port rather than relaying the
//! received bytes. A `0x10`, `0x11` or `0x12` naming another node comes back
//! as [`Owed::forward`], which [`Node::reflood`] turns into one frame per
//! other port. Config and DFU will use the same path.
//!
//! # Requests and replies
//!
//! Everything the node sends goes through
//! [`bm_wire::bcmp::registry::Registry`], which is `bcmp/packet.c`'s state:
//! which message types exist, what sequence number an outgoing message
//! carries, which outstanding requests a reply may answer, and when an
//! unanswered one is given up on. [`Node::send`] is `bcmp_tx`,
//! [`Node::request`] is `bcmp_tx` with no number to echo, and what comes back
//! arrives as an [`Event`] — the three exits of `process_received_message`
//! plus the `cb(NULL)` the expiry sweep takes.
//!
//! A type nothing registers is neither sent nor dispatched, which is the C's
//! `BmENODEV`. [`Node::new`] registers what `bcmp_init` registers for the
//! ported modules — heartbeat, ping, system time, device info and the
//! neighbour table; [`Node::register`] adds more.
//!
//! # Device information is asked for, and kept
//!
//! `bcmp/info.c` correlates its own replies, as `bcmp/ping.c` does: `0x05` is
//! registered unsequenced, so `packet.c` never matches one to a request, and
//! the module keeps `INFO_REQUEST_LIST` instead. [`Node::request_device_info`]
//! is `bcmp_request_info` and the list is
//! [`bm_wire::bcmp::info::InfoRequests`]; a reply nothing asked for is
//! dropped. What a reply is worth depends on how it was asked for
//! ([`InfoRequestKind`]): `Cache` puts it in [`Node::device_info_cache`] if
//! the sender is a neighbour, and `Report` hands it to the application as
//! [`Event::DeviceInfo`] and caches nothing.
//!
//! # Ping is correlated outside the registry
//!
//! `bcmp/ping.c` registers both of its types unsequenced, so `packet.c` never
//! matches an echo reply to an echo request; the module does it itself, from
//! file-scope statics tracking exactly one outstanding ping. [`Node::ping`] is
//! `bcmp_send_ping_request`, and that single slot is on the node, so a second
//! ping overwrites the first's expectations as it does in the C. The verdict
//! arrives as [`Event::EchoReply`], which bm_core reports to nobody.
//!
//! # Two timers, not one
//!
//! The 10-second heartbeat timer is [`Node::on_tick`]; `packet.c`'s 150 ms
//! expiry sweep is [`Node::on_expiry`]. The sweep carries its own phase (see
//! divergence #22), so [`Node::on_expiry`] only has to be called at least
//! every [`EXPIRY_PERIOD_MS`]. Putting it on a grid of the port's own would
//! make the port give up on requests at different moments from a C node.

use bm_wire::addr;
use bm_wire::bcmp::info::{
    CACHED_STRING_BYTES, CachedInfo, DeviceInfoReply, DeviceInfoRequest, InfoCache,
    InfoRequestKind, InfoRequests,
};
use bm_wire::bcmp::neighbors::{NeighborTableRequest, PortInfo, encode_neighbor_table_reply};
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::registry::{
    Delivery, MESSAGE_TIMER_EXPIRY_PERIOD_MS, PacketCfg, PendingRequest, Registry, RegistryError,
};
use bm_wire::bcmp::time::{SystemTimeHeader, SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
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

use crate::port::{Egress, Identity, NoRtc, Phy, Rtc, RtcTimeAndDate};

/// Largest frame the node will build or accept.
///
/// 1514 is a 1500-byte Ethernet payload plus the 14-byte header, which is what
/// `bcmp_max_payload_size_bytes` in `bcmp/bcmp.h` works out to.
pub const MTU: usize = 1514;

/// The IPv6 prefix bm_core builds a node's link-local address from.
pub const LINK_LOCAL_PREFIX: u32 = 0xFE80_0000;

/// The hop limit bm_core sets on everything it transmits.
pub const HOP_LIMIT: u8 = 64;

/// How many message types a node's registry holds.
///
/// bm_core registers thirty across the eight modules `bcmp_init` brings up,
/// each calling `packet_add` once per type it handles. This is that with room
/// to spare; [`Node::register`] reports [`RegistryError::Full`] past it.
/// bm_core has no such ceiling — its registry is a `bm_malloc`'d list.
pub const MESSAGE_TYPES: usize = 32;

/// How often [`Node::on_expiry`] must be called, `message_timer_expiry_period_ms`.
///
/// Not a deadline the node schedules against: the sweep's phase lives in the
/// registry, so this is only the *longest* a caller may leave between calls.
pub const EXPIRY_PERIOD_MS: u32 = MESSAGE_TIMER_EXPIRY_PERIOD_MS;

/// What a received message, or a request that gave up waiting, tells the
/// application.
///
/// The variants are the ways `process_received_message` can end for a
/// registered type, plus the one the expiry sweep takes. The C reaches the
/// application through function pointers — a `BcmpSequencedRequestCb` called
/// with the reply's payload or with `NULL`, and a `cfg->process` per type —
/// which a caller can confuse by not testing for the null payload. Here a
/// timeout has no payload to read.
///
/// The payload borrows the frame it arrived in and is gone when the handler
/// returns, which is what keeps this allocation-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a> {
    /// A reply answered an outstanding request, which is no longer
    /// outstanding. The C's `cb(data.payload)`.
    ///
    /// **`request.message_type` is not what was matched.** The C looks the
    /// outstanding request up by sequence number alone and never compares the
    /// type it recorded, so a reply of one type answers a request of another
    /// whenever the numbers line up. The port reproduces that; divergence #21
    /// has the measurement. A handler that cares has to compare
    /// `message_type` against `request.message_type` itself.
    Reply {
        /// The request this answered, as it was recorded when it was sent.
        request: PendingRequest,
        /// The type of the *reply*, which may be nothing like the request's.
        message_type: MessageType,
        /// Node id the reply came from.
        source: u64,
        /// The reply's body, after the BCMP header.
        payload: &'a [u8],
    },
    /// The expiry sweep gave up on a request. The C's `cb(NULL)`.
    ///
    /// A reply arriving after this no longer matches anything, so it is
    /// delivered as [`Event::Message`]: the application hears about one
    /// exchange twice, once as a failure and once as unsolicited traffic. See
    /// divergence #22.
    Timeout {
        /// The request that went unanswered.
        request: PendingRequest,
    },
    /// The message was handed to its type's own processor, the C's
    /// `cfg->process`. Ordinary inbound traffic: a heartbeat, a request
    /// addressed to this node or to any node, or a reply that matched no
    /// outstanding request.
    Message {
        /// The message type, which is registered — an unregistered type is
        /// dropped without an event.
        message_type: MessageType,
        /// The sequence number in the header, zero for anything unsequenced.
        seq_num: u32,
        /// Node id the message came from.
        source: u64,
        /// The body, after the BCMP header.
        payload: &'a [u8],
    },
    /// An echo reply answered the outstanding ping — `bcmp_process_ping_reply`
    /// reaching its `err = BmOK`.
    ///
    /// Reported **in addition to** [`Event::Message`] for the same frame: the
    /// `Message` is what `process_received_message` dispatched, this is what
    /// `ping.c` made of it. A reply that does not match produces only the
    /// `Message`.
    ///
    /// bm_core has nowhere to send this — `bcmp_send_ping_request` takes no
    /// callback and echo replies are unsequenced — so there the round-trip
    /// result is only a `bm_debug` line. See divergence #32.
    EchoReply {
        /// Node id the reply came from, from the frame's source address.
        ///
        /// Not what was matched on, and not necessarily
        /// [`bm_wire::bcmp::ping::EchoReply::node_id`] either: the C compares
        /// neither.
        source: u64,
        /// The reply as it arrived, payload included.
        reply: bm_wire::bcmp::ping::EchoReply<'a>,
        /// Milliseconds since [`Node::ping`] built the request, the value
        /// bm_core prints as `time=`. Wrapping, like every other clock here.
        round_trip_ms: u32,
    },
    /// A device-info reply answered a request made with
    /// [`InfoRequestKind::Report`] — `bcmp_process_info_reply` reaching
    /// `cb(info)`.
    ///
    /// Reported **in addition to** [`Event::Message`] for the same frame, as
    /// [`Event::EchoReply`] is. Nothing is cached: the C takes the callback
    /// branch *instead of* the neighbour branch, so an application that wants
    /// both has to keep the reply itself.
    ///
    /// The request this answers was matched on the low 32 bits of the node id
    /// the reply claims, and on nothing else — see divergence #33.
    DeviceInfo {
        /// Node id the reply came from, from the frame's source address.
        ///
        /// Not what was matched on: that is
        /// [`DeviceInfoReply::info`]`.node_id`, which the sender chose.
        source: u64,
        /// The reply as it arrived.
        reply: DeviceInfoReply<'a>,
    },
}

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
/// Field order is transmit order: L2 queues the relay before it submits the
/// frame up the stack, so a C node puts the relayed copy on the wire first, and
/// [`deliver`] does the same.
///
/// The lifetimes are separate because the two frames live in different buffers:
/// `'f` is the caller's receive buffer, `'n` the node's transmit buffer.
///
/// [`Owed::forward`] is an instruction rather than a frame: `bcmp_ll_forward`
/// builds one new frame per port and there is one transmit buffer, so the
/// caller builds and transmits them one at a time. [`Node::reflood`] is that
/// loop.
#[derive(Debug)]
pub struct Owed<'f, 'n> {
    /// The received frame, already prepared as a forwarded copy, and the ports
    /// it is relayed to. `None` when the routing policy asked for no relay.
    pub relay: Option<Outbound<'f>>,
    /// A frame the node built in answer, or `None` if it owes nothing.
    pub reply: Option<Outbound<'n>>,
    /// A message to re-flood out every other port, or `None`.
    ///
    /// Plain data rather than a frame, so it survives the [`Owed`] being
    /// consumed: read it out before handing the rest to [`deliver`].
    pub forward: Option<Reflood>,
}

impl Owed<'_, '_> {
    /// Whether there is nothing to transmit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.relay.is_none() && self.reply.is_none() && self.forward.is_none()
    }
}

/// A received message `bcmp_ll_forward` is to re-flood, as a range within the
/// frame it arrived in.
///
/// The C hands `bcmp_ll_forward` `data.header`, `data.payload` and `data.size`
/// — the BCMP header and body as they arrived, still inside the received
/// frame. A range rather than a borrow, so nothing is copied and the node's one
/// transmit buffer stays free for the copies.
///
/// [`Self::ingress_port`] is the C's `data.ingress_port`: the nibble the
/// *sender's* L2 stamped into the source address, not the port the PHY
/// reports. A sender that stamped nothing yields 0, and the message is then
/// re-flooded back out the port it came in on — see
/// [`bm_wire::bcmp::forward::egress_ports`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reflood {
    /// Offset of the first byte of the BCMP header within the received frame.
    pub start: usize,
    /// One past the last body byte, taken from the IPv6 payload length.
    pub end: usize,
    /// The one port the message is *not* re-flooded to.
    pub ingress_port: u8,
}

impl Reflood {
    /// The bytes to re-flood, out of the frame they arrived in.
    ///
    /// # Panics
    ///
    /// Never, for the frame this [`Reflood`] came from: the range was taken
    /// from that frame's own contents.
    #[must_use]
    pub fn bcmp<'f>(&self, frame: &'f [u8]) -> &'f [u8] {
        &frame[self.start..self.end]
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

/// Default number of unanswered device-info requests a node remembers,
/// [`Node`]'s `INFO_REQUESTS`.
///
/// bm_core's `INFO_REQUEST_LIST` is unbounded and never expires an entry
/// (divergence #19), so there is no C number to match. This is a ceiling the
/// port adds: past it a request still goes out, but its reply is unsolicited
/// and nothing is cached.
pub const INFO_REQUESTS_DEFAULT: usize = 8;

/// Default size of a node's expected-ping-payload buffer, [`Node`]'s
/// `PING_PAYLOAD`.
///
/// bm_core keeps this on the heap, reallocated per request, so it has no
/// ceiling but `bcmp_tx`'s. A node that wants to ping with more than this
/// raises the parameter.
pub const PING_PAYLOAD_BYTES: usize = 64;

/// `bcmp/ping.c`'s four file-scope statics, which track exactly one
/// outstanding ping.
///
/// A second [`Node::ping`] overwrites the first's expectations, as
/// `bcmp_send_ping_request` does by freeing and reallocating
/// `EXPECTED_PAYLOAD`. Nothing else ever clears them, so the last ping's
/// payload keeps answering for as long as the node runs.
#[derive(Debug)]
struct PingState<const PAYLOAD: usize> {
    /// `BCMP_SEQ`. A `uint32_t` counter whose low sixteen bits are what
    /// reaches the wire, and a sequence space entirely separate from
    /// `packet.c`'s `message_count`.
    seq: u32,
    /// `PING_REQUEST_TIMEOUT`, which despite the name times nothing out: it is
    /// stamped after every request and read only to report a round-trip. See
    /// divergence #32.
    sent_at_ms: u32,
    /// `EXPECTED_PAYLOAD_LEN`, and `None` for the `EXPECTED_PAYLOAD == NULL`
    /// the C starts in and returns to on a payload-free request. The C's two
    /// statics are only ever set and cleared together, so one field holds both.
    expected_len: Option<u16>,
    /// `EXPECTED_PAYLOAD`'s bytes, as much of them as is worth keeping.
    expected: [u8; PAYLOAD],
}

impl<const PAYLOAD: usize> Default for PingState<PAYLOAD> {
    fn default() -> Self {
        Self {
            seq: 0,
            sent_at_ms: 0,
            expected_len: None,
            expected: [0u8; PAYLOAD],
        }
    }
}

impl<const PAYLOAD: usize> PingState<PAYLOAD> {
    /// What `bcmp_process_ping_reply` compares against, or `None` for the C's
    /// null pointer.
    fn expected_payload(&self) -> Option<&[u8]> {
        self.expected_len
            .map(|len| &self.expected[..usize::from(len)])
    }

    /// `bcmp_send_ping_request`'s clear-then-copy: the old expectation goes
    /// whether or not a new one replaces it, and an empty payload leaves the
    /// pointer null.
    fn remember(&mut self, payload: &[u8]) {
        self.expected_len = None;
        if !payload.is_empty() {
            self.expected[..payload.len()].copy_from_slice(payload);
            self.expected_len = Some(payload.len() as u16);
        }
    }
}

/// A Bristlemouth node.
///
/// `NEIGHBORS` is the neighbour-table capacity, and must be at least the PHY's
/// port count, since bm_core keeps one neighbour per port.
///
/// `PENDING` is how many requests may await a reply at once. bm_core's list is
/// unbounded and discards a `bm_malloc` failure, so a full list here does the
/// same: the request goes out untracked and its reply arrives as ordinary
/// traffic. See
/// [`Outgoing::tracked`][bm_wire::bcmp::registry::Outgoing::tracked].
///
/// `PING_PAYLOAD` is the longest ping payload the node can remember well
/// enough to check a reply against, and so the longest [`Node::ping`] will
/// send. bm_core has no equivalent limit, only an unchecked `bm_malloc` whose
/// failure it dereferences. This is the one place ping's behaviour here is a
/// choice rather than a port.
///
/// `INFO_REQUESTS` is how many unanswered device-info requests are remembered,
/// and `INFO_STRINGS` how many bytes of each cached string are kept. Both are
/// ceilings bm_core does not have; the defaults keep every string whole, so
/// only the first is reachable in ordinary operation.
pub struct Node<
    I,
    R = NoRtc,
    const NEIGHBORS: usize = 4,
    const PENDING: usize = 4,
    const PING_PAYLOAD: usize = PING_PAYLOAD_BYTES,
    const INFO_REQUESTS: usize = INFO_REQUESTS_DEFAULT,
    const INFO_STRINGS: usize = CACHED_STRING_BYTES,
> {
    identity: I,
    rtc: R,
    neighbors: NeighborTable<NEIGHBORS>,
    registry: Registry<MESSAGE_TYPES, PENDING>,
    ping: PingState<PING_PAYLOAD>,
    /// `INFO_REQUEST_LIST`, and the device information the replies to it
    /// carried. bm_core hangs the second off its neighbour table entries and
    /// frees it with them, which is what [`NeighborTable`] evictions do here.
    info_requests: InfoRequests<INFO_REQUESTS>,
    info: InfoCache<NEIGHBORS, INFO_STRINGS>,
    port_count: u8,
    /// Link state per port, bit 0 for port 1. Cached rather than read from the
    /// PHY on demand, so the synchronous half stays free of I/O — the same
    /// arrangement bm_core has, where L2 keeps `enabled_ports_mask` up to date
    /// from link-change callbacks and `bm_l2_get_port_state` only reads it.
    link_mask: u16,
    tx: [u8; MTU],
}

impl<
    I: Identity,
    R: Rtc,
    const NEIGHBORS: usize,
    const PENDING: usize,
    const PING_PAYLOAD: usize,
    const INFO_REQUESTS: usize,
    const INFO_STRINGS: usize,
> Node<I, R, NEIGHBORS, PENDING, PING_PAYLOAD, INFO_REQUESTS, INFO_STRINGS>
{
    /// A node with an empty neighbour table, at time zero.
    ///
    /// The registry comes up holding what `bcmp_init` registers for the ported
    /// modules, with the expiry sweep phased from zero — where
    /// [`Node::on_tick`]'s uptime clock starts.
    pub fn new(identity: I, rtc: R, port_count: u8) -> Self {
        let mut registry = Registry::new();
        // heartbeat.c, ping.c, time.c, neighbors.c and info.c, in the order
        // `bcmp_init` calls their inits. Every one of them is
        // `{false, false}`: outside `bcmp/config.c`, nothing in bm_core is
        // sequenced at all, so all of this rides on the wire with a sequence
        // number of zero.
        for message_type in [
            MessageType::HEARTBEAT,
            MessageType::ECHO_REQUEST,
            MessageType::ECHO_REPLY,
            MessageType::SYSTEM_TIME_REQUEST,
            MessageType::SYSTEM_TIME_RESPONSE,
            MessageType::SYSTEM_TIME_SET,
            MessageType::NEIGHBOR_TABLE_REQUEST,
            MessageType::NEIGHBOR_TABLE_REPLY,
            MessageType::DEVICE_INFO_REQUEST,
            MessageType::DEVICE_INFO_REPLY,
        ] {
            // Cannot fail: MESSAGE_TYPES is far larger than this list.
            let _ = registry.add(message_type, PacketCfg::UNSEQUENCED);
        }
        Self {
            identity,
            rtc,
            neighbors: NeighborTable::new(),
            registry,
            ping: PingState::default(),
            info_requests: InfoRequests::new(),
            info: InfoCache::new(),
            port_count,
            link_mask: 0,
            tx: [0u8; MTU],
        }
    }

    /// Register a message type, as each module's init does with `packet_add`.
    ///
    /// A card that ports a new exchange registers its types here, with the
    /// flags its C module uses. Until a type is registered the node will
    /// neither send it nor dispatch it.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Full`] once [`MESSAGE_TYPES`] types are registered.
    pub fn register(
        &mut self,
        message_type: MessageType,
        cfg: PacketCfg,
    ) -> Result<(), RegistryError> {
        self.registry.add(message_type, cfg)
    }

    /// Remove the first registration for `message_type`, as `packet_remove`
    /// does, reporting whether there was one.
    ///
    /// A node that unregisters a type it answers stops answering it: the
    /// message is dropped before its body is looked at, the C's `BmENODEV`.
    pub fn unregister(&mut self, message_type: MessageType) -> bool {
        self.registry.remove(message_type)
    }

    /// The packet registry: what is registered, and what is still waiting for
    /// a reply.
    pub fn registry(&self) -> &Registry<MESSAGE_TYPES, PENDING> {
        &self.registry
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

    /// This node's real-time clock, the seam a `0x10` request is answered from.
    pub fn rtc(&self) -> &R {
        &self.rtc
    }

    /// The same, mutably, for a firmware that sets its clock from somewhere
    /// other than a `0x12` message.
    pub fn rtc_mut(&mut self) -> &mut R {
        &mut self.rtc
    }

    /// How many ports this node has. Ports are numbered 1..=`port_count`.
    ///
    /// [`bm_wire::bcmp::forward::egress_ports`] takes it, which is what a
    /// caller driving [`Owed::forward`] by hand needs.
    #[must_use]
    pub fn port_count(&self) -> u8 {
        self.port_count
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
    /// cleared, everything else as it arrived — and [`Owed::relay`] borrows it.
    /// Anything that does not validate as BCMP is dropped silently, as in
    /// bm_core; a dropped frame can still be relayed, since the two decisions
    /// are made by different layers.
    ///
    /// bm_core's L2 also takes a link-local routing callback, consulted for
    /// link-local multicast that is not `FF02::1`. Nothing in bm_core registers
    /// one (`bm_l2_register_link_local_routing_callback` has no callers), so
    /// this passes `None`: such a frame is submitted locally and relayed
    /// nowhere.
    pub fn on_frame<'f>(
        &mut self,
        now_ms: u32,
        ingress_port: u8,
        frame: &'f mut [u8],
    ) -> Owed<'f, '_> {
        self.on_frame_with(now_ms, ingress_port, frame, |_| {})
    }

    /// The same, reporting every [`Event`] the frame produces.
    ///
    /// The handler runs while the frame is still borrowed, so a payload is
    /// passed without copying. It runs before the node acts on the message, so
    /// the application sees what prompted a reply before the reply is built —
    /// the C's order, where `cfg->process` *is* the node's handling.
    pub fn on_frame_with<'f>(
        &mut self,
        now_ms: u32,
        ingress_port: u8,
        frame: &'f mut [u8],
        mut events: impl FnMut(Event<'_>),
    ) -> Owed<'f, '_> {
        let ingress_mask = port_mask(ingress_port);
        let policy = l2_policy::rx_apply(frame, ingress_mask, self.all_ports_mask(), None);

        // What the C copies for forwarding, it copies here -- before the
        // receive path rewrites any of it.
        let snapshot = Snapshot::take(frame);

        let (reply, forward) = if policy.should_submit {
            self.submit(now_ms, ingress_port, frame, &mut events)
        } else {
            (None, None)
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

        Owed {
            relay,
            reply,
            forward,
        }
    }

    /// Validate a frame as BCMP and answer it — `bm_l2_submit` and the BCMP
    /// task, minus the queue between them.
    ///
    /// Borrows `frame` only for the call, so the caller can still relay it.
    ///
    /// The second half of the return is the C's `should_forward`: a message
    /// this node is not the target of, which `bcmp_ll_forward` puts back on
    /// every other port. It is a range rather than a frame because that
    /// re-flood is one *new* frame per port and there is one transmit buffer.
    fn submit<'s>(
        &'s mut self,
        now_ms: u32,
        ingress_port: u8,
        frame: &mut [u8],
        events: &mut impl FnMut(Event<'_>),
    ) -> (Option<Outbound<'s>>, Option<Reflood>) {
        let Ok(received) = rx::accept(frame) else {
            return (None, None);
        };
        let message_type = received.header.message_type;
        let seq_num = received.header.seq_num;
        let source = received.src.to_node_id();
        let reply_to = received.dst;

        // `process_received_message`'s dispatch, in its order: an unregistered
        // type is dropped before its body is looked at, a reply that answers
        // an outstanding request goes to that request's callback *instead of*
        // the type's processor, and everything else is processed.
        match self.registry.on_received(message_type, seq_num) {
            Delivery::Unregistered => return (None, None),
            Delivery::SequencedReply(request) => {
                events(Event::Reply {
                    request,
                    message_type,
                    source,
                    payload: received.payload,
                });
                return (None, None);
            }
            Delivery::Process => events(Event::Message {
                message_type,
                seq_num,
                source,
                payload: received.payload,
            }),
        }

        // Everything the reply needs is copied out of the frame here, so the
        // borrow `accept` took ends before a reply is built.
        let reply = match message_type {
            MessageType::HEARTBEAT => {
                let Ok(heartbeat) = Heartbeat::decode(received.payload) else {
                    return (None, None);
                };
                let outcome = self
                    .neighbors
                    .on_heartbeat(now_ms, source, ingress_port, &heartbeat);
                if let Some(evicted) = outcome.evicted {
                    // `bcmp_add_neighbor` clears the port before it inserts,
                    // and `bcmp_remove_neighbor_from_table` frees the entry's
                    // two strings along with it.
                    self.info.forget(evicted);
                }
                if !outcome.request_info {
                    return (None, None);
                }
                // The two call sites ask about different nodes.
                // `bcmp_update_neighbor` passes the `node_id` it was given;
                // `bcmp_process_heartbeat`'s restart path passes
                // `neighbor->info.node_id`, which is whatever the last cached
                // reply said and zero until one arrives. See divergence #34.
                let target_node_id = if outcome.reset {
                    self.info
                        .get(source)
                        .map_or(0, |cached| cached.info.node_id)
                } else {
                    source
                };
                self.request_device_info(now_ms, target_node_id, InfoRequestKind::Cache)
            }
            MessageType::DEVICE_INFO_REQUEST => {
                let Ok(request) = DeviceInfoRequest::decode(received.payload) else {
                    return (None, None);
                };
                if !self.addressed_to_us(request.target_node_id) {
                    return (None, None);
                }
                self.build_device_info_reply(now_ms, &reply_to, seq_num)
            }
            MessageType::NEIGHBOR_TABLE_REQUEST => {
                let Ok(request) = NeighborTableRequest::decode(received.payload) else {
                    return (None, None);
                };
                if !self.addressed_to_us(request.target_node_id) {
                    return (None, None);
                }
                self.build_neighbor_table_reply(now_ms, &reply_to, seq_num)
            }
            MessageType::DEVICE_INFO_REPLY => {
                // `bcmp_process_info_reply`. The declared string lengths are
                // checked here and nowhere in the C -- divergence #14.
                let Ok(info_reply) = DeviceInfoReply::decode(received.payload) else {
                    return (None, None);
                };
                // `ll_get_item` then `ll_remove`, both keyed on the low 32
                // bits of the node id the *reply* claims rather than on the
                // address it came from. A reply nothing asked for does
                // nothing at all.
                match self.info_requests.take(info_reply.info.node_id) {
                    Some(InfoRequestKind::Report) => events(Event::DeviceInfo {
                        source,
                        reply: info_reply,
                    }),
                    // `bcmp_find_neighbor(info->info.node_id)`: the C keeps
                    // this on the neighbour, so a claimed node id of zero
                    // never matches -- divergence #18.
                    Some(InfoRequestKind::Cache)
                        if self.neighbors.find(info_reply.info.node_id).is_some() =>
                    {
                        self.info.store(&info_reply);
                    }
                    // A reply nothing asked for, and one that was asked for
                    // but names a node that is not a neighbour: both are
                    // decoded, matched, consumed and dropped.
                    Some(InfoRequestKind::Cache) | None => {}
                }
                None
            }
            MessageType::ECHO_REQUEST => {
                let Ok(request) = EchoRequest::decode(received.payload) else {
                    return (None, None);
                };
                if !self.addressed_to_us(request.target_node_id) {
                    return (None, None);
                }
                // `bcmp_process_ping_request` overwrites `target_node_id` in
                // the received buffer and casts it to a reply. Nothing else
                // changes, so the payload goes back out exactly as it came in
                // -- and the frame itself is left alone here, which matters
                // because the relayed copy of a global-multicast echo request
                // is the same bytes.
                let reply = request.into_reply(self.identity.node_id());
                self.build_echo_reply(now_ms, &reply_to, &reply)
            }
            MessageType::ECHO_REPLY => {
                let Ok(reply) = EchoReply::decode(received.payload) else {
                    return (None, None);
                };
                // `bcmp_process_ping_reply`, which transmits nothing: it
                // either recognises the reply as the answer to the one ping it
                // is tracking, or ignores it.
                let our_id = self.identity.node_id() as u16;
                if reply.answers(our_id, self.ping.expected_payload()) {
                    events(Event::EchoReply {
                        source,
                        reply,
                        round_trip_ms: now_ms.wrapping_sub(self.ping.sent_at_ms),
                    });
                }
                None
            }
            MessageType::SYSTEM_TIME_REQUEST
            | MessageType::SYSTEM_TIME_RESPONSE
            | MessageType::SYSTEM_TIME_SET => {
                // `bcmp_time_process_time_message` reads the 16-byte header out
                // of the body before it looks at the type, and reads it without
                // consulting `data.size` -- divergence #14's shape, so a body
                // too short to hold one is refused here rather than guessed at.
                let Ok(header) = SystemTimeHeader::decode(received.payload) else {
                    return (None, None);
                };
                if !header.is_local(self.identity.node_id()) {
                    // The C's `should_forward`, which skips the switch
                    // entirely. `data.size` is the body length, so the region
                    // handed to `bcmp_ll_forward` is the header and body as
                    // they arrived.
                    return (
                        None,
                        Some(Reflood {
                            start: BCMP_HEADER_OFFSET,
                            end: BCMP_HEADER_OFFSET + BCMP_HEADER_LEN + received.payload.len(),
                            ingress_port: received.ingress_port,
                        }),
                    );
                }
                let time_reply = self.process_system_time(now_ms, message_type, received.payload);
                return (time_reply, None);
            }
            _ => None,
        };
        (reply, None)
    }

    /// The `switch` in `bcmp_time_process_time_message`, for a message this
    /// node is not forwarding.
    ///
    /// Three arms, and they do not agree with each other about what
    /// `target_node_id == 0` means: see [`bm_wire::bcmp::time`] and divergence
    /// #27. All three answers go to `FF02::1` rather than back to the address
    /// the message arrived from, because `bcmp_time_send_response` always
    /// passes `multicast_ll_addr`.
    fn process_system_time<'s>(
        &'s mut self,
        now_ms: u32,
        message_type: MessageType,
        payload: &[u8],
    ) -> Option<Outbound<'s>> {
        let our_node_id = self.identity.node_id();
        match message_type {
            MessageType::SYSTEM_TIME_REQUEST => {
                let request = SystemTimeRequest::decode(payload).ok()?;
                if !request.is_for(our_node_id) {
                    // A broadcast request reached the switch and dies here.
                    return None;
                }
                // `bm_rtc_get` failing means no answer at all, not an empty one.
                let utc_time_us = self.rtc.get()?.to_utc_micros();
                self.build_system_time_response(now_ms, request.header.source_node_id, utc_time_us)
            }
            MessageType::SYSTEM_TIME_RESPONSE => {
                // `bm_debug` and nothing else. The application already has it
                // as `Event::Message`, which is more than a C node offers.
                None
            }
            MessageType::SYSTEM_TIME_SET => {
                // The C reads `utc_time_us` past the header without checking
                // the size, as above.
                let set = SystemTimeSet::decode(payload).ok()?;
                // The C's `0x12` arm makes no test of its own -- this is the
                // outer one restated, so the function is right when read
                // alone. That absence is the divergence: zero got past the
                // outer test by being a broadcast, and nothing here takes it
                // back, so a broadcast set is honoured where a broadcast
                // request is not.
                if !set.is_for(our_node_id) {
                    return None;
                }
                if !self
                    .rtc
                    .set(&RtcTimeAndDate::from_utc_micros(set.utc_time_us))
                {
                    return None;
                }
                // The response echoes the microseconds that were *asked for*,
                // not what the RTC kept -- which differ, because the RTC has
                // only millisecond resolution.
                self.build_system_time_response(now_ms, set.header.source_node_id, set.utc_time_us)
            }
            _ => None,
        }
    }

    /// Re-flood a received link-local message out one port, as `bcmp_ll_forward`
    /// does — a fresh frame from this node, carrying the received BCMP header
    /// and body unchanged.
    ///
    /// `bcmp` is the received message, header first, as
    /// [`bm_wire::bcmp::rx::accept`] found it: `&frame[BCMP_HEADER_OFFSET..]`
    /// truncated to the IPv6 payload length. Call it once per port from
    /// [`bm_wire::bcmp::forward::egress_ports`], transmitting each frame before
    /// building the next — there is one transmit buffer, as bm_core allocates
    /// one forward buffer per port.
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
        self.on_tick_with(uptime_ms, |_| {})
    }

    /// The same, reporting every [`Event`] the tick produces.
    ///
    /// This is bm_core's heartbeat timer, which does not itself sweep
    /// outstanding requests — `packet.c` has [`Node::on_expiry`] for that. The
    /// sweep runs here too, so a node driven only by this entry point still
    /// gives up on requests. A node that also calls [`Node::on_expiry`] on time
    /// is unaffected: the phase decides when a sweep happens, not the call.
    pub fn on_tick_with(
        &mut self,
        uptime_ms: u32,
        mut events: impl FnMut(Event<'_>),
    ) -> Option<Outbound<'_>> {
        // bm_core checks neighbours and sends a heartbeat on the same timer,
        // in that order.
        self.neighbors.check(uptime_ms, |_| {});
        self.registry.on_tick(uptime_ms, |request| {
            events(Event::Timeout { request: *request });
        });
        self.build_heartbeat(uptime_ms)
    }

    /// Run `packet.c`'s expiry sweep, reporting every request that gave up.
    ///
    /// Call it at least every [`EXPIRY_PERIOD_MS`]. It is
    /// `sequence_list_timer_callback`, and the 150 ms grid it fires on — not
    /// the 24 ms a request is stamped with — decides when a request dies
    /// (divergence #22). The phase lives in the registry, so calling this
    /// early, late or twice changes nothing; only a skipped sweep does, and
    /// that leaves a request alive that a C node would have given up on.
    pub fn on_expiry(&mut self, now_ms: u32, mut events: impl FnMut(Event<'_>)) {
        self.registry.on_tick(now_ms, |request| {
            events(Event::Timeout { request: *request });
        });
    }

    /// Serialize a BCMP message and hand it back ready to transmit — `bcmp_tx`.
    ///
    /// `reply_seq_num` is the number being echoed. The registry decides
    /// whether it is used: a [`PacketCfg::sequenced_reply`] type carries it, a
    /// [`PacketCfg::sequenced_request`] type ignores it and takes the next
    /// number from the node's counter, and anything else — which outside
    /// `bcmp/config.c` is every type bm_core has — carries zero. Use
    /// [`Node::request`] when there is nothing to echo.
    ///
    /// Returns `None` and sends nothing when the type is not registered — the
    /// C's `BmENODEV`, where `serialize` leaves the caller's buffer untouched
    /// and `bcmp_tx` never reaches `bm_ip_tx_perform`. Also `None` when the
    /// message does not fit the transmit buffer, checked before the registry is
    /// asked so an oversized request never becomes an outstanding one, as in
    /// the C. The ceiling here is 1447 body bytes; the C's guard admits one
    /// more and then builds a frame a byte over the MTU (divergence #8).
    pub fn send(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        body: &[u8],
        reply_seq_num: u32,
    ) -> Option<Outbound<'_>> {
        let end = MIN_FRAME_WITH_ADDRESSES
            .checked_add(BCMP_HEADER_LEN)?
            .checked_add(body.len())?;
        if end > MTU {
            return None;
        }
        let (seq_num, mask) = self.outgoing(now_ms, message_type, reply_seq_num)?;
        let Self { identity, tx, .. } = self;
        build_outbound(
            tx,
            identity.node_id(),
            dst,
            message_type,
            seq_num,
            mask,
            |buf| {
                buf.get_mut(..body.len())
                    .ok_or(BmWireError::Truncated)?
                    .copy_from_slice(body);
                Ok(body.len())
            },
        )
    }

    /// Send a message that is not answering one — [`Node::send`] with no
    /// sequence number to echo, which is what every one of bm_core's own
    /// request sites passes.
    ///
    /// If `message_type` is registered as a
    /// [`PacketCfg::sequenced_request`], the message carries the node's next
    /// sequence number and is recorded as outstanding: a reply carrying that
    /// number comes back as [`Event::Reply`], and silence comes back as
    /// [`Event::Timeout`] from the first [`Node::on_expiry`] sweep more than
    /// [`DEFAULT_MESSAGE_TIMEOUT_MS`][bm_wire::bcmp::registry::DEFAULT_MESSAGE_TIMEOUT_MS]
    /// later.
    pub fn request(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        body: &[u8],
    ) -> Option<Outbound<'_>> {
        self.send(now_ms, dst, message_type, body, 0)
    }

    /// Ping `target_node_id`, or every node if it is zero — the whole of
    /// `bcmp_send_ping_request`.
    ///
    /// The request carries this node's id truncated to sixteen bits as its
    /// `id`, and `ping.c`'s own counter — not `packet.c`'s — as its `seq_num`,
    /// also truncated. `payload` is copied into the node's single expectation
    /// slot, replacing whatever the last ping left there; a reply matching it
    /// arrives as [`Event::EchoReply`].
    ///
    /// bm_core sends to `multicast_ll_addr` from every call site it has, but
    /// takes the address as an argument, so this does too.
    ///
    /// Returns `None` without sending, and **without disturbing the ping
    /// state**, when `payload` is longer than `PING_PAYLOAD` or than the `u16`
    /// length field: there would be nowhere to remember it. That is the one
    /// thing here with no C counterpart — bm_core `bm_malloc`s the copy and
    /// dereferences the result unchecked.
    ///
    /// Also `None`, but *after* the counter has advanced and the payload has
    /// been stored, if the request does not fit the transmit buffer or
    /// [`MessageType::ECHO_REQUEST`] is unregistered. That is the C's order,
    /// where `BCMP_SEQ++` and the copy both happen before `bcmp_tx` is called.
    pub fn ping(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        target_node_id: u64,
        payload: &[u8],
    ) -> Option<Outbound<'_>> {
        if payload.len() > PING_PAYLOAD || payload.len() > EchoRequest::MAX_PAYLOAD_LEN {
            return None;
        }

        // `echo_req->seq_num = BCMP_SEQ++`: the frame carries the value from
        // before the increment, sixteen bits of a thirty-two bit counter.
        let seq_num = self.ping.seq as u16;
        self.ping.seq = self.ping.seq.wrapping_add(1);
        // An empty slice stands in for both of the C's ways of asking for a
        // payload-free ping, a null pointer and a zero length -- it folds them
        // together at the top of `bcmp_send_ping_request` itself.
        self.ping.remember(payload);
        // The C stamps this after `bcmp_tx` returns rather than before. No
        // clock moves in between, so the order is not observable.
        self.ping.sent_at_ms = now_ms;

        let request = EchoRequest {
            target_node_id,
            id: self.identity.node_id() as u16,
            seq_num,
            payload,
        };
        let (header_seq, mask) = self.outgoing(now_ms, MessageType::ECHO_REQUEST, 0)?;
        let Self { identity, tx, .. } = self;
        build_outbound(
            tx,
            identity.node_id(),
            dst,
            MessageType::ECHO_REQUEST,
            header_seq,
            mask,
            |body| request.encode(body),
        )
    }

    /// `BCMP_SEQ`: the number the *next* ping will carry, before truncation.
    #[must_use]
    pub fn ping_sequence(&self) -> u32 {
        self.ping.seq
    }

    /// `EXPECTED_PAYLOAD` and `EXPECTED_PAYLOAD_LEN`, which an echo reply is
    /// compared against. `None` is the C's null pointer.
    ///
    /// Nothing clears this: bm_core keeps the last ping's payload for the life
    /// of the process, so a reply carrying it is accepted however long
    /// afterwards it arrives.
    #[must_use]
    pub fn expected_ping_payload(&self) -> Option<&[u8]> {
        self.ping.expected_payload()
    }

    /// Ask `target_node_id` what time it is — `bcmp_time_get_time`.
    ///
    /// Goes to `FF02::1`, so every node on the link sees it and exactly one
    /// answers. Passing zero asks **nobody**: the broadcast reaches every
    /// node's switch and every node drops it on the exact-match test. That is
    /// divergence #27, reproduced rather than corrected — a C node on the
    /// other end would ignore it too.
    pub fn request_system_time(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; SystemTimeRequest::LEN];
        SystemTimeRequest {
            header: SystemTimeHeader {
                target_node_id,
                source_node_id: self.identity.node_id(),
            },
        }
        .encode(&mut body)
        .ok()?;
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::SYSTEM_TIME_REQUEST,
            &body,
        )
    }

    /// Tell `target_node_id` what time it is — `bcmp_time_set_time`.
    ///
    /// Zero here *is* a broadcast, unlike [`Node::request_system_time`]: every
    /// node on the link adopts `utc_time_us` and every one of them answers.
    pub fn set_system_time(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        utc_time_us: u64,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; SystemTimeSet::LEN];
        SystemTimeSet {
            header: SystemTimeHeader {
                target_node_id,
                source_node_id: self.identity.node_id(),
            },
            utc_time_us,
        }
        .encode(&mut body)
        .ok()?;
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::SYSTEM_TIME_SET,
            &body,
        )
    }

    /// Ask `target_node_id` to describe itself, or every node if it is zero —
    /// `bcmp_request_info`.
    ///
    /// Goes to `FF02::1`, which is the address both of bm_core's own call
    /// sites pass. The request is recorded in [`Node::info_requests`] first
    /// and the record is dropped again if nothing could be sent, which is the
    /// C's `ll_item_add` before `bcmp_tx` and its `ll_remove` after a failure.
    ///
    /// `kind` is the C's `cb` argument. Both of bm_core's own call sites pass
    /// `NULL`, which is [`InfoRequestKind::Cache`].
    ///
    /// Returns `None` without sending when
    /// [`MessageType::DEVICE_INFO_REQUEST`] is unregistered. A request the
    /// list had no room for is still sent, and its reply then arrives as
    /// unsolicited traffic — the same shape as an untracked sequenced request.
    pub fn request_device_info(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        kind: InfoRequestKind,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; DeviceInfoRequest::LEN];
        DeviceInfoRequest { target_node_id }
            .encode(&mut body)
            .ok()?;
        let recorded = self.info_requests.record(target_node_id, kind);
        // `bcmp_tx` fails for an unregistered type and nothing else here: the
        // body is eight bytes, so the size guard cannot refuse it. Asking the
        // registry before the transmit borrow begins puts the C's `ll_remove`
        // ahead of the failure it answers, which is the same end state.
        if self
            .registry
            .cfg(MessageType::DEVICE_INFO_REQUEST)
            .is_none()
        {
            if recorded {
                // `ll_remove` takes the first entry with the key, which is not
                // necessarily the one just added -- see divergence #19.
                self.info_requests.take(target_node_id);
            }
            return None;
        }
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::DEVICE_INFO_REQUEST,
            &body,
        )
    }

    /// What is known about `node_id`, or `None` if nothing is.
    ///
    /// Populated by a [`InfoRequestKind::Cache`] reply from a node that is
    /// already in the neighbour table, and forgotten when that neighbour is
    /// evicted. An *offline* neighbour keeps its entry, as it keeps its table
    /// row.
    #[must_use]
    pub fn device_info(&self, node_id: u64) -> Option<CachedInfo<'_>> {
        self.info.get(node_id)
    }

    /// Everything known about every node — what `populate_neighbor_info`
    /// writes onto bm_core's neighbour table entries.
    pub fn device_info_cache(&self) -> &InfoCache<NEIGHBORS, INFO_STRINGS> {
        &self.info
    }

    /// `INFO_REQUEST_LIST`: which nodes have been asked to describe themselves
    /// and have not answered.
    pub fn info_requests(&self) -> &InfoRequests<INFO_REQUESTS> {
        &self.info_requests
    }

    fn addressed_to_us(&self, target_node_id: u64) -> bool {
        target_node_id == 0 || target_node_id == self.identity.node_id()
    }

    /// The sequence number `message_type` goes out with, and the port mask a
    /// frame this node built is transmitted on.
    ///
    /// `None` for an unregistered type, which is the C's `BmENODEV`: the
    /// registry is asked first, so a message whose type nothing registered is
    /// never built.
    fn outgoing(
        &mut self,
        now_ms: u32,
        message_type: MessageType,
        reply_seq_num: u32,
    ) -> Option<(u32, u16)> {
        let seq_num = self
            .registry
            .on_serialize(now_ms, message_type, reply_seq_num)
            .ok()?
            .seq_num;
        Some((seq_num, self.all_ports_mask()))
    }

    fn build_heartbeat(&mut self, uptime_ms: u32) -> Option<Outbound<'_>> {
        let (seq_num, mask) = self.outgoing(uptime_ms, MessageType::HEARTBEAT, 0)?;
        let Self { identity, tx, .. } = self;
        let heartbeat = heartbeat_for(uptime_ms, HEARTBEAT_PERIOD_S);
        build_outbound(
            tx,
            identity.node_id(),
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            seq_num,
            mask,
            |body| {
                heartbeat.encode(body)?;
                Ok(Heartbeat::LEN)
            },
        )
    }

    fn build_device_info_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        reply_seq_num: u32,
    ) -> Option<Outbound<'_>> {
        let (seq_num, mask) =
            self.outgoing(now_ms, MessageType::DEVICE_INFO_REPLY, reply_seq_num)?;
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
        build_outbound(
            tx,
            node_id,
            dst,
            MessageType::DEVICE_INFO_REPLY,
            seq_num,
            mask,
            |body| reply.encode(body),
        )
    }

    fn build_echo_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        reply: &EchoReply<'_>,
    ) -> Option<Outbound<'_>> {
        // `bcmp_send_ping_reply` hands `bcmp_tx` the request's *body*
        // `seq_num` to echo into the header -- and `serialize` throws it away,
        // because ping is registered neither `sequenced_reply` nor
        // `sequenced_request`, so the header gets a zero. Passing it anyway
        // keeps the call the same shape as the C's; divergence #31 is what
        // becomes of it.
        let (seq_num, mask) =
            self.outgoing(now_ms, MessageType::ECHO_REPLY, u32::from(reply.seq_num))?;
        let Self { identity, tx, .. } = self;
        build_outbound(
            tx,
            identity.node_id(),
            dst,
            MessageType::ECHO_REPLY,
            seq_num,
            mask,
            |body| reply.encode(body),
        )
    }

    /// `bcmp_time_send_response`: a `0x11` to `FF02::1`, naming `target_node_id`
    /// in the body and this node as its source.
    fn build_system_time_response(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        utc_time_us: u64,
    ) -> Option<Outbound<'_>> {
        let (seq_num, mask) = self.outgoing(now_ms, MessageType::SYSTEM_TIME_RESPONSE, 0)?;
        let Self { identity, tx, .. } = self;
        let node_id = identity.node_id();
        let response = SystemTimeResponse {
            header: SystemTimeHeader {
                target_node_id,
                source_node_id: node_id,
            },
            utc_time_us,
        };
        build_outbound(
            tx,
            node_id,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::SYSTEM_TIME_RESPONSE,
            seq_num,
            mask,
            |body| {
                response.encode(body)?;
                Ok(SystemTimeResponse::LEN)
            },
        )
    }

    fn build_neighbor_table_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        reply_seq_num: u32,
    ) -> Option<Outbound<'_>> {
        let (seq_num, mask) =
            self.outgoing(now_ms, MessageType::NEIGHBOR_TABLE_REPLY, reply_seq_num)?;
        let Self {
            identity,
            neighbors,
            port_count,
            link_mask,
            tx,
            ..
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
        for (slot, neighbor) in table.iter_mut().zip(neighbors.neighbors()) {
            *slot = bm_wire::bcmp::NeighborInfo {
                node_id: neighbor.node_id,
                port: neighbor.port,
                online: u8::from(neighbor.online),
            };
            count += 1;
        }

        build_outbound(
            tx,
            node_id,
            dst,
            MessageType::NEIGHBOR_TABLE_REPLY,
            seq_num,
            mask,
            |body| encode_neighbor_table_reply(body, node_id, ports, &table[..count]),
        )
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
    seq_num: u32,
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
    tx::serialize_in_place(tx.get_mut(..end)?, message_type, seq_num, body_len).ok()?;
    Some(end)
}

/// [`build_frame`], handed back as the [`Outbound`] every `build_*` returns.
fn build_outbound<'a, F>(
    tx: &'a mut [u8],
    node_id: u64,
    dst: &BmIpAddr,
    message_type: MessageType,
    seq_num: u32,
    mask: u16,
    body: F,
) -> Option<Outbound<'a>>
where
    F: FnOnce(&mut [u8]) -> Result<usize, BmWireError>,
{
    let end = build_frame(tx, node_id, dst, message_type, seq_num, body)?;
    Some(Outbound {
        frame: tx.get_mut(..end)?,
        mask,
    })
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
/// [`Owed::forward`] is **not** covered, because a re-flood needs the node's
/// transmit buffer once per port and this has already given it away. Read the
/// field out before calling this and hand it to [`Node::reflood`] afterwards,
/// which is what [`Node::run`] does.
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

impl<
    I: Identity,
    R: Rtc,
    const NEIGHBORS: usize,
    const PENDING: usize,
    const PING_PAYLOAD: usize,
    const INFO_REQUESTS: usize,
    const INFO_STRINGS: usize,
> Node<I, R, NEIGHBORS, PENDING, PING_PAYLOAD, INFO_REQUESTS, INFO_STRINGS>
{
    /// Run the node until the PHY fails, discarding every [`Event`].
    ///
    /// See [`Node::run_with`], which is the same loop with somewhere for the
    /// replies and timeouts to go.
    ///
    /// # Errors
    ///
    /// The first error the PHY reports, from either direction.
    pub async fn run<P: Phy>(&mut self, phy: &mut P) -> P::Error {
        self.run_with(phy, |_| {}).await
    }

    /// Re-flood a received message out every port but the one it arrived on —
    /// the loop `bcmp_ll_forward` runs internally.
    ///
    /// `frame` is the frame the [`Reflood`] came from, which the caller gets
    /// back once the [`Owed`] it was carried in has been delivered. Each copy
    /// is built into the node's one transmit buffer and put on the wire before
    /// the next is built, because there is only one of it — the same
    /// constraint the C has, allocating one forward buffer per port in turn.
    ///
    /// A copy that does not fit the transmit buffer is skipped rather than
    /// abandoning the rest, which is what the C does too: its per-port loop
    /// records the error and carries on.
    ///
    /// # Errors
    ///
    /// Whatever the PHY returns. A failure abandons the remaining ports.
    pub async fn reflood<P: Phy>(
        &mut self,
        phy: &mut P,
        reflood: Reflood,
        frame: &[u8],
    ) -> Result<(), P::Error> {
        let port_count = self.port_count;
        for egress_port in forward::egress_ports(port_count, reflood.ingress_port) {
            let Some(outbound) = self.forward_link_local(egress_port, reflood.bcmp(frame)) else {
                continue;
            };
            transmit(phy, outbound, port_count).await?;
        }
        Ok(())
    }

    /// Run the node until the PHY fails, reporting every [`Event`].
    ///
    /// Waits on whichever comes first — a frame, the heartbeat tick or the
    /// expiry sweep — handles it, and transmits anything owed. Both timers are
    /// bm_core's: `bcmp_heartbeat_s`, which also ages the neighbour table in
    /// that order, and `packet.c`'s [`EXPIRY_PERIOD_MS`] sweep. Keeping them
    /// apart is what lets a request time out on the C's grid while heartbeats
    /// stay ten seconds apart.
    ///
    /// Returns rather than panicking when the PHY errors, so the caller can
    /// decide whether that is fatal. It has no other exit.
    ///
    /// # Errors
    ///
    /// The first error the PHY reports, from either direction.
    pub async fn run_with<P: Phy>(
        &mut self,
        phy: &mut P,
        mut events: impl FnMut(Event<'_>),
    ) -> P::Error {
        use embassy_futures::select::{Either3, select3};
        use embassy_time::{Duration, Instant, Ticker};

        let started = Instant::now();
        let mut ticker = Ticker::every(Duration::from_secs(u64::from(HEARTBEAT_PERIOD_S)));
        let mut expiry = Ticker::every(Duration::from_millis(u64::from(EXPIRY_PERIOD_MS)));
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

            match select3(phy.receive(&mut rx), ticker.next(), expiry.next()).await {
                Either3::First(Ok((port, len))) => {
                    let now = uptime_ms(());
                    let owed = self.on_frame_with(now, port, &mut rx[..len], &mut events);
                    // Copied out before `owed` is consumed: the re-flood needs
                    // the frame back, and `deliver` is holding it.
                    let forward = owed.forward;
                    if let Err(error) = deliver(phy, owed, port_count).await {
                        return error;
                    }
                    if let Some(reflood) = forward
                        && let Err(error) = self.reflood(phy, reflood, &rx[..len]).await
                    {
                        return error;
                    }
                }
                Either3::First(Err(error)) => return error,
                Either3::Second(()) => {
                    let now = uptime_ms(());
                    if let Some(outbound) = self.on_tick_with(now, &mut events)
                        && let Err(error) = transmit(phy, outbound, port_count).await
                    {
                        return error;
                    }
                }
                Either3::Third(()) => {
                    let now = uptime_ms(());
                    self.on_expiry(now, &mut events);
                }
            }
        }
    }
}
