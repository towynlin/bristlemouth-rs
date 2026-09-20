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
//! this node, carrying the received BCMP header and body unchanged. Its C
//! callers are `bcmp/time.c`, `bcmp/config.c` and `bcmp/dfu_core.c`, none of
//! which is ported yet — so [`check_ll_forward`] calls `bcmp_ll_forward`
//! directly rather than provoking it, and compares against
//! [`bm_stack::Node::forward_link_local`] once per port.
//!
//! Calling it directly is the stronger comparison anyway: it pins the frame for
//! every message type and body length rather than only for the three exchanges
//! that happen to use it, and it does not depend on a caller that does not
//! exist yet.
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
use crate::stack::{self, NUM_PORTS, capture, drain, inject, node, oracle, pump_until_quiet};

/// Node id the injected frame appears to come from. Anything but the oracle's.
pub const PEER_NODE_ID: u64 = 0x0000_0000_55AA_0011;

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
    /// A system-time request. `time.c` registers it and would forward it, but
    /// the body here is arbitrary rather than a well-formed
    /// `BcmpSystemTimeHeader`, so this is only ever injected with a body long
    /// enough for the C to read one.
    SystemTime,
}

impl Message {
    fn message_type(self) -> MessageType {
        match self {
            // 0xFFFE is not one of bm_core's 45 constants, and `packet_add` is
            // only ever called with those.
            Self::Unregistered => MessageType(0xFFFE),
            Self::DeviceInfoForUs | Self::DeviceInfoForAnother => MessageType::DEVICE_INFO_REQUEST,
            Self::NeighborTableForUs => MessageType::NEIGHBOR_TABLE_REQUEST,
            Self::SystemTime => MessageType::SYSTEM_TIME_REQUEST,
        }
    }

    /// The prefix the C's processor reads out of the body, if it reads one.
    ///
    /// Both request types start with a target node id, and `time.c` reads a
    /// `BcmpSystemTimeHeader` whose first field is one too. A body shorter than
    /// that is an out-of-bounds read in the C, so the body always carries it.
    fn body_prefix(self) -> Option<[u8; 8]> {
        match self {
            Self::Unregistered => None,
            Self::DeviceInfoForUs | Self::NeighborTableForUs => Some(stack::NODE_ID.to_le_bytes()),
            Self::DeviceInfoForAnother => Some(PEER_NODE_ID.to_le_bytes()),
            // Addressed to us, so the C handles it rather than forwarding it --
            // the forward path gets its own comparison, which does not need a
            // caller.
            Self::SystemTime => Some(stack::NODE_ID.to_le_bytes()),
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
        let message = match u.arbitrary::<u8>()? % 5 {
            0 => Message::Unregistered,
            1 => Message::DeviceInfoForUs,
            2 => Message::DeviceInfoForAnother,
            3 => Message::NeighborTableForUs,
            _ => Message::SystemTime,
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
    let mut rs = Vec::new();
    if let Some(relay) = owed.relay {
        rs.extend(capture(relay));
    }
    if let Some(reply) = owed.reply {
        rs.extend(capture(reply));
    }

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
