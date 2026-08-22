#![no_main]

use bm_wire_diff::neighbor::{NeighborInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: NeighborInput| {
    check(&input);
});
