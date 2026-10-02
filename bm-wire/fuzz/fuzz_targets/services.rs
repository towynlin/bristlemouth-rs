#![no_main]

use bm_wire_diff::services::{ServicesInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: ServicesInput| {
    check(&input);
});
