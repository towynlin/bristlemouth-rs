//! The memory a node runs in, [`NodeResources`], and what it is built from
//! besides, [`Parts`].

use bm_wire::bcmp::info::{InfoCache, InfoRequests};
use bm_wire::bcmp::registry::Registry;
use bm_wire::bcmp::resource::{RESOURCE_NAME_BYTES, ResourceRequests, ResourceTable};
use bm_wire::neighbor::NeighborTable;
use bm_wire::pubsub::Subscriptions;

use crate::config::NoConfig;
use crate::port::{NoDfu, NoRtc};
use crate::service::NoServices;

use super::{HeldRequest, MESSAGE_TYPES, MTU};

#[cfg(doc)]
use super::Node;
#[cfg(doc)]
use crate::service::Services;
#[cfg(doc)]
use bm_wire::{bcmp::resource::ResourceAddError, pubsub::SubscriptionError};

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
    pub(super) neighbors: NeighborTable<NEIGHBORS>,
    pub(super) registry: Registry<MESSAGE_TYPES, PENDING>,
    pub(super) held: [HeldRequest; PENDING],
    pub(super) due: [u32; PENDING],
    pub(super) ping: [u8; PING_PAYLOAD],
    pub(super) info_requests: InfoRequests<INFO_REQUESTS>,
    pub(super) info: InfoCache<NEIGHBORS>,
    pub(super) resources: ResourceTable<RESOURCES>,
    pub(super) resource_requests: ResourceRequests<RESOURCE_REQUESTS>,
    pub(super) subscriptions: Subscriptions<SUBSCRIPTIONS, RESOURCE_NAME_BYTES>,
    pub(super) tx: [u8; MTU],
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
    pub(super) fn clear(&mut self) {
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
