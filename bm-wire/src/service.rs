//! bm_core's service layer: `middleware/bm_service*.c` and the built-in
//! services' bodies.
//!
//! The service list, request dispatch and echo are [`ServiceTable`] and
//! [`echo`]; the requests a node waits on are [`Requests`]. `bm_stack::Node`
//! runs them.
//!
//! A body is a CBOR map with fixed keys in a fixed order, built and read by
//! `bm_common_messages/*_msg.c` through tinycbor. Encoding uses [`cbor2`];
//! decoding goes through [`crate::cbor::parser`], tinycbor's parser ported,
//! because the decoders' outcomes on malformed input are tinycbor's.
//!
//! Every decoder reads keys by position and checks only that each is a text
//! string; the key's bytes are never compared.

pub mod config_map;
pub mod metrics;
pub mod power_info;
mod request;
pub mod sys_info;
mod table;

pub use request::{EXPIRY_PERIOD_MS, ReplyOutcome, Request, Requests, RequestsFull};
pub use table::{
    Lookup, MAX_DATA_SIZE, MAX_SERVICE_LEN, REPLY_DATA_LEN, REPLY_SUFFIX, REQUEST_SUFFIX,
    ReplyHeader, RequestHeader, ServiceTable, TableFull, echo, service_name, topic,
};

use cbor2::core::{Encoder, Header};

use crate::cbor::parser::{CborError, Value};

/// A map being encoded into a caller's buffer, as tinycbor's
/// `cbor_encoder_*` calls write one.
///
/// tinycbor keeps counting past the end of the buffer and reports
/// `CborErrorOutOfMemory` at the end; nothing is sent then, so only the
/// outcome is reproduced.
struct MapWriter<'o, 'b> {
    enc: Encoder<&'o mut &'b mut [u8]>,
    full: bool,
}

impl<'o, 'b> MapWriter<'o, 'b> {
    /// `cbor_encoder_init` and `cbor_encoder_create_map(fields)`.
    fn new(tail: &'o mut &'b mut [u8], fields: usize) -> Self {
        let mut w = Self {
            enc: Encoder::from(tail),
            full: false,
        };
        w.push(Header::Map(Some(fields)));
        w
    }

    fn push(&mut self, header: Header) {
        self.full |= self.enc.push(header).is_err();
    }

    fn write(&mut self, bytes: &[u8]) {
        self.full |= self.enc.write_all(bytes).is_err();
    }

    /// `cbor_encode_text_stringz(key)` then `cbor_encode_uint(value)`.
    fn uint(&mut self, key: &str, value: u64) {
        self.text(key.as_bytes());
        self.push(Header::Positive(value));
    }

    /// `cbor_encode_text_string`.
    fn text(&mut self, text: &[u8]) {
        self.push(Header::Text(Some(text.len())));
        self.write(text);
    }

    /// `cbor_encode_uint`.
    fn positive(&mut self, value: u64) {
        self.push(Header::Positive(value));
    }

    /// `cbor_encoder_create_map` with a definite length.
    fn map(&mut self, len: usize) {
        self.push(Header::Map(Some(len)));
    }

    /// `cbor_encode_float`: always the 5-byte `fa` form (divergence #43).
    fn float(&mut self, value: f32) {
        self.write(&[0xfa]);
        self.write(&value.to_bits().to_be_bytes());
    }

    /// `cbor_encode_double`: always the 9-byte `fb` form.
    fn double(&mut self, value: f64) {
        self.write(&[0xfb]);
        self.write(&value.to_bits().to_be_bytes());
    }

    /// `cbor_encode_byte_string`.
    fn bytes(&mut self, bytes: &[u8]) {
        self.push(Header::Bytes(Some(bytes.len())));
        self.write(bytes);
    }
}

/// Run `body` against a map of `fields` entries written into `out`,
/// returning the encoded length.
fn encode_map(
    out: &mut [u8],
    fields: usize,
    body: impl FnOnce(&mut MapWriter<'_, '_>),
) -> Result<usize, CborError> {
    let size = out.len();
    let mut tail: &mut [u8] = out;
    let full = {
        let mut w = MapWriter::new(&mut tail, fields);
        body(&mut w);
        w.full
    };
    if full {
        Err(CborError::OutOfMemory)
    } else {
        Ok(size - tail.len())
    }
}

/// `cbor_parser_init`, `cbor_value_validate_basic`, a map of exactly
/// `fields` entries, and `cbor_value_enter_container`: the opening every
/// decoder here shares. Returns the map and an iterator on its first key.
fn enter_map(buf: &[u8], fields: usize) -> Result<(Value<'_>, Value<'_>), CborError> {
    let map = Value::parse(buf)?;
    map.validate_basic()?;
    if !map.is_map() {
        return Err(CborError::IllegalType);
    }
    if map.map_length()? != fields {
        return Err(CborError::UnknownLength);
    }
    let value = map.enter_container()?;
    Ok((map, value))
}

/// A key: any text string, stepped over.
fn skip_key(value: &mut Value<'_>) -> Result<(), CborError> {
    if !value.is_text_string() {
        return Err(CborError::IllegalType);
    }
    value.advance()
}

/// `cbor_value_leave_container` and the `cbor_value_at_end` check that
/// follows it.
///
/// The map is the top-level item, so leaving it always reaches the end and
/// `CborErrorGarbageAtEnd` is never returned: bytes after the map are
/// ignored.
fn leave_map<'a>(map: &mut Value<'a>, value: &Value<'a>) -> Result<(), CborError> {
    map.leave_container(value)?;
    if !map.at_end() {
        return Err(CborError::GarbageAtEnd);
    }
    Ok(())
}
