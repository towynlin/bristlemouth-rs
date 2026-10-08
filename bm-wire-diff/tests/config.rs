//! The config exchange `0xA0`–`0xA9`, compared against bm_core's live stack.
//!
//! Its own binary because `bm_wire_diff::config` brings bm_core's stack up; see
//! `bm_wire_diff::stack` for the contract that forces it, and because
//! registering `bcmp/config.c`'s sequenced types would break the in-process
//! property `bm_wire_diff::bcmp` relies on.
//!
//! What must hold: the ten handlers, the store mutations and the
//! forwarding decision all agree with the C. Eight of the ten have no gtest, so
//! the comparator is the coverage.

use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::config::{ConfigStatusResponse, ConfigValue};
use bm_wire::configuration::Partition;
use bm_wire_diff::config::{
    ConfigInput, ConfigMessage, PEER_NODE_ID, Seed, Target, check, oracle_frames, read_config_reply,
};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::NUM_PORTS;

fn input(message: ConfigMessage, target: Target) -> ConfigInput {
    ConfigInput {
        seeds: Vec::new(),
        message,
        target,
        partition: Partition::System as u8,
        key: b"foo".to_vec(),
        value: vec![0x18, 0x2a], // uint 42
        seq_num: 7,
        flag: false,
        ingress_port: 1,
        global_multicast: false,
    }
}

fn seeded(message: ConfigMessage, target: Target) -> ConfigInput {
    let mut input = input(message, target);
    input.seeds = vec![
        Seed::uint(Partition::System, b"foo", 42),
        Seed::str(Partition::System, b"bar", b"hello"),
        Seed::uint(Partition::User, b"baz", 9),
    ];
    input
}

/// A get addressed to this node is answered with a `0xA1` carrying the whole
/// slot, and the answer decodes to the value that was stored.
#[test]
fn our_config_value_reply_is_byte_identical_to_the_c() {
    let mut input = seeded(ConfigMessage::Get, Target::ThisNode);
    input.key = b"foo".to_vec();
    check(&input);

    let captured = oracle_frames(&input);
    assert_eq!(
        captured.len(),
        usize::from(NUM_PORTS),
        "a get is answered once per port"
    );
    let (header, payload, _) = read_config_reply(captured[0].0, &captured[0].1);
    assert_eq!(header.message_type, MessageType::CONFIG_VALUE);
    assert_eq!(header.seq_num, input.seq_num, "the reply echoes seq_num");
    let value = ConfigValue::decode(&payload).expect("a 0xA1 body");
    assert_eq!(value.header.target_node_id, PEER_NODE_ID);
    assert_eq!(value.data.len(), 50, "the whole slot, divergence #49");
}

/// A request whose sequence number exceeds 16 bits is answered with only its
/// low half, because every `bcmp/config.c` handler takes a `uint16_t` seq_num
/// (divergence #53). The comparison is byte for byte; this reads the number
/// off the wire to show it.
#[test]
fn a_reply_echoes_only_the_low_sixteen_bits_of_the_sequence_number() {
    let mut input = seeded(ConfigMessage::StatusRequest, Target::ThisNode);
    input.seq_num = 0x0045_7c6a;
    check(&input);

    let captured = oracle_frames(&input);
    let (header, _, _) = read_config_reply(captured[0].0, &captured[0].1);
    assert_eq!(
        header.seq_num, 0x7c6a,
        "the C truncates the echoed seq_num to 16 bits"
    );
}

/// Every message type, to this node, to a third node, and to zero.
#[test]
fn every_config_message_agrees_addressed_every_way() {
    for message in [
        ConfigMessage::Get,
        ConfigMessage::Value,
        ConfigMessage::Set,
        ConfigMessage::Commit,
        ConfigMessage::StatusRequest,
        ConfigMessage::StatusResponse,
        ConfigMessage::DeleteRequest,
        ConfigMessage::DeleteResponse,
        ConfigMessage::ClearRequest,
        ConfigMessage::ClearResponse,
    ] {
        for target in [Target::ThisNode, Target::OtherNode, Target::Zero] {
            check(&seeded(message, target));
        }
    }
}

/// A set stores the value and answers with it; the store images stay in step.
#[test]
fn a_set_stores_and_answers() {
    let mut input = seeded(ConfigMessage::Set, Target::ThisNode);
    input.key = b"new".to_vec();
    input.value = vec![0x19, 0x01, 0x00]; // uint 256
    check(&input);
}

/// A status request lists the keys; the response walks them one length byte at
/// a time.
#[test]
fn a_status_request_lists_the_keys() {
    let input = seeded(ConfigMessage::StatusRequest, Target::ThisNode);
    check(&input);

    let captured = oracle_frames(&input);
    let (header, payload, _) = read_config_reply(captured[0].0, &captured[0].1);
    assert_eq!(header.message_type, MessageType::CONFIG_STATUS_RESPONSE);
    let status = ConfigStatusResponse::decode(&payload).expect("a 0xA5 body");
    assert_eq!(status.num_keys, 2, "two System keys were seeded");
    let keys: Vec<_> = status.keys().map(|k| k.expect("a key").to_vec()).collect();
    assert_eq!(keys, vec![b"foo".to_vec(), b"bar".to_vec()]);
}

/// A delete removes the key and reports success; a delete of an absent key
/// reports failure. Both frames match, and the store matches.
#[test]
fn a_delete_removes_the_key() {
    let mut hit = seeded(ConfigMessage::DeleteRequest, Target::ThisNode);
    hit.key = b"foo".to_vec();
    check(&hit);

    let mut miss = seeded(ConfigMessage::DeleteRequest, Target::ThisNode);
    miss.key = b"quux".to_vec();
    check(&miss);
}

/// A clear of a valid partition succeeds and empties it; a clear of an
/// out-of-range partition byte reports failure and touches nothing
/// (divergence #50).
#[test]
fn a_clear_checks_the_partition_byte() {
    check(&seeded(ConfigMessage::ClearRequest, Target::ThisNode));

    let mut bad = seeded(ConfigMessage::ClearRequest, Target::ThisNode);
    bad.partition = 7;
    check(&bad);
}

/// A commit saves and, on the C, restarts. The frames match (there are none)
/// and the RAM images match; the flash comparison is skipped because the
/// shim's reset clears it and `RamConfigStorage::reset` does not.
#[test]
fn a_commit_saves_without_a_reply() {
    let mut input = seeded(ConfigMessage::Commit, Target::ThisNode);
    // Dirty a partition first so the commit has something to clear.
    input.seeds.push(Seed::uint(Partition::System, b"a", 1));
    check(&input);
}

/// A message for another node is forwarded, and the global-multicast case is
/// relayed by L2 as well (divergence #28).
#[test]
fn a_message_for_another_node_is_forwarded() {
    let mut input = seeded(ConfigMessage::Get, Target::OtherNode);
    check(&input);
    input.global_multicast = true;
    check(&input);
    // Zero is forwarded too: it is not a broadcast here.
    check(&seeded(ConfigMessage::Get, Target::Zero));
}

/// Only this binary's own target, because each stack target needs its own
/// process.
#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("config");
    assert!(
        replayed > 0,
        "no config seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} config seeds");
}
