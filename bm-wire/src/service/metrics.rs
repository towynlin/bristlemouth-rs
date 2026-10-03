//! The `metrics` service's reply body: `bm_common_messages/metrics_reply_msg.c`
//! and the field tables of `bm_messages_helper.c`
//! (`bm_encode_fields_from_table`, `bm_decode_fields_from_table`).
//!
//! The body is a CBOR map of four pairs:
//!
//! ```text
//! { "version": u, "node_id": u, "uptime_ms": u,
//!   "data": { "<component>": { "<field>": value, ... }, ... } }
//! ```
//!
//! A component is a flat table of [`Entry`]s, each a key and a [`Field`].
//! The decoder looks components up by key and fields by key, writing each
//! into the [`Field`] of the matching entry.
//!
//! Divergences reproduced here: #85 (a `String` field), #86 (what decode
//! checks and what it skips), #87 (a tagged field value).

use super::{MapWriter, encode_map, enter_map, skip_key};
use crate::cbor::parser::{CborError, Value};

/// `max_key_len`: `bm_decode_fields_from_table`'s key buffer, NUL included.
pub const MAX_KEY_LEN: usize = 64;

/// `METRICS_REPLY_VERSION`.
pub const VERSION: u8 = 1;

/// `METRICS_REPLY_NUM_FIELDS`: the top-level map's pairs.
pub const NUM_FIELDS: usize = 4;

/// `metrics_service_suffix`: the service is `<node id>/metrics`.
pub const SUFFIX: &[u8] = b"/metrics";

/// `metrics_service_handler`: encode a [`VERSION`] reply of `node_id`,
/// `uptime_ms` and `components` into `out` and return its length, or `None`
/// for no reply.
///
/// The request's data is not read: a request carrying any is answered, where
/// sys_info and power_info send nothing (divergence #97). A reply that does
/// not fit `out`, or that [`encode`] refuses, is no reply.
#[must_use]
pub fn handle(
    node_id: u64,
    uptime_ms: u32,
    components: &[Component<'_>],
    out: &mut [u8],
) -> Option<usize> {
    let reply = Reply {
        version: VERSION,
        node_id,
        uptime_ms,
    };
    encode(&reply, components, out).ok()
}

/// `BmField`, with the value: the source to encode from, or the destination
/// a decode writes to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Field {
    /// `BM_FIELD_UINT8`.
    U8(u8),
    /// `BM_FIELD_UINT16`.
    U16(u16),
    /// `BM_FIELD_UINT32`.
    U32(u32),
    /// `BM_FIELD_UINT64`.
    U64(u64),
    /// `BM_FIELD_FLOAT`, on the wire as `fa` only.
    Float(f32),
    /// `BM_FIELD_DOUBLE`, on the wire as `fb` only.
    Double(f64),
    /// `BM_FIELD_STRING`, which neither table function implements: encoding
    /// one fails and decoding one skips it (divergence #85).
    String,
}

/// `BmEncoderTableEntry` and `BmDecodeTableEntry`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry<'a> {
    /// The key. As in the C, it ends at its first NUL.
    pub key: &'a str,
    /// The type, and the value.
    pub field: Field,
}

/// `MetricsComponent`: a component to encode.
#[derive(Debug, Clone, Copy)]
pub struct Component<'a> {
    /// The key in `data`. As in the C, it ends at its first NUL.
    pub key: &'a str,
    /// Its fields, in wire order.
    pub fields: &'a [Entry<'a>],
}

/// `MetricsComponentDecode`: a component to look for, and where its fields
/// go.
#[derive(Debug)]
pub struct ComponentMut<'a, 'b> {
    /// The key to find in `data`, compared up to its first NUL.
    pub key: &'a str,
    /// The fields to fill. Each written one's [`Field`] takes the value.
    pub fields: &'b mut [Entry<'a>],
}

/// The metadata of `MetricsReplyData` and `MetricsReplyDecode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reply {
    /// `version`.
    pub version: u8,
    /// `node_id`.
    pub node_id: u64,
    /// `uptime_ms`.
    pub uptime_ms: u32,
}

/// `metrics_reply_encode`: the body into `buf`, returning its length.
///
/// # Errors
///
/// In the order the C meets them:
///
/// | Error | When |
/// |---|---|
/// | `UnsupportedType` | a component's last field is [`Field::String`] |
/// | `TooFewItems` | a component has a [`Field::String`] elsewhere |
/// | `OutOfMemory` | the body does not fit `buf` |
///
/// On error `buf`'s contents are unspecified; the C sends nothing.
pub fn encode(
    reply: &Reply,
    components: &[Component<'_>],
    buf: &mut [u8],
) -> Result<usize, CborError> {
    // The C meets these while encoding, but they depend only on the tables
    // and win over `OutOfMemory`, so they are checked first.
    for c in components {
        string_entries(c.fields)?;
    }
    encode_map(buf, NUM_FIELDS, |w| {
        w.uint("version", reply.version.into());
        w.uint("node_id", reply.node_id);
        w.uint("uptime_ms", reply.uptime_ms.into());
        w.text(b"data");
        w.map(components.len());
        for c in components {
            w.text(c_str(c.key));
            w.map(c.fields.len());
            encode_fields(w, c.fields);
        }
    })
}

/// What `bm_encode_fields_from_table` and closing the component's map make
/// of [`Field::String`] entries.
///
/// A `String` entry writes its key and no value, and sets `UnsupportedType`,
/// which the next entry's key overwrites. So a trailing `String` returns
/// `UnsupportedType`, and any other leaves the map one item short, which
/// `cbor_encoder_close_container` reports as `TooFewItems`.
fn string_entries(entries: &[Entry<'_>]) -> Result<(), CborError> {
    match entries.last() {
        Some(Entry {
            field: Field::String,
            ..
        }) => Err(CborError::UnsupportedType),
        _ if entries.iter().any(|e| e.field == Field::String) => Err(CborError::TooFewItems),
        _ => Ok(()),
    }
}

/// `bm_encode_fields_from_table`, for a table [`string_entries`] accepts.
fn encode_fields(w: &mut MapWriter<'_, '_>, entries: &[Entry<'_>]) {
    for e in entries {
        w.text(c_str(e.key));
        match e.field {
            Field::U8(v) => w.positive(v.into()),
            Field::U16(v) => w.positive(v.into()),
            Field::U32(v) => w.positive(v.into()),
            Field::U64(v) => w.positive(v),
            Field::Float(v) => w.float(v),
            Field::Double(v) => w.double(v),
            Field::String => {}
        }
    }
}

/// `key` as `strlen` and `strcmp` see it: up to its first NUL.
fn c_str(key: &str) -> &[u8] {
    let key = key.as_bytes();
    let end = key.iter().position(|&b| b == 0).unwrap_or(key.len());
    &key[..end]
}

/// `decode_key_value_uint8`, `_uint32` and `_uint64` before the narrowing
/// cast: a text key, of any content (divergence #86), then an unsigned
/// integer.
fn key_value_uint(value: &mut Value<'_>) -> Result<u64, CborError> {
    skip_key(value)?;
    if !value.is_unsigned_integer() {
        return Err(CborError::IllegalType);
    }
    let v = value.extract();
    value.advance()?;
    Ok(v)
}

/// `metrics_reply_decode`: fill `reply` and each component's fields from
/// `buf`.
///
/// Writes happen as the C makes them, so on error what was decoded before it
/// stays written:
///
/// - The top-level map must have exactly four pairs. Its keys are not
///   compared with `version`, `node_id`, `uptime_ms` and `data`; the first
///   three must be text keys with unsigned values, truncated to the field,
///   and the fourth a text key with a map value.
/// - Components are looked up in `components` order, by exact key; the
///   first match wins. An absent component is skipped, its fields
///   untouched. A present one that is not a map ends the decode.
/// - See [`decode_fields`] for a component's fields. A type mismatch in one
///   ends the decode; an unknown key does not.
///
/// # Errors
///
/// A tinycbor parse error, `IllegalType` or `UnknownLength` from the shape
/// above, or `ImproperValue`, `UnknownLength` or `IllegalType` from a
/// component's fields. `Unreachable` where the C fails a tinycbor assertion
/// (divergence #87).
pub fn decode(
    buf: &[u8],
    reply: &mut Reply,
    components: &mut [ComponentMut<'_, '_>],
) -> Result<(), CborError> {
    let (_, mut value) = enter_map(buf, NUM_FIELDS)?;
    reply.version = key_value_uint(&mut value)? as u8;
    reply.node_id = key_value_uint(&mut value)?;
    reply.uptime_ms = key_value_uint(&mut value)? as u32;

    if !value.is_text_string() {
        return Err(CborError::IllegalType);
    }
    value.advance()?;
    if !value.is_map() {
        return Err(CborError::IllegalType);
    }
    let data = value;

    for c in components.iter_mut() {
        let comp = data.map_find_value(c_str(c.key))?;
        if !comp.is_valid() {
            continue;
        }
        if !comp.is_map() {
            return Err(CborError::IllegalType);
        }
        let mut field = comp.enter_container()?;
        match decode_fields(&mut field, c.fields) {
            Ok(()) | Err(CborError::UnsupportedType) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `bm_decode_fields_from_table` over the map `value` has entered.
///
/// For each pair in wire order, so a repeated key is written each time:
///
/// | Pair | Effect |
/// |---|---|
/// | key not a text string | stop: `IllegalType` |
/// | key of indefinite length | stop: `UnknownLength` |
/// | key over 63 bytes | skipped |
/// | key equal, up to its first NUL, to an entry's | that entry, below |
/// | any other key | skipped; `UnsupportedType` at the end |
///
/// The first entry whose key matches takes the value if its type does: an
/// unsigned integer for `U8`–`U64`, truncated; `fa` for `Float`; `fb` for
/// `Double`. Otherwise it is untouched and the decode returns
/// `ImproperValue` at the end, which takes precedence over
/// `UnsupportedType`. A `String` entry matches and is never written.
///
/// # Errors
///
/// As in the table; `Unreachable` for a tagged value (divergence #87).
pub fn decode_fields(value: &mut Value<'_>, entries: &mut [Entry<'_>]) -> Result<(), CborError> {
    let mut unknown_key = false;
    let mut type_mismatch = false;

    while !value.at_end() {
        if !value.is_text_string() {
            return Err(CborError::IllegalType);
        }
        let key_len = value.string_length()?;
        if key_len > MAX_KEY_LEN - 1 {
            value.advance()?;
            value.advance()?;
            continue;
        }
        let mut key = [0u8; MAX_KEY_LEN];
        // `key_len` bytes into `key_len` bytes of room: no NUL, never short.
        let copied = value.copy_string(key_len, |at, chunk| {
            key[at..at + chunk.len()].copy_from_slice(chunk);
        })?;
        if !copied.all {
            return Err(CborError::OutOfMemory);
        }
        let key = &key[..key.iter().position(|&b| b == 0).unwrap_or(key_len)];
        value.advance()?;

        match entries.iter_mut().find(|e| c_str(e.key) == key) {
            Some(entry) => {
                let v = &*value;
                let uint = v.is_unsigned_integer();
                match &mut entry.field {
                    Field::U8(d) if uint => *d = v.extract() as u8,
                    Field::U16(d) if uint => *d = v.extract() as u16,
                    Field::U32(d) if uint => *d = v.extract() as u32,
                    Field::U64(d) if uint => *d = v.extract(),
                    Field::Float(d) if v.is_float() => *d = f32::from_bits(v.extract() as u32),
                    Field::Double(d) if v.is_double() => *d = f64::from_bits(v.extract()),
                    // The C sets `UnsupportedType` and the advance below
                    // overwrites it.
                    Field::String => {}
                    _ => type_mismatch = true,
                }
            }
            None => unknown_key = true,
        }
        value.advance()?;
    }

    if type_mismatch {
        Err(CborError::ImproperValue)
    } else if unknown_key {
        Err(CborError::UnsupportedType)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
