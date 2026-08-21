//! Differential harness: run the same input through [`bm_wire`] and through the
//! real bm_core C in `bm_wire_sys`, and assert the results are identical.
//!
//! Every comparator here is a plain function taking a structured input, so the
//! same code backs three things:
//!
//! * `cargo fuzz` targets in `bm-wire/fuzz/`, which supply inputs from libFuzzer;
//! * `#[test]`s in this crate, which replay the committed corpus and the gold
//!   vectors lifted from bm_core's gtest suite;
//! * regression tests added by hand when a fuzzer finds a crash.
//!
//! A comparator panics on divergence. That is what libFuzzer reports and what
//! `cargo test` reports, so the two agree by construction.
//!
//! # Which side is authoritative
//!
//! The C is. Where bm_core does something surprising, `bm-wire` reproduces it
//! and the surprise is recorded in `docs/c-divergences.md` for upstream repair.
//! A comparator must never be relaxed to paper over a real behavioural
//! difference — narrow the *input domain* instead, and say why.

pub mod bcmp;
pub mod checksum;
pub mod crc;
pub mod l2_egress;
pub mod l2_policy;
pub mod replay;
pub mod util;

/// Inputs restricted to a domain where the C has defined behaviour.
///
/// bm_core has at least one function that reads out of bounds outside its
/// documented range (see [`util::DateTimeInput`]). Comparing against undefined
/// behaviour is meaningless, so those inputs are constrained here rather than
/// having the comparator ignore the mismatch.
pub trait Domain {
    /// Force this input into the valid domain.
    fn clamp_to_domain(&mut self);
}
