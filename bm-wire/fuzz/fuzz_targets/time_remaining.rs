#![no_main]

use bm_wire_diff::util::{TimeRemainingInput, check_time_remaining};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: TimeRemainingInput| {
    check_time_remaining(&input);
});
