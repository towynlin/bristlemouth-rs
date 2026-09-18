//! The BCMP header, ported from `BcmpHeader` in `bcmp/messages.h`.
//!
//! The C declares the header as a `__attribute__((packed))` struct and relies
//! on the host being little-endian — `check_endianness` in `bcmp/packet.c` is
//! a no-op on every shipped target. This port writes the byte order down
//! explicitly instead of mirroring the struct, so the wire layout survives a
//! big-endian build.

use crate::BmWireError;
use crate::frame::{IPV6_ADDRESS_SIZE, IPV6_DESTINATION_ADDRESS_OFFSET};

/// Wire size of a BCMP header.
///
/// Thirteen, not sixteen: the C struct is packed, so nothing is padded.
pub const BCMP_HEADER_LEN: usize = 13;

/// Offset of the BCMP header within an Ethernet + IPv6 frame.
///
/// The C derives this the same way, as `bcmp_header_offset` in `messages.h`.
pub const BCMP_HEADER_OFFSET: usize = IPV6_DESTINATION_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE;

/// Shortest frame that can carry a BCMP header, `min_bcmp_frame_size` in the C.
pub const MIN_BCMP_FRAME_SIZE: usize = BCMP_HEADER_OFFSET + BCMP_HEADER_LEN;

/// Offset of the checksum field within the BCMP header.
///
/// Named because L2 reaches into it directly on transmit: `network_add_egress_port`
/// adjusts the checksum in place rather than recomputing it.
pub const CHECKSUM_FIELD_OFFSET: usize = 2;

/// A BCMP message type.
///
/// A newtype rather than an enum because that is how bm_core treats it: the
/// dispatcher looks the raw `uint16_t` up in a linked list and silently ignores
/// anything it does not find, so an unknown type is ordinary traffic to be
/// round-tripped, not a parse error.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MessageType(pub u16);

impl MessageType {
    /// `BcmpAckMessage`.
    pub const ACK: Self = Self(0x00);
    /// `BcmpHeartbeatMessage`.
    pub const HEARTBEAT: Self = Self(0x01);
    /// `BcmpEchoRequestMessage`.
    pub const ECHO_REQUEST: Self = Self(0x02);
    /// `BcmpEchoReplyMessage`.
    pub const ECHO_REPLY: Self = Self(0x03);
    /// `BcmpDeviceInfoRequestMessage`.
    pub const DEVICE_INFO_REQUEST: Self = Self(0x04);
    /// `BcmpDeviceInfoReplyMessage`.
    pub const DEVICE_INFO_REPLY: Self = Self(0x05);
    /// `BcmpProtocolCapsRequestMessage`.
    pub const PROTOCOL_CAPS_REQUEST: Self = Self(0x06);
    /// `BcmpProtocolCapsReplyMessage`.
    pub const PROTOCOL_CAPS_REPLY: Self = Self(0x07);
    /// `BcmpNeighborTableRequestMessage`.
    pub const NEIGHBOR_TABLE_REQUEST: Self = Self(0x08);
    /// `BcmpNeighborTableReplyMessage`.
    pub const NEIGHBOR_TABLE_REPLY: Self = Self(0x09);
    /// `BcmpResourceTableRequestMessage`.
    pub const RESOURCE_TABLE_REQUEST: Self = Self(0x0A);
    /// `BcmpResourceTableReplyMessage`.
    pub const RESOURCE_TABLE_REPLY: Self = Self(0x0B);
    /// `BcmpNeighborProtoRequestMessage`.
    pub const NEIGHBOR_PROTO_REQUEST: Self = Self(0x0C);
    /// `BcmpNeighborProtoReplyMessage`.
    pub const NEIGHBOR_PROTO_REPLY: Self = Self(0x0D);
    /// `BcmpSystemTimeRequestMessage`.
    pub const SYSTEM_TIME_REQUEST: Self = Self(0x10);
    /// `BcmpSystemTimeResponseMessage`.
    pub const SYSTEM_TIME_RESPONSE: Self = Self(0x11);
    /// `BcmpSystemTimeSetMessage`.
    pub const SYSTEM_TIME_SET: Self = Self(0x12);
    /// `BcmpConfigGetMessage`.
    pub const CONFIG_GET: Self = Self(0xA0);
    /// `BcmpConfigValueMessage`.
    pub const CONFIG_VALUE: Self = Self(0xA1);
    /// `BcmpConfigSetMessage`.
    pub const CONFIG_SET: Self = Self(0xA2);
    /// `BcmpConfigCommitMessage`.
    pub const CONFIG_COMMIT: Self = Self(0xA3);
    /// `BcmpConfigStatusRequestMessage`.
    pub const CONFIG_STATUS_REQUEST: Self = Self(0xA4);
    /// `BcmpConfigStatusResponseMessage`.
    pub const CONFIG_STATUS_RESPONSE: Self = Self(0xA5);
    /// `BcmpConfigDeleteRequestMessage`.
    pub const CONFIG_DELETE_REQUEST: Self = Self(0xA6);
    /// `BcmpConfigDeleteResponseMessage`.
    pub const CONFIG_DELETE_RESPONSE: Self = Self(0xA7);
    /// `BcmpConfigClearRequestMessage`.
    pub const CONFIG_CLEAR_REQUEST: Self = Self(0xA8);
    /// `BcmpConfigClearResponseMessage`.
    pub const CONFIG_CLEAR_RESPONSE: Self = Self(0xA9);
    /// `BcmpNetStateRequestMessage`.
    pub const NET_STATE_REQUEST: Self = Self(0xB0);
    /// `BcmpNetStateReplyMessage`.
    pub const NET_STATE_REPLY: Self = Self(0xB1);
    /// `BcmpPowerStateRequestMessage`.
    pub const POWER_STATE_REQUEST: Self = Self(0xB2);
    /// `BcmpPowerStateReplyMessage`.
    pub const POWER_STATE_REPLY: Self = Self(0xB3);
    /// `BcmpRebootRequestMessage`.
    pub const REBOOT_REQUEST: Self = Self(0xC0);
    /// `BcmpRebootReplyMessage`.
    pub const REBOOT_REPLY: Self = Self(0xC1);
    /// `BcmpNetAssertQuietMessage`.
    pub const NET_ASSERT_QUIET: Self = Self(0xC2);
    /// `BcmpDFUStartMessage`.
    pub const DFU_START: Self = Self(0xD0);
    /// `BcmpDFUPayloadReqMessage`.
    pub const DFU_PAYLOAD_REQ: Self = Self(0xD1);
    /// `BcmpDFUPayloadMessage`.
    pub const DFU_PAYLOAD: Self = Self(0xD2);
    /// `BcmpDFUEndMessage`.
    pub const DFU_END: Self = Self(0xD3);
    /// `BcmpDFUAckMessage`.
    pub const DFU_ACK: Self = Self(0xD4);
    /// `BcmpDFUAbortMessage`.
    pub const DFU_ABORT: Self = Self(0xD5);
    /// `BcmpDFUHeartbeatMessage`.
    pub const DFU_HEARTBEAT: Self = Self(0xD6);
    /// `BcmpDFURebootReqMessage`.
    pub const DFU_REBOOT_REQ: Self = Self(0xD7);
    /// `BcmpDFURebootMessage`.
    pub const DFU_REBOOT: Self = Self(0xD8);
    /// `BcmpDFUBootCompleteMessage`.
    pub const DFU_BOOT_COMPLETE: Self = Self(0xD9);
    /// `BcmpHeaderMessage`, the sentinel the C uses to byte-swap a header.
    pub const HEADER: Self = Self(0xFFFF);

    /// The spec name for this type, or `None` if it is not one bm_core knows.
    #[must_use]
    pub const fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::ACK => "Ack",
            Self::HEARTBEAT => "Heartbeat",
            Self::ECHO_REQUEST => "EchoRequest",
            Self::ECHO_REPLY => "EchoReply",
            Self::DEVICE_INFO_REQUEST => "DeviceInfoRequest",
            Self::DEVICE_INFO_REPLY => "DeviceInfoReply",
            Self::PROTOCOL_CAPS_REQUEST => "ProtocolCapsRequest",
            Self::PROTOCOL_CAPS_REPLY => "ProtocolCapsReply",
            Self::NEIGHBOR_TABLE_REQUEST => "NeighborTableRequest",
            Self::NEIGHBOR_TABLE_REPLY => "NeighborTableReply",
            Self::RESOURCE_TABLE_REQUEST => "ResourceTableRequest",
            Self::RESOURCE_TABLE_REPLY => "ResourceTableReply",
            Self::NEIGHBOR_PROTO_REQUEST => "NeighborProtoRequest",
            Self::NEIGHBOR_PROTO_REPLY => "NeighborProtoReply",
            Self::SYSTEM_TIME_REQUEST => "SystemTimeRequest",
            Self::SYSTEM_TIME_RESPONSE => "SystemTimeResponse",
            Self::SYSTEM_TIME_SET => "SystemTimeSet",
            Self::CONFIG_GET => "ConfigGet",
            Self::CONFIG_VALUE => "ConfigValue",
            Self::CONFIG_SET => "ConfigSet",
            Self::CONFIG_COMMIT => "ConfigCommit",
            Self::CONFIG_STATUS_REQUEST => "ConfigStatusRequest",
            Self::CONFIG_STATUS_RESPONSE => "ConfigStatusResponse",
            Self::CONFIG_DELETE_REQUEST => "ConfigDeleteRequest",
            Self::CONFIG_DELETE_RESPONSE => "ConfigDeleteResponse",
            Self::CONFIG_CLEAR_REQUEST => "ConfigClearRequest",
            Self::CONFIG_CLEAR_RESPONSE => "ConfigClearResponse",
            Self::NET_STATE_REQUEST => "NetStateRequest",
            Self::NET_STATE_REPLY => "NetStateReply",
            Self::POWER_STATE_REQUEST => "PowerStateRequest",
            Self::POWER_STATE_REPLY => "PowerStateReply",
            Self::REBOOT_REQUEST => "RebootRequest",
            Self::REBOOT_REPLY => "RebootReply",
            Self::NET_ASSERT_QUIET => "NetAssertQuiet",
            Self::DFU_START => "DfuStart",
            Self::DFU_PAYLOAD_REQ => "DfuPayloadReq",
            Self::DFU_PAYLOAD => "DfuPayload",
            Self::DFU_END => "DfuEnd",
            Self::DFU_ACK => "DfuAck",
            Self::DFU_ABORT => "DfuAbort",
            Self::DFU_HEARTBEAT => "DfuHeartbeat",
            Self::DFU_REBOOT_REQ => "DfuRebootReq",
            Self::DFU_REBOOT => "DfuReboot",
            Self::DFU_BOOT_COMPLETE => "DfuBootComplete",
            Self::HEADER => "Header",
            _ => return None,
        })
    }
}

impl core::fmt::Debug for MessageType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "MessageType({:#06x})", self.0),
        }
    }
}

/// The BCMP header, decoded.
///
/// `flags`, `reserved`, `frag_total`, `frag_id` and `next_header` are all
/// documented by bm_core as unused and are written as zero by `serialize`; they
/// are kept here so a received header round-trips byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BcmpHeader {
    /// Message type.
    pub message_type: MessageType,
    /// One's-complement checksum over the header and body, computed with this
    /// field zeroed. See [`crate::checksum::ipv6_pseudo_checksum`].
    pub checksum: u16,
    /// Unused by bm_core.
    pub flags: u8,
    /// Unused by bm_core.
    pub reserved: u8,
    /// Sequence number, correlating a reply with its request.
    pub seq_num: u32,
    /// Unused by bm_core; fragmentation is not implemented.
    pub frag_total: u8,
    /// Unused by bm_core; fragmentation is not implemented.
    pub frag_id: u8,
    /// Unused by bm_core.
    pub next_header: u8,
}

impl BcmpHeader {
    /// Decode a header from the first [`BCMP_HEADER_LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the header.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let buf: &[u8; BCMP_HEADER_LEN] = buf
            .get(..BCMP_HEADER_LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;

        Ok(Self {
            message_type: MessageType(u16::from_le_bytes([buf[0], buf[1]])),
            checksum: u16::from_le_bytes([buf[2], buf[3]]),
            flags: buf[4],
            reserved: buf[5],
            seq_num: u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]),
            frag_total: buf[10],
            frag_id: buf[11],
            next_header: buf[12],
        })
    }

    /// Encode into the first [`BCMP_HEADER_LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the header.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf
            .get_mut(..BCMP_HEADER_LEN)
            .ok_or(BmWireError::Truncated)?;

        buf[0..2].copy_from_slice(&self.message_type.0.to_le_bytes());
        buf[2..4].copy_from_slice(&self.checksum.to_le_bytes());
        buf[4] = self.flags;
        buf[5] = self.reserved;
        buf[6..10].copy_from_slice(&self.seq_num.to_le_bytes());
        buf[10] = self.frag_total;
        buf[11] = self.frag_id;
        buf[12] = self.next_header;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_thirteen_bytes_at_offset_fifty_four() {
        assert_eq!(BCMP_HEADER_LEN, 13, "the C struct is packed, not padded");
        assert_eq!(BCMP_HEADER_OFFSET, 54);
        assert_eq!(MIN_BCMP_FRAME_SIZE, 67);
    }

    #[test]
    fn header_round_trips_little_endian() {
        let header = BcmpHeader {
            message_type: MessageType::DEVICE_INFO_REPLY,
            checksum: 0x1AEF,
            flags: 0x11,
            reserved: 0x22,
            seq_num: 0xDEAD_BEEF,
            frag_total: 0x33,
            frag_id: 0x44,
            next_header: 0x55,
        };

        let mut buf = [0u8; BCMP_HEADER_LEN];
        header.encode(&mut buf).unwrap();
        assert_eq!(
            buf,
            [
                0x05, 0x00, 0xEF, 0x1A, 0x11, 0x22, 0xEF, 0xBE, 0xAD, 0xDE, 0x33, 0x44, 0x55
            ]
        );
        assert_eq!(BcmpHeader::decode(&buf).unwrap(), header);
    }

    /// The first thirteen bytes of `ipv6_pseudo_checksum_real_packet2` in
    /// `bm_core/test/src/bm_linux_test.cpp` -- a heartbeat off a real link,
    /// captured with its checksum already zeroed.
    #[test]
    fn a_captured_heartbeat_header_decodes() {
        let bytes = [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let header = BcmpHeader::decode(&bytes).unwrap();
        assert_eq!(header.message_type, MessageType::HEARTBEAT);
        assert_eq!(header.checksum, 0);
        assert_eq!(header.seq_num, 0, "heartbeat is not a sequenced message");
    }

    #[test]
    fn short_buffers_are_rejected_rather_than_panicking() {
        for len in 0..BCMP_HEADER_LEN {
            assert_eq!(
                BcmpHeader::decode(&[0u8; BCMP_HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
            let mut buf = [0u8; BCMP_HEADER_LEN];
            assert_eq!(
                BcmpHeader::default().encode(&mut buf[..len]),
                Err(BmWireError::Truncated)
            );
        }
    }

    /// bm_core dispatches on the raw `uint16_t` and silently ignores a type it
    /// has no entry for, so an unknown type is traffic to be carried, not a
    /// parse error.
    #[test]
    fn unknown_types_round_trip_unchanged() {
        let ty = MessageType(0x4242);
        assert_eq!(ty.name(), None);
        assert_eq!(MessageType::HEARTBEAT.name(), Some("Heartbeat"));

        let header = BcmpHeader {
            message_type: ty,
            ..BcmpHeader::default()
        };
        let mut buf = [0u8; BCMP_HEADER_LEN];
        header.encode(&mut buf).unwrap();
        assert_eq!(&buf[0..2], &[0x42, 0x42]);
        assert_eq!(BcmpHeader::decode(&buf).unwrap().message_type, ty);
    }
}
