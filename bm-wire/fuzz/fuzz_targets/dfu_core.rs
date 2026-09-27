#![no_main]

use libfuzzer_sys::fuzz_target;

use bm_wire_diff::dfu_core::DfuCoreInput;

fuzz_target!(|input: DfuCoreInput| {
    bm_wire_diff::dfu_core::check(&input);
});
