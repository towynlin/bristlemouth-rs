//! Differential comparators for the `cbor2` crate against the tinycbor
//! bm_core vendors.
//!
//! `bm-wire` does not carry a CBOR codec of its own; it depends on `cbor2`.
//! So what this file proves is narrower and more useful than a port
//! comparison: **for the values bm_core actually stores, cbor2 and tinycbor
//! put the same bytes on the wire, and read the same item back off it.**
//!
//! tinycbor is pure — no shim state, no allocation, no clock — so this
//! comparator needs nothing brought up and its target lives in
//! [`crate::replay::TARGETS`].
//!
//! # What is compared, and what is not
//!
//! | Compared | Not compared |
//! |---|---|
//! | Encoded bytes, for every value shape `bcmp/configuration.c` stores | Buffer-overflow behaviour: cbor2 fails the write, tinycbor keeps counting and reports a shortfall. Neither is visible on the wire. |
//! | The head of one decoded item: kind, argument, and whether the length is known | Container item counts: tinycbor's encoder tracks them and cbor2's does not, by design. |
//! | Definite-length string bodies | Indefinite-length string *reassembly*, which cbor2 leaves to the caller without `alloc`. C2 needs a helper; see `docs/bcmp-port-todo.md`. |
//!
//! # The two places they disagree
//!
//! Both are asserted here rather than papered over, so a cbor2 upgrade that
//! changes either fails CI.
//!
//! 1. **Floats.** `Header::Float` applies RFC 8949 preferred serialization and
//!    narrows to the shortest lossless width, so `1.0` encodes as `f9 3c00`.
//!    tinycbor's `cbor_encode_float` always emits the 5-byte `fa` form, and
//!    `cbor_value_is_float` accepts *only* `fa` — so a half-precision float is
//!    not a `FLOAT` to bm_core, and `cbor_type_to_config` rejects the value
//!    outright. Anything writing a config value for a C node to read must push
//!    the wide form, which [`bm_wire::cbor::push_f32_wide`] is. Divergence #43.
//! 2. **A break byte at the top level.** tinycbor reports
//!    `CborErrorUnexpectedBreak`; cbor2 returns `Ok(Header::Break)` and leaves
//!    the judgement to the caller. Divergence #40.

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::cbor::push_f32_wide;
use cbor2::core::{Decoder, Encoder, Header};

/// One value-encoding step, in the shapes `bcmp/configuration.c` stores.
#[derive(Debug, Clone)]
pub enum Op {
    /// `UINT32`, though the whole `u64` range is encoded.
    Uint(u64),
    /// `INT32`, over the whole `i64` range.
    Int(i64),
    /// `FLOAT`, carried as bits so a NaN compares as itself.
    Float(u32),
    /// `STR`. No UTF-8 constraint: tinycbor does not validate, and cbor2's
    /// raw header path does not either.
    Text(Vec<u8>),
    /// `BYTES`.
    Bytes(Vec<u8>),
    /// The map `services_cbor_as_map` builds.
    OpenMap(u8),
    /// `ARRAY`.
    OpenArray(u8),
}

/// An encode script plus a decode payload.
#[derive(Debug, Clone)]
pub struct CborInput {
    /// Values to encode, in order.
    pub ops: Vec<Op>,
    /// Bytes handed to both decoders. Unconstrained: a `ConfigSet` (`0xA2`)
    /// body is whatever the sender put in it.
    pub payload: Vec<u8>,
}

/// A string body, short enough to keep the fuzzer's inputs dense.
fn bounded_bytes(u: &mut Unstructured<'_>, max: usize) -> Result<Vec<u8>> {
    let len = u.int_in_range(0..=max)?;
    let mut bytes = vec![0u8; len];
    u.fill_buffer(&mut bytes)?;
    Ok(bytes)
}

impl<'a> Arbitrary<'a> for Op {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(match u.int_in_range(0u8..=6)? {
            0 => Self::Uint(u.arbitrary()?),
            1 => Self::Int(u.arbitrary()?),
            2 => Self::Float(u.arbitrary()?),
            3 => Self::Text(bounded_bytes(u, 48)?),
            4 => Self::Bytes(bounded_bytes(u, 48)?),
            5 => Self::OpenMap(u.int_in_range(0u8..=4)?),
            _ => Self::OpenArray(u.int_in_range(0u8..=4)?),
        })
    }
}

impl<'a> Arbitrary<'a> for CborInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let op_count = u.int_in_range(0usize..=10)?;
        let mut ops = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            ops.push(u.arbitrary()?);
        }
        Ok(Self {
            ops,
            payload: bounded_bytes(u, 48)?,
        })
    }
}

/// Assert cbor2 agrees with tinycbor for this input.
///
/// # Panics
///
/// On any divergence outside the two recorded in this module's docs.
pub fn check(input: &CborInput) {
    check_encode(&input.ops);
    check_decode(&input.payload);
}

/// Room for any script this comparator builds: ten ops, each at most a 9-byte
/// head plus a 48-byte body.
const SCRATCH: usize = 1024;

/// Encode `ops` through both libraries and compare the bytes.
///
/// Both buffers are large enough that neither runs out, because the two
/// overflow contracts genuinely differ and the wire never sees them; see the
/// module docs.
fn check_encode(ops: &[Op]) {
    let mut rust_buf = [0u8; SCRATCH];
    let rust_len = {
        let mut tail: &mut [u8] = &mut rust_buf;
        {
            let mut enc = Encoder::from(&mut tail);
            for op in ops {
                let pushed = match op {
                    Op::Uint(v) => enc.push(Header::Positive(*v)),
                    // CBOR encodes a negative integer as `-1 - argument`, and
                    // cbor2's `Negative` carries that argument, so the
                    // complement is the conversion. tinycbor's
                    // `cbor_encode_int` does the same thing with a sign-extend
                    // and an xor.
                    Op::Int(v) if *v < 0 => enc.push(Header::Negative(!(*v as u64))),
                    Op::Int(v) => enc.push(Header::Positive(*v as u64)),
                    // Not `Header::Float`: see `push_f32_wide`.
                    Op::Float(bits) => push_f32_wide(&mut enc, f32::from_bits(*bits)),
                    Op::Text(bytes) => enc
                        .push(Header::Text(Some(bytes.len())))
                        .and_then(|()| enc.write_all(bytes)),
                    Op::Bytes(bytes) => enc
                        .push(Header::Bytes(Some(bytes.len())))
                        .and_then(|()| enc.write_all(bytes)),
                    Op::OpenMap(n) => enc.push(Header::Map(Some(usize::from(*n)))),
                    Op::OpenArray(n) => enc.push(Header::Array(Some(usize::from(*n)))),
                };
                pushed.expect("the scratch buffer is sized for any script");
            }
        }
        SCRATCH - tail.len()
    };

    let mut c_buf = [0u8; SCRATCH];
    let c_len = run_c_encoder(&mut c_buf, ops);

    assert_eq!(
        &c_buf[..c_len],
        &rust_buf[..rust_len],
        "cbor2 and tinycbor encoded {ops:?} differently"
    );
}

/// Drive tinycbor's encoder through the same script.
///
/// Containers are opened and never closed, exactly as the cbor2 side does:
/// `cbor_encoder_close_container` writes nothing for a definite-length
/// container, it only checks the item count, and cbor2's encoder does not
/// track item counts at all. Nothing that reaches the wire is skipped.
fn run_c_encoder(buf: &mut [u8], ops: &[Op]) -> usize {
    let mut enc = bm_wire_sys::CborEncoder::default();
    let ptr = buf.as_mut_ptr();
    // SAFETY: `enc` is a live, correctly-sized CborEncoder and `ptr`/`len`
    // describe a buffer that outlives every call below.
    unsafe { bm_wire_sys::cbor_encoder_init(&raw mut enc, ptr, buf.len(), 0) };

    for op in ops {
        // SAFETY: `enc` stays live for the whole loop, and every pointer and
        // length pair comes from a slice that outlives its call. A container
        // is written through the same encoder rather than a child, which is
        // what `create_container` would do to it anyway minus the item count.
        let err = unsafe {
            let e = &raw mut enc;
            match op {
                Op::Uint(v) => bm_wire_sys::cbor_encode_uint(e, *v),
                Op::Int(v) => bm_wire_sys::cbor_encode_int(e, *v),
                Op::Float(bits) => bm_wire_sys::cbor_encode_float(e, f32::from_bits(*bits)),
                Op::Text(bytes) => {
                    bm_wire_sys::cbor_encode_text_string(e, bytes.as_ptr().cast(), bytes.len())
                }
                Op::Bytes(bytes) => {
                    bm_wire_sys::cbor_encode_byte_string(e, bytes.as_ptr(), bytes.len())
                }
                // `create_container` writes the head through the *child*
                // and leaves the parent's cursor where it was until a close
                // resyncs it. Nothing is ever closed here, so the child
                // becomes the working encoder; for a definite-length
                // container that loses only the item count, which
                // `close_container` checks and never writes.
                Op::OpenMap(n) | Op::OpenArray(n) => {
                    let mut child = bm_wire_sys::CborEncoder::default();
                    let err = if matches!(op, Op::OpenMap(_)) {
                        bm_wire_sys::cbor_encoder_create_map(e, &raw mut child, usize::from(*n))
                    } else {
                        bm_wire_sys::cbor_encoder_create_array(e, &raw mut child, usize::from(*n))
                    };
                    enc = child;
                    err
                }
            }
        };
        assert_eq!(
            err,
            bm_wire_sys::CborError_CborNoError,
            "tinycbor refused {op:?} into a {SCRATCH}-byte buffer"
        );
    }

    // SAFETY: `enc` is live and `ptr` is the buffer it was initialised with.
    unsafe { bm_wire_sys::cbor_encoder_get_buffer_size(&raw const enc, ptr) }
}

/// One decoded item head, normalised so the two libraries can be compared.
///
/// Floats carry their wire width as well as their value: the width is what
/// `cbor_value_is_float` discriminates on, so two floats of equal value and
/// different width are not interchangeable to bm_core.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Positive(u64),
    /// The encoded argument, i.e. the `n` of `-1 - n`, as both libraries
    /// report it.
    Negative(u64),
    Bytes(Option<u64>),
    Text(Option<u64>),
    Array(Option<u64>),
    Map(Option<u64>),
    Tag(u64),
    Simple(u8),
    /// `(additional information, the f32 bits)`, the second only for `fa`.
    Float(u8, Option<u32>),
    Break,
}

/// Decode one item head with both libraries and compare.
fn check_decode(payload: &[u8]) {
    let c = c_head(payload);
    let mut decoder = Decoder::from(payload);
    let rust = decoder
        .pull()
        .ok()
        .map(|header| cbor2_item(header, payload));

    match (c, rust) {
        (Some(c), Some(rust)) => assert!(
            same_item(c, rust),
            "cbor2 and tinycbor read a different item from {payload:02x?}: \
             tinycbor {c:?}, cbor2 {rust:?}"
        ),
        (None, None) => {}
        // The one accepted asymmetry, divergence #40: tinycbor calls a
        // top-level break `CborErrorUnexpectedBreak`; cbor2 hands it back and
        // lets the caller decide. Neither reads a value out of it.
        (None, Some(Item::Break)) => {}
        (c, rust) => panic!(
            "cbor2 and tinycbor disagreed on whether {payload:02x?} is an item: \
             tinycbor {c:?}, cbor2 {rust:?}"
        ),
    }
}

/// Whether the two libraries read the same item.
///
/// Equality, except for NaN: cbor2's decoder widens every float to `f64`, and
/// that conversion quiets a signalling NaN, so the payload bits of a `fa`
/// NaN do not survive it. tinycbor copies the four bytes out untouched.
/// Divergence #43 records it. Every non-NaN `f32` round-trips through `f64`
/// exactly, so this is the whole of the float difference on the read side.
fn same_item(c: Item, rust: Item) -> bool {
    match (c, rust) {
        (Item::Float(cw, Some(cb)), Item::Float(rw, Some(rb))) => {
            cw == rw && (cb == rb || (f32::from_bits(cb).is_nan() && f32::from_bits(rb).is_nan()))
        }
        _ => c == rust,
    }
}

/// Normalise a cbor2 header. `payload` supplies the wire width of a float,
/// which `Header::Float` has already widened away.
fn cbor2_item(header: Header, payload: &[u8]) -> Item {
    let width = payload.first().map_or(0, |b| b & 0x1f);
    match header {
        Header::Positive(v) => Item::Positive(v),
        Header::Negative(v) => Item::Negative(v),
        Header::Bytes(len) => Item::Bytes(len.map(|l| l as u64)),
        Header::Text(len) => Item::Text(len.map(|l| l as u64)),
        Header::Array(len) => Item::Array(len.map(|l| l as u64)),
        Header::Map(len) => Item::Map(len.map(|l| l as u64)),
        Header::Tag(v) => Item::Tag(v),
        Header::Simple(v) => Item::Simple(v),
        Header::Break => Item::Break,
        Header::Float(v) => Item::Float(
            width,
            // Only the 4-byte form is compared by value: tinycbor's
            // half-float reader lives in `cborparser_float.c`, which
            // `build.rs` does not compile, so there is no oracle for `f9`.
            (width == 26).then_some(v as f32).map(f32::to_bits),
        ),
    }
}

/// tinycbor's view of the same head, or `None` if `cbor_parser_init` failed.
fn c_head(payload: &[u8]) -> Option<Item> {
    let mut parser = std::mem::MaybeUninit::<bm_wire_sys::CborParser>::zeroed();
    let mut it = std::mem::MaybeUninit::<bm_wire_sys::CborValue>::zeroed();
    // SAFETY: both out-parameters are correctly sized and aligned, and
    // `payload` describes a live slice tinycbor never reads past the end of.
    let (err, it) = unsafe {
        let err = bm_wire_sys::cbor_parser_init(
            payload.as_ptr(),
            payload.len(),
            0,
            parser.as_mut_ptr(),
            it.as_mut_ptr(),
        );
        (err, it.assume_init())
    };
    if err != bm_wire_sys::CborError_CborNoError {
        return None;
    }
    let value = &raw const it;
    let width = payload.first().map_or(0, |b| b & 0x1f);

    // SAFETY: every accessor below is called behind the predicate its C
    // assertion names, and `value` points at the initialised CborValue.
    unsafe {
        let len = |known: bool| -> Option<u64> {
            let mut out = 0usize;
            known.then(|| {
                let e = bm_wire_sys::cbor_value_get_string_length(value, &raw mut out);
                assert_eq!(e, bm_wire_sys::CborError_CborNoError);
                out as u64
            })
        };
        let known = bm_wire_sys::cbor_value_is_length_known(value);

        if bm_wire_sys::cbor_value_is_unsigned_integer(value) {
            let mut out = 0u64;
            bm_wire_sys::cbor_value_get_uint64(value, &raw mut out);
            return Some(Item::Positive(out));
        }
        if bm_wire_sys::cbor_value_is_negative_integer(value) {
            // Read the argument off the wire rather than through
            // `cbor_value_get_int64`, which overflows for arguments at or
            // above `1 << 63` (divergence #41) and is undefined there.
            return Some(Item::Negative(argument(payload)));
        }
        if bm_wire_sys::cbor_value_is_byte_string(value) {
            return Some(Item::Bytes(len(known)));
        }
        if bm_wire_sys::cbor_value_is_text_string(value) {
            return Some(Item::Text(len(known)));
        }
        if bm_wire_sys::cbor_value_is_array(value) {
            let mut out = 0usize;
            return Some(Item::Array(known.then(|| {
                bm_wire_sys::cbor_value_get_array_length(value, &raw mut out);
                out as u64
            })));
        }
        if bm_wire_sys::cbor_value_is_map(value) {
            let mut out = 0usize;
            return Some(Item::Map(known.then(|| {
                bm_wire_sys::cbor_value_get_map_length(value, &raw mut out);
                out as u64
            })));
        }
        if bm_wire_sys::cbor_value_is_tag(value) {
            let mut out = 0u64;
            bm_wire_sys::cbor_value_get_tag(value, &raw mut out);
            return Some(Item::Tag(out));
        }
        if bm_wire_sys::cbor_value_is_float(value) {
            let mut out = 0f32;
            bm_wire_sys::cbor_value_get_float(value, &raw mut out);
            return Some(Item::Float(width, Some(out.to_bits())));
        }
        // Half and double floats: no value oracle, only the width. See
        // `cbor2_item`.
        if matches!(
            u8::try_from(bm_wire_sys::cbor_value_get_type(value)).unwrap(),
            0xf9 | 0xfb
        ) {
            return Some(Item::Float(width, None));
        }
        // Booleans, null and undefined are major type 7 values that tinycbor
        // gives their own `CborType`; cbor2 leaves them as simple values.
        Some(Item::Simple(
            match u8::try_from(bm_wire_sys::cbor_value_get_type(value)).unwrap() {
                0xf5 => payload[0] & 0x1f, // false is 20, true is 21
                0xf6 => 22,
                0xf7 => 23,
                _ => {
                    let mut out = 0u8;
                    bm_wire_sys::cbor_value_get_simple_type(value, &raw mut out);
                    out
                }
            },
        ))
    }
}

/// The argument of the head at the start of `payload`, read off the wire.
fn argument(payload: &[u8]) -> u64 {
    let low = payload[0] & 0x1f;
    if low < 24 {
        return u64::from(low);
    }
    let n = 1usize << (low - 24);
    payload[1..=n]
        .iter()
        .fold(0u64, |a, &b| (a << 8) | u64::from(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ops: &[Op], payload: &[u8]) {
        check(&CborInput {
            ops: ops.to_vec(),
            payload: payload.to_vec(),
        });
    }

    fn encode(ops: &[Op]) -> Vec<u8> {
        let mut buf = [0u8; SCRATCH];
        let n = run_c_encoder(&mut buf, ops);
        check_encode(ops);
        buf[..n].to_vec()
    }

    /// `float baz = 3.14159;` from `cbor_service_helper_test.cpp`, as bits.
    /// The literal is what the gold vector encodes; `f32::consts::PI` is
    /// `0x4049_0fdb` and would not reproduce it.
    const GOLD_BAZ: u32 = 0x4049_0fd0;

    /// The map `cbor_service_helper_test.cpp` builds, byte for byte, plus the
    /// 91-byte total it asserts. cbor2 has to produce it too.
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
        ];
        let got = encode(&ops);

        let mut expected = Vec::new();
        expected.extend_from_slice(&[0xa5, 0x63, b'f', b'o', b'o', 0x18, 0x2a]);
        expected.extend_from_slice(&[0x63, b'b', b'a', b'r', 0x39, 0x03, 0xe7]);
        expected.extend_from_slice(&[0x63, b'b', b'a', b'z', 0xfa, 0x40, 0x49, 0x0f, 0xd0]);
        expected.extend_from_slice(&[0x65, b's', b'i', b'l', b'l', b'y', 0x78, 0x2b]);
        expected.extend_from_slice(SILLY);
        expected.extend_from_slice(&[0x65, b'b', b'y', b't', b'e', b's', 0x4a]);
        expected.extend_from_slice(BYTES);

        assert_eq!(got.len(), 91, "the gtest asserts 91 bytes");
        assert_eq!(got, expected);
        // The eleven offsets the gtest checks by hand.
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

    /// Divergence #43. `Header::Float` narrows to the shortest lossless
    /// width; bm_core reads only the 5-byte form. This is the whole reason
    /// [`bm_wire::cbor::push_f32_wide`] exists, and the table is the evidence that skipping
    /// it breaks interoperability rather than merely wasting two bytes.
    #[test]
    fn cbor2_narrows_floats_and_bm_core_cannot_read_them() {
        for (value, preferred, wide) in [
            (
                1.0f32,
                &[0xf9, 0x3c, 0x00][..],
                &[0xfa, 0x3f, 0x80, 0x00, 0x00][..],
            ),
            (0.0, &[0xf9, 0x00, 0x00], &[0xfa, 0x00, 0x00, 0x00, 0x00]),
            (-0.0, &[0xf9, 0x80, 0x00], &[0xfa, 0x80, 0x00, 0x00, 0x00]),
            (0.5, &[0xf9, 0x38, 0x00], &[0xfa, 0x3f, 0x00, 0x00, 0x00]),
            (
                f32::INFINITY,
                &[0xf9, 0x7c, 0x00],
                &[0xfa, 0x7f, 0x80, 0x00, 0x00],
            ),
        ] {
            let mut buf = [0u8; 16];
            let n = {
                let mut tail: &mut [u8] = &mut buf;
                {
                    let mut enc = Encoder::from(&mut tail);
                    enc.push(Header::Float(f64::from(value))).unwrap();
                }
                16 - tail.len()
            };
            assert_eq!(&buf[..n], preferred, "cbor2's preferred form for {value}");

            let mut buf = [0u8; 16];
            let n = {
                let mut tail: &mut [u8] = &mut buf;
                {
                    let mut enc = Encoder::from(&mut tail);
                    push_f32_wide(&mut enc, value).unwrap();
                }
                16 - tail.len()
            };
            assert_eq!(&buf[..n], wide, "push_f32_wide's form for {value}");

            // What bm_core makes of each: only the wide form is a FLOAT, and
            // `cbor_type_to_config` refuses the narrow one outright, so the
            // whole ConfigSet would be rejected rather than misread.
            assert!(!c_is_float(preferred), "bm_core must not see f9 as a float");
            assert!(c_is_float(wide), "bm_core must see fa as a float");
            assert!(
                !c_classifies(preferred),
                "cbor_type_to_config must refuse the narrow form"
            );
            assert!(c_classifies(wide));
        }

        // cbor2 also quiets a signalling NaN on the way through f64, so even
        // the width is not the whole story.
        let mut buf = [0u8; 16];
        let n = {
            let mut tail: &mut [u8] = &mut buf;
            {
                let mut enc = Encoder::from(&mut tail);
                enc.push(Header::Float(f64::from(f32::from_bits(0x7f80_0001))))
                    .unwrap();
            }
            16 - tail.len()
        };
        assert_eq!(&buf[..n], &[0xfa, 0x7f, 0xc0, 0x00, 0x01]);
    }

    /// `cbor_value_is_float` over a payload, which is what `get_config_float`
    /// gates on.
    fn c_is_float(payload: &[u8]) -> bool {
        with_c_value(payload, |v| unsafe { bm_wire_sys::cbor_value_is_float(v) })
    }

    /// `cbor_type_to_config`, which is what `set_config_cbor` gates on.
    fn c_classifies(payload: &[u8]) -> bool {
        with_c_value(payload, |v| unsafe {
            let mut ty = 0u32;
            bm_wire_sys::cbor_type_to_config(v, &raw mut ty)
        })
    }

    fn with_c_value<T>(payload: &[u8], f: impl FnOnce(*const bm_wire_sys::CborValue) -> T) -> T {
        let mut parser = std::mem::MaybeUninit::<bm_wire_sys::CborParser>::zeroed();
        let mut it = std::mem::MaybeUninit::<bm_wire_sys::CborValue>::zeroed();
        // SAFETY: both out-parameters are correctly sized; `payload` is live.
        let it = unsafe {
            bm_wire_sys::cbor_parser_init(
                payload.as_ptr(),
                payload.len(),
                0,
                parser.as_mut_ptr(),
                it.as_mut_ptr(),
            );
            it.assume_init()
        };
        f(&raw const it)
    }

    #[test]
    fn every_first_byte_reads_the_same_way() {
        for byte in 0u8..=255 {
            run(&[], &[byte]);
            run(&[], &[byte, 1, 2, 3, 4, 5, 6, 7, 8]);
            run(&[], &[byte, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        }
    }

    #[test]
    fn truncated_heads() {
        for byte in [
            0x18u8, 0x19, 0x1a, 0x1b, 0x38, 0x3a, 0x3b, 0xf8, 0xf9, 0xfa, 0xfb,
        ] {
            for len in 0..9 {
                let mut payload = vec![byte];
                payload.extend(core::iter::repeat_n(0x5au8, len));
                run(&[], &payload);
            }
        }
    }

    #[test]
    fn indefinite_and_malformed_heads() {
        for payload in [
            &[0x7f, 0x63, b'f', b'o', b'o', 0xff][..],
            &[0x5f, 0x41, 0xde, 0xff][..],
            &[0x7f][..],
            &[0x5f][..],
            &[0x9f][..],
            &[0xbf][..],
            &[0xff][..], // the accepted asymmetry
            &[0x1c][..],
            &[0x1d][..],
            &[0x1e][..],
            &[0x3f][..],
            &[0xdf][..],
            &[0xff, 0xff][..],
            &[0x78, 0x40, b'a'][..],
        ] {
            run(&[], payload);
        }
    }

    /// The whole `i64` and `u64` head-width ladder, both libraries.
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
            run(&[Op::Uint(value)], &[]);
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
            run(&[Op::Int(value)], &[]);
        }
        // The negative integer tinycbor's `cbor_value_get_int64` cannot read
        // without overflowing (divergence #41). cbor2 hands back the argument
        // and never overflows, so both sides agree on the head.
        run(&[], &[0x3b, 0x80, 0, 0, 0, 0, 0, 0, 0]);
        run(&[], &[0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }

    /// The five values `cbor_service_helper_test.cpp` stores, each alone,
    /// which is the shape `set_config_cbor` is handed.
    #[test]
    fn the_cbor_service_helper_gold_values() {
        assert_eq!(encode(&[Op::Uint(42)]), [0x18, 0x2a]);
        assert_eq!(encode(&[Op::Int(-1000)]), [0x39, 0x03, 0xe7]);
        assert_eq!(
            encode(&[Op::Float(GOLD_BAZ)]),
            [0xfa, 0x40, 0x49, 0x0f, 0xd0]
        );
        assert_eq!(
            encode(&[Op::Bytes(vec![0xde, 0xad, 0xbe, 0xef])]),
            [0x44, 0xde, 0xad, 0xbe, 0xef]
        );
        let silly = b"The quick brown fox jumps over the lazy dog";
        let mut expect = vec![0x78, 0x2b];
        expect.extend_from_slice(silly);
        assert_eq!(encode(&[Op::Text(silly.to_vec())]), expect);
    }

    /// What cbor2 does *not* do that tinycbor does, so C3 does not discover
    /// it the hard way. Neither is visible on the wire; both change what the
    /// caller must do.
    #[test]
    fn the_contracts_that_do_not_carry_over() {
        // 1. No item counting. tinycbor's close reports TooFewItems; cbor2
        //    has no close and will happily emit a map that lies about its
        //    length.
        let short = encode(&[Op::OpenMap(5), Op::Uint(1)]);
        assert_eq!(short, [0xa5, 0x01], "cbor2 emits a map declaring 5 pairs");

        // 2. No shortfall accounting. tinycbor keeps counting past the end of
        //    the buffer and reports how much more it needed; cbor2's slice
        //    writer just fails. `serialized_size` is the replacement, and it
        //    is available without `alloc`.
        let mut tiny = [0u8; 2];
        let mut tail: &mut [u8] = &mut tiny;
        let mut enc = Encoder::from(&mut tail);
        assert!(enc.push(Header::Positive(1_000_000)).is_err());
    }

    #[test]
    fn empty_input() {
        run(&[], &[]);
    }
}
