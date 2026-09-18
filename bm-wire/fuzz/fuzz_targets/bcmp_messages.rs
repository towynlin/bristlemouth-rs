#![no_main]

use bm_wire_diff::bcmp_messages::{BcmpMessagesInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: BcmpMessagesInput| {
    check(&input);
});
