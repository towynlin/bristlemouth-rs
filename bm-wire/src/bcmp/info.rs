//! `BcmpDeviceInfoRequest` and `BcmpDeviceInfoReply`, ported from
//! `bcmp/messages.h` and `bcmp/info.c`.
//!
//! The reply is the first message with a variable-length body: a fixed
//! 38-byte head followed by two strings whose lengths are declared inside it,
//! concatenated with no separator and no NUL terminator.
//!
//! # Lengths are checked here and nowhere in the C
//!
//! `populate_neighbor_info` copies `ver_str_len` and `dev_name_len` bytes out
//! of the received frame without ever comparing them to how many bytes
//! actually arrived. [`DeviceInfoReply::decode`] validates both against the
//! buffer instead. See divergence #14 — this is a domain limit, not a
//! behaviour the port reproduces: there is no defined C behaviour to match.

use crate::BmWireError;

/// `BcmpDeviceInfoRequest`: ask one node, or every node, to describe itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceInfoRequest {
    /// Node to answer, or zero for all of them.
    pub target_node_id: u64,
}

impl DeviceInfoRequest {
    /// Wire size.
    pub const LEN: usize = 8;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let bytes: [u8; 8] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        Ok(Self {
            target_node_id: u64::from_le_bytes(bytes),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf.copy_from_slice(&self.target_node_id.to_le_bytes());
        Ok(())
    }
}

/// `BcmpDeviceInfo`: the fixed part of a node's self-description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    /// Node id of the node being described.
    pub node_id: u64,
    /// Vendor of the hardware module.
    pub vendor_id: u16,
    /// Product identifier within the vendor.
    pub product_id: u16,
    /// Factory-flashed serial number.
    pub serial_num: [u8; 16],
    /// Last four bytes of the firmware's git SHA.
    pub git_sha: u32,
    /// Firmware major version.
    pub ver_major: u8,
    /// Firmware minor version.
    pub ver_minor: u8,
    /// Firmware revision.
    pub ver_rev: u8,
    /// Hardware revision, or zero for don't-care.
    pub ver_hw: u8,
}

impl DeviceInfo {
    /// Wire size.
    pub const LEN: usize = 36;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let buf: &[u8; Self::LEN] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        let mut serial_num = [0u8; 16];
        serial_num.copy_from_slice(&buf[12..28]);
        Ok(Self {
            node_id: u64::from_le_bytes(buf[0..8].try_into().expect("8 bytes")),
            vendor_id: u16::from_le_bytes([buf[8], buf[9]]),
            product_id: u16::from_le_bytes([buf[10], buf[11]]),
            serial_num,
            git_sha: u32::from_le_bytes(buf[28..32].try_into().expect("4 bytes")),
            ver_major: buf[32],
            ver_minor: buf[33],
            ver_rev: buf[34],
            ver_hw: buf[35],
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf[0..8].copy_from_slice(&self.node_id.to_le_bytes());
        buf[8..10].copy_from_slice(&self.vendor_id.to_le_bytes());
        buf[10..12].copy_from_slice(&self.product_id.to_le_bytes());
        buf[12..28].copy_from_slice(&self.serial_num);
        buf[28..32].copy_from_slice(&self.git_sha.to_le_bytes());
        buf[32] = self.ver_major;
        buf[33] = self.ver_minor;
        buf[34] = self.ver_rev;
        buf[35] = self.ver_hw;
        Ok(())
    }
}

/// `BcmpDeviceInfoReply`: [`DeviceInfo`] plus two length-prefixed strings.
///
/// The strings are borrowed from the frame and are **not** NUL-terminated on
/// the wire, nor validated as UTF-8 by bm_core. They are exposed as bytes for
/// that reason; use [`core::str::from_utf8`] if a caller needs text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfoReply<'a> {
    /// The fixed part.
    pub info: DeviceInfo,
    /// Firmware version string, at most 255 bytes.
    pub version_string: &'a [u8],
    /// Device name, at most 255 bytes.
    pub device_name: &'a [u8],
}

impl<'a> DeviceInfoReply<'a> {
    /// Size of the fixed part, before the strings. `sizeof(BcmpDeviceInfoReply)`.
    pub const HEADER_LEN: usize = DeviceInfo::LEN + 2;

    /// Longest string either field can carry, since each length is a `u8`.
    pub const MAX_STRING_LEN: usize = u8::MAX as usize;

    /// Decode a reply, borrowing its strings from `buf`.
    ///
    /// Trailing bytes past the declared strings are ignored, as they are by
    /// the C.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// shorter than the string lengths it declares.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let head = buf.get(..Self::HEADER_LEN).ok_or(BmWireError::Truncated)?;
        let info = DeviceInfo::decode(head)?;
        let version_len = usize::from(head[DeviceInfo::LEN]);
        let name_len = usize::from(head[DeviceInfo::LEN + 1]);

        // The check bm_core does not do.
        let strings = buf
            .get(Self::HEADER_LEN..Self::HEADER_LEN + version_len + name_len)
            .ok_or(BmWireError::Truncated)?;

        Ok(Self {
            info,
            version_string: &strings[..version_len],
            device_name: &strings[version_len..],
        })
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.version_string.len() + self.device_name.len()
    }

    /// Encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    /// [`BmWireError::Invalid`] if either string is longer than
    /// [`Self::MAX_STRING_LEN`], since the length fields could not describe it.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        if self.version_string.len() > Self::MAX_STRING_LEN
            || self.device_name.len() > Self::MAX_STRING_LEN
        {
            return Err(BmWireError::Invalid);
        }
        let end = self.encoded_len();
        let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;

        self.info.encode(buf)?;
        buf[DeviceInfo::LEN] = self.version_string.len() as u8;
        buf[DeviceInfo::LEN + 1] = self.device_name.len() as u8;
        let split = Self::HEADER_LEN + self.version_string.len();
        buf[Self::HEADER_LEN..split].copy_from_slice(self.version_string);
        buf[split..end].copy_from_slice(self.device_name);
        Ok(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(DeviceInfo::LEN, 36);
        assert_eq!(DeviceInfoReply::HEADER_LEN, 38);
        assert_eq!(DeviceInfoRequest::LEN, 8);
    }

    /// `ipv6_pseudo_checksum_real_packet3` in
    /// `bm_core/test/src/bm_linux_test.cpp` is a device-info reply captured
    /// off a real link. Decoding it checks the layout against the wire rather
    /// than against our own encoder.
    #[test]
    fn a_captured_reply_decodes() {
        // The 49 body bytes, after the 13-byte BCMP header.
        let body = [
            0x11, 0x00, 0xAA, 0x55, 0x00, 0x00, 0x00, 0x00, // node_id
            0x01, 0x00, // vendor_id
            0x01, 0x00, // product_id
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, // serial_num
            0x00, 0x00, 0x00, 0x00, // git_sha
            0x00, 0x01, 0x00, 0x01, // ver_major, ver_minor, ver_rev, ver_hw
            0x05, 0x06, // ver_str_len, dev_name_len
            0x30, 0x2E, 0x31, 0x2E, 0x30, // "0.1.0"
            0x62, 0x6D, 0x5F, 0x73, 0x62, 0x63, // "bm_sbc"
        ];
        assert_eq!(body.len(), DeviceInfoReply::HEADER_LEN + 5 + 6);

        let reply = DeviceInfoReply::decode(&body).unwrap();
        assert_eq!(reply.info.node_id, 0x0000_0000_55AA_0011);
        assert_eq!(reply.info.vendor_id, 1);
        assert_eq!(reply.info.product_id, 1);
        assert_eq!(reply.info.serial_num, [0u8; 16]);
        assert_eq!(reply.info.git_sha, 0);
        assert_eq!(
            (
                reply.info.ver_major,
                reply.info.ver_minor,
                reply.info.ver_rev,
                reply.info.ver_hw
            ),
            (0, 1, 0, 1)
        );
        assert_eq!(reply.version_string, b"0.1.0");
        assert_eq!(reply.device_name, b"bm_sbc");

        // The node id matches the address the frame came from, fe80::55aa:11.
        let src = crate::util::BmIpAddr([
            0xFE, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0xAA,
            0x00, 0x11,
        ]);
        assert_eq!(src.to_node_id(), reply.info.node_id);

        let mut out = [0u8; 64];
        assert_eq!(reply.encode(&mut out).unwrap(), body.len());
        assert_eq!(&out[..body.len()], &body);
    }

    #[test]
    fn declared_string_lengths_are_checked_against_the_buffer() {
        let mut body = [0u8; DeviceInfoReply::HEADER_LEN + 4];
        body[DeviceInfo::LEN] = 3; // version
        body[DeviceInfo::LEN + 1] = 1; // name -- 4 bytes, exactly fits
        assert!(DeviceInfoReply::decode(&body).is_ok());

        // One byte more than arrived. bm_core would read past the frame here.
        body[DeviceInfo::LEN + 1] = 2;
        assert_eq!(
            DeviceInfoReply::decode(&body),
            Err(BmWireError::Truncated),
            "a length longer than the buffer must be refused, not trusted"
        );

        // The worst case: both lengths saturated on a minimum-size body.
        let mut minimal = [0u8; DeviceInfoReply::HEADER_LEN];
        minimal[DeviceInfo::LEN] = 255;
        minimal[DeviceInfo::LEN + 1] = 255;
        assert_eq!(
            DeviceInfoReply::decode(&minimal),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn empty_strings_round_trip() {
        let reply = DeviceInfoReply {
            info: DeviceInfo::default(),
            version_string: b"",
            device_name: b"",
        };
        let mut buf = [0u8; DeviceInfoReply::HEADER_LEN];
        assert_eq!(reply.encode(&mut buf).unwrap(), DeviceInfoReply::HEADER_LEN);
        assert_eq!(DeviceInfoReply::decode(&buf).unwrap(), reply);
    }

    #[test]
    fn strings_too_long_for_a_u8_length_are_refused() {
        let long = [b'x'; 256];
        let reply = DeviceInfoReply {
            info: DeviceInfo::default(),
            version_string: &long,
            device_name: b"",
        };
        let mut buf = [0u8; 512];
        assert_eq!(reply.encode(&mut buf), Err(BmWireError::Invalid));
    }

    #[test]
    fn trailing_bytes_past_the_strings_are_ignored() {
        let mut body = [0xAAu8; DeviceInfoReply::HEADER_LEN + 16];
        body[..DeviceInfo::LEN].fill(0);
        body[DeviceInfo::LEN] = 2;
        body[DeviceInfo::LEN + 1] = 2;
        let reply = DeviceInfoReply::decode(&body).unwrap();
        assert_eq!(reply.version_string, &[0xAA, 0xAA]);
        assert_eq!(reply.device_name, &[0xAA, 0xAA]);
        assert_eq!(reply.encoded_len(), DeviceInfoReply::HEADER_LEN + 4);
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..DeviceInfoReply::HEADER_LEN {
            assert_eq!(
                DeviceInfoReply::decode(&[0u8; DeviceInfoReply::HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
        }
        for len in 0..DeviceInfoRequest::LEN {
            assert_eq!(
                DeviceInfoRequest::decode(&[0u8; 8][..len]),
                Err(BmWireError::Truncated)
            );
        }
    }
}
