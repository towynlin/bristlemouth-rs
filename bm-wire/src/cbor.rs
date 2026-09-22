//! CBOR, via the [`cbor2`] crate, plus the one thing bm_core needs that
//! cbor2's defaults will not give it.
//!
//! There is no codec here. [`cbor2::core`] is the codec: [`cbor2::core::Encoder`]
//! writes item heads and bodies through a `&mut [u8]`, [`cbor2::core::Decoder`]
//! reads them back, and both work `no_std` with no allocator, which is why
//! `bm-wire` can depend on it. Config values are CBOR
//! (`bcmp/configuration.c`), so the config chain uses it directly.
//!
//! # Why this module exists at all
//!
//! [`cbor2::core::Header::Float`] applies RFC 8949 §4.1 *preferred
//! serialization*: it narrows to the shortest width that holds the value
//! exactly, so `1.0` goes on the wire as `f9 3c00`.
//!
//! bm_core cannot read that. `cbor_value_is_float` tests
//! `type == CborFloatType`, which is `0xfa` and nothing else, so to a C node a
//! half-precision float is not a `FLOAT` — and `cbor_type_to_config` falls
//! through every case and refuses to classify it, which makes `set_config_cbor`
//! reject the whole value rather than misread it. Divergence #43.
//!
//! So every float written for a C node to read goes through
//! [`push_f32_wide`]. RFC 8949 permits the wider encoding; it is only
//! *preferred* serialization that does not.

pub use cbor2;

use cbor2::core::Encoder;
use cbor2::io::{Error, Write};

/// Write `value` as CBOR's 5-byte single-precision float, which is the only
/// float encoding bm_core reads.
///
/// [`cbor2::core::Header::Float`] would narrow it; see the module docs for
/// what that costs. `bm-wire-diff`'s `cbor` comparator asserts both halves of
/// that claim against the real C.
///
/// # Errors
///
/// [`cbor2::io::Error`] if the writer has no room for five bytes.
pub fn push_f32_wide<W: Write>(enc: &mut Encoder<W>, value: f32) -> Result<(), Error> {
    let mut head = [0u8; 5];
    head[0] = 0xfa;
    head[1..].copy_from_slice(&value.to_bits().to_be_bytes());
    enc.write_all(&head)
}
