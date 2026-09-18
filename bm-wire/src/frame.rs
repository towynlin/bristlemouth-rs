//! Ethernet and IPv6 frame field offsets, ported from `network/network_frames.h`.
//!
//! The C keeps these as macros derived from each other so a field cannot drift
//! out of position. The same derivation is preserved here.

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
    fn ethernet_type_needs_fourteen_bytes() {
        assert_eq!(ethernet_type(&[0u8; 13]), None);
        let mut frame = [0u8; 14];
        frame[12] = 0x86;
        frame[13] = 0xDD;
        assert_eq!(ethernet_type(&frame), Some(ETHERNET_TYPE_IPV6));
    }
}
