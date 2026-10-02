#![no_main]

use bm_wire_diff::metrics_codec::{MetricsCodecInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: MetricsCodecInput| {
    check(&input);
});
