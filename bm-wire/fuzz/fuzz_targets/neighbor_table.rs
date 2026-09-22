#![no_main]

use bm_wire_diff::neighbor_table::{NeighborTableInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: NeighborTableInput| {
    check(&input);
});
