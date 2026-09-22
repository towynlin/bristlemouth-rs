//! Differential comparators for `bm_wire::cbor` against the tinycbor bm_core
//! vendors.
//!
//! tinycbor is pure — no shim state, no allocation, no clock — so this
//! comparator needs nothing brought up and its target lives in
//! [`crate::replay::TARGETS`].
//!
//! Two halves, run against the same input:
//!
//! * **encode** — a scripted sequence of `cbor_encode*` calls into a buffer
//!   deliberately small enough that overflow is the common case, comparing the
//!   error, the cursor, the shortfall and the bytes after every call;
//! * **decode** — `cbor_parser_init` over arbitrary bytes, comparing the
//!   error, `cbor_value_is_valid`, `cbor_value_get_type`, every predicate
//!   `bcmp/configuration.c` calls and every accessor those predicates unlock.
//!
//! The two are independent: nothing the encoder writes is fed to the parser,
//! because a round trip would only ever exercise the well-formed subset and
//! the parser's real input is a `ConfigSet` (`0xA2`) body from the wire.

use std::mem::MaybeUninit;

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::cbor::{Copied, Encoder, Error};

/// Largest encoder output buffer a step may ask for.
///
/// The gold vector from `cbor_service_helper_test.cpp` is 91 bytes, so this
/// is enough to encode something real while staying small enough that
/// libFuzzer reaches the overflow paths constantly.
pub const MAX_BUFFER: usize = 160;

/// Largest buffer a `copy_*_string` step may be given.
pub const MAX_COPY: usize = 80;

/// Deepest the encode script will nest containers.
///
/// Half of [`bm_wire::cbor::MAX_NESTING`], so [`check`] can assert the port's
/// ceiling was never what stopped a step.
pub const MAX_SCRIPT_DEPTH: usize = 4;

/// One `cbor_encode*` call.
#[derive(Debug, Clone)]
pub enum Op {
    /// `cbor_encode_uint`.
    Uint(u64),
    /// `cbor_encode_int`.
    Int(i64),
    /// `cbor_encode_float`, carried as bits so a NaN compares as itself —
    /// tinycbor copies the bits out without inspecting them.
    Float(u32),
    /// `cbor_encode_text_string`. No UTF-8 constraint: neither side validates.
    Text(Vec<u8>),
    /// `cbor_encode_byte_string`.
    Bytes(Vec<u8>),
    /// `cbor_encoder_create_map`.
    OpenMap(u8),
    /// `cbor_encoder_create_array`.
    OpenArray(u8),
    /// `cbor_encoder_close_container`.
    Close,
}

/// An encode script plus a decode payload.
#[derive(Debug, Clone)]
pub struct CborInput {
    /// Size of the encoder's output buffer, 0..=[`MAX_BUFFER`].
    pub buffer_len: usize,
    /// Calls to make, in order.
    pub ops: Vec<Op>,
    /// Bytes handed to `cbor_parser_init`. Unconstrained: a `ConfigSet` body
    /// is whatever the sender put in it.
    pub payload: Vec<u8>,
    /// Size of the buffer handed to `cbor_value_copy_*_string`,
    /// 0..=[`MAX_COPY`].
    pub copy_len: usize,
}

impl<'a> Arbitrary<'a> for Op {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(match u.int_in_range(0u8..=7)? {
            0 => Self::Uint(u.arbitrary()?),
            1 => Self::Int(u.arbitrary()?),
            2 => Self::Float(u.arbitrary()?),
            3 => Self::Text(bounded_bytes(u)?),
            4 => Self::Bytes(bounded_bytes(u)?),
            5 => Self::OpenMap(u.int_in_range(0u8..=4)?),
            6 => Self::OpenArray(u.int_in_range(0u8..=4)?),
            _ => Self::Close,
        })
    }
}

/// A string body, kept short enough that a step can both fit and overflow.
fn bounded_bytes(u: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let len = u.int_in_range(0usize..=48)?;
    let mut bytes = vec![0u8; len];
    u.fill_buffer(&mut bytes)?;
    Ok(bytes)
}

impl<'a> Arbitrary<'a> for CborInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let buffer_len = u.int_in_range(0..=MAX_BUFFER)?;
        let copy_len = u.int_in_range(0..=MAX_COPY)?;
        let op_count = u.int_in_range(0usize..=12)?;
        let mut ops = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            ops.push(u.arbitrary()?);
        }
        let payload = bounded_bytes(u)?;
        Ok(Self {
            buffer_len,
            ops,
            payload,
            copy_len,
        })
    }
}

/// Assert the Rust CBOR codec agrees with tinycbor for this input.
///
/// # Panics
///
/// On any divergence in either half.
pub fn check(input: &CborInput) {
    check_encode(input.buffer_len, &input.ops);
    check_decode(&input.payload, input.copy_len);
}

/// tinycbor's `CborError` for one of ours.
fn c_error(err: Result<(), Error>) -> bm_wire_sys::CborError {
    match err {
        Ok(()) => bm_wire_sys::CborError_CborNoError,
        Err(Error::UnknownLength) => bm_wire_sys::CborError_CborErrorUnknownLength,
        Err(Error::UnexpectedEof) => bm_wire_sys::CborError_CborErrorUnexpectedEOF,
        Err(Error::UnexpectedBreak) => bm_wire_sys::CborError_CborErrorUnexpectedBreak,
        Err(Error::UnknownType) => bm_wire_sys::CborError_CborErrorUnknownType,
        Err(Error::IllegalType) => bm_wire_sys::CborError_CborErrorIllegalType,
        Err(Error::IllegalNumber) => bm_wire_sys::CborError_CborErrorIllegalNumber,
        Err(Error::IllegalSimpleType) => bm_wire_sys::CborError_CborErrorIllegalSimpleType,
        Err(Error::TooManyItems) => bm_wire_sys::CborError_CborErrorTooManyItems,
        Err(Error::TooFewItems) => bm_wire_sys::CborError_CborErrorTooFewItems,
        Err(Error::DataTooLarge) => bm_wire_sys::CborError_CborErrorDataTooLarge,
        Err(Error::OutOfMemory) => bm_wire_sys::CborError_CborErrorOutOfMemory,
        // Port-side ceilings with no C counterpart. `check_encode` asserts no
        // step ever reaches one, so these are unreachable by construction.
        Err(other) => panic!("{other:?} has no CborError; the script should not have reached it"),
    }
}

/// What one op did to an encoder, on either side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Step {
    err: bm_wire_sys::CborError,
    buffer_size: usize,
    extra_needed: usize,
}

/// Which ops the script must skip, on both sides.
///
/// Two steps cannot be put to the C: a close with nothing open, and an open
/// past [`MAX_SCRIPT_DEPTH`], which is as many `CborEncoder`s as the harness
/// holds. The port answers both with an error that has no `CborError`, so
/// there would be nothing to compare; skipping them on both sides keeps the
/// two encoders in step for everything after. [`bm_wire::cbor`]'s own unit
/// tests cover what the port does with them.
fn skipped(ops: &[Op]) -> Vec<bool> {
    let mut depth = 0usize;
    ops.iter()
        .map(|op| match op {
            Op::OpenMap(_) | Op::OpenArray(_) => {
                let full = depth == MAX_SCRIPT_DEPTH;
                if !full {
                    depth += 1;
                }
                full
            }
            Op::Close => {
                let empty = depth == 0;
                if !empty {
                    depth -= 1;
                }
                empty
            }
            _ => false,
        })
        .collect()
}

/// Run `ops` through both encoders and compare after every one.
fn check_encode(buffer_len: usize, ops: &[Op]) {
    let skip = skipped(ops);
    let mut rust_buf = vec![0u8; buffer_len];
    let mut rust_steps = Vec::with_capacity(ops.len());

    {
        let mut enc = Encoder::new(&mut rust_buf);
        for (op, skip) in ops.iter().zip(&skip) {
            if *skip {
                rust_steps.push(None);
                continue;
            }
            let err = match op {
                Op::Uint(v) => enc.encode_uint(*v),
                Op::Int(v) => enc.encode_int(*v),
                Op::Float(bits) => enc.encode_float(f32::from_bits(*bits)),
                Op::Text(bytes) => enc.encode_text(bytes),
                Op::Bytes(bytes) => enc.encode_bytes(bytes),
                Op::OpenMap(n) => enc.open_map(usize::from(*n)),
                Op::OpenArray(n) => enc.open_array(usize::from(*n)),
                Op::Close => enc.close_container(),
            };
            assert_ne!(
                err,
                Err(Error::NestingTooDeep),
                "the port's MAX_NESTING ceiling was reached; the script is \
                 capped at {MAX_SCRIPT_DEPTH} and the ceiling must stay out of reach"
            );
            rust_steps.push(Some(Step {
                err: c_error(err),
                buffer_size: enc.buffer_size(),
                extra_needed: enc.extra_bytes_needed(),
            }));
        }
    }

    let mut c_buf = vec![0u8; buffer_len];
    let c_steps = run_c_encoder(&mut c_buf, ops, &skip);

    for (index, (op, (rs, c))) in ops.iter().zip(rust_steps.iter().zip(&c_steps)).enumerate() {
        let (Some(rs), Some(c)) = (rs, c) else {
            assert_eq!(
                rs.is_none(),
                c.is_none(),
                "op {index} ({op:?}) was skipped on one side only"
            );
            continue;
        };
        assert_eq!(
            rs.err, c.err,
            "op {index} ({op:?}) returned a different error into a {buffer_len}-byte buffer"
        );
        assert_eq!(
            rs.extra_needed, c.extra_needed,
            "op {index} ({op:?}) left a different shortfall"
        );
        if rs.extra_needed == 0 {
            // Once tinycbor has overflowed, `data.ptr` is the union member
            // holding `bytes_needed`, so `cbor_encoder_get_buffer_size` is a
            // pointer difference against a count. Nothing is written after
            // that point, so the bytes stay comparable but the cursor does not.
            assert_eq!(
                rs.buffer_size, c.buffer_size,
                "op {index} ({op:?}) left a different cursor"
            );
        }
    }

    assert_eq!(
        rust_buf,
        c_buf,
        "the encoded buffers differ after {} ops into {buffer_len} bytes",
        ops.len()
    );
}

/// Drive tinycbor's encoder through the same script, skipping what
/// [`skipped`] marks.
fn run_c_encoder(buf: &mut [u8], ops: &[Op], skip: &[bool]) -> Vec<Option<Step>> {
    let mut encoders = [bm_wire_sys::CborEncoder::default(); MAX_SCRIPT_DEPTH + 1];
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    // SAFETY: `encoders[0]` is a live, correctly-sized CborEncoder, and
    // `ptr`/`len` describe a buffer that outlives every call below.
    unsafe { bm_wire_sys::cbor_encoder_init(&raw mut encoders[0], ptr, len, 0) };

    let mut depth = 0usize;
    let mut steps = Vec::with_capacity(ops.len());
    for (op, skip) in ops.iter().zip(skip) {
        if *skip {
            steps.push(None);
            continue;
        }
        // SAFETY: every call below takes `&raw mut encoders[depth]`, which is
        // in bounds because `depth` never exceeds MAX_SCRIPT_DEPTH, and any
        // pointer/length pair comes from a slice that outlives the call.
        let err = unsafe {
            let active = &raw mut encoders[depth];
            match op {
                Op::Uint(v) => bm_wire_sys::cbor_encode_uint(active, *v),
                Op::Int(v) => bm_wire_sys::cbor_encode_int(active, *v),
                Op::Float(bits) => bm_wire_sys::cbor_encode_float(active, f32::from_bits(*bits)),
                Op::Text(bytes) => {
                    bm_wire_sys::cbor_encode_text_string(active, bytes.as_ptr().cast(), bytes.len())
                }
                Op::Bytes(bytes) => {
                    bm_wire_sys::cbor_encode_byte_string(active, bytes.as_ptr(), bytes.len())
                }
                Op::OpenMap(n) | Op::OpenArray(n) => {
                    let child = &raw mut encoders[depth + 1];
                    let err = if matches!(op, Op::OpenMap(_)) {
                        bm_wire_sys::cbor_encoder_create_map(active, child, usize::from(*n))
                    } else {
                        bm_wire_sys::cbor_encoder_create_array(active, child, usize::from(*n))
                    };
                    depth += 1;
                    err
                }
                Op::Close => {
                    let parent = &raw mut encoders[depth - 1];
                    let child = &raw const encoders[depth];
                    depth -= 1;
                    bm_wire_sys::cbor_encoder_close_container(parent, child)
                }
            }
        };
        // SAFETY: `encoders[depth]` is live, and `ptr` is the same buffer
        // pointer `cbor_encoder_init` was given.
        let (buffer_size, extra_needed) = unsafe {
            let active = &raw const encoders[depth];
            (
                bm_wire_sys::cbor_encoder_get_buffer_size(active, ptr),
                bm_wire_sys::cbor_encoder_get_extra_bytes_needed(active),
            )
        };
        steps.push(Some(Step {
            err,
            buffer_size,
            extra_needed,
        }));
    }
    steps
}

/// Parse `payload` on both sides and compare everything reachable.
fn check_decode(payload: &[u8], copy_len: usize) {
    let mut parser = MaybeUninit::<bm_wire_sys::CborParser>::zeroed();
    let mut it = MaybeUninit::<bm_wire_sys::CborValue>::zeroed();
    // SAFETY: `payload`'s pointer and length describe a live slice, and both
    // out-parameters are correctly-sized and aligned. tinycbor reads no byte
    // beyond `payload.as_ptr() + payload.len()`.
    let c_err = unsafe {
        bm_wire_sys::cbor_parser_init(
            payload.as_ptr(),
            payload.len(),
            0,
            parser.as_mut_ptr(),
            it.as_mut_ptr(),
        )
    };
    // SAFETY: cbor_parser_init writes every field of both structs before it
    // returns, on the error paths as well as the success one.
    let (parser, it) = unsafe { (parser.assume_init(), it.assume_init()) };
    let _ = parser; // `it` borrows it; keep it alive for the accessors below.
    let value = &raw const it;

    let rs = bm_wire::cbor::parse(payload);
    assert_eq!(
        c_error(rs.result),
        c_err,
        "cbor_parser_init diverged for {payload:02x?}"
    );

    // SAFETY (all of the below): `value` points at the initialised CborValue,
    // and every accessor is guarded by the predicate its C assertion demands.
    unsafe {
        assert_eq!(
            bm_wire_sys::cbor_value_is_valid(value),
            rs.value.is_valid(),
            "cbor_value_is_valid diverged for {payload:02x?}"
        );
        assert_eq!(
            u8::try_from(bm_wire_sys::cbor_value_get_type(value)).unwrap(),
            rs.value.ty().as_u8(),
            "cbor_value_get_type diverged for {payload:02x?}"
        );

        for (name, c, rust) in [
            (
                "is_integer",
                bm_wire_sys::cbor_value_is_integer(value),
                rs.value.is_integer(),
            ),
            (
                "is_unsigned_integer",
                bm_wire_sys::cbor_value_is_unsigned_integer(value),
                rs.value.is_unsigned_integer(),
            ),
            (
                "is_negative_integer",
                bm_wire_sys::cbor_value_is_negative_integer(value),
                rs.value.is_negative_integer(),
            ),
            (
                "is_byte_string",
                bm_wire_sys::cbor_value_is_byte_string(value),
                rs.value.is_byte_string(),
            ),
            (
                "is_text_string",
                bm_wire_sys::cbor_value_is_text_string(value),
                rs.value.is_text_string(),
            ),
            (
                "is_array",
                bm_wire_sys::cbor_value_is_array(value),
                rs.value.is_array(),
            ),
            (
                "is_map",
                bm_wire_sys::cbor_value_is_map(value),
                rs.value.is_map(),
            ),
            (
                "is_float",
                bm_wire_sys::cbor_value_is_float(value),
                rs.value.is_float(),
            ),
            (
                "is_length_known",
                bm_wire_sys::cbor_value_is_length_known(value),
                rs.value.is_length_known(),
            ),
        ] {
            assert_eq!(c, rust, "cbor_value_{name} diverged for {payload:02x?}");
        }

        if rs.value.is_unsigned_integer() {
            let mut out = 0u64;
            let err = bm_wire_sys::cbor_value_get_uint64(value, &raw mut out);
            assert_eq!(err, bm_wire_sys::CborError_CborNoError);
            assert_eq!(
                Some(out),
                rs.value.get_uint64(),
                "cbor_value_get_uint64 diverged for {payload:02x?}"
            );
        }

        if rs.value.is_integer() && !is_int64_negation_overflow(payload) {
            let mut out = 0i64;
            let err = bm_wire_sys::cbor_value_get_int64(value, &raw mut out);
            assert_eq!(err, bm_wire_sys::CborError_CborNoError);
            assert_eq!(
                Some(out),
                rs.value.get_int64(),
                "cbor_value_get_int64 diverged for {payload:02x?}"
            );
        }

        if rs.value.is_float() {
            let mut out = 0f32;
            let err = bm_wire_sys::cbor_value_get_float(value, &raw mut out);
            assert_eq!(err, bm_wire_sys::CborError_CborNoError);
            assert_eq!(
                out.to_bits(),
                rs.value.get_float().map(f32::to_bits).unwrap(),
                "cbor_value_get_float diverged for {payload:02x?}"
            );
        }

        if rs.value.is_byte_string() || rs.value.is_text_string() {
            check_length(
                "get_string_length",
                bm_wire_sys::cbor_value_get_string_length,
                value,
                rs.value.string_length().unwrap(),
                payload,
            );
            check_copy(&rs.value, value, copy_len, payload);
        }
        if rs.value.is_array() {
            check_length(
                "get_array_length",
                bm_wire_sys::cbor_value_get_array_length,
                value,
                rs.value.array_length().unwrap(),
                payload,
            );
        }
        if rs.value.is_map() {
            check_length(
                "get_map_length",
                bm_wire_sys::cbor_value_get_map_length,
                value,
                rs.value.map_length().unwrap(),
                payload,
            );
        }
    }
}

/// `-(2^63) - 1`, the one CBOR value `cbor_value_get_int64` cannot read.
///
/// tinycbor computes a negative integer as `-(int64_t)argument - 1`. For an
/// argument of `1 << 63` that negation overflows `int64_t`, which C leaves
/// undefined; gcc and clang wrap and yield [`i64::MAX`], and `bm-wire`
/// reproduces that in wrapping arithmetic (divergence #41). Under
/// `cargo fuzz`, where `build.rs` compiles the C with
/// `-fsanitize=undefined`, the C aborts instead of answering, so there is
/// nothing to compare against and this one input is held out.
///
/// The head is recognised from the bytes rather than from the port, so the
/// thing under test does not decide its own domain.
fn is_int64_negation_overflow(payload: &[u8]) -> bool {
    payload.len() >= 9
        && payload[0] == 0x3b
        && payload[1] == 0x80
        && payload[2..9].iter().all(|&b| b == 0)
}

/// Compare one of the three `get_*_length` accessors.
unsafe fn check_length(
    name: &str,
    c_fn: unsafe extern "C" fn(*const bm_wire_sys::CborValue, *mut usize) -> bm_wire_sys::CborError,
    value: *const bm_wire_sys::CborValue,
    rust: std::result::Result<usize, Error>,
    payload: &[u8],
) {
    let mut out = usize::MAX;
    // SAFETY: the caller has checked the predicate `c_fn` asserts on.
    let err = unsafe { c_fn(value, &raw mut out) };
    assert_eq!(
        err,
        c_error(rust.map(|_| ())),
        "cbor_value_{name} returned a different error for {payload:02x?}"
    );
    if let Ok(len) = rust {
        assert_eq!(
            out, len,
            "cbor_value_{name} returned a different length for {payload:02x?}"
        );
    }
}

/// Compare `cbor_value_copy_text_string` / `_byte_string` into a buffer of
/// `copy_len` bytes: the error, the length written back, and every byte of
/// the buffer including the ones neither side should have touched.
unsafe fn check_copy(
    rs: &bm_wire::cbor::Value<'_>,
    value: *const bm_wire_sys::CborValue,
    copy_len: usize,
    payload: &[u8],
) {
    /// Neither 0 nor anything a chunk is likely to carry, so an untouched
    /// byte is distinguishable from a written one.
    const FILL: u8 = 0xa7;

    let text = rs.is_text_string();
    let mut c_buf = vec![FILL; copy_len];
    let mut c_len = copy_len;
    // SAFETY: the caller has checked the predicate the C asserts on, and
    // `c_buf`/`c_len` are a live buffer and its length.
    let c_err = unsafe {
        if text {
            bm_wire_sys::cbor_value_copy_text_string(
                value,
                c_buf.as_mut_ptr().cast(),
                &raw mut c_len,
                std::ptr::null_mut(),
            )
        } else {
            bm_wire_sys::cbor_value_copy_byte_string(
                value,
                c_buf.as_mut_ptr(),
                &raw mut c_len,
                std::ptr::null_mut(),
            )
        }
    };

    let mut rust_buf = vec![FILL; copy_len];
    let rust = if text {
        rs.copy_text_string(&mut rust_buf).unwrap()
    } else {
        rs.copy_byte_string(&mut rust_buf).unwrap()
    };

    let rust_err = match rust {
        Ok(Copied::Fits { .. }) => Ok(()),
        Ok(Copied::TooSmall { .. }) => Err(Error::OutOfMemory),
        Err(err) => Err(err),
    };
    assert_eq!(
        c_err,
        c_error(rust_err),
        "copy_string returned a different error for {payload:02x?} into {copy_len} bytes"
    );
    assert_eq!(
        c_buf, rust_buf,
        "copy_string wrote different bytes for {payload:02x?} into {copy_len} bytes"
    );
    if let Ok(copied) = rust {
        // tinycbor writes the total back through `buflen` whether or not the
        // whole string fit, and only leaves it alone on the errors that come
        // out of chunk iteration.
        assert_eq!(
            c_len,
            copied.len(),
            "copy_string reported a different length for {payload:02x?} into {copy_len} bytes"
        );
    } else {
        assert_eq!(
            c_len, copy_len,
            "copy_string overwrote buflen on an error path for {payload:02x?}"
        );
    }
}

/// Every `CborType` value tinycbor can assign, for the test that pins the
/// port's discriminants to the C's.
#[cfg(test)]
const C_TYPES: &[(bm_wire::cbor::Type, bm_wire_sys::CborType)] = &[
    (
        bm_wire::cbor::Type::Integer,
        bm_wire_sys::CborType_CborIntegerType,
    ),
    (
        bm_wire::cbor::Type::ByteString,
        bm_wire_sys::CborType_CborByteStringType,
    ),
    (
        bm_wire::cbor::Type::TextString,
        bm_wire_sys::CborType_CborTextStringType,
    ),
    (
        bm_wire::cbor::Type::Array,
        bm_wire_sys::CborType_CborArrayType,
    ),
    (bm_wire::cbor::Type::Map, bm_wire_sys::CborType_CborMapType),
    (bm_wire::cbor::Type::Tag, bm_wire_sys::CborType_CborTagType),
    (
        bm_wire::cbor::Type::Simple,
        bm_wire_sys::CborType_CborSimpleType,
    ),
    (
        bm_wire::cbor::Type::Boolean,
        bm_wire_sys::CborType_CborBooleanType,
    ),
    (
        bm_wire::cbor::Type::Null,
        bm_wire_sys::CborType_CborNullType,
    ),
    (
        bm_wire::cbor::Type::Undefined,
        bm_wire_sys::CborType_CborUndefinedType,
    ),
    (
        bm_wire::cbor::Type::HalfFloat,
        bm_wire_sys::CborType_CborHalfFloatType,
    ),
    (
        bm_wire::cbor::Type::Float,
        bm_wire_sys::CborType_CborFloatType,
    ),
    (
        bm_wire::cbor::Type::Double,
        bm_wire_sys::CborType_CborDoubleType,
    ),
    (
        bm_wire::cbor::Type::Invalid,
        bm_wire_sys::CborType_CborInvalidType,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn run(buffer_len: usize, ops: &[Op], payload: &[u8], copy_len: usize) {
        check(&CborInput {
            buffer_len,
            ops: ops.to_vec(),
            payload: payload.to_vec(),
            copy_len,
        });
    }

    fn decode(payload: &[u8]) {
        for copy_len in [0, 1, 4, 43, MAX_COPY] {
            run(0, &[], payload, copy_len);
        }
    }

    /// The port's `Type` discriminants are compared against the C's on every
    /// decode, so they have to be the C's.
    #[test]
    fn every_type_discriminant_is_the_c_constant() {
        for (rust, c) in C_TYPES {
            assert_eq!(
                u32::from(rust.as_u8()),
                *c,
                "{rust:?} does not carry tinycbor's CborType value"
            );
        }
    }

    #[test]
    fn every_first_byte_parses_the_same_way() {
        for byte in 0u8..=255 {
            decode(&[byte]);
            // With a full 8-byte argument behind it, so the multi-byte heads
            // reach their fixups instead of stopping at end-of-buffer.
            decode(&[byte, 1, 2, 3, 4, 5, 6, 7, 8]);
            decode(&[byte, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        }
    }

    #[test]
    fn truncated_heads() {
        for byte in [0x18u8, 0x19, 0x1a, 0x1b, 0x38, 0x3a, 0x3b, 0xf9, 0xfa, 0xfb] {
            for len in 0..9 {
                let payload: Vec<u8> = core::iter::repeat_n(byte, 1)
                    .chain(core::iter::repeat_n(0x5a, len))
                    .collect();
                decode(&payload);
            }
        }
    }

    #[test]
    fn indefinite_length_strings() {
        // Two chunks and a break, well formed.
        decode(&[0x7f, 0x63, b'f', b'o', b'o', 0x62, b'e', b'r', 0xff]);
        decode(&[0x5f, 0x41, 0xde, 0x42, 0xad, 0xbe, 0xff]);
        // No break byte.
        decode(&[0x7f, 0x63, b'f', b'o', b'o']);
        // Empty.
        decode(&[0x7f, 0xff]);
        decode(&[0x5f, 0xff]);
        // A chunk of the wrong major type.
        decode(&[0x7f, 0x43, 1, 2, 3, 0xff]);
        // A chunk that is itself indefinite.
        decode(&[0x7f, 0x7f, 0xff, 0xff]);
        // A chunk whose declared length runs off the end.
        decode(&[0x7f, 0x78, 0x40, b'a', 0xff]);
        // A definite string with a length longer than the buffer.
        decode(&[0x78, 0x40, b'a']);
    }

    /// The negative integer whose negation overflows, and its neighbours.
    #[test]
    fn negative_integer_extremes() {
        decode(&[0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        decode(&[0x3b, 0x80, 0, 0, 0, 0, 0, 0, 0]);
        decode(&[0x3b, 0x80, 0, 0, 0, 0, 0, 0, 1]);
        decode(&[0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }

    /// The map `cbor_service_helper_test.cpp` builds, byte for byte, plus the
    /// 91-byte total it asserts. Both encoders have to produce it.
    /// `float baz = 3.14159;` from the gtest, as bits. Written this way
    /// because the literal is what the gold vector encodes —
    /// `f32::consts::PI` is `0x4049_0fdb` and would not reproduce it.
    const GOLD_BAZ: u32 = 0x4049_0fd0;

    #[test]
    fn the_cbor_service_helper_gold_map() {
        const SILLY: &[u8] = b"The quick brown fox jumps over the lazy dog";
        const BYTES: &[u8] = &[0xde, 0xad, 0xbe, 0xef, 0x5a, 0xad, 0xda, 0xad, 0xb0, 0xdd];
        let ops = vec![
            Op::OpenMap(5),
            Op::Text(b"foo".to_vec()),
            Op::Uint(42),
            Op::Text(b"bar".to_vec()),
            Op::Int(-1000),
            Op::Text(b"baz".to_vec()),
            Op::Float(GOLD_BAZ),
            Op::Text(b"silly".to_vec()),
            Op::Text(SILLY.to_vec()),
            Op::Text(b"bytes".to_vec()),
            Op::Bytes(BYTES.to_vec()),
            Op::Close,
        ];
        run(MAX_BUFFER, &ops, &[], 0);

        // The literal the gtest asserts, from the cbor.me listing in its
        // comment. Nothing in bm_core pins these bytes; the test pins the
        // eleven offsets it happens to check, and this pins all 91.
        let mut buf = [0u8; MAX_BUFFER];
        let mut enc = Encoder::new(&mut buf);
        enc.open_map(5).unwrap();
        enc.encode_text(b"foo").unwrap();
        enc.encode_uint(42).unwrap();
        enc.encode_text(b"bar").unwrap();
        enc.encode_int(-1000).unwrap();
        enc.encode_text(b"baz").unwrap();
        enc.encode_float(f32::from_bits(GOLD_BAZ)).unwrap();
        enc.encode_text(b"silly").unwrap();
        enc.encode_text(SILLY).unwrap();
        enc.encode_text(b"bytes").unwrap();
        enc.encode_bytes(BYTES).unwrap();
        enc.close_container().unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(&[0xa5, 0x63, b'f', b'o', b'o', 0x18, 0x2a]);
        expected.extend_from_slice(&[0x63, b'b', b'a', b'r', 0x39, 0x03, 0xe7]);
        expected.extend_from_slice(&[0x63, b'b', b'a', b'z', 0xfa, 0x40, 0x49, 0x0f, 0xd0]);
        expected.extend_from_slice(&[0x65, b's', b'i', b'l', b'l', b'y', 0x78, 0x2b]);
        expected.extend_from_slice(SILLY);
        expected.extend_from_slice(&[0x65, b'b', b'y', b't', b'e', b's', 0x4a]);
        expected.extend_from_slice(BYTES);

        assert_eq!(enc.buffer_size(), 91, "the gtest asserts 91 bytes");
        assert_eq!(enc.written(), expected);
        // The eleven offsets cbor_service_helper_test.cpp checks by hand.
        for (offset, byte) in [
            (0, 0xa5),
            (1, 0x63),
            (2, 0x66),
            (3, 0x6f),
            (4, 0x6f),
            (5, 0x18),
            (18, 0xfa),
            (23, 0x65),
            (29, 0x78),
            (30, 0x2b),
            (74, 0x65),
            (80, 0x4a),
        ] {
            assert_eq!(expected[offset], byte, "gtest asserts buffer[{offset}]");
        }
    }

    /// The five values `cbor_service_helper_test.cpp` stores, each on its own,
    /// which is what `set_config_cbor` is handed.
    #[test]
    fn the_cbor_service_helper_gold_values() {
        run(MAX_COPY, &[Op::Uint(42)], &[0x18, 0x2a], MAX_COPY);
        run(MAX_COPY, &[Op::Int(-1000)], &[0x39, 0x03, 0xe7], MAX_COPY);
        run(
            MAX_COPY,
            &[Op::Float(GOLD_BAZ)],
            &[0xfa, 0x40, 0x49, 0x0f, 0xd0],
            MAX_COPY,
        );
        let mut silly = vec![0x78, 0x2b];
        silly.extend_from_slice(b"The quick brown fox jumps over the lazy dog");
        run(
            MAX_COPY,
            &[Op::Text(
                b"The quick brown fox jumps over the lazy dog".to_vec(),
            )],
            &silly,
            MAX_COPY,
        );
        run(
            MAX_COPY,
            &[Op::Bytes(vec![0xde, 0xad, 0xbe, 0xef])],
            &[0x44, 0xde, 0xad, 0xbe, 0xef],
            MAX_COPY,
        );
    }

    /// The head lengths tinycbor picks, at every boundary.
    #[test]
    fn integer_head_boundaries() {
        for value in [
            0u64,
            23,
            24,
            0xff,
            0x100,
            0xffff,
            0x1_0000,
            0xffff_ffff,
            0x1_0000_0000,
            u64::MAX,
        ] {
            for len in 0..=10 {
                run(len, &[Op::Uint(value)], &[], 0);
            }
        }
        for value in [
            0i64,
            -1,
            -24,
            -25,
            -256,
            -257,
            -65536,
            -65537,
            i64::MIN,
            i64::MAX,
        ] {
            for len in 0..=10 {
                run(len, &[Op::Int(value)], &[], 0);
            }
        }
    }

    /// Overflow, the retry loop `services_cbor_as_map` runs, and the close
    /// that reports it.
    #[test]
    fn overflow_reports_the_exact_shortfall() {
        let ops = vec![
            Op::OpenMap(2),
            Op::Text(b"alpha".to_vec()),
            Op::Uint(1_000_000),
            Op::Text(b"beta".to_vec()),
            Op::Bytes(vec![7; 20]),
            Op::Close,
        ];
        for len in 0..48 {
            run(len, &ops, &[], 0);
        }

        // 8 bytes holds the map head and "alpha" and nothing else, so the
        // uint is the first call to overflow and every call after it only
        // counts.
        let mut buf = [0u8; 8];
        let mut enc = Encoder::new(&mut buf);
        enc.open_map(2).unwrap();
        assert_eq!(enc.encode_text(b"alpha"), Ok(()));
        assert_eq!(enc.buffer_size(), 7);
        assert_eq!(enc.encode_uint(1_000_000), Err(Error::OutOfMemory));
        assert_eq!(enc.encode_text(b"beta"), Err(Error::OutOfMemory));
        assert_eq!(enc.encode_bytes(&[7; 20]), Err(Error::OutOfMemory));
        // The item count still came out right, so the close reports the
        // buffer rather than the script.
        assert_eq!(enc.close_container(), Err(Error::OutOfMemory));
        let needed = enc.extra_bytes_needed();
        assert_eq!(needed, 30);

        // The point of the shortfall: a retry into `len + needed` fits exactly.
        let mut big = vec![0u8; 8 + needed];
        let mut enc = Encoder::new(&mut big);
        enc.open_map(2).unwrap();
        enc.encode_text(b"alpha").unwrap();
        enc.encode_uint(1_000_000).unwrap();
        enc.encode_text(b"beta").unwrap();
        enc.encode_bytes(&[7; 20]).unwrap();
        enc.close_container().unwrap();
        assert_eq!(enc.buffer_size(), 8 + needed);
    }

    #[test]
    fn container_item_counts() {
        // Too few, too many, and exactly right.
        for declared in 0u8..=3 {
            for items in 0u8..=6 {
                let mut ops = vec![Op::OpenArray(declared)];
                ops.extend((0..items).map(|i| Op::Uint(u64::from(i))));
                ops.push(Op::Close);
                run(MAX_BUFFER, &ops, &[], 0);
            }
        }
        for declared in 0u8..=2 {
            for items in 0u8..=5 {
                let mut ops = vec![Op::OpenMap(declared)];
                ops.extend((0..items).map(|i| Op::Uint(u64::from(i))));
                ops.push(Op::Close);
                run(MAX_BUFFER, &ops, &[], 0);
            }
        }
    }

    #[test]
    fn nested_containers() {
        let ops = vec![
            Op::OpenArray(1),
            Op::OpenMap(1),
            Op::Text(b"k".to_vec()),
            Op::OpenArray(2),
            Op::Uint(1),
            Op::Uint(2),
            Op::Close,
            Op::Close,
            Op::Close,
        ];
        for len in [0, 4, 8, 12, MAX_BUFFER] {
            run(len, &ops, &[], 0);
        }
    }

    /// A close with nothing open and an open past the script's depth are the
    /// two steps the C cannot be asked to do; both sides must skip the same
    /// ones and stay in step afterwards.
    #[test]
    fn steps_the_c_cannot_be_asked_to_do() {
        run(MAX_BUFFER, &[Op::Close, Op::Uint(1)], &[], 0);
        let mut ops = vec![Op::OpenArray(1); MAX_SCRIPT_DEPTH + 2];
        ops.push(Op::Uint(9));
        run(MAX_BUFFER, &ops, &[], 0);
    }

    /// What the port does with the two steps the comparator has to skip.
    /// Neither has a `CborError`, so only this pins them.
    #[test]
    fn the_ports_own_refusals() {
        let mut buf = [0u8; 16];
        let mut enc = Encoder::new(&mut buf);
        assert_eq!(enc.close_container(), Err(Error::NotInContainer));
        assert_eq!(enc.buffer_size(), 0);

        for _ in 0..bm_wire::cbor::MAX_NESTING - 1 {
            assert_eq!(enc.open_array(0), Ok(()));
        }
        assert_eq!(enc.open_array(0), Err(Error::NestingTooDeep));
    }

    #[test]
    fn float_bit_patterns_round_trip_through_both_encoders() {
        for bits in [
            0x0000_0000u32,
            0x8000_0000,
            0x3f80_0000,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0000,
            0x7f80_0001,
            0x0000_0001,
        ] {
            run(MAX_BUFFER, &[Op::Float(bits)], &[], 0);
            let mut payload = vec![0xfa];
            payload.extend_from_slice(&bits.to_be_bytes());
            decode(&payload);
        }
    }

    /// Read the C's own answer for a payload, bypassing every predicate.
    /// Only the two divergence tests below need this.
    fn c_parse(payload: &[u8]) -> (bm_wire_sys::CborError, u32, bool) {
        let mut parser = MaybeUninit::<bm_wire_sys::CborParser>::zeroed();
        let mut it = MaybeUninit::<bm_wire_sys::CborValue>::zeroed();
        // SAFETY: both out-parameters are correctly sized, and `payload`
        // describes a live slice.
        unsafe {
            let err = bm_wire_sys::cbor_parser_init(
                payload.as_ptr(),
                payload.len(),
                0,
                parser.as_mut_ptr(),
                it.as_mut_ptr(),
            );
            let it = it.assume_init();
            let value = &raw const it;
            (
                err,
                bm_wire_sys::cbor_value_get_type(value),
                bm_wire_sys::cbor_value_is_valid(value),
            )
        }
    }

    /// Divergence #40: a failed `cbor_parser_init` still reports a type and
    /// still reports valid. Major type 1 is left holding `0x20`, which is not
    /// a `CborType` constant.
    #[test]
    fn divergence_40_a_failed_parse_is_still_valid_and_typed() {
        for (payload, ty) in [
            // Additional information 28, on each major type.
            (&[0x1c][..], 0x00u32),
            (&[0x3c][..], 0x20),
            (&[0x5c][..], 0x40),
            (&[0xdc][..], 0xc0),
            // A break byte at the top level.
            (&[0xff][..], 0xe0),
            // An 8-byte argument with one byte behind it.
            (&[0x1b, 0][..], 0x00),
            (&[0x3b, 0][..], 0x20),
        ] {
            let (err, c_ty, valid) = c_parse(payload);
            assert_ne!(err, bm_wire_sys::CborError_CborNoError, "{payload:02x?}");
            assert!(valid, "cbor_value_is_valid was false for {payload:02x?}");
            assert_eq!(c_ty, ty, "cbor_value_get_type for {payload:02x?}");
        }
        // The one type value that is not in `CborType`.
        assert!(!C_TYPES.iter().any(|(_, c)| *c == 0x20));
        // And the truncated unsigned integer that reads back as its own
        // additional-information byte.
        let payload = [0x1bu8, 0];
        let rs = bm_wire::cbor::parse(&payload);
        assert!(rs.result.is_err());
        assert_eq!(rs.value.get_uint64(), Some(27));
    }

    /// Divergence #41: `-(2^63) - 1` overflows tinycbor's negation and comes
    /// back as `i64::MAX`. Asserted against the C here rather than in `check`,
    /// which holds this one input out because UBSan aborts on it.
    #[test]
    fn divergence_41_the_negative_integer_that_overflows() {
        let payload = [0x3bu8, 0x80, 0, 0, 0, 0, 0, 0, 0];
        let mut parser = MaybeUninit::<bm_wire_sys::CborParser>::zeroed();
        let mut it = MaybeUninit::<bm_wire_sys::CborValue>::zeroed();
        let mut out = 0i64;
        // SAFETY: as in `c_parse`; the value is a negative integer, which is
        // what `cbor_value_get_int64` asserts on.
        let c = unsafe {
            bm_wire_sys::cbor_parser_init(
                payload.as_ptr(),
                payload.len(),
                0,
                parser.as_mut_ptr(),
                it.as_mut_ptr(),
            );
            let it = it.assume_init();
            bm_wire_sys::cbor_value_get_int64(&raw const it, &raw mut out);
            out
        };
        assert_eq!(c, i64::MAX, "gcc and clang wrap the overflowing negation");
        assert_eq!(bm_wire::cbor::parse(&payload).value.get_int64(), Some(c));

        // One less is the largest negative integer that is well defined, and
        // it is i64::MIN.
        let payload = [0x3bu8, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(
            bm_wire::cbor::parse(&payload).value.get_int64(),
            Some(i64::MIN)
        );
    }

    /// From `cargo fuzz run cbor`: a chunk declaring `u64::MAX` bytes.
    /// tinycbor checks the bytes are there before it checks the running total
    /// for overflow, so the answer is `UnexpectedEOF`, not `DataTooLarge`.
    #[test]
    fn a_chunk_longer_than_the_address_space() {
        decode(&[
            0x7f, 0x62, b'2', b'{', 0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]);
        decode(&[0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        decode(&[
            0x5f, 0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]);
    }

    #[test]
    fn empty_payload_and_empty_buffers() {
        run(0, &[], &[], 0);
        run(0, &[Op::Uint(0)], &[], 0);
        decode(&[]);
    }
}
