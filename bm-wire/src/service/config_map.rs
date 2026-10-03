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
use crate::configuration::{MapError, Partition};

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

/// `config_map_suffix`: the service is `<node id>/config_map`.
pub const SUFFIX: &[u8] = b"/config_map";

/// The partition a request's `partition_id` names, as
/// `config_map_service_handler` maps it; `None` for any other id.
#[must_use]
pub fn partition(partition_id: u32) -> Option<Partition> {
    match partition_id {
        PARTITION_ID_SYS => Some(Partition::System),
        PARTITION_ID_HW => Some(Partition::Hardware),
        PARTITION_ID_USER => Some(Partition::User),
        _ => None,
    }
}

/// `config_map_service_handler`: write the reply into `out` and return its
/// length, or `None` for no reply.
///
/// `map` is `services_cbor_as_map` for a partition, as
/// [`crate::configuration::ConfigPartition::cbor_map`] writes it. It is
/// called twice, to measure and then to write the map into `out` after the
/// reply's other fields, so the map needs no buffer of its own.
///
/// | Request | Reply |
/// |---|---|
/// | does not decode | none |
/// | `partition_id` not 1, 2 or 3 | `success` 0, no data, the id echoed |
/// | a partition [`MapError::NoMap`] | `success` 0, no data |
/// | a partition [`MapError::Unreachable`] | none: the C is undefined |
/// | a reply over `out` | none (contract 8) |
/// | otherwise | `success` 1 and the map |
#[must_use]
pub fn handle(
    request: &[u8],
    node_id: u64,
    mut map: impl FnMut(Partition, &mut [u8]) -> Result<usize, MapError>,
    out: &mut [u8],
) -> Option<usize> {
    let mut req = ConfigMapRequest::default();
    req.decode_into(request).ok()?;
    let found = match partition(req.partition_id) {
        None => None,
        Some(p) => match map(p, &mut []) {
            Ok(len) | Err(MapError::TooSmall(len)) => Some((p, len)),
            Err(MapError::NoMap) => None,
            Err(MapError::Unreachable) => return None,
        },
    };
    let len = found.map_or(0, |(_, len)| len);
    let head = encode_head(node_id, req.partition_id, found.is_some(), len, out).ok()?;
    let end = head.checked_add(len).filter(|end| *end <= out.len())?;
    if let Some((p, _)) = found {
        map(p, &mut out[head..end]).ok()?;
    }
    Some(end)
}

/// `config_cbor_map_reply_encode` up to `cbor_data`'s bytes: the map, every
/// field, and the byte string's head for `len` bytes.
fn encode_head(
    node_id: u64,
    partition_id: u32,
    success: bool,
    len: usize,
    out: &mut [u8],
) -> Result<usize, CborError> {
    let len32 = u32::try_from(len).map_err(|_| CborError::OutOfMemory)?;
    encode_map(out, REPLY_NUM_FIELDS, |w| {
        w.uint("node_id", node_id);
        w.uint("partition_id", partition_id.into());
        w.uint("success", success.into());
        w.uint("cbor_encoded_map_len", len32.into());
        w.text(b"cbor_data");
        w.bytes_head(len);
    })
}

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
        let data = self.cbor_data;
        let head = encode_head(
            self.node_id,
            self.partition_id,
            self.success,
            data.len(),
            out,
        )?;
        let end = head + data.len();
        out.get_mut(head..end)
            .ok_or(CborError::OutOfMemory)?
            .copy_from_slice(data);
        Ok(end)
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

    /// A partition whose map is `len` bytes of filler.
    fn map_of(len: usize) -> impl FnMut(Partition, &mut [u8]) -> Result<usize, MapError> {
        move |_, out| {
            let Some(out) = out.get_mut(..len) else {
                return Err(MapError::TooSmall(len));
            };
            out.fill(0x5a);
            Ok(len)
        }
    }

    fn request(partition_id: u32) -> ([u8; 32], usize) {
        let mut buf = [0u8; 32];
        let len = ConfigMapRequest { partition_id }.encode(&mut buf).unwrap();
        (buf, len)
    }

    /// The decoded fields, and whether the data is `data`.
    fn reply(out: &[u8], data: &[u8]) -> (u64, u32, bool, u32, bool) {
        let mut d = DecodedConfigMapReply::default();
        d.decode_into(out).unwrap();
        let same = d.cbor_data.map_or(data.is_empty(), |s| s.eq_bytes(data));
        (
            d.node_id,
            d.partition_id,
            d.success,
            d.cbor_encoded_map_len,
            same,
        )
    }

    const NODE: u64 = 0xc0ff_ee00_1234_5678;

    #[test]
    fn the_handler_maps_each_partition_id() {
        for (id, expected) in [
            (PARTITION_ID_SYS, Partition::System),
            (PARTITION_ID_HW, Partition::Hardware),
            (PARTITION_ID_USER, Partition::User),
        ] {
            let (req, len) = request(id);
            let mut out = [0u8; super::super::REPLY_DATA_LEN];
            let n = handle(
                &req[..len],
                NODE,
                |p, out| {
                    assert_eq!(p, expected);
                    map_of(1)(p, out)
                },
                &mut out,
            )
            .unwrap();
            assert_eq!(reply(&out[..n], &[0x5a]), (NODE, id, true, 1, true));
        }
    }

    /// An unknown partition, and one with no map, are still answered.
    #[test]
    fn the_handler_answers_an_unknown_partition_unsuccessfully() {
        let mut out = [0u8; super::super::REPLY_DATA_LEN];
        let (req, len) = request(7);
        let n = handle(
            &req[..len],
            NODE,
            |_, _| unreachable!("no partition"),
            &mut out,
        )
        .unwrap();
        assert_eq!(reply(&out[..n], b""), (NODE, 7, false, 0, true));
        assert_eq!(
            &out[n - 11..n],
            b"\x69cbor_data\x40",
            "an empty byte string"
        );

        let (req, len) = request(PARTITION_ID_HW);
        let n = handle(&req[..len], NODE, |_, _| Err(MapError::NoMap), &mut out).unwrap();
        assert_eq!(reply(&out[..n], b""), (NODE, 2, false, 0, true));
    }

    #[test]
    fn the_handler_sends_nothing_for_a_bad_request_or_an_undefined_map() {
        let mut out = [0u8; super::super::REPLY_DATA_LEN];
        assert_eq!(handle(b"", NODE, map_of(1), &mut out), None);
        assert_eq!(handle(b"\xa0", NODE, map_of(1), &mut out), None);
        let (req, len) = request(PARTITION_ID_SYS);
        assert_eq!(
            handle(
                &req[..len],
                NODE,
                |_, _| Err(MapError::Unreachable),
                &mut out
            ),
            None
        );
    }

    /// Contract 8: a reply over the handler's 1008 bytes is no reply. The
    /// fields before the map take 78 bytes here.
    #[test]
    fn the_handler_sends_nothing_past_its_buffer() {
        let mut out = [0u8; super::super::REPLY_DATA_LEN];
        let (req, len) = request(PARTITION_ID_USER);
        let n = handle(&req[..len], NODE, map_of(930), &mut out).unwrap();
        assert_eq!(n, out.len());
        assert_eq!(reply(&out, &[0x5a; 930]), (NODE, 3, true, 930, true));
        assert_eq!(handle(&req[..len], NODE, map_of(931), &mut out), None);
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
