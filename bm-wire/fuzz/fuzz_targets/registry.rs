#![no_main]

use bm_wire_diff::registry::{RegistryInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: RegistryInput| {
    check(&input);
});
