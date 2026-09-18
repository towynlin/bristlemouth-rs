//! The neighbour table, compared against bm_core by driving both with the same
//! sequence of heartbeats and clock advances.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire::neighbor::{HEARTBEAT_PERIOD_S, NeighborTable};
use bm_wire_diff::neighbor::{MAX_ADVANCE_MS, NODE_IDS, NeighborInput, Step, check};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};

fn hb(node: u8, port: u8, uptime_us: u64) -> Step {
    Step::Heartbeat {
        node,
        port,
        uptime_us,
        lease_s: HEARTBEAT_PERIOD_S,
    }
}

fn run(steps: Vec<Step>) {
    check(&NeighborInput { steps });
}

#[test]
fn a_single_neighbour_appears_and_stays() {
    run(vec![
        hb(1, 1, 1_000_000),
        Step::Advance { ms: 5_000 },
        hb(1, 1, 6_000_000),
        Step::Advance { ms: 5_000 },
        hb(1, 1, 11_000_000),
    ]);
}

#[test]
fn a_neighbour_falls_off_after_two_lease_periods() {
    run(vec![
        hb(1, 1, 1_000_000),
        // Two ten-second leases, plus slack.
        Step::Advance { ms: 21_000 },
        Step::Advance { ms: 1_000 },
    ]);
}

#[test]
fn a_neighbour_comes_back_after_falling_off() {
    run(vec![
        hb(1, 1, 1_000_000),
        Step::Advance { ms: 25_000 },
        hb(1, 1, 26_000_000),
        Step::Advance { ms: 1_000 },
    ]);
}

#[test]
fn one_neighbour_per_port_and_the_previous_one_is_dropped() {
    run(vec![
        hb(1, 1, 1_000_000),
        hb(2, 2, 1_000_000),
        hb(3, 1, 2_000_000),
        Step::Advance { ms: 1_000 },
    ]);
}

#[test]
fn a_neighbour_that_restarts_is_noticed() {
    run(vec![
        hb(1, 1, 9_000_000),
        Step::Advance { ms: 1_000 },
        hb(1, 1, 1_000),
        Step::Advance { ms: 1_000 },
    ]);
}

/// A node whose address carries a zero id is never recognised, so it is
/// "discovered" over and over.
#[test]
fn a_zero_node_id_is_rediscovered_on_every_heartbeat() {
    assert_eq!(NODE_IDS[0], 0);
    run(vec![
        hb(0, 1, 1_000_000),
        hb(0, 1, 2_000_000),
        hb(0, 1, 3_000_000),
        Step::Advance { ms: 1_000 },
    ]);
}

#[test]
fn neighbours_move_between_ports() {
    run(vec![
        hb(1, 1, 1_000_000),
        hb(1, 2, 2_000_000),
        hb(1, 1, 3_000_000),
        hb(2, 2, 3_000_000),
        Step::Advance { ms: 1_000 },
    ]);
}

/// An advertised lease is doubled and scaled to milliseconds in 32-bit
/// arithmetic, so a large one wraps. Both implementations must wrap the same.
#[test]
fn advertised_leases_across_the_range() {
    for lease_s in [0u32, 1, 10, 2_147_483, 2_147_484, 2_147_483_648, u32::MAX] {
        run(vec![
            Step::Heartbeat {
                node: 1,
                port: 1,
                uptime_us: 1_000_000,
                lease_s,
            },
            Step::Advance { ms: 1_000 },
            Step::Advance { ms: 30_000 },
        ]);
    }
}

#[test]
fn every_node_and_port_combination() {
    for node in 0..NODE_IDS.len() as u8 {
        for port in 1..=2u8 {
            run(vec![
                hb(node, port, 1_000_000),
                Step::Advance { ms: 1_000 },
                hb(node, port, 2_000_000),
                Step::Advance { ms: 25_000 },
            ]);
        }
    }
}

#[test]
fn a_long_alternating_sequence() {
    let mut steps = Vec::new();
    for round in 0..12u64 {
        steps.push(hb(
            (round % NODE_IDS.len() as u64) as u8,
            (round % 2) as u8 + 1,
            round * 1_000_000,
        ));
        steps.push(Step::Advance { ms: 3_000 });
    }
    run(steps);
}

#[test]
fn the_clock_advance_bound_is_respected() {
    let mut input = NeighborInput {
        steps: vec![Step::Advance { ms: u32::MAX }],
    };
    use bm_wire_diff::Domain;
    input.clamp_to_domain();
    match input.steps[0] {
        Step::Advance { ms } => assert!(ms <= MAX_ADVANCE_MS),
        other => panic!("expected an advance, got {other:?}"),
    }
}

#[test]
fn the_table_type_is_usable_without_the_oracle() {
    // A plain sanity check that the public type is constructible and empty,
    // so a firmware caller has something to copy.
    let table = NeighborTable::<4>::new();
    assert!(table.is_empty());
    assert_eq!(table.neighbors().count(), 0);
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("neighbor");
    assert!(
        replayed > 0,
        "no neighbor seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} neighbor seeds");
}
