//! Serializing a BCMP message into a frame, ported from `serialize` in
//! `bcmp/packet.c`.

use crate::BmWireError;
use crate::bcmp::header::{
    BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, BcmpHeader, CHECKSUM_FIELD_OFFSET, MessageType,
};
use crate::checksum::ipv6_pseudo_checksum;
use crate::frame::{
    IP_PROTO_BCMP, IPV6_ADDRESS_SIZE, IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
};
use crate::util::BmIpAddr;

/// Write a BCMP header and body into `frame`, then checksum them.
///
/// `frame` must already carry the IPv6 source and destination addresses: the
/// checksum covers them, so filling them in afterwards invalidates it. This is
/// the same contract the C has, where `bm_ip_tx_new` sets the addresses before
/// `serialize` runs.
///
/// The five fields bm_core documents as unused — `flags`, `reserved`,
/// `frag_total`, `frag_id`, `next_header` — are written as zero, as the C does.
///
/// `seq_num` is the caller's to choose. In the C it comes from the packet
/// registry: a reply echoes the request's number, a request takes the next
/// value from a global counter, and anything else gets zero. That policy is
/// protocol state, not wire format, so it does not live here.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `frame` cannot hold the frame header, the
/// BCMP header and `body`.
pub fn serialize(
    frame: &mut [u8],
    message_type: MessageType,
    seq_num: u32,
    body: &[u8],
) -> Result<(), BmWireError> {
    let end = BCMP_HEADER_OFFSET + BCMP_HEADER_LEN + body.len();
    frame
        .get_mut(BCMP_HEADER_OFFSET + BCMP_HEADER_LEN..end)
        .ok_or(BmWireError::Truncated)?
        .copy_from_slice(body);
    serialize_in_place(frame, message_type, seq_num, body.len())
}

/// The same, for a body the caller has already written into the frame.
///
/// A node building a reply has one buffer, not two: it encodes the message
/// straight into the frame at [`BCMP_HEADER_OFFSET`] + [`BCMP_HEADER_LEN`] and
/// then calls this to put the header and checksum around it. `body_len` is how
/// many bytes it wrote.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `frame` cannot hold the frame header, the
/// BCMP header and `body_len` bytes.
pub fn serialize_in_place(
    frame: &mut [u8],
    message_type: MessageType,
    seq_num: u32,
    body_len: usize,
) -> Result<(), BmWireError> {
    let payload_len = BCMP_HEADER_LEN
        .checked_add(body_len)
        .ok_or(BmWireError::Truncated)?;
    let end = BCMP_HEADER_OFFSET
        .checked_add(payload_len)
        .ok_or(BmWireError::Truncated)?;
    if frame.len() < end {
        return Err(BmWireError::Truncated);
    }

    let header = BcmpHeader {
        message_type,
        checksum: 0,
        seq_num,
        ..BcmpHeader::default()
    };
    header.encode(&mut frame[BCMP_HEADER_OFFSET..])?;

    let src = read_addr(frame, IPV6_SOURCE_ADDRESS_OFFSET);
    let dst = read_addr(frame, IPV6_DESTINATION_ADDRESS_OFFSET);
    let checksum = ipv6_pseudo_checksum(&src, &dst, IP_PROTO_BCMP, &frame[BCMP_HEADER_OFFSET..end]);

    let checksum_offset = BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET;
    frame[checksum_offset..checksum_offset + 2].copy_from_slice(&checksum.to_le_bytes());
    Ok(())
}

fn read_addr(frame: &[u8], offset: usize) -> BmIpAddr {
    let mut addr = [0u8; IPV6_ADDRESS_SIZE];
    addr.copy_from_slice(&frame[offset..offset + IPV6_ADDRESS_SIZE]);
    BmIpAddr(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::MIN_FRAME_WITH_ADDRESSES;

    #[test]
    fn serialize_reproduces_a_captured_heartbeat() {
        // `ipv6_pseudo_checksum_real_packet2` from
        // `bm_core/test/src/bm_linux_test.cpp`: a heartbeat that a live node
        // accepted, checksum 0x3F0C.
        let src = BmIpAddr([
            0xFE, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0xAA,
            0x00, 0x11,
        ]);
        // The capture's 25 BCMP bytes are a 13-byte header -- type 0x01 and
        // twelve zeroes -- followed by this heartbeat body.
        let body = [
            0x30, 0x7C, 0x71, 0x22, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00,
        ];
        let mut frame = [0u8; MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN + 12];
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16].copy_from_slice(&src.0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&BmIpAddr::LINK_LOCAL_MULTICAST.0);

        serialize(&mut frame, MessageType::HEARTBEAT, 0, &body).unwrap();

        let checksum_offset = BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET;
        assert_eq!(
            u16::from_le_bytes([frame[checksum_offset], frame[checksum_offset + 1]]),
            0x3F0C,
            "must agree with the capture, not merely with the C"
        );
        assert_eq!(&frame[BCMP_HEADER_OFFSET + BCMP_HEADER_LEN..], &body);
    }

    #[test]
    fn unused_header_fields_are_zeroed() {
        let mut frame = [0xFFu8; MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN];
        serialize(&mut frame, MessageType::ACK, 0x1234_5678, &[]).unwrap();
        let header = BcmpHeader::decode(&frame[BCMP_HEADER_OFFSET..]).unwrap();
        assert_eq!(header.flags, 0);
        assert_eq!(header.reserved, 0);
        assert_eq!(header.frag_total, 0);
        assert_eq!(header.frag_id, 0);
        assert_eq!(header.next_header, 0);
        assert_eq!(header.seq_num, 0x1234_5678);
    }

    #[test]
    fn in_place_matches_copying_the_body_in() {
        let body = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut copied = [0u8; MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN + 8];
        copied[IPV6_SOURCE_ADDRESS_OFFSET] = 0xFE;
        let mut in_place = copied;

        serialize(&mut copied, MessageType::HEARTBEAT, 7, &body).unwrap();

        let at = BCMP_HEADER_OFFSET + BCMP_HEADER_LEN;
        in_place[at..at + body.len()].copy_from_slice(&body);
        serialize_in_place(&mut in_place, MessageType::HEARTBEAT, 7, body.len()).unwrap();

        assert_eq!(copied, in_place);
    }

    #[test]
    fn a_frame_too_short_for_the_body_is_refused() {
        let mut frame = [0u8; MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN];
        assert_eq!(
            serialize(&mut frame, MessageType::HEARTBEAT, 0, &[0]),
            Err(BmWireError::Truncated)
        );
        assert_eq!(
            serialize(&mut frame, MessageType::HEARTBEAT, 0, &[]),
            Ok(())
        );
    }
}
