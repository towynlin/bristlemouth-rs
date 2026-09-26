#![no_main]

use libfuzzer_sys::fuzz_target;

use bm_wire_diff::config::ConfigInput;

fuzz_target!(|input: ConfigInput| {
    bm_wire_diff::config::check(&input);
});
