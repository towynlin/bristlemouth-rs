#![no_main]

use bm_wire_diff::info::{InfoInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: InfoInput| {
    check(&input);
});
