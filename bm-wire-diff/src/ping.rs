//! Differential comparator for [`bm_wire::bcmp::ping`] and [`bm_stack`]'s ping
//! half, driven through bm_core's live stack.
//!
//! # What is being compared
//!
//! Two frames, in both directions of the exchange:
//!
//! * **The request.** `bcmp_send_ping_request` is the one public encoder in
//!   `bcmp/ping.c`, so the oracle can simply be told to ping and the frame read
//!   off the wire. [`Node::ping`][bm_stack::Node::ping] is asked for the same
//!   ping from the same identity, and the two are compared byte for byte. That
//!   pins every quirk the request carries: the id truncated out of the node id,
//!   `ping.c`'s own counter truncated into a sixteen-bit field, a header
//!   sequence number of zero, and a `payload_len` of zero whenever the payload
//!   pointer was null.
//! * **The reply.** An echo request is injected and the reply the oracle builds
//!   is compared against the one `Node::on_frame` builds. That pins the rest:
//!   the in-place reuse of the request buffer, the substitution of
//!   `target_node_id`, and what becomes of the `seq_num`
//!   `bcmp_send_ping_reply` asks `bcmp_tx` to echo.
//!
//! # What cannot be compared, and why it is unit-tested instead
//!
//! `bcmp_process_ping_reply` is the requester's half and it is **invisible from
//! outside the C**. It is `static`, it transmits nothing, and its verdict
//! reaches nobody: `bcmp_send_ping_request` takes no callback, and
//! `BcmpEchoReplyMessage` is registered unsequenced so `packet.c` has no
//! callback to reach either. A matched reply produces a `bm_debug` line and an
//! ignored return code. That is divergence #30.
//!
//! So the acceptance rule — [`EchoReply::answers`] — has no oracle, and this
//! comparator does not pretend otherwise. It is ported by reading and asserted
//! in `bm_wire::bcmp::ping`'s own unit tests, and the divergences it admits are
//! written down rather than measured. What *is* compared here is everything
//! that reaches the wire.
//!
//! # Input domain
//!
//! Nothing malformed is handed to the C. `bcmp_process_ping_request` echoes
//! `payload_len` bytes out of the received frame without ever comparing it
//! against `BcmpProcessData.size`, which is right there in the same struct, so
//! a request declaring more payload than it carries makes the C read — and
//! transmit — past the frame. That is divergence #27, and it has no defined
//! behaviour to compare against. Every injected request declares exactly what
//! it carries; the [`PingInput::decode_probe`] bytes exercise the Rust
//! decoders with arbitrary input instead, and never reach the C.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`].

use std::sync::{Mutex, MutexGuard, OnceLock};

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_stack::Node;
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::l2;
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::stack::{self, NUM_PORTS, OracleIdentity, drain, inject, oracle, pump_until_quiet};

/// Node id the injected request appears to come from, and the one a ping is
/// aimed at. Anything but the oracle's own.
pub const PEER_NODE_ID: u64 = 0x0000_0000_55AA_0011;

/// Longest ping payload the comparator will use.
///
/// bm_core's own ceiling is `bcmp_tx`'s `max_payload_len` (1460). This is well
/// inside it, keeps fuzz inputs small, and is what [`PingNode`]'s expectation
/// slot is sized for.
pub const MAX_PING_PAYLOAD: usize = 256;

/// A `bm-stack` node whose ping slot is as large as this comparator's domain.
pub type PingNode = Node<OracleIdentity, 4, 4, MAX_PING_PAYLOAD>;

/// The Rust node, kept for the life of the process alongside the C stack.
///
/// It has to be persistent for the same reason the oracle does: `BCMP_SEQ` is
/// a file-scope `static` in `ping.c` with nothing that resets it, and a fresh
/// Rust node would start counting from zero again while the C carried on. One
/// node per process keeps the two counters in lockstep, which is the point —
/// the number the *nth* ping carries is part of what is being compared.
static NODE: OnceLock<Mutex<PingNode>> = OnceLock::new();

/// The paired oracle and node, both locked, both where the last call left them.
fn pair() -> (MutexGuard<'static, ()>, MutexGuard<'static, PingNode>) {
    let guard = oracle();
    let node = NODE
        .get_or_init(|| {
            let mut node = PingNode::new(OracleIdentity, NUM_PORTS);
            // `stack::oracle` brings both of the capture device's ports up
            // before any comparison; a node that disagreed about that would
            // disagree about more than ping.
            for port in 1..=NUM_PORTS {
                node.set_link_up(port, true);
            }
            Mutex::new(node)
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    (guard, node)
}

// ---------------------------------------------------------------------------
// The input
// ---------------------------------------------------------------------------

/// Who a ping, or an injected echo request, is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Zero, which every node answers.
    All,
    /// The oracle's own node id, which it answers.
    ThisNode,
    /// Somebody else's, which it must ignore.
    OtherNode,
}

impl Target {
    /// The `target_node_id` this puts in the body.
    #[must_use]
    pub fn node_id(self) -> u64 {
        match self {
            Self::All => 0,
            Self::ThisNode => stack::NODE_ID,
            Self::OtherNode => PEER_NODE_ID,
        }
    }

    fn expects_reply(self) -> bool {
        !matches!(self, Self::OtherNode)
    }
}

/// One ping put to the oracle and one echo request injected into it, plus
/// bytes for the decoders alone.
#[derive(Debug, Clone)]
pub struct PingInput {
    /// Who the ping and the injected request are addressed to.
    pub target: Target,
    /// Port to inject on, clamped to 1..=[`NUM_PORTS`] by [`Domain`].
    pub ingress_port: u8,
    /// Use `FF03::1` rather than `FF02::1`. The reply goes back to whichever
    /// the request came to, so this also decides whether it is stamped with an
    /// egress port.
    pub global_multicast: bool,
    /// Identifier the injected echo request carries, which the reply must echo
    /// unchanged. Not the one an *outgoing* ping carries — that is derived
    /// from the node id and is one of the things being compared.
    pub request_id: u16,
    /// Sequence number the injected echo request carries, likewise echoed.
    pub request_seq: u16,
    /// Pass a null payload pointer to `bcmp_send_ping_request` while still
    /// declaring `payload.len()`, which the C silently rewrites to zero.
    pub null_payload: bool,
    /// The ping payload, capped at [`MAX_PING_PAYLOAD`] by [`Domain`].
    pub payload: Vec<u8>,
    /// Arbitrary bytes fed to the Rust decoders and **never** to the C, for
    /// the reason the module docs give.
    pub decode_probe: Vec<u8>,
}

impl<'a> Arbitrary<'a> for PingInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(u)?;
        let len = u.arbitrary_len::<u8>()?;
        input.payload = u.bytes(len)?.to_vec();
        let len = u.arbitrary_len::<u8>()?;
        input.decode_probe = u.bytes(len)?.to_vec();
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(&mut u)?;
        // `Vec<u8>`'s own `Arbitrary` stops after each element with
        // probability one half, so taking a length up front is what keeps the
        // fuzzer from only ever seeing two-byte payloads.
        let len = u.arbitrary_len::<u8>()?;
        input.payload = u.bytes(len)?.to_vec();
        input.decode_probe = u.take_rest().to_vec();
        input.clamp_to_domain();
        Ok(input)
    }
}

impl Domain for PingInput {
    fn clamp_to_domain(&mut self) {
        // Map any byte into 1..=NUM_PORTS without moving values already in range.
        self.ingress_port = self.ingress_port.wrapping_sub(1) % NUM_PORTS + 1;
        self.payload.truncate(MAX_PING_PAYLOAD);
        self.decode_probe.truncate(2048);
    }
}

impl PingInput {
    fn scalars(u: &mut Unstructured<'_>) -> Result<Self> {
        let target = match u.arbitrary::<u8>()? % 3 {
            0 => Target::All,
            1 => Target::ThisNode,
            _ => Target::OtherNode,
        };
        Ok(Self {
            target,
            ingress_port: u.arbitrary()?,
            global_multicast: u.arbitrary()?,
            request_id: u.arbitrary()?,
            request_seq: u.arbitrary()?,
            null_payload: u.arbitrary()?,
            payload: Vec::new(),
            decode_probe: Vec::new(),
        })
    }

    /// The address both sides send to.
    #[must_use]
    pub fn destination(&self) -> BmIpAddr {
        if self.global_multicast {
            BmIpAddr::GLOBAL_MULTICAST
        } else {
            BmIpAddr::LINK_LOCAL_MULTICAST
        }
    }

    /// What `bcmp_send_ping_request` actually pings with: a null pointer makes
    /// it drop the length on the floor.
    fn sent_payload(&self) -> &[u8] {
        if self.null_payload {
            &[]
        } else {
            &self.payload
        }
    }

    /// The echo request frame to inject, checksummed and ready.
    ///
    /// Its `payload_len` is what it carries. See the module docs for why the C
    /// is never told otherwise.
    fn build_request(&self) -> Vec<u8> {
        let request = EchoRequest {
            target_node_id: self.target.node_id(),
            id: self.request_id,
            seq_num: self.request_seq,
            payload: &self.payload,
        };
        let mut body = vec![0u8; request.encoded_len()];
        request.encode(&mut body).expect("body is sized for it");

        let payload_len = BCMP_HEADER_LEN + body.len();
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
            .copy_from_slice(&bm_wire::addr::nodeid_to_ip(0xFE80_0000, PEER_NODE_ID).0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&self.destination().0);
        tx::serialize(&mut frame, MessageType::ECHO_REQUEST, 0, &body)
            .expect("frame is sized for the body");
        frame
    }
}

/// The echo request frame an input injects, for tests that want to look at it.
#[must_use]
pub fn build_request(input: &PingInput) -> Vec<u8> {
    let mut input = input.clone();
    input.clamp_to_domain();
    input.build_request()
}

// ---------------------------------------------------------------------------
// Comparing frames
// ---------------------------------------------------------------------------

fn assert_same_frame(what: &str, port: u8, c: &[u8], rs: &[u8], input: &PingInput) {
    if c == rs {
        return;
    }
    let at = c
        .iter()
        .zip(rs)
        .position(|(a, b)| a != b)
        .unwrap_or(c.len().min(rs.len()));
    panic!(
        "{what} on port {port} diverged at byte {at}: C {:#04x?}, bm-stack {:#04x?}\n  input:    {input:?}\n  C:        {c:02x?}\n  bm-stack: {rs:02x?}",
        c.get(at),
        rs.get(at),
    );
}

/// Compare one frame we built against every copy the C put on the wire.
///
/// A link-local frame leaves once per port with that port stamped into its
/// source address and the checksum patched; a global-multicast one leaves once,
/// unstamped, on the device's all-ports encoding. `bm_stack::transmit` is what
/// does the stamping in the port, so this stamps the same way rather than
/// asserting about a frame no PHY would ever see.
fn compare_transmitted(what: &str, input: &PingInput, captured: &[&Vec<u8>], mut ours: Vec<u8>) {
    let expected = if input.global_multicast {
        1
    } else {
        usize::from(NUM_PORTS)
    };
    assert_eq!(
        captured.len(),
        expected,
        "{what}: expected {expected} copies, got {} ({input:?})",
        captured.len()
    );

    if input.global_multicast {
        assert_same_frame(what, 0, captured[0], &ours, input);
        return;
    }
    for (index, c_frame) in captured.iter().enumerate() {
        let port = index as u8 + 1;
        let stamped = l2::stamp_egress_port(&mut ours, port).expect("stampable");
        assert_same_frame(what, port, c_frame, &stamped, input);
    }
}

/// Every captured frame of one message type, in transmit order.
///
/// The type is read straight out of the BCMP header rather than through
/// [`rx::accept`], because **some of these frames do not validate** — and that
/// is the C being right, not wrong. `network_add_egress_port` patches the
/// checksum rather than recomputing it when it stamps a link-local frame's
/// egress port, and drops the end-around carry when the carry itself carries;
/// divergence #12 measures that at 120 of 983 040 cases. A ping's payload is
/// whatever the fuzzer chose, so this is the first comparator whose frames
/// range widely enough to land on one, and it does within minutes. The port
/// reproduces the patch exactly, in `l2::add_egress_port`, so the byte
/// comparison in [`compare_transmitted`] still holds — the frames agree, and
/// agree in being rejected by any node that receives them.
fn frames_of(captured: &[(u8, Vec<u8>)], message_type: MessageType) -> Vec<&Vec<u8>> {
    captured
        .iter()
        .filter(|(port, frame)| {
            let header = frame
                .get(BCMP_HEADER_OFFSET..)
                .and_then(|bcmp| BcmpHeader::decode(bcmp).ok())
                .unwrap_or_else(|| {
                    panic!("bm_core sent a frame with no BCMP header on port {port}")
                });
            header.message_type == message_type
        })
        .map(|(_, frame)| frame)
        .collect()
}

// ---------------------------------------------------------------------------
// The comparators
// ---------------------------------------------------------------------------

/// Assert the echo request `Node::ping` builds is the one
/// `bcmp_send_ping_request` builds.
///
/// # Panics
///
/// If the frames differ anywhere, or if either side declines to send.
pub fn check_request(input: &PingInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let (_guard, mut node) = pair();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );

    let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
    let addr = if input.global_multicast {
        &raw const bm_wire_sys::multicast_global_addr
    } else {
        &raw const bm_wire_sys::multicast_ll_addr
    };
    // A null payload pointer with a non-zero length: the C rewrites the length
    // to zero before it does anything else with it.
    let (payload_ptr, declared_len) = if input.null_payload {
        (std::ptr::null(), input.payload.len() as u16)
    } else {
        (input.payload.as_ptr(), input.payload.len() as u16)
    };
    unsafe {
        assert_eq!(
            bm_wire_sys::bcmp_send_ping_request(
                input.target.node_id(),
                addr.cast(),
                payload_ptr,
                declared_len,
            ),
            bm_wire_sys::BmErr_BmOK,
            "the oracle refused to ping ({input:?})"
        );
    }
    pump_until_quiet();
    let captured = drain();
    let requests = frames_of(&captured, MessageType::ECHO_REQUEST);

    let ours = node
        .ping(
            now_ms,
            &input.destination(),
            input.target.node_id(),
            input.sent_payload(),
        )
        .unwrap_or_else(|| panic!("the port refused to ping ({input:?})"))
        .frame()
        .to_vec();

    compare_transmitted("echo request", &input, &requests, ours);
}

/// Assert the reply `Node::on_frame` builds for an injected echo request is the
/// one `bcmp_process_ping_request` builds.
///
/// # Panics
///
/// If the frames differ, if one side answers and the other does not, or if a
/// request addressed to another node is answered at all.
pub fn check_reply(input: &PingInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let (_guard, mut node) = pair();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );

    let frame = input.build_request();
    inject(input.ingress_port, &frame);
    let captured = drain();
    let replies = frames_of(&captured, MessageType::ECHO_REPLY);

    let mut ours_frame = frame.clone();
    let owed = node.on_frame(0, input.ingress_port, &mut ours_frame);
    let ours = owed.reply.map(|reply| reply.frame().to_vec());

    if !input.target.expects_reply() {
        assert!(
            replies.is_empty(),
            "the C answered a ping addressed to another node ({input:?})"
        );
        assert!(
            ours.is_none(),
            "the port answered a ping addressed to another node ({input:?})"
        );
        return;
    }

    let ours = ours.unwrap_or_else(|| panic!("the port did not answer a ping ({input:?})"));
    compare_transmitted("echo reply", &input, &replies, ours);
}

/// Feed arbitrary bytes to both ping decoders.
///
/// There is no C counterpart to compare against — that is divergence #27 — so
/// the property checked is the port's own: a decoder either refuses the bytes
/// or produces something that re-encodes to a prefix of them, and it never
/// panics.
fn probe_decoders(bytes: &[u8]) {
    if let Ok(request) = EchoRequest::decode(bytes) {
        let len = request.encoded_len();
        let mut again = vec![0u8; len];
        assert_eq!(request.encode(&mut again).unwrap(), len);
        assert_eq!(&again[..], &bytes[..len], "echo request did not round-trip");

        // The reply is the same fourteen bytes, so anything that decodes as
        // one decodes as the other, and the substitution is the only change.
        let reply = EchoReply::decode(bytes).expect("identical layouts");
        assert_eq!(reply.id, request.id);
        assert_eq!(reply.seq_num, request.seq_num);
        assert_eq!(reply.payload, request.payload);
        assert_eq!(reply.node_id, request.target_node_id);
        assert_eq!(
            request.into_reply(stack::NODE_ID),
            EchoReply {
                node_id: stack::NODE_ID,
                ..reply
            }
        );
    } else {
        assert!(
            EchoReply::decode(bytes).is_err(),
            "the two decoders disagreed about bytes with the same layout"
        );
    }
}

/// Run every comparator.
///
/// # Panics
///
/// If any of them diverges from the C.
pub fn check(input: &PingInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    // The decoders get their workout whether or not the C is involved.
    probe_decoders(&input.decode_probe);

    check_request(&input);
    check_reply(&input);
}
