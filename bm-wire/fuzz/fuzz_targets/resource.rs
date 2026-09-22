#![no_main]

use bm_wire_diff::resource::{ResourceInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: ResourceInput| {
    check(&input);
});
