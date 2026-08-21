#![no_main]

use bm_wire_diff::crc::{check, CrcInput};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: CrcInput| {
    check(&input);
});
