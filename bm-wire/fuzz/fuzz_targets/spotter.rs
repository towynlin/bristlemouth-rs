#![no_main]

use bm_wire_diff::spotter::{SpotterInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: SpotterInput| {
    check(&input);
});
