//! Differential comparator for the service layer on the node:
//! `bm_service_register`, `bm_service_unregister`, `echo_service_init`,
//! `sys_info_service_init`, `config_cbor_map_service_init`,
//! `power_info_service_init`, `metrics_service_init`,
//! `_service_request_received_cb`, `bm_service_request`,
//! `sys_info_service_request`, `config_cbor_map_service_request`,
//! `power_info_service_request`, `metrics_service_request`,
//! `_service_request_cb` and the request expiry sweep against
//! [`Node::register_service`], [`Node::unregister_service`],
//! [`Node::register_echo_service`], [`Node::register_sys_info_service`],
//! [`Node::register_config_map_service`],
//! [`Node::register_power_info_service`], [`Node::with_services`],
//! [`Node::on_frame_with`], [`Node::service_request_with`],
//! [`Node::sys_info_request_with`], [`Node::config_map_request_with`],
//! [`Node::power_info_request_with`], [`Node::metrics_request_with`] and
//! [`Node::on_service_expiry`].
//!
//! A stack target, for the reason [`crate::stack`] gives. Driven from
//! `tests/services.rs`.
//!
//! The oracle and one Rust node are mirrored for the life of the process:
//! `BM_SERVICE_CONTEXT.service_list`, the subscription list and the resource
//! lists have no reset. After every step:
//!
//! | What | C | Rust |
//! |---|---|---|
//! | The call's result | the `bool` or `BmErr` | the `Result` |
//! | Subscriptions, in order | [`oracle_subscriptions`] | [`Node::subscriptions`] |
//! | `PUB_LIST` and `SUB_LIST` | [`oracle_local_resources`] | [`Node::resources`] |
//! | Frames a request provokes | the relay, then one reply per service callback called | [`Owed::relay`](bm_stack::Owed::relay), then [`Owed::reply`](bm_stack::Owed::reply) |
//! | Handler calls | [`c_handler`]'s, one per service callback called | [`StandIn`]'s, one |
//! | Deliveries to the application | `on_publication`'s: the request's, then the replies' | [`Event::Publication`]: the request's, then the reply's |
//! | A request this node makes | `bm_service_request`'s result, its frame and local deliveries | [`Node::service_request_with`]'s |
//! | Answers to requests this node made | [`c_reply_cb`]'s, with `ack` true from a reply and false from the sweep | [`Event::ServiceReply`], [`Event::ServiceTimeout`] |
//! | Answers to power_info requests | which of [`C_POWER_REPLY`] was called, and with what | [`Event::PowerInfoReply`]'s request id, mapped to the callback that request queued |
//! | Power stats read | [`c_power_stats`]'s calls, one per service callback called | [`StandIn`]'s, one |
//! | Metrics components read | [`C_METRIC`]'s calls, one per added component per service callback called | [`StandIn`]'s, one |
//! | `sys_config_crc`, before each sys_info reply | `services_cbor_encoded_as_crc32` | [`ConfigPartition::cbor_map_crc32`](bm_wire::configuration::ConfigPartition::cbor_map_crc32) |
//!
//! Both nodes keep a config store, emptied at the start of each input by
//! [`crate::config`]'s reset and written by [`Step::Configure`]. Both list
//! the metrics service first: `bm_shim_stack_init` and
//! [`Node::with_services`] register it. The oracle's components are
//! [`COMPONENTS`], added once at bring-up; [`Step::Metrics`] sets which of
//! them report and with what, on both sides, and each input starts with none
//! reporting. A sys_info
//! or config_map reply's bytes are compared whole; their decoding on a
//! requester is [`crate::service_codecs`]'s, so [`Summary::sys_info_decoded`]
//! and [`Summary::config_map_decoded`] only count answers the Rust decoders
//! read.
//!
//! Time moves only in [`Step::Wait`], one 500 ms sweep at a time, and at the
//! start of each input, which waits until no request is outstanding
//! (`CTX.service_request_list` has no reset either). The oracle's sweep runs
//! on `timer_callback_handler.c`'s task, which
//! [`stack::start_timer_callback_handler`] starts.
//!
//! Where the C calls the service callback `k` times for one publication
//! (divergence #79, or two service subscriptions matching one topic), it
//! sends `k` identical replies, calls the handler `k` times and delivers the
//! reply locally `k` times; the Rust node does each once (divergence #89).
//! The comparison asserts exactly that.
//!
//! # The domain
//!
//! | Rule | Why |
//! |---|---|
//! | Service names are [`NAMES`], application topics [`APP_TOPICS`]; none holds a NUL, `/req` or `/rep` | `bm_get_subs` stops at a NUL; no service subscription then matches a reply topic, so a local reply delivery calls no service |
//! | Every `SUB` and `PUB` resource is added once at start-up, longest first | divergence #38: a later lookup of a longer name reads past a shorter entry |
//! | No request whose lookup is [`Lookup::OverRead`] or [`Lookup::ShortRequest`] | the C reads past the datagram (divergence #89) |
//! | No publication shorter than a [`ReplyHeader`] reaching a reply subscription | `_service_request_cb` reads past it (divergence #92) |
//! | A reply's data is compared up to what arrived | the C passes `data_size` unchecked; [`c_reply_cb`] reads no further (divergence #92) |
//! | No [`Step::Ask`] that the node's own service would answer | the C answers it from its middleware task; the Rust node does not dispatch its own publications to its services |
//! | Fewer than [`SERVICE_REQUESTS`] requests outstanding; asserted after every step | the Rust node's ceiling |
//! | Timeouts that expire within seconds ([`Timeout`]) | the start of each input waits them out |
//! | No request answered by echo with more than [`REPLY_DATA_LEN`] bytes | the C copies past its buffer (divergence #90) |
//! | No reply to a power_info request whose `data_size` is more than it carries | `power_info_reply_cb` decodes `data_size` bytes, past the publication (divergence #92) |
//! | Under ASan, `strict_memcmp=0` (the `services` fuzz target) | `<id>/sys_info/req` and the peer's sys_info reply topic are longer than `<id>/metrics/req`, which `SUB_LIST` lists first, so each `bm_sub_wl` compares past it (divergence #38); only bytes up to the first difference are checked |
//! | No sys_info request while the system partition's map is [`MapError::Unreachable`] | the C reads it with undefined behaviour (divergences #42, #88) |
//! | No config_map request naming a partition whose map is [`MapError::Unreachable`], or whose `partition_id` is tagged | the C reads either with undefined behaviour (divergences #42, #82, #88) |
//! | At most [`LEAK_BUDGET`] steps per process that leave a listed service nothing can unlist, and only for `x` | see below |
//! | Fewer than [`SERVICES`] services listed, [`CALLBACKS`] callbacks per topic | the Rust node's ceilings |
//!
//! # The service list cannot be reset
//!
//! Registering adds one entry and at most one service callback; unregistering
//! removes one callback and at most one entry. So a step that adds an entry
//! without a callback (registering a name whose request topic's first callback
//! is already the service layer's, divergence #79), or that removes an entry
//! other than the one named (divergence #89), leaves an entry nothing can
//! remove. `reset` unregisters everything else at the start of each input;
//! [`LEAK_BUDGET`] bounds the rest. A stuck entry ends the walk for every
//! topic its name prefixes, and is what unregistering any name prefixing it
//! removes, so only a name sharing a prefix with no other (`x`) may be left
//! stuck (`harmless_if_stuck`).

use std::ffi::CString;
use std::sync::{Mutex, MutexGuard, OnceLock};

use arbitrary::Arbitrary;

use bm_stack::config::Config;
use bm_stack::node::{INFO_REQUESTS_DEFAULT, PING_PAYLOAD_BYTES, RESOURCE_REQUESTS_DEFAULT};
use bm_stack::service::{SERVICE_REQUESTS, SERVICES, ServiceHandler, ServiceRequestError};
use bm_stack::{Event, NoDfu, Node, RamConfigStorage, Services, SoftRtc};
use bm_wire::bcmp::info::CACHED_STRING_BYTES;
use bm_wire::bcmp::resource::{RESOURCE_NAME_BYTES, ResourceType};
use bm_wire::cbor::parser::CborError;
use bm_wire::configuration::{MapError, Partition};
use bm_wire::pubsub::{self as codec, CALLBACKS, Subscriber, SubscriptionError};
use bm_wire::service::config_map::{self, ConfigMapRequest, DecodedConfigMapReply};
use bm_wire::service::metrics::{self, Component, Entry};
use bm_wire::service::power_info::{self, PowerInfoReply};
use bm_wire::service::sys_info::{self, DecodedSysInfoReply, SysInfoReply};
use bm_wire::service::{
    Lookup, MAX_DATA_SIZE, REPLY_DATA_LEN, REPLY_SUFFIX, REQUEST_SUFFIX, ReplyHeader, RequestHeader,
};
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::config::{self as config_diff, Seed};
use crate::l2_egress::port_transmit;
use crate::metrics_codec::{self, FieldIn};
use crate::pubsub::oracle_subscriptions;
use crate::resource::oracle_local_resources;
use crate::stack::{
    self, APP_NAME, GIT_SHA, NUM_PORTS, OracleIdentity, capture, captured_message_type, drain,
    inject, oracle, tick_count,
};
use crate::udp::as_bm_linux_sends_it;

/// The metrics service `bm_shim_stack_init` registers, `<node id>/metrics`.
pub const METRICS: &[u8] = b"c0ffee0012345678/metrics";

/// `echo_service_init`'s name, `<node id>/echo`.
pub const ECHO: &[u8] = b"c0ffee0012345678/echo";

/// `sys_info_service_init`'s name, `<node id>/sys_info`.
pub const SYS_INFO: &[u8] = b"c0ffee0012345678/sys_info";

/// `config_cbor_map_service_init`'s name, `<node id>/config_map`.
pub const CONFIG_MAP: &[u8] = b"c0ffee0012345678/config_map";

/// `power_info_service_init`'s name, the same on every node.
pub const POWER_INFO: &[u8] = power_info::SERVICE;

/// The service [`Step::AskSysInfo`] asks: `sys_info_service_request` of
/// the first of [`PEERS`].
pub const PEER_SYS_INFO: &[u8] = b"0b54ccce5c7978bf/sys_info";

/// The service [`Step::AskConfigMap`] asks: `config_cbor_map_service_request`
/// of the first of [`PEERS`].
pub const PEER_CONFIG_MAP: &[u8] = b"0b54ccce5c7978bf/config_map";

/// The service [`Step::AskMetrics`] asks: `metrics_service_request` of the
/// first of [`PEERS`].
pub const PEER_METRICS: &[u8] = b"0b54ccce5c7978bf/metrics";

/// The metrics components the oracle adds at bring-up, in order, with
/// `metrics_service_add_component`; each is [`C_METRIC`]'s callback of the
/// same index. NUL-terminated: the C keeps the pointer.
pub const COMPONENTS: [&str; 3] = ["memory\0", "network_port_stats\0", "a\0"];

/// Field keys a [`Step::Metrics`] table draws from: one with an interior
/// NUL, the empty key, and a 63-byte key, thirteen fields of which outgrow
/// the reply buffer.
pub const FIELD_KEYS: [&str; 5] = [
    "free_bytes",
    "a",
    "a\0b",
    "",
    "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijk",
];

/// The most fields a [`Step::Metrics`] table keeps.
pub const MAX_FIELDS: usize = 24;

/// Service names. [`ECHO`] is registered with `echo_service_init` and
/// [`Node::register_echo_service`], [`SYS_INFO`] with `sys_info_service_init`
/// and [`Node::register_sys_info_service`], [`CONFIG_MAP`] with
/// `config_cbor_map_service_init` and [`Node::register_config_map_service`],
/// [`POWER_INFO`] with `power_info_service_init` of [`c_power_stats`] and
/// [`Node::register_power_info_service`]; the rest with [`c_handler`] and
/// [`StandIn`].
///
/// `s` prefixes `svc`; `s*` subscribes a pattern that matches other
/// services' request topics; `x` shares a prefix with none, so it is the one
/// name [`LEAK_BUDGET`] may leave listed. `bm_wire::service`'s unit tests
/// cover a name prefixing a built-in's.
///
/// Not the empty name: it prefixes [`METRICS`], which is listed first, so
/// unregistering it removes the metrics service instead (divergence #89), and
/// nothing can then remove it. Listed first, it ends the walk for every
/// request. `bm_wire::service`'s unit tests cover it.
pub const NAMES: [&[u8]; 8] = [
    ECHO, SYS_INFO, CONFIG_MAP, POWER_INFO, b"svc", b"s", b"s*", b"x",
];

/// Topics the application subscribes: a service's request topic, a prefix,
/// everything, echo's request topic, and a peer's sys_info reply topic.
pub const APP_TOPICS: [&[u8]; 5] = [
    b"svc/req",
    b"s",
    b"*",
    b"c0ffee0012345678/echo/req",
    b"0b54ccce5c7978bf/sys_info/rep",
];

/// Services this node asks: a peer's sys_info, config_map and metrics, a
/// pattern whose reply topic matches the peer's reply topics (divergence
/// #74), and [`NAMES`] entries the node may itself list. [`POWER_INFO`] is
/// asked by [`Step::AskPowerInfo`] too.
pub const ASKED: [&[u8]; 6] = [
    PEER_SYS_INFO,
    PEER_CONFIG_MAP,
    b"0*",
    b"svc",
    POWER_INFO,
    PEER_METRICS,
];

/// Steps per process that may leave a listed service no request can unlist.
pub const LEAK_BUDGET: u32 = 4;

/// Topics the Rust node holds: every request, application and asked reply
/// topic.
pub const SUBSCRIPTIONS: usize = 28;

/// Resources the Rust node holds: every request, reply and application topic.
pub const RESOURCES: usize = 48;

/// The node ids requests come from. The second shares the first's low 32
/// bits.
pub const PEERS: [u64; 2] = [0x0b54_ccce_5c79_78bf, 0x7777_7777_5c79_78bf];

/// The mirrored Rust node.
pub type ServicesNode = Node<
    OracleIdentity,
    SoftRtc,
    4,
    4,
    PING_PAYLOAD_BYTES,
    INFO_REQUESTS_DEFAULT,
    CACHED_STRING_BYTES,
    RESOURCES,
    RESOURCE_NAME_BYTES,
    RESOURCE_REQUESTS_DEFAULT,
    SUBSCRIPTIONS,
    Config<RamConfigStorage>,
    NoDfu,
    StandIn,
>;

/// A handler call: `(service, request data)`.
pub type Call = (Vec<u8>, Vec<u8>);

/// A delivery: `(node id, topic, data, type, version)`.
pub type Delivery = (u64, Vec<u8>, Vec<u8>, u8, u8);

/// An answer to a request this node made: `(ack, id, service, data)`.
pub type Answer = (bool, u32, Vec<u8>, Vec<u8>);

/// The answer both sides' application handlers give: the request reversed,
/// no reply to one starting `!`, and no reply to one longer than the buffer.
fn answer(request: &[u8], reply: &mut [u8]) -> Option<usize> {
    if request.first() == Some(&b'!') || request.len() > reply.len() {
        return None;
    }
    for (out, byte) in reply.iter_mut().zip(request.iter().rev()) {
        *out = *byte;
    }
    Some(request.len())
}

/// The Rust node's [`Services`]: `answer`, recording each call, the power
/// stats [`Step::PowerStats`] set and the components [`Step::Metrics`] set,
/// counting each read.
#[derive(Debug, Default)]
pub struct StandIn {
    calls: Vec<Call>,
    power_calls: usize,
    metrics_calls: usize,
}

impl Services for StandIn {
    fn handle(&mut self, service: &[u8], request: &[u8], reply: &mut [u8]) -> Option<usize> {
        assert_eq!(reply.len(), REPLY_DATA_LEN, "the handler's buffer");
        assert!(reply.iter().all(|b| *b == 0), "the buffer is zeroed");
        self.calls.push((service.to_vec(), request.to_vec()));
        answer(request, reply)
    }

    fn power_info(&mut self) -> Option<PowerInfoReply> {
        self.power_calls += 1;
        Some(*lock(&POWER_STATS))
    }

    fn metrics<R>(&mut self, encode: impl FnOnce(&[Component<'_>]) -> R) -> R {
        self.metrics_calls += 1;
        with_components(&lock(&METRIC_TABLES), encode)
    }
}

/// A component's table: `(index into FIELD_KEYS, field)`s.
pub type Table = Vec<(u8, FieldIn)>;

/// The table of each of [`COMPONENTS`], or `None` where its callback fails. Set by
/// [`Step::Metrics`]; each input starts with every one `None`.
pub type Tables = [Option<Table>; COMPONENTS.len()];

/// The tables both sides' components report.
static METRIC_TABLES: Mutex<Tables> = Mutex::new([None, None, None]);

/// The C's view of [`METRIC_TABLES`]: the entries each callback hands out,
/// and the values they point at.
struct CTables {
    luts: Vec<Vec<bm_wire_sys::BmEncoderTableEntry>>,
    /// Each value in an 8-byte slot the C reads at its own width; boxed so
    /// the pointers outlive moves.
    _values: Vec<Box<[u64]>>,
}

// The pointers are into `_values` and `FIELD_KEYS`, which live as long as
// the `CTables`, and are only read under `C_TABLES`'s lock and the oracle's.
unsafe impl Send for CTables {}

static C_TABLES: Mutex<Option<CTables>> = Mutex::new(None);
static C_METRIC_CALLS: Mutex<usize> = Mutex::new(0);

/// `tables` as the Rust node reports them: the components whose table is
/// set, in [`COMPONENTS`] order.
fn with_components<R>(tables: &Tables, encode: impl FnOnce(&[Component<'_>]) -> R) -> R {
    let entries: Vec<(usize, Vec<Entry<'_>>)> = tables
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let t = t.as_ref()?;
            let fields = t
                .iter()
                .map(|(k, f)| Entry {
                    key: field_key(*k),
                    field: f.field(),
                })
                .collect();
            Some((i, fields))
        })
        .collect();
    let components: Vec<Component<'_>> = entries
        .iter()
        .map(|(i, fields)| Component {
            key: COMPONENTS[*i].trim_end_matches('\0'),
            fields,
        })
        .collect();
    encode(&components)
}

fn field_key(k: u8) -> &'static str {
    FIELD_KEYS[usize::from(k) % FIELD_KEYS.len()]
}

/// Set both sides' tables, and rebuild the C's.
fn set_tables(tables: &Tables) {
    let keys: &'static [std::ffi::CString] = {
        static KEYS: OnceLock<Vec<std::ffi::CString>> = OnceLock::new();
        KEYS.get_or_init(|| {
            FIELD_KEYS
                .iter()
                // A key is passed with a NUL appended, so the C sees it up
                // to its first NUL, as the port does.
                .map(|k| {
                    let to_nul = k.split('\0').next().expect("one piece at least");
                    std::ffi::CString::new(to_nul).expect("no NUL")
                })
                .collect()
        })
    };
    let values: Vec<Box<[u64]>> = tables
        .iter()
        .map(|t| {
            t.iter()
                .flatten()
                .map(|(_, f)| metrics_codec::slot(*f))
                .collect()
        })
        .collect();
    let luts = tables
        .iter()
        .zip(&values)
        .map(|(t, v)| {
            t.iter()
                .flatten()
                .zip(v.iter())
                .map(|((k, f), slot)| bm_wire_sys::BmEncoderTableEntry {
                    key: keys[usize::from(*k) % FIELD_KEYS.len()].as_ptr(),
                    type_: metrics_codec::c_type(*f),
                    value_source: (&raw const *slot).cast(),
                })
                .collect()
        })
        .collect();
    *lock(&C_TABLES) = Some(CTables {
        luts,
        _values: values,
    });
    *lock(&METRIC_TABLES) = tables.clone();
}

/// The oracle's `MetricComponentDataCb` for [`COMPONENTS`]`[K]`: its table
/// from [`METRIC_TABLES`], or `BmENODEV` where that is `None`, which
/// `metrics_collect_component` skips. Counts each call.
unsafe extern "C" fn c_metric<const K: usize>(
    metric_key: *const core::ffi::c_char,
    lut: *mut *const bm_wire_sys::BmEncoderTableEntry,
    num_fields: *mut usize,
) -> bm_wire_sys::BmErr {
    *lock(&C_METRIC_CALLS) += 1;
    // `metrics_collect_component` passes the key it was added with.
    let key = unsafe { core::ffi::CStr::from_ptr(metric_key) };
    assert_eq!(key.to_bytes_with_nul(), COMPONENTS[K].as_bytes());
    if lock(&METRIC_TABLES)[K].is_none() {
        return bm_wire_sys::BmErr_BmENODEV;
    }
    let tables = lock(&C_TABLES);
    let table = &tables.as_ref().expect("set at bring-up").luts[K];
    // The table lives in `C_TABLES` until the next `set_tables`, after the
    // handler has encoded it.
    unsafe {
        *lut = table.as_ptr();
        *num_fields = table.len();
    }
    bm_wire_sys::BmErr_BmOK
}

/// The oracle's `MetricComponentDataCb`s, one per [`COMPONENTS`] entry.
pub const C_METRIC: [unsafe extern "C" fn(
    *const core::ffi::c_char,
    *mut *const bm_wire_sys::BmEncoderTableEntry,
    *mut usize,
) -> bm_wire_sys::BmErr; COMPONENTS.len()] = [c_metric::<0>, c_metric::<1>, c_metric::<2>];

/// The power stats both sides' callbacks return; [`Step::PowerStats`] sets
/// them, and each input starts from the default.
static POWER_STATS: Mutex<PowerInfoReply> = Mutex::new(PowerInfoReply {
    total_on_s: 0,
    remaining_on_s: 0,
    upcoming_off_s: 0,
});
static C_POWER_CALLS: Mutex<usize> = Mutex::new(0);
/// Calls of [`C_POWER_REPLY`]: `(which, reply)`.
static C_POWER: Mutex<Vec<(usize, PowerInfoReply)>> = Mutex::new(Vec::new());

/// The oracle's `BmPowerInfoStatsCb`: the power stats [`Step::PowerStats`]
/// set, counting each call.
///
/// # Safety
///
/// Called by `power_info_request_cb`.
pub unsafe extern "C" fn c_power_stats(
    _arg: *mut core::ffi::c_void,
) -> bm_wire_sys::PowerInfoReplyData {
    *lock(&C_POWER_CALLS) += 1;
    let d = *lock(&POWER_STATS);
    bm_wire_sys::PowerInfoReplyData {
        total_on_s: d.total_on_s,
        remaining_on_s: d.remaining_on_s,
        upcoming_off_s: d.upcoming_off_s,
    }
}

/// The `BmPowerInfoReplyCb` `K`, recording its call.
unsafe extern "C" fn c_power_reply<const K: usize>(
    d: *const bm_wire_sys::PowerInfoReplyData,
) -> bm_wire_sys::BmErr {
    // `power_info_reply_cb` passes its decoded struct.
    let d = unsafe { &*d };
    lock(&C_POWER).push((
        K,
        PowerInfoReply {
            total_on_s: d.total_on_s,
            remaining_on_s: d.remaining_on_s,
            upcoming_off_s: d.upcoming_off_s,
        },
    ));
    bm_wire_sys::BmErr_BmOK
}

/// The oracle's `BmPowerInfoReplyCb`s, told apart by index, as the C passes
/// a callback no context. Each [`Step::AskPowerInfo`] queues one not already
/// queued; at most [`SERVICE_REQUESTS`] − 1 requests wait.
pub const C_POWER_REPLY: [unsafe extern "C" fn(
    *const bm_wire_sys::PowerInfoReplyData,
) -> bm_wire_sys::BmErr; SERVICE_REQUESTS] = [
    c_power_reply::<0>,
    c_power_reply::<1>,
    c_power_reply::<2>,
    c_power_reply::<3>,
    c_power_reply::<4>,
    c_power_reply::<5>,
    c_power_reply::<6>,
    c_power_reply::<7>,
];

static C_CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());
static DELIVERED: Mutex<Vec<Delivery>> = Mutex::new(Vec::new());
static C_ANSWERS: Mutex<Vec<Answer>> = Mutex::new(Vec::new());
/// The bytes after the reply header in the publication being injected:
/// what [`c_reply_cb`] may read.
static AVAILABLE: Mutex<usize> = Mutex::new(0);

fn lock<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// The oracle's `BmServiceHandler` for every name but [`ECHO`]: `answer`,
/// recording each call.
///
/// # Safety
///
/// Called by `_service_request_received_cb` with its own arguments.
pub unsafe extern "C" fn c_handler(
    service_strlen: usize,
    service: *const core::ffi::c_char,
    req_data_len: usize,
    req_data: *mut u8,
    buffer_len: *mut usize,
    reply_data: *mut u8,
) -> bool {
    // The C checked `data_len == 8 + data_size`, so the request is in the
    // datagram, and the reply buffer is `*buffer_len` bytes.
    let (service, request, reply) = unsafe {
        (
            std::slice::from_raw_parts(service.cast::<u8>(), service_strlen).to_vec(),
            std::slice::from_raw_parts(req_data, req_data_len).to_vec(),
            std::slice::from_raw_parts_mut(reply_data, *buffer_len),
        )
    };
    assert_eq!(reply.len(), REPLY_DATA_LEN, "the C handler's buffer");
    lock(&C_CALLS).push((service, request.clone()));
    match answer(&request, reply) {
        Some(len) => {
            unsafe { *buffer_len = len };
            true
        }
        None => false,
    }
}

/// The oracle's `BmServiceReplyCb` for every request: records the answer,
/// reading a reply no further than the publication being injected (divergence #92).
///
/// # Safety
///
/// Called by `_service_request_cb` or the expiry sweep with their own
/// arguments.
pub unsafe extern "C" fn c_reply_cb(
    ack: bool,
    msg_id: u32,
    service_strlen: usize,
    service: *const core::ffi::c_char,
    reply_len: usize,
    reply_data: *mut u8,
) -> bool {
    let service =
        unsafe { std::slice::from_raw_parts(service.cast::<u8>(), service_strlen) }.to_vec();
    let data = if ack {
        let len = reply_len.min(*lock(&AVAILABLE));
        unsafe { std::slice::from_raw_parts(reply_data, len) }.to_vec()
    } else {
        assert!(reply_data.is_null() && reply_len == 0, "a timeout's data");
        Vec::new()
    };
    lock(&C_ANSWERS).push((ack, msg_id, service, data));
    true
}

unsafe extern "C" fn on_publication(
    node_id: u64,
    topic: *const core::ffi::c_char,
    topic_len: u16,
    data: *const u8,
    data_len: u16,
    kind: u8,
    version: u8,
) {
    // Every publication reaching this callback is well-formed.
    let (topic, data) = unsafe {
        (
            std::slice::from_raw_parts(topic.cast::<u8>(), usize::from(topic_len)).to_vec(),
            std::slice::from_raw_parts(data, usize::from(data_len)).to_vec(),
        )
    };
    lock(&DELIVERED).push((node_id, topic, data, kind, version));
}

/// [`NAMES`] as C strings. `bm_service_register` keeps the pointer, so they
/// live as long as the process.
fn c_name(name: &[u8]) -> *const core::ffi::c_char {
    static NAMES_C: OnceLock<Vec<CString>> = OnceLock::new();
    let names = NAMES_C.get_or_init(|| {
        NAMES
            .iter()
            .map(|n| CString::new(*n).expect("no NUL"))
            .collect()
    });
    names
        .iter()
        .find(|c| c.as_bytes() == name)
        .expect("a pool name")
        .as_ptr()
}

fn request_topic(name: &[u8]) -> Vec<u8> {
    [name, REQUEST_SUFFIX].concat()
}

fn reply_topic(name: &[u8]) -> Vec<u8> {
    [name, REPLY_SUFFIX].concat()
}

/// A topic a request is published on.
#[derive(Debug, Clone, Arbitrary)]
pub enum RequestTopic {
    /// `<name>/req` of a [`NAMES`] entry, reduced modulo its length.
    Service(u8),
    /// The same, with bytes after it (cut to 8).
    Suffixed(u8, Vec<u8>),
    /// A [`NAMES`] entry alone.
    Bare(u8),
    /// Any bytes (cut to 32).
    Raw(Vec<u8>),
    /// `<node id>/metrics/req`, [`METRICS`]' request topic.
    Metrics,
}

impl RequestTopic {
    fn bytes(&self) -> Vec<u8> {
        let name = |i: &u8| NAMES[usize::from(*i) % NAMES.len()];
        match self {
            Self::Service(i) => request_topic(name(i)),
            Self::Suffixed(i, extra) => {
                [&request_topic(name(i))[..], &extra[..extra.len().min(8)]].concat()
            }
            Self::Bare(i) => name(i).to_vec(),
            Self::Raw(bytes) => bytes[..bytes.len().min(32)].to_vec(),
            Self::Metrics => request_topic(METRICS),
        }
    }
}

/// `data_size` against the data's length.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum Size {
    /// The data's length.
    Exact,
    /// The data's length plus this, saturating.
    Off(i8),
    /// This.
    Raw(u32),
}

impl Size {
    fn of(self, len: usize) -> u32 {
        match self {
            Self::Exact => len as u32,
            Self::Off(off) => (len as i64 + i64::from(off)).clamp(0, i64::from(u32::MAX)) as u32,
            Self::Raw(size) => size,
        }
    }
}

/// A peer's service request.
#[derive(Debug, Clone, Arbitrary)]
pub struct Request {
    /// The port it arrives on, reduced to 1..=[`NUM_PORTS`].
    pub ingress: u8,
    /// Which of [`PEERS`] sends it.
    pub peer: bool,
    /// The topic.
    pub topic: RequestTopic,
    /// The header's `id`.
    pub id: u32,
    /// The header's `data_size`.
    pub size: Size,
    /// The data, cut to what fits [`codec::MAX_MESSAGE_LEN`].
    pub data: Vec<u8>,
    /// Cut the body (header and data) to this many bytes.
    pub cut: Option<u16>,
}

impl Request {
    /// The publication's topic and body.
    fn publication(&self) -> (Vec<u8>, Vec<u8>) {
        let topic = self.topic.bytes();
        let room = codec::MAX_MESSAGE_LEN - codec::HEADER_LEN - topic.len() - RequestHeader::LEN;
        let data = &self.data[..self.data.len().min(room)];
        let data_size = self.size.of(data.len());
        let mut body = vec![0u8; RequestHeader::LEN];
        RequestHeader {
            id: self.id,
            data_size,
        }
        .encode(&mut body)
        .expect("eight bytes");
        body.extend_from_slice(data);
        if let Some(cut) = self.cut {
            body.truncate(usize::from(cut));
        }
        (topic, body)
    }

    fn source(&self) -> u64 {
        PEERS[usize::from(self.peer)]
    }
}

/// `timeout_s` for a request: one that expires within seconds.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum Timeout {
    /// 0 to 3 seconds.
    Seconds(u8),
    /// 4 294 968 to 4 294 970 seconds, which wrap to 704 to 2704 ms
    /// (divergence #91).
    Wrapped(u8),
    /// 2 147 486 to 4 294 966 seconds: more than 2^31 ms, which
    /// `time_remaining` reads as overdue at the next sweep (divergence #91).
    Overdue(u32),
}

impl Timeout {
    fn seconds(self) -> u32 {
        match self {
            Self::Seconds(s) => u32::from(s % 4),
            Self::Wrapped(k) => 4_294_968 + u32::from(k % 3),
            Self::Overdue(x) => 2_147_486 + x % 2_147_481,
        }
    }
}

/// A request this node makes.
#[derive(Debug, Clone, Arbitrary)]
pub struct Ask {
    /// An [`ASKED`] entry.
    pub service: u8,
    /// The data, cut to [`MAX_DATA_SIZE`] + 8 bytes.
    pub data: Vec<u8>,
    /// The timeout.
    pub timeout: Timeout,
}

/// A reply's `target_node_id`.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum Target {
    /// This node.
    Us,
    /// One of [`PEERS`].
    Peer(bool),
    /// Any id.
    Raw(u64),
}

/// A reply's `id`.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum ReplyId {
    /// An outstanding request's, reduced modulo how many there are; the
    /// next id if there are none.
    Waiting(u8),
    /// Any id.
    Raw(u32),
}

/// A topic a reply is published on.
#[derive(Debug, Clone, Arbitrary)]
pub enum ReplyTopic {
    /// `<name>/rep` of an [`ASKED`] entry.
    Asked(u8),
    /// Any bytes (cut to 32).
    Raw(Vec<u8>),
}

/// A peer's reply to a request this node made.
#[derive(Debug, Clone, Arbitrary)]
pub struct Reply {
    /// The port it arrives on, reduced to 1..=[`NUM_PORTS`].
    pub ingress: u8,
    /// Which of [`PEERS`] sends it.
    pub peer: bool,
    /// The topic.
    pub topic: ReplyTopic,
    /// The header's `target_node_id`.
    pub target: Target,
    /// The header's `id`.
    pub id: ReplyId,
    /// The header's `data_size`.
    pub size: Size,
    /// The data, cut to what fits [`codec::MAX_MESSAGE_LEN`].
    pub data: Vec<u8>,
    /// Cut the body (header and data) to this many bytes.
    pub cut: Option<u16>,
}

impl Reply {
    fn publication(&self, node: &ServicesNode) -> (Vec<u8>, Vec<u8>) {
        let topic = match &self.topic {
            ReplyTopic::Asked(i) => reply_topic(ASKED[usize::from(*i) % ASKED.len()]),
            ReplyTopic::Raw(bytes) => bytes[..bytes.len().min(32)].to_vec(),
        };
        let room = codec::MAX_MESSAGE_LEN - codec::HEADER_LEN - topic.len() - ReplyHeader::LEN;
        let data = &self.data[..self.data.len().min(room)];
        let target_node_id = match self.target {
            Target::Us => stack::NODE_ID,
            Target::Peer(peer) => PEERS[usize::from(peer)],
            Target::Raw(id) => id,
        };
        let requests = node.service_requests();
        let id = match self.id {
            ReplyId::Waiting(i) if !requests.is_empty() => requests
                .iter()
                .nth(usize::from(i) % requests.len())
                .expect("in range")
                .id(),
            ReplyId::Waiting(_) => requests.next_id(),
            ReplyId::Raw(id) => id,
        };
        let mut body = vec![0u8; ReplyHeader::LEN];
        ReplyHeader {
            target_node_id,
            id,
            data_size: self.size.of(data.len()),
        }
        .encode(&mut body)
        .expect("sixteen bytes");
        body.extend_from_slice(data);
        if let Some(cut) = self.cut {
            body.truncate(usize::from(cut));
        }
        (topic, body)
    }
}

/// One step, applied to the oracle and the Rust node.
#[derive(Debug, Clone, Arbitrary)]
pub enum Step {
    /// Register a [`NAMES`] entry.
    Register(u8),
    /// Unregister a [`NAMES`] entry.
    Unregister(u8),
    /// The application subscribes an [`APP_TOPICS`] entry.
    Subscribe(u8),
    /// The application unsubscribes an [`APP_TOPICS`] entry.
    Unsubscribe(u8),
    /// A peer's request.
    Request(Request),
    /// This node asks a service.
    Ask(Ask),
    /// A peer's reply.
    Reply(Reply),
    /// Let this many milliseconds pass, modulo 2000.
    Wait(u16),
    /// Store a key on both, as `bm-wire-diff/src/config.rs` seeds. A
    /// system key changes `sys_config_crc`.
    Configure(Seed),
    /// This node asks [`PEER_SYS_INFO`] with `sys_info_service_request` and
    /// [`Node::sys_info_request_with`].
    AskSysInfo(Timeout),
    /// A peer's request to [`CONFIG_MAP`] for a `partition_id`, as
    /// `config_cbor_map_service_request` encodes it. A [`Step::Request`]'s
    /// data is rarely a CBOR map.
    RequestConfigMap {
        /// The port it arrives on, reduced to 1..=[`NUM_PORTS`].
        ingress: u8,
        /// Which of [`PEERS`] sends it.
        peer: bool,
        /// The header's `id`.
        id: u32,
        /// The partition.
        partition_id: PartitionId,
    },
    /// This node asks [`PEER_CONFIG_MAP`] for a `partition_id` with
    /// `config_cbor_map_service_request` and [`Node::config_map_request_with`].
    AskConfigMap(PartitionId, Timeout),
    /// Set the power stats both sides' power_info services send.
    PowerStats(u32, u32, u32),
    /// This node asks [`POWER_INFO`] with `power_info_service_request` and
    /// [`Node::power_info_request_with`].
    AskPowerInfo(Timeout),
    /// A peer's power_info reply that decodes: a [`Step::Reply`] on
    /// [`POWER_INFO`]'s reply topic to this node, carrying these stats. A
    /// [`Step::Reply`]'s data rarely decodes.
    ReplyPowerInfo {
        /// The port it arrives on, reduced to 1..=[`NUM_PORTS`].
        ingress: u8,
        /// Which of [`PEERS`] sends it.
        peer: bool,
        /// The header's `id`.
        id: ReplyId,
        /// `total_on_s`, `remaining_on_s` and `upcoming_off_s`.
        stats: (u32, u32, u32),
    },
    /// Set which of [`COMPONENTS`] report to both sides' metrics services,
    /// and with what; tables are cut to [`MAX_FIELDS`].
    Metrics(Tables),
    /// This node asks [`PEER_METRICS`] with `metrics_service_request` and
    /// [`Node::metrics_request_with`].
    AskMetrics(Timeout),
}

/// A config_map request's `partition_id`.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum PartitionId {
    /// 0 to 3: an unknown id or one of `config_map::PARTITION_ID_*`.
    Small(u8),
    /// Any id.
    Raw(u32),
}

impl PartitionId {
    fn id(self) -> u32 {
        match self {
            Self::Small(id) => u32::from(id % 4),
            Self::Raw(id) => id,
        }
    }
}

/// Steps run in order, after `reset`.
#[derive(Debug, Clone, Arbitrary)]
pub struct ServicesInput {
    /// The steps.
    pub steps: Vec<Step>,
}

/// What a run did, for tests to assert coverage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Steps outside the domain, not run.
    pub skipped: usize,
    /// Requests answered: one per reply the Rust node sent.
    pub replies: usize,
    /// The most service callbacks one request reached on the oracle.
    pub most_calls: usize,
    /// Requests this node made.
    pub asked: usize,
    /// Answers with `ack` true.
    pub answered: usize,
    /// Answers with `ack` false.
    pub timeouts: usize,
    /// Replies the sys_info service sent.
    pub sys_info_replies: usize,
    /// Answers to [`PEER_SYS_INFO`] that [`DecodedSysInfoReply`] reads.
    pub sys_info_decoded: usize,
    /// Replies the config_map service sent.
    pub config_map_replies: usize,
    /// Of those, the ones with `success` set.
    pub config_map_successes: usize,
    /// Answers to [`PEER_CONFIG_MAP`] that [`DecodedConfigMapReply`] reads.
    pub config_map_decoded: usize,
    /// Replies the power_info service sent.
    pub power_info_replies: usize,
    /// Power_info callbacks called.
    pub power_info_reported: usize,
    /// Of those, the ones called for a request other than the one answered
    /// (divergence #96).
    pub power_info_crossed: usize,
    /// Replies the metrics service sent.
    pub metrics_replies: usize,
    /// Of those, the ones to a request carrying data (divergence #97).
    pub metrics_replies_with_data: usize,
    /// Of those, the ones with at least one component.
    pub metrics_with_components: usize,
    /// Requests the metrics service sent no reply to: a reply over its
    /// buffer, or a string field (divergence #85).
    pub metrics_refused: usize,
    /// Answers to [`PEER_METRICS`].
    pub metrics_answered: usize,
}

/// What the oracle and the Rust node hold between inputs.
pub struct State {
    node: ServicesNode,
    /// What is left of [`LEAK_BUDGET`].
    budget: u32,
    /// `(request id, index into C_POWER_REPLY)` of each power_info callback
    /// queued.
    power_callbacks: Vec<(u32, usize)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Take the oracle and the mirrored state, bringing both up on first use.
///
/// # Panics
///
/// If the oracle's lists are not what `bm_shim_stack_init` leaves, or the
/// start-up publications or subscriptions disagree.
fn state() -> (MutexGuard<'static, ()>, MutexGuard<'static, Option<State>>) {
    let guard = oracle();
    let mut state = lock(&STATE);
    if state.is_none() {
        stack::start_timer_callback_handler();
        assert_eq!(
            tick_count(),
            0,
            "both request sweeps are phased from bring-up"
        );
        let metrics_request = request_topic(METRICS);
        let (pubs, subs, _) = oracle_local_resources();
        assert!(pubs.is_empty(), "PUB_LIST: {pubs:?}");
        assert_eq!(subs, vec![metrics_request.clone()], "SUB_LIST");
        assert_eq!(oracle_subscriptions(), subs, "the subscription list");
        set_tables(&Tables::default());
        for (key, cb) in COMPONENTS.iter().zip(C_METRIC) {
            let err = unsafe {
                bm_wire_sys::metrics_service_add_component(key.as_ptr().cast(), Some(cb), 0)
            };
            assert_eq!(err, bm_wire_sys::BmErr_BmOK, "{key:?}");
        }

        let node = Node::with_services(
            OracleIdentity,
            SoftRtc::new(),
            config_diff::reset(&[]),
            NoDfu,
            StandIn::default(),
            NUM_PORTS,
        );
        assert!(
            node.service_table()
                .iter()
                .eq([(METRICS, ServiceHandler::Metrics)]),
            "the metrics service, listed at construction"
        );
        let mut state_ = State {
            node,
            budget: LEAK_BUDGET,
            power_callbacks: Vec::new(),
        };
        advertise_everything(&mut state_);
        *state = Some(state_);
    }
    (guard, state)
}

/// Add every `SUB` and `PUB` resource a step can add, longest first
/// (divergence #38): subscribe and unsubscribe the application to each
/// topic, and publish nothing to each reply topic.
fn advertise_everything(state: &mut State) {
    let mut subs: Vec<Vec<u8>> = NAMES
        .iter()
        .map(|n| request_topic(n))
        .chain(APP_TOPICS.iter().map(|t| t.to_vec()))
        .chain(ASKED.iter().map(|n| reply_topic(n)))
        .collect();
    subs.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    subs.dedup();
    for topic in &subs {
        assert_eq!(app_subscribe(state, topic), Ok(()), "{topic:?}");
        assert_eq!(app_unsubscribe(state, topic), Ok(()), "{topic:?}");
    }

    let mut pubs: Vec<Vec<u8>> = NAMES
        .iter()
        .map(|n| reply_topic(n))
        .chain(ASKED.iter().map(|n| request_topic(n)))
        .collect();
    pubs.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    pubs.dedup();
    for topic in &pubs {
        let err = unsafe {
            bm_wire_sys::bm_pub_wl(
                topic.as_ptr().cast(),
                topic.len() as u16,
                [].as_ptr(),
                0,
                0,
                codec::COMMON_VERSION,
            )
        };
        assert_eq!(err, bm_wire_sys::BmErr_BmOK, "{topic:?}");
        stack::pump_until_quiet();
        let c = drain();
        let rs = state
            .node
            .publish(topic, 0, codec::COMMON_VERSION, &[])
            .map(capture)
            .expect("publishes");
        let mut payload = vec![0u8; codec::HEADER_LEN + topic.len()];
        codec::encode(&mut payload, topic, 0, codec::COMMON_VERSION, &[]).expect("a topic");
        assert_eq!(
            c,
            as_bm_linux_sends_it(
                codec::PORT,
                &BmIpAddr::GLOBAL_MULTICAST,
                codec::PORT,
                &payload
            )
        );
        assert_eq!(rs, rust_sends(&payload));
    }
    assert!(take_delivered().is_empty());
    assert_lists(state, "start-up");
}

/// What the Rust node transmits for a publication's payload: from
/// [`udp::source_address`], to `FF03::1`, from and to [`codec::PORT`].
fn rust_sends(payload: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let dst = BmIpAddr::GLOBAL_MULTICAST;
    let mut frame = vec![0u8; udp::PAYLOAD_OFFSET + payload.len()];
    udp::build(
        &mut frame,
        &udp::source_address(stack::NODE_ID, &dst),
        &dst,
        codec::PORT,
        codec::PORT,
        payload,
    )
    .expect("sized for the payload");
    port_transmit(&frame)
}

fn take_delivered() -> Vec<Delivery> {
    std::mem::take(&mut *lock(&DELIVERED))
}

fn take_c_calls() -> Vec<Call> {
    std::mem::take(&mut *lock(&C_CALLS))
}

/// Run every step, after `reset`.
///
/// # Panics
///
/// On any divergence.
pub fn check(input: &ServicesInput) -> Summary {
    let (_guard, mut state) = state();
    let state = state.as_mut().expect("brought up");
    reset(state);
    let mut summary = Summary::default();
    for step in &input.steps {
        let ran = match step {
            Step::Register(i) => register(state, NAMES[usize::from(*i) % NAMES.len()]),
            Step::Unregister(i) => unregister(state, NAMES[usize::from(*i) % NAMES.len()]),
            Step::Subscribe(i) => {
                let topic = APP_TOPICS[usize::from(*i) % APP_TOPICS.len()];
                let full = state
                    .node
                    .subscriptions()
                    .callbacks(topic)
                    .is_some_and(|c| c.len() == CALLBACKS && c[0] != Subscriber::Application);
                if !full {
                    assert_eq!(app_subscribe(state, topic), Ok(()), "{topic:?}");
                }
                !full
            }
            Step::Unsubscribe(i) => {
                let topic = APP_TOPICS[usize::from(*i) % APP_TOPICS.len()];
                let _ = app_unsubscribe(state, topic);
                true
            }
            Step::Request(request) => {
                let (topic, body) = request.publication();
                let ingress = request.ingress % NUM_PORTS + 1;
                receive(
                    state,
                    ingress,
                    request.source(),
                    &topic,
                    &body,
                    &mut summary,
                )
            }
            Step::RequestConfigMap {
                ingress,
                peer,
                id,
                partition_id,
            } => {
                let request = Request {
                    ingress: *ingress,
                    peer: *peer,
                    topic: RequestTopic::Service(name_index(CONFIG_MAP)),
                    id: *id,
                    size: Size::Exact,
                    data: config_map_request(partition_id.id()),
                    cut: None,
                };
                let (topic, body) = request.publication();
                let ingress = request.ingress % NUM_PORTS + 1;
                receive(
                    state,
                    ingress,
                    request.source(),
                    &topic,
                    &body,
                    &mut summary,
                )
            }
            Step::Reply(reply) => {
                let (topic, body) = reply.publication(&state.node);
                let ingress = reply.ingress % NUM_PORTS + 1;
                let source = PEERS[usize::from(reply.peer)];
                receive(state, ingress, source, &topic, &body, &mut summary)
            }
            Step::Ask(ask) => self::ask(state, ask, Builtin::None, &mut summary),
            Step::AskSysInfo(timeout) => self::ask(
                state,
                &Ask {
                    service: asked_index(PEER_SYS_INFO),
                    data: Vec::new(),
                    timeout: *timeout,
                },
                Builtin::SysInfo,
                &mut summary,
            ),
            Step::AskConfigMap(partition_id, timeout) => self::ask(
                state,
                &Ask {
                    service: asked_index(PEER_CONFIG_MAP),
                    data: config_map_request(partition_id.id()),
                    timeout: *timeout,
                },
                Builtin::ConfigMap(partition_id.id()),
                &mut summary,
            ),
            Step::PowerStats(total_on_s, remaining_on_s, upcoming_off_s) => {
                *lock(&POWER_STATS) = PowerInfoReply {
                    total_on_s: *total_on_s,
                    remaining_on_s: *remaining_on_s,
                    upcoming_off_s: *upcoming_off_s,
                };
                true
            }
            Step::AskPowerInfo(timeout) => self::ask(
                state,
                &Ask {
                    service: asked_index(POWER_INFO),
                    data: Vec::new(),
                    timeout: *timeout,
                },
                Builtin::PowerInfo,
                &mut summary,
            ),
            Step::ReplyPowerInfo {
                ingress,
                peer,
                id,
                stats: (total_on_s, remaining_on_s, upcoming_off_s),
            } => {
                let mut data = [0u8; 64];
                let len = PowerInfoReply {
                    total_on_s: *total_on_s,
                    remaining_on_s: *remaining_on_s,
                    upcoming_off_s: *upcoming_off_s,
                }
                .encode(&mut data)
                .expect("at most 49 bytes");
                let reply = Reply {
                    ingress: *ingress,
                    peer: *peer,
                    topic: ReplyTopic::Asked(asked_index(POWER_INFO)),
                    target: Target::Us,
                    id: *id,
                    size: Size::Exact,
                    data: data[..len].to_vec(),
                    cut: None,
                };
                let (topic, body) = reply.publication(&state.node);
                let ingress = reply.ingress % NUM_PORTS + 1;
                let source = PEERS[usize::from(reply.peer)];
                receive(state, ingress, source, &topic, &body, &mut summary)
            }
            Step::Metrics(tables) => {
                let mut tables = tables.clone();
                for table in tables.iter_mut().flatten() {
                    table.truncate(MAX_FIELDS);
                }
                set_tables(&tables);
                true
            }
            Step::AskMetrics(timeout) => self::ask(
                state,
                &Ask {
                    service: asked_index(PEER_METRICS),
                    data: Vec::new(),
                    timeout: *timeout,
                },
                Builtin::Metrics,
                &mut summary,
            ),
            Step::Configure(seed) => {
                seed.apply_c();
                seed.apply_rust(&mut state.node.config_mut().store);
                true
            }
            Step::Wait(ms) => {
                wait(state, u32::from(*ms % 2000), &mut summary);
                true
            }
        };
        summary.skipped += usize::from(!ran);
        assert_lists(state, &format!("{step:?}"));
        assert!(
            state.node.service_requests().len() < SERVICE_REQUESTS,
            "the request table kept a slot free"
        );
    }
    summary
}

/// The first listed service `name` prefixes, as `_service_list_remove_service`
/// finds it. Pool names hold no NUL, so `strncmp` is a prefix test.
fn first_prefixed<'a>(node: &'a ServicesNode, name: &[u8]) -> Option<&'a [u8]> {
    node.service_table()
        .iter()
        .map(|(listed, _)| listed)
        .find(|listed| listed.starts_with(name))
}

/// Whether `name`, listed for good, would leave every other pool name
/// answerable and unregistrable: it prefixes no other name's request topic,
/// so no request stops at it, and no other name prefixes it, so no other
/// name's unregistration removes it instead.
fn harmless_if_stuck(name: &[u8]) -> bool {
    NAMES.iter().chain([&METRICS]).all(|other| {
        *other == name || !(request_topic(other).starts_with(name) || name.starts_with(other))
    })
}

fn has_service_callback(node: &ServicesNode, name: &[u8]) -> bool {
    node.subscriptions()
        .callbacks(&request_topic(name))
        .is_some_and(|c| c.contains(&Subscriber::Service))
}

/// Return both sides to the metrics service, the application subscribed to
/// nothing, and whatever [`LEAK_BUDGET`] has left listed, through their own
/// front doors.
///
/// Unregisters a name only where that removes the name's own entry, or no
/// entry: anything else leaves an entry nothing can remove.
fn reset(state: &mut State) {
    assert!(drain().is_empty(), "the ring was not drained");
    let _ = take_delivered();
    let _ = take_c_calls();
    assert!(lock(&C_ANSWERS).is_empty(), "an answer was not compared");
    assert!(lock(&C_POWER).is_empty(), "a power answer was not compared");
    *lock(&C_POWER_CALLS) = 0;
    *lock(&POWER_STATS) = PowerInfoReply::default();
    *lock(&C_METRIC_CALLS) = 0;
    set_tables(&Tables::default());
    state.node.services_mut().calls.clear();
    state.node.services_mut().power_calls = 0;
    state.node.services_mut().metrics_calls = 0;
    *state.node.config_mut() = config_diff::reset(&[]);

    // Every timeout `Timeout` allows has passed within 3 s of its request.
    let mut summary = Summary::default();
    for _ in 0..8 {
        if state.node.service_requests().is_empty() {
            break;
        }
        wait(state, 500, &mut summary);
    }
    assert!(
        state.node.service_requests().is_empty(),
        "requests outlived their timeouts"
    );
    assert!(
        state.power_callbacks.is_empty(),
        "power_info callbacks queued"
    );

    for topic in APP_TOPICS {
        while state
            .node
            .subscriptions()
            .callbacks(topic)
            .is_some_and(|c| c.contains(&Subscriber::Application))
        {
            assert_eq!(app_unsubscribe(state, topic), Ok(()), "{topic:?}");
        }
    }
    loop {
        let next = NAMES.iter().find(|name| {
            has_service_callback(&state.node, name)
                && first_prefixed(&state.node, name).is_none_or(|listed| listed == **name)
        });
        let Some(name) = next else { break };
        assert!(unregister(state, name), "{name:?}");
    }
    assert_lists(state, "reset");
}

fn app_subscribe(state: &mut State, topic: &[u8]) -> Result<(), bm_stack::SubscribeError> {
    let err = unsafe {
        bm_wire_sys::bm_sub_wl(
            topic.as_ptr().cast(),
            topic.len() as u16,
            Some(on_publication),
        )
    };
    let rs = state.node.subscribe(topic);
    assert_eq!(
        err == bm_wire_sys::BmErr_BmOK,
        rs.is_ok(),
        "bm_sub_wl {err}, Node::subscribe {rs:?}: {topic:?}"
    );
    rs
}

fn app_unsubscribe(state: &mut State, topic: &[u8]) -> Result<(), SubscriptionError> {
    let err = unsafe {
        bm_wire_sys::bm_unsub_wl(
            topic.as_ptr().cast(),
            topic.len() as u16,
            Some(on_publication),
        )
    };
    let rs = state.node.unsubscribe(topic);
    let expected = match err {
        bm_wire_sys::BmErr_BmOK => Ok(()),
        bm_wire_sys::BmErr_BmEINVAL => Err(SubscriptionError::NotSubscribed),
        bm_wire_sys::BmErr_BmENOENT => Err(SubscriptionError::NoSuchSubscriber),
        _ => panic!("bm_unsub_wl returned {err}"),
    };
    assert_eq!(rs, expected, "unsubscribe {topic:?}");
    rs
}

/// `bm_service_register` (or `echo_service_init`) and its Rust twin. Returns
/// whether it ran.
fn register(state: &mut State, name: &[u8]) -> bool {
    let node = &state.node;
    if node.service_table().len() == SERVICES {
        return false;
    }
    let callbacks = node.subscriptions().callbacks(&request_topic(name));
    let dedupe = callbacks.is_some_and(|c| c[0] == Subscriber::Service);
    if callbacks.is_some_and(|c| c.len() == CALLBACKS && !dedupe) {
        return false;
    }
    if dedupe {
        if state.budget == 0 || !harmless_if_stuck(name) {
            return false;
        }
        state.budget -= 1;
    }
    if name == ECHO {
        unsafe { bm_wire_sys::echo_service_init() };
        state
            .node
            .register_echo_service()
            .expect("echo_service_init registers");
    } else if name == SYS_INFO {
        unsafe { bm_wire_sys::sys_info_service_init() };
        state
            .node
            .register_sys_info_service()
            .expect("sys_info_service_init registers");
    } else if name == CONFIG_MAP {
        unsafe { bm_wire_sys::config_cbor_map_service_init() };
        state
            .node
            .register_config_map_service()
            .expect("config_cbor_map_service_init registers");
    } else if name == POWER_INFO {
        let c = unsafe {
            bm_wire_sys::power_info_service_init(Some(c_power_stats), core::ptr::null_mut())
        };
        assert_eq!(c, bm_wire_sys::BmErr_BmOK, "power_info_service_init");
        state
            .node
            .register_power_info_service()
            .expect("power_info_service_init registers");
    } else {
        let c =
            unsafe { bm_wire_sys::bm_service_register(name.len(), c_name(name), Some(c_handler)) };
        let rs = state.node.register_service(name);
        assert_eq!(c, rs.is_ok(), "register {name:?}: {rs:?}");
    }
    let handler = match name {
        ECHO => ServiceHandler::Echo,
        SYS_INFO => ServiceHandler::SysInfo,
        CONFIG_MAP => ServiceHandler::ConfigMap,
        POWER_INFO => ServiceHandler::PowerInfo,
        _ => ServiceHandler::Application,
    };
    assert_eq!(
        state.node.service_table().iter().last(),
        Some((name, handler)),
        "appended"
    );
    true
}

/// `bm_service_unregister` and its Rust twin. Returns whether it ran.
fn unregister(state: &mut State, name: &[u8]) -> bool {
    if has_service_callback(&state.node, name)
        && first_prefixed(&state.node, name).is_some_and(|listed| listed != name)
    {
        if state.budget == 0 || !harmless_if_stuck(name) {
            return false;
        }
        state.budget -= 1;
    }
    let c = unsafe { bm_wire_sys::bm_service_unregister(name.len(), c_name(name)) };
    let rs = state.node.unregister_service(name);
    assert_eq!(c, rs.is_ok(), "unregister {name:?}: {rs:?}");
    true
}

/// Whether a publication on `topic` reaches a reply subscription.
fn reaches_reply(node: &ServicesNode, topic: &[u8]) -> bool {
    node.subscriptions()
        .matching_callbacks(topic)
        .any(|(_, c)| c.contains(&Subscriber::Reply))
}

/// Inject a peer's publication of `body` on `topic` into both. Returns
/// whether it ran: `false` if it is outside the domain.
fn receive(
    state: &mut State,
    ingress: u8,
    source: u64,
    topic: &[u8],
    body: &[u8],
    summary: &mut Summary,
) -> bool {
    let what = format!("{topic:?} from {source:#x}, body {body:?}");
    let Some(received) = receive_publication(state, ingress, source, topic, body, &what) else {
        return false;
    };
    summary.most_calls = summary.most_calls.max(received.calls);
    summary.replies += usize::from(received.replied);
    let handler = match state.node.service_table().lookup(topic, body) {
        Lookup::Call { handler, .. } if received.calls > 0 && received.replied => Some(handler),
        _ => None,
    };
    summary.sys_info_replies += usize::from(handler == Some(ServiceHandler::SysInfo));
    summary.power_info_replies += usize::from(handler == Some(ServiceHandler::PowerInfo));
    if let Some(with_components) = received.metrics {
        if received.replied {
            summary.metrics_replies += 1;
            summary.metrics_with_components += usize::from(with_components);
            summary.metrics_replies_with_data += usize::from(
                matches!(state.node.service_table().lookup(topic, body), Lookup::Call { data, .. } if !data.is_empty()),
            );
        } else {
            summary.metrics_refused += 1;
        }
    }
    if let Some(success) = received.config_map_success {
        summary.config_map_replies += 1;
        summary.config_map_successes += usize::from(success);
    }
    summary.power_info_reported += received.power_reported;
    summary.power_info_crossed += received.power_crossed;
    count(summary, &received.answers);
    true
}

/// What [`receive_publication`] compared.
struct Received {
    /// How many service callbacks the publication reached on the oracle.
    calls: usize,
    /// Whether it was answered.
    replied: bool,
    /// The answers it gave requests this node made.
    answers: Vec<Answer>,
    /// A config_map reply's `success`.
    config_map_success: Option<bool>,
    /// Power_info callbacks called.
    power_reported: usize,
    /// Of those, the ones called for a request other than the one answered.
    power_crossed: usize,
    /// For a request the metrics service was called for, whether any
    /// component reported.
    metrics: Option<bool>,
}

/// [`receive`]'s comparison, or `None` if the publication is outside the
/// domain.
fn receive_publication(
    state: &mut State,
    ingress: u8,
    source: u64,
    topic: &[u8],
    body: &[u8],
    what: &str,
) -> Option<Received> {
    let (topic, body) = (topic.to_vec(), body.to_vec());
    let node = &state.node;
    let mut answered = None;
    if reaches_reply(node, &topic) {
        let header = ReplyHeader::decode(&body).ok()?;
        let carried = body.len() - ReplyHeader::LEN;
        if header.target_node_id == stack::NODE_ID
            && node.power_info_callbacks().is_waiting(header.id)
        {
            if header.data_size as usize > carried {
                return None;
            }
            answered = Some(header.id);
        }
    }
    let calls: usize = node
        .subscriptions()
        .matching_callbacks(&topic)
        .map(|(_, c)| c.iter().filter(|s| **s == Subscriber::Service).count())
        .sum();

    // The reply the C sends `calls` times, and the handler call it makes as
    // often.
    let mut reply: Option<Vec<u8>> = None;
    let mut call: Option<Call> = None;
    let mut config_map_success = None;
    // Power stats reads per service callback called.
    let mut power_reads = 0;
    // Metrics reads per service callback called, and whether any component
    // reported.
    let mut metrics_reads = 0;
    let mut metrics_seen = None;
    if calls > 0 {
        match node.service_table().lookup(&topic, &body) {
            Lookup::OverRead { .. } | Lookup::ShortRequest { .. } => return None,
            Lookup::Call {
                handler: ServiceHandler::Echo,
                data,
                ..
            } if data.len() > REPLY_DATA_LEN => return None,
            Lookup::Call {
                name,
                handler,
                header,
                data,
                ..
            } => {
                let mut out = vec![0u8; REPLY_DATA_LEN];
                let len = match handler {
                    ServiceHandler::Echo => bm_wire::service::echo(data, &mut out),
                    ServiceHandler::SysInfo => {
                        let crc = sys_config_crc(node)?;
                        let info = SysInfoReply::new(stack::NODE_ID, GIT_SHA, crc, APP_NAME);
                        sys_info::handle(data, &info, &mut out)
                    }
                    ServiceHandler::ConfigMap => {
                        let len = config_map_reply(node, data, &mut out)?;
                        config_map_success = len.map(|len| {
                            let mut d = DecodedConfigMapReply::default();
                            d.decode_into(&out[..len]).expect("the reply decodes");
                            d.success
                        });
                        len
                    }
                    ServiceHandler::PowerInfo => {
                        power_reads = usize::from(data.is_empty());
                        let stats = *lock(&POWER_STATS);
                        power_info::handle(data, || Some(stats), &mut out)
                    }
                    ServiceHandler::Metrics => {
                        metrics_reads = 1;
                        let tables = lock(&METRIC_TABLES).clone();
                        metrics_seen = Some(tables.iter().any(Option::is_some));
                        with_components(&tables, |components| {
                            metrics::handle(stack::NODE_ID, tick_count(), components, &mut out)
                        })
                    }
                    ServiceHandler::Application => {
                        call = Some((name.to_vec(), data.to_vec()));
                        answer(data, &mut out)
                    }
                };
                reply = len.map(|len| {
                    let mut body = vec![0u8; ReplyHeader::LEN];
                    ReplyHeader {
                        target_node_id: source,
                        id: header.id,
                        data_size: len as u32,
                    }
                    .encode(&mut body)
                    .expect("sixteen bytes");
                    body.extend_from_slice(&out[..len]);
                    let topic = reply_topic(name);
                    let mut payload = vec![0u8; codec::HEADER_LEN + topic.len() + body.len()];
                    codec::encode(&mut payload, &topic, 0, codec::COMMON_VERSION, &body)
                        .expect("a topic");
                    payload
                });
            }
            Lookup::NoService | Lookup::LengthMismatch { .. } | Lookup::TopicMismatch { .. } => {}
        }
    }

    let mut payload = vec![0, 0, topic.len() as u8, 0, codec::COMMON_VERSION];
    payload.extend_from_slice(&topic);
    payload.extend_from_slice(&body);
    let frame = crate::frames::udp(
        source,
        &BmIpAddr::GLOBAL_MULTICAST,
        codec::PORT,
        codec::PORT,
        &payload,
    );

    *lock(&AVAILABLE) = body.len().saturating_sub(ReplyHeader::LEN);
    inject(ingress, &frame);
    let c_frames = drain();
    let c_delivered = take_delivered();
    let c_calls = take_c_calls();
    let c_answers = take_c_answers();
    let c_power = take_c_power();
    let c_power_reads = std::mem::take(&mut *lock(&C_POWER_CALLS));
    let c_metric_reads = std::mem::take(&mut *lock(&C_METRIC_CALLS));

    let mut rs_frame = frame.clone();
    let mut rs_delivered = Vec::new();
    let mut rs_answers = Vec::new();
    let mut rs_power = Vec::new();
    let owed = state
        .node
        .on_frame_with(tick_count(), ingress, &mut rs_frame, |event| {
            record(event, &mut rs_delivered, &mut rs_answers, &mut rs_power);
        });
    assert!(owed.forward.is_none());
    let rs_relay = owed.relay.map(capture).unwrap_or_default();
    let rs_reply = owed.reply.map(capture);
    let rs_calls = std::mem::take(&mut state.node.services_mut().calls);
    let rs_power_reads = std::mem::take(&mut state.node.services_mut().power_calls);
    let rs_metrics_reads = std::mem::take(&mut state.node.services_mut().metrics_calls);

    let what = format!("{what}: {calls} service callbacks");
    assert_eq!(rs_answers, c_answers, "answers, {what}");
    compare_power(state, &rs_power, &c_power, &what);
    assert_eq!(
        rs_power_reads, power_reads,
        "Rust power stats reads, {what}"
    );
    assert_eq!(
        c_power_reads,
        power_reads * calls,
        "C power stats reads, {what}"
    );
    assert_eq!(
        rs_metrics_reads, metrics_reads,
        "Rust metrics reads, {what}"
    );
    assert_eq!(
        c_metric_reads,
        metrics_reads * calls * COMPONENTS.len(),
        "C component callbacks, {what}"
    );
    let expected_reply = reply.as_ref().map(|p| rust_sends(p));
    assert_eq!(rs_reply, expected_reply, "Rust reply, {what}");
    assert_eq!(
        rs_calls,
        call.iter().cloned().collect::<Vec<_>>(),
        "Rust handler calls, {what}"
    );
    assert_eq!(
        c_calls,
        vec![call.clone(); calls]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        "C handler calls, {what}"
    );

    let mut expected_c = rs_relay.clone();
    if let Some(payload) = &reply {
        for _ in 0..calls {
            expected_c.extend(as_bm_linux_sends_it(
                codec::PORT,
                &BmIpAddr::GLOBAL_MULTICAST,
                codec::PORT,
                payload,
            ));
        }
    }
    assert_eq!(c_frames, expected_c, "C frames, {what}");

    // The request's deliveries, then the reply's: once from Rust, once per
    // reply from the C.
    let received = deliveries(&state.node, source, &topic, &body);
    let replied = reply.as_ref().map_or_else(Vec::new, |payload| {
        let reply = codec::decode(payload).expect("well-formed");
        deliveries(&state.node, stack::NODE_ID, reply.topic, reply.data)
    });
    assert_eq!(
        rs_delivered,
        [&received[..], &replied[..]].concat(),
        "Rust deliveries, {what}"
    );
    let mut expected_c = received;
    for _ in 0..calls {
        expected_c.extend(replied.iter().cloned());
    }
    assert_eq!(c_delivered, expected_c, "C deliveries, {what}");
    Some(Received {
        calls,
        replied: reply.is_some(),
        answers: c_answers,
        config_map_success,
        power_reported: rs_power.len(),
        power_crossed: rs_power
            .iter()
            .filter(|(id, _)| Some(*id) != answered)
            .count(),
        metrics: metrics_seen,
    })
}

/// A publication's deliveries to the application: one per application
/// callback on each matching subscription, in list order.
fn deliveries(node: &ServicesNode, source: u64, topic: &[u8], data: &[u8]) -> Vec<Delivery> {
    node.subscriptions()
        .matching_callbacks(topic)
        .flat_map(|(_, callbacks)| callbacks.iter())
        .filter(|c| **c == Subscriber::Application)
        .map(|_| {
            (
                source,
                topic.to_vec(),
                data.to_vec(),
                0,
                codec::COMMON_VERSION,
            )
        })
        .collect()
}

fn take_c_answers() -> Vec<Answer> {
    std::mem::take(&mut *lock(&C_ANSWERS))
}

fn take_c_power() -> Vec<(usize, PowerInfoReply)> {
    std::mem::take(&mut *lock(&C_POWER))
}

/// Compare the power_info callbacks called: the Rust node's, named by the
/// request that queued each, against the oracle's, by which of
/// [`C_POWER_REPLY`] it is. Then forget the callbacks no longer queued.
fn compare_power(
    state: &mut State,
    rs: &[(u32, PowerInfoReply)],
    c: &[(usize, PowerInfoReply)],
    what: &str,
) {
    let rs: Vec<(usize, PowerInfoReply)> = rs
        .iter()
        .map(|(id, reply)| {
            let (_, k) = state
                .power_callbacks
                .iter()
                .find(|(queued, _)| queued == id)
                .expect("a queued callback");
            (*k, *reply)
        })
        .collect();
    assert_eq!(rs, c, "power_info callbacks, {what}");
    let queued: Vec<u32> = state.node.power_info_callbacks().queued().collect();
    state.power_callbacks.retain(|(id, _)| queued.contains(id));
    assert_eq!(
        state.power_callbacks.len(),
        queued.len(),
        "callbacks queued, {what}"
    );
}

/// File a Rust event as a delivery, an answer or a power_info callback.
fn record(
    event: Event<'_>,
    delivered: &mut Vec<Delivery>,
    answers: &mut Vec<Answer>,
    power: &mut Vec<(u32, PowerInfoReply)>,
) {
    match event {
        Event::Publication {
            source,
            topic,
            kind,
            version,
            data,
            ..
        } => delivered.push((source, topic.to_vec(), data.to_vec(), kind, version)),
        Event::ServiceReply { id, service, data } => {
            answers.push((true, id, service.to_vec(), data.to_vec()));
        }
        Event::ServiceTimeout { id, service } => {
            answers.push((false, id, service.to_vec(), Vec::new()));
        }
        Event::PowerInfoReply { id, reply } => power.push((id, reply)),
        _ => {}
    }
}

/// Which requester [`ask`] calls.
#[derive(Debug, Clone, Copy)]
enum Builtin {
    /// `bm_service_request` and [`Node::service_request_with`].
    None,
    /// `sys_info_service_request` and [`Node::sys_info_request_with`], to
    /// [`PEER_SYS_INFO`] with no data.
    SysInfo,
    /// `config_cbor_map_service_request` and
    /// [`Node::config_map_request_with`], to [`PEER_CONFIG_MAP`] with this
    /// partition id's [`ConfigMapRequest`].
    ConfigMap(u32),
    /// `power_info_service_request` and [`Node::power_info_request_with`],
    /// to [`POWER_INFO`] with no data.
    PowerInfo,
    /// `metrics_service_request` and [`Node::metrics_request_with`], to
    /// [`PEER_METRICS`] with no data.
    Metrics,
}

fn name_index(name: &[u8]) -> u8 {
    NAMES.iter().position(|n| *n == name).expect("a pool name") as u8
}

fn asked_index(service: &[u8]) -> u8 {
    ASKED
        .iter()
        .position(|n| *n == service)
        .expect("an asked name") as u8
}

fn config_map_request(partition_id: u32) -> Vec<u8> {
    let mut data = [0u8; 32];
    let len = ConfigMapRequest { partition_id }
        .encode(&mut data)
        .expect("at most 19 bytes");
    data[..len].to_vec()
}

/// One of `builtin`'s requests, compared: `ask.data` is the request data
/// `builtin` sends. Returns whether it ran.
fn ask(state: &mut State, ask: &Ask, builtin: Builtin, summary: &mut Summary) -> bool {
    let service = ASKED[usize::from(ask.service) % ASKED.len()];
    match builtin {
        Builtin::None => {}
        Builtin::SysInfo => assert!(service == PEER_SYS_INFO && ask.data.is_empty()),
        Builtin::ConfigMap(id) => {
            assert!(service == PEER_CONFIG_MAP && ask.data == config_map_request(id));
        }
        Builtin::PowerInfo => assert!(service == POWER_INFO && ask.data.is_empty()),
        Builtin::Metrics => assert!(service == PEER_METRICS && ask.data.is_empty()),
    }
    let data = &ask.data[..ask.data.len().min(MAX_DATA_SIZE + 8)];
    let timeout_s = ask.timeout.seconds();
    let node = &state.node;
    let id = node.service_requests().next_id();
    let topic = request_topic(service);
    let mut body = vec![0u8; RequestHeader::LEN];
    RequestHeader {
        id,
        data_size: data.len() as u32,
    }
    .encode(&mut body)
    .expect("eight bytes");
    body.extend_from_slice(data);

    // The Rust node's ceilings.
    if node.service_requests().len() + 1 == SERVICE_REQUESTS {
        return false;
    }
    let rep = reply_topic(service);
    if node
        .subscriptions()
        .callbacks(&rep)
        .is_some_and(|c| c.len() == CALLBACKS && c[0] != Subscriber::Reply)
    {
        return false;
    }
    // A local service the C would call, or read past the request for.
    let serves = node
        .subscriptions()
        .matching_callbacks(&topic)
        .any(|(_, c)| c.contains(&Subscriber::Service));
    if serves
        && matches!(
            node.service_table().lookup(&topic, &body),
            Lookup::Call { .. } | Lookup::OverRead { .. } | Lookup::ShortRequest { .. }
        )
    {
        return false;
    }
    let too_large = data.len() > MAX_DATA_SIZE;
    if !too_large && reaches_reply(node, &topic) && body.len() < ReplyHeader::LEN {
        return false;
    }

    *lock(&AVAILABLE) = body.len().saturating_sub(ReplyHeader::LEN);
    if let Builtin::PowerInfo = builtin {
        let k = (0..SERVICE_REQUESTS)
            .find(|k| state.power_callbacks.iter().all(|(_, used)| used != k))
            .expect("fewer requests wait than there are callbacks");
        state.power_callbacks.push((id, k));
    }
    let c_ok = unsafe {
        match builtin {
            Builtin::None => bm_wire_sys::bm_service_request(
                service.len(),
                service.as_ptr().cast(),
                data.len(),
                data.as_ptr(),
                Some(c_reply_cb),
                timeout_s,
            ),
            Builtin::SysInfo => {
                bm_wire_sys::sys_info_service_request(PEERS[0], Some(c_reply_cb), timeout_s)
            }
            Builtin::ConfigMap(id) => bm_wire_sys::config_cbor_map_service_request(
                PEERS[0],
                id,
                Some(c_reply_cb),
                timeout_s,
            ),
            Builtin::PowerInfo => {
                let (_, k) = state.power_callbacks.last().expect("queued above");
                bm_wire_sys::power_info_service_request(Some(C_POWER_REPLY[*k]), timeout_s)
                    == bm_wire_sys::BmErr_BmOK
            }
            Builtin::Metrics => {
                bm_wire_sys::metrics_service_request(PEERS[0], Some(c_reply_cb), timeout_s)
            }
        }
    };
    stack::pump_until_quiet();
    let c_frames = drain();
    let c_delivered = take_delivered();
    let c_answers = take_c_answers();
    let c_power = take_c_power();
    assert!(take_c_calls().is_empty(), "no handler is called");

    let mut rs_delivered = Vec::new();
    let mut rs_answers = Vec::new();
    let mut rs_power = Vec::new();
    let on_event =
        |event: Event<'_>| record(event, &mut rs_delivered, &mut rs_answers, &mut rs_power);
    let now = tick_count();
    let node = &mut state.node;
    let rs = match builtin {
        Builtin::None => node.service_request_with(now, service, data, timeout_s, on_event),
        Builtin::SysInfo => node.sys_info_request_with(now, PEERS[0], timeout_s, on_event),
        Builtin::ConfigMap(id) => {
            node.config_map_request_with(now, PEERS[0], id, timeout_s, on_event)
        }
        Builtin::PowerInfo => node.power_info_request_with(now, timeout_s, on_event),
        Builtin::Metrics => node.metrics_request_with(now, PEERS[0], timeout_s, on_event),
    }
    .map(|(id, outbound)| (id, capture(outbound)));
    let what = format!("{ask:?}");
    assert_eq!(c_ok, rs.is_ok(), "the result, {what}: {rs:?}");
    assert_eq!(rs_answers, c_answers, "local answers, {what}");
    compare_power(state, &rs_power, &c_power, &what);
    match rs {
        Err(ServiceRequestError::TooLarge) => {
            assert!(too_large, "{what}");
            assert!(c_frames.is_empty() && c_delivered.is_empty(), "{what}");
            assert!(rs_delivered.is_empty(), "{what}");
        }
        Err(e) => panic!("{e:?} outside the domain, {what}"),
        Ok((rs_id, rs_frames)) => {
            summary.asked += 1;
            assert_eq!(rs_id, id, "{what}");
            let mut payload = vec![0u8; codec::HEADER_LEN + topic.len() + body.len()];
            codec::encode(&mut payload, &topic, 0, codec::COMMON_VERSION, &body).expect("a topic");
            assert_eq!(
                c_frames,
                as_bm_linux_sends_it(
                    codec::PORT,
                    &BmIpAddr::GLOBAL_MULTICAST,
                    codec::PORT,
                    &payload
                ),
                "C frames, {what}"
            );
            assert_eq!(rs_frames, rust_sends(&payload), "Rust frames, {what}");
            let expected = deliveries(&state.node, stack::NODE_ID, &topic, &body);
            assert_eq!(c_delivered, expected, "C deliveries, {what}");
            assert_eq!(rs_delivered, expected, "Rust deliveries, {what}");
        }
    }
    count(summary, &c_answers);
    true
}

fn count(summary: &mut Summary, answers: &[Answer]) {
    for (ack, _, service, data) in answers {
        if *ack {
            summary.answered += 1;
            summary.sys_info_decoded += usize::from(
                service == PEER_SYS_INFO
                    && DecodedSysInfoReply::default().decode_into(data).is_ok(),
            );
            summary.config_map_decoded += usize::from(
                service == PEER_CONFIG_MAP
                    && DecodedConfigMapReply::default().decode_into(data).is_ok(),
            );
            summary.metrics_answered += usize::from(service == PEER_METRICS);
        } else {
            summary.timeouts += 1;
        }
    }
}

/// The `sys_config_crc` both nodes send, or `None` where the C reads the
/// system partition's map with undefined behaviour ([`MapError::Unreachable`],
/// divergences #42 and #88).
fn sys_config_crc(node: &ServicesNode) -> Option<u32> {
    let partition = node.config().store.partition(Partition::System);
    if partition.cbor_map(&mut []) == Err(MapError::Unreachable) {
        return None;
    }
    let crc = partition.cbor_map_crc32();
    let c = unsafe { bm_wire_sys::services_cbor_encoded_as_crc32(Partition::System as _) };
    assert_eq!(c, crc, "services_cbor_encoded_as_crc32");
    Some(crc)
}

/// The config_map reply both nodes send for `request`, as
/// [`config_map::handle`] writes it into `out`: `Some(None)` for none, or
/// `None` where the C is undefined: a tagged `partition_id`, or a partition
/// whose map is [`MapError::Unreachable`] (divergences #42, #82, #88).
/// Asserts the C's `services_cbor_as_map` writes the same map first.
fn config_map_reply(node: &ServicesNode, request: &[u8], out: &mut [u8]) -> Option<Option<usize>> {
    let mut req = ConfigMapRequest::default();
    if req.decode_into(request) == Err(CborError::Unreachable) {
        return None;
    }
    let store = &node.config().store;
    if let Some(p) = config_map::partition(req.partition_id) {
        let partition = store.partition(p);
        let mut map = Vec::new();
        let mut rs = partition.cbor_map(&mut map);
        if let Err(MapError::TooSmall(len)) = rs {
            map.resize(len, 0);
            rs = partition.cbor_map(&mut map);
        }
        if rs == Err(MapError::Unreachable) {
            return None;
        }
        let mut c_len = 0usize;
        let c = unsafe { bm_wire_sys::services_cbor_as_map(&mut c_len, p as _) };
        let c_map = (!c.is_null()).then(|| {
            let bytes = unsafe { std::slice::from_raw_parts(c, c_len) }.to_vec();
            unsafe { bm_wire_sys::bm_free(c.cast()) };
            bytes
        });
        assert_eq!(
            c_map.as_deref(),
            rs.ok().map(|len| &map[..len]),
            "services_cbor_as_map({p:?})"
        );
    }
    Some(config_map::handle(
        request,
        stack::NODE_ID,
        |p, out| store.partition(p).cbor_map(out),
        out,
    ))
}

/// Let `ms` pass on both, one request sweep at a time, comparing what each
/// sweep expires. The oracle's other timers run too; their frames are BCMP
/// (heartbeats), and are dropped.
fn wait(state: &mut State, ms: u32, summary: &mut Summary) {
    let target = tick_count().wrapping_add(ms);
    loop {
        let now = tick_count();
        let next = state.node.service_requests().next_sweep_ms();
        let to = if next.wrapping_sub(now) <= target.wrapping_sub(now) {
            next
        } else {
            target
        };
        unsafe { bm_wire_sys::bm_shim_advance_ticks(to.wrapping_sub(now)) };
        stack::pump_until_quiet();
        for (_, frame) in drain() {
            assert!(
                captured_message_type(&frame).is_some(),
                "only BCMP while waiting: {frame:?}"
            );
        }
        assert!(take_delivered().is_empty() && take_c_calls().is_empty());
        let c_answers = take_c_answers();
        let c_power = take_c_power();
        let mut rs_answers = Vec::new();
        let mut rs_power = Vec::new();
        state.node.on_service_expiry(to, |event| {
            record(event, &mut Vec::new(), &mut rs_answers, &mut rs_power);
        });
        assert_eq!(rs_answers, c_answers, "the sweep at {to}");
        let what = format!("the sweep at {to}");
        compare_power(state, &rs_power, &c_power, &what);
        count(summary, &c_answers);
        if to == target {
            return;
        }
    }
}

fn assert_lists(state: &State, after: &str) {
    assert!(
        oracle_subscriptions()
            .iter()
            .map(Vec::as_slice)
            .eq(state.node.subscriptions().iter()),
        "subscriptions after {after}: C {:?}",
        oracle_subscriptions()
    );
    let (pubs, subs, _) = oracle_local_resources();
    let resources = state.node.resources();
    assert!(
        resources
            .iter(ResourceType::Publisher)
            .eq(pubs.iter().map(Vec::as_slice)),
        "PUB_LIST {pubs:?}, after {after}"
    );
    assert!(
        resources
            .iter(ResourceType::Subscriber)
            .eq(subs.iter().map(Vec::as_slice)),
        "SUB_LIST {subs:?}, after {after}"
    );
}

/// The budget left, for tests.
#[must_use]
pub fn budget() -> u32 {
    let (_guard, state) = state();
    state.as_ref().expect("brought up").budget
}
