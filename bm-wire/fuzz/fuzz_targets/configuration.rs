#![no_main]

use bm_wire_diff::configuration::{ConfigInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: ConfigInput| {
    check(&input);
});
