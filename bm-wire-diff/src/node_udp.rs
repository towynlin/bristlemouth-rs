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
//! | [`Step::Receive`] | a Rust peer's frame injected on one port | the same frame to [`Node::on_frame_with`] on the same port | the same relay; the same payload reaching each bound port |
//!
//! Where the oracle delivers a received datagram:
//!
//! | Destination port | Source port | Reaches |
//! |---|---|---|
//! | [`MIDDLEWARE_PORT`] | [`MIDDLEWARE_PORT`] | `bm_middleware_rx`, then `bm_handle_msg`, then this module's `*` subscription |
//! | [`MIDDLEWARE_PORT`] | anything else | `bm_middleware_rx`, then nothing: `middleware_net_task` looks the application up by source port (divergence #73) |
//! | another of [`BOUND_PORTS`] | any | [`crate::udp`]'s callback for it |
//!
//! `bm_handle_msg` reads past a payload shorter than its `topic_len`
//! (suspected, card P1), so [`Arrival`] rewrites the payload of a datagram that
//! reaches it into a well-formed publication.

use std::sync::{Mutex, Once};

use arbitrary::Arbitrary;

use bm_stack::{Event, Identity, NoRtc, Node};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::udp;

use crate::l2_egress::port_transmit;
use crate::stack::{self, Captured, NUM_PORTS, capture, drain, inject, oracle};
use crate::udp::{BOUND_PORTS, Dst, MAX_PAYLOAD, MIDDLEWARE_PORT, Send, bind, take_delivered};

/// `sizeof(BmPubSubData)`: type, flags, `topic_len`, ext type, ext version.
pub const PUBSUB_HEADER_LEN: usize = 5;

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
    /// The payload, cut to [`MAX_PAYLOAD`], and rewritten by
    /// [`publication`] when the datagram reaches `bm_handle_msg`.
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
            publication(payload)
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
        }
    }
}

/// A publication `bm_handle_msg` handed the `*` subscription:
/// `(node id, topic, data, type, version)`.
pub type Published = (u64, Vec<u8>, Vec<u8>, u8, u8);

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
    let (topic, data) = unsafe {
        (
            std::slice::from_raw_parts(topic.cast::<u8>(), usize::from(topic_len)).to_vec(),
            std::slice::from_raw_parts(data, usize::from(data_len)).to_vec(),
        )
    };
    PUBLISHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((node_id, topic, data, kind, version));
}

static SUBSCRIBED: Once = Once::new();

/// Bind [`BOUND_PORTS`] and subscribe to `*` in the oracle, once per
/// process. `*` matches every topic `bm_wildcard_match` is given.
fn prepare(guard: &std::sync::MutexGuard<'static, ()>) {
    bind(guard);
    SUBSCRIBED.call_once(|| unsafe {
        assert_eq!(
            bm_wire_sys::bm_sub_wl(c"*".as_ptr(), 1, Some(on_publication)),
            bm_wire_sys::BmErr_BmOK,
            "bm_sub_wl"
        );
    });
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
        let topic_len = usize::from(payload[2]);
        let rest = &payload[PUBSUB_HEADER_LEN..];
        vec![(
            source,
            rest[..topic_len].to_vec(),
            rest[topic_len..].to_vec(),
            payload[3],
            payload[4],
        )]
    } else {
        Vec::new()
    };
    assert_eq!(published, expected, "C publication ({arrival:?})");
}
