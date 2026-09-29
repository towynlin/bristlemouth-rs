#![no_main]

use bm_wire_diff::udp::{UdpInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: UdpInput| {
    check(&input);
});
