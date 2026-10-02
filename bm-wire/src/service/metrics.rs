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
//! Divergences reproduced here: #82 (a `String` field), #83 (what decode
//! checks and what it skips).

use crate::cbor::tinycbor::{Error, Value};
use crate::service::messages::{
    Encoder, MAX_KEY_LEN, c_str, decode_key_value_uint, decoder_message_enter,
};

/// `METRICS_REPLY_VERSION`.
pub const VERSION: u8 = 1;

/// `METRICS_REPLY_NUM_FIELDS`: the top-level map's pairs.
pub const NUM_FIELDS: usize = 4;

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
    /// one fails and decoding one skips it (divergence #82).
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
/// On error `buf` holds a partial body.
pub fn encode(reply: &Reply, components: &[Component<'_>], buf: &mut [u8]) -> Result<usize, Error> {
    let mut enc = Encoder::new(buf);
    enc.map(NUM_FIELDS);
    enc.text_stringz("version");
    enc.uint(reply.version.into());
    enc.text_stringz("node_id");
    enc.uint(reply.node_id);
    enc.text_stringz("uptime_ms");
    enc.uint(reply.uptime_ms.into());
    enc.text_stringz("data");
    enc.map(components.len());
    for c in components {
        enc.text_stringz(c.key);
        enc.map(c.fields.len());
        encode_fields(&mut enc, c.fields)?;
    }
    enc.finish()
}

/// `bm_encode_fields_from_table`, then closing the component's map.
///
/// A `String` entry writes its key and no value, and sets `UnsupportedType`,
/// which the next entry's key overwrites. So a trailing `String` returns
/// `UnsupportedType`, and any other leaves the map one item short, which
/// `cbor_encoder_close_container` reports as `TooFewItems`.
fn encode_fields(enc: &mut Encoder<'_>, entries: &[Entry<'_>]) -> Result<(), Error> {
    let mut short = false;
    for e in entries {
        enc.text_stringz(e.key);
        match e.field {
            Field::U8(v) => enc.uint(v.into()),
            Field::U16(v) => enc.uint(v.into()),
            Field::U32(v) => enc.uint(v.into()),
            Field::U64(v) => enc.uint(v),
            Field::Float(v) => enc.float(v),
            Field::Double(v) => enc.double(v),
            Field::String => short = true,
        }
    }
    match entries.last() {
        Some(Entry {
            field: Field::String,
            ..
        }) => Err(Error::UnsupportedType),
        _ if short => Err(Error::TooFewItems),
        _ => Ok(()),
    }
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
/// component's fields.
pub fn decode(
    buf: &[u8],
    reply: &mut Reply,
    components: &mut [ComponentMut<'_, '_>],
) -> Result<(), Error> {
    let mut value = decoder_message_enter(buf, NUM_FIELDS)?;
    reply.version = decode_key_value_uint(&mut value)? as u8;
    reply.node_id = decode_key_value_uint(&mut value)?;
    reply.uptime_ms = decode_key_value_uint(&mut value)? as u32;

    if !value.is_text_string() {
        return Err(Error::IllegalType);
    }
    value.advance()?;
    if !value.is_map() {
        return Err(Error::IllegalType);
    }
    let data = value;

    for c in components.iter_mut() {
        let comp = data.map_find_value(c_str(c.key))?;
        if !comp.is_valid() {
            continue;
        }
        if !comp.is_map() {
            return Err(Error::IllegalType);
        }
        let mut field = comp.enter_container()?;
        match decode_fields(&mut field, c.fields) {
            Ok(()) | Err(Error::UnsupportedType) => {}
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
/// As in the table.
pub fn decode_fields(value: &mut Value<'_>, entries: &mut [Entry<'_>]) -> Result<(), Error> {
    let mut unknown_key = false;
    let mut type_mismatch = false;

    while !value.at_end() {
        if !value.is_text_string() {
            return Err(Error::IllegalType);
        }
        let key_len = value.get_length()?;
        if key_len > MAX_KEY_LEN - 1 {
            value.advance()?;
            value.advance()?;
            continue;
        }
        let mut key = [0u8; MAX_KEY_LEN];
        // `key_len` bytes into `key_len` bytes of room: no NUL, never short.
        value.copy_string(&mut key[..key_len])?;
        let key = &key[..key.iter().position(|&b| b == 0).unwrap_or(key_len)];
        value.advance()?;

        match entries.iter_mut().find(|e| c_str(e.key) == key) {
            Some(entry) => {
                let v = &*value;
                let uint = v.is_unsigned_integer();
                match &mut entry.field {
                    Field::U8(d) if uint => *d = v.get_uint64() as u8,
                    Field::U16(d) if uint => *d = v.get_uint64() as u16,
                    Field::U32(d) if uint => *d = v.get_uint64() as u32,
                    Field::U64(d) if uint => *d = v.get_uint64(),
                    Field::Float(d) if v.is_float() => *d = v.get_float(),
                    Field::Double(d) if v.is_double() => *d = v.get_double(),
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
        Err(Error::ImproperValue)
    } else if unknown_key {
        Err(Error::UnsupportedType)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
