#![no_main]

use bm_wire_diff::util::{check_wildcard, WildcardInput};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: WildcardInput| {
    check_wildcard(&input);
});
