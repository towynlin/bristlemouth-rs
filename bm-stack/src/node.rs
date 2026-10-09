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
//! other port. Config and DFU use the same path.
//!
//! # Requests and replies
//!
//! Everything the node sends goes through
//! [`bm_wire::bcmp::registry::Registry`], which is `bcmp/packet.c`'s state:
//! which message types exist, what sequence number an outgoing message
//! carries, which outstanding requests a reply may answer, and when an
//! unanswered one is re-sent and then given up on. [`Node::send`] is
//! `bcmp_tx`, [`Node::request`] is `bcmp_tx` with no number to echo, and what
//! comes back arrives as an [`Event`] — the three exits of
//! `process_received_message` plus the timeout the expiry sweep reports. The
//! sweep's re-sends come out of [`Node::next_retransmission`].
//!
//! A type nothing registers is neither sent nor dispatched, which is the C's
//! `BmENODEV`. [`Node::new`] registers what `bcmp_init` registers for the
//! ported modules — heartbeat, ping, system time, device info, the neighbour
//! table and resource discovery; [`Node::register`] adds more.
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
//! # So is the neighbour table, differently
//!
//! `bcmp/neighbors.c` correlates its replies too, from three statics that hold
//! one request between them. [`Node::request_neighbor_table`] is
//! `bcmp_request_neighbor_table` and the statics are
//! [`bm_wire::bcmp::neighbors::TableRequests`]. A reply claiming the node that
//! was asked arrives as [`Event::NeighborTable`]; the 1 s timer the request
//! arms arrives as [`Event::NeighborTableTimeout`] and does **not** end the
//! request. Two things are asymmetric with the responder side and are
//! divergences rather than choices: a broadcast request is answered by every
//! node and accepted from none (#35), and the timeout disarms nothing (#36).
//!
//! # And the resource table, differently again
//!
//! `bcmp/resource_discovery.c` keeps two append-only lists of topic names —
//! [`Node::resources`], which [`Node::add_resource`] grows — and answers a
//! `0x0A` with all of both. [`Node::request_resource_table`] is
//! `bcmp_resource_discovery_send_request` and `RESOURCE_REQUEST_LIST` is
//! [`bm_wire::bcmp::resource::ResourceRequests`]; a reply arrives as
//! [`Event::ResourceTable`].
//!
//! Two things here are this module's alone. A request naming node zero is
//! answered by **nobody**, where the same request to `bcmp/info.c`,
//! `bcmp/ping.c` or `bcmp/neighbors.c` is answered by everybody — divergence
//! #37. And a reply is accepted only when the `node_id` in its body equals the
//! address it arrived from, which is the only place in BCMP those two are
//! compared.
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
//! # DFU runs beside the rest
//!
//! bm_core runs DFU on a task of its own. A DFU frame addressed to this node
//! is queued by [`Node::on_frame`] and run by [`Node::next_dfu_transmission`],
//! which [`Node::run`] drains after everything else; one for another node
//! arriving link-local is re-flooded as config's are. The node is a DFU
//! client, and a host once [`Node::dfu_initiate_update`] starts an update;
//! the finish callback arrives as [`Event::DfuUpdateFinished`].
//!
//! # UDP
//!
//! [`Node::on_frame`] hands a UDP datagram addressed to a port
//! [`Node::bind_udp`] bound to the application as [`Event::Udp`], after the
//! relay decision, as `bm_l2_process_rx_evt` submits after it relays.
//! Datagrams to any other port are dropped, and those to [`pubsub::PORT`] go
//! to pub/sub. The frame is taken by
//! [`bm_wire::udp::accept`], which follows lwIP rather than `bm_linux.c`
//! (divergence #72): no checksum check, and no filter on the destination
//! address. [`Node::send_udp`] is `bm_udp_tx_perform` through
//! `bm_l2_link_output`.
//!
//! # Pub/sub
//!
//! `middleware/pubsub.c` and the part of `middleware/middleware.c` it uses.
//! [`pubsub::PORT`] is bound from construction, as `bm_pubsub_init` binds it.
//!
//! | C | Here |
//! |---|---|
//! | `bm_sub_wl` | [`Node::subscribe`]; adds a `SUB` resource on every success |
//! | `bm_unsub_wl` | [`Node::unsubscribe`]; the resource stays |
//! | `bm_pub_wl` | [`Node::publish`]: local delivery first, then the frame to `FF03::1` from and to [`pubsub::PORT`]; adds a `PUB` resource once the frame is built |
//! | `middleware_net_task`, then `bm_handle_msg` | [`Node::on_frame`]: a datagram to [`pubsub::PORT`] **from** [`pubsub::PORT`] is decoded and reported as one [`Event::Publication`] per matching subscription; from any other port it is dropped (divergence #73) |
//!
//! The C has a list of callbacks per topic; a node here has two, its
//! application and the service layer, listed per topic as the C lists them
//! ([`Subscriptions`], divergence #79). A publication [`pubsub::decode`]
//! refuses is dropped, where `bm_handle_msg` reads past it (divergence #75).
//!
//! # Services
//!
//! `middleware/bm_service.c` and `echo_service.c`. [`Node::register_service`]
//! lists a service answered by the application's [`Services`], and
//! [`Node::register_echo_service`] lists echo; each subscribes the service
//! layer to `<name>/req`. A request reaching that subscription is matched
//! against the list by [`ServiceTable::lookup`], answered, and the reply
//! published to `<name>/rep` comes back in [`Owed::reply`], as the C builds
//! it inside the subscriber callback. One reply per received publication;
//! see [`Node::on_frame_with`] and divergence #89.
//!
//! The metrics service is listed at construction, before any other, unless
//! [`Services::METRICS`] is false.
//!
//! A publication this node makes is not dispatched to its own services, so
//! a request to one of them goes unanswered and times out, where a C node
//! answers it from its middleware task.
//!
//! `middleware/bm_service_request.c` is [`Node::service_request`]: it lists
//! the request in [`Node::service_requests`], subscribes `<service>/rep` and
//! publishes `<service>/req`. A reply reaching that subscription arrives as
//! [`Event::ServiceReply`], and silence as [`Event::ServiceTimeout`] from
//! [`Node::on_service_expiry`]. A request [`Node::power_info_request`] made
//! ends as `power_info_service.c`'s callback queue says instead:
//! [`Event::PowerInfoReply`] or nothing.
//!
//! # Three timers, not one
//!
//! The 10-second heartbeat timer is [`Node::on_tick`]; `packet.c`'s 150 ms
//! expiry sweep is [`Node::on_expiry`]; `bm_service_request.c`'s 500 ms
//! sweep is [`Node::on_service_expiry`]. Each sweep carries its own phase
//! (see divergence #22), so it only has to be called at least once per
//! period. Putting either on a grid of the port's own would make the port
//! retry and give up on requests at different moments from a C node.

use bm_wire::BmWireError;
use bm_wire::addr;
use bm_wire::bcmp::config::ConfigHeader;
use bm_wire::bcmp::dfu::DfuAddress;
use bm_wire::bcmp::info::{
    DeviceInfoReply, DeviceInfoRequest, InfoCache, InfoCacheView, InfoRequestKind, InfoRequests,
    InfoRequestsView,
};
use bm_wire::bcmp::neighbors::{
    NeighborTableReply, NeighborTableRequest, TableReplyOutcome, TableRequests,
};
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::registry::{
    Delivery, Expiry, MESSAGE_TIMER_EXPIRY_PERIOD_MS, PacketCfg, Registry, RegistryError,
    RegistryView,
};
use bm_wire::bcmp::resource::{
    RESOURCE_NAME_BYTES, ResourceReplyOutcome, ResourceRequests, ResourceRequestsView,
    ResourceTable, ResourceTableReply, ResourceTableRequest, ResourceTableView,
};
use bm_wire::bcmp::time::SystemTimeHeader;
use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, Heartbeat, MessageType, forward, rx, tx};
use bm_wire::frame::{self, IP_PROTO_BCMP, MIN_FRAME_WITH_ADDRESSES};
use bm_wire::l2;
use bm_wire::l2_policy;
use bm_wire::neighbor::{HEARTBEAT_PERIOD_S, NeighborTable, NeighborTableView, heartbeat_for};
use bm_wire::pubsub::{self, Subscriptions, SubscriptionsView};
use bm_wire::service::ServiceTable;
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::config::{Configuration, NoConfig};
use crate::dfu::NodeDfu;
use crate::port::{DfuSlot, Identity, NoDfu, NoInitRam, NoRtc, Rtc};
use crate::service::{
    NoServices, PowerInfoCallbacks, SERVICE_NAME_BYTES, SERVICES, ServiceHandler, ServiceRequests,
    Services,
};

mod bcmp;
mod config;
mod dfu;
mod event;
mod held;
mod pubsub_api;
mod run;
mod services;
mod udp_api;

pub use event::{
    Event, Outbound, Owed, PublishError, Reflood, SpotterError, SubscribeError, UdpBindError,
};
pub use run::{deliver, transmit};

use held::{HeldRequest, HeldRequests, PingState, Snapshot, Stamp};

#[cfg(doc)]
use bm_wire::{bcmp::resource::ResourceAddError, pubsub::SubscriptionError};

/// Largest frame the node will build or accept.
///
/// 1514 is a 1500-byte Ethernet payload plus the 14-byte header, which is what
/// `bcmp_max_payload_size_bytes` in `bcmp/bcmp.h` works out to.
pub const MTU: usize = 1514;

pub use bm_wire::addr::LINK_LOCAL_PREFIX;
pub use bm_wire::frame::HOP_LIMIT;

/// How many message types a node's registry holds.
///
/// bm_core registers thirty-three across the eight modules `bcmp_init` brings
/// up, each calling `packet_add` once per type it handles and `bm_dfu_init`
/// `0xD9` twice (divergence #56). This is that with room to spare;
/// [`Node::register`] reports [`RegistryError::Full`] past it. bm_core has no
/// such ceiling — its registry is a `bm_malloc`'d list.
pub const MESSAGE_TYPES: usize = 40;

/// How often [`Node::on_expiry`] must be called, `message_timer_expiry_period_ms`.
///
/// Not a deadline the node schedules against: the sweep's phase lives in the
/// registry, so this is only the *longest* a caller may leave between calls.
pub const EXPIRY_PERIOD_MS: u32 = MESSAGE_TIMER_EXPIRY_PERIOD_MS;

/// How long a neighbour-table request waits before
/// [`Node::on_neighbor_request_timer`] reports
/// [`Event::NeighborTableTimeout`], `bcmp_neighbor_timer_timeout_s`.
///
/// Unlike [`EXPIRY_PERIOD_MS`] this *is* a deadline: the C's timer is a
/// one-shot armed by the request. What it is not is a give-up — see
/// divergence #36.
pub const NEIGHBOR_REQUEST_TIMEOUT_MS: u32 = bm_wire::bcmp::neighbors::NEIGHBOR_REQUEST_TIMEOUT_MS;

/// How many UDP ports a node can [`Node::bind_udp`] at once.
///
/// bm_core binds one, `BM_MIDDLEWARE_PORT` 4321, for pub/sub; its UDP list is
/// unbounded.
pub const UDP_PORTS: usize = 4;

/// Default number of unanswered device-info requests a node remembers,
/// [`NodeResources`]' `INFO_REQUESTS`.
///
/// bm_core's `INFO_REQUEST_LIST` is unbounded and never expires an entry
/// (divergence #19), so there is no C number to match. This is a ceiling the
/// port adds: past it a request still goes out, but its reply is unsolicited
/// and nothing is cached.
pub const INFO_REQUESTS_DEFAULT: usize = 8;

/// Default number of resources a node's [`ResourceTable`] holds across both
/// of its lists, [`NodeResources`]' `RESOURCES`.
///
/// bm_core's `PUB_LIST` and `SUB_LIST` are `bm_malloc`'d and have no ceiling.
/// A node advertising more topics than this raises the parameter;
/// [`Node::add_resource`] reports [`ResourceAddError::Full`] rather than
/// truncating a name, because a truncated name is a different name. One is
/// the metrics service's subscription ([`Services::METRICS`]).
pub const RESOURCES_DEFAULT: usize = 9;

/// Default number of unanswered resource-table requests a node remembers,
/// [`NodeResources`]' `RESOURCE_REQUESTS`.
///
/// `RESOURCE_REQUEST_LIST` is unbounded and never expires an entry, as
/// `INFO_REQUEST_LIST` is (divergence #19), so there is no C number to match.
pub const RESOURCE_REQUESTS_DEFAULT: usize = 4;

/// Default number of topics a node can subscribe to at once,
/// [`NodeResources`]' `SUBSCRIPTIONS`.
///
/// bm_core's `CTX.subscription_list` is `bm_malloc`'d and unbounded.
/// [`Node::subscribe`] reports [`SubscriptionError::Full`] past it. One is
/// the metrics service's ([`Services::METRICS`]).
pub const SUBSCRIPTIONS_DEFAULT: usize = 9;

/// Default size of a node's expected-ping-payload buffer,
/// [`NodeResources`]' `PING_PAYLOAD`.
///
/// bm_core keeps this on the heap, reallocated per request, so it has no
/// ceiling but `bcmp_tx`'s. A node that wants to ping with more than this
/// raises the parameter.
pub const PING_PAYLOAD_BYTES: usize = 64;

/// The memory a [`Node`] runs in: its tables and its frame buffers.
///
/// The firmware owns it, usually in a `static_cell::StaticCell`, and lends it
/// to [`Node::new`] for as long as the node runs. Every ceiling is set here,
/// where the memory is declared, and nowhere else:
///
/// | Parameter | Default | What it bounds |
/// |---|---|---|
/// | `RESOURCES` | [`RESOURCES_DEFAULT`] | resources advertised across both of `bcmp/resource_discovery.c`'s lists |
/// | `SUBSCRIPTIONS` | [`SUBSCRIPTIONS_DEFAULT`] | topics [`Node::subscribe`] holds at once |
/// | `NEIGHBORS` | 4 | the neighbour table and the device-info cache |
/// | `PENDING` | 4 | requests awaiting a reply |
/// | `PING_PAYLOAD` | [`PING_PAYLOAD_BYTES`] | the longest payload [`Node::ping`] sends |
/// | `INFO_REQUESTS` | [`INFO_REQUESTS_DEFAULT`] | unanswered device-info requests |
/// | `RESOURCE_REQUESTS` | [`RESOURCE_REQUESTS_DEFAULT`] | unanswered resource-table requests |
///
/// The parameters are ordered by how often a node changes them, so a board
/// that changes only `RESOURCES` writes `NodeResources<16>`.
///
/// `NEIGHBORS` must be at least the PHY's port count, since bm_core keeps one
/// neighbour per port.
///
/// `PENDING`: bm_core's list is unbounded and discards a `bm_malloc` failure,
/// so a full list here does the same: the request goes out untracked and its
/// reply arrives as ordinary traffic. See
/// [`Outgoing::tracked`][bm_wire::bcmp::registry::Outgoing::tracked]. Each
/// outstanding request's frame is kept for re-sending, so this costs
/// `PENDING` times [`MTU`] bytes.
///
/// `PING_PAYLOAD` is the longest ping payload the node can remember well
/// enough to check a reply against. bm_core has no equivalent limit, only an
/// unchecked `bm_malloc` whose failure it dereferences. This is the one place
/// ping's behaviour here is a choice rather than a port.
///
/// The others are ceilings bm_core does not have. Device-info strings are kept
/// to [`CACHED_STRING_BYTES`](bm_wire::bcmp::info::CACHED_STRING_BYTES), which keeps every string whole, and resource
/// names and topics to [`RESOURCE_NAME_BYTES`].
pub struct NodeResources<
    const RESOURCES: usize = RESOURCES_DEFAULT,
    const SUBSCRIPTIONS: usize = SUBSCRIPTIONS_DEFAULT,
    const NEIGHBORS: usize = 4,
    const PENDING: usize = 4,
    const PING_PAYLOAD: usize = PING_PAYLOAD_BYTES,
    const INFO_REQUESTS: usize = INFO_REQUESTS_DEFAULT,
    const RESOURCE_REQUESTS: usize = RESOURCE_REQUESTS_DEFAULT,
> {
    neighbors: NeighborTable<NEIGHBORS>,
    registry: Registry<MESSAGE_TYPES, PENDING>,
    held: [HeldRequest; PENDING],
    due: [u32; PENDING],
    ping: [u8; PING_PAYLOAD],
    info_requests: InfoRequests<INFO_REQUESTS>,
    info: InfoCache<NEIGHBORS>,
    resources: ResourceTable<RESOURCES>,
    resource_requests: ResourceRequests<RESOURCE_REQUESTS>,
    subscriptions: Subscriptions<SUBSCRIPTIONS, RESOURCE_NAME_BYTES>,
    tx: [u8; MTU],
}

impl<
    const RESOURCES: usize,
    const SUBSCRIPTIONS: usize,
    const NEIGHBORS: usize,
    const PENDING: usize,
    const PING_PAYLOAD: usize,
    const INFO_REQUESTS: usize,
    const RESOURCE_REQUESTS: usize,
>
    NodeResources<
        RESOURCES,
        SUBSCRIPTIONS,
        NEIGHBORS,
        PENDING,
        PING_PAYLOAD,
        INFO_REQUESTS,
        RESOURCE_REQUESTS,
    >
{
    /// Empty tables and zeroed buffers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            neighbors: NeighborTable::new(),
            registry: Registry::new(),
            held: [HeldRequest::EMPTY; PENDING],
            due: [0; PENDING],
            ping: [0; PING_PAYLOAD],
            info_requests: InfoRequests::new(),
            info: InfoCache::new(),
            resources: ResourceTable::new(),
            resource_requests: ResourceRequests::new(),
            subscriptions: Subscriptions::new(),
            tx: [0; MTU],
        }
    }

    /// Return every table to [`Self::new`]'s state, in place.
    fn clear(&mut self) {
        self.neighbors = NeighborTable::new();
        self.registry = Registry::new();
        for held in &mut self.held {
            held.len = 0;
        }
        self.due = [0; PENDING];
        self.ping = [0; PING_PAYLOAD];
        self.info_requests = InfoRequests::new();
        self.info = InfoCache::new();
        self.resources = ResourceTable::new();
        self.resource_requests = ResourceRequests::new();
        self.subscriptions = Subscriptions::new();
        self.tx = [0; MTU];
    }
}

impl<
    const RESOURCES: usize,
    const SUBSCRIPTIONS: usize,
    const NEIGHBORS: usize,
    const PENDING: usize,
    const PING_PAYLOAD: usize,
    const INFO_REQUESTS: usize,
    const RESOURCE_REQUESTS: usize,
> Default
    for NodeResources<
        RESOURCES,
        SUBSCRIPTIONS,
        NEIGHBORS,
        PENDING,
        PING_PAYLOAD,
        INFO_REQUESTS,
        RESOURCE_REQUESTS,
    >
{
    fn default() -> Self {
        Self::new()
    }
}

/// What a [`Node`] is built from besides its memory: its identity, its clock,
/// and, optionally, a config store, a DFU slot and application services.
///
/// [`Parts::new`] starts with [`NoConfig`], [`NoDfu`] and [`NoServices`]; the
/// `with_` methods replace one each.
#[derive(Debug)]
pub struct Parts<I, R = NoRtc, C = NoConfig, D = NoDfu, S = NoServices> {
    /// Who the node is.
    pub identity: I,
    /// Its real-time clock.
    pub rtc: R,
    /// `CONFIGS` and its flash, which `0xA0`–`0xA9` read and write. With
    /// [`NoConfig`], config messages for other nodes are still forwarded, and
    /// none addressed to this one is answered.
    pub config: C,
    /// The update slot and no-init RAM. With [`NoDfu`], DFU messages for other
    /// nodes are forwarded, and an update request is refused with
    /// `BmDfuErrFlashAccess`.
    pub dfu: D,
    /// The application's service handlers. With [`NoServices`], a service
    /// [`Node::register_service`] lists goes unanswered.
    pub services: S,
}

impl<I, R> Parts<I, R> {
    /// `identity` and `rtc`, with no config store, no DFU slot and no
    /// services.
    pub fn new(identity: I, rtc: R) -> Self {
        Self {
            identity,
            rtc,
            config: NoConfig,
            dfu: NoDfu,
            services: NoServices,
        }
    }
}

impl<I, R, C, D, S> Parts<I, R, C, D, S> {
    /// The same parts, answering config messages from `config`.
    pub fn with_config<C2>(self, config: C2) -> Parts<I, R, C2, D, S> {
        let Self {
            identity,
            rtc,
            dfu,
            services,
            ..
        } = self;
        Parts {
            identity,
            rtc,
            config,
            dfu,
            services,
        }
    }

    /// The same parts, taking DFU updates into `dfu`.
    pub fn with_dfu<D2>(self, dfu: D2) -> Parts<I, R, C, D2, S> {
        let Self {
            identity,
            rtc,
            config,
            services,
            ..
        } = self;
        Parts {
            identity,
            rtc,
            config,
            dfu,
            services,
        }
    }

    /// The same parts, answering application services from `services`.
    pub fn with_services<S2>(self, services: S2) -> Parts<I, R, C, D, S2> {
        let Self {
            identity,
            rtc,
            config,
            dfu,
            ..
        } = self;
        Parts {
            identity,
            rtc,
            config,
            dfu,
            services,
        }
    }
}

/// A Bristlemouth node.
///
/// Built by [`Node::new`] from a [`NodeResources`], which sets every ceiling,
/// and a [`Parts`].
pub struct Node<'r, I, R = NoRtc, C = NoConfig, D = NoDfu, S = NoServices> {
    identity: I,
    rtc: R,
    neighbors: &'r mut NeighborTableView,
    registry: &'r mut RegistryView<MESSAGE_TYPES>,
    /// The frames the registry's outstanding requests were sent in.
    held: HeldRequests<'r>,
    ping: PingState<'r>,
    /// `INFO_REQUEST_LIST`, and the device information the replies to it
    /// carried. bm_core hangs the second off its neighbour table entries and
    /// frees it with them, which is what [`NeighborTable`] evictions do here.
    info_requests: &'r mut InfoRequestsView,
    info: &'r mut InfoCacheView,
    /// `bcmp/neighbors.c`'s `TARGET_NODE_ID`, `NEIGHBOR_REQUEST_CB` and
    /// `NEIGHBOR_TIMER` — one outstanding neighbour-table request, however
    /// many have been sent.
    table_requests: TableRequests,
    /// `PUB_LIST` and `SUB_LIST`, which a `0x0A` is answered with, and
    /// `RESOURCE_REQUEST_LIST`, which correlates the `0x0B`s that come back.
    resources: &'r mut ResourceTableView,
    resource_requests: &'r mut ResourceRequestsView,
    /// `middleware/pubsub.c`'s `CTX.subscription_list`.
    subscriptions: &'r mut SubscriptionsView<RESOURCE_NAME_BYTES>,
    /// `CONFIGS` and its flash, which `0xA0`–`0xA9` read and write.
    config: C,
    /// `dfu_core.c` and `dfu_client.c`, their update slot and no-init RAM.
    dfu: NodeDfu<D>,
    /// `BM_SERVICE_CONTEXT.service_list`.
    service_table: ServiceTable<ServiceHandler, SERVICES, SERVICE_NAME_BYTES>,
    /// The application's service handlers.
    services: S,
    /// `CTX.service_request_list` and its expiry timer.
    service_requests: ServiceRequests,
    /// `power_info_service.c`'s `service_queue`.
    power_info: PowerInfoCallbacks,
    port_count: u8,
    /// Link state per port, bit 0 for port 1. Cached rather than read from the
    /// PHY on demand, so the synchronous half stays free of I/O — the same
    /// arrangement bm_core has, where L2 keeps `enabled_ports_mask` up to date
    /// from link-change callbacks and `bm_l2_get_port_state` only reads it.
    link_mask: u16,
    /// Ports [`Node::bind_udp`] bound, `CTX.udp_list`.
    udp_ports: [Option<u16>; UDP_PORTS],
    tx: &'r mut [u8; MTU],
}

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// A node built from `parts` in `resources`, with an empty neighbour
    /// table, at time zero.
    ///
    /// `resources` is cleared first, so the node starts the same whatever it
    /// last held.
    ///
    /// The registry comes up holding what `bcmp_init` registers for the ported
    /// modules, with the expiry sweep phased from zero — where
    /// [`Node::on_tick`]'s uptime clock starts. The DFU machine comes up in
    /// `Init` with the reboot info [`NoInitRam::load`] returns, and moves on
    /// from it at the first [`Node::next_dfu_transmission`]. The service
    /// request sweep is phased from zero too.
    ///
    /// If [`Services::METRICS`], the metrics service, `<node id>/metrics`, is
    /// listed and `<node id>/metrics/req` subscribed, as `bristlemouth_init`
    /// calls `metrics_service_init` before an application registers
    /// anything (contract 4 of `docs/history/services-todo.md`). A request is
    /// answered with [`Services::metrics`]; see
    /// [`bm_wire::service::metrics::handle`]. Otherwise no service is listed.
    pub fn new<
        const RESOURCES: usize,
        const SUBSCRIPTIONS: usize,
        const NEIGHBORS: usize,
        const PENDING: usize,
        const PING_PAYLOAD: usize,
        const INFO_REQUESTS: usize,
        const RESOURCE_REQUESTS: usize,
    >(
        resources: &'r mut NodeResources<
            RESOURCES,
            SUBSCRIPTIONS,
            NEIGHBORS,
            PENDING,
            PING_PAYLOAD,
            INFO_REQUESTS,
            RESOURCE_REQUESTS,
        >,
        parts: Parts<I, R, C, D, S>,
        port_count: u8,
    ) -> Self {
        resources.clear();
        for (message_type, cfg) in [
            (MessageType::HEARTBEAT, PacketCfg::UNSEQUENCED),
            (MessageType::ECHO_REQUEST, PacketCfg::UNSEQUENCED),
            (MessageType::ECHO_REPLY, PacketCfg::UNSEQUENCED),
            (MessageType::SYSTEM_TIME_REQUEST, PacketCfg::UNSEQUENCED),
            (MessageType::SYSTEM_TIME_RESPONSE, PacketCfg::UNSEQUENCED),
            (MessageType::SYSTEM_TIME_SET, PacketCfg::UNSEQUENCED),
            // `bm_dfu_init`, in its `packet_add` order, `0xD9` twice
            // (divergence #56).
            (MessageType::DFU_START, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_PAYLOAD_REQ, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_PAYLOAD, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_END, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_ACK, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_ABORT, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_HEARTBEAT, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_REBOOT_REQ, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_REBOOT, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_BOOT_COMPLETE, PacketCfg::UNSEQUENCED),
            (MessageType::DFU_BOOT_COMPLETE, PacketCfg::UNSEQUENCED),
            // `bcmp_config_init`, in its `packet_add` order.
            (MessageType::CONFIG_GET, PacketCfg::REQUEST),
            (MessageType::CONFIG_SET, PacketCfg::REQUEST),
            (MessageType::CONFIG_COMMIT, PacketCfg::UNSEQUENCED),
            (MessageType::CONFIG_STATUS_REQUEST, PacketCfg::REQUEST),
            (MessageType::CONFIG_STATUS_RESPONSE, PacketCfg::REPLY),
            (MessageType::CONFIG_DELETE_REQUEST, PacketCfg::REQUEST),
            (MessageType::CONFIG_DELETE_RESPONSE, PacketCfg::REPLY),
            (MessageType::CONFIG_CLEAR_REQUEST, PacketCfg::REQUEST),
            (MessageType::CONFIG_CLEAR_RESPONSE, PacketCfg::REPLY),
            (MessageType::CONFIG_VALUE, PacketCfg::REPLY),
            (MessageType::NEIGHBOR_TABLE_REQUEST, PacketCfg::UNSEQUENCED),
            (MessageType::NEIGHBOR_TABLE_REPLY, PacketCfg::UNSEQUENCED),
            (MessageType::DEVICE_INFO_REQUEST, PacketCfg::UNSEQUENCED),
            (MessageType::DEVICE_INFO_REPLY, PacketCfg::UNSEQUENCED),
            (MessageType::RESOURCE_TABLE_REQUEST, PacketCfg::UNSEQUENCED),
            (MessageType::RESOURCE_TABLE_REPLY, PacketCfg::UNSEQUENCED),
        ] {
            // Cannot fail: MESSAGE_TYPES is larger than this list.
            let _ = resources.registry.add(message_type, cfg);
        }
        let Parts {
            identity,
            rtc,
            config,
            dfu,
            services,
        } = parts;
        let NodeResources {
            neighbors,
            registry,
            held,
            due,
            ping,
            info_requests,
            info,
            resources,
            resource_requests,
            subscriptions,
            tx,
        } = resources;
        let dfu = NodeDfu::new(identity.node_id(), dfu);
        let mut node = Self {
            identity,
            rtc,
            neighbors,
            registry,
            held: HeldRequests::new(held, due),
            ping: PingState::new(ping),
            info_requests,
            info,
            table_requests: TableRequests::new(),
            resources,
            resource_requests,
            subscriptions,
            config,
            dfu,
            service_table: ServiceTable::new(),
            services,
            service_requests: ServiceRequests::new(),
            power_info: PowerInfoCallbacks::new(),
            port_count,
            link_mask: 0,
            udp_ports: [None; UDP_PORTS],
            tx,
        };
        if S::METRICS {
            // Fails only for ceilings of zero, as `metrics_service_init`'s
            // failure is a `bm_malloc` failure; the node runs without it.
            let _ = node.register_metrics_service();
        }
        node
    }

    /// Register a message type, as each module's init does with `packet_add`.
    ///
    /// A newly ported exchange registers its types here, with the flags its
    /// C module uses. Until a type is registered the node will
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
    pub fn registry(&self) -> &RegistryView<MESSAGE_TYPES> {
        self.registry
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
    pub fn neighbors(&self) -> &NeighborTableView {
        self.neighbors
    }

    /// This node's link-local address, the source of every BCMP frame it
    /// sends. [`Node::send_udp`] sends from [`udp::source_address`].
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
    /// 2. if it does, a UDP datagram to a bound port is reported as
    ///    [`Event::Udp`], a publication reaches its subscribers, and anything
    ///    else is validated as BCMP and answered.
    ///
    /// A publication reaching the service layer's subscription is a service
    /// request, and [`Owed::reply`] carries the reply. The C calls the
    /// service callback once for each time it is listed on a matching
    /// subscription and publishes one reply per call; each call finds the
    /// same service, so this calls the handler once and sends one reply
    /// (divergence #89).
    ///
    /// `frame` is mutated in place, as bm_core mutates it. When a relay is owed
    /// the frame comes back as the C's forwarded copy — the whole ports byte
    /// cleared, everything else as it arrived — and [`Owed::relay`] borrows it.
    /// Anything else is dropped silently, as in bm_core; a dropped frame can
    /// still be relayed, since the two decisions are made by different layers.
    ///
    /// bm_core's L2 also takes a link-local routing callback, consulted for
    /// link-local multicast that is not `FF02::1`. `bm_middleware_init`
    /// registers `handle_middleware_routing`, which defers to the application
    /// whose destination address is the frame's. Pub/sub's is `FF03::1`, never
    /// link-local, so the callback returns true and leaves the egress mask
    /// zero, which is what no callback does. This passes `None`: such a frame
    /// is submitted locally and relayed nowhere.
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
        if let Ok(datagram) = udp::accept(frame) {
            if datagram.dst_port == pubsub::PORT {
                // `middleware_net_task` looks the application up by the
                // source port (divergence #73).
                if datagram.src_port == pubsub::PORT
                    && let Ok(publication) = pubsub::decode(datagram.payload)
                {
                    let reply =
                        self.deliver_publication(now_ms, datagram.source, &publication, events);
                    return (reply, None);
                }
            } else if self.udp_bound(datagram.dst_port) {
                events(Event::Udp {
                    port: datagram.dst_port,
                    src_port: datagram.src_port,
                    source: datagram.source,
                    payload: datagram.payload,
                });
            }
            return (None, None);
        }
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
                self.held.release(request.seq_num);
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
            MessageType::NEIGHBOR_TABLE_REPLY => {
                // `bcmp_process_neighbor_table_reply`. The declared entry
                // counts are checked here and nowhere in the C -- divergence
                // #14, whose worse half is `topology.c`'s.
                let Ok(table) = NeighborTableReply::decode(received.payload) else {
                    return (None, None);
                };
                // `TARGET_NODE_ID == reply->node_id`: the id the *body*
                // claims, matched whole, against a slot that is never cleared
                // and that a timeout does not disarm.
                match self.table_requests.accept(table.node_id) {
                    TableReplyOutcome::Reported => events(Event::NeighborTable {
                        source,
                        reply: table,
                    }),
                    // `Accepted`: the target matched but no callback was
                    // armed, so the C stops the timer and drops the reply.
                    // `Rejected`: it names another node, and the C returns
                    // before even the timer stop.
                    TableReplyOutcome::Accepted | TableReplyOutcome::Rejected => {}
                }
                None
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
            MessageType::RESOURCE_TABLE_REQUEST => {
                let Ok(request) = ResourceTableRequest::decode(received.payload) else {
                    return (None, None);
                };
                // Not `addressed_to_us`:
                // `bcmp_process_resource_discovery_request` breaks unless the
                // target is an exact match, so a request naming zero is
                // answered by nobody where every other request type in BCMP
                // takes zero as a broadcast. Divergence #37.
                if !request.is_for(self.identity.node_id()) {
                    return (None, None);
                }
                self.build_resource_table_reply(now_ms, &reply_to)
            }
            MessageType::RESOURCE_TABLE_REPLY => {
                // `bcmp_process_resource_discovery_reply`. The record lengths
                // are checked here and nowhere in the C -- divergence #14
                // again, and worse than its other two cases: each record's
                // length advances the cursor for the next one.
                let Ok(reply) = ResourceTableReply::decode(received.payload) else {
                    return (None, None);
                };
                // `repl->node_id == src_node_id` first, then `ll_get_item`
                // and `ll_remove` on the low 32 bits of that source. The
                // module transmits nothing either way.
                match self.resource_requests.accept(reply.node_id, source) {
                    ResourceReplyOutcome::Reported => {
                        events(Event::ResourceTable { source, reply });
                    }
                    // `Accepted`: consumed, and `cb->cb` was null, so the C
                    // only printed it. `Unsolicited`: the claim agreed with
                    // the source but nothing had asked. `Mismatched`: the
                    // body named another node, and the C returned before the
                    // list was consulted.
                    ResourceReplyOutcome::Accepted
                    | ResourceReplyOutcome::Unsolicited
                    | ResourceReplyOutcome::Mismatched => {}
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
            MessageType::CONFIG_GET
            | MessageType::CONFIG_VALUE
            | MessageType::CONFIG_SET
            | MessageType::CONFIG_COMMIT
            | MessageType::CONFIG_STATUS_REQUEST
            | MessageType::CONFIG_STATUS_RESPONSE
            | MessageType::CONFIG_DELETE_REQUEST
            | MessageType::CONFIG_DELETE_RESPONSE
            | MessageType::CONFIG_CLEAR_REQUEST
            | MessageType::CONFIG_CLEAR_RESPONSE => {
                // `bcmp_process_config_message` reads the 16-byte header
                // without consulting `data.size` (divergence #51).
                let Ok(header) = ConfigHeader::decode(received.payload) else {
                    return (None, None);
                };
                if !header.is_for(self.identity.node_id()) {
                    // `should_forward`. Zero is not a broadcast here: it is
                    // forwarded like any other node's id.
                    return (
                        None,
                        Some(Reflood {
                            start: BCMP_HEADER_OFFSET,
                            end: BCMP_HEADER_OFFSET + BCMP_HEADER_LEN + received.payload.len(),
                            ingress_port: received.ingress_port,
                        }),
                    );
                }
                let config_reply = self.process_config(
                    now_ms,
                    message_type,
                    header.source_node_id,
                    seq_num,
                    received.payload,
                );
                return (config_reply, None);
            }
            MessageType::DFU_START
            | MessageType::DFU_PAYLOAD_REQ
            | MessageType::DFU_PAYLOAD
            | MessageType::DFU_END
            | MessageType::DFU_ACK
            | MessageType::DFU_ABORT
            | MessageType::DFU_HEARTBEAT
            | MessageType::DFU_REBOOT_REQ
            | MessageType::DFU_REBOOT
            | MessageType::DFU_BOOT_COMPLETE => {
                // `dfu_copy_and_process_message`, which reads the address
                // without consulting `data.size` (divergence #55).
                let Ok(address) = DfuAddress::of_body(received.payload) else {
                    return (None, None);
                };
                if address.dst_node_id == self.identity.node_id() {
                    // `bm_dfu_process_message`. The machine runs from
                    // `next_dfu_transmission`, as the C's runs on its own
                    // task.
                    let _ = self.dfu.on_message(received.payload);
                    return (None, None);
                }
                if !reply_to.is_link_local_multicast() {
                    return (None, None);
                }
                return (
                    None,
                    Some(Reflood {
                        start: BCMP_HEADER_OFFSET,
                        end: BCMP_HEADER_OFFSET + BCMP_HEADER_LEN + received.payload.len(),
                        ingress_port: received.ingress_port,
                    }),
                );
            }
            _ => None,
        };
        (reply, None)
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
        frame::write_headers(
            frame,
            &addr::nodeid_to_ip(LINK_LOCAL_PREFIX, identity.node_id()),
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            IP_PROTO_BCMP,
            HOP_LIMIT,
            bcmp.len(),
        )
        .ok()?;
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
    /// retries and gives up on requests; drain [`Node::next_retransmission`]
    /// after it. [`Node::on_service_expiry`] runs here for the same reason.
    /// A node that also calls the sweeps on time is unaffected: the phase
    /// decides when a sweep happens, not the call.
    pub fn on_tick_with(
        &mut self,
        uptime_ms: u32,
        mut events: impl FnMut(Event<'_>),
    ) -> Option<Outbound<'_>> {
        // bm_core checks neighbours and sends a heartbeat on the same timer,
        // in that order.
        self.neighbors.check(uptime_ms, |_| {});
        self.sweep(uptime_ms, &mut events);
        self.on_neighbor_request_timer(uptime_ms, &mut events);
        self.on_service_expiry(uptime_ms, &mut events);
        self.build_heartbeat(uptime_ms)
    }

    /// Run `packet.c`'s expiry sweep, reporting every request that gave up.
    ///
    /// Call it at least every [`EXPIRY_PERIOD_MS`]. It is
    /// `sequence_list_timer_callback`, and the 150 ms grid it fires on — not
    /// the 24 ms a request is stamped with — decides when a request is retried
    /// and when it dies (divergence #22). The phase lives in the registry, so
    /// calling this early, late or twice changes nothing; only a skipped sweep
    /// does, and that leaves a request unretried that a C node would have
    /// retried.
    ///
    /// A request is re-sent on each of its first
    /// [`PACKET_RETRY_COUNT`][bm_wire::bcmp::registry::PACKET_RETRY_COUNT]
    /// expiries and reported as [`Event::Timeout`] on the next. The re-sends
    /// are queued rather than returned: call [`Node::next_retransmission`]
    /// until it returns `None` before handling anything else, since the C puts
    /// them on the wire from inside the sweep.
    pub fn on_expiry(&mut self, now_ms: u32, mut events: impl FnMut(Event<'_>)) {
        self.sweep(now_ms, &mut events);
        self.on_neighbor_request_timer(now_ms, &mut events);
    }

    /// The next request frame a sweep owes the network again, or `None`.
    ///
    /// `timer_traverse_cb`'s `PACKET.cb.send(element->buf)`: the frame the
    /// request was first sent in, byte for byte, to every port. Frames come
    /// out in the order the sweep retried them. A reply or timeout that
    /// arrives before a queued re-send is taken drops it, so take them all
    /// straight after [`Node::on_expiry`] or [`Node::on_tick`].
    pub fn next_retransmission(&mut self) -> Option<Outbound<'_>> {
        let mask = self.all_ports_mask();
        let frame = self.held.next_due()?;
        Some(Outbound { frame, mask })
    }

    /// `ll_traverse(&PACKET.sequence_list, timer_traverse_cb)`, if a sweep is
    /// due: queue a re-send for each retry and report each timeout.
    fn sweep(&mut self, now_ms: u32, events: &mut impl FnMut(Event<'_>)) {
        let Self { registry, held, .. } = self;
        registry.on_tick(now_ms, |expiry| match expiry {
            Expiry::Retry(request) => held.mark_due(request.seq_num),
            Expiry::TimedOut(request) => {
                held.release(request.seq_num);
                events(Event::Timeout { request });
            }
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
        self.send_with(
            now_ms,
            dst,
            message_type,
            reply_seq_num,
            body.len(),
            |_, buf| {
                buf.get_mut(..body.len())
                    .ok_or(BmWireError::Truncated)?
                    .copy_from_slice(body);
                Ok(body.len())
            },
        )
    }

    /// [`Node::send`] for a body of `body_len` bytes that `encode` writes
    /// straight into the transmit buffer, with the node's [`Configuration`]
    /// to read from.
    fn send_with(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        reply_seq_num: u32,
        body_len: usize,
        encode: impl FnOnce(&C, &mut [u8]) -> Result<usize, BmWireError>,
    ) -> Option<Outbound<'_>> {
        let end = MIN_FRAME_WITH_ADDRESSES
            .checked_add(BCMP_HEADER_LEN)?
            .checked_add(body_len)?;
        if end > MTU {
            return None;
        }
        let stamp = self.outgoing(now_ms, message_type, reply_seq_num)?;
        let Self {
            identity,
            tx,
            held,
            config,
            ..
        } = self;
        build_outbound(
            &mut tx[..],
            held,
            identity.node_id(),
            dst,
            message_type,
            stamp,
            |buf| encode(config, buf),
        )
    }

    /// [`Node::send_with`] to `FF02::1`, which is where every `bcmp_tx` in
    /// `bcmp/config.c` sends.
    fn send_multicast(
        &mut self,
        now_ms: u32,
        message_type: MessageType,
        reply_seq_num: u32,
        body_len: usize,
        encode: impl FnOnce(&C, &mut [u8]) -> Result<usize, BmWireError>,
    ) -> Option<Outbound<'_>> {
        self.send_with(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            message_type,
            reply_seq_num,
            body_len,
            encode,
        )
    }

    /// Send a message that is not answering one — [`Node::send`] with no
    /// sequence number to echo, which is what every one of bm_core's own
    /// request sites passes.
    ///
    /// If `message_type` is registered as a
    /// [`PacketCfg::sequenced_request`], the message carries the node's next
    /// sequence number and is recorded as outstanding: a reply carrying that
    /// number comes back as [`Event::Reply`]. Silence is answered by re-sending
    /// the same frame from each [`Node::on_expiry`] sweep at least
    /// [`DEFAULT_MESSAGE_TIMEOUT_MS`][bm_wire::bcmp::registry::DEFAULT_MESSAGE_TIMEOUT_MS]
    /// after the last send, [`PACKET_RETRY_COUNT`][bm_wire::bcmp::registry::PACKET_RETRY_COUNT]
    /// times, and then by [`Event::Timeout`] from the next.
    pub fn request(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        body: &[u8],
    ) -> Option<Outbound<'_>> {
        self.send(now_ms, dst, message_type, body, 0)
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
    ) -> Option<Stamp> {
        let outgoing = self
            .registry
            .on_serialize(now_ms, message_type, reply_seq_num)
            .ok()?;
        Some(Stamp {
            seq_num: outgoing.seq_num,
            mask: self.all_ports_mask(),
            tracked: outgoing.tracked,
        })
    }

    fn build_heartbeat(&mut self, uptime_ms: u32) -> Option<Outbound<'_>> {
        let stamp = self.outgoing(uptime_ms, MessageType::HEARTBEAT, 0)?;
        let Self {
            identity, tx, held, ..
        } = self;
        let heartbeat = heartbeat_for(uptime_ms, HEARTBEAT_PERIOD_S);
        build_outbound(
            &mut tx[..],
            held,
            identity.node_id(),
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            stamp,
            |body| {
                heartbeat.encode(body)?;
                Ok(Heartbeat::LEN)
            },
        )
    }
}

/// The mask holding just `port`, or nothing for a port the device cannot have.
fn port_mask(port: u8) -> u16 {
    port.checked_sub(1)
        .filter(|bit| *bit < 16)
        .map_or(0, |bit| 1u16 << bit)
}

/// [`tx::build`], handed back as the [`Outbound`] every `build_*` returns.
///
/// A tracked request's frame is also kept in `held`, for re-sending.
fn build_outbound<'a, F>(
    tx: &'a mut [u8],
    held: &mut HeldRequests<'_>,
    node_id: u64,
    dst: &BmIpAddr,
    message_type: MessageType,
    stamp: Stamp,
    body: F,
) -> Option<Outbound<'a>>
where
    F: FnOnce(&mut [u8]) -> Result<usize, BmWireError>,
{
    let end = tx::build(tx, node_id, dst, message_type, stamp.seq_num, body).ok()?;
    let frame = tx.get_mut(..end)?;
    if stamp.tracked {
        held.hold(stamp.seq_num, frame);
    }
    Some(Outbound {
        frame,
        mask: stamp.mask,
    })
}
