#![no_main]

use bm_wire_diff::util::{check_addr, AddrInput};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: AddrInput| {
    check_addr(&input);
});
