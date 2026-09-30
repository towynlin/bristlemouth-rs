//! `spotter_log` and `spotter_tx_data` through `bm_stack::Node`, compared
//! against the oracle's whole stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire::spotter::{self, NetworkType};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::spotter::{
    FILE_NAME_BYTES, Log, SpotterInput, Step, TEXT_BYTES, TxData, check, check_log, check_tx_data,
};

fn log(file_name: Option<&[u8]>, text: &[u8], padding: u16) -> Log {
    Log {
        target_node_id: 0x0123_4567_89ab_cdef,
        file_name: file_name.map(<[u8]>::to_vec),
        print_time: spotter::USE_TIMESTAMP,
        text: text.to_vec(),
        padding,
    }
}

fn tx(network: NetworkType, len: usize) -> TxData {
    TxData {
        network: network.0,
        data: (0..=255).cycle().take(len).collect(),
        padding: 0,
    }
}

/// `spotter_log_console` and `spotter_log` with a file name.
#[test]
fn console_and_file() {
    check_log(&log(None, b"hello world", 0));
    check_log(&log(Some(b"hello.log"), b"hello world", 0));
    check_log(&log(Some(b""), b"empty file name", 0));
}

/// `BmENODATA`, and a file name read to its NUL.
#[test]
fn empty_text_and_nul_in_the_file_name() {
    check_log(&log(None, b"", 0));
    check_log(&log(Some(b"x"), b"\0after the NUL", 0));
    check_log(&log(Some(b"abc\0def"), b"text", 0));
}

/// File names either side of `max_file_name_len`.
#[test]
fn file_name_lengths() {
    for len in [63, 64, 65, FILE_NAME_BYTES] {
        check_log(&log(Some(&vec![b'f'; len]), b"x", 0));
    }
}

/// Divergence #81: text lengths either side of the pub/sub limit and of
/// `max_str_len`, to each topic.
#[test]
fn text_lengths() {
    let printf_fits = bm_wire::pubsub::MAX_MESSAGE_LEN
        - bm_wire::pubsub::HEADER_LEN
        - spotter::PRINTF_TOPIC.len()
        - spotter::LOG_HEADER_LEN
        - 1;
    for name in [None, Some(&b"f"[..]), Some(&[b'f'; 63][..])] {
        let name_len = name.map_or(0, <[u8]>::len);
        let max = spotter::max_text_len(name_len);
        for len in [
            printf_fits - name_len - 1,
            printf_fits - name_len,
            printf_fits - name_len + 1,
            max,
            max + 1,
            TEXT_BYTES,
        ] {
            check_log(&log(name, b"", len as u16));
        }
    }
}

/// `spotter_test.cpp`'s sizes, and network types the C has no name for.
#[test]
fn tx_data() {
    let iri = NetworkType::CELLULAR_IRI_FALLBACK;
    let cell = NetworkType::CELLULAR_ONLY;
    for (network, len) in [
        (iri, 311),
        (cell, 1000),
        (iri, 0),
        (cell, 0),
        (iri, 312),
        (cell, 1001),
        (NetworkType(0), 311),
        (NetworkType(0), 312),
        (NetworkType(0xFF), 1000),
    ] {
        check_tx_data(&tx(network, len));
    }
}

#[test]
fn steps_interleave() {
    check(&SpotterInput {
        steps: vec![
            Step::TxData(tx(NetworkType::CELLULAR_ONLY, 4)),
            Step::Log(log(Some(b"a.log"), b"first", 0)),
            Step::Log(log(None, b"second", 1400)),
            Step::TxData(tx(NetworkType(3), 400)),
        ],
    });
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("spotter");
    assert!(
        replayed > 0,
        "no spotter seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} spotter seeds");
}
