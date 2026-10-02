//! `power_info`'s reply body, `bm_common_messages/power_info_reply_msg.c`.
//!
//! The request is empty. The reply is a map of three uint entries,
//! `total_on_s`, `remaining_on_s` and `upcoming_off_s`, each 32 bits kept.
//! Unlike the other services' decoders this one goes through
//! `bm_messages_helper.c` and checks each value is an unsigned integer.

use super::{encode_map, enter_map, leave_map, skip_key};
use crate::cbor::parser::{CborError, Value};

/// `power_info_reply_msg_num_fields`.
pub const NUM_FIELDS: usize = 3;

/// `PowerInfoReplyData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PowerInfoReply {
    /// `total_on_s`.
    pub total_on_s: u32,
    /// `remaining_on_s`.
    pub remaining_on_s: u32,
    /// `upcoming_off_s`.
    pub upcoming_off_s: u32,
}

/// `decode_key_value_uint32`: the value is written only once the key, the
/// value's type and both advances have succeeded.
fn uint32(value: &mut Value<'_>, out: &mut u32) -> Result<(), CborError> {
    skip_key(value)?;
    if !value.is_unsigned_integer() {
        return Err(CborError::IllegalType);
    }
    let v = value.extract();
    value.advance()?;
    *out = v as u32;
    Ok(())
}

impl PowerInfoReply {
    /// `power_info_reply_encode` into `out`, returning the encoded length.
    ///
    /// # Errors
    ///
    /// [`CborError::OutOfMemory`] if it does not fit.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, CborError> {
        encode_map(out, NUM_FIELDS, |w| {
            w.uint("total_on_s", self.total_on_s.into());
            w.uint("remaining_on_s", self.remaining_on_s.into());
            w.uint("upcoming_off_s", self.upcoming_off_s.into());
        })
    }

    /// `power_info_reply_decode`.
    ///
    /// Fields are written as they are read, so a failure leaves the ones
    /// before it changed. `power_info_reply_cb` hands the result to the
    /// requester's callback whether or not the decode succeeded.
    ///
    /// # Errors
    ///
    /// tinycbor's error where the C returns one.
    pub fn decode_into(&mut self, buf: &[u8]) -> Result<(), CborError> {
        let (mut map, mut value) = enter_map(buf, NUM_FIELDS)?;
        uint32(&mut value, &mut self.total_on_s)?;
        uint32(&mut value, &mut self.remaining_on_s)?;
        uint32(&mut value, &mut self.upcoming_off_s)?;
        leave_map(&mut map, &value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bm_common_messages/test/power_info_ut.cpp`, `PowerInfoReply`. The
    /// test round-trips through a 1024-byte buffer; the bytes are the
    /// oracle's.
    #[test]
    fn power_info_ut() {
        let encode = PowerInfoReply {
            total_on_s: u32::MAX,
            remaining_on_s: 100_000,
            upcoming_off_s: 3_333_333,
        };
        let mut buf = [0u8; 1024];
        let len = encode.encode(&mut buf).unwrap();
        assert_eq!(
            &buf[..len],
            b"\xa3\x6atotal_on_s\x1a\xff\xff\xff\xff\
              \x6eremaining_on_s\x1a\x00\x01\x86\xa0\
              \x6eupcoming_off_s\x1a\x00\x32\xdc\xd5"
        );
        let mut decode = PowerInfoReply::default();
        assert_eq!(decode.decode_into(&buf), Ok(()));
        assert_eq!(decode, encode);
    }

    #[test]
    fn a_failure_keeps_the_fields_read_before_it() {
        let mut buf = [0u8; 64];
        let len = PowerInfoReply {
            total_on_s: 1,
            remaining_on_s: 2,
            upcoming_off_s: 3,
        }
        .encode(&mut buf)
        .unwrap();
        // The last value, 0x03, made negative.
        buf[len - 1] = 0x23;
        let mut d = PowerInfoReply::default();
        assert_eq!(d.decode_into(&buf[..len]), Err(CborError::IllegalType));
        assert_eq!(
            (d.total_on_s, d.remaining_on_s, d.upcoming_off_s),
            (1, 2, 0)
        );
    }
}
