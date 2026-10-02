//! Differential comparator for the service layer on the node:
//! `bm_service_register`, `bm_service_unregister`, `echo_service_init`,
//! `_service_request_received_cb`, `bm_service_request`, `_service_request_cb`
//! and the request expiry sweep against [`Node::register_service`],
//! [`Node::unregister_service`], [`Node::register_echo_service`],
//! [`Node::on_frame_with`], [`Node::service_request_with`] and
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
//!
//! Time moves only in [`Step::Wait`], one 500 ms sweep at a time, and at the
//! start of each input, which waits until no request is outstanding
//! (`CTX.service_request_list` has no reset either). The oracle's sweep runs
//! on `timer_callback_handler.c`'s task, which
//! [`stack::start_timer_callback_handler`] starts.//!
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
//! | No request answered by the metrics service | `bm_shim_stack_init` registers it, and its reply is card E4's; the Rust node lists [`METRICS`] with [`StandIn`] so the list walks agree |
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

use bm_stack::node::{INFO_REQUESTS_DEFAULT, PING_PAYLOAD_BYTES, RESOURCE_REQUESTS_DEFAULT};
use bm_stack::service::{SERVICE_REQUESTS, SERVICES, ServiceHandler, ServiceRequestError};
use bm_stack::{Event, NoConfig, NoDfu, Node, Services, SoftRtc};
use bm_wire::bcmp::info::CACHED_STRING_BYTES;
use bm_wire::bcmp::resource::{RESOURCE_NAME_BYTES, ResourceType};
use bm_wire::pubsub::{self as codec, CALLBACKS, Subscriber, SubscriptionError};
use bm_wire::service::{
    Lookup, MAX_DATA_SIZE, REPLY_DATA_LEN, REPLY_SUFFIX, REQUEST_SUFFIX, ReplyHeader, RequestHeader,
};
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::l2_egress::port_transmit;
use crate::pubsub::oracle_subscriptions;
use crate::resource::oracle_local_resources;
use crate::stack::{
    self, NUM_PORTS, OracleIdentity, capture, captured_message_type, drain, inject, oracle,
    tick_count,
};
use crate::udp::as_bm_linux_sends_it;

/// The metrics service `bm_shim_stack_init` registers, `<node id>/metrics`.
pub const METRICS: &[u8] = b"c0ffee0012345678/metrics";

/// `echo_service_init`'s name, `<node id>/echo`.
pub const ECHO: &[u8] = b"c0ffee0012345678/echo";

/// Service names. [`ECHO`] is registered with `echo_service_init` and
/// [`Node::register_echo_service`]; the rest with [`c_handler`] and
/// [`StandIn`].
///
/// `<id>/e` prefixes echo's name and `<id>/echo/x` is prefixed by it; `s`,
/// `sv` and `svc` prefix each other; `s*` and `s?c` subscribe patterns that
/// match other services' request topics; `x` shares a prefix with none, so
/// it is the one name [`LEAK_BUDGET`] may leave listed.
///
/// Not the empty name: it prefixes [`METRICS`], which is listed first, so
/// unregistering it removes the metrics service instead (divergence #89), and
/// nothing can then remove it. Listed first, it ends the walk for every
/// request. `bm_wire::service`'s unit tests cover it.
pub const NAMES: [&[u8]; 9] = [
    ECHO,
    b"c0ffee0012345678/e",
    b"c0ffee0012345678/echo/x",
    b"svc",
    b"sv",
    b"s",
    b"s*",
    b"s?c",
    b"x",
];

/// Topics the application subscribes: a service's request topic, a prefix,
/// everything, echo's request topic, and a peer's echo reply topic.
pub const APP_TOPICS: [&[u8]; 5] = [
    b"svc/req",
    b"s",
    b"*",
    b"c0ffee0012345678/echo/req",
    b"0b54ccce5c7978bf/echo/rep",
];

/// Services this node asks: each peer's echo, a pattern whose reply topic
/// matches the first peer's other reply topics (divergence #74), and a
/// [`NAMES`] entry the node may itself list.
pub const ASKED: [&[u8]; 4] = [
    b"0b54ccce5c7978bf/echo",
    b"777777775c7978bf/echo",
    b"0b54ccce5c7978bf/*",
    b"svc",
];

/// Steps per process that may leave a listed service no request can unlist.
pub const LEAK_BUDGET: u32 = 4;

/// Topics the Rust node holds: every request, application and asked reply
/// topic.
pub const SUBSCRIPTIONS: usize = 24;

/// Resources the Rust node holds: every request, reply and application topic.
pub const RESOURCES: usize = 40;

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
    NoConfig,
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

/// The Rust node's [`Services`]: `answer`, recording each call.
#[derive(Debug, Default)]
pub struct StandIn {
    calls: Vec<Call>,
}

impl Services for StandIn {
    fn handle(&mut self, service: &[u8], request: &[u8], reply: &mut [u8]) -> Option<usize> {
        assert_eq!(reply.len(), REPLY_DATA_LEN, "the handler's buffer");
        assert!(reply.iter().all(|b| *b == 0), "the buffer is zeroed");
        self.calls.push((service.to_vec(), request.to_vec()));
        answer(request, reply)
    }
}

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
            .chain([&METRICS])
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
}

/// What the oracle and the Rust node hold between inputs.
pub struct State {
    node: ServicesNode,
    /// What is left of [`LEAK_BUDGET`].
    budget: u32,
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

        let mut node = Node::with_services(
            OracleIdentity,
            SoftRtc::new(),
            NoConfig,
            NoDfu,
            StandIn::default(),
            NUM_PORTS,
        );
        node.register_service(METRICS)
            .expect("the metrics service's stand-in");
        let mut state_ = State {
            node,
            budget: LEAK_BUDGET,
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
            Step::Reply(reply) => {
                let (topic, body) = reply.publication(&state.node);
                let ingress = reply.ingress % NUM_PORTS + 1;
                let source = PEERS[usize::from(reply.peer)];
                receive(state, ingress, source, &topic, &body, &mut summary)
            }
            Step::Ask(ask) => self::ask(state, ask, &mut summary),
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
    state.node.services_mut().calls.clear();

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
    } else {
        let c =
            unsafe { bm_wire_sys::bm_service_register(name.len(), c_name(name), Some(c_handler)) };
        let rs = state.node.register_service(name);
        assert_eq!(c, rs.is_ok(), "register {name:?}: {rs:?}");
    }
    let handler = if name == ECHO {
        ServiceHandler::Echo
    } else {
        ServiceHandler::Application
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
    let Some((calls, replied, answers)) =
        receive_publication(state, ingress, source, topic, body, &what)
    else {
        return false;
    };
    summary.most_calls = summary.most_calls.max(calls);
    summary.replies += usize::from(replied);
    count(summary, &answers);
    true
}

/// [`receive`]'s comparison. Returns how many service callbacks the
/// publication reached, whether it was answered and the answers it gave
/// requests this node made, or `None` if it is outside the domain.
fn receive_publication(
    state: &mut State,
    ingress: u8,
    source: u64,
    topic: &[u8],
    body: &[u8],
    what: &str,
) -> Option<(usize, bool, Vec<Answer>)> {
    let (topic, body) = (topic.to_vec(), body.to_vec());
    let node = &state.node;
    if reaches_reply(node, &topic) && body.len() < ReplyHeader::LEN {
        return None;
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
    if calls > 0 {
        match node.service_table().lookup(&topic, &body) {
            Lookup::OverRead { .. } | Lookup::ShortRequest { .. } => return None,
            Lookup::Call { name, .. } if name == METRICS => return None,
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

    let mut rs_frame = frame.clone();
    let mut rs_delivered = Vec::new();
    let mut rs_answers = Vec::new();
    let owed = state
        .node
        .on_frame_with(tick_count(), ingress, &mut rs_frame, |event| {
            record(event, &mut rs_delivered, &mut rs_answers);
        });
    assert!(owed.forward.is_none());
    let rs_relay = owed.relay.map(capture).unwrap_or_default();
    let rs_reply = owed.reply.map(capture);
    let rs_calls = std::mem::take(&mut state.node.services_mut().calls);

    let what = format!("{what}: {calls} service callbacks");
    assert_eq!(rs_answers, c_answers, "answers, {what}");
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
    Some((calls, reply.is_some(), c_answers))
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

/// File a Rust event as a delivery or an answer.
fn record(event: Event<'_>, delivered: &mut Vec<Delivery>, answers: &mut Vec<Answer>) {
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
        _ => {}
    }
}

/// `bm_service_request` and [`Node::service_request_with`]. Returns whether
/// it ran.
fn ask(state: &mut State, ask: &Ask, summary: &mut Summary) -> bool {
    let service = ASKED[usize::from(ask.service) % ASKED.len()];
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
    let c_ok = unsafe {
        bm_wire_sys::bm_service_request(
            service.len(),
            service.as_ptr().cast(),
            data.len(),
            data.as_ptr(),
            Some(c_reply_cb),
            timeout_s,
        )
    };
    stack::pump_until_quiet();
    let c_frames = drain();
    let c_delivered = take_delivered();
    let c_answers = take_c_answers();
    assert!(take_c_calls().is_empty(), "no handler is called");

    let mut rs_delivered = Vec::new();
    let mut rs_answers = Vec::new();
    let rs = state
        .node
        .service_request_with(tick_count(), service, data, timeout_s, |event| {
            record(event, &mut rs_delivered, &mut rs_answers);
        })
        .map(|(id, outbound)| (id, capture(outbound)));
    let what = format!("{ask:?}");
    assert_eq!(c_ok, rs.is_ok(), "the result, {what}: {rs:?}");
    assert_eq!(rs_answers, c_answers, "local answers, {what}");
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
    for (ack, ..) in answers {
        if *ack {
            summary.answered += 1;
        } else {
            summary.timeouts += 1;
        }
    }
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
        let mut rs_answers = Vec::new();
        state.node.on_service_expiry(to, |event| {
            record(event, &mut Vec::new(), &mut rs_answers);
        });
        assert_eq!(rs_answers, c_answers, "the sweep at {to}");
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
