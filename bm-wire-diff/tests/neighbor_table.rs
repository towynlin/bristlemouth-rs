//! Neighbour-table reply consumption, compared against bm_core by driving
//! both nodes with the same requests, replies and clock advances.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire_diff::neighbor_table::{MAX_ENTRIES, NODE_IDS, NeighborTableInput, Step, check};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};

fn run(steps: Vec<Step>) {
    check(&NeighborTableInput { steps });
}

fn ask(node: u8) -> Step {
    Step::Request { node, report: true }
}

fn reply(node: u8, ports: u8, neighbors: u8, revision: u8) -> Step {
    Step::Reply {
        from: node,
        claims: node,
        port: 1,
        ports,
        neighbors,
        revision,
    }
}

/// Past the 1 s timer, which is the only thing an advance can do here.
const PAST_THE_TIMEOUT: Step = Step::Advance { ms: 1_001 };

/// The exchange the card is about: ask one node, and walk the two-node table
/// it answers with.
#[test]
fn a_node_that_answers_reports_its_whole_table() {
    run(vec![ask(1), reply(1, 2, 2, 3)]);
}

/// `TARGET_NODE_ID` is an exact match on the body's claim, so a reply naming
/// anyone else is dropped -- and does not stop the timer either.
#[test]
fn a_reply_claiming_another_node_answers_nothing() {
    run(vec![
        ask(1),
        Step::Reply {
            from: 1,
            claims: 2,
            port: 1,
            ports: 2,
            neighbors: 2,
            revision: 1,
        },
        PAST_THE_TIMEOUT,
    ]);
}

/// The address the frame arrived from takes no part in the match: a reply
/// forwarded by one node on behalf of another still answers.
#[test]
fn the_source_address_is_not_what_is_matched() {
    run(vec![
        ask(1),
        Step::Reply {
            from: 2,
            claims: 1,
            port: 2,
            ports: 1,
            neighbors: 2,
            revision: 5,
        },
    ]);
}

/// Nothing asked, so `TARGET_NODE_ID` is the zero a process starts with and no
/// real node's reply matches it.
#[test]
fn an_unsolicited_reply_changes_nothing() {
    run(vec![reply(1, 2, 2, 1), reply(2, 1, 1, 2)]);
}

/// Divergence #35, both halves. A broadcast request is answered by every node
/// and accepted from none, because no replying node calls itself zero -- and
/// the one id that would match is the one `bcmp_find_neighbor` refuses
/// (divergence #18).
#[test]
fn a_broadcast_request_is_answered_by_everyone_and_accepted_from_nobody() {
    assert_eq!(NODE_IDS[0], 0);
    run(vec![
        Step::Request {
            node: 0,
            report: true,
        },
        reply(1, 2, 2, 1),
        reply(2, 2, 2, 2),
        reply(3, 2, 2, 3),
        // And then the node that does call itself zero.
        reply(0, 2, 2, 4),
    ]);
}

/// `TARGET_NODE_ID` starts at zero, so a reply claiming zero is accepted
/// before anything has been asked. Nothing is reported -- the callback is
/// null -- but the C does reach `bm_timer_stop`.
#[test]
fn a_reply_claiming_node_zero_is_accepted_before_any_request() {
    run(vec![reply(0, 1, 1, 1), reply(0, 0, 0, 2)]);
}

/// The callback is single-shot: the C clears it the moment it has run, so the
/// second reply is accepted and dropped.
#[test]
fn only_the_first_reply_is_reported() {
    run(vec![ask(1), reply(1, 2, 2, 1), reply(1, 1, 1, 2)]);
}

/// A request with no callback still accepts the reply and reports nothing.
#[test]
fn a_request_with_no_callback_reports_nothing() {
    run(vec![
        Step::Request {
            node: 1,
            report: false,
        },
        reply(1, 2, 2, 1),
        PAST_THE_TIMEOUT,
    ]);
}

/// Divergence #36: the timeout fires, the request survives it, and a reply
/// arriving afterwards is reported as an answer.
#[test]
fn a_reply_after_the_timeout_is_still_reported() {
    run(vec![
        ask(1),
        PAST_THE_TIMEOUT,
        Step::Advance { ms: 60_000 },
        reply(1, 2, 2, 7),
    ]);
}

/// The timer is a one-shot, so it fires once however far the clock goes.
#[test]
fn the_timeout_fires_once_per_request() {
    run(vec![
        ask(1),
        Step::Advance { ms: 60_000 },
        Step::Advance { ms: 60_000 },
        ask(2),
        Step::Advance { ms: 60_000 },
    ]);
}

/// An advance short of a second leaves the request waiting, and the next one
/// crosses the deadline.
#[test]
fn the_deadline_is_a_second_from_the_request() {
    run(vec![
        ask(1),
        Step::Advance { ms: 999 },
        Step::Advance { ms: 1 },
        Step::Advance { ms: 1 },
    ]);
}

/// A second request deletes the first's timer and takes its callback slot, so
/// the first can neither be answered nor time out.
#[test]
fn a_second_request_replaces_the_first_whole() {
    run(vec![
        ask(1),
        Step::Advance { ms: 600 },
        ask(2),
        Step::Advance { ms: 500 },
        reply(1, 2, 2, 1),
        Step::Advance { ms: 600 },
        reply(2, 2, 2, 2),
    ]);
}

/// `TARGET_NODE_ID` is a plain `uint64_t`, so two nodes sharing their low 32
/// bits are not the same request -- unlike `INFO_REQUEST_LIST`, whose key is a
/// `uint32_t` (divergence #33).
#[test]
fn the_target_is_matched_on_all_sixty_four_bits() {
    assert_eq!(NODE_IDS[1] as u32, NODE_IDS[3] as u32);
    assert_ne!(NODE_IDS[1], NODE_IDS[3]);
    run(vec![ask(3), reply(1, 2, 2, 1), reply(3, 2, 2, 2)]);
}

#[test]
fn every_entry_count_round_trips_through_the_callback() {
    for ports in 0..=MAX_ENTRIES {
        for neighbors in 0..=MAX_ENTRIES {
            run(vec![ask(1), reply(1, ports, neighbors, ports + neighbors)]);
        }
    }
}

#[test]
fn every_target_and_claim_combination() {
    for target in 0..NODE_IDS.len() as u8 {
        for claims in 0..NODE_IDS.len() as u8 {
            run(vec![
                ask(target),
                Step::Reply {
                    from: claims,
                    claims,
                    port: 1,
                    ports: 2,
                    neighbors: 2,
                    revision: claims,
                },
                PAST_THE_TIMEOUT,
            ]);
        }
    }
}

#[test]
fn a_long_mixed_sequence() {
    let mut steps = Vec::new();
    for round in 0..6u8 {
        let node = round % NODE_IDS.len() as u8;
        steps.push(Step::Request {
            node,
            report: round % 3 != 0,
        });
        steps.push(Step::Advance { ms: 400 });
        steps.push(reply(node, round % 3, (round + 1) % 3, round));
        steps.push(Step::Advance { ms: 700 });
    }
    run(steps);
}

/// Every seed depends on `reset_requester`, which `check` runs first, so it
/// has to recover a process left mid-exchange: the first sequence ends with a
/// request outstanding and its timer running, and the second must still see
/// its own reply as unsolicited.
#[test]
fn the_requester_resets_from_an_outstanding_request() {
    run(vec![ask(1)]);
    run(vec![reply(1, 2, 2, 1)]);
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("neighbor_table");
    assert!(
        replayed > 0,
        "no neighbor_table seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} neighbor_table seeds");
}
