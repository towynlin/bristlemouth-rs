#![no_main]

use bm_wire_diff::services::{ServicesInput, check};
use libfuzzer_sys::fuzz_target;

/// `strict_memcmp=0`: ASan checks the bytes `memcmp` compares up to the first
/// difference, not all `n`. `bm_shim_stack_init` lists `<id>/metrics/req`
/// first in `SUB_LIST`, and every longer topic, `<id>/sys_info/req`
/// included, is then compared past that entry's allocation (divergence #38).
/// The bytes past it still decide nothing, and ASan still reports a needle
/// the entry prefixes.
#[unsafe(no_mangle)]
pub extern "C" fn __asan_default_options() -> *const core::ffi::c_char {
    c"strict_memcmp=0".as_ptr()
}

fuzz_target!(|input: ServicesInput| {
    check(&input);
});
