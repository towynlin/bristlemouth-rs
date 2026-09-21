//! Re-flooding a link-local BCMP message, ported from `bcmp_ll_forward` in
//! `bcmp/bcmp.c`.
//!
//! A node that receives a link-local message addressed to somebody else puts it
//! back on every port except the one it arrived on, so a chain of nodes relays
//! it hop by hop. `bcmp/time.c`, `bcmp/config.c` and `bcmp/dfu_core.c` all go
//! through the one function ported here.
//!
//! # It is a new datagram, not a relay
//!
//! bm_core does not re-transmit the frame it received. It builds a *fresh* one:
//! `bm_ip_tx_new` fills in **this** node's link-local address as the source, so
//! the forwarded copy claims the forwarder as its sender and the original
//! sender survives only inside the message body. See divergence #23.
//!
//! What is carried over verbatim is the BCMP header — type, sequence number and
//! the five fields bm_core documents as unused — and the body. Only the
//! checksum is recomputed, because the source address changed.
//!
//! # The egress port travels in the destination address
//!
//! bm_core has no per-port transmit call. To get one copy onto one port it
//! encodes the port into byte 13 of the IPv6 destination address, which
//! `bm_l2_link_output` reads and clears; [`l2::take_requested_egress_port`] is
//! that half. The checksum is computed *before* the byte is set and the byte is
//! cleared before the frame is stamped, so the sum stays valid — the C's own
//! comment says as much.
//!
//! The Ethernet destination MAC does not get the same treatment, and that is
//! divergence #24: it is derived from the port-specific address, so the port
//! number reaches the wire inside the multicast MAC.
//!
//! # Order
//!
//! The three steps are the C's three calls, in its order:
//!
//! 1. write the frame headers with the **plain** `FF02::1` destination, then
//!    [`serialize_forwarded`] — `bm_ip_tx_new` and the copy-checksum-copy
//!    sequence in `bcmp_ll_forward`;
//! 2. [`apply_port_specific_destination`] — `bm_ip_tx_perform(forward, dst)`;
//! 3. [`l2::take_requested_egress_port`] — `bm_l2_link_output`.
//!
//! Doing 2 before 1 checksums the port into the frame and every receiver
//! rejects it.

use crate::BmWireError;
use crate::addr;
use crate::bcmp::header::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, CHECKSUM_FIELD_OFFSET};
use crate::checksum::ipv6_pseudo_checksum;
use crate::frame::{
    ETHERNET_DESTINATION_OFFSET, IP_PROTO_BCMP, IPV6_ADDRESS_SIZE, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
};
use crate::l2;
use crate::util::BmIpAddr;

/// Ports a link-local message is re-flooded out of.
///
/// `bcmp_ll_forward` walks 1..=`num_ports` and skips `ingress_port`. An
/// `ingress_port` of zero — which is what [`crate::bcmp::rx::accept`] reports
/// for a sender that encoded no port — skips nothing, so the message goes
/// straight back out the port it came in on.
///
/// The iterator is empty for a single-port device, and the C turns that into a
/// `BmEINVAL`: see [`ll_forward_is_a_no_op`].
pub fn egress_ports(num_ports: u8, ingress_port: u8) -> impl Iterator<Item = u8> {
    (1..=num_ports).filter(move |port| *port != ingress_port)
}

/// Whether `bcmp_ll_forward` would find no port to forward to, and so return
/// `BmEINVAL` having transmitted nothing.
///
/// The C initialises its error to `BmEINVAL` and only ever clears it on a
/// successful transmit, so a one-port device — or a `num_ports` of zero —
/// reports a forward it never attempted as a bad argument.
#[must_use]
pub fn ll_forward_is_a_no_op(num_ports: u8, ingress_port: u8) -> bool {
    egress_ports(num_ports, ingress_port).next().is_none()
}

/// The destination `bcmp_ll_forward` hands the IP layer for `egress_port`.
///
/// The C writes it as
/// `((uint32_t *)port_specific_dst)[3] = 0x1000000 | (egress_port << 8)`, a
/// 32-bit store over the last four bytes of a `uint8_t[16]`. On a little-endian
/// host that lands `[0x00, egress_port, 0x00, 0x01]` at bytes 12..16 — the
/// `0x01` reinstating the byte `FF02::1` already had, and `egress_port` landing
/// where `bm_l2_link_output` looks for it.
///
/// Written out byte by byte here, because the C's store is a strict-aliasing
/// violation *and* endianness-dependent: on a big-endian host it would produce
/// `[0x01, 0x00, egress_port, 0x00]`, which addresses no port and corrupts the
/// address. `bm_core` has no big-endian target, so this reproduces the
/// little-endian result deliberately rather than by accident.
#[must_use]
pub fn port_specific_destination(egress_port: u8) -> BmIpAddr {
    let mut dst = BmIpAddr::LINK_LOCAL_MULTICAST;
    let word = 0x0100_0000u32 | (u32::from(egress_port) << 8);
    dst.0[12..16].copy_from_slice(&word.to_le_bytes());
    dst
}

/// Copy a received BCMP header and body into `frame`, then checksum them.
///
/// `bcmp` is the received message exactly as it arrived — header first, body
/// after it — which is what the C's two `bm_ip_tx_copy` calls put in the
/// forwarded buffer. `frame` must already carry the Ethernet and IPv6 headers,
/// with the **plain** `FF02::1` destination and this node's source address: the
/// checksum covers both, and the port-specific destination goes on afterwards.
///
/// Returns the total frame length.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `bcmp` is shorter than a BCMP header, or if
/// `frame` cannot hold the frame headers and `bcmp`.
pub fn serialize_forwarded(frame: &mut [u8], bcmp: &[u8]) -> Result<usize, BmWireError> {
    if bcmp.len() < BCMP_HEADER_LEN {
        return Err(BmWireError::Truncated);
    }
    let end = BCMP_HEADER_OFFSET + bcmp.len();
    frame
        .get_mut(BCMP_HEADER_OFFSET..end)
        .ok_or(BmWireError::Truncated)?
        .copy_from_slice(bcmp);

    // The C zeroes the header's checksum before the copy and sums the buffer
    // with the field clear, so whatever the received frame carried there is
    // not part of the new sum.
    let checksum_offset = BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET;
    frame[checksum_offset] = 0;
    frame[checksum_offset + 1] = 0;

    let src = read_addr(frame, IPV6_SOURCE_ADDRESS_OFFSET);
    let dst = read_addr(frame, IPV6_DESTINATION_ADDRESS_OFFSET);
    let checksum = ipv6_pseudo_checksum(&src, &dst, IP_PROTO_BCMP, &frame[BCMP_HEADER_OFFSET..end]);
    frame[checksum_offset..checksum_offset + 2].copy_from_slice(&checksum.to_le_bytes());

    Ok(end)
}

/// Point a finished frame at one egress port, as `bm_ip_tx_perform` does.
///
/// Writes [`port_specific_destination`] into the IPv6 destination field and
/// re-derives the Ethernet destination MAC from it. The MAC is the part that
/// reaches the wire: `bm_l2_link_output` clears the port byte out of the
/// address again, but nothing clears it out of the MAC. That is divergence #24.
///
/// Call this *after* the checksum has been computed over the plain destination.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `frame` cannot hold both addresses.
pub fn apply_port_specific_destination(
    frame: &mut [u8],
    egress_port: u8,
) -> Result<(), BmWireError> {
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return Err(BmWireError::Truncated);
    }
    let dst = port_specific_destination(egress_port);
    frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE]
        .copy_from_slice(&dst.0);
    // is_multicast(dst) always holds for FF02::1, so bm_ip_tx_perform takes
    // the multicast-MAC branch and never the broadcast one.
    frame[ETHERNET_DESTINATION_OFFSET..ETHERNET_DESTINATION_OFFSET + addr::MAC_LEN]
        .copy_from_slice(&addr::multicast_mac_from_ipv6(&dst));
    Ok(())
}

/// The mask `bm_l2_link_output` derives for a frame
/// [`apply_port_specific_destination`] prepared, clearing the port byte.
///
/// A thin alias for [`l2::take_requested_egress_port`], named for the caller
/// that motivates it. The egress port is only ever a request channel between
/// `bcmp_ll_forward` and L2; it must not reach the wire.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `frame` cannot hold a destination address.
pub fn take_egress_port(frame: &mut [u8], num_ports: u8) -> Result<u16, BmWireError> {
    l2::take_requested_egress_port(frame, num_ports)
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
    use crate::bcmp::{rx, tx};
    use crate::frame::{
        ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IPV6_INGRESS_EGRESS_PORTS_OFFSET,
        IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    };
    use crate::l2::REQUESTED_EGRESS_PORT_OFFSET;

    const FORWARDER: u64 = 0xC0FF_EE00_1234_5678;
    const ORIGINATOR: u64 = 0x0000_0000_55AA_0011;

    /// A frame carrying `bcmp` from `src`, with the headers a forwarder writes.
    fn frame_for(src: u64, bcmp_len: usize) -> [u8; 256] {
        let mut frame = [0u8; 256];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(bcmp_len as u16).to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
            .copy_from_slice(&addr::nodeid_to_ip(0xFE80_0000, src).0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&BmIpAddr::LINK_LOCAL_MULTICAST.0);
        frame
    }

    /// A received BCMP region: a header of some type with a distinctive body,
    /// plus the five unused header fields deliberately dirtied.
    fn received_bcmp(body_len: usize) -> ([u8; 64], usize) {
        let mut bcmp = [0u8; 64];
        bcmp[0..2].copy_from_slice(&MessageType::SYSTEM_TIME_REQUEST.0.to_le_bytes());
        bcmp[2..4].copy_from_slice(&0xBEEFu16.to_le_bytes()); // the arriving checksum
        bcmp[4] = 0xA5; // flags
        bcmp[5] = 0x5A; // reserved
        bcmp[6..10].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // seq_num
        bcmp[10] = 0x12; // frag_total
        bcmp[11] = 0x34; // frag_id
        bcmp[12] = 0x77; // next_header
        for (index, byte) in bcmp[BCMP_HEADER_LEN..BCMP_HEADER_LEN + body_len]
            .iter_mut()
            .enumerate()
        {
            *byte = (index as u8).wrapping_mul(37);
        }
        (bcmp, BCMP_HEADER_LEN + body_len)
    }

    /// Collect an egress-port walk without an allocator.
    fn ports_of(num_ports: u8, ingress_port: u8) -> ([u8; 16], usize) {
        let mut out = [0u8; 16];
        let mut len = 0;
        for port in egress_ports(num_ports, ingress_port) {
            out[len] = port;
            len += 1;
        }
        (out, len)
    }

    #[test]
    fn the_ingress_port_is_the_only_one_skipped() {
        let (ports, len) = ports_of(4, 1);
        assert_eq!(&ports[..len], &[2, 3, 4]);
        let (ports, len) = ports_of(4, 3);
        assert_eq!(&ports[..len], &[1, 2, 4]);
        // A port the device does not have skips nothing.
        let (ports, len) = ports_of(2, 7);
        assert_eq!(&ports[..len], &[1, 2]);
        // Neither does an unencoded ingress port -- the message goes back out
        // the way it came.
        let (ports, len) = ports_of(2, 0);
        assert_eq!(&ports[..len], &[1, 2]);
    }

    #[test]
    fn a_forward_with_nowhere_to_go_is_reported_as_a_bad_argument() {
        assert!(ll_forward_is_a_no_op(1, 1));
        assert!(ll_forward_is_a_no_op(0, 0));
        assert!(!ll_forward_is_a_no_op(1, 0));
        assert!(!ll_forward_is_a_no_op(2, 1));
    }

    #[test]
    fn the_port_specific_destination_is_ff02_1_with_the_port_at_byte_13() {
        for port in 0..=255u8 {
            let dst = port_specific_destination(port);
            let mut expected = BmIpAddr::LINK_LOCAL_MULTICAST;
            expected.0[13] = port;
            assert_eq!(dst.0, expected.0, "port {port}");
            assert!(
                dst.is_link_local_neighbor_multicast(),
                "L2 must still see FF02::1, or it would not stamp the frame"
            );
        }
        // Byte 15 is rewritten with the same value FF02::1 already carried, so
        // the address is still the neighbour multicast address when the port is
        // taken back out.
        let dst = port_specific_destination(2);
        assert_eq!(dst.0[15], 0x01);
        assert_eq!(dst.0[12], 0x00);
        assert_eq!(dst.0[14], 0x00);
    }

    #[test]
    fn the_header_is_carried_over_verbatim_and_only_the_checksum_changes() {
        let (bcmp, len) = received_bcmp(12);
        let mut frame = frame_for(FORWARDER, len);
        let end = serialize_forwarded(&mut frame, &bcmp[..len]).unwrap();
        assert_eq!(end, MIN_FRAME_WITH_ADDRESSES + len);

        let forwarded = &frame[BCMP_HEADER_OFFSET..end];
        // Every byte but the two checksum bytes is the received message.
        assert_eq!(forwarded[0..2], bcmp[0..2], "type");
        assert_eq!(forwarded[4..len], bcmp[4..len], "everything after checksum");
        assert_ne!(
            forwarded[2..4],
            bcmp[2..4],
            "the checksum must have been recomputed"
        );
    }

    #[test]
    fn a_forwarded_frame_is_accepted_by_the_receive_path_on_every_port() {
        for body_len in 0..16usize {
            let (bcmp, len) = received_bcmp(body_len);
            for port in 1..=8u8 {
                let mut frame = frame_for(FORWARDER, len);
                let end = serialize_forwarded(&mut frame, &bcmp[..len]).unwrap();

                apply_port_specific_destination(&mut frame[..end], port).unwrap();
                assert_eq!(
                    take_egress_port(&mut frame[..end], 8).unwrap(),
                    1u16 << (port - 1),
                    "the port byte selects exactly that port"
                );
                assert_eq!(
                    frame[REQUESTED_EGRESS_PORT_OFFSET], 0,
                    "the request channel must not reach the wire"
                );

                // L2 stamps the egress port and patches the checksum; the node
                // at the other end stamps its ingress port on arrival.
                let mut arrived = [0u8; 256];
                {
                    let stamped = l2::stamp_egress_port(&mut frame[..end], port).unwrap();
                    arrived[..end].copy_from_slice(&stamped);
                }
                arrived[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= 3 << 4;

                let received = rx::accept(&mut arrived[..end]).unwrap_or_else(|e| {
                    panic!("body {body_len} port {port}: a forwarded frame was rejected: {e}")
                });
                assert_eq!(
                    received.header.message_type,
                    MessageType::SYSTEM_TIME_REQUEST
                );
                assert_eq!(received.header.seq_num, 0xDEAD_BEEF);
                assert_eq!(received.payload, &bcmp[BCMP_HEADER_LEN..len]);
                assert_eq!(received.ingress_port, 3);
            }
        }
    }

    /// The forwarded copy claims the forwarder, not the originator, as its
    /// source: divergence #23.
    #[test]
    fn the_forwarder_replaces_the_source_address() {
        let (bcmp, len) = received_bcmp(12);
        let mut frame = frame_for(FORWARDER, len);
        let end = serialize_forwarded(&mut frame, &bcmp[..len]).unwrap();
        let received = rx::accept(&mut frame[..end]).unwrap();
        assert_eq!(received.src.to_node_id(), FORWARDER);
        assert_ne!(received.src.to_node_id(), ORIGINATOR);
    }

    /// The egress port survives in the Ethernet destination MAC: divergence #24.
    #[test]
    fn the_multicast_mac_carries_the_egress_port() {
        let (bcmp, len) = received_bcmp(4);
        for port in 1..=4u8 {
            let mut frame = frame_for(FORWARDER, len);
            let end = serialize_forwarded(&mut frame, &bcmp[..len]).unwrap();
            apply_port_specific_destination(&mut frame[..end], port).unwrap();
            take_egress_port(&mut frame[..end], 4).unwrap();

            assert_eq!(
                &frame[ETHERNET_DESTINATION_OFFSET..ETHERNET_DESTINATION_OFFSET + 6],
                &[0x33, 0x33, 0x00, port, 0x00, 0x01],
                "the port reaches the wire inside the multicast MAC"
            );
            assert_eq!(
                &frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16],
                &BmIpAddr::LINK_LOCAL_MULTICAST.0,
                "while the address it came from is clean again"
            );
        }
    }

    /// Setting the port-specific destination before checksumming is the
    /// mistake the C's own comment warns against, and it is fatal.
    #[test]
    fn checksumming_after_the_port_is_set_produces_a_frame_no_node_accepts() {
        let (bcmp, len) = received_bcmp(12);
        let mut frame = frame_for(FORWARDER, len);
        let end = MIN_FRAME_WITH_ADDRESSES + len;
        apply_port_specific_destination(&mut frame[..end], 2).unwrap();
        serialize_forwarded(&mut frame[..end], &bcmp[..len]).unwrap();
        take_egress_port(&mut frame[..end], 2).unwrap();

        assert_eq!(
            rx::accept(&mut frame[..end]),
            Err(rx::RxError::BadChecksum),
            "the port number would be inside the sum"
        );
    }

    #[test]
    fn short_buffers_are_refused() {
        let (bcmp, len) = received_bcmp(12);
        let mut frame = frame_for(FORWARDER, len);
        assert_eq!(
            serialize_forwarded(
                &mut frame[..MIN_FRAME_WITH_ADDRESSES + len - 1],
                &bcmp[..len]
            ),
            Err(BmWireError::Truncated)
        );
        assert_eq!(
            serialize_forwarded(&mut frame, &bcmp[..BCMP_HEADER_LEN - 1]),
            Err(BmWireError::Truncated),
            "a message shorter than a BCMP header is not one"
        );
        let mut short = [0u8; MIN_FRAME_WITH_ADDRESSES - 1];
        assert_eq!(
            apply_port_specific_destination(&mut short, 1),
            Err(BmWireError::Truncated)
        );
    }

    /// A body-less forward is legal: `bcmp_ll_forward` is called with
    /// `data.size` straight from the frame, and a header-only message has a
    /// size of zero.
    #[test]
    fn a_header_only_message_forwards() {
        let (bcmp, len) = received_bcmp(0);
        assert_eq!(len, BCMP_HEADER_LEN);
        let mut frame = frame_for(FORWARDER, len);
        let end = serialize_forwarded(&mut frame, &bcmp[..len]).unwrap();
        let received = rx::accept(&mut frame[..end]).unwrap();
        assert!(received.payload.is_empty());
    }

    /// And a forward of a message this node itself built round-trips, which is
    /// the shape M2's system-time relay will have.
    #[test]
    fn a_message_serialize_built_can_be_forwarded_unchanged() {
        let body = [9u8; 16];
        let mut original = frame_for(ORIGINATOR, BCMP_HEADER_LEN + body.len());
        let end = MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN + body.len();
        tx::serialize(
            &mut original[..end],
            MessageType::SYSTEM_TIME_SET,
            0x0102_0304,
            &body,
        )
        .unwrap();
        let received = rx::accept(&mut original[..end]).unwrap();
        assert_eq!(received.header.seq_num, 0x0102_0304);
        let bcmp_len = end - BCMP_HEADER_OFFSET;
        let mut bcmp = [0u8; 64];
        bcmp[..bcmp_len].copy_from_slice(&original[BCMP_HEADER_OFFSET..end]);

        let mut frame = frame_for(FORWARDER, bcmp_len);
        let end = serialize_forwarded(&mut frame, &bcmp[..bcmp_len]).unwrap();
        let forwarded = rx::accept(&mut frame[..end]).unwrap();
        assert_eq!(forwarded.header.message_type, MessageType::SYSTEM_TIME_SET);
        assert_eq!(forwarded.header.seq_num, 0x0102_0304);
        assert_eq!(forwarded.payload, &body);
    }
}
