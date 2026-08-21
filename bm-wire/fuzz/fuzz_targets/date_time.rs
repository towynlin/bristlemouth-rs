#![no_main]

use bm_wire_diff::util::{check_date_time, DateTimeInput};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: DateTimeInput| {
    check_date_time(&input);
});
