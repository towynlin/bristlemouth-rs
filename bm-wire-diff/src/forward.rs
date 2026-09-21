//! Differential comparator for forwarding: does a `bm-stack` node put the same
//! bytes on the same other ports as bm_core does?
//!
//! Two paths carry a frame from one port to another, and this compares both.
//!
//! # The L2 relay
//!
//! `bm_l2_process_rx_evt` applies the routing policy, and if it asks for egress
//! copies the received frame, clears both port nibbles and hands the copy to
//! `bm_l2_tx`. That is [`check_relay`]: inject a frame into the oracle on one
//! port, drain the capture ring, then run the same frame through
//! [`bm_stack::Node::on_frame`] and [`bm_stack::deliver`], and compare the
//! `(port, bytes)` sequences. It covers the relay *and* whatever the node
//! replies, in transmit order — so it also checks that the relay comes first.
//!
//! # `bcmp_ll_forward`
//!
//! The other path is a message-level re-flood: a fresh frame per port, from
//! this node, carrying the received BCMP header and body unchanged. Of its
//! three C callers — `bcmp/time.c`, `bcmp/config.c` and `bcmp/dfu_core.c` —
//! only the first is ported, so [`check_ll_forward`] calls `bcmp_ll_forward`
//! directly rather than provoking it, and compares against
//! [`bm_stack::Node::forward_link_local`] once per port.
//!
//! Calling it directly is the stronger comparison anyway: it pins the frame for
//! every message type and body length rather than only for the three exchanges
//! that happen to use it, and it does not depend on a caller that does not
//! exist yet.
//!
//! Since card M2 there *is* one caller, and [`Message::SystemTimeForAnother`]
//! is it: [`check_relay`] then compares the whole receive path, the decision to
//! forward included, rather than the forward alone.
//!
//! # Input domain
//!
//! Heartbeats are excluded. bm_core's neighbour table is file-scope state with
//! no deinit, and this module must not reset the shim, so a heartbeat injected
//! by one seed would change the neighbour-table reply every later seed gets —
//! against a freshly constructed Rust node that never saw it. Every other
//! message type is fair game, including types nothing registers: the relay
//! decision is L2's and does not depend on the payload at all.
//!
//! **Nothing here may set the oracle's RTC.** It is process-global and has no
//! un-set, and [`stack::node`] hands the Rust side a clock that has never been
//! set, so the two agree only while the oracle's has not been either — which is
//! what makes a system-time request addressed to this node produce silence on
//! both sides. Setting a clock is [`crate::time`]'s business, and it lives in
//! its own binary. That is also why a `0x12` is not in the input domain here:
//! it would move the oracle's clock and leave it moved.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`].

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, MessageType, forward, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::stack::{
    self, NUM_PORTS, capture, capture_reflood, drain, inject, node, oracle, pump_until_quiet,
};

/// Node id the injected frame appears to come from. Anything but the oracle's.
pub const PEER_NODE_ID: u64 = 0x0000_0000_55AA_0011;

/// A third node, so a system-time message can be addressed to somebody who is
/// neither end of the link and so gets forwarded.
pub const THIRD_NODE_ID: u64 = 0x0000_0000_0BAD_F00D;

/// Largest body the comparator will build.
///
/// Well inside the capture ring's per-frame allocation and inside bm_core's own
/// `bcmp_max_payload_size_bytes`, which is what `bcmp_tx` checks against.
pub const MAX_BODY: usize = 256;

/// Which message the injected frame carries.
///
/// The type decides whether the oracle answers, which is what makes the relay
/// comparison also a transmit-order comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// A type nothing in bm_core registers, so nothing answers it. The relay is
    /// then the only thing on the wire.
    Unregistered,
    /// A device-info request for this node, which it answers.
    DeviceInfoForUs,
    /// A device-info request for somebody else, which it ignores.
    DeviceInfoForAnother,
    /// A neighbour-table request for this node, which it answers.
    NeighborTableForUs,
    /// A system-time request addressed to this node, which `time.c` answers
    /// from the RTC — and, since nothing here sets the RTC, does not answer at
    /// all. The relay is then the only thing on the wire, exactly as for an
    /// unregistered type, but by a different route.
    SystemTime,
    /// A system-time request addressed to a third node, which `time.c` hands to
    /// `bcmp_ll_forward`. The one input here that provokes a real re-flood
    /// rather than calling for one.
    SystemTimeForAnother,
}

impl Message {
    fn message_type(self) -> MessageType {
        match self {
            // 0xFFFE is not one of bm_core's 45 constants, and `packet_add` is
            // only ever called with those.
            Self::Unregistered => MessageType(0xFFFE),
            Self::DeviceInfoForUs | Self::DeviceInfoForAnother => MessageType::DEVICE_INFO_REQUEST,
            Self::NeighborTableForUs => MessageType::NEIGHBOR_TABLE_REQUEST,
            Self::SystemTime | Self::SystemTimeForAnother => MessageType::SYSTEM_TIME_REQUEST,
        }
    }

    /// Whether the C hands this message to `bcmp_ll_forward`.
    fn is_forwarded(self) -> bool {
        matches!(self, Self::SystemTimeForAnother)
    }

    /// The prefix the C's processor reads out of the body, if it reads one.
    ///
    /// Both request types start with a target node id. `time.c` reads a whole
    /// 16-byte `BcmpSystemTimeHeader`, and `bcmp_time_process_time_request_msg`
    /// then reads its `source_node_id`, so a system-time body is never shorter
    /// than that. Anything shorter is an out-of-bounds read in the C, with
    /// nothing to compare against -- divergence #14's shape.
    fn body_prefix(self) -> Option<Vec<u8>> {
        let target = |id: u64| Some(id.to_le_bytes().to_vec());
        let time_header = |target: u64| {
            let mut prefix = target.to_le_bytes().to_vec();
            prefix.extend_from_slice(&PEER_NODE_ID.to_le_bytes());
            Some(prefix)
        };
        match self {
            Self::Unregistered => None,
            Self::DeviceInfoForUs | Self::NeighborTableForUs => target(stack::NODE_ID),
            Self::DeviceInfoForAnother => target(PEER_NODE_ID),
            Self::SystemTime => time_header(stack::NODE_ID),
            Self::SystemTimeForAnother => time_header(THIRD_NODE_ID),
        }
    }
}

/// Where the injected frame is addressed, which is what the routing policy
/// decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// `FF03::1`. Flooded to every port but the ingress one.
    GlobalMulticast,
    /// `FF02::1`, the neighbour address. Consumed, never relayed.
    LinkLocalNeighbor,
    /// `FF02::5`. The routing callback's business — and nothing in bm_core
    /// registers one, so it is submitted locally and relayed nowhere.
    LinkLocalOther,
}

impl Destination {
    fn addr(self) -> BmIpAddr {
        match self {
            Self::GlobalMulticast => BmIpAddr::GLOBAL_MULTICAST,
            Self::LinkLocalNeighbor => BmIpAddr::LINK_LOCAL_MULTICAST,
            Self::LinkLocalOther => {
                let mut addr = BmIpAddr::LINK_LOCAL_MULTICAST;
                addr.0[15] = 0x05;
                addr
            }
        }
    }
}

/// One frame delivered to both stacks.
#[derive(Debug, Clone)]
pub struct ForwardInput {
    /// Which message it carries.
    pub message: Message,
    /// Where it is addressed.
    pub destination: Destination,
    /// Port it arrives on, clamped to 1..=[`NUM_PORTS`] by [`Domain`].
    pub ingress_port: u8,
    /// Egress-port nibble the previous hop's L2 stamped into the source
    /// address. The checksum is computed with it in place, so any value is
    /// legal on the wire; it is cleared by both the ingress stamp and the
    /// forwarded copy.
    pub sender_egress_nibble: u8,
    /// The two source-address bytes `clear_ports_legacy` zeroes — frame bytes 26
    /// and 27. Non-zero means the frame fails its local checksum (divergence
    /// #9) while still being relayed, which is the case worth pinning: the
    /// forwarded copy is taken before the receive path clears them.
    pub legacy_ports: [u8; 2],
    /// Message body, capped at [`MAX_BODY`] by [`Domain`].
    pub body: Vec<u8>,
}

impl ForwardInput {
    /// Whether bm_core's BCMP layer ever sees this message.
    ///
    /// `process_received_message` clears the legacy port bytes and *then*
    /// verifies the checksum, which was computed with them in place — so a
    /// frame carrying them is normally rejected before its type is looked at,
    /// and `time.c` never runs. That is divergence #9. L2 relays the frame
    /// anyway, because the relay is a different layer's decision, which is what
    /// `a_frame_that_fails_its_checksum_is_still_relayed_with_its_legacy_bytes`
    /// pins.
    ///
    /// **Normally, but not always.** Frame bytes 26 and 27 are one aligned
    /// 16-bit word of the checksum's one's-complement sum. Removing a term of
    /// `0x0000` changes nothing — and neither does removing `0xFFFF`, which is
    /// one's-complement *negative zero*. So legacy bytes of `FF FF` survive the
    /// clear with the checksum still valid and the message reaches `time.c`
    /// after all. `cargo fuzz run forward` found that within seven minutes of
    /// this predicate being written as `== [0, 0]`;
    /// `bm-wire/fuzz/seeds/forward/system-time-for-another-legacy-ones` keeps
    /// the input.
    ///
    /// The egress nibble is not in the same position: `clear_ingress_port`
    /// touches only the high nibble, so the low one survives into the checksum
    /// exactly as the sender computed it.
    #[must_use]
    pub fn reaches_bcmp(&self) -> bool {
        matches!(u16::from_be_bytes(self.legacy_ports), 0x0000 | 0xFFFF)
    }
}

impl<'a> Arbitrary<'a> for ForwardInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(u)?;
        let len = u.arbitrary_len::<u8>()?;
        input.body = u.bytes(len)?.to_vec();
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        let mut input = Self::scalars(&mut u)?;
        input.body = u.take_rest().to_vec();
        input.clamp_to_domain();
        Ok(input)
    }
}

impl Domain for ForwardInput {
    fn clamp_to_domain(&mut self) {
        // Map any byte into 1..=NUM_PORTS without moving values already in range.
        self.ingress_port = self.ingress_port.wrapping_sub(1) % NUM_PORTS + 1;
        self.sender_egress_nibble &= 0x0F;
        self.body.truncate(MAX_BODY);
        // The C's processors read a target node id straight out of the body
        // without checking its length, so anything shorter is an out-of-bounds
        // read with nothing to compare against -- divergence #14's shape.
        if let Some(prefix) = self.message.body_prefix() {
            if self.body.len() < prefix.len() {
                self.body.resize(prefix.len(), 0);
            }
            self.body[..prefix.len()].copy_from_slice(&prefix);
        }
    }
}

impl ForwardInput {
    fn scalars(u: &mut Unstructured<'_>) -> Result<Self> {
        // Six now, not five. Every committed seed picks its message with a
        // byte below 5, so `% 6` leaves all of them meaning what they did.
        let message = match u.arbitrary::<u8>()? % 6 {
            0 => Message::Unregistered,
            1 => Message::DeviceInfoForUs,
            2 => Message::DeviceInfoForAnother,
            3 => Message::NeighborTableForUs,
            4 => Message::SystemTime,
            _ => Message::SystemTimeForAnother,
        };
        let destination = match u.arbitrary::<u8>()? % 3 {
            0 => Destination::GlobalMulticast,
            1 => Destination::LinkLocalNeighbor,
            _ => Destination::LinkLocalOther,
        };
        Ok(Self {
            message,
            destination,
            ingress_port: u.arbitrary()?,
            sender_egress_nibble: u.arbitrary()?,
            legacy_ports: [u.arbitrary()?, u.arbitrary()?],
            body: Vec::new(),
        })
    }

    /// The frame both stacks are given, exactly as the wire would deliver it.
    fn build(&self) -> Vec<u8> {
        let payload_len = BCMP_HEADER_LEN + self.body.len();
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;

        let mut src = bm_wire::addr::nodeid_to_ip(0xFE80_0000, PEER_NODE_ID);
        src.0[2] = self.sender_egress_nibble;
        src.0[4] = self.legacy_ports[0];
        src.0[5] = self.legacy_ports[1];
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16].copy_from_slice(&src.0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&self.destination.addr().0);

        // The checksum the previous hop's L2 put on the wire, over the address
        // as it stands -- egress nibble, legacy bytes and all.
        tx::serialize(&mut frame, self.message.message_type(), 0, &self.body)
            .expect("frame is sized for the body");
        frame
    }

    /// The BCMP region of the frame: header first, then body.
    ///
    /// This is what `bcmp_ll_forward`'s `header` and `payload` point at, and
    /// what [`bm_stack::Node::forward_link_local`] takes.
    fn bcmp(&self) -> Vec<u8> {
        let frame = self.build();
        frame[BCMP_HEADER_OFFSET..].to_vec()
    }
}

/// The frame an input delivers, for tests that want to look at it first.
#[must_use]
pub fn build_frame(input: &ForwardInput) -> Vec<u8> {
    let mut input = input.clone();
    input.clamp_to_domain();
    input.build()
}

fn assert_same_frames(what: &str, input: &ForwardInput, c: &[(u8, Vec<u8>)], rs: &[(u8, Vec<u8>)]) {
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

/// Assert a `bm-stack` node relays and answers a received frame exactly as
/// bm_core's stack does, on the same ports and in the same order.
///
/// # Panics
///
/// If the number of frames, the egress ports, the order, or any frame's bytes
/// differ.
pub fn check_relay(input: &ForwardInput) {
    let mut input = input.clone();
    input.clamp_to_domain();
    let frame = input.build();

    let _guard = oracle();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );
    inject(input.ingress_port, &frame);
    let c = drain();

    let mut node = node();
    let mut ours = frame.clone();
    let owed = node.on_frame(0, input.ingress_port, &mut ours);
    // Read out before `owed` is consumed: a re-flood needs the frame back.
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

    assert_eq!(
        forward.is_some(),
        input.message.is_forwarded() && input.reaches_bcmp(),
        "the port disagreed with the C about whether this message is forwarded ({input:?})"
    );
    assert_same_frames("relay", &input, &c, &rs);
}

/// Assert a `bm-stack` node re-floods a link-local message exactly as
/// `bcmp_ll_forward` does.
///
/// The C is called directly, with a buffer holding the received BCMP header and
/// body — which is what its callers hand it, since `data.header` and
/// `data.payload` point into the received frame.
///
/// # Panics
///
/// If the number of frames, the egress ports, or any frame's bytes differ.
pub fn check_ll_forward(input: &ForwardInput) {
    let mut input = input.clone();
    input.clamp_to_domain();
    let bcmp = input.bcmp();

    let _guard = oracle();
    assert!(
        drain().is_empty(),
        "the ring was not drained before this run"
    );

    // bcmp_ll_forward writes the new checksum back through `header`, so the C
    // gets a scratch copy and the port keeps the pristine one.
    let mut scratch = bcmp.clone();
    let err = unsafe {
        let header = scratch.as_mut_ptr().cast::<bm_wire_sys::BcmpHeader>();
        let payload = scratch[BCMP_HEADER_LEN..].as_mut_ptr().cast();
        bm_wire_sys::bcmp_ll_forward(
            header,
            payload,
            (bcmp.len() - BCMP_HEADER_LEN) as u32,
            input.ingress_port,
        )
    };
    pump_until_quiet();
    let c = drain();

    // The C reports BmEINVAL when it found no port to forward to, having sent
    // nothing -- and NUM_PORTS is 2, so that never happens here. Assert it
    // rather than assume it.
    assert!(
        !forward::ll_forward_is_a_no_op(NUM_PORTS, input.ingress_port),
        "the comparator only injects on ports the device has"
    );
    assert_eq!(
        err,
        bm_wire_sys::BmErr_BmOK,
        "bcmp_ll_forward failed ({input:?})"
    );

    // One frame per port that is not the ingress port, so the comparison below
    // is never vacuously satisfied by both sides emitting nothing.
    assert_eq!(
        c.len(),
        usize::from(NUM_PORTS) - 1,
        "bcmp_ll_forward should have emitted one frame per other port ({input:?})"
    );

    let mut node = node();
    let mut rs = Vec::new();
    for port in forward::egress_ports(NUM_PORTS, input.ingress_port) {
        let outbound = node
            .forward_link_local(port, &bcmp)
            .expect("a forward the C managed must fit our transmit buffer");
        rs.extend(capture(outbound));
    }

    assert_same_frames("bcmp_ll_forward", &input, &c, &rs);
}

/// Both halves, which is what the fuzz target and the seed replay run.
///
/// # Panics
///
/// If either half diverges.
pub fn check(input: &ForwardInput) {
    check_relay(input);
    check_ll_forward(input);
}
