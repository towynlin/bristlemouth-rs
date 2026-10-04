//! tinycbor 0.6's parser (`cborparser.c`), as bm_core's message decoders
//! drive it.
//!
//! [`cbor2::core::Decoder`] reads well-formed CBOR, but bm_core's decoders
//! (`bm_common_messages/*_msg.c`) are observable through tinycbor's error
//! codes, its iterator bookkeeping and its item counts: a tag does not count
//! as an item, a top-level item is validated and anything after it ignored,
//! and an unchecked `cbor_value_get_uint64` reads whatever argument the head
//! carries. [`Value`] is `CborValue`, field for field, and each method is the
//! C function it names.
//!
//! Where tinycbor's precondition is a `cbor_assert` the C decoders can reach,
//! the method returns [`CborError::Unreachable`] rather than guessing:
//! a debug build aborts there, and a release build has undefined behaviour.

/// `CborError`, the codes the ported decoders can return.
///
/// The discriminant is the C value, except [`Self::Unreachable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CborError {
    /// `CborErrorUnknownLength`.
    UnknownLength,
    /// `CborErrorAdvancePastEOF`.
    AdvancePastEof,
    /// `CborErrorGarbageAtEnd`.
    GarbageAtEnd,
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
    /// `CborErrorTooFewItems`, from closing an encoder's container.
    TooFewItems,
    /// `CborErrorDataTooLarge`.
    DataTooLarge,
    /// `CborErrorNestingTooDeep`.
    NestingTooDeep,
    /// `CborErrorUnsupportedType`.
    UnsupportedType,
    /// `CborErrorOutOfMemory`.
    OutOfMemory,
    /// No C value: tinycbor's precondition (`assert` or `cbor_assert`) does
    /// not hold here. A debug build of the C aborts; a release build is
    /// undefined.
    Unreachable,
}

impl CborError {
    /// The C `CborError` value, `None` for [`Self::Unreachable`].
    #[must_use]
    pub fn code(self) -> Option<i32> {
        Some(match self {
            Self::UnknownLength => 2,
            Self::AdvancePastEof => 3,
            Self::GarbageAtEnd => 256,
            Self::UnexpectedEof => 257,
            Self::UnexpectedBreak => 258,
            Self::UnknownType => 259,
            Self::IllegalType => 260,
            Self::IllegalNumber => 261,
            Self::IllegalSimpleType => 262,
            Self::ImproperValue => 519,
            Self::TooFewItems => 769,
            Self::DataTooLarge => 1024,
            Self::NestingTooDeep => 1025,
            Self::UnsupportedType => 1026,
            Self::OutOfMemory => i32::MIN,
            Self::Unreachable => return None,
        })
    }
}

/// `CborErrorNoMoreStringChunks`, which the string iteration consumes.
#[derive(Debug)]
enum Chunk {
    More(usize, usize),
    End,
}

/// `CBOR_PARSER_MAX_RECURSIONS`, as `bm-wire-sys/build.rs` and bm_core's
/// CMake define it.
pub const MAX_RECURSIONS: u32 = 10;

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

// `CborIteratorFlag_*`. 0x04 is both `NegativeInteger` and
// `BeforeFirstStringChunk`, as in the C.
const INTEGER_IS_64_BIT: u8 = 0x01;
const INTEGER_TOO_LARGE: u8 = 0x02;
const NEGATIVE_INTEGER: u8 = 0x04;
const BEFORE_FIRST_STRING_CHUNK: u8 = 0x04;
const ITERATING_STRING_CHUNKS: u8 = 0x08;
const UNKNOWN_LENGTH: u8 = 0x10;
const CONTAINER_IS_MAP: u8 = 0x20;
const NEXT_IS_MAP_KEY: u8 = 0x40;

const BREAK: u8 = 0xff;

/// `CborValue`: an iterator positioned on one item.
#[derive(Debug, Clone, Copy)]
pub struct Value<'a> {
    buf: &'a [u8],
    pos: usize,
    remaining: u32,
    flags: u8,
    ty: u8,
    extra: u16,
}

/// What [`Value::copy_string`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copied {
    /// The string's whole length, which the C returns in `*buflen`.
    pub total: usize,
    /// Whether every chunk was copied: `CborNoError` rather than
    /// `CborErrorOutOfMemory`.
    pub all: bool,
    /// Whether a NUL was written after the last byte.
    pub nul: bool,
}

fn is_fixed_type(ty: u8) -> bool {
    !matches!(ty, TEXT_STRING | BYTE_STRING | ARRAY | MAP)
}

fn read_be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |a, &b| (a << 8) | u64::from(b))
}

impl<'a> Value<'a> {
    /// `cbor_parser_init(buf, len, 0, ...)`.
    ///
    /// # Errors
    ///
    /// The first item's head is unreadable.
    pub fn parse(buf: &'a [u8]) -> Result<Self, CborError> {
        let mut it = Self {
            buf,
            pos: 0,
            remaining: 1,
            flags: 0,
            ty: INVALID,
            extra: 0,
        };
        it.preparse_value()?;
        Ok(it)
    }

    fn can_read(&self, n: usize) -> bool {
        self.buf.len().saturating_sub(self.pos) >= n
    }

    fn at(&self, offset: usize) -> &'a [u8] {
        self.buf.get(self.pos + offset..).unwrap_or(&[])
    }

    fn preparse_value(&mut self) -> Result<(), CborError> {
        self.ty = INVALID;
        self.flags &= CONTAINER_IS_MAP | NEXT_IS_MAP_KEY;
        let &descriptor = self.at(0).first().ok_or(CborError::UnexpectedEof)?;
        let ty = descriptor & 0xe0;
        let info = descriptor & 0x1f;
        self.ty = ty;
        self.extra = u16::from(info);

        if info > 27 {
            if info != 31 {
                return Err(if ty == SIMPLE {
                    CborError::UnknownType
                } else {
                    CborError::IllegalNumber
                });
            }
            if !is_fixed_type(ty) {
                self.flags |= UNKNOWN_LENGTH;
                return Ok(());
            }
            return Err(if ty == SIMPLE {
                CborError::UnexpectedBreak
            } else {
                CborError::IllegalNumber
            });
        }

        let needed = if info < 24 { 0 } else { 1usize << (info - 24) };
        if needed > 0 {
            if !self.can_read(needed + 1) {
                return Err(CborError::UnexpectedEof);
            }
            self.extra = 0;
            match needed {
                1 | 2 => self.extra = read_be(&self.at(1)[..needed]) as u16,
                _ => self.flags |= info & 3,
            }
        }

        match ty >> 5 {
            1 => {
                self.flags |= NEGATIVE_INTEGER;
                self.ty = INTEGER;
            }
            7 => match info {
                20 => {
                    self.extra = 0;
                    self.ty = BOOLEAN;
                }
                26 | 27 => {
                    self.flags |= INTEGER_TOO_LARGE;
                    self.ty = descriptor;
                }
                21 | 22 | 23 | 25 => self.ty = descriptor,
                24 if self.extra < 32 => {
                    self.ty = INVALID;
                    return Err(CborError::IllegalSimpleType);
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn preparse_next_value_nodecrement(&mut self) -> Result<(), CborError> {
        if self.remaining == u32::MAX && self.at(0).first() == Some(&BREAK) {
            let expecting_value =
                self.flags & CONTAINER_IS_MAP != 0 && self.flags & NEXT_IS_MAP_KEY != 0;
            if expecting_value || self.ty == TAG {
                return Err(CborError::UnexpectedBreak);
            }
            self.ty = INVALID;
            self.remaining = 0;
            self.flags |= UNKNOWN_LENGTH;
            return Ok(());
        }
        self.preparse_value()
    }

    fn preparse_next_value(&mut self) -> Result<(), CborError> {
        // Tags count neither towards a container's items nor as a map key.
        let counts = self.ty != TAG;
        if self.remaining != u32::MAX && counts {
            self.remaining = self.remaining.wrapping_sub(1);
            if self.remaining == 0 {
                self.ty = INVALID;
                self.flags &= !UNKNOWN_LENGTH;
                return Ok(());
            }
        }
        if counts {
            self.flags ^= NEXT_IS_MAP_KEY;
        }
        self.preparse_next_value_nodecrement()
    }

    /// `_cbor_value_extract_int64_helper`: the head's argument as tinycbor
    /// keeps it, for any type.
    ///
    /// This is what `cbor_value_get_uint64` returns with `NDEBUG` defined,
    /// whatever the item is: the argument for most heads, 0 for `false`, 31
    /// for an indefinite length, and the bits of a 4- or 8-byte float.
    #[must_use]
    pub fn extract(&self) -> u64 {
        if self.flags & INTEGER_TOO_LARGE == 0 {
            return u64::from(self.extra);
        }
        let width = if self.flags & INTEGER_IS_64_BIT != 0 {
            8
        } else {
            4
        };
        read_be(&self.at(1)[..width])
    }

    fn extract_number_and_advance(&mut self) -> u64 {
        let v = self.extract();
        let info = self.at(0)[0] & 0x1f;
        let needed = if info < 24 { 0 } else { 1usize << (info - 24) };
        self.pos += needed + 1;
        v
    }

    fn advance_internal(&mut self) -> Result<(), CborError> {
        self.extract_number_and_advance();
        self.preparse_next_value()
    }

    fn advance_recursive(&mut self, nesting: u32) -> Result<(), CborError> {
        if is_fixed_type(self.ty) {
            return self.advance_internal();
        }
        if !self.is_container() {
            let (_, next) = self.iterate_string(usize::MAX, |_, _| {})?;
            *self = next;
            return Ok(());
        }
        if nesting == 0 {
            return Err(CborError::NestingTooDeep);
        }
        let mut recursed = self.enter_container()?;
        while !recursed.at_end() {
            recursed.advance_recursive(nesting - 1)?;
        }
        self.leave_container(&recursed)
    }

    /// `cbor_value_advance`: step over this item, containers and strings
    /// included, validating as it goes.
    ///
    /// # Errors
    ///
    /// As the C.
    pub fn advance(&mut self) -> Result<(), CborError> {
        if self.ty == INVALID {
            return Err(CborError::Unreachable);
        }
        if self.remaining == 0 {
            return Err(CborError::AdvancePastEof);
        }
        self.advance_recursive(MAX_RECURSIONS)
    }

    /// `cbor_value_validate_basic`: advance a copy.
    ///
    /// Only this item is read. At the top level whatever follows it is never
    /// looked at.
    ///
    /// # Errors
    ///
    /// As [`Self::advance`].
    pub fn validate_basic(&self) -> Result<(), CborError> {
        let mut copy = *self;
        copy.advance()
    }

    /// `cbor_value_enter_container`.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if this is not a map or an array; otherwise
    /// as the C.
    pub fn enter_container(&self) -> Result<Self, CborError> {
        if !self.is_container() {
            return Err(CborError::Unreachable);
        }
        let mut recursed = *self;
        if self.flags & UNKNOWN_LENGTH != 0 {
            recursed.remaining = u32::MAX;
            recursed.pos += 1;
        } else {
            let len = recursed.extract_number_and_advance();
            recursed.remaining = len as u32;
            if u64::from(recursed.remaining) != len || len == u64::from(u32::MAX) {
                return Err(CborError::DataTooLarge);
            }
            if recursed.ty == MAP {
                if recursed.remaining > u32::MAX / 2 {
                    return Err(CborError::DataTooLarge);
                }
                recursed.remaining *= 2;
            }
            if len == 0 {
                recursed.ty = INVALID;
                return Ok(recursed);
            }
        }
        recursed.flags = recursed.ty & CONTAINER_IS_MAP;
        recursed.preparse_next_value_nodecrement()?;
        Ok(recursed)
    }

    /// `cbor_value_leave_container`.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if `recursed` is not at its end, which
    /// tinycbor asserts; otherwise as the C.
    pub fn leave_container(&mut self, recursed: &Self) -> Result<(), CborError> {
        if !self.is_container() || recursed.ty != INVALID {
            return Err(CborError::Unreachable);
        }
        self.pos = recursed.pos;
        if recursed.flags & UNKNOWN_LENGTH != 0 {
            self.pos += 1;
        }
        self.preparse_next_value()
    }

    /// `cbor_value_get_next_byte`, as an offset into the buffer.
    #[must_use]
    pub fn next_byte(&self) -> usize {
        self.pos
    }

    /// `cbor_value_at_end`.
    #[must_use]
    pub fn at_end(&self) -> bool {
        self.remaining == 0
    }

    /// `cbor_value_is_container`.
    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self.ty, ARRAY | MAP)
    }

    /// `cbor_value_is_map`.
    #[must_use]
    pub fn is_map(&self) -> bool {
        self.ty == MAP
    }

    /// `cbor_value_is_text_string`.
    #[must_use]
    pub fn is_text_string(&self) -> bool {
        self.ty == TEXT_STRING
    }

    /// `cbor_value_is_byte_string`.
    #[must_use]
    pub fn is_byte_string(&self) -> bool {
        self.ty == BYTE_STRING
    }

    /// `cbor_value_is_unsigned_integer`.
    #[must_use]
    pub fn is_unsigned_integer(&self) -> bool {
        self.ty == INTEGER && self.flags & NEGATIVE_INTEGER == 0
    }

    /// `cbor_value_is_float`: the 5-byte form only (divergence #43).
    #[must_use]
    pub fn is_float(&self) -> bool {
        self.ty == FLOAT
    }

    /// `cbor_value_is_double`: the 9-byte form only.
    #[must_use]
    pub fn is_double(&self) -> bool {
        self.ty == DOUBLE
    }

    /// `cbor_value_is_tag`.
    #[must_use]
    pub fn is_tag(&self) -> bool {
        self.ty == TAG
    }

    /// `cbor_value_skip_tag`: step over tags to the item they qualify.
    ///
    /// # Errors
    ///
    /// [`CborError::AdvancePastEof`] at the end of a container, else what
    /// parsing the next item returns.
    pub fn skip_tag(&mut self) -> Result<(), CborError> {
        while self.is_tag() {
            // `cbor_value_advance_fixed`.
            if self.remaining == 0 {
                return Err(CborError::AdvancePastEof);
            }
            self.advance_internal()?;
        }
        Ok(())
    }

    /// `cbor_value_get_string_length`.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if this is not a string;
    /// [`CborError::UnknownLength`] for a chunked one;
    /// [`CborError::DataTooLarge`] if the length does not fit a `usize`.
    pub fn string_length(&self) -> Result<usize, CborError> {
        if !self.is_text_string() && !self.is_byte_string() {
            return Err(CborError::Unreachable);
        }
        if !self.is_length_known() {
            return Err(CborError::UnknownLength);
        }
        usize::try_from(self.extract()).map_err(|_| CborError::DataTooLarge)
    }

    /// `cbor_value_map_find_value`: the value under the first text key whose
    /// bytes are exactly `key`, or an iterator that is not
    /// [`valid`](Self::is_valid) if there is none. Tags before a key or a
    /// value are skipped; keys of other types are stepped over.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if this is not a map; a parse error met
    /// while searching.
    pub fn map_find_value(&self, key: &[u8]) -> Result<Self, CborError> {
        if !self.is_map() {
            return Err(CborError::Unreachable);
        }
        let mut element = self.enter_container()?;
        while !element.at_end() {
            element.skip_tag()?;
            if element.is_text_string() {
                // `iterate_string_chunks` with `iterate_memcmp` over
                // `strlen(key)` bytes: equal only if every chunk fits and
                // matches and the total is exactly that length.
                let mut equal = true;
                let (copied, next) = element.iterate_string(key.len(), |at, chunk| {
                    equal &= key[at..at + chunk.len()] == *chunk;
                })?;
                element = next;
                if equal && copied.all && copied.total == key.len() {
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

    /// `cbor_value_is_valid`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.ty != INVALID
    }

    /// `cbor_value_get_int64` with `NDEBUG` defined: [`Self::extract`],
    /// negated as a negative integer if the flag says so, for any type.
    ///
    /// The C's `-*result - 1` overflows for an argument of `1 << 63`
    /// (divergence #41); this wraps.
    #[must_use]
    pub fn get_int64(&self) -> i64 {
        let v = self.extract() as i64;
        if self.flags & NEGATIVE_INTEGER != 0 {
            v.wrapping_neg().wrapping_sub(1)
        } else {
            v
        }
    }

    /// `cbor_value_get_float` with `NDEBUG` defined: the low 32 bits of the
    /// head's 4- or 8-byte argument, for any type.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] for a head whose argument fits in 16 bits:
    /// `_cbor_value_decode_int64_internal`'s `cbor_assert` fails.
    pub fn get_float(&self) -> Result<u32, CborError> {
        if self.flags & INTEGER_TOO_LARGE == 0 {
            return Err(CborError::Unreachable);
        }
        Ok(self.extract() as u32)
    }

    /// This iterator's state over `buf` instead of its own buffer: what a
    /// `CborValue` reads after the bytes under it have been overwritten.
    #[must_use]
    pub fn rebind<'b>(&self, buf: &'b [u8]) -> Value<'b> {
        Value {
            buf,
            pos: self.pos,
            remaining: self.remaining,
            flags: self.flags,
            ty: self.ty,
            extra: self.extra,
        }
    }

    /// `cbor_value_is_length_known`.
    #[must_use]
    pub fn is_length_known(&self) -> bool {
        self.flags & UNKNOWN_LENGTH == 0
    }

    /// `cbor_value_get_map_length`.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if this is not a map;
    /// [`CborError::UnknownLength`] for an indefinite length;
    /// [`CborError::DataTooLarge`] if the length does not fit a `usize`.
    pub fn map_length(&self) -> Result<usize, CborError> {
        if !self.is_map() {
            return Err(CborError::Unreachable);
        }
        if !self.is_length_known() {
            return Err(CborError::UnknownLength);
        }
        usize::try_from(self.extract()).map_err(|_| CborError::DataTooLarge)
    }

    /// `get_string_chunk`: the next chunk's body as a range of `buf`.
    fn string_chunk(&mut self) -> Result<Chunk, CborError> {
        if self.is_length_known() && self.flags & BEFORE_FIRST_STRING_CHUNK == 0 {
            return Ok(Chunk::End);
        }
        let &descriptor = self.at(0).first().ok_or(CborError::UnexpectedEof)?;
        if descriptor == BREAK {
            return Ok(Chunk::End);
        }
        if descriptor & 0xe0 != self.ty {
            return Err(CborError::IllegalType);
        }
        let info = descriptor & 0x1f;
        let (len, offset) = if info < 24 {
            (usize::from(info), 1)
        } else if info > 27 {
            return Err(CborError::IllegalNumber);
        } else {
            let n = 1usize << (info - 24);
            if !self.can_read(1 + n) {
                return Err(CborError::UnexpectedEof);
            }
            let val = read_be(&self.at(1)[..n]);
            let len = usize::try_from(val).map_err(|_| CborError::DataTooLarge)?;
            (len, n + 1)
        };
        // `transfer_string`.
        self.pos += offset;
        if !self.can_read(len) {
            return Err(CborError::UnexpectedEof);
        }
        let start = self.pos;
        self.pos += len;
        self.flags &= !BEFORE_FIRST_STRING_CHUNK;
        Ok(Chunk::More(start, len))
    }

    /// `iterate_string_chunks` into a buffer of `buflen` bytes, calling
    /// `copy(offset, chunk)` for each chunk the C would `memcpy`. Returns
    /// what was copied and the iterator after the string.
    fn iterate_string(
        &self,
        buflen: usize,
        mut copy: impl FnMut(usize, &'a [u8]),
    ) -> Result<(Copied, Self), CborError> {
        if !self.is_text_string() && !self.is_byte_string() {
            return Err(CborError::Unreachable);
        }
        let mut next = *self;
        next.flags |= ITERATING_STRING_CHUNKS | BEFORE_FIRST_STRING_CHUNK;
        if !next.is_length_known() {
            next.pos += 1;
        }
        let mut total = 0usize;
        let mut all = true;
        while let Chunk::More(start, len) = next.string_chunk()? {
            let new_total = total.checked_add(len).ok_or(CborError::DataTooLarge)?;
            if all && buflen >= new_total {
                copy(total, &self.buf[start..start + len]);
            } else {
                all = false;
            }
            total = new_total;
        }
        let nul = all && buflen > total;
        // `_cbor_value_finish_string_iteration`.
        if !next.is_length_known() {
            next.pos += 1;
        }
        next.preparse_next_value()?;
        Ok((Copied { total, all, nul }, next))
    }

    /// `_cbor_value_copy_string(value, buffer, &buflen, NULL)` with a
    /// `buflen`-byte buffer, calling `copy(offset, chunk)` for each chunk
    /// that is written to it. A NUL is written after the last byte when
    /// [`Copied::nul`] says so.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if this is not a string; a parse error
    /// in its chunks. A string that does not fit is not an error here:
    /// [`Copied::all`] is false where the C returns `CborErrorOutOfMemory`.
    pub fn copy_string(
        &self,
        buflen: usize,
        copy: impl FnMut(usize, &'a [u8]),
    ) -> Result<Copied, CborError> {
        self.iterate_string(buflen, copy).map(|(copied, _)| copied)
    }

    /// The string this iterator is on, for reading its chunks later.
    ///
    /// # Errors
    ///
    /// [`CborError::Unreachable`] if this is not a string.
    pub fn string(&self) -> Result<CborString<'a>, CborError> {
        if !self.is_text_string() && !self.is_byte_string() {
            return Err(CborError::Unreachable);
        }
        let copied = self.copy_string(usize::MAX, |_, _| {})?;
        Ok(CborString {
            at: *self,
            len: copied.total,
        })
    }
}

/// A text or byte string item that has parsed cleanly, definite or chunked,
/// borrowed from the buffer it was decoded from.
#[derive(Debug, Clone, Copy)]
pub struct CborString<'a> {
    at: Value<'a>,
    len: usize,
}

impl<'a> CborString<'a> {
    /// The string's length: its chunks' lengths summed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the string is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Call `f` with each chunk's body, in order. A definite-length string
    /// is one chunk.
    pub fn for_each_chunk(&self, mut f: impl FnMut(&'a [u8])) {
        // Parsed once already, so it cannot fail now.
        let _ = self.at.copy_string(usize::MAX, |_, chunk| f(chunk));
    }

    /// Copy the string into `out`, returning its length, or `None` if `out`
    /// is too short.
    #[must_use]
    pub fn copy_to(&self, out: &mut [u8]) -> Option<usize> {
        let dst = out.get_mut(..self.len)?;
        let mut at = 0;
        self.for_each_chunk(|chunk| {
            dst[at..at + chunk.len()].copy_from_slice(chunk);
            at += chunk.len();
        });
        Some(self.len)
    }

    /// Whether the string's bytes are `bytes`.
    #[must_use]
    pub fn eq_bytes(&self, bytes: &[u8]) -> bool {
        if bytes.len() != self.len {
            return false;
        }
        let mut at = 0;
        let mut eq = true;
        self.for_each_chunk(|chunk| {
            eq &= bytes[at..at + chunk.len()] == *chunk;
            at += chunk.len();
        });
        eq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_top_level_item_is_validated_and_what_follows_ignored() {
        let v = Value::parse(&[0x01, 0xff, 0xff]).unwrap();
        assert_eq!(v.validate_basic(), Ok(()));
        assert_eq!(
            Value::parse(&[0xff]).unwrap_err(),
            CborError::UnexpectedBreak
        );
        assert_eq!(Value::parse(&[0x1c]).unwrap_err(), CborError::IllegalNumber);
        assert_eq!(Value::parse(&[0xfc]).unwrap_err(), CborError::UnknownType);
        assert_eq!(
            Value::parse(&[0xf8, 0x1f]).unwrap_err(),
            CborError::IllegalSimpleType
        );
        assert_eq!(
            Value::parse(&[0x19, 0x01]).unwrap_err(),
            CborError::UnexpectedEof
        );
    }

    #[test]
    fn extract_reads_the_argument_of_any_head() {
        let cases: &[(&[u8], u64)] = &[
            (&[0x17], 23),
            (&[0x18, 0xab], 0xab),
            (&[0x39, 0x01, 0x02], 0x0102),
            (&[0x1a, 1, 2, 3, 4], 0x0102_0304),
            (&[0x1b, 1, 2, 3, 4, 5, 6, 7, 8], 0x0102_0304_0506_0708),
            (&[0x63, b'a', b'b', b'c'], 3),
            (&[0x7f, 0xff], 31),
            (&[0xf4], 0),
            (&[0xf5], 21),
            (&[0xfa, 0x3f, 0x80, 0, 0], 0x3f80_0000),
            (&[0xc6, 0x00], 6),
        ];
        for (bytes, want) in cases {
            assert_eq!(
                Value::parse(bytes).unwrap().extract(),
                *want,
                "{bytes:02x?}"
            );
        }
    }

    #[test]
    fn a_tag_is_not_an_item() {
        // [tag(1, 0), 2]: after the tag the iterator sits on 0, not 2.
        let v = Value::parse(&[0x82, 0xc1, 0x00, 0x02]).unwrap();
        let mut r = v.enter_container().unwrap();
        r.advance().unwrap();
        assert!(r.is_unsigned_integer());
        assert_eq!(r.extract(), 0);
        r.advance().unwrap();
        r.advance().unwrap();
        assert!(r.at_end());
    }

    #[test]
    fn nesting_is_limited_to_ten_containers() {
        let mut ten = [0x81; 11];
        ten[10] = 0x00;
        assert_eq!(Value::parse(&ten).unwrap().validate_basic(), Ok(()));
        let mut eleven = [0x81; 12];
        eleven[11] = 0x00;
        assert_eq!(
            Value::parse(&eleven).unwrap().validate_basic(),
            Err(CborError::NestingTooDeep)
        );
    }

    #[test]
    fn chunked_strings_copy_until_one_does_not_fit() {
        // (_ "ab" "cd")
        let bytes = [0x7f, 0x62, b'a', b'b', 0x62, b'c', b'd', 0xff];
        let v = Value::parse(&bytes).unwrap();
        let mut out = [0u8; 8];
        let copied = v.copy_string(3, |at, c| out[at..at + c.len()].copy_from_slice(c));
        assert_eq!(
            copied,
            Ok(Copied {
                total: 4,
                all: false,
                nul: false
            })
        );
        assert_eq!(&out[..2], b"ab");
        let s = v.string().unwrap();
        assert_eq!(s.len(), 4);
        assert!(s.eq_bytes(b"abcd"));
    }
}
