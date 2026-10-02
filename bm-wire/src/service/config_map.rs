//! `config_map`'s bodies, `bm_common_messages/config_cbor_map_srv_request_msg.c`
//! and `config_cbor_map_srv_reply_msg.c`.
//!
//! | Body | Entries |
//! |---|---|
//! | request | `partition_id` uint, 32 bits kept |
//! | reply | `node_id` uint, `partition_id` uint, `success` uint, `cbor_encoded_map_len` uint, `cbor_data` bytes |
//!
//! Both decoders read uints with an unchecked `cbor_value_get_uint64`, as
//! `sys_info`'s does (divergence #82); `success` is any non-zero value.

use super::{encode_map, enter_map, leave_map, skip_key};
use crate::cbor::parser::{CborError, CborString};

/// `CONFIG_CBOR_MAP_REQUEST_NUM_FIELDS`.
pub const REQUEST_NUM_FIELDS: usize = 1;
/// `CONFIG_CBOR_MAP_REPLY_NUM_FIELDS`.
pub const REPLY_NUM_FIELDS: usize = 5;

/// `CONFIG_CBOR_MAP_PARTITION_ID_SYS`.
pub const PARTITION_ID_SYS: u32 = 1;
/// `CONFIG_CBOR_MAP_PARTITION_ID_HW`.
pub const PARTITION_ID_HW: u32 = 2;
/// `CONFIG_CBOR_MAP_PARTITION_ID_USER`.
pub const PARTITION_ID_USER: u32 = 3;

/// `ConfigCborMapRequestData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigMapRequest {
    /// `partition_id`.
    pub partition_id: u32,
}

impl ConfigMapRequest {
    /// `config_cbor_map_request_encode` into `out`, returning the encoded
    /// length.
    ///
    /// # Errors
    ///
    /// [`CborError::OutOfMemory`] if it does not fit.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, CborError> {
        encode_map(out, REQUEST_NUM_FIELDS, |w| {
            w.uint("partition_id", self.partition_id.into());
        })
    }

    /// `config_cbor_map_request_decode`. `partition_id` is written when it
    /// is read, so a later failure leaves it changed.
    ///
    /// # Errors
    ///
    /// tinycbor's error where the C returns one; [`CborError::Unreachable`]
    /// where the C has undefined behaviour (a tagged value).
    pub fn decode_into(&mut self, buf: &[u8]) -> Result<(), CborError> {
        let (mut map, mut value) = enter_map(buf, REQUEST_NUM_FIELDS)?;
        skip_key(&mut value)?;
        self.partition_id = value.extract() as u32;
        value.advance()?;
        leave_map(&mut map, &value)
    }
}

/// `ConfigCborMapReplyData`, to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigMapReply<'a> {
    /// `node_id`.
    pub node_id: u64,
    /// `partition_id`.
    pub partition_id: u32,
    /// `success`, encoded as the uint 0 or 1.
    pub success: bool,
    /// `cbor_data`; `cbor_encoded_map_len` is its length. The service sends
    /// it empty when `success` is false.
    pub cbor_data: &'a [u8],
}

impl ConfigMapReply<'_> {
    /// `config_cbor_map_reply_encode` into `out`, returning the encoded
    /// length.
    ///
    /// # Errors
    ///
    /// [`CborError::OutOfMemory`] if it does not fit.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, CborError> {
        let len = u32::try_from(self.cbor_data.len()).map_err(|_| CborError::OutOfMemory)?;
        encode_map(out, REPLY_NUM_FIELDS, |w| {
            w.uint("node_id", self.node_id);
            w.uint("partition_id", self.partition_id.into());
            w.uint("success", self.success.into());
            w.uint("cbor_encoded_map_len", len.into());
            w.text(b"cbor_data");
            w.bytes(self.cbor_data);
        })
    }
}

/// `ConfigCborMapReplyData`, as `config_cbor_map_reply_decode` fills it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodedConfigMapReply<'a> {
    /// `node_id`.
    pub node_id: u64,
    /// `partition_id`.
    pub partition_id: u32,
    /// `success`: the value read was non-zero.
    pub success: bool,
    /// `cbor_encoded_map_len`, as the sender claims it.
    pub cbor_encoded_map_len: u32,
    /// `cbor_data`, set only when the decode succeeds with `success` and a
    /// non-zero `cbor_encoded_map_len`.
    pub cbor_data: Option<CborString<'a>>,
}

impl<'a> DecodedConfigMapReply<'a> {
    /// `config_cbor_map_reply_decode`.
    ///
    /// Fields are written as they are read, so a failure leaves the ones
    /// before it changed. `cbor_data` is cleared first. Where the C fails
    /// after copying the data it leaves its partial copy in `cbor_data`;
    /// this leaves `None`.
    ///
    /// Unless `success` is set and `cbor_encoded_map_len` is non-zero, the
    /// C returns at the `cbor_data` key without reading its value or
    /// leaving the map (divergence #84): any well-formed value is accepted.
    /// Otherwise the value must be a byte string of exactly
    /// `cbor_encoded_map_len` bytes.
    ///
    /// The C allocates `cbor_encoded_map_len` bytes with `bm_malloc`, and
    /// when that fails succeeds with no data. This assumes the allocation
    /// succeeds.
    ///
    /// # Errors
    ///
    /// tinycbor's error where the C returns one; [`CborError::Unreachable`]
    /// where the C has undefined behaviour (a tagged value, or data that is
    /// not a byte string).
    pub fn decode_into(&mut self, buf: &'a [u8]) -> Result<(), CborError> {
        self.cbor_data = None;
        let (mut map, mut value) = enter_map(buf, REPLY_NUM_FIELDS)?;

        skip_key(&mut value)?;
        self.node_id = value.extract();
        value.advance()?;

        skip_key(&mut value)?;
        self.partition_id = value.extract() as u32;
        value.advance()?;

        skip_key(&mut value)?;
        self.success = value.extract() != 0;
        value.advance()?;

        skip_key(&mut value)?;
        self.cbor_encoded_map_len = value.extract() as u32;
        value.advance()?;

        skip_key(&mut value)?;
        if self.cbor_encoded_map_len == 0 || !self.success {
            return Ok(());
        }
        if !value.is_byte_string() {
            return Err(CborError::Unreachable);
        }
        let len = self.cbor_encoded_map_len as usize;
        let copied = value.copy_string(len, |_, _| {})?;
        if !copied.all {
            return Err(CborError::OutOfMemory);
        }
        if copied.total != len {
            return Err(CborError::IllegalType);
        }
        let data = value.string()?;
        value.advance()?;
        leave_map(&mut map, &value)?;
        self.cbor_data = Some(data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let mut buf = [0u8; 32];
        let len = ConfigMapRequest { partition_id: 3 }
            .encode(&mut buf)
            .unwrap();
        assert_eq!(&buf[..len], b"\xa1\x6cpartition_id\x03");
        let mut d = ConfigMapRequest::default();
        d.decode_into(&buf[..len]).unwrap();
        assert_eq!(d.partition_id, 3);
    }

    /// A non-uint value is read for its head's argument, as a release build
    /// of the C reads it.
    #[test]
    fn request_partition_id_of_any_type() {
        let mut d = ConfigMapRequest::default();
        d.decode_into(b"\xa1\x61p\x62ab").unwrap();
        assert_eq!(d.partition_id, 2);
        assert_eq!(
            d.decode_into(b"\xa1\x61p\xc6\x00"),
            Err(CborError::Unreachable)
        );
    }

    #[test]
    fn reply_round_trip() {
        let reply = ConfigMapReply {
            node_id: 0xfeed,
            partition_id: 1,
            success: true,
            cbor_data: b"\xa0",
        };
        let mut buf = [0u8; 128];
        let len = reply.encode(&mut buf).unwrap();
        let mut d = DecodedConfigMapReply::default();
        d.decode_into(&buf[..len]).unwrap();
        assert_eq!((d.node_id, d.partition_id, d.success), (0xfeed, 1, true));
        assert_eq!(d.cbor_encoded_map_len, 1);
        assert!(d.cbor_data.unwrap().eq_bytes(b"\xa0"));
    }
}
