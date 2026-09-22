//! Resource discovery, compared against bm_core by driving both nodes with the
//! same adds, finds, requests and arriving frames.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.
//!
//! # Every test here is order-independent, and has to be
//!
//! `PUB_LIST` and `SUB_LIST` have no remove and no deinit, so the oracle's two
//! lists are shared by every test in this binary and grow monotonically as it
//! runs. Nothing below may assert that a list holds a particular thing: what
//! `check` asserts is that the two implementations *agree*, step by step, and
//! that holds whatever the lists already contain.
//!
//! The behaviours a test would otherwise want to pin — that a prefix is
//! refused, that a `0x0A` naming zero is answered by nobody — are pinned in
//! `bm_wire::bcmp::resource`'s unit tests and in `bm-stack/tests/node.rs`,
//! where the state belongs to the test.

use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::resource::{NAMES, PEER_IDS, RESOURCE_LEN, ResourceInput, Step, TARGETS, check};

fn run(steps: Vec<Step>) {
    check(&ResourceInput { steps });
}

fn add(name: u8, subscriber: bool) -> Step {
    Step::Add { name, subscriber }
}

fn find(name: u8, subscriber: bool) -> Step {
    Step::Find { name, subscriber }
}

fn incoming(target: u8) -> Step {
    Step::Incoming { target, port: 1 }
}

fn reply(peer: u8, shape: u8) -> Step {
    Step::Reply {
        from: peer,
        claims: peer,
        port: 1,
        shape,
    }
}

/// The index in [`TARGETS`] of the oracle's own node id — the only one a
/// `0x0A` is answered for.
const US: u8 = 1;

/// The pool the domain limit rests on: the storable names are all the same
/// length, so no `memcmp` the C performs can run off the end of an entry.
#[test]
fn the_name_pool_is_shaped_the_way_the_domain_limit_assumes() {
    let storable: Vec<&&[u8]> = NAMES.iter().filter(|n| n.len() == RESOURCE_LEN).collect();
    assert_eq!(storable.len(), 4, "four names may be stored");
    assert!(
        NAMES.iter().all(|name| name.len() <= RESOURCE_LEN),
        "a longer needle would read past a stored entry"
    );
    assert!(
        NAMES
            .iter()
            .any(|name| name.len() < RESOURCE_LEN && NAMES[0].starts_with(name)),
        "and one of the shorter ones must be a strict prefix of a storable one"
    );
}

/// The exchange the card is about: advertise both kinds, then answer a
/// request for them.
#[test]
fn a_node_that_is_asked_answers_with_both_lists() {
    run(vec![
        add(0, false),
        add(1, false),
        add(2, true),
        incoming(US),
    ]);
}

/// Divergence #37, against the two request types that do take zero as a
/// broadcast. `check` compares the frames both nodes built, so "neither
/// answered" is an assertion rather than an absence.
#[test]
fn a_request_naming_zero_is_answered_by_nobody() {
    assert_eq!(TARGETS[0], 0);
    run(vec![add(0, false), incoming(0), incoming(US)]);
}

#[test]
fn a_request_naming_another_node_is_answered_by_nobody() {
    run(vec![incoming(2), incoming(3), incoming(US)]);
}

/// Divergence #38's defined half: the de-duplication is a prefix match, so a
/// name that is not in the list is refused because a longer one is.
#[test]
fn every_name_against_every_list() {
    let mut steps = Vec::new();
    for name in 0..NAMES.len() as u8 {
        for subscriber in [false, true] {
            steps.push(find(name, subscriber));
            steps.push(add(name, subscriber));
            steps.push(find(name, subscriber));
        }
    }
    // More than one seed's worth, and the lists carry over between them.
    for chunk in steps.chunks(20) {
        run(chunk.to_vec());
    }
}

/// The empty needle matches whatever is at the head of a list, and nothing at
/// all when the list is empty — whichever of those this process is up to.
#[test]
fn the_empty_needle_is_compared_both_ways() {
    let empty = NAMES.iter().position(|name| name.is_empty()).unwrap() as u8;
    run(vec![find(empty, false), find(empty, true)]);
    run(vec![add(0, false), find(empty, false), add(empty, false)]);
}

/// The requester half: the `0x0A` each side builds, and the callback a
/// matching `0x0B` runs.
#[test]
fn a_request_and_the_reply_that_answers_it() {
    run(vec![
        Step::Request {
            node: 2,
            report: true,
        },
        reply(1, 3),
    ]);
}

/// `fp == NULL`: the reply is matched, consumed and reported to nobody.
#[test]
fn a_request_without_a_callback_consumes_its_reply_silently() {
    run(vec![
        Step::Request {
            node: 2,
            report: false,
        },
        reply(1, 1),
    ]);
}

/// The one correlation in BCMP that compares the body's claim against the
/// address it arrived from.
#[test]
fn a_reply_whose_claim_disagrees_with_its_source_is_dropped() {
    run(vec![
        Step::Request {
            node: 2,
            report: true,
        },
        Step::Reply {
            from: 1,
            claims: 2,
            port: 1,
            shape: 2,
        },
        // And the honest one, which still answers.
        reply(1, 2),
    ]);
}

#[test]
fn an_unsolicited_reply_changes_nothing() {
    run(vec![reply(1, 3), reply(2, 4), reply(0, 0)]);
}

/// Divergence #33: `LLItem::id` holds half a node id, so a reply from the node
/// sharing the low half answers the other's request.
#[test]
fn the_request_list_is_keyed_on_half_an_id() {
    assert_eq!(TARGETS[3] as u32, PEER_IDS[1] as u32);
    assert_ne!(TARGETS[3], PEER_IDS[1]);
    run(vec![
        Step::Request {
            node: 3,
            report: true,
        },
        reply(1, 1),
    ]);
}

/// Divergence #19's shape: asking twice leaves two entries, and it takes two
/// replies to clear them.
#[test]
fn asking_twice_needs_answering_twice() {
    run(vec![
        Step::Request {
            node: 2,
            report: true,
        },
        Step::Request {
            node: 2,
            report: false,
        },
        reply(1, 2),
        reply(1, 3),
        reply(1, 4),
    ]);
}

/// Every reply shape through the callback, which is the walk divergence #14 is
/// about: each record's own length advances the cursor to the next one.
#[test]
fn every_reply_shape_round_trips_through_the_callback() {
    for shape in 0..5u8 {
        run(vec![
            Step::Request {
                node: 2,
                report: true,
            },
            reply(1, shape),
        ]);
    }
}

/// A request naming a node that never answers leaves an entry behind for the
/// life of the process, so the next seed has to start by clearing it —
/// `check`'s first act. This is that path, twice over.
#[test]
fn the_request_list_is_emptied_between_seeds() {
    run(vec![
        Step::Request {
            node: 2,
            report: true,
        },
        Step::Request {
            node: 3,
            report: true,
        },
    ]);
    run(vec![reply(1, 1), reply(3, 2)]);
}

/// Every target and claim combination, so the exact-match test on the
/// responder and the source-versus-claim test on the requester are both
/// crossed with everything.
#[test]
fn every_target_and_every_claim() {
    for target in 0..TARGETS.len() as u8 {
        let mut steps = vec![
            incoming(target),
            Step::Request {
                node: target,
                report: true,
            },
        ];
        for claims in 0..PEER_IDS.len() as u8 {
            steps.push(Step::Reply {
                from: 1,
                claims,
                port: 2,
                shape: claims % 5,
            });
        }
        run(steps);
    }
}

#[test]
fn a_long_mixed_sequence() {
    let mut steps = Vec::new();
    for round in 0..5u8 {
        steps.push(add(round % NAMES.len() as u8, round % 2 == 0));
        steps.push(find((round + 3) % NAMES.len() as u8, round % 2 == 1));
        steps.push(incoming(round % TARGETS.len() as u8));
        steps.push(Step::Request {
            node: (round + 1) % TARGETS.len() as u8,
            report: round % 3 != 0,
        });
        steps.push(reply(round % PEER_IDS.len() as u8, round % 5));
    }
    run(steps);
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("resource");
    assert!(
        replayed > 0,
        "no resource seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} resource seeds");
}
