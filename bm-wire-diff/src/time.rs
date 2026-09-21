//! Differential comparator for [`bm_wire::bcmp::time`] and the system-time half
//! of [`bm_stack::Node`], driven through bm_core's live stack.
//!
//! # What is being compared
//!
//! `bcmp_time_process_time_message` and everything it calls are `static` inside
//! `bcmp/time.c`, so as with [`crate::bcmp_messages`] the only way at them is
//! the wire: inject a `0x10`, `0x11` or `0x12`, pump the tasks, and read what
//! the oracle put on the network device. [`check`] then runs the identical
//! frame through [`bm_stack::Node::on_frame`] and compares the `(port, bytes)`
//! sequences.
//!
//! Three separate behaviours fall out of that one comparison, which is why it
//! is worth doing it this way round rather than testing a codec in isolation:
//!
//! * **the response's bytes**, when the oracle answers at all — header,
//!   checksum, and the timestamp its RTC reported;
//! * **the decision not to answer**, which is where divergence #27 lives: a
//!   request or a response addressed to node 0 reaches the `switch` and is
//!   dropped by an inner exact-match test, while a *set* addressed to node 0 is
//!   honoured;
//! * **the decision to forward**, when the target is neither this node nor
//!   zero. `bcmp_ll_forward` itself is already compared per port by
//!   [`crate::forward`]; what this adds is the decision, and its interaction
//!   with L2's own relay — a global-multicast time message for a third node is
//!   put on the wire **twice**, once by each, which is divergence #28.
//!
//! # The clock is not bm_core's
//!
//! `bm_rtc_get`, `bm_rtc_set` and `bm_rtc_get_micro_seconds` are declared in
//! `bcmp/bm_rtc.h` and defined nowhere in bm_core: they are integrator hooks,
//! and the implementation the oracle links is ours,
//! `bm-wire-sys/csrc/bm_generic_shim.c`. So the *reading* is not a divergence
//! surface — there is no authoritative C to diverge from. Everything
//! downstream of it is: which messages provoke an answer, what the answer
//! carries, and what a `0x12` leaves the clock reading.
//!
//! [`crate::stack::set_both_clocks`] puts the same reading on both sides
//! before every comparison, and [`check`] asserts they still agree afterwards,
//! so a `0x12` that the two apply differently fails here rather than silently
//! later.
//!
//! # Input domain
//!
//! The C reads `BcmpSystemTimeHeader` — and, for `0x11` and `0x12`,
//! `utc_time_us` sixteen bytes past it — straight out of the received frame
//! without consulting `BcmpProcessData.size`. A body shorter than the message
//! type calls for is therefore an out-of-bounds read with no defined behaviour
//! to compare against; that is divergence #14's shape, and [`Domain`] keeps
//! every injected body at least as long as its type. `decode_probe` exercises
//! the Rust decoders with arbitrary bytes instead and is never handed to the C.
//!
//! Timestamps are kept inside `utc_from_date_time`'s `u32` seconds, which run
//! out in 2106. Past that the C's own conversion wraps its year field, which
//! [`bm_wire::util::date_time_from_utc`] reproduces and
//! `bm-wire/src/util.rs`'s tests pin; going there through this comparator
//! would cost a `while` loop of half a million iterations per seed and prove
//! nothing new.
//!
//! # Divergence #12 stops being rare here
//!
//! A system-time response carries a 64-bit timestamp straight off a clock, so
//! its body is the first thing ported whose bytes vary freely rather than
//! coming from a fixed `DeviceCfg`. That matters because `network_add_egress_port`
//! patches the BCMP checksum without folding the end-around carry back in
//! (divergence #12), which corrupts it whenever the carry itself carries —
//! about one frame in 40 000. Fixed-content messages hardly ever land there;
//! this one walks straight into it, and `cargo fuzz run time` finds a case in
//! under a minute.
//!
//! So [`read_back`] tolerates exactly that: a frame whose egress port has been
//! stamped may fail its checksum, and its body is read anyway. It is not a
//! divergence between the two stacks — [`check`] has already compared the
//! frames byte for byte, so both emitted the same unverifiable bytes — and
//! `bm-wire-diff/tests/l2_egress.rs` is where the corruption itself is pinned.
//! `a_response_whose_stamped_checksum_carries_twice_is_unverifiable` in
//! `bm-wire-diff/tests/time.rs` keeps one such timestamp on record.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`].

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_stack::port::RtcTimeAndDate;
use bm_wire::bcmp::time::{SystemTimeHeader, SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, MessageType, rx, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::stack::{
    self, NUM_PORTS, capture, capture_reflood, drain, inject, node_with_clock, oracle,
    oracle_clock_micros, set_both_clocks,
};

/// Node id the injected message appears to come from. Anything but the
/// oracle's own.
pub const PEER_NODE_ID: u64 = 0x0000_0000_55AA_0011;

/// A third node, so `target_node_id` can name somebody who is neither end.
pub const THIRD_NODE_ID: u64 = 0x0000_0000_0BAD_F00D;

/// One microsecond past the last timestamp `utc_from_date_time` can represent
/// in its `u32` of seconds: 2106-02-07T06:28:16Z.
pub const MAX_UTC_US: u64 = (u32::MAX as u64 + 1) * 1_000_000;

/// Most trailing bytes an injected body carries past the message it declares.
///
/// `data.size` is whatever arrived and the C's casts read a fixed prefix, so
/// trailing bytes must change nothing. Keeping the cap small keeps seeds small.
pub const MAX_TRAILING: usize = 64;

/// Which of the three system-time messages to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeMessage {
    /// `0x10`, answered with a `0x11` when the target matches exactly.
    Request,
    /// `0x11`, which a C node logs and otherwise ignores.
    Response,
    /// `0x12`, which sets the clock and is answered with a `0x11`.
    Set,
}

impl TimeMessage {
    /// The BCMP type byte.
    #[must_use]
    pub fn message_type(self) -> MessageType {
        match self {
            Self::Request => MessageType::SYSTEM_TIME_REQUEST,
            Self::Response => MessageType::SYSTEM_TIME_RESPONSE,
            Self::Set => MessageType::SYSTEM_TIME_SET,
        }
    }

    /// Shortest body the C can read without going out of bounds.
    #[must_use]
    pub fn body_len(self) -> usize {
        match self {
            Self::Request => SystemTimeRequest::LEN,
            Self::Response => SystemTimeResponse::LEN,
            Self::Set => SystemTimeSet::LEN,
        }
    }
}

/// Who the injected message names in its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Zero. Reaches the `switch` for all three types and is honoured by one.
    Everyone,
    /// The oracle's own node id.
    ThisNode,
    /// A third node's, which makes the oracle forward it.
    OtherNode,
}

impl Target {
    fn node_id(self) -> u64 {
        match self {
            Self::Everyone => 0,
            Self::ThisNode => stack::NODE_ID,
            Self::OtherNode => THIRD_NODE_ID,
        }
    }
}

/// One system-time message put to both stacks, plus the clock they start from.
#[derive(Debug, Clone)]
pub struct TimeInput {
    /// Which of the three to inject.
    pub message: TimeMessage,
    /// Who the body addresses.
    pub target: Target,
    /// Port it arrives on, clamped to 1..=[`NUM_PORTS`] by [`Domain`].
    pub ingress_port: u8,
    /// Address the frame to `FF03::1` rather than `FF02::1`. L2 relays the
    /// first and consumes the second, which is what makes the double
    /// transmission of divergence #28 visible.
    pub global_multicast: bool,
    /// Microseconds both clocks are set to before the message arrives.
    /// Clamped to [`MAX_UTC_US`] by [`Domain`].
    pub clock_us: u64,
    /// The timestamp a `0x11` or `0x12` carries. Clamped the same way.
    pub utc_time_us: u64,
    /// Trailing bytes past the declared message, capped at [`MAX_TRAILING`].
    pub trailing: Vec<u8>,
    /// Arbitrary bytes fed to the Rust decoders and **never** to the C.
    pub decode_probe: Vec<u8>,
}

impl<'a> Arbitrary<'a> for TimeInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(u)?;
        let len = u.arbitrary_len::<u8>()?;
        input.decode_probe = u.bytes(len)?.to_vec();
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(&mut u)?;
        input.decode_probe = u.take_rest().to_vec();
        input.clamp_to_domain();
        Ok(input)
    }
}

impl Domain for TimeInput {
    fn clamp_to_domain(&mut self) {
        // Map any byte into 1..=NUM_PORTS without moving values already in range.
        self.ingress_port = self.ingress_port.wrapping_sub(1) % NUM_PORTS + 1;
        self.clock_us %= MAX_UTC_US;
        self.utc_time_us %= MAX_UTC_US;
        self.trailing.truncate(MAX_TRAILING);
        self.decode_probe.truncate(512);
    }
}

impl TimeInput {
    fn scalars(u: &mut Unstructured<'_>) -> Result<Self> {
        let message = match u.arbitrary::<u8>()? % 3 {
            0 => TimeMessage::Request,
            1 => TimeMessage::Response,
            _ => TimeMessage::Set,
        };
        let target = match u.arbitrary::<u8>()? % 3 {
            0 => Target::Everyone,
            1 => Target::ThisNode,
            _ => Target::OtherNode,
        };
        let trailing_len = usize::from(u.arbitrary::<u8>()?) % (MAX_TRAILING + 1);
        Ok(Self {
            message,
            target,
            ingress_port: u.arbitrary()?,
            global_multicast: u.arbitrary()?,
            clock_us: u.arbitrary()?,
            utc_time_us: u.arbitrary()?,
            trailing: u.bytes(trailing_len)?.to_vec(),
            decode_probe: Vec::new(),
        })
    }

    fn destination(&self) -> BmIpAddr {
        if self.global_multicast {
            BmIpAddr::GLOBAL_MULTICAST
        } else {
            BmIpAddr::LINK_LOCAL_MULTICAST
        }
    }

    /// The reading both clocks are set to before the message arrives.
    #[must_use]
    pub fn clock(&self) -> RtcTimeAndDate {
        RtcTimeAndDate::from_utc_micros(self.clock_us)
    }

    /// Whether the oracle answers this message with a `0x11` of its own.
    ///
    /// The divergence-#27 table, in code: a request has to name this node
    /// exactly, a set only has to reach the switch, and a response is never
    /// answered at all. [`check`] sets both clocks first, so `bm_rtc_get`
    /// never fails and the third condition the C has does not arise here —
    /// `a_node_whose_clock_is_unset_answers_no_request` in
    /// `bm-wire-diff/tests/time.rs` covers that one.
    #[must_use]
    pub fn expects_response(&self) -> bool {
        match self.message {
            TimeMessage::Request => self.target == Target::ThisNode,
            TimeMessage::Response => false,
            TimeMessage::Set => self.target != Target::OtherNode,
        }
    }

    /// The message body: the declared type, then the trailing bytes.
    fn body(&self) -> Vec<u8> {
        let header = SystemTimeHeader {
            target_node_id: self.target.node_id(),
            source_node_id: PEER_NODE_ID,
        };
        let mut body = vec![0u8; self.message.body_len()];
        match self.message {
            TimeMessage::Request => SystemTimeRequest { header }.encode(&mut body),
            TimeMessage::Response => SystemTimeResponse {
                header,
                utc_time_us: self.utc_time_us,
            }
            .encode(&mut body),
            TimeMessage::Set => SystemTimeSet {
                header,
                utc_time_us: self.utc_time_us,
            }
            .encode(&mut body),
        }
        .expect("the buffer is the message's own length");
        body.extend_from_slice(&self.trailing);
        body
    }

    /// The frame both stacks are given, checksummed and ready to inject.
    fn build(&self) -> Vec<u8> {
        let body = self.body();
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
        tx::serialize(&mut frame, self.message.message_type(), 0, &body)
            .expect("frame is sized for the body");
        frame
    }
}

/// The frame an input injects, for tests that want to look at it first.
#[must_use]
pub fn build_frame(input: &TimeInput) -> Vec<u8> {
    let mut input = input.clone();
    input.clamp_to_domain();
    input.build()
}

/// Parse a captured frame the way a peer would, tolerating the one checksum
/// failure bm_core itself produces.
///
/// Returns the BCMP header, the body, and whether the checksum verified. A
/// frame L2 stamped an egress port into may fail — see divergence #12 and this
/// module's docs — and its body is still exactly what the sender wrote, because
/// the patch only ever touches the two checksum bytes.
///
/// # Panics
///
/// If the frame is not BCMP at all, or fails its checksum without having been
/// stamped, which would mean something other than #12 was wrong with it.
#[must_use]
pub fn read_back(port: u8, captured: &[u8]) -> (BcmpHeader, Vec<u8>, bool) {
    // The nibble L2 stamped, read before `accept` clears it.
    let stamped = captured[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F != 0;
    let payload_len = usize::from(u16::from_be_bytes([
        captured[IPV6_PAYLOAD_LENGTH_OFFSET],
        captured[IPV6_PAYLOAD_LENGTH_OFFSET + 1],
    ]));

    let mut copy = captured.to_vec();
    match rx::accept(&mut copy) {
        Ok(received) => (received.header, received.payload.to_vec(), true),
        Err(rx::RxError::BadChecksum) if stamped => {
            let bcmp = &captured[BCMP_HEADER_OFFSET..BCMP_HEADER_OFFSET + payload_len];
            (
                BcmpHeader::decode(bcmp).expect("13 bytes is a header"),
                bcmp[BCMP_HEADER_LEN..].to_vec(),
                false,
            )
        }
        Err(e) => {
            panic!("the port could not accept a frame bm_core transmitted on port {port}: {e}")
        }
    }
}

fn assert_same_frames(what: &str, input: &TimeInput, c: &[(u8, Vec<u8>)], rs: &[(u8, Vec<u8>)]) {
    assert_eq!(
        c.len(),
        rs.len(),
        "{what}: frame count differs -- C sent {} on ports {:?}, bm-stack sent {} on ports {:?} ({input:?})",
        c.len(),
        c.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        rs.len(),
        rs.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
    );

    for (index, ((c_port, c_frame), (rs_port, rs_frame))) in c.iter().zip(rs).enumerate() {
        assert_eq!(
            c_port, rs_port,
            "{what}: frame {index} went to different ports ({input:?})"
        );
        if c_frame != rs_frame {
            let at = c_frame
                .iter()
                .zip(rs_frame)
                .position(|(a, b)| a != b)
                .unwrap_or(c_frame.len().min(rs_frame.len()));
            panic!(
                "{what}: frame {index} (port {c_port}) diverged at byte {at}: \
                 C {:#04x?}, bm-stack {:#04x?}\n  input: {input:?}\n  \
                 C:        {c_frame:02x?}\n  bm-stack: {rs_frame:02x?}",
                c_frame.get(at),
                rs_frame.get(at),
            );
        }
    }
}

/// Assert a `bm-stack` node handles one system-time message exactly as
/// bm_core's stack does: the same frames, on the same ports, in the same order,
/// and the same clock left behind.
///
/// # Panics
///
/// If the decoders disagree with themselves on `decode_probe`, if the frame
/// sequences differ, or if the two clocks no longer read the same.
pub fn check(input: &TimeInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    // The decoders get their workout whether or not the C is involved.
    probe_decoders(&input.decode_probe);

    let frame = input.build();

    let _guard = oracle();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );

    // Both clocks to the same reading, so the only thing that can differ is
    // what each node does with the message.
    let rtc = set_both_clocks(input.clock());
    let before = oracle_clock_micros();
    assert_eq!(
        rtc.reading().expect("just set").to_utc_micros(),
        before,
        "the two clocks disagree before the message even arrives ({input:?})"
    );

    inject(input.ingress_port, &frame);
    let c = drain();

    let mut node = node_with_clock(rtc);
    let mut ours = frame.clone();
    let owed = node.on_frame(0, input.ingress_port, &mut ours);
    let forward = owed.forward;
    let mut rs = Vec::new();
    if let Some(relay) = owed.relay {
        rs.extend(capture(relay));
    }
    if let Some(reply) = owed.reply {
        rs.extend(capture(reply));
    }
    if let Some(reflood) = forward {
        rs.extend(capture_reflood(&mut node, reflood, &ours));
    }

    assert_same_frames("system time", &input, &c, &rs);

    // A `0x12` the node accepted moved both clocks; anything else left them.
    // Either way they have to still agree, which is what pins
    // `RtcTimeAndDate::from_utc_micros` against `date_time_from_utc` plus the
    // field copy at time.c:103.
    assert_eq!(
        node.rtc()
            .reading()
            .expect("a clock that was set stays set")
            .to_utc_micros(),
        oracle_clock_micros(),
        "the clocks parted company over this message ({input:?})"
    );

    // Whatever the oracle sent, the port must also be able to read it back --
    // and a response it *built* must re-encode to the C's bytes exactly.
    //
    // "Built" is not the same as "of type 0x11": a relayed or re-flooded copy
    // of the injected message has the same type, and a re-flood even carries
    // the oracle's address (divergence #23). What tells them apart is the body,
    // whose `source_node_id` the forwarding paths leave alone.
    let mut responses = 0;
    for (port, captured) in &c {
        let (header, payload, _verified) = read_back(*port, captured);
        if header.message_type != MessageType::SYSTEM_TIME_RESPONSE {
            continue;
        }
        let response = SystemTimeResponse::decode(&payload)
            .unwrap_or_else(|e| panic!("could not decode a 0x11 bm_core transmitted: {e}"));
        if response.header.source_node_id != stack::NODE_ID {
            continue; // a copy of the injected message travelling onward
        }
        responses += 1;
        assert_eq!(
            response.header.target_node_id, PEER_NODE_ID,
            "the oracle answers the body's source_node_id, not the frame's ({input:?})"
        );
        let expected = match input.message {
            // A set is echoed back at full microsecond precision even though
            // the RTC kept only the millisecond.
            TimeMessage::Set => input.utc_time_us,
            _ => before,
        };
        assert_eq!(
            response.utc_time_us, expected,
            "the response carries the wrong timestamp ({input:?})"
        );

        let mut again = [0u8; SystemTimeResponse::LEN];
        response.encode(&mut again).expect("re-encode");
        assert_eq!(
            &again[..],
            &payload[..SystemTimeResponse::LEN],
            "re-encoded bytes differ from bm_core's ({input:?})"
        );
    }

    // And the count, which is the divergence-#27 table said a third way: a
    // response always goes to `FF02::1`, so it is stamped once per port
    // whatever the request's own destination was.
    assert_eq!(
        responses,
        if input.expects_response() {
            usize::from(NUM_PORTS)
        } else {
            0
        },
        "wrong number of responses ({input:?})"
    );
}

/// Feed arbitrary bytes to every decoder in this module.
///
/// There is no C counterpart to compare against — the C's casts read past the
/// buffer, which is divergence #14 — so the property checked is the port's
/// own: a decoder either refuses the bytes or produces something that
/// re-encodes to a prefix of them, and it never panics.
fn probe_decoders(bytes: &[u8]) {
    if let Ok(header) = SystemTimeHeader::decode(bytes) {
        let mut again = [0u8; SystemTimeHeader::LEN];
        header.encode(&mut again).unwrap();
        assert_eq!(&again[..], &bytes[..SystemTimeHeader::LEN]);
    }
    if let Ok(request) = SystemTimeRequest::decode(bytes) {
        let mut again = [0u8; SystemTimeRequest::LEN];
        request.encode(&mut again).unwrap();
        assert_eq!(&again[..], &bytes[..SystemTimeRequest::LEN]);
    }
    if let Ok(response) = SystemTimeResponse::decode(bytes) {
        let mut again = [0u8; SystemTimeResponse::LEN];
        response.encode(&mut again).unwrap();
        assert_eq!(&again[..], &bytes[..SystemTimeResponse::LEN]);
    }
    if let Ok(set) = SystemTimeSet::decode(bytes) {
        let mut again = [0u8; SystemTimeSet::LEN];
        set.encode(&mut again).unwrap();
        assert_eq!(&again[..], &bytes[..SystemTimeSet::LEN]);
        // The two 24-byte bodies are the same bytes, which is the whole reason
        // the header's type field has to be trusted.
        let response = SystemTimeResponse::decode(bytes).expect("same length");
        assert_eq!(set.header, response.header);
        assert_eq!(set.utc_time_us, response.utc_time_us);
    }
}
