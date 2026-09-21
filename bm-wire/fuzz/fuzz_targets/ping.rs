#![no_main]

use bm_wire_diff::ping::{PingInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: PingInput| {
    check(&input);
});
