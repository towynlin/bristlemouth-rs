//! The system-time exchange, compared against bm_core's live stack.
//!
//! Its own binary because `bm_wire_diff::time` brings bm_core's stack up; see
//! `bm_wire_diff::stack` for the contract that forces it.
//!
//! The headline is [`our_response_to_a_time_request_is_byte_identical_to_the_c`]
//! — card M2's "done when" — and the two quirk tests below it, which pin
//! divergences #27 and #28 against the C so that an upstream fix fails here
//! first and says why.

use bm_stack::port::RtcTimeAndDate;
use bm_wire::bcmp::time::{SystemTimeHeader, SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
use bm_wire::bcmp::{MessageType, rx};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::{self, NUM_PORTS, drain, inject, oracle, oracle_clock_micros};
use bm_wire_diff::time::{
    MAX_UTC_US, PEER_NODE_ID, THIRD_NODE_ID, Target, TimeInput, TimeMessage, build_frame, check,
};

/// 2026-09-21T12:34:56.789Z, a reading a human can check against the response.
const NOON_ISH: RtcTimeAndDate = RtcTimeAndDate {
    year: 2026,
    month: 9,
    day: 21,
    hour: 12,
    minute: 34,
    second: 56,
    ms: 789,
};

fn input(message: TimeMessage, target: Target, ingress_port: u8) -> TimeInput {
    TimeInput {
        message,
        target,
        ingress_port,
        global_multicast: false,
        clock_us: NOON_ISH.to_utc_micros(),
        utc_time_us: 1_700_000_000_123_456,
        trailing: Vec::new(),
        decode_probe: Vec::new(),
    }
}

/// Card M2's acceptance test: a node with a clock answers a time request with
/// the bytes bm_core would have sent.
#[test]
fn our_response_to_a_time_request_is_byte_identical_to_the_c() {
    check(&input(TimeMessage::Request, Target::ThisNode, 1));

    // And what that looks like, read off the wire rather than inferred from
    // the comparison passing.
    let frame = build_frame(&input(TimeMessage::Request, Target::ThisNode, 1));
    let _guard = oracle();
    drain();
    stack::set_both_clocks(NOON_ISH);
    inject(1, &frame);
    let captured = drain();

    assert_eq!(
        captured.len(),
        usize::from(NUM_PORTS),
        "a link-local response goes out stamped, once per port"
    );
    let mut copy = captured[0].1.clone();
    let received = rx::accept(&mut copy).expect("a peer accepts the response");
    assert_eq!(
        received.header.message_type,
        MessageType::SYSTEM_TIME_RESPONSE
    );
    let response = SystemTimeResponse::decode(received.payload).expect("decodes");
    assert_eq!(response.header.source_node_id, stack::NODE_ID);
    assert_eq!(response.header.target_node_id, PEER_NODE_ID);
    assert_eq!(
        response.utc_time_us,
        NOON_ISH.to_utc_micros(),
        "the reading the shim's bm_rtc_get_micro_seconds reports"
    );
    assert_eq!(response.utc_time_us, oracle_clock_micros());
}

/// A `0x12` moves the clock and is answered; the echo carries the microseconds
/// that were asked for, not the milliseconds the RTC kept.
#[test]
fn a_set_moves_both_clocks_and_is_echoed_at_full_precision() {
    let mut i = input(TimeMessage::Set, Target::ThisNode, 2);
    i.utc_time_us = 1_789_948_800_250_999;
    check(&i);

    let _guard = oracle();
    drain();
    stack::set_both_clocks(NOON_ISH);
    inject(2, &build_frame(&i));
    let captured = drain();
    assert_eq!(captured.len(), usize::from(NUM_PORTS));

    let mut copy = captured[0].1.clone();
    let received = rx::accept(&mut copy).expect("accepts");
    let response = SystemTimeResponse::decode(received.payload).expect("decodes");
    assert_eq!(
        response.utc_time_us, i.utc_time_us,
        "the echo is the requested value, to the microsecond"
    );
    assert_eq!(
        oracle_clock_micros(),
        1_789_948_800_250_000,
        "while the clock itself kept only the millisecond"
    );
}

/// Divergence #27: `target_node_id == 0` means three different things.
///
/// Asserted against the C rather than only against the port, so an upstream
/// fix breaks this test and not a deployment.
#[test]
fn a_broadcast_is_honoured_only_by_the_set_message() {
    for (message, answered) in [
        (TimeMessage::Request, false),
        (TimeMessage::Response, false),
        (TimeMessage::Set, true),
    ] {
        let i = input(message, Target::Everyone, 1);
        check(&i);

        let _guard = oracle();
        drain();
        stack::set_both_clocks(NOON_ISH);
        inject(1, &build_frame(&i));
        let captured = drain();
        assert_eq!(
            !captured.is_empty(),
            answered,
            "a broadcast {message:?} should {} be answered",
            if answered { "" } else { "not" }
        );
        if answered {
            assert_eq!(
                oracle_clock_micros(),
                1_700_000_000_123_000,
                "and a broadcast set moves the clock of every node that hears it"
            );
        } else {
            assert_eq!(
                oracle_clock_micros(),
                NOON_ISH.to_utc_micros(),
                "an ignored message leaves the clock alone"
            );
        }
    }
}

/// Divergence #28: a *global* multicast time message for a third node is put
/// back on the wire twice — once by L2's relay, once by `bcmp_ll_forward` —
/// and the second copy is link-local, so it is not the message that arrived.
#[test]
fn a_global_multicast_for_a_third_node_is_forwarded_twice() {
    let mut i = input(TimeMessage::Request, Target::OtherNode, 1);
    i.global_multicast = true;
    check(&i);

    let _guard = oracle();
    drain();
    stack::set_both_clocks(NOON_ISH);
    inject(1, &build_frame(&i));
    let captured = drain();

    assert_eq!(
        captured.len(),
        2,
        "L2 relays it and BCMP re-floods it, both onto port 2"
    );
    assert!(captured.iter().all(|(port, _)| *port == 2));

    let mut relay = captured[0].1.clone();
    let mut reflood = captured[1].1.clone();
    let relayed = rx::accept(&mut relay).expect("the relay is still valid");
    let reflooded = rx::accept(&mut reflood).expect("the re-flood is valid too");
    assert_eq!(
        relayed.src.to_node_id(),
        PEER_NODE_ID,
        "the relay keeps the originator"
    );
    assert_eq!(
        reflooded.src.to_node_id(),
        stack::NODE_ID,
        "the re-flood claims the forwarder -- divergence #23"
    );
    assert!(relayed.dst.is_global_multicast());
    assert!(
        reflooded.dst.is_link_local_multicast(),
        "and demotes FF03::1 to FF02::1, so the third node may never see it"
    );
    // The message itself survives both, which is how a chain still works.
    for received in [&relayed, &reflooded] {
        let header = SystemTimeHeader::decode(received.payload).expect("decodes");
        assert_eq!(header.target_node_id, THIRD_NODE_ID);
        assert_eq!(header.source_node_id, PEER_NODE_ID);
    }
}

/// A link-local message for a third node is re-flooded and not relayed: L2
/// consumes `FF02::1`, so `bcmp_ll_forward` is the only thing that moves it on.
#[test]
fn a_link_local_message_for_a_third_node_is_only_re_flooded() {
    for message in [
        TimeMessage::Request,
        TimeMessage::Response,
        TimeMessage::Set,
    ] {
        let i = input(message, Target::OtherNode, 1);
        check(&i);

        let _guard = oracle();
        drain();
        stack::set_both_clocks(NOON_ISH);
        inject(1, &build_frame(&i));
        let captured = drain();
        assert_eq!(
            captured.len(),
            1,
            "{message:?}: one copy, on the other port"
        );
        assert_eq!(captured[0].0, 2);
        assert_eq!(
            oracle_clock_micros(),
            NOON_ISH.to_utc_micros(),
            "{message:?}: a forwarded set must not be applied on the way past"
        );
    }
}

/// Divergence #12, met in the field rather than constructed.
///
/// `network_add_egress_port` patches the BCMP checksum when L2 stamps an
/// egress port and drops the end-around carry, so about one frame in 40 000
/// leaves a node with a checksum the far end rejects. Fixed-content messages
/// rarely land there; a system-time message carries a free-running 64-bit
/// timestamp, and `cargo fuzz run time` found this one in under a minute.
///
/// The input below is `bm-wire/fuzz/seeds/time/stamped-checksum-carries-twice`,
/// spelled out so the numbers are visible. Both stacks emit the same
/// unverifiable bytes — `check` compares them frame for frame — which is the
/// point: the port reproduces the corruption rather than quietly fixing it.
///
/// **If this test starts failing because the frame now verifies, the C has been
/// fixed and `bm_wire::l2::add_egress_port` must follow.** The same note is on
/// the tests in `bm-wire-diff/tests/l2_egress.rs`.
#[test]
fn a_response_whose_stamped_checksum_carries_twice_is_unverifiable() {
    let i = TimeInput {
        message: TimeMessage::Response,
        target: Target::OtherNode,
        ingress_port: 1,
        global_multicast: true,
        clock_us: 2_848_834_627_567_616,
        utc_time_us: 762_813_504_912_053,
        trailing: vec![248, 249],
        decode_probe: Vec::new(),
    };
    check(&i);

    let _guard = oracle();
    drain();
    stack::set_both_clocks(NOON_ISH);
    inject(1, &build_frame(&i));
    let captured = drain();

    // The relay (FF03::1, unstamped) and the re-flood (FF02::1, stamped).
    assert_eq!(
        captured.len(),
        2,
        "relayed and re-flooded -- divergence #28"
    );
    let verified: Vec<bool> = captured
        .iter()
        .map(|(port, frame)| bm_wire_diff::time::read_back(*port, frame).2)
        .collect();
    assert_eq!(
        verified,
        vec![true, false],
        "the unstamped relay verifies; the stamped re-flood does not"
    );
}

/// A node whose `bm_rtc_get` fails answers nothing — the one path the clock
/// itself decides. The oracle's RTC is process-global and has no un-set, so
/// this is checked on the Rust side against the C's documented `break`.
#[test]
fn a_node_whose_clock_is_unset_answers_no_request() {
    let frame = build_frame(&input(TimeMessage::Request, Target::ThisNode, 1));
    let mut node = bm_wire_diff::stack::node(); // clock unset
    let mut ours = frame.clone();
    let owed = node.on_frame(0, 1, &mut ours);
    assert!(owed.reply.is_none(), "no clock, no answer");
    assert!(owed.relay.is_none(), "and FF02::1 is consumed, not relayed");
    assert!(owed.forward.is_none(), "and it was addressed to us");
}

#[test]
fn every_message_and_target_on_every_port() {
    for message in [
        TimeMessage::Request,
        TimeMessage::Response,
        TimeMessage::Set,
    ] {
        for target in [Target::Everyone, Target::ThisNode, Target::OtherNode] {
            for port in 1..=NUM_PORTS {
                for global_multicast in [false, true] {
                    let mut i = input(message, target, port);
                    i.global_multicast = global_multicast;
                    check(&i);
                }
            }
        }
    }
}

/// Trailing bytes past the declared message are `data.size`'s business and
/// nobody else's: they must change nothing.
#[test]
fn trailing_bytes_change_nothing() {
    for len in [1usize, 7, 64] {
        for message in [TimeMessage::Request, TimeMessage::Set] {
            let mut i = input(message, Target::ThisNode, 1);
            i.trailing = (0..len).map(|n| (n as u8).wrapping_mul(37)).collect();
            check(&i);
        }
    }
}

/// The edges of the timestamp domain, where `utc_from_date_time`'s `u32` of
/// seconds runs out.
#[test]
fn clocks_at_the_edges_of_the_representable_range() {
    for clock_us in [0u64, 1_000, MAX_UTC_US - 1, MAX_UTC_US - 1_000_000] {
        let mut i = input(TimeMessage::Request, Target::ThisNode, 1);
        i.clock_us = clock_us;
        check(&i);

        let mut set = input(TimeMessage::Set, Target::ThisNode, 2);
        set.utc_time_us = clock_us;
        check(&set);
    }
}

/// The requester half: `bcmp_time_get_time` and `bcmp_time_set_time` build a
/// body and hand it to `bcmp_tx`, which is what `Node::request_system_time`
/// and `Node::set_system_time` do. Any `u64` is legal in a `0x12`, including
/// the ones the receive-side domain excludes, because neither side converts it.
#[test]
fn the_requests_we_issue_are_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    for target in [0u64, stack::NODE_ID, THIRD_NODE_ID] {
        for utc_time_us in [0u64, 1_789_948_800_250_999, u64::MAX] {
            // `bcmp_time_get_time` first.
            let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
            unsafe {
                assert_eq!(
                    bm_wire_sys::bcmp_time_get_time(target),
                    bm_wire_sys::BmErr_BmOK
                );
            }
            stack::pump_until_quiet();
            let captured = drain();

            let mut node = bm_wire_diff::stack::node();
            let ours = node
                .request_system_time(now_ms, target)
                .expect("a registered type is sent")
                .frame()
                .to_vec();
            compare_stamped("system time request", &captured, ours);

            // Then `bcmp_time_set_time`.
            let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
            unsafe {
                assert_eq!(
                    bm_wire_sys::bcmp_time_set_time(target, utc_time_us),
                    bm_wire_sys::BmErr_BmOK
                );
            }
            stack::pump_until_quiet();
            let captured = drain();

            let ours = node
                .set_system_time(now_ms, target, utc_time_us)
                .expect("a registered type is sent")
                .frame()
                .to_vec();
            compare_stamped("system time set", &captured, ours);

            // And the body is the codec's, read back the way a peer would.
            let mut copy = captured[0].1.clone();
            let received = rx::accept(&mut copy).expect("accepts");
            let set = SystemTimeSet::decode(received.payload).expect("decodes");
            assert_eq!(set.header.target_node_id, target);
            assert_eq!(set.header.source_node_id, stack::NODE_ID);
            assert_eq!(set.utc_time_us, utc_time_us);
            assert_eq!(
                received.payload.len(),
                SystemTimeSet::LEN,
                "sizeof(BcmpSystemTimeSet), with nothing after it"
            );
        }
    }

    // The request carries no timestamp at all, which is the one place the
    // three types differ in length.
    assert_eq!(SystemTimeRequest::LEN + 8, SystemTimeSet::LEN);
}

/// Stamp our frame the way L2 would for each port, then compare.
fn compare_stamped(what: &str, captured: &[(u8, Vec<u8>)], mut ours: Vec<u8>) {
    assert_eq!(
        captured.len(),
        usize::from(NUM_PORTS),
        "{what}: expected one copy per port, got {}",
        captured.len()
    );
    for (port, c_frame) in captured {
        let stamped = bm_wire::l2::stamp_egress_port(&mut ours, *port).expect("stampable");
        assert_eq!(
            c_frame,
            &stamped.to_vec(),
            "{what} on port {port} differs\n  C:        {c_frame:02x?}\n  bm-stack: {:02x?}",
            &stamped[..]
        );
    }
}

/// Only this binary's own target, because each stack target needs its own
/// process.
#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("time");
    assert!(
        replayed > 0,
        "no time seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} time seeds");
}

/// A corpus that replays is not the same as a corpus that covers anything.
///
/// The seed files are hand-built bytes in `Arbitrary`'s own layout, and one
/// field inserted in the wrong place would shift every later one — leaving
/// fifteen files that all decode to the same harmless input and still pass. So
/// decode them the way the fuzzer does and check the corners are all present.
#[test]
fn the_committed_seeds_cover_every_corner() {
    use arbitrary::{Arbitrary, Unstructured};

    let dir = bm_wire_diff::replay::seeds_dir().join("time");
    let mut seen = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the time seeds directory exists") {
        let path = entry.expect("readable entry").path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).expect("seed file is readable");
        let input = TimeInput::arbitrary_take_rest(Unstructured::new(&bytes))
            .unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()));
        seen.push(input);
    }

    for message in [
        TimeMessage::Request,
        TimeMessage::Response,
        TimeMessage::Set,
    ] {
        for target in [Target::Everyone, Target::ThisNode, Target::OtherNode] {
            assert!(
                seen.iter()
                    .any(|i| i.message == message && i.target == target),
                "no seed carries a {message:?} for {target:?}"
            );
        }
    }
    for port in 1..=NUM_PORTS {
        assert!(
            seen.iter().any(|i| i.ingress_port == port),
            "no seed on port {port}"
        );
    }
    assert!(seen.iter().any(|i| i.global_multicast), "no FF03::1 seed");
    assert!(
        seen.iter().any(|i| !i.trailing.is_empty()),
        "no trailing bytes"
    );
    assert!(
        seen.iter().any(|i| !i.decode_probe.is_empty()),
        "no decode probe"
    );
    assert!(seen.iter().any(|i| i.clock_us == 0), "no seed at the epoch");
    assert!(
        seen.iter().any(|i| i.clock_us == MAX_UTC_US - 1),
        "no seed at the last second utc_from_date_time can represent"
    );
    assert!(
        seen.iter().any(|i| i.expects_response()),
        "no seed provokes a response"
    );
    assert!(
        seen.iter().any(|i| !i.expects_response()),
        "every seed provokes a response, so the silent paths are untested"
    );
}
