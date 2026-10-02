#![no_main]

use bm_wire_diff::service_codecs::{ServiceCodecsInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: ServiceCodecsInput| {
    check(&input);
});
