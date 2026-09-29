#![no_main]

use bm_wire_diff::node_udp::{NodeUdpInput, check};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: NodeUdpInput| {
    check(&input);
});
