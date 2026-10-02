//! `bm_common_messages/bm_messages_helper.c`: the encoder and decoder steps
//! the message bodies share.

use crate::cbor::tinycbor::{Error, Value};

/// `max_key_len`: a key buffer's size, NUL included.
pub const MAX_KEY_LEN: usize = 64;

/// `key` as `strlen` and `strcmp` see it: up to its first NUL.
#[must_use]
pub fn c_str(key: &str) -> &[u8] {
    let key = key.as_bytes();
    let end = key.iter().position(|&b| b == 0).unwrap_or(key.len());
    &key[..end]
}

/// tinycbor's encoder over a fixed buffer.
///
/// Items are written while they fit. The first that does not sets
/// out-of-memory and nothing more is written, but lengths keep counting, as
/// `append_to_buffer` does.
#[derive(Debug)]
pub struct Encoder<'b> {
    buf: &'b mut [u8],
    len: usize,
    out_of_memory: bool,
}

impl<'b> Encoder<'b> {
    /// `cbor_encoder_init`.
    pub fn new(buf: &'b mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            out_of_memory: false,
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        let end = self.len + bytes.len();
        if !self.out_of_memory && end <= self.buf.len() {
            self.buf[self.len..end].copy_from_slice(bytes);
        } else {
            self.out_of_memory = true;
        }
        self.len = end;
    }

    /// `encode_number_no_update`: a head of major type `major` in its
    /// shortest form.
    pub fn head(&mut self, major: u8, arg: u64) {
        let m = major << 5;
        let b = arg.to_be_bytes();
        match arg {
            0..24 => self.put(&[m | arg as u8]),
            24..0x100 => self.put(&[m | 24, arg as u8]),
            0x100..0x1_0000 => self.put(&[m | 25, b[6], b[7]]),
            0x1_0000..0x1_0000_0000 => self.put(&[m | 26, b[4], b[5], b[6], b[7]]),
            _ => {
                self.put(&[m | 27]);
                self.put(&b);
            }
        }
    }

    /// `cbor_encode_uint`.
    pub fn uint(&mut self, value: u64) {
        self.head(0, value);
    }

    /// `cbor_encode_text_stringz`: up to the first NUL.
    pub fn text_stringz(&mut self, s: &str) {
        let s = c_str(s);
        self.head(3, s.len() as u64);
        self.put(s);
    }

    /// `cbor_encoder_create_map` with a definite length.
    pub fn map(&mut self, len: usize) {
        self.head(5, len as u64);
    }

    /// `cbor_encode_float`: always the 5-byte `fa` form.
    pub fn float(&mut self, value: f32) {
        self.put(&[0xfa]);
        self.put(&value.to_bits().to_be_bytes());
    }

    /// `cbor_encode_double`: always the 9-byte `fb` form.
    pub fn double(&mut self, value: f64) {
        self.put(&[0xfb]);
        self.put(&value.to_bits().to_be_bytes());
    }

    /// The length written, as `cbor_encoder_get_buffer_size` gives it.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if anything did not fit.
    pub fn finish(self) -> Result<usize, Error> {
        if self.out_of_memory {
            Err(Error::OutOfMemory)
        } else {
            Ok(self.len)
        }
    }
}

/// `decoder_message_enter`: `buf` must start with a well-formed map of
/// exactly `num_fields` pairs. Returns the iterator at its first key.
///
/// Only the first item is validated; bytes after it are ignored.
///
/// # Errors
///
/// A parse error, `IllegalType` if the item is not a map, `UnknownLength` if
/// it has an indefinite length or another number of pairs.
pub fn decoder_message_enter(buf: &[u8], num_fields: usize) -> Result<Value<'_>, Error> {
    let map = Value::init(buf)?;
    map.validate_basic()?;
    if !map.is_map() {
        return Err(Error::IllegalType);
    }
    if map.get_length()? != num_fields {
        return Err(Error::UnknownLength);
    }
    map.enter_container()
}

/// `decode_key_value_uint8`, `_uint32` and `_uint64` before the narrowing
/// cast: a text key, of any content, then an unsigned integer.
///
/// The key is not compared with the field's name.
///
/// # Errors
///
/// `IllegalType` if the key is not a text string or the value not an
/// unsigned integer; else a parse error.
pub fn decode_key_value_uint(value: &mut Value<'_>) -> Result<u64, Error> {
    if !value.is_text_string() {
        return Err(Error::IllegalType);
    }
    value.advance()?;
    if !value.is_unsigned_integer() {
        return Err(Error::IllegalType);
    }
    let v = value.get_uint64();
    value.advance()?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_take_the_shortest_form() {
        let mut buf = [0u8; 32];
        let mut enc = Encoder::new(&mut buf);
        for v in [
            23,
            24,
            0xff,
            0x100,
            0xffff,
            0x1_0000,
            0xffff_ffff,
            0x1_0000_0000,
        ] {
            enc.uint(v);
        }
        let len = enc.finish().unwrap();
        assert_eq!(
            &buf[..len],
            &[
                0x17, 0x18, 0x18, 0x18, 0xff, 0x19, 0x01, 0x00, 0x19, 0xff, 0xff, 0x1a, 0, 1, 0, 0,
                0x1a, 0xff, 0xff, 0xff, 0xff, 0x1b, 0, 0, 0, 1, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn an_overflow_writes_nothing_more_and_keeps_counting() {
        let mut buf = [0u8; 4];
        let mut enc = Encoder::new(&mut buf);
        enc.text_stringz("ab\0cd");
        enc.uint(0x100);
        enc.uint(1);
        assert_eq!(enc.finish(), Err(Error::OutOfMemory));
        assert_eq!(buf, [0x62, b'a', b'b', 0]);
    }
}
