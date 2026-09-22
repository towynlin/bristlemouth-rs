#![no_main]

use bm_wire_diff::cbor::{CborInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: CborInput| {
    check(&input);
});
