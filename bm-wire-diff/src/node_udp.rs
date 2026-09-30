//! Differential comparator for UDP through `bm_stack::Node` against the
//! oracle's whole stack: `bm_l2_process_rx_evt`, `bm_l2_submit`'s UDP branch,
//! `bm_middleware_rx` and `middleware_net_task`.
//!
//! A stack target, for the reason [`crate::stack`] gives. Driven from
//! `tests/node_udp.rs`.
//!
//! | Step | C | Rust | Asserted |
//! |---|---|---|---|
//! | [`Step::Send`] | `bm_udp_tx_perform`, as [`crate::udp::check_send`] | [`Node::send_udp`] | the node's frames are [`udp::build`]'s from [`udp::source_address`] through L2's egress; `check_send` compares that build from `fe80::<id>` with the oracle's; each oracle frame reaches a Rust node's bound port as [`Event::Udp`] |
//! | [`Step::Receive`] | a Rust peer's frame injected on one port | the same frame to [`Node::on_frame_with`] on the same port | the same relay; the same payload reaching each bound port; `bm_handle_msg` reports what [`pubsub::decode`] reads |
//! | [`Step::Publish`] | `bm_pub_wl` | [`pubsub::encode`] | the oracle's frames are [`as_bm_linux_sends_it`] of the encoding; the local delivery is its decoding; the same refusals |
//!
//! Where the oracle delivers a received datagram:
//!
//! | Destination port | Source port | Reaches |
//! |---|---|---|
//! | [`MIDDLEWARE_PORT`] | [`MIDDLEWARE_PORT`] | `bm_middleware_rx`, then `bm_handle_msg`, then this module's `*` subscription |
//! | [`MIDDLEWARE_PORT`] | anything else | `bm_middleware_rx`, then nothing: `middleware_net_task` looks the application up by source port (divergence #73) |
//! | another of [`BOUND_PORTS`] | any | [`crate::udp`]'s callback for it |
//!
//! `bm_handle_msg` computes a publication's data length without checking it
//! against the payload (divergence #75). [`Arrival`] keeps the payload of a
//! datagram that reaches it to what the C reads in bounds; see
//! [`pubsub_domain`].

use std::sync::{Mutex, Once};

use arbitrary::Arbitrary;

use bm_stack::{Event, Identity, NoRtc, Node};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::util::BmIpAddr;
use bm_wire::{BmWireError, pubsub, udp};

use crate::l2_egress::port_transmit;
use crate::stack::{self, Captured, NUM_PORTS, capture, drain, inject, oracle, pump_until_quiet};
use crate::udp::{
    BOUND_PORTS, Dst, MAX_PAYLOAD, MIDDLEWARE_PORT, Send, as_bm_linux_sends_it, bind,
    take_delivered,
};

/// `sizeof(BmPubSubData)`: type, flags, `topic_len`, ext type, ext version.
pub const PUBSUB_HEADER_LEN: usize = pubsub::HEADER_LEN;

/// The first topic byte of a publication whose topic runs past its payload.
/// No oracle subscription starts with it; every comparator here asserts so.
pub const OFF_PATTERN: u8 = b'#';

/// The ports a Rust node binds: the first [`bm_stack::node::UDP_PORTS`] of
/// [`BOUND_PORTS`]. The last, `0xFFFF`, is bound in the oracle only.
pub fn rust_bound_ports() -> &'static [u16] {
    &BOUND_PORTS[..bm_stack::node::UDP_PORTS]
}

/// An identity for a Rust node other than the oracle's.
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub u64);

impl Identity for Peer {
    fn node_id(&self) -> u64 {
        self.0
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo::default()
    }

    fn version_string(&self) -> &[u8] {
        b""
    }

    fn device_name(&self) -> &[u8] {
        b""
    }
}

/// A Rust node with `id`, both links up, and [`rust_bound_ports`] bound.
///
/// # Panics
///
/// Never: the ports are distinct and fit.
#[must_use]
pub fn receiver(id: u64) -> Node<Peer, NoRtc, 4> {
    let mut node = Node::new(Peer(id), NoRtc, NUM_PORTS);
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    for port in rust_bound_ports() {
        node.bind_udp(*port)
            .expect("distinct ports, within UDP_PORTS");
    }
    node
}

/// A datagram from a Rust peer, received by the oracle and a Rust node.
#[derive(Debug, Clone, Arbitrary)]
pub struct Arrival {
    /// The port it arrives on, reduced to 1..=[`NUM_PORTS`].
    pub ingress: u8,
    /// The sending node's id.
    pub src: u64,
    /// Where it is sent.
    pub dst: Dst,
    /// The sender's port: an index into [`BOUND_PORTS`] when `Ok`, a raw port
    /// when `Err`.
    pub src_port: Result<u8, u16>,
    /// The destination port, as `src_port`.
    pub dst_port: Result<u8, u16>,
    /// The payload, cut to [`MAX_PAYLOAD`], and passed through
    /// [`pubsub_domain`] when the datagram reaches `bm_handle_msg`.
    pub payload: Vec<u8>,
}

fn port(choice: Result<u8, u16>) -> u16 {
    match choice {
        Ok(index) => BOUND_PORTS[usize::from(index) % BOUND_PORTS.len()],
        Err(port) => port,
    }
}

/// Whether the oracle hands a datagram between these ports to
/// `bm_handle_msg`.
#[must_use]
pub fn reaches_pubsub(src_port: u16, dst_port: u16) -> bool {
    dst_port == MIDDLEWARE_PORT && src_port == MIDDLEWARE_PORT
}

/// `bytes` as a publication `bm_handle_msg` reads in bounds: a header of type
/// 0, flags 0, a `topic_len` of the first byte capped at what follows, ext
/// type 1, version 2, then `bytes` as topic and data.
#[must_use]
pub fn publication(bytes: &[u8]) -> Vec<u8> {
    let topic_len = bytes
        .first()
        .map_or(0, |b| usize::from(*b).min(bytes.len())) as u8;
    let mut out = vec![0, 0, topic_len, 1, 2];
    out.extend_from_slice(bytes);
    out
}

/// `bytes` as a payload `bm_handle_msg` reads nothing past: padded to
/// [`PUBSUB_HEADER_LEN`], and, where `topic_len` runs past the end, with one
/// topic byte in bounds set to [`OFF_PATTERN`].
///
/// With the topic past the end, `bm_handle_msg` still matches it against every
/// subscription and calls the matching callbacks with a wrapped data length
/// (divergence #75). `bm_wildcard_match` reads nothing of the topic against
/// `*`, and against a pattern starting with any other literal byte reads the
/// topic's first byte and stops. The `*` callback here reads nothing when the
/// data length is wrapped. So the C reads in bounds, and the fuzz build's
/// AddressSanitizer checks that.
#[must_use]
pub fn pubsub_domain(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    if out.len() < PUBSUB_HEADER_LEN {
        out.resize(PUBSUB_HEADER_LEN, 0);
    }
    if pubsub::decode(&out).is_err() {
        match out.get_mut(PUBSUB_HEADER_LEN) {
            Some(first) => *first = OFF_PATTERN,
            None => out.push(OFF_PATTERN),
        }
    }
    out
}

impl Arrival {
    fn ingress(&self) -> u8 {
        self.ingress % NUM_PORTS + 1
    }

    fn ports(&self) -> (u16, u16) {
        (port(self.src_port), port(self.dst_port))
    }

    fn payload(&self) -> Vec<u8> {
        let payload = &self.payload[..self.payload.len().min(MAX_PAYLOAD - PUBSUB_HEADER_LEN)];
        let (src_port, dst_port) = self.ports();
        if reaches_pubsub(src_port, dst_port) {
            pubsub_domain(payload)
        } else {
            payload.to_vec()
        }
    }

    /// The frame the peer put on the wire toward [`Self::ingress`], or `None`
    /// if it sends nothing to `dst`, or nothing to that port.
    fn frame(&self) -> Option<Vec<u8>> {
        let (src_port, dst_port) = self.ports();
        let mut peer: Node<Peer, NoRtc, 4> = Node::new(Peer(self.src), NoRtc, NUM_PORTS);
        let outbound = peer.send_udp(src_port, &self.dst.addr(), dst_port, &self.payload())?;
        capture(outbound)
            .into_iter()
            .find(|(port, _)| *port == 0 || *port == self.ingress())
            .map(|(_, frame)| frame)
    }
}

/// One step of a run.
#[derive(Debug, Clone, Arbitrary)]
pub enum Step {
    /// The oracle and a Rust node send the same datagram.
    Send(Send),
    /// A Rust peer's datagram arrives at the oracle and a Rust node.
    Receive(Arrival),
    /// The oracle publishes, and `bm_wire::pubsub` encodes the same.
    Publish(Publish),
}

/// Steps run in order against the one oracle.
#[derive(Debug, Clone, Arbitrary)]
pub struct NodeUdpInput {
    /// The steps.
    pub steps: Vec<Step>,
}

/// Run every step.
///
/// # Panics
///
/// On any divergence; see [`check_send`] and [`check_receive`].
pub fn check(input: &NodeUdpInput) {
    for step in &input.steps {
        match step {
            Step::Send(send) => check_send(send),
            Step::Receive(arrival) => check_receive(arrival),
            Step::Publish(publish) => check_publish(publish),
        }
    }
}

/// A call `bm_handle_msg` made to the `*` subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Published {
    /// Topic and data within the payload: `(node id, topic, data, type,
    /// version)`.
    Read(u64, Vec<u8>, Vec<u8>, u8, u8),
    /// A data length past [`pubsub::MAX_MESSAGE_LEN`], which no payload here
    /// reaches, so wrapped (divergence #75); the topic and data are not read.
    /// `(node id, topic_len, data_len, type, version)`.
    Wrapped(u64, u16, u16, u8, u8),
}

/// What `bm_handle_msg` hands a subscriber for `payload` from `node_id`,
/// computed from [`pubsub::decode`] and, where it refuses, from the C's
/// `size - sizeof(BmPubSubData) - topic_len` in 16 bits.
///
/// # Panics
///
/// If `payload` is shorter than [`PUBSUB_HEADER_LEN`].
#[must_use]
pub fn expected_publication(node_id: u64, payload: &[u8]) -> Published {
    match pubsub::decode(payload) {
        Ok(p) => Published::Read(
            node_id,
            p.topic.to_vec(),
            p.data.to_vec(),
            p.kind,
            p.version,
        ),
        Err(_) => {
            let topic_len = u16::from(payload[2]);
            let data_len = (payload.len() as u16)
                .wrapping_sub(PUBSUB_HEADER_LEN as u16)
                .wrapping_sub(topic_len);
            Published::Wrapped(node_id, topic_len, data_len, payload[3], payload[4])
        }
    }
}

static PUBLISHED: Mutex<Vec<Published>> = Mutex::new(Vec::new());

unsafe extern "C" fn on_publication(
    node_id: u64,
    topic: *const core::ffi::c_char,
    topic_len: u16,
    data: *const u8,
    data_len: u16,
    kind: u8,
    version: u8,
) {
    let published = if usize::from(data_len) > pubsub::MAX_MESSAGE_LEN {
        Published::Wrapped(node_id, topic_len, data_len, kind, version)
    } else {
        let (topic, data) = unsafe {
            (
                std::slice::from_raw_parts(topic.cast::<u8>(), usize::from(topic_len)).to_vec(),
                std::slice::from_raw_parts(data, usize::from(data_len)).to_vec(),
            )
        };
        Published::Read(node_id, topic, data, kind, version)
    };
    PUBLISHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(published);
}

static SUBSCRIBED: Once = Once::new();

/// Bind [`BOUND_PORTS`] and subscribe to `*` in the oracle, once per
/// process. `*` matches every topic `bm_wildcard_match` is given.
///
/// Asserts [`pubsub_domain`]'s premise on every call, since a test may have
/// subscribed since: every oracle subscription but `*` starts with a literal
/// byte other than [`OFF_PATTERN`].
fn prepare(guard: &std::sync::MutexGuard<'static, ()>) {
    bind(guard);
    SUBSCRIBED.call_once(|| unsafe {
        assert_eq!(
            bm_wire_sys::bm_sub_wl(c"*".as_ptr(), 1, Some(on_publication)),
            bm_wire_sys::BmErr_BmOK,
            "bm_sub_wl"
        );
        seed_pub_list();
    });
    for topic in subscriptions() {
        if topic != b"*" {
            assert!(
                !matches!(topic.first(), None | Some(b'*' | b'?' | &OFF_PATTERN)),
                "subscription {:?} breaks pubsub_domain",
                String::from_utf8_lossy(&topic)
            );
        }
    }
}

/// Put [`pool_topic`]`(254)` at the head of `resource_discovery.c`'s
/// `PUB_LIST`, which starts empty.
///
/// `bm_pub_wl` looks each topic it sends up in `PUB_LIST` with
/// `bcmp_resource_discovery_find_resource`, which compares the topic's length
/// against every entry and so reads past any shorter one (divergence #38).
/// Every [`Publish`] topic is a prefix of this entry, so the lookup matches it
/// first and reads nothing past it, and `PUB_LIST` never grows.
fn seed_pub_list() {
    let topic = pool_topic(TOPIC_POOL_LEN);
    unsafe {
        let mut count = 0u16;
        assert_eq!(
            bm_wire_sys::bcmp_resource_discovery_get_num_resources(
                &mut count,
                bm_wire_sys::ResourceType_PUB,
                0,
            ),
            bm_wire_sys::BmErr_BmOK
        );
        assert_eq!(count, 0, "PUB_LIST is not empty");
        assert_eq!(
            bm_wire_sys::bcmp_resource_discovery_add_resource(
                topic.as_ptr().cast(),
                topic.len() as u16,
                bm_wire_sys::ResourceType_PUB,
                0,
            ),
            bm_wire_sys::BmErr_BmOK
        );
    }
}

/// The oracle's subscriptions, from `bm_get_subs`, which joins them with
/// `" | "`. Its buffer is 256 bytes and unchecked (divergence #78), so the
/// oracle must hold few.
fn subscriptions() -> Vec<Vec<u8>> {
    unsafe {
        let subs = bm_wire_sys::bm_get_subs();
        assert!(!subs.is_null(), "bm_get_subs");
        let out = std::ffi::CStr::from_ptr(subs)
            .to_bytes()
            .split(|b| *b == b'|')
            .map(|t| t.trim_ascii().to_vec())
            .collect();
        bm_wire_sys::bm_free(subs.cast());
        out
    }
}

fn take_published() -> Vec<Published> {
    std::mem::take(&mut *PUBLISHED.lock().unwrap_or_else(|p| p.into_inner()))
}

/// What a Rust node reported as [`Event::Udp`]: `(port, source port, source
/// node id, payload)`.
pub type Reported = (u16, u16, u64, Vec<u8>);

/// Hand `frame` to `node` on `ingress`, returning its UDP events and what it
/// relayed.
pub fn receive(
    node: &mut Node<Peer, NoRtc, 4>,
    ingress: u8,
    frame: &mut [u8],
) -> (Vec<Reported>, Vec<Captured>) {
    let mut reported = Vec::new();
    let owed = node.on_frame_with(0, ingress, frame, |event| {
        if let Event::Udp {
            port,
            src_port,
            source,
            payload,
        } = event
        {
            reported.push((port, src_port, source, payload.to_vec()));
        }
    });
    assert!(owed.reply.is_none() && owed.forward.is_none());
    let relayed = owed.relay.map(capture).unwrap_or_default();
    (reported, relayed)
}

/// Assert the oracle and [`Node::send_udp`] send the same datagram as the
/// module docs say, and that each oracle frame reaches a Rust node.
///
/// # Panics
///
/// On any divergence.
pub fn check_send(send: &Send) {
    let c = crate::udp::check_send(send);

    let src_port = send.src_port();
    let dst = send.dst_addr();
    let payload = send.payload();

    let mut node = stack::node();
    let rs = node
        .send_udp(src_port, &dst, send.dst_port, payload)
        .map(capture)
        .unwrap_or_default();
    let mut built = vec![0u8; udp::PAYLOAD_OFFSET + payload.len()];
    udp::build(
        &mut built,
        &udp::source_address(stack::NODE_ID, &dst),
        &dst,
        src_port,
        send.dst_port,
        payload,
    )
    .expect("sized for the payload");
    assert_eq!(rs, port_transmit(&built), "Node::send_udp ({send:?})");
    assert_eq!(
        c.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        rs.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        "egress ports ({send:?})"
    );

    for (port, frame) in c {
        let mut frame = frame;
        let mut receiver = receiver(!stack::NODE_ID);
        let (reported, _) = receive(&mut receiver, port.max(1), &mut frame);
        let expected: Vec<Reported> = rust_bound_ports()
            .contains(&send.dst_port)
            .then(|| (send.dst_port, src_port, stack::NODE_ID, payload.to_vec()))
            .into_iter()
            .collect();
        assert_eq!(
            reported, expected,
            "the oracle's frame to port {port} ({send:?})"
        );
    }
}

/// Assert a Rust peer's datagram is relayed alike by the oracle and a Rust
/// node, and reaches the same bound ports with the same payload.
///
/// # Panics
///
/// On any divergence.
pub fn check_receive(arrival: &Arrival) {
    let Some(frame) = arrival.frame() else {
        return;
    };
    let ingress = arrival.ingress();
    let (src_port, dst_port) = arrival.ports();

    let guard = oracle();
    prepare(&guard);
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );
    let _ = take_delivered();
    let _ = take_published();
    inject(ingress, &frame);
    let c_relayed = drain();
    let delivered = take_delivered();
    let published = take_published();
    drop(guard);

    let mut rs_frame = frame.clone();
    let mut node = receiver(!arrival.src);
    let (reported, rs_relayed) = receive(&mut node, ingress, &mut rs_frame);

    assert_eq!(c_relayed, rs_relayed, "relay ({arrival:?})");

    let datagram = udp::accept(&frame).expect("a frame the peer built");
    let payload = datagram.payload.to_vec();
    let source = datagram.source;

    let expected: Vec<Reported> = rust_bound_ports()
        .contains(&dst_port)
        .then(|| (dst_port, src_port, source, payload.clone()))
        .into_iter()
        .collect();
    assert_eq!(reported, expected, "Rust delivery ({arrival:?})");

    let index = BOUND_PORTS.iter().position(|p| *p == dst_port);
    let expected: Vec<crate::udp::Delivery> = index
        .filter(|_| dst_port != MIDDLEWARE_PORT)
        .map(|index| (index, src_port, source, payload.clone()))
        .into_iter()
        .collect();
    assert_eq!(delivered, expected, "C delivery ({arrival:?})");

    let expected: Vec<Published> = if reaches_pubsub(src_port, dst_port) {
        vec![expected_publication(source, &payload)]
    } else {
        Vec::new()
    };
    assert_eq!(published, expected, "C publication ({arrival:?})");
}

/// Topics [`Publish`] draws from: a prefix of this, cycled to its length.
/// Starts with no oracle subscription's first byte, so only `*` matches a
/// local delivery. All are prefixes of one so that `bm_pub_wl`'s `PUB_LIST`
/// lookup matches the entry this module seeds first (divergence #38).
pub const TOPIC_POOL: &[u8] = b"spotter/printf/sensor/0123456789abcdef/";

/// The longest topic `bm_pub_wl` sends, `BM_TOPIC_MAX_LEN - 1`.
pub const TOPIC_POOL_LEN: usize = pubsub::TOPIC_MAX_LEN - 1;

/// The topic of `len` bytes [`Publish`] sends.
#[must_use]
pub fn pool_topic(len: usize) -> Vec<u8> {
    TOPIC_POOL.iter().copied().cycle().take(len).collect()
}

/// A publication the oracle makes with `bm_pub_wl`.
#[derive(Debug, Clone, Arbitrary)]
pub struct Publish {
    /// The topic's length; the topic is [`pool_topic`]'s. 0 and 255 are
    /// refused.
    pub topic_len: u8,
    /// `ext_header.type`.
    pub kind: u8,
    /// `ext_header.version`.
    pub version: u8,
    /// The data, cut to [`pubsub::MAX_MESSAGE_LEN`], so that a publication
    /// too long to send is reachable. `bm_pub_wl` takes a `uint16_t` length
    /// and wraps past 65530 bytes less the topic (divergence #76).
    pub data: Vec<u8>,
}

impl Publish {
    fn data(&self) -> &[u8] {
        &self.data[..self.data.len().min(pubsub::MAX_MESSAGE_LEN)]
    }
}

static PREFIXED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn count_publication(
    _: u64,
    _: *const core::ffi::c_char,
    _: u16,
    _: *const u8,
    _: u16,
    _: u8,
    _: u8,
) {
    PREFIXED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The oracle's metrics service request topic, `<node id>/metrics/req`: the
/// one entry in `SUB_LIST` before `*`.
#[must_use]
pub fn metrics_request_topic() -> Vec<u8> {
    format!("{:016x}/metrics/req", stack::NODE_ID).into_bytes()
}

/// Whether the oracle delivers a local publication on `topic` to a
/// subscription to `pattern`: subscribe, `bm_pub_wl`, unsubscribe.
///
/// `pattern` must be a prefix of [`metrics_request_topic`]: `bm_sub_wl` looks
/// it up in `SUB_LIST`, which reads past every shorter entry before a match
/// (divergence #38), and the entry after it is `*`. The publication carries
/// [`pubsub::MAX_MESSAGE_LEN`] bytes of data, too long to send: `bm_pub_wl`
/// delivers it locally, then refuses it before it reaches `PUB_LIST`.
///
/// # Panics
///
/// If `pattern` is not such a prefix, `topic` matches the metrics service's
/// subscription, or the oracle refuses the subscription or the
/// unsubscription.
#[must_use]
pub fn oracle_delivers(pattern: &[u8], topic: &[u8]) -> bool {
    let service = metrics_request_topic();
    assert!(service.starts_with(pattern), "{pattern:?}");
    assert!(
        !bm_wire::util::bm_wildcard_match(topic, &service),
        "{topic:?}"
    );
    let data = [0u8; pubsub::MAX_MESSAGE_LEN];

    let guard = oracle();
    prepare(&guard);
    PREFIXED.store(0, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        let ok = bm_wire_sys::BmErr_BmOK;
        let len = |s: &[u8]| s.len() as u16;
        assert_eq!(
            bm_wire_sys::bm_sub_wl(
                pattern.as_ptr().cast(),
                len(pattern),
                Some(count_publication)
            ),
            ok,
            "bm_sub_wl"
        );
        assert_eq!(
            bm_wire_sys::bm_pub_wl(
                topic.as_ptr().cast(),
                len(topic),
                data.as_ptr().cast(),
                len(&data),
                1,
                2
            ),
            bm_wire_sys::BmErr_BmEINVAL,
            "bm_pub_wl"
        );
        pump_until_quiet();
        assert_eq!(
            bm_wire_sys::bm_unsub_wl(
                pattern.as_ptr().cast(),
                len(pattern),
                Some(count_publication)
            ),
            ok,
            "bm_unsub_wl"
        );
    }
    drain();
    let _ = take_published();
    PREFIXED.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// Assert `bm_pub_wl` and [`pubsub::encode`] agree: on refusing, on the frames
/// the oracle sends, and on what reaches the oracle's own `*` subscription.
///
/// | [`pubsub::encode`] | `bm_pub_wl` | Local delivery | Frames |
/// |---|---|---|---|
/// | [`BmWireError::Invalid`] | `BmEINVAL`, `BmEMSGSIZE` | none | none |
/// | longer than [`pubsub::MAX_MESSAGE_LEN`] | `BmEINVAL` | the decoding | none |
/// | otherwise | `BmOK` | the decoding | [`as_bm_linux_sends_it`] from and to [`pubsub::PORT`] at `ff03::1` |
///
/// # Panics
///
/// On any divergence.
pub fn check_publish(publish: &Publish) {
    let topic = pool_topic(usize::from(publish.topic_len));
    let data = publish.data();

    let guard = oracle();
    prepare(&guard);
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );
    let _ = take_published();
    let err = unsafe {
        bm_wire_sys::bm_pub_wl(
            topic.as_ptr().cast(),
            topic.len() as u16,
            data.as_ptr().cast(),
            data.len() as u16,
            publish.kind,
            publish.version,
        )
    };
    pump_until_quiet();
    let c = drain();
    let published = take_published();
    drop(guard);

    let mut buf = vec![0u8; PUBSUB_HEADER_LEN + topic.len() + data.len()];
    match pubsub::encode(&mut buf, &topic, publish.kind, publish.version, data) {
        Err(e) => {
            assert_eq!(e, BmWireError::Invalid, "encode ({publish:?})");
            assert!(
                [bm_wire_sys::BmErr_BmEINVAL, bm_wire_sys::BmErr_BmEMSGSIZE].contains(&err),
                "bm_pub_wl returned {err} ({publish:?})"
            );
            assert!(published.is_empty(), "local delivery ({publish:?})");
            assert!(c.is_empty(), "frames ({publish:?})");
        }
        Ok(len) => {
            assert_eq!(len, buf.len());
            assert_eq!(
                published,
                vec![expected_publication(stack::NODE_ID, &buf)],
                "local delivery ({publish:?})"
            );
            if len > pubsub::MAX_MESSAGE_LEN {
                assert_eq!(err, bm_wire_sys::BmErr_BmEINVAL, "bm_pub_wl ({publish:?})");
                assert!(c.is_empty(), "frames ({publish:?})");
            } else {
                assert_eq!(err, bm_wire_sys::BmErr_BmOK, "bm_pub_wl ({publish:?})");
                let rs = as_bm_linux_sends_it(
                    pubsub::PORT,
                    &BmIpAddr::GLOBAL_MULTICAST,
                    pubsub::PORT,
                    &buf,
                );
                assert_eq!(c, rs, "frames ({publish:?})");
            }
        }
    }
}
