#![no_main]

use bm_wire_diff::util::{check_strnlen, StrnlenInput};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: StrnlenInput| {
    check_strnlen(&input);
});
