//! Raw FFI bindings to bm_core's C implementation.
//!
//! This crate exists to be a differential-fuzzing oracle for a pure-Rust port
//! of bm_core: it compiles the real C and exposes it verbatim. It is host-only
//! and must never be a dependency of firmware.
//!
//! bm_core leaves its platform layer (`bm_os.h`, `bm_ip.h`, ...) to the
//! integrator. This crate supplies a deterministic, single-threaded
//! implementation of that layer in `csrc/`; see README.md for the contract,
//! which differs from an RTOS in ways that matter.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
// bindgen derives PartialEq on structs of function pointers; comparing those
// is meaningless but harmless here, since we never do it.
#![allow(unpredictable_function_pointer_comparisons)]
// bindgen emits transmutes in its bitfield accessors that rustc can now do
// with a cast. Nothing we control.
#![allow(unnecessary_transmutes)]
// bindgen's flexible-array-member accessors are unsafe fns with safe-by-default
// bodies, which edition 2024 warns about. Also nothing we control.
#![allow(unsafe_op_in_unsafe_fn)]
// Clippy's findings here are all in the same generated code: bitfield
// accessors that transmute a type to itself, `offset` where `add` would do,
// unsafe fns with no `# Safety` section, and the `(bits + 7) / 8` byte counts
// bindgen 0.73 emits in its bitfield get/set. Silencing them crate-wide is
// safe because this crate is nothing but `include!`d bindings -- and it means
// a lint that does fire is one worth reading.
#![allow(
    clippy::useless_transmute,
    clippy::ptr_offset_with_cast,
    clippy::missing_safety_doc,
    clippy::manual_div_ceil
)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
