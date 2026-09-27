#![no_main]

use bm_wire_diff::dfu_codec::{DfuCodecInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: DfuCodecInput| {
    check(&input);
});
