#![no_main]

use libfuzzer_sys::fuzz_target;

use bm_wire_diff::time::TimeInput;

fuzz_target!(|input: TimeInput| {
    bm_wire_diff::time::check(&input);
});
