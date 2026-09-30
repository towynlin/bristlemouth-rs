#![no_main]

use bm_wire_diff::pubsub::{PubSubInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: PubSubInput| {
    check(&input);
});
