//! Validating a received BCMP frame, ported from `bm_l2_submit`'s BCMP arm in
//! `network/bm_linux.c` and from `process_received_message` in `bcmp/packet.c`.
//!
//! # The frame is mutated before it is checked
//!
//! `process_received_message` rewrites three bytes of the source address
//! *before* verifying the checksum, and the checksum is then computed over the
//! rewritten address. A receiver that skips the rewrite computes a different
//! checksum and rejects perfectly good traffic from a C node, so [`accept`]
//! reproduces it exactly. See divergence #9 in `docs/c-divergences.md`.

use crate::bcmp::header::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, CHECKSUM_FIELD_OFFSET};
use crate::checksum::ipv6_pseudo_checksum;
use crate::frame::{
    ETHERNET_TYPE_IPV6, IP_PROTO_BCMP, IPV6_ADDRESS_SIZE, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES, ethernet_type,
};
use crate::util::BmIpAddr;

/// Byte of the source address the legacy port clear zeroes.
///
/// `clear_ports_legacy` in `bcmp/packet.c` is
/// `((uint32_t *)src)[1] &= ~0xFFFFU`, a little-endian 32-bit read at
/// `src + 4`. Clearing its low 16 bits clears source-address bytes 4 and 5,
/// which is frame bytes 26 and 27.
const LEGACY_PORT_CLEAR_OFFSET: usize = IPV6_SOURCE_ADDRESS_OFFSET + 4;

/// Why a frame was not accepted as BCMP.
///
/// bm_core signals all of these as `BmEBADMSG` from either `bm_l2_submit` or
/// `process_received_message`; they are split apart here because a firmware
/// caller wants to count them separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RxError {
    /// Shorter than the headers it claims to carry.
    Truncated,
    /// The EtherType is not IPv6.
    NotIpv6,
    /// The IPv6 next header is not [`IP_PROTO_BCMP`].
    NotBcmp,
    /// The checksum in the header does not match the computed one.
    ///
    /// The frame's checksum field is left **zeroed** in this case, matching the
    /// C, which returns before restoring it.
    BadChecksum,
}

impl core::fmt::Display for RxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Truncated => "frame too short",
            Self::NotIpv6 => "not IPv6",
            Self::NotBcmp => "not BCMP",
            Self::BadChecksum => "checksum mismatch",
        })
    }
}

/// A validated BCMP message, borrowed from the frame it arrived in.
///
/// Mirrors the C `BcmpProcessData`, except that `src` and `dst` are copies
/// rather than pointers into the frame — the C hands out aliasing mutable
/// pointers, which the port has no reason to reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received<'a> {
    /// The decoded header.
    pub header: BcmpHeader,
    /// The message body, after the header, of exactly the length the IPv6
    /// payload-length field accounts for. Trailing bytes in the frame are not
    /// included, matching the C.
    pub payload: &'a [u8],
    /// Source address, **after** the port bytes have been cleared.
    pub src: BmIpAddr,
    /// Destination address.
    pub dst: BmIpAddr,
    /// Ingress port, read out of the source address before it was cleared.
    ///
    /// 1-15 in practice; 0 means the sender encoded no port.
    pub ingress_port: u8,
}

/// Validate a received frame as BCMP, mutating it as bm_core does.
///
/// On success the frame's source address has had its ingress nibble and its
/// legacy port bytes cleared, and the checksum field holds the value it
/// arrived with. On [`RxError::BadChecksum`] the same clears have happened but
/// the checksum field is left zeroed — again matching the C.
///
/// # Errors
///
/// See [`RxError`].
pub fn accept(frame: &mut [u8]) -> Result<Received<'_>, RxError> {
    // --- bm_l2_submit's checks, in its order ---
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return Err(RxError::Truncated);
    }
    if ethernet_type(frame) != Some(ETHERNET_TYPE_IPV6) {
        return Err(RxError::NotIpv6);
    }

    let payload_len = usize::from(u16::from_be_bytes([
        frame[IPV6_PAYLOAD_LENGTH_OFFSET],
        frame[IPV6_PAYLOAD_LENGTH_OFFSET + 1],
    ]));
    // The C rejects a payload length that would read past the buffer, but
    // tolerates trailing bytes beyond it.
    if payload_len + MIN_FRAME_WITH_ADDRESSES > frame.len() {
        return Err(RxError::Truncated);
    }
    if frame[IPV6_NEXT_HEADER_OFFSET] != IP_PROTO_BCMP {
        return Err(RxError::NotBcmp);
    }
    if payload_len < BCMP_HEADER_LEN {
        return Err(RxError::Truncated);
    }

    // --- process_received_message, in its order ---

    // The ingress port is read first: the clears below destroy it.
    let ingress_port = (frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] >> 4) & 0x0F;
    frame[LEGACY_PORT_CLEAR_OFFSET] = 0;
    frame[LEGACY_PORT_CLEAR_OFFSET + 1] = 0;
    frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] &= 0x0F;

    let checksum_offset = BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET;
    let checksum_read = u16::from_le_bytes([frame[checksum_offset], frame[checksum_offset + 1]]);
    frame[checksum_offset] = 0;
    frame[checksum_offset + 1] = 0;

    let src = read_addr(frame, IPV6_SOURCE_ADDRESS_OFFSET);
    let dst = read_addr(frame, IPV6_DESTINATION_ADDRESS_OFFSET);
    let bcmp = &frame[BCMP_HEADER_OFFSET..BCMP_HEADER_OFFSET + payload_len];
    let checksum_calc = ipv6_pseudo_checksum(&src, &dst, IP_PROTO_BCMP, bcmp);

    if checksum_calc != checksum_read {
        // Deliberately not restored: the C returns here, leaving the field zero.
        return Err(RxError::BadChecksum);
    }
    frame[checksum_offset..checksum_offset + 2].copy_from_slice(&checksum_read.to_le_bytes());

    let bcmp = &frame[BCMP_HEADER_OFFSET..BCMP_HEADER_OFFSET + payload_len];
    Ok(Received {
        header: BcmpHeader::decode(bcmp).expect("payload_len >= BCMP_HEADER_LEN, checked above"),
        payload: &bcmp[BCMP_HEADER_LEN..],
        src,
        dst,
        ingress_port,
    })
}

fn read_addr(frame: &[u8], offset: usize) -> BmIpAddr {
    let mut addr = [0u8; IPV6_ADDRESS_SIZE];
    addr.copy_from_slice(&frame[offset..offset + IPV6_ADDRESS_SIZE]);
    BmIpAddr(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bcmp::header::MessageType;
    use crate::bcmp::heartbeat::Heartbeat;
    use crate::bcmp::tx;

    /// A frame carrying `body` as BCMP, with a valid checksum.
    fn bcmp_frame(src: BmIpAddr, dst: BmIpAddr, ty: MessageType, body: &[u8]) -> TestFrame {
        let payload_len = BCMP_HEADER_LEN + body.len();
        let mut frame = [0u8; 256];
        frame[12] = 0x86;
        frame[13] = 0xDD;
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16].copy_from_slice(&src.0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&dst.0);
        let end = MIN_FRAME_WITH_ADDRESSES + payload_len;
        tx::serialize(&mut frame[..end], ty, 0, body).unwrap();
        TestFrame {
            buf: frame,
            len: end,
        }
    }

    /// A fixed-capacity frame buffer, so these tests need no allocator.
    struct TestFrame {
        buf: [u8; 256],
        len: usize,
    }

    impl TestFrame {
        fn as_mut(&mut self) -> &mut [u8] {
            &mut self.buf[..self.len]
        }
    }

    fn node_src(id: u64) -> BmIpAddr {
        crate::addr::nodeid_to_ip(0xFE80_0000, id)
    }

    #[test]
    fn a_well_formed_heartbeat_is_accepted() {
        let hb = Heartbeat {
            time_since_boot_us: 1_234_567,
            liveliness_lease_dur_s: 10,
        };
        let mut body = [0u8; Heartbeat::LEN];
        hb.encode(&mut body).unwrap();

        let src = node_src(0x0000_0000_55AA_0011);
        let mut frame = bcmp_frame(
            src,
            BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            &body,
        );
        let received = accept(frame.as_mut()).expect("a frame we built must validate");

        assert_eq!(received.header.message_type, MessageType::HEARTBEAT);
        assert_eq!(received.payload.len(), Heartbeat::LEN);
        assert_eq!(Heartbeat::decode(received.payload).unwrap(), hb);
        assert_eq!(received.src.to_node_id(), 0x0000_0000_55AA_0011);
    }

    #[test]
    fn the_ingress_nibble_is_read_then_cleared() {
        let src = node_src(1);
        let mut frame = bcmp_frame(
            src,
            BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            &[0; 12],
        );

        // A C node stamps the ingress port into the upper nibble on arrival,
        // which is after the checksum was computed -- and the checksum still
        // has to match, because the clear undoes the stamp.
        frame.as_mut()[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= 0x30;

        let received =
            accept(frame.as_mut()).expect("stamping the ingress port must not break the checksum");
        let (ingress_port, reported_src) = (received.ingress_port, received.src);
        assert_eq!(ingress_port, 3);
        assert_eq!(frame.as_mut()[IPV6_INGRESS_EGRESS_PORTS_OFFSET] >> 4, 0);
        assert_eq!(
            reported_src.0[2] >> 4,
            0,
            "the reported src is the cleared one"
        );
    }

    #[test]
    fn the_legacy_port_bytes_are_cleared_before_checksumming() {
        let mut src = node_src(1);
        src.0[4] = 0xAB;
        src.0[5] = 0xCD;
        let mut frame = bcmp_frame(
            src,
            BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            &[0; 12],
        );

        // Built with the bytes set, but the C clears them before it checks,
        // so a sender that leaves them set is only interoperable if the
        // checksum was computed over the cleared address.
        assert_eq!(accept(frame.as_mut()), Err(RxError::BadChecksum));
        assert_eq!(frame.as_mut()[LEGACY_PORT_CLEAR_OFFSET], 0);
        assert_eq!(frame.as_mut()[LEGACY_PORT_CLEAR_OFFSET + 1], 0);
    }

    #[test]
    fn a_bad_checksum_leaves_the_field_zeroed() {
        let src = node_src(7);
        let mut frame = bcmp_frame(
            src,
            BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            &[0; 12],
        );
        let checksum_offset = BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET;
        frame.as_mut()[checksum_offset] ^= 0xFF;

        assert_eq!(accept(frame.as_mut()), Err(RxError::BadChecksum));
        assert_eq!(
            &frame.as_mut()[checksum_offset..checksum_offset + 2],
            &[0, 0],
            "the C returns before restoring the checksum field"
        );
    }

    #[test]
    fn malformed_frames_are_rejected_without_panicking() {
        let mut short = [0u8; MIN_FRAME_WITH_ADDRESSES - 1];
        assert_eq!(accept(&mut short), Err(RxError::Truncated));

        let src = node_src(1);
        let frame = bcmp_frame(
            src,
            BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            &[0; 12],
        );

        let mut arp = frame.buf;
        arp[13] = 0x06;
        assert_eq!(accept(&mut arp[..frame.len]), Err(RxError::NotIpv6));

        let mut udp = frame.buf;
        udp[IPV6_NEXT_HEADER_OFFSET] = 17;
        assert_eq!(accept(&mut udp[..frame.len]), Err(RxError::NotBcmp));

        // A payload length longer than the buffer.
        let mut long = frame.buf;
        long[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(accept(&mut long[..frame.len]), Err(RxError::Truncated));

        // A payload length too short to hold the BCMP header.
        let mut stub = frame.buf;
        stub[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&((BCMP_HEADER_LEN - 1) as u16).to_be_bytes());
        assert_eq!(accept(&mut stub[..frame.len]), Err(RxError::Truncated));
    }

    #[test]
    fn trailing_bytes_beyond_the_payload_length_are_ignored() {
        let src = node_src(9);
        let mut frame = bcmp_frame(
            src,
            BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::HEARTBEAT,
            &[0; 12],
        );
        let end = frame.len;
        frame.len = end + 16;
        frame.buf[end..end + 16].fill(0xA5);
        let received =
            accept(frame.as_mut()).expect("trailing garbage must not affect the checksum");
        assert_eq!(received.payload.len(), 12);
    }
}
