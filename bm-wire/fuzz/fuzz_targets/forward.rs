#![no_main]

use libfuzzer_sys::fuzz_target;

use bm_wire_diff::forward::ForwardInput;

fuzz_target!(|input: ForwardInput| {
    bm_wire_diff::forward::check(&input);
});
