//! Differential comparator for pub/sub on the node: `bm_sub_wl`,
//! `bm_unsub_wl`, `bm_pub_wl` and `bm_handle_msg` against
//! [`Node::subscribe`], [`Node::unsubscribe`], [`Node::publish_with`] and
//! [`Event::Publication`].
//!
//! A stack target, for the reason [`crate::stack`] gives. Driven from
//! `tests/pubsub.rs`.
//!
//! The oracle and one Rust node with its identity are mirrored for the life of
//! the process: the C's subscription list and resource lists have no reset,
//! so [`State`] holds the Rust node beside them and every step applies to
//! both. After every step:
//!
//! | What | C | Rust |
//! |---|---|---|
//! | The call's result | the `BmErr` | the `Result` |
//! | Subscriptions, in order | `bm_get_subs` | [`Node::subscriptions`]; less the metrics service's, [`State`]'s own list |
//! | `PUB_LIST` and `SUB_LIST` | [`crate::resource::oracle_local_resources`] | [`Node::resources`] |
//! | Deliveries | the callback every oracle subscription shares | [`Event::Publication`], whose `subscription` is each matching entry of [`State`]'s list in order |
//! | Frames a publish sends | [`as_bm_linux_sends_it`] of [`bm_wire::pubsub::encode`] | [`udp::build`] of it from [`udp::source_address`] |
//! | The oracle's publish, received | — | a fresh Rust node subscribed alike reports what the publishing node delivered locally |
//! | A peer's publication, received | relayed, and delivered by `bm_handle_msg` | relayed alike, and delivered alike |
//!
//! # The domain
//!
//! Divergence #38: `bcmp_resource_discovery_find_resource` compares the
//! needle's length against every entry before a match, so a shorter entry is
//! read past. Every subscription pattern is [`PATTERN_LEN`] bytes, and
//! `SUB_LIST`'s one other entry, `<id>/metrics/req`, is longer; every
//! published topic is [`TOPIC_LEN`] bytes, and `PUB_LIST` starts empty. No
//! lookup then reads past an entry, and the prefix match is equality.
//!
//! Nothing may reach the metrics service's subscription, which would publish a
//! reply: no pool topic matches it, and a received topic that does is skipped.
//!
//! Received publications are well-formed; `crate::node_udp` covers the
//! malformed ones (divergence #75).
//!
//! # One subscriber per topic
//!
//! Every oracle subscription here uses one callback, the application's.
//! [`duplicate_callbacks`] uses two, to show what the C does with more
//! (divergence #79); `crate::services` compares a node's second kind, the
//! service layer.

use std::sync::{Mutex, MutexGuard};

use arbitrary::Arbitrary;

use bm_stack::node::{INFO_REQUESTS_DEFAULT, PING_PAYLOAD_BYTES, RESOURCE_REQUESTS_DEFAULT};
use bm_stack::{Event, Node, PublishError, SoftRtc, SubscribeError};
use bm_wire::bcmp::info::CACHED_STRING_BYTES;
use bm_wire::bcmp::resource::{RESOURCE_NAME_BYTES, ResourceType};
use bm_wire::pubsub::{self as codec, SubscriptionError};
use bm_wire::udp;
use bm_wire::util::{BmIpAddr, bm_wildcard_match};

use crate::l2_egress::port_transmit;
use crate::node_udp::{Peer, metrics_request_topic};
use crate::resource::oracle_local_resources;
use crate::stack::{self, Captured, NUM_PORTS, OracleIdentity, capture, drain, inject, oracle};
use crate::udp::as_bm_linux_sends_it;

/// The length of every subscription pattern.
pub const PATTERN_LEN: usize = 10;

/// Subscription patterns: exact, `*`, `?`, and prefixes (divergence #74).
pub const PATTERNS: [&[u8; PATTERN_LEN]; 8] = [
    b"sensor/tmp",
    b"sensor/*/0",
    b"sensor/???",
    b"sensor/hum",
    b"**********",
    b"spotter/pr",
    b"*tmp*/0001",
    b"other/*/01",
];

/// The length of every topic [`Step::Publish`] publishes.
pub const TOPIC_LEN: usize = 14;

/// Topics [`Step::Publish`] publishes.
pub const TOPICS: [&[u8; TOPIC_LEN]; 7] = [
    b"sensor/tmp/001",
    b"sensor/hum/001",
    b"spotter/printf",
    b"sensor/tmp/xyz",
    b"sensorXtmp/001",
    b"other/topic/01",
    b"sensor/a/b/c/0",
];

/// The pattern [`duplicate_callbacks`] subscribes, [`PATTERN_LEN`] bytes, in
/// no pool.
pub const DUPLICATE_PATTERN: &[u8; PATTERN_LEN] = b"dup/callba";

/// Subscriptions the Rust node holds: the metrics service's and every
/// pattern.
pub const SUBSCRIPTIONS: usize = 1 + PATTERNS.len();

/// Resources the Rust node holds: the metrics service's, every pattern and
/// topic, and [`DUPLICATE_PATTERN`].
pub const RESOURCES: usize = 1 + PATTERNS.len() + TOPICS.len() + 1;

/// The mirrored Rust node.
pub type PubSubNode = Node<
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
>;

/// A topic argument.
#[derive(Debug, Clone, Copy, Arbitrary)]
pub enum Topic {
    /// An index into the step's pool, reduced modulo its length.
    Pool(u8),
    /// The empty topic: `BmEINVAL`.
    Empty,
    /// [`codec::TOPIC_MAX_LEN`] bytes: `BmEMSGSIZE`.
    TooLong,
}

impl Topic {
    fn bytes<const N: usize>(self, pool: &[&[u8; N]]) -> Vec<u8> {
        match self {
            Self::Pool(i) => pool[usize::from(i) % pool.len()].to_vec(),
            Self::Empty => Vec::new(),
            Self::TooLong => vec![b's'; codec::TOPIC_MAX_LEN],
        }
    }
}

/// A publication from a peer.
#[derive(Debug, Clone, Arbitrary)]
pub struct Arrival {
    /// The port it arrives on, reduced to 1..=[`NUM_PORTS`].
    pub ingress: u8,
    /// Whether it is sent from [`codec::PORT`]; otherwise from the next port
    /// up, and nobody receives it (divergence #73).
    pub from_pubsub_port: bool,
    /// The topic, cut to 255 bytes; empty and 255 are delivered.
    pub topic: Vec<u8>,
    /// `ext_header.type`.
    pub kind: u8,
    /// `ext_header.version`.
    pub version: u8,
    /// The data, cut to what fits [`codec::MAX_MESSAGE_LEN`].
    pub data: Vec<u8>,
}

/// One step, applied to the oracle and the Rust node.
#[derive(Debug, Clone, Arbitrary)]
pub enum Step {
    /// `bm_sub_wl` and [`Node::subscribe`], from [`PATTERNS`].
    Subscribe(Topic),
    /// `bm_unsub_wl` and [`Node::unsubscribe`], from [`PATTERNS`].
    Unsubscribe(Topic),
    /// `bm_pub_wl` and [`Node::publish_with`], from [`TOPICS`].
    Publish {
        /// The topic.
        topic: Topic,
        /// `ext_header.type`.
        kind: u8,
        /// `ext_header.version`.
        version: u8,
        /// The data, cut to 8 bytes past what fits
        /// [`codec::MAX_MESSAGE_LEN`], so both sides of it are reachable.
        data: Vec<u8>,
    },
    /// A peer's publication.
    Receive(Arrival),
}

/// Steps run in order.
#[derive(Debug, Clone, Arbitrary)]
pub struct PubSubInput {
    /// The steps.
    pub steps: Vec<Step>,
}

/// A delivery: `(node id, topic, data, type, version)`.
pub type Delivery = (u64, Vec<u8>, Vec<u8>, u8, u8);

/// What the oracle and the Rust node hold between steps.
pub struct State {
    node: PubSubNode,
    /// The subscriptions, in list order, as this module made them.
    subscriptions: Vec<Vec<u8>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static DELIVERED: Mutex<Vec<Delivery>> = Mutex::new(Vec::new());

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

fn lock<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn take_delivered() -> Vec<Delivery> {
    std::mem::take(&mut *lock(&DELIVERED))
}

/// Take the oracle and the mirrored state, bringing both up on first use.
///
/// # Panics
///
/// If the oracle's lists are not what `bm_shim_stack_init` leaves: no
/// publishers, and the metrics service's subscription.
fn state() -> (MutexGuard<'static, ()>, MutexGuard<'static, Option<State>>) {
    let guard = oracle();
    let mut state = lock(&STATE);
    if state.is_none() {
        let (pubs, subs, _) = oracle_local_resources();
        assert!(pubs.is_empty(), "PUB_LIST: {pubs:?}");
        assert_eq!(subs, vec![metrics_request_topic()], "SUB_LIST");
        assert_eq!(oracle_subscriptions(), subs, "the subscription list");
        let node: PubSubNode = Node::new(OracleIdentity, SoftRtc::new(), NUM_PORTS);
        assert!(
            node.subscriptions().iter().eq([&subs[0][..]]),
            "the metrics service's, subscribed at construction"
        );
        *state = Some(State {
            node,
            subscriptions: Vec::new(),
        });
    }
    (guard, state)
}

/// The most bytes [`oracle_subscriptions`] reads: `bm_get_subs`'s joined
/// list, with its NUL.
pub const SUBS_BYTES: usize = 4096;

/// `bm_get_subs`, split. Its buffer is 256 bytes and unchecked (divergence
/// #78), so it is called under [`bm_wire_sys::bm_shim_alloc_floor`] of
/// [`SUBS_BYTES`].
///
/// The caller holds [`oracle`]'s lock.
///
/// # Panics
///
/// If the list joined does not fit [`SUBS_BYTES`].
#[must_use]
pub fn oracle_subscriptions() -> Vec<Vec<u8>> {
    unsafe {
        bm_wire_sys::bm_shim_alloc_floor(SUBS_BYTES);
        let subs = bm_wire_sys::bm_get_subs();
        bm_wire_sys::bm_shim_alloc_floor(0);
        assert!(!subs.is_null(), "bm_get_subs");
        let joined = std::ffi::CStr::from_ptr(subs).to_bytes().to_vec();
        bm_wire_sys::bm_free(subs.cast());
        assert!(joined.len() < SUBS_BYTES, "bm_get_subs past its floor");
        if joined.is_empty() {
            return Vec::new();
        }
        joined
            .split(|b| *b == b'|')
            .map(|t| t.trim_ascii().to_vec())
            .collect()
    }
}

/// Run every step.
///
/// # Panics
///
/// On any divergence.
pub fn check(input: &PubSubInput) {
    let (_guard, mut state) = state();
    let state = state.as_mut().expect("brought up");
    assert!(drain().is_empty(), "the ring was not drained");
    let _ = take_delivered();
    for step in &input.steps {
        match step {
            Step::Subscribe(topic) => subscribe(state, &topic.bytes(&PATTERNS)),
            Step::Unsubscribe(topic) => unsubscribe(state, &topic.bytes(&PATTERNS)),
            Step::Publish {
                topic,
                kind,
                version,
                data,
            } => {
                let cap = codec::MAX_MESSAGE_LEN - codec::HEADER_LEN - TOPIC_LEN + 8;
                let data = &data[..data.len().min(cap)];
                publish(state, &topic.bytes(&TOPICS), *kind, *version, data);
            }
            Step::Receive(arrival) => receive(state, arrival),
        }
        assert_lists(state, step);
    }
}

fn subscribe(state: &mut State, topic: &[u8]) {
    let err = unsafe {
        bm_wire_sys::bm_sub_wl(
            topic.as_ptr().cast(),
            topic.len() as u16,
            Some(on_publication),
        )
    };
    let rs = state.node.subscribe(topic);
    let expected = match err {
        bm_wire_sys::BmErr_BmOK => Ok(()),
        bm_wire_sys::BmErr_BmEINVAL => Err(SubscribeError::Refused(SubscriptionError::EmptyTopic)),
        bm_wire_sys::BmErr_BmEMSGSIZE => {
            Err(SubscribeError::Refused(SubscriptionError::TopicTooLong))
        }
        _ => panic!("bm_sub_wl returned {err}"),
    };
    assert_eq!(rs, expected, "subscribe {topic:?}");
    if rs.is_ok() && !state.subscriptions.iter().any(|t| t == topic) {
        state.subscriptions.push(topic.to_vec());
    }
}

fn unsubscribe(state: &mut State, topic: &[u8]) {
    let err = unsafe {
        bm_wire_sys::bm_unsub_wl(
            topic.as_ptr().cast(),
            topic.len() as u16,
            Some(on_publication),
        )
    };
    let rs = state.node.unsubscribe(topic);
    let subscribed = state.subscriptions.iter().position(|t| t == topic);
    let expected = match err {
        bm_wire_sys::BmErr_BmOK => Ok(()),
        // Also an unknown topic: `bm_unsub_wl` never reassigns `err`.
        bm_wire_sys::BmErr_BmEINVAL if topic.is_empty() => Err(SubscriptionError::EmptyTopic),
        bm_wire_sys::BmErr_BmEINVAL => {
            assert!(subscribed.is_none(), "bm_unsub_wl refused {topic:?}");
            Err(SubscriptionError::NotSubscribed)
        }
        bm_wire_sys::BmErr_BmEMSGSIZE => Err(SubscriptionError::TopicTooLong),
        _ => panic!("bm_unsub_wl returned {err}"),
    };
    assert_eq!(rs, expected, "unsubscribe {topic:?}");
    if let Some(index) = subscribed {
        state.subscriptions.remove(index);
    }
}

/// The deliveries [`State`]'s list says a publication on `topic` makes, with
/// the subscription each is for.
fn expected_deliveries(
    state: &State,
    source: u64,
    topic: &[u8],
    data: &[u8],
    kind: u8,
    version: u8,
) -> Vec<(Vec<u8>, Delivery)> {
    state
        .subscriptions
        .iter()
        .filter(|pattern| bm_wildcard_match(topic, pattern))
        .map(|pattern| {
            (
                pattern.clone(),
                (source, topic.to_vec(), data.to_vec(), kind, version),
            )
        })
        .collect()
}

fn as_delivery(event: &Event<'_>) -> Option<(Vec<u8>, Delivery)> {
    match *event {
        Event::Publication {
            source,
            subscription,
            topic,
            kind,
            version,
            data,
        } => Some((
            subscription.to_vec(),
            (source, topic.to_vec(), data.to_vec(), kind, version),
        )),
        _ => None,
    }
}

fn without_subscription(deliveries: &[(Vec<u8>, Delivery)]) -> Vec<Delivery> {
    deliveries.iter().map(|(_, d)| d.clone()).collect()
}

fn publish(state: &mut State, topic: &[u8], kind: u8, version: u8, data: &[u8]) {
    let err = unsafe {
        bm_wire_sys::bm_pub_wl(
            topic.as_ptr().cast(),
            topic.len() as u16,
            data.as_ptr().cast(),
            data.len() as u16,
            kind,
            version,
        )
    };
    stack::pump_until_quiet();
    let c = drain();
    let c_delivered = take_delivered();

    let mut rs_delivered = Vec::new();
    let rs = state
        .node
        .publish_with(topic, kind, version, data, |event| {
            rs_delivered.extend(as_delivery(&event));
        })
        .map(capture);

    let what = format!("publish {topic:?} ({} bytes)", data.len());
    let valid = !topic.is_empty() && topic.len() < codec::TOPIC_MAX_LEN;
    let expected = if valid {
        expected_deliveries(state, stack::NODE_ID, topic, data, kind, version)
    } else {
        Vec::new()
    };
    assert_eq!(rs_delivered, expected, "Rust local delivery, {what}");
    assert_eq!(
        c_delivered,
        without_subscription(&expected),
        "C local delivery, {what}"
    );

    let len = codec::HEADER_LEN + topic.len() + data.len();
    let rs_frames = match (err, rs) {
        (bm_wire_sys::BmErr_BmOK, Ok(frames)) => frames,
        (bm_wire_sys::BmErr_BmEINVAL, Err(PublishError::EmptyTopic)) if topic.is_empty() => {
            return assert!(c.is_empty(), "{what}");
        }
        (bm_wire_sys::BmErr_BmEMSGSIZE, Err(PublishError::TopicTooLong)) => {
            return assert!(c.is_empty(), "{what}");
        }
        (bm_wire_sys::BmErr_BmEINVAL, Err(PublishError::MessageTooLong))
            if len > codec::MAX_MESSAGE_LEN =>
        {
            return assert!(c.is_empty(), "{what}");
        }
        (err, rs) => panic!("bm_pub_wl returned {err}, Node::publish {rs:?}: {what}"),
    };

    let mut payload = vec![0u8; len];
    codec::encode(&mut payload, topic, kind, version, data).expect("a valid topic");
    let dst = BmIpAddr::GLOBAL_MULTICAST;
    assert_eq!(
        c,
        as_bm_linux_sends_it(codec::PORT, &dst, codec::PORT, &payload),
        "C frames, {what}"
    );
    let mut built = vec![0u8; udp::PAYLOAD_OFFSET + len];
    udp::build(
        &mut built,
        &udp::source_address(stack::NODE_ID, &dst),
        &dst,
        codec::PORT,
        codec::PORT,
        &payload,
    )
    .expect("sized for the payload");
    assert_eq!(rs_frames, port_transmit(&built), "Rust frames, {what}");

    // The oracle's publication, received by a Rust node subscribed alike.
    for (port, mut frame) in c {
        let mut receiver =
            Node::<Peer, SoftRtc, 4, 4>::new(Peer(!stack::NODE_ID), SoftRtc::new(), NUM_PORTS);
        for pattern in &state.subscriptions {
            receiver.subscribe(pattern).expect("room for every pattern");
        }
        let mut received = Vec::new();
        let _ = receiver.on_frame_with(0, port.max(1), &mut frame, |event| {
            received.extend(as_delivery(&event));
        });
        assert_eq!(received, expected, "the oracle's frame received, {what}");
    }
}

/// `arrival`'s UDP payload: a well-formed publication.
fn arrival_payload(arrival: &Arrival) -> Vec<u8> {
    let topic = &arrival.topic[..arrival.topic.len().min(usize::from(u8::MAX))];
    let room = codec::MAX_MESSAGE_LEN - codec::HEADER_LEN - topic.len();
    let data = &arrival.data[..arrival.data.len().min(room)];
    let mut payload = vec![0, 0, topic.len() as u8, arrival.kind, arrival.version];
    payload.extend_from_slice(topic);
    payload.extend_from_slice(data);
    payload
}

fn receive(state: &mut State, arrival: &Arrival) {
    let payload = arrival_payload(arrival);
    let publication = codec::decode(&payload).expect("well-formed");
    if bm_wildcard_match(publication.topic, &metrics_request_topic()) {
        return;
    }
    let src_port = if arrival.from_pubsub_port {
        codec::PORT
    } else {
        codec::PORT + 1
    };
    let ingress = arrival.ingress % NUM_PORTS + 1;
    let frame = crate::frames::udp(
        PEER,
        &BmIpAddr::GLOBAL_MULTICAST,
        src_port,
        codec::PORT,
        &payload,
    );
    if arrival.from_pubsub_port && !publication.topic.is_empty() && publication.topic.len() < 255 {
        let mut peer: Node<Peer, SoftRtc, 4> = Node::new(Peer(PEER), SoftRtc::new(), NUM_PORTS);
        let sent = peer
            .publish(
                publication.topic,
                publication.kind,
                publication.version,
                publication.data,
            )
            .expect("fits");
        assert_eq!(sent.frame(), frame, "a peer's Node::publish");
    }

    inject(ingress, &frame);
    let c_relayed: Vec<Captured> = drain();
    let c_delivered = take_delivered();

    let mut rs_frame = frame.clone();
    let mut rs_delivered = Vec::new();
    let owed = state
        .node
        .on_frame_with(0, ingress, &mut rs_frame, |event| {
            rs_delivered.extend(as_delivery(&event));
        });
    assert!(owed.reply.is_none() && owed.forward.is_none());
    let rs_relayed = owed.relay.map(capture).unwrap_or_default();

    let what = format!("{arrival:?}");
    assert_eq!(c_relayed, rs_relayed, "relay, {what}");
    let expected = if arrival.from_pubsub_port {
        expected_deliveries(
            state,
            PEER,
            publication.topic,
            publication.data,
            publication.kind,
            publication.version,
        )
    } else {
        Vec::new()
    };
    assert_eq!(rs_delivered, expected, "Rust delivery, {what}");
    assert_eq!(
        c_delivered,
        without_subscription(&expected),
        "C delivery, {what}"
    );
}

/// The node id [`Step::Receive`] publications come from.
pub const PEER: u64 = 0x0b54_ccce_5c79_78bf;

fn assert_lists(state: &State, step: &Step) {
    let c_subs = oracle_subscriptions();
    assert_eq!(
        c_subs.first(),
        Some(&metrics_request_topic()),
        "the metrics service's subscription, after {step:?}"
    );
    assert_eq!(
        c_subs[1..],
        state.subscriptions,
        "C subscriptions, after {step:?}"
    );
    assert!(
        state
            .node
            .subscriptions()
            .iter()
            .eq(c_subs.iter().map(Vec::as_slice)),
        "Rust subscriptions, after {step:?}"
    );

    let (pubs, subs, _) = oracle_local_resources();
    let resources = state.node.resources();
    assert!(
        resources
            .iter(ResourceType::Publisher)
            .eq(pubs.iter().map(Vec::as_slice)),
        "PUB_LIST {pubs:?}, after {step:?}"
    );
    assert!(
        resources
            .iter(ResourceType::Subscriber)
            .eq(subs.iter().map(Vec::as_slice)),
        "SUB_LIST {subs:?}, after {step:?}"
    );
    assert!(
        pubs.iter().all(|name| name.len() == TOPIC_LEN)
            && subs[1..].iter().all(|name| name.len() == PATTERN_LEN),
        "a resource breaks the domain: {pubs:?} {subs:?}"
    );
}

static COUNT_A: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static COUNT_B: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

macro_rules! counter {
    ($name:ident, $count:ident) => {
        unsafe extern "C" fn $name(
            _: u64,
            _: *const core::ffi::c_char,
            _: u16,
            _: *const u8,
            _: u16,
            _: u8,
            _: u8,
        ) {
            $count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    };
}
counter!(callback_a, COUNT_A);
counter!(callback_b, COUNT_B);

/// A step of [`duplicate_callbacks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    /// `bm_sub_wl(DUPLICATE_PATTERN, a or b)`.
    Sub(char),
    /// `bm_unsub_wl(DUPLICATE_PATTERN, a or b)`.
    Unsub(char),
}

/// Run `calls` against [`DUPLICATE_PATTERN`] with two callbacks, `a` and `b`,
/// then inject one publication on it, and return how often each was called.
///
/// Oracle only: a Rust node's two kinds of subscriber are compared in
/// `crate::services`. Its resource list
/// takes the `SUB` entry `bm_sub_wl` adds, and every callback is unlinked
/// before returning, so the two sides' lists still agree.
///
/// # Panics
///
/// If the oracle refuses a call, or the lists stop agreeing.
pub fn duplicate_callbacks(calls: &[Call]) -> (u32, u32) {
    use std::sync::atomic::Ordering::Relaxed;

    let (_guard, mut state) = state();
    let state = state.as_mut().expect("brought up");
    let pattern = DUPLICATE_PATTERN;
    let len = PATTERN_LEN as u16;
    for call in calls {
        let (sub, callback) = match *call {
            Call::Sub(c) => (true, c),
            Call::Unsub(c) => (false, c),
        };
        let callback = if callback == 'a' {
            callback_a
        } else {
            callback_b
        };
        let err = unsafe {
            if sub {
                bm_wire_sys::bm_sub_wl(pattern.as_ptr().cast(), len, Some(callback))
            } else {
                bm_wire_sys::bm_unsub_wl(pattern.as_ptr().cast(), len, Some(callback))
            }
        };
        assert_eq!(err, bm_wire_sys::BmErr_BmOK, "{call:?}");
        if sub {
            let _ = state.node.add_resource(pattern, ResourceType::Subscriber);
        }
    }

    COUNT_A.store(0, Relaxed);
    COUNT_B.store(0, Relaxed);
    let payload = arrival_payload(&Arrival {
        ingress: 0,
        from_pubsub_port: true,
        topic: pattern.to_vec(),
        kind: 1,
        version: 2,
        data: b"twice?".to_vec(),
    });
    inject(
        1,
        &crate::frames::udp(
            PEER,
            &BmIpAddr::GLOBAL_MULTICAST,
            codec::PORT,
            codec::PORT,
            &payload,
        ),
    );
    drain();
    let _ = take_delivered();
    let counts = (COUNT_A.load(Relaxed), COUNT_B.load(Relaxed));

    for callback in [callback_a, callback_b] {
        while unsafe { bm_wire_sys::bm_unsub_wl(pattern.as_ptr().cast(), len, Some(callback)) }
            == bm_wire_sys::BmErr_BmOK
        {}
    }
    assert_lists(state, &Step::Subscribe(Topic::Empty));
    counts
}
