//! `sys_info`'s reply body, `bm_common_messages/sys_info_svc_reply_msg.c`.
//!
//! The request is empty. The reply is a map of five entries:
//!
//! | Key | Value |
//! |---|---|
//! | `node_id` | uint |
//! | `git_sha` | uint, 32 bits kept |
//! | `sys_config_crc` | uint, 32 bits kept |
//! | `app_name_strlen` | uint, 32 bits kept |
//! | `app_name` | text |
//!
//! `sys_info_reply_decode` reads each uint with an unchecked
//! `cbor_value_get_uint64`, which asserts in a debug build and reads the
//! head's argument of any item in a release build (divergence #82). The
//! decoder here does the latter,
//! [`crate::cbor::parser::Value::extract`].

use super::{encode_map, enter_map, leave_map, skip_key};
use crate::cbor::parser::{CborError, CborString};

/// `SYS_INFO_REPLY_NUM_FIELDS`.
pub const NUM_FIELDS: usize = 5;

/// `SysInfoReplyData`, to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysInfoReply<'a> {
    /// `node_id`.
    pub node_id: u64,
    /// `git_sha`.
    pub git_sha: u32,
    /// `sys_config_crc`.
    pub sys_config_crc: u32,
    /// `app_name_strlen`. The sys_info service sends `app_name`'s length; the
    /// encoder writes whatever it is given.
    pub app_name_strlen: u32,
    /// `app_name`, without its NUL. The C reads it to its first NUL, so a
    /// name with one in it is not what a C node sends.
    pub app_name: &'a [u8],
}

impl SysInfoReply<'_> {
    /// `sys_info_reply_encode` into `out`, returning the encoded length.
    ///
    /// # Errors
    ///
    /// [`CborError::OutOfMemory`] if it does not fit.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, CborError> {
        encode_map(out, NUM_FIELDS, |w| {
            w.uint("node_id", self.node_id);
            w.uint("git_sha", self.git_sha.into());
            w.uint("sys_config_crc", self.sys_config_crc.into());
            w.uint("app_name_strlen", self.app_name_strlen.into());
            w.text(b"app_name");
            w.text(self.app_name);
        })
    }
}

/// `SysInfoReplyData`, as `sys_info_reply_decode` fills it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecodedSysInfoReply<'a> {
    /// `node_id`.
    pub node_id: u64,
    /// `git_sha`.
    pub git_sha: u32,
    /// `sys_config_crc`.
    pub sys_config_crc: u32,
    /// `app_name_strlen`, as the sender claims it.
    pub app_name_strlen: u32,
    /// `app_name`. Set only on success.
    ///
    /// The C copies it into a buffer of `app_name_strlen + 1` bytes, wrapping
    /// in 32 bits, and NUL-terminates it only if there is room: a name one
    /// byte longer than `app_name_strlen` is accepted without a terminator
    /// (divergence #83).
    pub app_name: Option<CborString<'a>>,
}

impl<'a> DecodedSysInfoReply<'a> {
    /// `sys_info_reply_decode`.
    ///
    /// Fields are written in order as they are read, so a failure leaves the
    /// ones before it changed, as in the C. `app_name` is cleared first.
    ///
    /// The C allocates the name's buffer with `bm_malloc`; this assumes the
    /// allocation succeeds.
    ///
    /// # Errors
    ///
    /// tinycbor's error where the C returns one; [`CborError::Unreachable`]
    /// where the C has undefined behaviour (a tag among the values).
    pub fn decode_into(&mut self, buf: &'a [u8]) -> Result<(), CborError> {
        self.app_name = None;
        let (mut map, mut value) = enter_map(buf, NUM_FIELDS)?;

        skip_key(&mut value)?;
        self.node_id = value.extract();
        value.advance()?;

        skip_key(&mut value)?;
        self.git_sha = value.extract() as u32;
        value.advance()?;

        skip_key(&mut value)?;
        self.sys_config_crc = value.extract() as u32;
        value.advance()?;

        skip_key(&mut value)?;
        self.app_name_strlen = value.extract() as u32;
        value.advance()?;

        skip_key(&mut value)?;
        if !value.is_text_string() {
            return Err(CborError::IllegalType);
        }
        let buflen = self.app_name_strlen.wrapping_add(1) as usize;
        if !value.copy_string(buflen, |_, _| {})?.all {
            return Err(CborError::OutOfMemory);
        }
        self.app_name = Some(value.string()?);
        value.advance()?;

        leave_map(&mut map, &value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let reply = SysInfoReply {
            node_id: 0x0123_4567_89ab_cdef,
            git_sha: 0xdead_beef,
            sys_config_crc: 7,
            app_name_strlen: 11,
            app_name: b"bm_wire_sys",
        };
        let mut buf = [0u8; 128];
        let len = reply.encode(&mut buf).unwrap();
        assert_eq!(
            reply.encode(&mut buf[..len - 1]),
            Err(CborError::OutOfMemory)
        );

        let mut d = DecodedSysInfoReply::default();
        d.decode_into(&buf[..len]).unwrap();
        assert_eq!(d.node_id, reply.node_id);
        assert_eq!(d.git_sha, reply.git_sha);
        assert_eq!(d.sys_config_crc, reply.sys_config_crc);
        assert_eq!(d.app_name_strlen, 11);
        assert!(d.app_name.unwrap().eq_bytes(b"bm_wire_sys"));
    }

    #[test]
    fn a_name_longer_than_its_claimed_length_plus_one_is_refused() {
        let reply = |app_name_strlen| SysInfoReply {
            node_id: 1,
            git_sha: 2,
            sys_config_crc: 3,
            app_name_strlen,
            app_name: b"abc",
        };
        let mut buf = [0u8; 128];
        for (claimed, ok) in [(3, true), (2, true), (1, false), (u32::MAX, false)] {
            let len = reply(claimed).encode(&mut buf).unwrap();
            let got = DecodedSysInfoReply::default().decode_into(&buf[..len]);
            assert_eq!(got.is_ok(), ok, "app_name_strlen {claimed}");
        }
    }
}
