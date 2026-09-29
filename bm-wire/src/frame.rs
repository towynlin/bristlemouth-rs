//! Ethernet and IPv6 frame field offsets, ported from `network/network_frames.h`.
//!
//! The C keeps these as macros derived from each other so a field cannot drift
//! out of position. The same derivation is preserved here.
//!
//! [`write_headers`] fills the Ethernet and IPv6 headers every transmitted
//! frame starts with.

use crate::BmWireError;
use crate::addr::{self, MAC_LEN};
use crate::util::BmIpAddr;

/// Size of the Ethernet destination MAC field.
pub const ETHERNET_DESTINATION_SIZE: usize = 6;
/// Size of the Ethernet source MAC field.
pub const ETHERNET_SRC_SIZE: usize = 6;
/// Size of the EtherType field.
pub const ETHERNET_TYPE_SIZE: usize = 2;

/// Offset of the Ethernet destination MAC.
pub const ETHERNET_DESTINATION_OFFSET: usize = 0;
/// Offset of the Ethernet source MAC.
pub const ETHERNET_SRC_OFFSET: usize = ETHERNET_DESTINATION_OFFSET + ETHERNET_DESTINATION_SIZE;
/// Offset of the EtherType.
pub const ETHERNET_TYPE_OFFSET: usize = ETHERNET_SRC_OFFSET + ETHERNET_SRC_SIZE;

/// EtherType identifying an IPv6 payload.
pub const ETHERNET_TYPE_IPV6: u16 = 0x86DD;

/// Size of the combined IPv6 version/traffic-class/flow-label word.
pub const IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_SIZE: usize = 4;
/// Size of the IPv6 payload-length field.
pub const IPV6_PAYLOAD_LENGTH_SIZE: usize = 2;
/// Size of the IPv6 next-header field.
pub const IPV6_NEXT_HEADER_SIZE: usize = 1;
/// Size of the IPv6 hop-limit field.
pub const IPV6_HOP_LIMIT_SIZE: usize = 1;
/// Size of an IPv6 address.
pub const IPV6_ADDRESS_SIZE: usize = 16;

/// Offset of the IPv6 version/traffic-class/flow-label word.
pub const IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET: usize =
    ETHERNET_TYPE_OFFSET + ETHERNET_TYPE_SIZE;
/// Offset of the IPv6 payload-length field.
pub const IPV6_PAYLOAD_LENGTH_OFFSET: usize =
    IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET + IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_SIZE;
/// Offset of the IPv6 next-header field.
pub const IPV6_NEXT_HEADER_OFFSET: usize = IPV6_PAYLOAD_LENGTH_OFFSET + IPV6_PAYLOAD_LENGTH_SIZE;
/// Offset of the IPv6 hop-limit field.
pub const IPV6_HOP_LIMIT_OFFSET: usize = IPV6_NEXT_HEADER_OFFSET + IPV6_NEXT_HEADER_SIZE;
/// Offset of the IPv6 source address.
pub const IPV6_SOURCE_ADDRESS_OFFSET: usize = IPV6_HOP_LIMIT_OFFSET + IPV6_HOP_LIMIT_SIZE;
/// Offset of the IPv6 destination address.
pub const IPV6_DESTINATION_ADDRESS_OFFSET: usize = IPV6_SOURCE_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE;

/// Byte within the IPv6 source address carrying the ingress port (upper nibble)
/// and egress port (lower nibble), per the Bristlemouth port-encoding
/// convention.
pub const IPV6_INGRESS_EGRESS_PORTS_OFFSET: usize = IPV6_SOURCE_ADDRESS_OFFSET + 2;

/// Shortest frame that still contains both IPv6 addresses.
///
/// L2 policy refuses to act on anything shorter.
pub const MIN_FRAME_WITH_ADDRESSES: usize = IPV6_DESTINATION_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE;

/// IP protocol number Bristlemouth uses for BCMP.
pub const IP_PROTO_BCMP: u8 = 0xBC;
/// IP protocol number for UDP.
pub const IP_PROTO_UDP: u8 = 17;

/// Offset of the UDP source-port field, immediately after the IPv6 header.
pub const UDP_SOURCE_PORT_OFFSET: usize = IPV6_DESTINATION_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE;
/// Offset of the UDP destination-port field.
pub const UDP_DESTINATION_PORT_OFFSET: usize = UDP_SOURCE_PORT_OFFSET + 2;
/// Offset of the UDP length field.
pub const UDP_LENGTH_OFFSET: usize = UDP_DESTINATION_PORT_OFFSET + 2;
/// Offset of the UDP checksum field.
pub const UDP_CHECKSUM_OFFSET: usize = UDP_LENGTH_OFFSET + 2;
/// Size of a UDP header.
pub const UDP_HEADER_LEN: usize = 8;

/// The hop limit a deployed node sets on everything it transmits: lwIP's
/// `UDP_TTL` for UDP and its default `RAW_TTL` for BCMP's raw pcb, both 255.
/// `bm_linux.c` writes 64 (divergence #70).
pub const HOP_LIMIT: u8 = 255;

/// Write the Ethernet and IPv6 headers for a `payload_len`-byte payload into
/// the start of `buf`.
///
/// What a deployed node's lwIP writes, which is what `bm_ip_tx_new` and
/// `bm_ip_tx_perform` in `network/bm_linux.c` write except for the source MAC
/// (divergence #70):
///
/// | Field | Value |
/// |---|---|
/// | Destination MAC | [`addr::multicast_mac_from_ipv6`] of `dst` if it is multicast, else broadcast: bm_core has no neighbour discovery to resolve a unicast address with |
/// | Source MAC | [`addr::mac_address`] of `src`'s node id |
/// | EtherType | [`ETHERNET_TYPE_IPV6`] |
/// | Version, traffic class, flow label | 6, 0, 0 |
/// | Payload length | `payload_len` |
/// | Next header, hop limit, addresses | as given |
///
/// The payload itself is the caller's. Its length is written here, so a caller
/// filling it afterwards has to know it up front.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `buf` is shorter than
/// [`MIN_FRAME_WITH_ADDRESSES`]; [`BmWireError::Invalid`] if `payload_len`
/// does not fit the 16-bit field.
pub fn write_headers(
    buf: &mut [u8],
    src: &BmIpAddr,
    dst: &BmIpAddr,
    next_header: u8,
    hop_limit: u8,
    payload_len: usize,
) -> Result<(), BmWireError> {
    let payload_len = u16::try_from(payload_len).map_err(|_| BmWireError::Invalid)?;
    let buf = buf
        .get_mut(..MIN_FRAME_WITH_ADDRESSES)
        .ok_or(BmWireError::Truncated)?;

    let dst_mac = if addr::is_multicast(dst) {
        addr::multicast_mac_from_ipv6(dst)
    } else {
        [0xFF; MAC_LEN]
    };
    buf[ETHERNET_DESTINATION_OFFSET..ETHERNET_DESTINATION_OFFSET + MAC_LEN]
        .copy_from_slice(&dst_mac);
    buf[ETHERNET_SRC_OFFSET..ETHERNET_SRC_OFFSET + MAC_LEN]
        .copy_from_slice(&addr::mac_address(src.to_node_id()));
    buf[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + ETHERNET_TYPE_SIZE]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());

    buf[IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET
        ..IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET
            + IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_SIZE]
        .copy_from_slice(&[0x60, 0, 0, 0]);
    buf[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + IPV6_PAYLOAD_LENGTH_SIZE]
        .copy_from_slice(&payload_len.to_be_bytes());
    buf[IPV6_NEXT_HEADER_OFFSET] = next_header;
    buf[IPV6_HOP_LIMIT_OFFSET] = hop_limit;
    buf[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE]
        .copy_from_slice(&src.0);
    buf[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE]
        .copy_from_slice(&dst.0);
    Ok(())
}

/// Read the EtherType from a frame, or `None` if it is too short.
#[must_use]
pub fn ethernet_type(frame: &[u8]) -> Option<u16> {
    let bytes = frame.get(ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + ETHERNET_TYPE_SIZE)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_the_c_header() {
        assert_eq!(ETHERNET_TYPE_OFFSET, 12);
        assert_eq!(IPV6_PAYLOAD_LENGTH_OFFSET, 18);
        assert_eq!(IPV6_NEXT_HEADER_OFFSET, 20);
        assert_eq!(IPV6_SOURCE_ADDRESS_OFFSET, 22);
        assert_eq!(IPV6_INGRESS_EGRESS_PORTS_OFFSET, 24);
        assert_eq!(IPV6_DESTINATION_ADDRESS_OFFSET, 38);
        assert_eq!(MIN_FRAME_WITH_ADDRESSES, 54);
    }

    #[test]
    fn udp_offsets_match_the_c_macros() {
        // udp_src_offset and friends in network/l2.c.
        assert_eq!(UDP_SOURCE_PORT_OFFSET, 54);
        assert_eq!(UDP_DESTINATION_PORT_OFFSET, 56);
        assert_eq!(UDP_LENGTH_OFFSET, 58);
        assert_eq!(UDP_CHECKSUM_OFFSET, 60);
    }

    #[test]
    fn headers_to_multicast_and_unicast() {
        let src = addr::nodeid_to_ip(addr::LINK_LOCAL_PREFIX, 0x0011_2233_4455_6677);
        let mut buf = [0xAAu8; MIN_FRAME_WITH_ADDRESSES + 1];
        write_headers(
            &mut buf,
            &src,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            IP_PROTO_BCMP,
            HOP_LIMIT,
            0x0102,
        )
        .unwrap();
        assert_eq!(buf[..6], [0x33, 0x33, 0, 0, 0, 1]);
        assert_eq!(buf[6..12], [0, 0, 0x44, 0x55, 0x66, 0x77]);
        assert_eq!(
            buf[12..22],
            [0x86, 0xDD, 0x60, 0, 0, 0, 0x01, 0x02, 0xBC, 255]
        );
        assert_eq!(buf[22..38], src.0);
        assert_eq!(buf[38..54], BmIpAddr::LINK_LOCAL_MULTICAST.0);
        assert_eq!(buf[54], 0xAA, "the payload is not touched");

        let unicast = addr::nodeid_to_ip(0xFD00_0000, 7);
        write_headers(&mut buf, &src, &unicast, IP_PROTO_UDP, 1, 0).unwrap();
        assert_eq!(buf[..6], [0xFF; 6]);
        assert_eq!(buf[20..22], [IP_PROTO_UDP, 1]);
    }

    #[test]
    fn headers_need_room_and_a_sixteen_bit_length() {
        let src = BmIpAddr::LINK_LOCAL_MULTICAST;
        let mut short = [0u8; MIN_FRAME_WITH_ADDRESSES - 1];
        assert_eq!(
            write_headers(&mut short, &src, &src, IP_PROTO_BCMP, HOP_LIMIT, 0),
            Err(BmWireError::Truncated)
        );
        let mut buf = [0u8; MIN_FRAME_WITH_ADDRESSES];
        assert_eq!(
            write_headers(&mut buf, &src, &src, IP_PROTO_BCMP, HOP_LIMIT, 0x1_0000),
            Err(BmWireError::Invalid)
        );
    }

    #[test]
    fn ethernet_type_needs_fourteen_bytes() {
        assert_eq!(ethernet_type(&[0u8; 13]), None);
        let mut frame = [0u8; 14];
        frame[12] = 0x86;
        frame[13] = 0xDD;
        assert_eq!(ethernet_type(&frame), Some(ETHERNET_TYPE_IPV6));
    }
}
