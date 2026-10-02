//! tinycbor's parser (`third_party/tinycbor/src/cborparser.c`), as far as
//! `bm_common_messages` uses it.
//!
//! [`cbor2::core::Decoder`] is a different parser: it checks UTF-8, has no
//! nesting limit and reports errors at different points. bm_core's message
//! decoders return tinycbor's error codes and write their outputs as they go,
//! so which error comes first, and where, is observable. This is a
//! line-for-line port of `CborValue` and the functions those decoders call.
//!
//! `cbor_value_advance` recurses up to `CBOR_PARSER_MAX_RECURSIONS` levels,
//! which bm_core's `CMakeLists.txt` sets to 10 (tinycbor's default is 1024).
//! [`Value::advance`] is iterative over a fixed stack of [`MAX_RECURSIONS`]
//! frames.

/// `CborError`, the codes this module and its callers return.
///
/// [`Error::code`] is the C's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Error {
    /// `CborErrorUnknownLength`.
    UnknownLength,
    /// `CborErrorAdvancePastEOF`.
    AdvancePastEof,
    /// `CborErrorUnexpectedEOF`.
    UnexpectedEof,
    /// `CborErrorUnexpectedBreak`.
    UnexpectedBreak,
    /// `CborErrorUnknownType`.
    UnknownType,
    /// `CborErrorIllegalType`.
    IllegalType,
    /// `CborErrorIllegalNumber`.
    IllegalNumber,
    /// `CborErrorIllegalSimpleType`.
    IllegalSimpleType,
    /// `CborErrorImproperValue`.
    ImproperValue,
    /// `CborErrorTooManyItems`.
    TooManyItems,
    /// `CborErrorTooFewItems`.
    TooFewItems,
    /// `CborErrorDataTooLarge`.
    DataTooLarge,
    /// `CborErrorNestingTooDeep`.
    NestingTooDeep,
    /// `CborErrorUnsupportedType`.
    UnsupportedType,
    /// `CborErrorOutOfMemory`.
    OutOfMemory,
}

impl Error {
    /// The `CborError` value.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::UnknownLength => 2,
            Self::AdvancePastEof => 3,
            Self::UnexpectedEof => 257,
            Self::UnexpectedBreak => 258,
            Self::UnknownType => 259,
            Self::IllegalType => 260,
            Self::IllegalNumber => 261,
            Self::IllegalSimpleType => 262,
            Self::ImproperValue => 519,
            Self::TooManyItems => 768,
            Self::TooFewItems => 769,
            Self::DataTooLarge => 1024,
            Self::NestingTooDeep => 1025,
            Self::UnsupportedType => 1026,
            Self::OutOfMemory => i32::MIN,
        }
    }
}

/// `CBOR_PARSER_MAX_RECURSIONS`, as bm_core's `CMakeLists.txt` defines it.
pub const MAX_RECURSIONS: usize = 10;

// `CborType`.
const INTEGER: u8 = 0x00;
const BYTE_STRING: u8 = 0x40;
const TEXT_STRING: u8 = 0x60;
const ARRAY: u8 = 0x80;
const MAP: u8 = 0xa0;
const TAG: u8 = 0xc0;
const SIMPLE: u8 = 0xe0;
const BOOLEAN: u8 = 0xf5;
const FLOAT: u8 = 0xfa;
const DOUBLE: u8 = 0xfb;
const INVALID: u8 = 0xff;

// `CborIteratorFlags`. 0x04 means `NegativeInteger` on an integer and
// `BeforeFirstStringChunk` on a string, as in the C.
const INTEGER_VALUE_TOO_LARGE: u8 = 0x02;
const INTEGER_VALUE_IS_64_BIT: u8 = 0x01;
const NEGATIVE_INTEGER: u8 = 0x04;
const BEFORE_FIRST_STRING_CHUNK: u8 = 0x04;
const ITERATING_STRING_CHUNKS: u8 = 0x08;
const UNKNOWN_LENGTH: u8 = 0x10;
const CONTAINER_IS_MAP: u8 = 0x20;
const NEXT_IS_MAP_KEY: u8 = 0x40;

const VALUE_8_BIT: u8 = 24;
const VALUE_64_BIT: u8 = 27;
const INDEFINITE_LENGTH: u8 = 31;
const BREAK_BYTE: u8 = 0xff;

/// `CborValue` over an in-memory buffer: an iterator positioned at one item.
#[derive(Debug, Clone, Copy)]
pub struct Value<'a> {
    buf: &'a [u8],
    ptr: usize,
    remaining: u32,
    flags: u8,
    ty: u8,
    extra: u16,
}

fn is_fixed_type(ty: u8) -> bool {
    !matches!(ty, TEXT_STRING | BYTE_STRING | ARRAY | MAP)
}

impl<'a> Value<'a> {
    /// `cbor_parser_init`: an iterator at the first item of `buf`.
    ///
    /// # Errors
    ///
    /// What `preparse_value` returns for the first item.
    pub fn init(buf: &'a [u8]) -> Result<Self, Error> {
        let mut it = Self {
            buf,
            ptr: 0,
            remaining: 1,
            flags: 0,
            ty: INVALID,
            extra: 0,
        };
        it.preparse_value()?;
        Ok(it)
    }

    fn can_read(&self, n: usize) -> bool {
        self.buf.len() - self.ptr >= n
    }

    fn byte(&self, offset: usize) -> u8 {
        self.buf[self.ptr + offset]
    }

    fn read_be(&self, offset: usize, n: usize) -> u64 {
        self.buf[self.ptr + offset..self.ptr + offset + n]
            .iter()
            .fold(0, |a, &b| (a << 8) | u64::from(b))
    }

    fn preparse_value(&mut self) -> Result<(), Error> {
        self.ty = INVALID;
        self.flags &= CONTAINER_IS_MAP | NEXT_IS_MAP_KEY;
        if !self.can_read(1) {
            return Err(Error::UnexpectedEof);
        }
        let descriptor = self.byte(0);
        let ty = descriptor & 0xe0;
        let info = descriptor & 0x1f;
        self.ty = ty;
        self.extra = u16::from(info);

        if info > VALUE_64_BIT {
            if info != INDEFINITE_LENGTH {
                return Err(if ty == SIMPLE {
                    Error::UnknownType
                } else {
                    Error::IllegalNumber
                });
            }
            if !is_fixed_type(ty) {
                self.flags |= UNKNOWN_LENGTH;
                return Ok(());
            }
            return Err(if ty == SIMPLE {
                Error::UnexpectedBreak
            } else {
                Error::IllegalNumber
            });
        }

        let bytes_needed = if info < VALUE_8_BIT {
            0
        } else {
            1usize << (info - VALUE_8_BIT)
        };
        if bytes_needed != 0 {
            if !self.can_read(bytes_needed + 1) {
                return Err(Error::UnexpectedEof);
            }
            self.extra = 0;
            match bytes_needed {
                1 => self.extra = u16::from(self.byte(1)),
                2 => self.extra = self.read_be(1, 2) as u16,
                _ => self.flags |= info & 3,
            }
        }

        match ty {
            0x20 => {
                self.flags |= NEGATIVE_INTEGER;
                self.ty = INTEGER;
            }
            SIMPLE => match info {
                20 => {
                    self.extra = 0;
                    self.ty = BOOLEAN;
                }
                26 | 27 => {
                    self.flags |= INTEGER_VALUE_TOO_LARGE;
                    self.ty = descriptor;
                }
                21..=23 | 25 => self.ty = descriptor,
                24 if self.extra < 32 => {
                    self.ty = INVALID;
                    return Err(Error::IllegalSimpleType);
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn preparse_next_value_nodecrement(&mut self) -> Result<(), Error> {
        if self.remaining == u32::MAX && self.can_read(1) && self.byte(0) == BREAK_BYTE {
            if (self.flags & CONTAINER_IS_MAP != 0 && self.flags & NEXT_IS_MAP_KEY != 0)
                || self.ty == TAG
            {
                return Err(Error::UnexpectedBreak);
            }
            self.ty = INVALID;
            self.remaining = 0;
            self.flags |= UNKNOWN_LENGTH;
            return Ok(());
        }
        self.preparse_value()
    }

    fn preparse_next_value(&mut self) -> Result<(), Error> {
        let item_counts = self.ty != TAG;
        if self.remaining != u32::MAX && item_counts {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.ty = INVALID;
                self.flags &= !UNKNOWN_LENGTH;
                return Ok(());
            }
        }
        if item_counts {
            self.flags ^= NEXT_IS_MAP_KEY;
        }
        self.preparse_next_value_nodecrement()
    }

    /// `_cbor_value_extract_int64_helper`.
    fn extract_int64(&self) -> u64 {
        if self.flags & INTEGER_VALUE_TOO_LARGE != 0 {
            if self.flags & INTEGER_VALUE_IS_64_BIT != 0 {
                self.read_be(1, 8)
            } else {
                self.read_be(1, 4)
            }
        } else {
            u64::from(self.extra)
        }
    }

    fn extract_number_and_advance(&mut self) -> u64 {
        let v = self.extract_int64();
        let info = self.byte(0) & 0x1f;
        let bytes_needed = if info < VALUE_8_BIT {
            0
        } else {
            1usize << (info - VALUE_8_BIT)
        };
        self.ptr += bytes_needed + 1;
        v
    }

    fn advance_internal(&mut self) -> Result<(), Error> {
        // Only fixed types reach here; strings go through the chunk iterator.
        self.extract_number_and_advance();
        self.preparse_next_value()
    }

    /// `cbor_value_at_end`.
    #[must_use]
    pub fn at_end(&self) -> bool {
        self.remaining == 0
    }

    /// `cbor_value_is_valid`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.ty != INVALID
    }

    /// `cbor_value_is_text_string`.
    #[must_use]
    pub fn is_text_string(&self) -> bool {
        self.ty == TEXT_STRING
    }

    /// `cbor_value_is_map`.
    #[must_use]
    pub fn is_map(&self) -> bool {
        self.ty == MAP
    }

    /// `cbor_value_is_tag`.
    #[must_use]
    pub fn is_tag(&self) -> bool {
        self.ty == TAG
    }

    /// `cbor_value_is_unsigned_integer`.
    #[must_use]
    pub fn is_unsigned_integer(&self) -> bool {
        self.ty == INTEGER && self.flags & NEGATIVE_INTEGER == 0
    }

    /// `cbor_value_is_float`: the 5-byte `fa` form only.
    #[must_use]
    pub fn is_float(&self) -> bool {
        self.ty == FLOAT
    }

    /// `cbor_value_is_double`: the 9-byte `fb` form only.
    #[must_use]
    pub fn is_double(&self) -> bool {
        self.ty == DOUBLE
    }

    fn is_container(&self) -> bool {
        self.ty == ARRAY || self.ty == MAP
    }

    fn is_length_known(&self) -> bool {
        self.flags & UNKNOWN_LENGTH == 0
    }

    /// `cbor_value_get_uint64`. The caller has checked
    /// [`Self::is_unsigned_integer`].
    #[must_use]
    pub fn get_uint64(&self) -> u64 {
        self.extract_int64()
    }

    /// `cbor_value_get_float`. The caller has checked [`Self::is_float`].
    #[must_use]
    pub fn get_float(&self) -> f32 {
        f32::from_bits(self.read_be(1, 4) as u32)
    }

    /// `cbor_value_get_double`. The caller has checked [`Self::is_double`].
    #[must_use]
    pub fn get_double(&self) -> f64 {
        f64::from_bits(self.read_be(1, 8))
    }

    /// `cbor_value_get_string_length` and `cbor_value_get_map_length`.
    ///
    /// # Errors
    ///
    /// `UnknownLength` for an indefinite length, `DataTooLarge` if it does not
    /// fit a `usize`.
    pub fn get_length(&self) -> Result<usize, Error> {
        if !self.is_length_known() {
            return Err(Error::UnknownLength);
        }
        usize::try_from(self.extract_int64()).map_err(|_| Error::DataTooLarge)
    }

    /// `cbor_value_validate_basic`: advance a copy over this item.
    ///
    /// # Errors
    ///
    /// The first error [`Self::advance`] meets.
    pub fn validate_basic(&self) -> Result<(), Error> {
        let mut copy = *self;
        copy.advance()
    }

    /// `cbor_value_advance_fixed`.
    fn advance_fixed(&mut self) -> Result<(), Error> {
        if self.remaining == 0 {
            return Err(Error::AdvancePastEof);
        }
        self.advance_internal()
    }

    /// `cbor_value_skip_tag`.
    ///
    /// # Errors
    ///
    /// What advancing over a tag returns.
    pub fn skip_tag(&mut self) -> Result<(), Error> {
        while self.is_tag() {
            self.advance_fixed()?;
        }
        Ok(())
    }

    /// `cbor_value_advance`: step over this item and everything inside it.
    ///
    /// `advance_recursive`'s recursion made iterative: each open container's
    /// parent keeps its `remaining` and `flags`, the only fields
    /// `cbor_value_leave_container` reads from it.
    ///
    /// # Errors
    ///
    /// `AdvancePastEof` at the end of a container, else the first parse error
    /// in document order, or `NestingTooDeep` at the eleventh nested container.
    pub fn advance(&mut self) -> Result<(), Error> {
        if self.remaining == 0 {
            return Err(Error::AdvancePastEof);
        }
        let mut remaining = [0u32; MAX_RECURSIONS];
        let mut flags = [0u8; MAX_RECURSIONS];
        let mut depth = 0usize;
        let mut cur = *self;
        loop {
            if is_fixed_type(cur.ty) {
                cur.advance_internal()?;
            } else if !cur.is_container() {
                let mut next = cur;
                cur.iterate_string_chunks(usize::MAX, &mut next, |_, _| true)?;
                cur = next;
            } else {
                if depth == MAX_RECURSIONS {
                    return Err(Error::NestingTooDeep);
                }
                let inner = cur.enter_container()?;
                remaining[depth] = cur.remaining;
                flags[depth] = cur.flags;
                depth += 1;
                cur = inner;
            }
            loop {
                if depth == 0 {
                    *self = cur;
                    return Ok(());
                }
                if !cur.at_end() {
                    break;
                }
                depth -= 1;
                let mut parent = Self {
                    remaining: remaining[depth],
                    flags: flags[depth],
                    ty: ARRAY,
                    ..cur
                };
                parent.leave_container(&cur)?;
                cur = parent;
            }
        }
    }

    /// `cbor_value_enter_container`. The caller has checked this is an array
    /// or a map.
    ///
    /// # Errors
    ///
    /// `DataTooLarge` for a length tinycbor cannot count, else what parsing
    /// the first item returns.
    pub fn enter_container(&self) -> Result<Self, Error> {
        let mut rec = *self;
        if self.flags & UNKNOWN_LENGTH != 0 {
            rec.remaining = u32::MAX;
            rec.ptr += 1;
        } else {
            let len = rec.extract_number_and_advance();
            rec.remaining = len as u32;
            if u64::from(rec.remaining) != len || len == u64::from(u32::MAX) {
                return Err(Error::DataTooLarge);
            }
            if rec.ty == MAP {
                if rec.remaining > u32::MAX / 2 {
                    return Err(Error::DataTooLarge);
                }
                rec.remaining *= 2;
            }
            if len == 0 {
                rec.ty = INVALID;
                return Ok(rec);
            }
        }
        rec.flags = rec.ty & CONTAINER_IS_MAP;
        rec.preparse_next_value_nodecrement()?;
        Ok(rec)
    }

    /// `cbor_value_leave_container`: `self` is the container, `rec` the
    /// iterator inside it, now at its end.
    ///
    /// # Errors
    ///
    /// What parsing the container's next sibling returns.
    pub fn leave_container(&mut self, rec: &Self) -> Result<(), Error> {
        self.ptr = rec.ptr;
        if rec.flags & UNKNOWN_LENGTH != 0 {
            self.ptr += 1;
        }
        self.preparse_next_value()
    }

    /// `get_string_chunk`: the next chunk's bytes, or `None` at the end.
    fn string_chunk(&mut self) -> Result<Option<&'a [u8]>, Error> {
        // `get_string_chunk_size`.
        if self.is_length_known() && self.flags & BEFORE_FIRST_STRING_CHUNK == 0 {
            return Ok(None);
        }
        if !self.can_read(1) {
            return Err(Error::UnexpectedEof);
        }
        let descriptor = self.byte(0);
        if descriptor == BREAK_BYTE {
            return Ok(None);
        }
        if descriptor & 0xe0 != self.ty {
            return Err(Error::IllegalType);
        }
        let info = descriptor & 0x1f;
        let (len, offset) = if info < VALUE_8_BIT {
            (u64::from(info), 1)
        } else if info > VALUE_64_BIT {
            return Err(Error::IllegalNumber);
        } else {
            let n = 1usize << (info - VALUE_8_BIT);
            if !self.can_read(1 + n) {
                return Err(Error::UnexpectedEof);
            }
            (self.read_be(1, n), 1 + n)
        };
        let len = usize::try_from(len).map_err(|_| Error::DataTooLarge)?;
        // `transfer_string`.
        self.ptr += offset;
        if !self.can_read(len) {
            return Err(Error::UnexpectedEof);
        }
        let chunk = &self.buf[self.ptr..self.ptr + len];
        self.ptr += len;
        self.flags &= !BEFORE_FIRST_STRING_CHUNK;
        Ok(Some(chunk))
    }

    /// `iterate_string_chunks`: walk this string's chunks, calling `func`
    /// with each chunk's offset and bytes while they fit `buflen`, then with
    /// a NUL if there is room. Leaves `next` after the string.
    ///
    /// Returns `(result, total)`: whether every call returned `true` and
    /// every chunk fitted, and the string's length.
    fn iterate_string_chunks(
        &self,
        buflen: usize,
        next: &mut Self,
        mut func: impl FnMut(usize, &[u8]) -> bool,
    ) -> Result<(bool, usize), Error> {
        *next = *self;
        let mut result = true;
        let mut total = 0usize;
        // `_cbor_value_begin_string_iteration`.
        next.flags |= ITERATING_STRING_CHUNKS | BEFORE_FIRST_STRING_CHUNK;
        if !next.is_length_known() {
            next.ptr += 1;
        }
        while let Some(chunk) = next.string_chunk()? {
            let new_total = total.checked_add(chunk.len()).ok_or(Error::DataTooLarge)?;
            result = result && buflen >= new_total && func(total, chunk);
            total = new_total;
        }
        if result && buflen > total {
            result = func(total, &[0]);
        }
        // `_cbor_value_finish_string_iteration`.
        if !next.is_length_known() {
            next.ptr += 1;
        }
        next.preparse_next_value()?;
        Ok((result, total))
    }

    /// `cbor_value_copy_text_string` (or `_byte_string`) with `next` NULL:
    /// copy into `out`, appending a NUL if there is room.
    ///
    /// # Errors
    ///
    /// `OutOfMemory` if the string does not fit `out`, which may then hold
    /// the leading chunks; else a parse error.
    pub fn copy_string(&self, out: &mut [u8]) -> Result<usize, Error> {
        let mut next = *self;
        let buflen = out.len();
        let (copied, total) = self.iterate_string_chunks(buflen, &mut next, |at, chunk| {
            out[at..at + chunk.len()].copy_from_slice(chunk);
            true
        })?;
        if copied {
            Ok(total)
        } else {
            Err(Error::OutOfMemory)
        }
    }

    /// `cbor_value_map_find_value`: the value under the first text key equal
    /// to `key`, or an invalid iterator if there is none. Tags before a key
    /// or a value are skipped; keys of other types are stepped over.
    ///
    /// # Errors
    ///
    /// A parse error met while searching.
    pub fn map_find_value(&self, key: &[u8]) -> Result<Self, Error> {
        let mut element = self.enter_container()?;
        while !element.at_end() {
            element.skip_tag()?;
            if element.is_text_string() {
                let mut next = element;
                let (equals, _) =
                    element.iterate_string_chunks(key.len(), &mut next, |at, chunk| {
                        key.get(at..at + chunk.len()) == Some(chunk)
                    })?;
                element = next;
                if equals {
                    element.preparse_value()?;
                    return Ok(element);
                }
            } else {
                element.advance()?;
            }
            element.skip_tag()?;
            element.advance()?;
        }
        element.ty = INVALID;
        Ok(element)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(buf: &[u8]) -> Result<(), Error> {
        Value::init(buf)?.validate_basic()
    }

    #[test]
    fn well_formed_items_validate() {
        for buf in [
            &[0x00][..],
            &[0x1b, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0x63, b'a', b'b', b'c'],
            &[0x7f, 0x61, b'a', 0x60, 0xff],
            &[0xa1, 0x61, b'k', 0x80],
            &[0xbf, 0x61, b'k', 0x9f, 0xff, 0xff],
            &[0xc1, 0x01],
            &[0xfa, 0, 0, 0, 0],
            &[0xf8, 32],
        ] {
            assert_eq!(validate(buf), Ok(()), "{buf:02x?}");
        }
    }

    #[test]
    fn malformed_items_fail_with_tinycbors_code() {
        for (buf, err) in [
            (&[][..], Error::UnexpectedEof),
            (&[0x1c], Error::IllegalNumber),
            (&[0xfc], Error::UnknownType),
            (&[0xff], Error::UnexpectedBreak),
            (&[0x1f], Error::IllegalNumber),
            (&[0xf8, 31], Error::IllegalSimpleType),
            (&[0x62, b'a'], Error::UnexpectedEof),
            (&[0x7f, 0x41, b'a', 0xff], Error::IllegalType),
            (&[0xbf, 0x61, b'k', 0xff], Error::UnexpectedBreak),
            (&[0x9f, 0xc1, 0xff], Error::UnexpectedBreak),
            (&[0x81], Error::UnexpectedEof),
            (
                &[0x9b, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
                Error::DataTooLarge,
            ),
        ] {
            assert_eq!(validate(buf), Err(err), "{buf:02x?}");
        }
    }

    #[test]
    fn nesting_stops_at_the_eleventh_container() {
        let mut deep = [0x81u8; MAX_RECURSIONS + 1];
        deep[MAX_RECURSIONS] = 0x80;
        assert_eq!(validate(&deep), Err(Error::NestingTooDeep));
        assert_eq!(validate(&deep[1..]), Ok(()));
    }

    #[test]
    fn find_value_matches_whole_text_keys_only() {
        // {"ab": 1, 2: 3, "a": 4, "a": 5}
        let buf = [
            0xa4, 0x62, b'a', b'b', 0x01, 0x02, 0x03, 0x61, b'a', 0x04, 0x61, b'a', 0x05,
        ];
        let map = Value::init(&buf).unwrap();
        assert_eq!(map.map_find_value(b"a").unwrap().get_uint64(), 4);
        assert_eq!(map.map_find_value(b"ab").unwrap().get_uint64(), 1);
        assert!(!map.map_find_value(b"b").unwrap().is_valid());
    }
}
