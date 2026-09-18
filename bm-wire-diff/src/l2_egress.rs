//! Differential comparator for [`bm_wire::l2`] against `network/l2.c`, driven
//! through the shim's TX capture ring.
//!
//! # Why this one needs its own process
//!
//! `network_add_egress_port` and `network_revert_checksum` are `static inline`
//! inside `l2.c`, so unlike the comparators that call a bm_core function
//! directly, there is nothing here to link against. The only way to observe
//! them is from outside: hand a frame to `bm_l2_link_output`, run the L2 task,
//! and read back what reached the network device. That is the wire boundary,
//! which makes this the strongest comparison in the harness — it checks the
//! stamp, the checksum patch, the per-port ordering and the buffer restore all
//! at once, against bytes, not against an intermediate value.
//!
//! It also means bringing the whole stack up, and that is a one-way door:
//! `bm_shim_stack_init` calls `bm_ip_init`, which calls `packet_init` with
//! `bm_linux.c`'s accessors. [`crate::bcmp`] calls `packet_init` with its own.
//! Whichever runs second wins, and the other comparator then reads frames
//! through the wrong accessors. **The two must never share a process.**
//! This module is therefore driven from `tests/l2_egress.rs`, which cargo runs
//! as its own binary, and its seeds are listed in
//! [`crate::replay::STACK_TARGETS`] rather than in the in-process list.
//!
//! `bm_shim_reset` must not be called here either, for the reason the crate
//! docs give: the stack's statics outlive it.

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IP_PROTO_UDP,
    IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES, UDP_CHECKSUM_OFFSET, UDP_HEADER_LEN,
    UDP_LENGTH_OFFSET,
};
use bm_wire::l2::{self, REQUESTED_EGRESS_PORT_OFFSET, TxKind};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::stack::{NUM_PORTS, drain, oracle, pump};

/// Largest body the comparator will build. Keeps frames well inside the
/// capture ring's per-frame allocation and inside bm_core's own MTU budget.
pub const MAX_BODY: usize = 256;

/// Which upper-layer protocol the frame carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upper {
    /// BCMP directly on IPv6. The checksum patch is 16-bit here.
    Bcmp,
    /// UDP. The checksum patch is 8-bit here — divergence #12.
    Udp,
}

/// Where the frame is addressed, which is what decides whether it is stamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// `FF03::1`. Not stamped.
    GlobalMulticast,
    /// `FF02::1`. Stamped per port.
    LinkLocalNeighbor,
    /// `FF02::5`, a link-local multicast that is not the neighbour address.
    /// Also stamped: L2 looks only at the multicast class.
    LinkLocalOther,
    /// A unicast address. bm_core's L2 drops it unsent.
    Unicast,
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
            Self::Unicast => bm_wire::addr::nodeid_to_ip(0xFD00_0000, 0x1234),
        }
    }
}

/// One frame handed to L2 for transmission.
#[derive(Debug, Clone)]
pub struct L2EgressInput {
    /// Upper-layer protocol.
    pub upper: Upper,
    /// Destination class.
    pub destination: Destination,
    /// Egress port an application requests in destination byte 13. Values
    /// outside 1..=[`NUM_PORTS`] mean "every port".
    pub requested_port: u8,
    /// Source node id, so the source address varies.
    pub source_node_id: u64,
    /// UDP source and destination ports, unused for BCMP.
    pub udp_ports: (u16, u16),
    /// Message body, capped at [`MAX_BODY`] by [`Domain`].
    pub body: Vec<u8>,
}

impl<'a> Arbitrary<'a> for L2EgressInput {
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

impl Domain for L2EgressInput {
    fn clamp_to_domain(&mut self) {
        self.body.truncate(MAX_BODY);
    }
}

impl L2EgressInput {
    fn scalars(u: &mut Unstructured<'_>) -> Result<Self> {
        let upper = if u.arbitrary::<bool>()? {
            Upper::Bcmp
        } else {
            Upper::Udp
        };
        let destination = match u.arbitrary::<u8>()? % 4 {
            0 => Destination::GlobalMulticast,
            1 => Destination::LinkLocalNeighbor,
            2 => Destination::LinkLocalOther,
            _ => Destination::Unicast,
        };
        Ok(Self {
            upper,
            destination,
            requested_port: u.arbitrary()?,
            source_node_id: u.arbitrary()?,
            udp_ports: (u.arbitrary()?, u.arbitrary()?),
            body: Vec::new(),
        })
    }

    fn payload_len(&self) -> usize {
        match self.upper {
            Upper::Bcmp => BCMP_HEADER_LEN + self.body.len(),
            Upper::Udp => UDP_HEADER_LEN + self.body.len(),
        }
    }

    /// A complete frame, checksummed, with the requested egress port in place —
    /// exactly what an upper layer hands `bm_l2_link_output`.
    fn build(&self) -> Vec<u8> {
        let payload_len = self.payload_len();
        let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());

        let src = bm_wire::addr::nodeid_to_ip(0xFE80_0000, self.source_node_id);
        let dst = self.destination.addr();
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16].copy_from_slice(&src.0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&dst.0);

        match self.upper {
            Upper::Bcmp => {
                frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
                tx::serialize(&mut frame, MessageType::HEARTBEAT, 0, &self.body)
                    .expect("frame is sized for the body");
            }
            Upper::Udp => {
                frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_UDP;
                let udp = MIN_FRAME_WITH_ADDRESSES;
                frame[udp..udp + 2].copy_from_slice(&self.udp_ports.0.to_be_bytes());
                frame[udp + 2..udp + 4].copy_from_slice(&self.udp_ports.1.to_be_bytes());
                frame[UDP_LENGTH_OFFSET..UDP_LENGTH_OFFSET + 2]
                    .copy_from_slice(&(payload_len as u16).to_be_bytes());
                frame[MIN_FRAME_WITH_ADDRESSES + UDP_HEADER_LEN..].copy_from_slice(&self.body);
                let checksum = bm_wire::checksum::ipv6_pseudo_checksum(
                    &src,
                    &dst,
                    IP_PROTO_UDP,
                    &frame[MIN_FRAME_WITH_ADDRESSES..],
                );
                frame[UDP_CHECKSUM_OFFSET..UDP_CHECKSUM_OFFSET + 2]
                    .copy_from_slice(&checksum.to_le_bytes());
            }
        }

        // The application's egress-port request, which bm_l2_link_output reads
        // and clears.
        frame[REQUESTED_EGRESS_PORT_OFFSET] = self.requested_port;
        frame
    }
}

/// The frame an input builds, for tests that need to inspect it before it is
/// transmitted.
#[must_use]
pub fn build_frame(input: &L2EgressInput) -> Vec<u8> {
    let mut input = input.clone();
    input.clamp_to_domain();
    input.build()
}

/// What the C emitted for this input.
fn oracle_transmit(frame: &[u8]) -> Vec<(u8, Vec<u8>)> {
    unsafe {
        assert!(
            drain().is_empty(),
            "the ring was not drained before this run"
        );
        let buf = bm_wire_sys::bm_l2_new(frame.len() as u32);
        assert!(!buf.is_null(), "bm_l2_new");
        let payload = bm_wire_sys::bm_l2_get_payload(buf).cast::<u8>();
        std::ptr::copy_nonoverlapping(frame.as_ptr(), payload, frame.len());

        let err = bm_wire_sys::bm_l2_link_output(buf, frame.len() as u32);
        assert_eq!(err, bm_wire_sys::BmErr_BmOK, "bm_l2_link_output");
        pump();
        assert_eq!(
            bm_wire_sys::bm_shim_tx_dropped(),
            0,
            "capture ring overflowed"
        );

        let captured = drain();
        // bm_l2_link_output takes a reference; the L2 task drops it again.
        // The one bm_l2_new gave us is still ours.
        bm_wire_sys::bm_l2_free(buf);
        captured
    }
}

/// What `bm_wire::l2` says should be emitted, composed the way firmware would.
///
/// This composition is the part `bm-stack` will eventually own. Spelling it out
/// here is the point: the comparator proves the primitives compose to the
/// bytes bm_core puts on the wire.
fn port_transmit(frame: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut frame = frame.to_vec();
    let all_ports = (1u16 << NUM_PORTS) - 1;
    let mask = l2::take_requested_egress_port(&mut frame, NUM_PORTS)
        .expect("the comparator only builds full frames");

    let mut sent = Vec::new();
    match l2::tx_kind(&frame) {
        // Every port at once: one frame, unstamped, on the device's "all
        // ports" encoding.
        TxKind::GlobalMulticast if mask == all_ports => sent.push((0u8, frame.clone())),
        // A subset: one unstamped frame per port.
        TxKind::GlobalMulticast => {
            for port in 1..=NUM_PORTS {
                if mask & (1 << (port - 1)) != 0 {
                    sent.push((port, frame.clone()));
                }
            }
        }
        TxKind::LinkLocalMulticast => {
            for port in 1..=NUM_PORTS {
                if mask & (1 << (port - 1)) != 0 {
                    l2::with_egress_port(&mut frame, port, |out| sent.push((port, out.to_vec())))
                        .expect("the comparator only builds full frames");
                }
            }
        }
        TxKind::Dropped => {}
    }
    sent
}

/// Assert `bm_wire::l2` puts the same bytes on the same ports as bm_core's L2.
///
/// # Panics
///
/// If the number of frames, the egress ports, or any frame's bytes differ.
pub fn check(input: &L2EgressInput) {
    let mut input = input.clone();
    input.clamp_to_domain();
    let frame = input.build();

    let _guard = oracle();
    let c = oracle_transmit(&frame);
    let rs = port_transmit(&frame);

    assert_eq!(
        c.len(),
        rs.len(),
        "frame count differs: C sent {} on ports {:?}, Rust sent {} on ports {:?} ({input:?})",
        c.len(),
        c.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        rs.len(),
        rs.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
    );

    for (index, ((c_port, c_frame), (rs_port, rs_frame))) in c.iter().zip(&rs).enumerate() {
        assert_eq!(
            c_port, rs_port,
            "frame {index} went to different ports ({input:?})"
        );
        if c_frame != rs_frame {
            let at = c_frame
                .iter()
                .zip(rs_frame)
                .position(|(a, b)| a != b)
                .unwrap_or(c_frame.len().min(rs_frame.len()));
            panic!(
                "frame {index} (port {c_port}) diverged at byte {at}: C {:#04x?}, Rust {:#04x?}\n  input: {input:?}\n  C:    {c_frame:02x?}\n  Rust: {rs_frame:02x?}",
                c_frame.get(at),
                rs_frame.get(at),
            );
        }
    }
}
