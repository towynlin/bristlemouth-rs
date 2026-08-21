#![no_main]

use bm_wire_diff::checksum::{ChecksumInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: ChecksumInput| {
    check(&input);
});
