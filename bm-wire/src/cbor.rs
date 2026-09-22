//! A CBOR reader and writer, ported from the tinycbor bm_core vendors.
//!
//! bm_core stores every config value as CBOR (`bcmp/configuration.c`) and
//! publishes a whole partition as a CBOR map (`middleware/cbor_service_helper.c`).
//! `bm-wire` may not take a dependency, so the subset those two files reach is
//! ported here rather than pulled in.
//!
//! # What is supported
//!
//! Encoding, matching the encoder calls bm_core makes. `open_array` is the
//! one entry with no bm_core caller; it shares `create_container` with
//! `open_map`, which does.
//!
//! | This module | tinycbor |
//! |---|---|
//! | [`Encoder::new`] | `cbor_encoder_init` |
//! | [`Encoder::encode_uint`] | `cbor_encode_uint` |
//! | [`Encoder::encode_int`] | `cbor_encode_int` |
//! | [`Encoder::encode_float`] | `cbor_encode_float` |
//! | [`Encoder::encode_text`] | `cbor_encode_text_string` |
//! | [`Encoder::encode_bytes`] | `cbor_encode_byte_string` |
//! | [`Encoder::open_map`] / [`Encoder::open_array`] | `cbor_encoder_create_map` / `_array` |
//! | [`Encoder::close_container`] | `cbor_encoder_close_container` |
//! | [`Encoder::buffer_size`] | `cbor_encoder_get_buffer_size` |
//! | [`Encoder::extra_bytes_needed`] | `cbor_encoder_get_extra_bytes_needed` |
//!
//! Decoding is the head of one item — [`parse`] is `cbor_parser_init`, whose
//! only work is `preparse_value` on the first item — plus the accessors
//! `bcmp/configuration.c` calls on it: the type predicates, `get_uint64`,
//! `get_int64`, `get_float`, `get_string_length`, `get_array_length`,
//! `get_map_length` and the two `copy_*_string` calls. Decoder input is
//! attacker-supplied — a `ConfigSet` (`0xA2`) body is stored verbatim — so
//! every major type is classified, including the ones bm_core then rejects,
//! and indefinite-length strings are iterated chunk by chunk.
//!
//! # What is not supported
//!
//! | Left out | Why |
//! |---|---|
//! | Iterating into a container (`cbor_value_enter_container`, `cbor_value_advance`) | bm_core parses one top-level item and stops. `CBOR_PARSER_MAX_RECURSIONS=10` is therefore unreachable, and nothing here recurses. |
//! | Indefinite-length **containers** in [`Encoder`] | `cbor_encoder_create_map` is only ever called with a definite count. Indefinite-length *strings* are still decoded. |
//! | Half and double floats, booleans, null, undefined, simple values, tags | Classified by [`parse`] because `cbor_type_to_config` must reject them, but there is no accessor: bm_core never encodes one and never reads one back. |
//! | Canonical/strict validation (`cbor_value_validate`) | `cborvalidation.c` is built but bm_core calls nothing in it. |
//! | The external-source reader and the writer callback | `CBOR_PARSER_READER_CONTROL` and `CBOR_ENCODER_WRITER_CONTROL` are at their defaults, so both are compiled out. |
//!
//! # Fidelity
//!
//! tinycbor's parser is lazy: `cbor_parser_init` validates the *head* of the
//! first item and nothing else. A declared string length longer than the
//! buffer, an array whose elements are absent, trailing garbage — none of
//! these is an error here, and [`Value::string_length`] reports the declared
//! length whether or not the bytes exist. That laziness is load-bearing, since
//! it decides which `ConfigSet` bodies a node accepts.

/// Maximum container nesting [`Encoder`] will open.
///
/// tinycbor has no limit — each container is a separate `CborEncoder` on the
/// caller's stack — so this is a ceiling the port adds to stay alloc-free.
/// bm_core opens one map at one level, so it is well out of reach; the
/// differential harness asserts no run ever reaches it.
pub const MAX_NESTING: usize = 8;

/// The subset of `CborError` this module can produce.
///
/// The C constant each one stands for is named, because the differential
/// harness maps between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Error {
    /// `CborErrorUnknownLength` — a length was asked of an indefinite-length
    /// string, array or map.
    UnknownLength,
    /// `CborErrorUnexpectedEOF` — the buffer ended inside an item's head, or
    /// inside a string's declared bytes.
    UnexpectedEof,
    /// `CborErrorUnexpectedBreak` — a break byte (`0xff`) where a value was
    /// expected.
    UnexpectedBreak,
    /// `CborErrorUnknownType` — additional information 28, 29 or 30 on major
    /// type 7.
    UnknownType,
    /// `CborErrorIllegalType` — a chunk of an indefinite-length string whose
    /// major type is not the string's own.
    IllegalType,
    /// `CborErrorIllegalNumber` — additional information 28, 29 or 30, or an
    /// indefinite length on a type that cannot have one.
    IllegalNumber,
    /// `CborErrorIllegalSimpleType` — a simple value below 32 encoded in the
    /// two-byte form.
    IllegalSimpleType,
    /// `CborErrorTooManyItems` — a container was closed with more items than
    /// it declared.
    TooManyItems,
    /// `CborErrorTooFewItems` — a container was closed with fewer items than
    /// it declared.
    TooFewItems,
    /// `CborErrorDataTooLarge` — a length that does not fit a `usize`. Only
    /// reachable where `usize` is narrower than 64 bits, which is every
    /// firmware target and no host one.
    DataTooLarge,
    /// `CborErrorOutOfMemory` — the output buffer was too small. The encoder
    /// keeps counting, so [`Encoder::extra_bytes_needed`] says by how much.
    OutOfMemory,
    /// No C counterpart: [`MAX_NESTING`] containers are already open.
    NestingTooDeep,
    /// No C counterpart: [`Encoder::close_container`] with nothing open.
    NotInContainer,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::UnknownLength => "indefinite length",
            Self::UnexpectedEof => "unexpected end of buffer",
            Self::UnexpectedBreak => "unexpected break",
            Self::UnknownType => "unknown type",
            Self::IllegalType => "illegal type",
            Self::IllegalNumber => "illegal number",
            Self::IllegalSimpleType => "illegal simple type",
            Self::TooManyItems => "too many items",
            Self::TooFewItems => "too few items",
            Self::DataTooLarge => "data too large",
            Self::OutOfMemory => "out of memory",
            Self::NestingTooDeep => "nesting too deep",
            Self::NotInContainer => "no container is open",
        })
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// The type tinycbor assigns an item, with `CborType`'s own values.
///
/// The discriminants are `CborType`'s, so a comparator can compare them
/// against the C directly. Two of them are not what the first byte says:
/// `0xf4` (false) and `0xf5` (true) both classify as [`Type::Boolean`], and
/// major type 1 classifies as [`Type::Integer`] with the sign kept in a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Type {
    /// `CborIntegerType`. Both major type 0 and major type 1, once
    /// `preparse_value`'s fixup has run.
    Integer = 0x00,
    /// Major type 1 *before* that fixup, which is not a `CborType` constant
    /// at all.
    ///
    /// `preparse_value` assigns the masked first byte before it can fail, and
    /// only rewrites major type 1 to [`Type::Integer`] on the way out. A
    /// negative integer whose head is malformed or truncated is therefore
    /// left holding `0x20`: `cbor_value_get_type` returns it, every predicate
    /// is false, and `cbor_value_is_valid` is true. See divergence #40.
    RawNegative = 0x20,
    /// `CborByteStringType`.
    ByteString = 0x40,
    /// `CborTextStringType`.
    TextString = 0x60,
    /// `CborArrayType`.
    Array = 0x80,
    /// `CborMapType`.
    Map = 0xa0,
    /// `CborTagType`.
    Tag = 0xc0,
    /// `CborSimpleType` — a major type 7 value that is not one of the named
    /// ones below.
    Simple = 0xe0,
    /// `CborBooleanType`. Both `0xf4` and `0xf5`.
    Boolean = 0xf5,
    /// `CborNullType`.
    Null = 0xf6,
    /// `CborUndefinedType`.
    Undefined = 0xf7,
    /// `CborHalfFloatType`.
    HalfFloat = 0xf9,
    /// `CborFloatType`.
    Float = 0xfa,
    /// `CborDoubleType`.
    Double = 0xfb,
    /// `CborInvalidType` — no value. [`Value::is_valid`] is false for this and
    /// only this.
    Invalid = 0xff,
}

impl Type {
    /// This type's `CborType` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The type a major-type byte (the first byte masked with `0xe0`) maps to
    /// before `preparse_value`'s major-type-1 and major-type-7 fixups.
    const fn from_major(major: u8) -> Self {
        match major {
            0x00 => Self::Integer,
            0x20 => Self::RawNegative,
            0x40 => Self::ByteString,
            0x60 => Self::TextString,
            0x80 => Self::Array,
            0xa0 => Self::Map,
            0xc0 => Self::Tag,
            _ => Self::Simple,
        }
    }
}

/// tinycbor's `is_fixed_type`: everything but the four types that carry a
/// length. Only these four may be given an indefinite length.
const fn is_fixed_major(major: u8) -> bool {
    !matches!(major, 0x40 | 0x60 | 0x80 | 0xa0)
}

/// The head of one CBOR item, as `cbor_parser_init` leaves `CborValue`.
///
/// This holds no cursor beyond the item's own bytes: bm_core never advances
/// past the first item, so neither does this.
#[derive(Debug, Clone, Copy)]
pub struct Value<'a> {
    /// The buffer from this item's first byte onwards.
    buf: &'a [u8],
    ty: Type,
    /// `_cbor_value_extract_int64_helper`'s result. tinycbor splits this
    /// between a 16-bit `extra` and a re-read of the 32- or 64-bit argument;
    /// the value both paths yield is the same.
    extra: u64,
    /// `CborIteratorFlag_NegativeInteger`.
    negative: bool,
    /// The complement of `cbor_value_is_length_known`.
    unknown_length: bool,
    /// Bytes in the head: 1, plus the 1, 2, 4 or 8 argument bytes.
    head_len: usize,
}

/// What `cbor_parser_init` returns: an error, a value, or both.
///
/// tinycbor leaves `CborValue::type` set to the major type on four of its five
/// preparse errors, so `cbor_value_is_valid` reports the value as valid even
/// though `cbor_parser_init` failed. `set_config_cbor` tests both, in that
/// order; anything that tests only one inherits the difference.
#[derive(Debug, Clone, Copy)]
pub struct Parsed<'a> {
    /// What `cbor_parser_init` returned.
    pub result: Result<(), Error>,
    /// The preparsed head, populated as far as tinycbor got.
    pub value: Value<'a>,
}

/// Preparse the first item in `buf`, exactly as `cbor_parser_init` does.
///
/// Only the item's head is read. Its payload, and anything after it, is left
/// for the accessors — see the module docs on laziness.
#[must_use]
pub fn parse(buf: &[u8]) -> Parsed<'_> {
    let mut value = Value {
        buf,
        ty: Type::Invalid,
        extra: 0,
        negative: false,
        unknown_length: false,
        head_len: 1,
    };

    let Some(&descriptor) = buf.first() else {
        return Parsed {
            result: Err(Error::UnexpectedEof),
            value,
        };
    };

    let major = descriptor & 0xe0;
    let low = descriptor & 0x1f;
    // tinycbor assigns both before any error return, which is what leaves a
    // failed parse looking valid.
    value.ty = Type::from_major(major);
    value.extra = u64::from(low);

    if low > 27 {
        let err = if low == 31 {
            if !is_fixed_major(major) {
                value.unknown_length = true;
                return Parsed {
                    result: Ok(()),
                    value,
                };
            }
            // A break byte is major type 7; anything else with additional
            // information 31 is a length where none is allowed.
            if major == 0xe0 {
                Error::UnexpectedBreak
            } else {
                Error::IllegalNumber
            }
        } else if major == 0xe0 {
            Error::UnknownType
        } else {
            Error::IllegalNumber
        };
        return Parsed {
            result: Err(err),
            value,
        };
    }

    let arg_len = if low < 24 { 0 } else { 1usize << (low - 24) };
    if arg_len != 0 {
        if buf.len() < arg_len + 1 {
            // tinycbor returns here with `extra` still holding the additional
            // information and `type` still the major type, so the major-type-1
            // and major-type-7 fixups below never run.
            return Parsed {
                result: Err(Error::UnexpectedEof),
                value,
            };
        }
        let mut arg = 0u64;
        for &byte in &buf[1..=arg_len] {
            arg = (arg << 8) | u64::from(byte);
        }
        value.extra = arg;
        value.head_len = 1 + arg_len;
    }

    if major == 0x20 {
        value.negative = true;
        value.ty = Type::Integer;
    } else if major == 0xe0 {
        match low {
            20 => {
                // False. tinycbor rewrites the value to 0 and the type to
                // CborBooleanType, which is true's byte.
                value.extra = 0;
                value.ty = Type::Boolean;
            }
            21 => value.ty = Type::Boolean,
            22 => value.ty = Type::Null,
            23 => value.ty = Type::Undefined,
            25 => value.ty = Type::HalfFloat,
            26 => value.ty = Type::Float,
            27 => value.ty = Type::Double,
            // A simple value in the next byte. Below 32 it had a one-byte
            // form, so the two-byte form is rejected.
            24 if value.extra < 32 => {
                value.ty = Type::Invalid;
                return Parsed {
                    result: Err(Error::IllegalSimpleType),
                    value,
                };
            }
            _ => {}
        }
    }

    Parsed {
        result: Ok(()),
        value,
    }
}

/// What a `copy_*_string` call did with the caller's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// The whole string fit.
    Fits {
        /// Its length, not counting any NUL.
        len: usize,
        /// Whether a NUL was written after the last byte. tinycbor adds one
        /// whenever a byte is spare, for byte strings as well as text, and
        /// never counts it in `len`.
        nul_terminated: bool,
    },
    /// The buffer was too small: `CborErrorOutOfMemory`. tinycbor still
    /// reports the total length, and may have written earlier chunks of an
    /// indefinite-length string before running out.
    TooSmall {
        /// The string's total length.
        len: usize,
    },
}

impl Copied {
    /// The string's total length, which tinycbor reports either way.
    #[must_use]
    pub const fn len(self) -> usize {
        match self {
            Self::Fits { len, .. } | Self::TooSmall { len } => len,
        }
    }

    /// Whether the string is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }
}

impl<'a> Value<'a> {
    /// `cbor_value_is_valid`.
    ///
    /// False only for [`Type::Invalid`]. A [`parse`] that returned an error
    /// can still be valid by this test — see [`Parsed`].
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        !matches!(self.ty, Type::Invalid)
    }

    /// `cbor_value_get_type`.
    #[must_use]
    pub const fn ty(&self) -> Type {
        self.ty
    }

    /// `cbor_value_is_integer` — either sign.
    #[must_use]
    pub const fn is_integer(&self) -> bool {
        matches!(self.ty, Type::Integer)
    }

    /// `cbor_value_is_unsigned_integer`.
    #[must_use]
    pub const fn is_unsigned_integer(&self) -> bool {
        self.is_integer() && !self.negative
    }

    /// `cbor_value_is_negative_integer`.
    #[must_use]
    pub const fn is_negative_integer(&self) -> bool {
        self.is_integer() && self.negative
    }

    /// `cbor_value_is_byte_string`.
    #[must_use]
    pub const fn is_byte_string(&self) -> bool {
        matches!(self.ty, Type::ByteString)
    }

    /// `cbor_value_is_text_string`.
    #[must_use]
    pub const fn is_text_string(&self) -> bool {
        matches!(self.ty, Type::TextString)
    }

    /// `cbor_value_is_array`.
    #[must_use]
    pub const fn is_array(&self) -> bool {
        matches!(self.ty, Type::Array)
    }

    /// `cbor_value_is_map`.
    #[must_use]
    pub const fn is_map(&self) -> bool {
        matches!(self.ty, Type::Map)
    }

    /// `cbor_value_is_float` — single precision only, as the C predicate is.
    #[must_use]
    pub const fn is_float(&self) -> bool {
        matches!(self.ty, Type::Float)
    }

    /// `cbor_value_is_length_known`.
    #[must_use]
    pub const fn is_length_known(&self) -> bool {
        !self.unknown_length
    }

    /// `cbor_value_get_uint64`.
    ///
    /// `None` where the C asserts: this is not an unsigned integer.
    #[must_use]
    pub const fn get_uint64(&self) -> Option<u64> {
        if self.is_unsigned_integer() {
            Some(self.extra)
        } else {
            None
        }
    }

    /// `cbor_value_get_int64`.
    ///
    /// `None` where the C asserts: this is not an integer of either sign.
    ///
    /// A negative integer is returned as `-argument - 1`, computed in
    /// wrapping arithmetic because tinycbor computes it in `int64_t`, where
    /// an argument of `1 << 63` makes the negation overflow. That is
    /// undefined in C and both gcc and clang wrap, yielding [`i64::MAX`] for
    /// the one CBOR value `-(2^63) - 1`; see divergence #41.
    #[must_use]
    pub const fn get_int64(&self) -> Option<i64> {
        if !self.is_integer() {
            return None;
        }
        let raw = self.extra as i64;
        Some(if self.negative {
            raw.wrapping_neg().wrapping_sub(1)
        } else {
            raw
        })
    }

    /// `cbor_value_get_float`.
    ///
    /// `None` where the C asserts: this is not a single-precision float.
    #[must_use]
    pub const fn get_float(&self) -> Option<f32> {
        if self.is_float() {
            Some(f32::from_bits(self.extra as u32))
        } else {
            None
        }
    }

    /// `cbor_value_get_string_length`.
    ///
    /// The *declared* length. tinycbor does not check that the bytes are
    /// there, and neither does this.
    ///
    /// `None` where the C asserts: this is not a string.
    #[must_use]
    pub const fn string_length(&self) -> Option<Result<usize, Error>> {
        if self.is_byte_string() || self.is_text_string() {
            Some(self.declared_length())
        } else {
            None
        }
    }

    /// `cbor_value_get_array_length`. `None` where the C asserts.
    #[must_use]
    pub const fn array_length(&self) -> Option<Result<usize, Error>> {
        if self.is_array() {
            Some(self.declared_length())
        } else {
            None
        }
    }

    /// `cbor_value_get_map_length`. `None` where the C asserts.
    #[must_use]
    pub const fn map_length(&self) -> Option<Result<usize, Error>> {
        if self.is_map() {
            Some(self.declared_length())
        } else {
            None
        }
    }

    const fn declared_length(&self) -> Result<usize, Error> {
        if self.unknown_length {
            return Err(Error::UnknownLength);
        }
        let len = self.extra as usize;
        // tinycbor's `*length != v` check, which only ever fires where usize
        // is narrower than 64 bits.
        if len as u64 == self.extra {
            Ok(len)
        } else {
            Err(Error::DataTooLarge)
        }
    }

    /// `cbor_value_copy_text_string(value, out, &out.len(), NULL)`.
    ///
    /// `None` where the C asserts: this is not a text string. No UTF-8
    /// validation is done, because tinycbor does none either.
    pub fn copy_text_string(&self, out: &mut [u8]) -> Option<Result<Copied, Error>> {
        if self.is_text_string() {
            Some(self.copy_string(out))
        } else {
            None
        }
    }

    /// `cbor_value_copy_byte_string(value, out, &out.len(), NULL)`.
    ///
    /// `None` where the C asserts: this is not a byte string.
    pub fn copy_byte_string(&self, out: &mut [u8]) -> Option<Result<Copied, Error>> {
        if self.is_byte_string() {
            Some(self.copy_string(out))
        } else {
            None
        }
    }

    /// `iterate_string_chunks` with `iterate_memcpy`, which is all
    /// `_cbor_value_copy_string` is.
    fn copy_string(&self, out: &mut [u8]) -> Result<Copied, Error> {
        // `_cbor_value_begin_string_iteration`: an indefinite-length string
        // starts one byte in, past its own head.
        let mut cursor = if self.unknown_length { 1 } else { 0 };
        let mut before_first_chunk = true;
        let mut total = 0usize;
        let mut fits = true;

        loop {
            let chunk = match self.chunk_at(cursor, before_first_chunk) {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(err) => return Err(err),
            };
            before_first_chunk = false;

            // `transfer_string` runs before the running total is checked for
            // overflow, so a chunk claiming more bytes than the buffer holds
            // is end-of-buffer rather than too-large however long it claims.
            let start = cursor + chunk.head_len;
            let Some(end) = start
                .checked_add(chunk.len)
                .filter(|e| *e <= self.buf.len())
            else {
                return Err(Error::UnexpectedEof);
            };
            let Some(new_total) = total.checked_add(chunk.len) else {
                return Err(Error::DataTooLarge);
            };

            if fits && out.len() >= new_total {
                out[total..new_total].copy_from_slice(&self.buf[start..end]);
            } else {
                fits = false;
            }
            total = new_total;
            cursor = end;
        }

        if !fits {
            return Ok(Copied::TooSmall { len: total });
        }
        // A NUL whenever a byte is spare, uncounted -- for byte strings too.
        let nul_terminated = out.len() > total;
        if nul_terminated {
            out[total] = 0;
        }
        Ok(Copied::Fits {
            len: total,
            nul_terminated,
        })
    }

    /// `get_string_chunk_size`. `Ok(None)` is `CborErrorNoMoreStringChunks`,
    /// which is how the loop above ends.
    fn chunk_at(&self, cursor: usize, before_first_chunk: bool) -> Result<Option<Chunk>, Error> {
        if self.is_length_known() && !before_first_chunk {
            return Ok(None);
        }
        let Some(&descriptor) = self.buf.get(cursor) else {
            return Err(Error::UnexpectedEof);
        };
        if descriptor == 0xff {
            return Ok(None);
        }
        if descriptor & 0xe0 != self.ty.as_u8() {
            return Err(Error::IllegalType);
        }

        let low = descriptor & 0x1f;
        if low < 24 {
            return Ok(Some(Chunk {
                head_len: 1,
                len: usize::from(low),
            }));
        }
        if low > 27 {
            // Including 31: a chunk may not itself be indefinite-length.
            return Err(Error::IllegalNumber);
        }
        let arg_len = 1usize << (low - 24);
        let Some(arg) = self.buf.get(cursor + 1..cursor + 1 + arg_len) else {
            return Err(Error::UnexpectedEof);
        };
        let mut val = 0u64;
        for &byte in arg {
            val = (val << 8) | u64::from(byte);
        }
        let len = val as usize;
        if len as u64 != val {
            return Err(Error::DataTooLarge);
        }
        Ok(Some(Chunk {
            head_len: 1 + arg_len,
            len,
        }))
    }
}

/// One chunk of a string: its head length and its payload length.
struct Chunk {
    head_len: usize,
    len: usize,
}

/// A CBOR writer over a caller-supplied buffer.
///
/// Mirrors `CborEncoder`, with tinycbor's parent/child encoder pair replaced
/// by an internal stack of item counters — the buffer state the two share is
/// one object here, so there is nothing to synchronise on close.
///
/// Running out of room is not fatal: like tinycbor, the encoder stops writing,
/// keeps counting, and reports the shortfall through
/// [`Self::extra_bytes_needed`] so the caller can retry with a bigger buffer.
/// That is the loop `services_cbor_as_map` runs.
#[derive(Debug)]
pub struct Encoder<'a> {
    buf: &'a mut [u8],
    /// Bytes written; tinycbor's `data.ptr - buffer`.
    written: usize,
    /// `Some` once the buffer overflowed, holding `data.bytes_needed`.
    /// tinycbor signals the same state by nulling `end`.
    shortfall: Option<usize>,
    /// Item counters, outermost first. `stack[0]` is the top-level encoder's,
    /// which `cbor_encoder_init` seeds with 2 and nothing ever reads.
    stack: [usize; MAX_NESTING],
    /// Open containers; `stack[depth]` is the innermost.
    depth: usize,
}

impl<'a> Encoder<'a> {
    /// `cbor_encoder_init`. The `flags` argument is always 0 in bm_core, so
    /// there is nothing to pass.
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        let mut stack = [0; MAX_NESTING];
        stack[0] = 2;
        Self {
            buf,
            written: 0,
            shortfall: None,
            stack,
            depth: 0,
        }
    }

    /// `cbor_encoder_get_buffer_size`: bytes written so far.
    ///
    /// Meaningful only while the encoder has room. Once tinycbor's `end` goes
    /// NULL its `data.ptr` is the union member now holding `bytes_needed`, so
    /// the C's answer after an overflow is a pointer difference against a
    /// count — read [`Self::extra_bytes_needed`] instead, as bm_core does.
    #[must_use]
    pub const fn buffer_size(&self) -> usize {
        self.written
    }

    /// `cbor_encoder_get_extra_bytes_needed`: 0 while there is room, else how
    /// much more buffer the same encoding would have taken.
    #[must_use]
    pub const fn extra_bytes_needed(&self) -> usize {
        match self.shortfall {
            Some(needed) => needed,
            None => 0,
        }
    }

    /// The bytes written so far. Empty of meaning after an overflow, for the
    /// reason [`Self::buffer_size`] gives.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.written]
    }

    /// `cbor_encode_uint`.
    pub fn encode_uint(&mut self, value: u64) -> Result<(), Error> {
        self.encode_number(value, 0x00)
    }

    /// `cbor_encode_int`.
    pub fn encode_int(&mut self, value: i64) -> Result<(), Error> {
        // RFC 7049 appendix C, as tinycbor writes it: complement negatives
        // and put the sign in the major type.
        if value < 0 {
            self.encode_number(!(value as u64), 0x20)
        } else {
            self.encode_number(value as u64, 0x00)
        }
    }

    /// `cbor_encode_float`. Always the 5-byte single-precision form; tinycbor
    /// copies the bits out and never shortens.
    pub fn encode_float(&mut self, value: f32) -> Result<(), Error> {
        let mut head = [0u8; 5];
        head[0] = Type::Float.as_u8();
        head[1..].copy_from_slice(&value.to_bits().to_be_bytes());
        self.saturated_decrement();
        self.append(&head)
    }

    /// `cbor_encode_text_string`. No UTF-8 validation, as in the C.
    pub fn encode_text(&mut self, value: &[u8]) -> Result<(), Error> {
        self.encode_string(value, 0x60)
    }

    /// `cbor_encode_byte_string`.
    pub fn encode_bytes(&mut self, value: &[u8]) -> Result<(), Error> {
        self.encode_string(value, 0x40)
    }

    /// `cbor_encoder_create_map` with a definite `len`.
    ///
    /// Each key and each value counts towards the container, so
    /// [`Self::close_container`] expects `2 * len` items.
    pub fn open_map(&mut self, len: usize) -> Result<(), Error> {
        // tinycbor's guard against `remaining` overflowing when it doubles.
        if len > usize::MAX / 2 {
            return Err(Error::DataTooLarge);
        }
        self.open_container(len, 2 * len + 1, 0xa0)
    }

    /// `cbor_encoder_create_array` with a definite `len`.
    pub fn open_array(&mut self, len: usize) -> Result<(), Error> {
        self.open_container(len, len + 1, 0x80)
    }

    /// `cbor_encoder_close_container`.
    ///
    /// Reports [`Error::TooManyItems`] or [`Error::TooFewItems`] against the
    /// declared count first, and [`Error::OutOfMemory`] only once the count
    /// was right — the order matters, because `services_cbor_as_map` reads a
    /// close that reports `OutOfMemory` as "retry with a bigger buffer" and
    /// anything else as "give up".
    pub fn close_container(&mut self) -> Result<(), Error> {
        if self.depth == 0 {
            return Err(Error::NotInContainer);
        }
        let remaining = self.stack[self.depth];
        self.depth -= 1;
        match remaining {
            1 => {}
            0 => return Err(Error::TooManyItems),
            _ => return Err(Error::TooFewItems),
        }
        if self.shortfall.is_some() {
            return Err(Error::OutOfMemory);
        }
        Ok(())
    }

    fn open_container(&mut self, len: usize, remaining: usize, major: u8) -> Result<(), Error> {
        if self.depth + 1 >= MAX_NESTING {
            return Err(Error::NestingTooDeep);
        }
        self.saturated_decrement();
        self.depth += 1;
        self.stack[self.depth] = remaining;
        // The container's own head does not count against the container.
        self.encode_number_no_update(len as u64, major)
    }

    fn encode_string(&mut self, value: &[u8], major: u8) -> Result<(), Error> {
        // tinycbor returns the *body* append's result. A head that reported
        // OutOfMemory cannot be followed by a body that succeeds, so the two
        // always agree.
        let _ = self.encode_number(value.len() as u64, major);
        self.append(value)
    }

    fn encode_number(&mut self, value: u64, major: u8) -> Result<(), Error> {
        self.saturated_decrement();
        self.encode_number_no_update(value, major)
    }

    /// `encode_number_no_update`: the shortest head that holds `value`.
    fn encode_number_no_update(&mut self, value: u64, major: u8) -> Result<(), Error> {
        if value < 24 {
            return self.append(&[major | (value as u8)]);
        }
        let more =
            u8::from(value > 0xff) + u8::from(value > 0xffff) + u8::from(value > 0xffff_ffff);
        let arg_len = 1usize << more;
        let mut head = [0u8; 9];
        head[0] = major + 24 + more;
        head[1..=arg_len].copy_from_slice(&value.to_be_bytes()[8 - arg_len..]);
        self.append(&head[..=arg_len])
    }

    /// `saturated_decrement` on the innermost open container.
    fn saturated_decrement(&mut self) {
        let remaining = &mut self.stack[self.depth];
        *remaining = remaining.saturating_sub(1);
    }

    /// `append_to_buffer`.
    ///
    /// The first overflow credits the bytes that would have fit without
    /// writing them, then switches to counting the shortfall; everything
    /// after it only counts. Re-encoding into a buffer grown by the shortfall
    /// therefore fits exactly.
    fn append(&mut self, data: &[u8]) -> Result<(), Error> {
        match self.shortfall {
            None => {
                let room = self.buf.len() - self.written;
                if data.len() > room {
                    self.shortfall = Some(data.len() - room);
                    return Err(Error::OutOfMemory);
                }
                self.buf[self.written..self.written + data.len()].copy_from_slice(data);
                self.written += data.len();
                Ok(())
            }
            // `would_overflow` reports no overflow when both the shortfall and
            // the append are empty, and tinycbor then memcpys nothing and
            // succeeds. The first overflow always leaves a non-zero shortfall,
            // so this is unreachable; it is here because the C has it.
            Some(0) if data.is_empty() => Ok(()),
            Some(needed) => {
                self.shortfall = Some(needed + data.len());
                Err(Error::OutOfMemory)
            }
        }
    }
}
