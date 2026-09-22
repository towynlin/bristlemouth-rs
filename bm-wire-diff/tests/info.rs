//! Device-info reply consumption, compared against bm_core by driving both
//! nodes with the same heartbeats, replies and requests.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire_diff::info::{InfoInput, NODE_IDS, STRINGS, Step, check};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};

fn run(steps: Vec<Step>) {
    check(&InfoInput { steps });
}

fn hb(node: u8, port: u8, uptime_us: u64) -> Step {
    Step::Heartbeat {
        node,
        port,
        uptime_us,
    }
}

fn reply(node: u8, port: u8, strings: u8, revision: u8) -> Step {
    Step::Reply {
        from: node,
        claims: node,
        port,
        strings,
        revision,
    }
}

/// The exchange the card is about: a neighbour appears, the node asks, the
/// neighbour answers, and both nodes keep the answer.
#[test]
fn a_neighbour_that_answers_is_remembered() {
    run(vec![hb(1, 1, 1_000_000), reply(1, 1, 1, 7)]);
}

/// The cache belongs to the neighbour, so a reply from a node that is not one
/// is matched against the request list, consumed, and kept nowhere.
#[test]
fn a_reply_from_a_stranger_is_consumed_and_dropped() {
    run(vec![
        Step::Request {
            node: 2,
            report: false,
        },
        reply(2, 1, 1, 3),
    ]);
}

/// Nothing asked, so nothing happens at all -- `ll_get_item` misses and the
/// handler returns before it reaches the neighbour table.
#[test]
fn an_unsolicited_reply_changes_nothing() {
    run(vec![
        hb(1, 1, 1_000_000),
        reply(1, 1, 1, 1),
        reply(1, 1, 2, 2),
    ]);
}

/// A request made with a callback reports and caches nothing; one made without
/// caches and reports nothing.
#[test]
fn a_callback_request_reports_instead_of_caching() {
    run(vec![
        hb(1, 1, 1_000_000),
        reply(1, 1, 1, 1),
        Step::Request {
            node: 1,
            report: true,
        },
        reply(1, 1, 4, 9),
    ]);
}

/// `populate_neighbor_info` guards each string with `if (len)`, so a reply
/// declaring zero leaves the last one in place.
#[test]
fn a_reply_with_no_strings_keeps_the_last_ones() {
    assert_eq!(STRINGS[0], (&b""[..], &b""[..]));
    run(vec![
        hb(1, 1, 1_000_000),
        reply(1, 1, 1, 1),
        Step::Request {
            node: 1,
            report: false,
        },
        reply(1, 1, 0, 5),
    ]);
}

/// Each string is replaced on its own, so a reply carrying one of the two
/// leaves the other as it was.
#[test]
fn each_string_is_replaced_independently() {
    for strings in 0..STRINGS.len() as u8 {
        run(vec![
            hb(1, 1, 1_000_000),
            reply(1, 1, 1, 1),
            Step::Request {
                node: 1,
                report: false,
            },
            reply(1, 1, strings, 2),
        ]);
    }
}

/// The restart path asks about `neighbor->info.node_id`, which is zero until a
/// reply has been cached and the neighbour's own id afterwards. Both halves,
/// in order.
#[test]
fn a_restart_before_any_reply_asks_about_node_zero() {
    run(vec![
        hb(1, 1, 9_000_000),
        // Uptime goes backwards: a restart, with nothing cached yet.
        hb(1, 1, 1_000),
        // Now cache something, and restart again.
        reply(1, 1, 1, 4),
        hb(1, 1, 9_000_000),
        hb(1, 1, 500),
    ]);
}

/// A reply may name any node it likes; the request list and the neighbour
/// lookup both key on that claim rather than on where the frame came from.
#[test]
fn a_reply_is_keyed_on_what_it_claims_not_on_who_sent_it() {
    run(vec![
        hb(1, 1, 1_000_000),
        hb(2, 2, 1_000_000),
        Step::Reply {
            from: 2,
            claims: 1,
            port: 2,
            strings: 1,
            revision: 6,
        },
    ]);
}

/// `LLItem::id` is a `uint32_t`, so two nodes sharing their low 32 bits share
/// an entry: a reply from one answers the request made about the other.
#[test]
fn a_reply_answers_a_request_made_about_a_node_that_collides_with_it() {
    assert_eq!(NODE_IDS[1] as u32, NODE_IDS[3] as u32);
    assert_ne!(NODE_IDS[1], NODE_IDS[3]);
    run(vec![
        hb(3, 1, 1_000_000),
        Step::Request {
            node: 1,
            report: false,
        },
        Step::Reply {
            from: 3,
            claims: 3,
            port: 1,
            strings: 2,
            revision: 8,
        },
    ]);
}

/// Node id zero is never found in the neighbour table, so a reply claiming it
/// is matched, consumed and cached nowhere.
#[test]
fn a_reply_claiming_node_id_zero_is_cached_nowhere() {
    assert_eq!(NODE_IDS[0], 0);
    run(vec![hb(0, 1, 1_000_000), reply(0, 1, 1, 1)]);
}

/// One neighbour per port: whoever takes the port takes the entry, and the
/// information the last occupant reported goes with it.
#[test]
fn eviction_forgets_what_the_evicted_neighbour_reported() {
    run(vec![
        hb(1, 1, 1_000_000),
        reply(1, 1, 1, 1),
        // A different node on the same port replaces it.
        hb(2, 1, 1_000_000),
        reply(2, 1, 2, 2),
        // And the first one comes back, with nothing remembered about it.
        hb(1, 1, 2_000_000),
    ]);
}

/// An offline neighbour keeps its table row, so it keeps its information too.
#[test]
fn going_offline_does_not_forget_anything() {
    run(vec![
        hb(1, 1, 1_000_000),
        reply(1, 1, 1, 1),
        Step::Advance { ms: 25_000 },
        Step::Advance { ms: 1_000 },
    ]);
}

/// The list does not de-duplicate, so three requests take three replies to
/// clear -- and the third is the first one that is unsolicited.
#[test]
fn duplicate_requests_take_one_reply_each() {
    let ask = Step::Request {
        node: 1,
        report: false,
    };
    run(vec![
        hb(1, 1, 1_000_000),
        ask,
        ask,
        reply(1, 1, 1, 1),
        reply(1, 1, 2, 2),
        reply(1, 1, 3, 3),
        reply(1, 1, 4, 4),
    ]);
}

#[test]
fn every_node_and_string_combination() {
    for node in 0..NODE_IDS.len() as u8 {
        for strings in 0..STRINGS.len() as u8 {
            run(vec![
                hb(node, 1, 1_000_000),
                reply(node, 1, strings, strings),
                Step::Advance { ms: 1_000 },
            ]);
        }
    }
}

#[test]
fn a_long_mixed_sequence() {
    let mut steps = Vec::new();
    for round in 0..8u64 {
        let node = (round % NODE_IDS.len() as u64) as u8;
        steps.push(hb(node, (round % 2) as u8 + 1, round * 1_000_000));
        steps.push(reply(node, (round % 2) as u8 + 1, node, node));
        steps.push(Step::Request {
            node,
            report: round % 3 == 0,
        });
        steps.push(reply(node, 1, node.wrapping_add(1), node.wrapping_add(1)));
        steps.push(Step::Advance { ms: 2_000 });
    }
    run(steps);
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("info");
    assert!(
        replayed > 0,
        "no info seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} info seeds");
}
