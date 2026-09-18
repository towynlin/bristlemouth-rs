//! The packet registry and the sequenced-request machinery, compared against
//! bm_core by driving both with the same script of sends, receives and clock
//! advances.
//!
//! Its own binary because it owns `packet.c`'s `PACKET`; see
//! `bm_wire_diff::registry` for the whole reason.
//!
//! # Where the gold vectors are, and are not
//!
//! bm_core ships no on-wire vectors for any BCMP message type, so for the
//! frames themselves the ground truth is the compiled oracle. `packet.c` is
//! the one exception in this area: `test/src/packet_test.cpp`'s
//! `Packet.sequence_request` asserts *literal* values for the sequence
//! counter — the nth sequenced request carries `seq_num == n`, and a second
//! run of requests continues from where the first stopped. Those are asserted
//! directly in `bm_wire::bcmp::registry`'s unit tests as well as being
//! compared here.
//!
//! Nothing upstream tests expiry at all: the gtest suite fakes `bm_timer_*`,
//! so `sequence_list_timer_callback` never runs there. Everything below about
//! timeouts is measured against the real timer in the shim.

use bm_wire::bcmp::registry::{DEFAULT_MESSAGE_TIMEOUT_MS, MESSAGE_TIMER_EXPIRY_PERIOD_MS};
use bm_wire_diff::registry::{MAX_STEPS, RegistryInput, SeqChoice, Step, check};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};

/// Index into `bm_wire_diff::registry::REGISTERED`.
const CONFIG_GET: u8 = 0;
const CONFIG_SET: u8 = 1;
const NEIGHBOR_PROTO_REQUEST: u8 = 3;
const CONFIG_VALUE: u8 = 4;
const CONFIG_STATUS_RESPONSE: u8 = 5;
const NEIGHBOR_PROTO_REPLY: u8 = 6;
const CONFIG_COMMIT: u8 = 7;
const BOTH_FLAGS: u8 = 9;
/// One past the end: the type nothing registers.
const UNREGISTERED: u8 = 10;

fn send(type_index: u8) -> Step {
    Step::Send {
        type_index,
        reply_seq_num: 0,
        with_callback: true,
        body_len: 8,
    }
}

fn send_without_callback(type_index: u8) -> Step {
    Step::Send {
        type_index,
        reply_seq_num: 0,
        with_callback: false,
        body_len: 8,
    }
}

fn reply_to(type_index: u8, which: u8) -> Step {
    Step::Receive {
        type_index,
        seq: SeqChoice::Outstanding(which),
        body_len: 12,
    }
}

fn receive(type_index: u8, seq_num: u32) -> Step {
    Step::Receive {
        type_index,
        seq: SeqChoice::Raw(seq_num),
        body_len: 4,
    }
}

fn advance(ms: u32) -> Step {
    Step::Advance { ms }
}

/// Put the clock exactly `ms` before the next expiry sweep. The sweep's phase
/// survives every run, so this is the only way a script can say where in it
/// the next step happens.
fn align(ms: u32) -> Step {
    Step::AlignBeforeSweep { ms }
}

fn run(steps: Vec<Step>) {
    check(&RegistryInput { steps });
}

#[test]
fn a_request_and_its_reply() {
    run(vec![send(CONFIG_GET), reply_to(CONFIG_VALUE, 0)]);
}

#[test]
fn a_request_nobody_answers_times_out() {
    run(vec![
        send(CONFIG_GET),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS),
    ]);
}

/// The reply arrives, but only after the sweep has already given up on the
/// request — so it reaches `cfg->process` as an unsolicited message, and the
/// caller has already been told the request failed.
#[test]
fn a_reply_that_arrives_after_the_timeout_is_an_unsolicited_message() {
    run(vec![
        send(CONFIG_GET),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS),
        receive(CONFIG_VALUE, 0),
    ]);
}

/// Nothing expires between sweeps, however far past the nominal timeout the
/// clock is. `default_message_timeout_ms` is 24; this walks right up to the
/// first sweep without one firing.
#[test]
fn the_nominal_timeout_expires_nothing_on_its_own() {
    let mut steps = vec![send(CONFIG_GET)];
    for _ in 0..MESSAGE_TIMER_EXPIRY_PERIOD_MS / DEFAULT_MESSAGE_TIMEOUT_MS {
        steps.push(advance(DEFAULT_MESSAGE_TIMEOUT_MS));
    }
    steps.push(advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS));
    run(steps);
}

/// The boundary itself, from both sides. `default_message_timeout_ms` is 24
/// and the comparison is strict, so a request that is exactly 24 ms old when
/// the sweep reaches it survives — and the next sweep is 150 ms away. One
/// millisecond of difference costs a request 150 ms of life.
#[test]
fn the_timeout_boundary_from_both_sides() {
    // 25 ms old at the sweep: the youngest a request can be and still die.
    run(vec![
        align(DEFAULT_MESSAGE_TIMEOUT_MS + 1),
        send(CONFIG_GET),
        advance(DEFAULT_MESSAGE_TIMEOUT_MS + 1),
    ]);
    // 24 ms old at the sweep: survives it, and dies at the next one.
    run(vec![
        align(DEFAULT_MESSAGE_TIMEOUT_MS),
        send(CONFIG_GET),
        advance(DEFAULT_MESSAGE_TIMEOUT_MS),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS),
    ]);
}

/// How long a request actually lives, measured rather than assumed: for every
/// offset in the sweep's phase, send one and walk the clock forward a
/// millisecond at a time until the C gives up on it. Divergence #22 quotes the
/// range this produces.
#[test]
fn the_effective_timeout_across_the_whole_phase() {
    for offset in 1..=MESSAGE_TIMER_EXPIRY_PERIOD_MS {
        let mut steps = vec![align(offset), send(CONFIG_GET)];
        steps.extend(
            std::iter::repeat_n(
                advance(1),
                (MESSAGE_TIMER_EXPIRY_PERIOD_MS + offset) as usize,
            )
            .take(MAX_STEPS - 2),
        );
        run(steps);
        // A script is capped at 32 steps, so finish the job in a second run:
        // whatever is left outstanding expires in the drain either way.
    }
}

#[test]
fn several_requests_expire_together_in_the_order_they_were_sent() {
    run(vec![
        send(CONFIG_GET),
        send(CONFIG_SET),
        send(NEIGHBOR_PROTO_REQUEST),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS * 2),
    ]);
}

/// The sweep takes only what has aged out. The two requests are 130 ms apart,
/// so the sweep at 150 ms reaches the first and not the second.
#[test]
fn a_sweep_takes_the_old_and_leaves_the_young() {
    run(vec![
        send(CONFIG_GET),
        advance(130),
        send(CONFIG_SET),
        advance(20),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS),
    ]);
}

/// Replies may come back in any order: the sequence list is searched, not
/// popped.
#[test]
fn replies_out_of_order() {
    run(vec![
        send(CONFIG_GET),
        send(CONFIG_SET),
        send(NEIGHBOR_PROTO_REQUEST),
        reply_to(CONFIG_VALUE, 2),
        reply_to(CONFIG_VALUE, 0),
        reply_to(CONFIG_VALUE, 0),
    ]);
}

/// Divergence #21: the sequence entry records the request's type and nothing
/// ever compares it, so a `CONFIG_VALUE` reply answers a
/// `NEIGHBOR_PROTO_REQUEST`, and vice versa. The requester's callback runs
/// with a payload from a message it never asked for.
#[test]
fn a_reply_of_the_wrong_type_answers_the_request_anyway() {
    run(vec![
        send(NEIGHBOR_PROTO_REQUEST),
        reply_to(CONFIG_VALUE, 0),
        send(CONFIG_GET),
        reply_to(NEIGHBOR_PROTO_REPLY, 0),
        send(CONFIG_SET),
        reply_to(CONFIG_STATUS_RESPONSE, 0),
    ]);
}

/// A request sent with a null callback is still tracked, still matched, and
/// still removed — but the reply then falls through to the type's own
/// `process`, and the timeout is silent. `Packet.sequence_request`'s second
/// half asserts exactly this for the reply path.
#[test]
fn a_request_without_a_callback_falls_through_to_process() {
    run(vec![
        send_without_callback(CONFIG_GET),
        reply_to(CONFIG_VALUE, 0),
        send_without_callback(CONFIG_SET),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS * 2),
    ]);
}

#[test]
fn a_reply_matching_nothing_goes_to_process() {
    run(vec![
        receive(CONFIG_VALUE, 0),
        receive(CONFIG_VALUE, u32::MAX),
        send(CONFIG_GET),
        receive(CONFIG_VALUE, 0xDEAD_BEEF),
        reply_to(CONFIG_VALUE, 0),
    ]);
}

#[test]
fn an_unsequenced_type_neither_takes_nor_gives_a_sequence_number() {
    run(vec![
        Step::Send {
            type_index: CONFIG_COMMIT,
            reply_seq_num: 0x1234_5678,
            with_callback: true,
            body_len: 4,
        },
        send(CONFIG_GET),
        receive(CONFIG_COMMIT, 0),
        reply_to(CONFIG_VALUE, 0),
    ]);
}

#[test]
fn a_reply_type_echoes_the_number_it_is_given() {
    for reply_seq_num in [0, 1, 0x1234_5678, u32::MAX] {
        run(vec![Step::Send {
            type_index: CONFIG_VALUE,
            reply_seq_num,
            with_callback: true,
            body_len: 16,
        }]);
    }
}

/// Both flags set is not something bm_core does, but the branches allow it:
/// `serialize` takes the reply arm, and `process_received_message` refuses to
/// match it against an outstanding request.
#[test]
fn a_type_with_both_flags_set() {
    run(vec![
        send(CONFIG_GET),
        Step::Send {
            type_index: BOTH_FLAGS,
            reply_seq_num: 42,
            with_callback: true,
            body_len: 4,
        },
        reply_to(BOTH_FLAGS, 0),
        reply_to(CONFIG_VALUE, 0),
    ]);
}

/// `serialize` writes nothing at all for a type it cannot find — not even the
/// header — and `process_received_message` validates the frame and drops it.
#[test]
fn an_unregistered_type_is_refused_on_the_way_out_and_dropped_on_the_way_in() {
    run(vec![
        Step::Send {
            type_index: UNREGISTERED,
            reply_seq_num: 7,
            with_callback: true,
            body_len: 8,
        },
        receive(UNREGISTERED, 0),
        send(CONFIG_GET),
        receive(UNREGISTERED, 0),
        reply_to(CONFIG_VALUE, 0),
    ]);
}

/// Bodies of every length the domain allows, on both directions of travel.
#[test]
fn bodies_of_every_length() {
    for body_len in 0..=32u8 {
        run(vec![
            Step::Send {
                type_index: CONFIG_GET,
                reply_seq_num: 0,
                with_callback: true,
                body_len,
            },
            Step::Receive {
                type_index: CONFIG_VALUE,
                seq: SeqChoice::Outstanding(0),
                body_len,
            },
        ]);
    }
}

/// The sequence counter is process-global in the C and never resets, so a run
/// has to pick up where the last one left off. Several runs back to back is
/// the cheapest way to prove the harness models that rather than accidentally
/// agreeing.
#[test]
fn the_sequence_counter_carries_across_runs() {
    for _ in 0..8 {
        run(vec![
            send(CONFIG_GET),
            send(CONFIG_SET),
            reply_to(CONFIG_VALUE, 1),
            advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS * 2),
        ]);
    }
}

/// A clock advance long enough to skip whole sweeps: the shim fires the timer
/// repeatedly to catch up, and everything due dies at the same instant.
#[test]
fn a_long_advance_catches_the_timer_up() {
    run(vec![
        send(CONFIG_GET),
        advance(1_000),
        send(CONFIG_SET),
        advance(1_000),
    ]);
}

/// The shape that divergence #20 makes undefined — a middle entry removed,
/// then the tail, then another append — is the one the comparator refuses to
/// complete. Everything up to that point is still compared, and the entries
/// left behind still expire on schedule.
#[test]
fn the_dangling_tail_shape_stops_short_of_the_undefined_append() {
    run(vec![
        send(CONFIG_GET),
        send(CONFIG_SET),
        send(NEIGHBOR_PROTO_REQUEST),
        // The middle one: its successor's `previous` now dangles.
        reply_to(CONFIG_VALUE, 1),
        // The tail: `ll_remove` writes that dangling pointer into `LL::tail`.
        reply_to(CONFIG_VALUE, 1),
        // Which the comparator declines to append through.
        send(CONFIG_GET),
        advance(MESSAGE_TIMER_EXPIRY_PERIOD_MS * 2),
    ]);
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    assert!(STACK_TARGETS.contains(&"registry"));
    let replayed = replay_target("registry");
    assert!(replayed > 0, "no seeds replayed for the registry target");
    eprintln!("replayed {replayed} registry seeds");
}
