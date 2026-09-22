//! A pure, safe, idiomatic Rust port of [bm_core](https://github.com/bristlemouth/bm_core).
//!
//! The port exists to run as firmware on Bristlemouth dev kits while staying
//! bit-compatible with nodes running the C/C++ firmware. Compatibility is not
//! assumed: every function here has a differential fuzz target in `bm-wire-diff`
//! that feeds identical input to this code and to the real C, and asserts the
//! outputs are identical.
//!
//! Where the C does something surprising, this crate reproduces it and the
//! surprise is written down in `docs/c-divergences.md`. On-wire compatibility
//! with deployed nodes wins over cleanliness; the cleanup happens upstream.
//!
//! # Constraints
//!
//! `no_std`, no `alloc`. Callers provide every buffer. Nothing here panics on
//! untrusted input — parsers return [`BmWireError`] instead.
//!
//! One dependency, [`cbor2`], for the config chain's CBOR values; it is
//! `no_std` and alloc-free in the configuration used here. See
//! `docs/c-divergences.md` for where it and bm_core's vendored tinycbor
//! disagree, and `bm-wire-diff/src/cbor.rs` for what is proven about the
//! bytes they both produce.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod addr;
pub mod bcmp;
pub mod cbor;
pub mod checksum;
pub mod crc;
pub mod frame;
pub mod l2;
pub mod l2_policy;
pub mod neighbor;
pub mod util;

/// Why a parse or encode could not be completed.
///
/// bm_core signals most of these by returning a zero value or silently doing
/// nothing, so a `Result` here often corresponds to a C function that quietly
/// succeeds. Where that is true the differential harness compares against the
/// value C produces, not against the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum BmWireError {
    /// The buffer was too short to hold the field being read or written.
    Truncated,
    /// A field held a value the wire format does not allow.
    Invalid,
}

impl core::fmt::Display for BmWireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => f.write_str("buffer too short"),
            Self::Invalid => f.write_str("invalid field value"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BmWireError {}
