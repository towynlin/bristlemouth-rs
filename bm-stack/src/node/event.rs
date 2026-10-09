//! What a node reports and what it owes the network: [`Event`], [`Outbound`],
//! [`Owed`], [`Reflood`], and the error enums of its public methods.

use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::info::DeviceInfoReply;
use bm_wire::bcmp::neighbors::NeighborTableReply;
use bm_wire::bcmp::registry::PendingRequest;
use bm_wire::bcmp::resource::ResourceTableReply;
use bm_wire::pubsub::SubscriptionError;
use bm_wire::service::power_info::PowerInfoReply;

use crate::dfu::DfuFinished;

#[cfg(doc)]
use super::{NEIGHBOR_REQUEST_TIMEOUT_MS, Node, UDP_PORTS, deliver};
#[cfg(doc)]
use bm_wire::{
    bcmp::info::InfoRequestKind, bcmp::neighbors::TableRequestKind,
    bcmp::resource::ResourceAddError, bcmp::resource::ResourceRequestKind, pubsub,
    pubsub::Subscriptions, spotter,
};

/// Why [`Node::subscribe`] refused, or subscribed without advertising.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SubscribeError {
    /// Not subscribed: the reason, as [`Subscriptions::subscribe`] gives it.
    Refused(SubscriptionError),
    /// Subscribed, but the `SUB` resource could not be added:
    /// [`ResourceAddError::Full`]. `bm_sub_wl` likewise keeps the subscription
    /// and returns `bcmp_resource_discovery_add_resource`'s `BmENOMEM`.
    NotAdvertised,
}

/// Why [`Node::publish`] sent nothing, with the `BmErr` `bm_pub_wl` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PublishError {
    /// The topic is empty: `BmEINVAL`. Nothing is delivered.
    EmptyTopic,
    /// The topic is [`pubsub::TOPIC_MAX_LEN`] bytes or longer: `BmEMSGSIZE`.
    /// Nothing is delivered.
    TopicTooLong,
    /// The publication is longer than [`pubsub::MAX_MESSAGE_LEN`]: `BmEINVAL`
    /// from `bm_middleware_net_tx`. **Local subscribers have already received
    /// it**, as in the C.
    MessageTooLong,
}

/// Why [`Node::spotter_log`] or [`Node::spotter_tx_data`] sent nothing, with
/// the `BmErr` `spotter_log` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SpotterError {
    /// The text is empty: `BmENODATA`. Nothing is delivered.
    NoData,
    /// The file name, text or data is too long for
    /// [`spotter::encode_log`] or [`spotter::encode_tx_data`]: `BmEMSGSIZE`.
    /// Nothing is delivered.
    MessageSize,
    /// The publication is longer than [`pubsub::MAX_MESSAGE_LEN`]:
    /// `BmENETDOWN`, as `spotter_log` reports any `bm_pub` failure. **Local
    /// subscribers have already received it**, as in the C. Only
    /// [`Node::spotter_log`] reaches it (divergence #81).
    NotSent,
}

/// Why [`Node::bind_udp`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum UdpBindError {
    /// The port is already bound, or is [`pubsub::PORT`].
    InUse,
    /// [`UDP_PORTS`] ports are already bound.
    Full,
}

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
    /// The expiry sweep gave up on a request, after re-sending it
    /// [`PACKET_RETRY_COUNT`][bm_wire::bcmp::registry::PACKET_RETRY_COUNT]
    /// times. The C's callback with a null payload or zeroed data.
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
    /// A neighbour-table reply answered a request made with
    /// [`TableRequestKind::Report`] — `bcmp_process_neighbor_table_reply`
    /// reaching `NEIGHBOR_REQUEST_CB(reply)`.
    ///
    /// Reported **in addition to** [`Event::Message`] for the same frame, as
    /// [`Event::DeviceInfo`] is. Reported at most once per
    /// [`Node::request_neighbor_table`]: the C clears the callback here.
    ///
    /// What was matched is `reply.node_id` against `TARGET_NODE_ID`, all 64
    /// bits of it, and nothing else — not the source address, and not whether
    /// the request has already timed out. See divergences #35 and #36.
    NeighborTable {
        /// Node id the reply came from, from the frame's source address.
        source: u64,
        /// The reply as it arrived, both of its arrays borrowed from the
        /// frame.
        reply: NeighborTableReply<'a>,
    },
    /// A resource-table reply answered a request made with
    /// [`ResourceRequestKind::Report`] —
    /// `bcmp_process_resource_discovery_reply` reaching `cb->cb(repl)`.
    ///
    /// Reported **in addition to** [`Event::Message`] for the same frame, as
    /// [`Event::NeighborTable`] is.
    ///
    /// What was matched is two things, and this is the only exchange in BCMP
    /// that checks the second: the low 32 bits of the source address against
    /// [`Node::resource_requests`] (divergence #33), and
    /// [`ResourceTableReply::node_id`] against the whole of that source
    /// address. A reply whose body names a node other than the one it came
    /// from is dropped without the list being consulted at all.
    ResourceTable {
        /// Node id the reply came from, from the frame's source address —
        /// which here is also what the body claims.
        source: u64,
        /// The reply as it arrived, both halves of its record list borrowed
        /// from the frame.
        reply: ResourceTableReply<'a>,
    },
    /// `NEIGHBOR_TIMER` fired: a neighbour-table request has gone unanswered
    /// for [`NEIGHBOR_REQUEST_TIMEOUT_MS`]. The C's `timeout` argument to
    /// `bcmp_request_neighbor_table`.
    ///
    /// **This gives up on nothing.** The request stays armed behind it, so a
    /// reply arriving afterwards still comes back as
    /// [`Event::NeighborTable`] — unlike [`Event::Timeout`], which is the end
    /// of its request. Divergence #36.
    NeighborTableTimeout {
        /// `TARGET_NODE_ID`: the node that was asked, or zero for the
        /// broadcast that nothing can answer (divergence #35).
        ///
        /// The C's timeout callback is a `BmTimerCallback` and is handed only
        /// the timer, so an integrator has to remember this itself.
        target_node_id: u64,
    },
    /// The `UpdateFinishCb` given to [`Node::dfu_initiate_update`]: the
    /// update this node hosted ended, or — divergence #58 — this node's DFU
    /// entered its error state for any reason since.
    DfuUpdateFinished(DfuFinished),
    /// A UDP datagram arrived for a port [`Node::bind_udp`] bound — what
    /// lwIP's `udp_input` hands `bm_lwip.c`'s `udp_recv_cb`.
    Udp {
        /// The destination port, which is bound.
        port: u16,
        /// The sender's port. bm_core's bound callback is given this and not
        /// `port`, and `bm_middleware_rx` looks the application up by it
        /// (divergence #73).
        src_port: u16,
        /// Node id in the low half of the source address.
        source: u64,
        /// The IPv6 payload after the UDP header.
        payload: &'a [u8],
    },
    /// A publication reached a subscription — `bm_handle_msg` calling a
    /// subscriber's `BmPubSubCb`.
    ///
    /// Reported once per application callback on each matching subscription,
    /// in the order they were made, so a publication matching two is
    /// reported twice; a subscription the service layer made first can list
    /// the application twice (divergence #79). Received from the network by
    /// [`Node::on_frame_with`], or delivered locally by
    /// [`Node::publish_with`] and by a service reply.
    Publication {
        /// Node id the publication came from: the source address's low half,
        /// or this node's own id for a local delivery.
        source: u64,
        /// The subscription that matched.
        subscription: &'a [u8],
        /// The publication's topic.
        topic: &'a [u8],
        /// `ext_header.type`.
        kind: u8,
        /// `ext_header.version`.
        version: u8,
        /// Everything after the topic.
        data: &'a [u8],
    },
    /// A reply answered a request [`Node::service_request`] made, which is
    /// no longer waiting: the `BmServiceReplyCb` with `ack` true.
    ///
    /// Matched on the reply's `id` and `target_node_id` alone, so a reply
    /// on another service's reply topic answers a request whose id it
    /// carries (divergence #92).
    ServiceReply {
        /// The request's id.
        id: u32,
        /// The service the request named.
        service: &'a [u8],
        /// The reply's data: `data_size` bytes, or what arrived if fewer
        /// (divergence #92).
        data: &'a [u8],
    },
    /// A request [`Node::service_request`] made expired unanswered: the
    /// `BmServiceReplyCb` with `ack` false. Reported by
    /// [`Node::on_service_expiry`].
    ServiceTimeout {
        /// The request's id.
        id: u32,
        /// The service the request named.
        service: &'a [u8],
    },
    /// A power_info reply decoded: the `BmPowerInfoReplyCb` a
    /// [`Node::power_info_request`] queued. Requests that function makes
    /// report this in place of [`Event::ServiceReply`] and
    /// [`Event::ServiceTimeout`], as their C `reply_cb` is
    /// `power_info_reply_cb`.
    ///
    /// Each of those requests that ends, answered or expired, uses up the
    /// oldest callback still queued, so `id` is the oldest such request's,
    /// which need not be the one answered (divergence #96). An expiry or a
    /// reply that does not decode reports nothing, and uses one up too.
    PowerInfoReply {
        /// The id of the request whose callback this is.
        id: u32,
        /// The reply.
        reply: PowerInfoReply,
    },
}

/// Payloads and frames as their length: a derive would send every byte.
/// Topic, subscription and service names as ASCII.
#[cfg(feature = "defmt")]
impl defmt::Format for Event<'_> {
    fn format(&self, f: defmt::Formatter<'_>) {
        match self {
            Self::Reply {
                request,
                message_type,
                source,
                payload,
            } => defmt::write!(
                f,
                "Reply {{ request: {}, message_type: {}, source: {=u64:016x}, payload: {=usize} bytes }}",
                request,
                message_type,
                source,
                payload.len()
            ),
            Self::Timeout { request } => defmt::write!(f, "Timeout {{ request: {} }}", request),
            Self::Message {
                message_type,
                seq_num,
                source,
                payload,
            } => defmt::write!(
                f,
                "Message {{ message_type: {}, seq_num: {=u32}, source: {=u64:016x}, payload: {=usize} bytes }}",
                message_type,
                seq_num,
                source,
                payload.len()
            ),
            Self::EchoReply {
                source,
                reply,
                round_trip_ms,
            } => defmt::write!(
                f,
                "EchoReply {{ source: {=u64:016x}, reply: {}, round_trip_ms: {=u32} }}",
                source,
                reply,
                round_trip_ms
            ),
            Self::DeviceInfo { source, reply } => defmt::write!(
                f,
                "DeviceInfo {{ source: {=u64:016x}, reply: {} }}",
                source,
                reply
            ),
            Self::NeighborTable { source, reply } => defmt::write!(
                f,
                "NeighborTable {{ source: {=u64:016x}, reply: {} }}",
                source,
                reply
            ),
            Self::ResourceTable { source, reply } => defmt::write!(
                f,
                "ResourceTable {{ source: {=u64:016x}, reply: {} }}",
                source,
                reply
            ),
            Self::NeighborTableTimeout { target_node_id } => defmt::write!(
                f,
                "NeighborTableTimeout {{ target_node_id: {=u64:016x} }}",
                target_node_id
            ),
            Self::DfuUpdateFinished(finished) => {
                defmt::write!(f, "DfuUpdateFinished({})", finished);
            }
            Self::Udp {
                port,
                src_port,
                source,
                payload,
            } => defmt::write!(
                f,
                "Udp {{ port: {=u16}, src_port: {=u16}, source: {=u64:016x}, payload: {=usize} bytes }}",
                port,
                src_port,
                source,
                payload.len()
            ),
            Self::Publication {
                source,
                subscription,
                topic,
                kind,
                version,
                data,
            } => defmt::write!(
                f,
                "Publication {{ source: {=u64:016x}, subscription: {=[u8]:a}, topic: {=[u8]:a}, \
                 kind: {=u8}, version: {=u8}, data: {=usize} bytes }}",
                source,
                subscription,
                topic,
                kind,
                version,
                data.len()
            ),
            Self::ServiceReply { id, service, data } => defmt::write!(
                f,
                "ServiceReply {{ id: {=u32}, service: {=[u8]:a}, data: {=usize} bytes }}",
                id,
                service,
                data.len()
            ),
            Self::ServiceTimeout { id, service } => defmt::write!(
                f,
                "ServiceTimeout {{ id: {=u32}, service: {=[u8]:a} }}",
                id,
                service
            ),
            Self::PowerInfoReply { id, reply } => {
                defmt::write!(f, "PowerInfoReply {{ id: {=u32}, reply: {} }}", id, reply)
            }
        }
    }
}

/// A frame the node wants transmitted, and the ports it goes out on.
///
/// Mutable because stamping the egress port rewrites it, once per port. The
/// borrow is of whichever buffer the frame lives in: the node's transmit buffer
/// for something it built, or the caller's receive buffer for a frame being
/// relayed.
#[derive(Debug)]
pub struct Outbound<'a> {
    pub(super) frame: &'a mut [u8],
    pub(super) mask: u16,
}

/// The frame as its length, as [`Event`]'s payloads are.
#[cfg(feature = "defmt")]
impl defmt::Format for Outbound<'_> {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(
            f,
            "Outbound {{ frame: {=usize} bytes, mask: {=u16:#06b} }}",
            self.frame.len(),
            self.mask
        );
    }
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

// By hand: the derive's bounds on `Option<Outbound<'f>>` are ambiguous.
#[cfg(feature = "defmt")]
impl defmt::Format for Owed<'_, '_> {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(
            f,
            "Owed {{ relay: {}, reply: {}, forward: {} }}",
            self.relay,
            self.reply,
            self.forward
        );
    }
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
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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
