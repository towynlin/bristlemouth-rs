#![no_main]

use bm_wire_diff::bcmp::{BcmpInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: BcmpInput| {
    check(&input);
});
