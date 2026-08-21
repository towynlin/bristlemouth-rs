#![no_main]

use bm_wire_diff::l2_policy::{L2PolicyInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: L2PolicyInput| {
    check(&input);
});
