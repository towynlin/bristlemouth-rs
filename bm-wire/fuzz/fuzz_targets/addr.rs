#![no_main]

use bm_wire_diff::util::{AddrInput, check_addr};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: AddrInput| {
    check_addr(&input);
});
