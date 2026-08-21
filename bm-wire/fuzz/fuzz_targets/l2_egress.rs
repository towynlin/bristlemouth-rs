#![no_main]

use bm_wire_diff::l2_egress::{L2EgressInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: L2EgressInput| {
    check(&input);
});
