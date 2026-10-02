//! The service layer on `bm_stack::Node`, compared against the oracle's whole
//! stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::services::{
    APP_TOPICS, ASKED, Ask, ECHO, NAMES, Reply, ReplyId, ReplyTopic, Request, RequestTopic,
    ServicesInput, Size, Step, Summary, Target, Timeout, budget, check,
};

fn index(name: &[u8]) -> u8 {
    NAMES.iter().position(|n| *n == name).expect("a pool name") as u8
}

fn app(topic: &[u8]) -> u8 {
    APP_TOPICS
        .iter()
        .position(|t| *t == topic)
        .expect("a pool topic") as u8
}

fn request(name: &[u8], data: &[u8]) -> Step {
    Step::Request(Request {
        ingress: 0,
        peer: false,
        topic: RequestTopic::Service(index(name)),
        id: 0x0102_0304,
        size: Size::Exact,
        data: data.to_vec(),
        cut: None,
    })
}

fn with(step: Step, f: impl FnOnce(&mut Request)) -> Step {
    let Step::Request(mut r) = step else {
        unreachable!()
    };
    f(&mut r);
    Step::Request(r)
}

fn run(steps: Vec<Step>) -> Summary {
    check(&ServicesInput { steps })
}

#[test]
fn the_node_id_is_the_pools() {
    assert_eq!(
        ECHO,
        format!("{:016x}/echo", bm_wire_diff::stack::NODE_ID).as_bytes()
    );
}

/// Every name registered, asked, and unregistered, from both peers and on
/// both ports.
#[test]
fn every_name() {
    let mut steps = Vec::new();
    for name in NAMES {
        steps.push(Step::Register(index(name)));
        steps.push(request(name, b"ping"));
        steps.push(with(request(name, b"pong"), |r| {
            r.peer = true;
            r.ingress = 1;
        }));
        steps.push(Step::Unregister(index(name)));
        steps.push(request(name, b"gone"));
    }
    let summary = run(steps);
    assert_eq!(summary.skipped, 0);
    assert!(summary.replies >= 2 * 6, "{summary:?}");
}

#[test]
fn echo_and_an_application_service() {
    let summary = run(vec![
        Step::Register(index(ECHO)),
        Step::Register(index(b"svc")),
        request(ECHO, b"hello"),
        request(ECHO, &[0x5a; 1008]),
        request(ECHO, b""),
        request(b"svc", b"abc"),
        request(b"svc", b"!refused"),
        request(b"svc", &[1; 1009]),
    ]);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.replies, 4);
}

/// Divergence #90 is outside the domain: echo would copy past its buffer.
#[test]
fn echo_past_its_buffer_is_skipped() {
    let summary = run(vec![
        Step::Register(index(ECHO)),
        request(ECHO, &[0x5a; 1009]),
    ]);
    assert_eq!(summary.skipped, 1);
}

/// The length checks, in the C's order: the body against `data_size`, then
/// the topic against the name.
#[test]
fn malformed_requests() {
    let summary = run(vec![
        Step::Register(index(b"svc")),
        with(request(b"svc", b"abc"), |r| r.size = Size::Off(1)),
        with(request(b"svc", b"abc"), |r| r.size = Size::Off(-1)),
        with(request(b"svc", b"abc"), |r| r.size = Size::Raw(u32::MAX)),
        with(request(b"svc", b"abc"), |r| {
            r.topic = RequestTopic::Suffixed(index(b"svc"), b"/x".to_vec());
        }),
        with(request(b"svc", b"abc"), |r| {
            r.topic = RequestTopic::Bare(index(b"svc"));
        }),
        with(request(b"svc", b"abc"), |r| {
            r.topic = RequestTopic::Raw(b"svc/re".to_vec());
        }),
    ]);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.replies, 0);
}

/// Divergence #89: the C reads a short body's header past the datagram, so
/// it is skipped. A name compared past the datagram needs a topic shorter
/// than a listed name that still reaches a `/req` subscription, which no pool
/// name allows; `bm_wire::service`'s unit tests cover it.
#[test]
fn a_short_body_is_skipped() {
    let summary = run(vec![
        Step::Register(index(b"svc")),
        with(request(b"svc", b""), |r| r.cut = Some(7)),
        with(request(b"svc", b""), |r| r.cut = Some(0)),
        with(request(b"svc", b""), |r| r.cut = Some(8)),
    ]);
    assert_eq!(summary.skipped, 2, "{summary:?}");
    assert_eq!(summary.replies, 1);
}

/// Unregistering a name that prefixes an earlier listed service removes that
/// service instead (divergence #89). `svc` was listed first, so `s` stays
/// listed and `svc/req` stays subscribed.
#[test]
fn unregistering_a_prefix_removes_an_earlier_service() {
    let summary = run(vec![
        Step::Register(index(b"svc")),
        Step::Register(index(b"s")),
        Step::Unregister(index(b"s")),
        request(b"svc", b"x"),
        request(b"s", b"y"),
    ]);
    // `s` would be left stuck, shadowing `sv` and `svc`: outside the budget.
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.replies, 2);
    // `x` registered twice leaves one entry stuck; it shares a prefix with
    // no other name, so it runs while the budget lasts. Seeds replayed first
    // may have spent it.
    let budget = budget();
    let summary = run(vec![
        Step::Register(index(b"x")),
        Step::Register(index(b"x")),
        request(b"x", b"z"),
    ]);
    assert_eq!(summary.skipped, usize::from(budget == 0));
    assert_eq!(summary.replies, 1);
}

/// Divergence #89: `<id>/e` prefixes `<id>/echo/req`, so listed first it
/// shadows echo; listed after, it does not.
#[test]
fn a_prefixing_name_shadows_a_later_service() {
    let e = b"c0ffee0012345678/e";
    let summary = run(vec![
        Step::Register(index(e)),
        Step::Register(index(ECHO)),
        request(ECHO, b"shadowed"),
        request(e, b"answered"),
    ]);
    assert_eq!(summary.replies, 1);
    let summary = run(vec![
        Step::Register(index(ECHO)),
        Step::Register(index(e)),
        request(ECHO, b"answered"),
        request(e, b"answered"),
    ]);
    assert_eq!(summary.replies, 2);
}

/// `s*/req` matches `svc/req`, so a request to `svc` reaches the service
/// callback twice: the C replies twice, the Rust node once.
#[test]
fn two_service_subscriptions_reply_twice_in_the_c() {
    let summary = run(vec![
        Step::Register(index(b"s*")),
        Step::Register(index(b"svc")),
        request(b"svc", b"twice"),
    ]);
    assert_eq!(summary.most_calls, 2);
    assert_eq!(summary.replies, 1);
}

/// Divergence #79 through the service layer: the application subscribed
/// first, so each registration lists the service callback again.
#[test]
fn a_service_registered_twice_after_the_application() {
    let summary = run(vec![
        Step::Subscribe(app(b"svc/req")),
        Step::Register(index(b"svc")),
        Step::Register(index(b"svc")),
        request(b"svc", b"x"),
        Step::Unregister(index(b"svc")),
        request(b"svc", b"y"),
        Step::Unsubscribe(app(b"svc/req")),
        request(b"svc", b"z"),
    ]);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.most_calls, 2);
    assert_eq!(summary.replies, 3);
}

/// The reply reaches the application's own subscriptions, once per reply on
/// the C.
#[test]
fn replies_are_delivered_locally() {
    let summary = run(vec![
        Step::Subscribe(app(b"*")),
        Step::Subscribe(app(b"s")),
        Step::Register(index(b"s")),
        Step::Register(index(ECHO)),
        request(b"s", b"local"),
        request(ECHO, b"local"),
        Step::Subscribe(app(ECHO_REQ)),
        request(ECHO, b"local"),
    ]);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.replies, 3);
}

const ECHO_REQ: &[u8] = b"c0ffee0012345678/echo/req";

/// The application and the service layer on one topic, unsubscribed in
/// either order.
#[test]
fn shared_topics_unsubscribe_one_callback_at_a_time() {
    let summary = run(vec![
        Step::Register(index(b"svc")),
        Step::Subscribe(app(b"svc/req")),
        Step::Subscribe(app(b"svc/req")),
        request(b"svc", b"a"),
        Step::Unsubscribe(app(b"svc/req")),
        Step::Unregister(index(b"svc")),
        request(b"svc", b"b"),
        Step::Unsubscribe(app(b"svc/req")),
        Step::Unsubscribe(app(b"svc/req")),
    ]);
    assert_eq!(summary.skipped, 0);
    assert_eq!(summary.replies, 1);
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("services");
    assert!(
        replayed > 0,
        "no services seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} services seeds");
}

// ---------------------------------------------------------------------------
// Service requests -- card S2.
// ---------------------------------------------------------------------------

fn asked(name: &[u8]) -> u8 {
    ASKED
        .iter()
        .position(|n| *n == name)
        .expect("an asked name") as u8
}

const PEER_ECHO: &[u8] = b"0b54ccce5c7978bf/echo";
const PEER_PATTERN: &[u8] = b"0b54ccce5c7978bf/*";

fn ask(name: &[u8], data: &[u8], timeout: Timeout) -> Step {
    Step::Ask(Ask {
        service: asked(name),
        data: data.to_vec(),
        timeout,
    })
}

/// A peer's reply to the oldest request waiting, on `name`'s reply topic.
fn reply(name: &[u8], data: &[u8]) -> Reply {
    Reply {
        ingress: 0,
        peer: false,
        topic: ReplyTopic::Asked(asked(name)),
        target: Target::Us,
        id: ReplyId::Waiting(0),
        size: Size::Exact,
        data: data.to_vec(),
        cut: None,
    }
}

#[test]
fn every_asked_name_is_answered_and_times_out() {
    let mut steps = Vec::new();
    for name in ASKED {
        steps.push(ask(name, b"ping", Timeout::Seconds(1)));
        steps.push(Step::Reply(reply(name, b"pong")));
        steps.push(ask(name, b"", Timeout::Seconds(1)));
        steps.push(Step::Wait(1500));
    }
    let summary = run(steps);
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert_eq!(
        (summary.asked, summary.answered, summary.timeouts),
        (8, 4, 4),
        "{summary:?}"
    );
}

#[test]
fn requests_left_waiting_expire_at_the_next_input() {
    run(vec![
        ask(PEER_ECHO, b"a", Timeout::Seconds(3)),
        ask(PEER_ECHO, b"b", Timeout::Wrapped(2)),
    ]);
    let summary = run(vec![ask(PEER_ECHO, b"c", Timeout::Seconds(0))]);
    assert_eq!(summary.asked, 1);
}

/// Divergence #91: a timeout that wraps, and one past `i32::MAX` ms.
#[test]
fn long_timeouts_wrap() {
    let summary = run(vec![
        ask(PEER_ECHO, b"", Timeout::Wrapped(0)),
        ask(PEER_ECHO, b"", Timeout::Overdue(0)),
        ask(PEER_ECHO, b"", Timeout::Overdue(u32::MAX)),
        Step::Wait(600),
        Step::Wait(700),
    ]);
    assert_eq!(summary.timeouts, 3, "{summary:?}");
}

/// Divergence #92: matched by id and target, not by topic; a reply to
/// the pattern's request arrives on echo's reply topic, which both reply
/// subscriptions match (divergence #74).
#[test]
fn a_reply_is_matched_by_id_not_topic() {
    let summary = run(vec![
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
        ask(PEER_PATTERN, b"", Timeout::Seconds(3)),
        Step::Reply(Reply {
            id: ReplyId::Waiting(1),
            ..reply(PEER_ECHO, b"for the pattern")
        }),
        Step::Reply(Reply {
            id: ReplyId::Waiting(0),
            ..reply(PEER_PATTERN, b"for echo")
        }),
    ]);
    assert_eq!((summary.skipped, summary.answered), (0, 2), "{summary:?}");
}

/// Divergence #92: `data_size` is passed unchecked; data is compared up to
/// what arrived.
#[test]
fn a_reply_claiming_more_data_than_it_carries() {
    let summary = run(vec![
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
        Step::Reply(Reply {
            size: Size::Off(100),
            ..reply(PEER_ECHO, b"short")
        }),
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
        Step::Reply(Reply {
            size: Size::Off(-3),
            ..reply(PEER_ECHO, b"longer")
        }),
    ]);
    assert_eq!((summary.skipped, summary.answered), (0, 2), "{summary:?}");
}

#[test]
fn replies_that_answer_nothing() {
    let summary = run(vec![
        ask(PEER_ECHO, b"", Timeout::Seconds(1)),
        Step::Reply(Reply {
            target: Target::Peer(false),
            ..reply(PEER_ECHO, b"")
        }),
        Step::Reply(Reply {
            target: Target::Raw(0),
            ..reply(PEER_ECHO, b"")
        }),
        Step::Reply(Reply {
            id: ReplyId::Raw(u32::MAX),
            ..reply(PEER_ECHO, b"")
        }),
        Step::Reply(Reply {
            topic: ReplyTopic::Raw(b"0b54ccce5c7978bf/echo/rep/more".to_vec()),
            ..reply(PEER_ECHO, b"prefixed")
        }),
        Step::Wait(1500),
    ]);
    assert_eq!(
        (summary.skipped, summary.answered, summary.timeouts),
        (0, 1, 0),
        "the prefixed topic answers (divergence #74): {summary:?}"
    );
}

/// Divergence #92: a body shorter than the reply header is read past.
#[test]
fn a_short_reply_is_skipped() {
    let summary = run(vec![
        ask(PEER_ECHO, b"", Timeout::Seconds(0)),
        Step::Reply(Reply {
            cut: Some(15),
            ..reply(PEER_ECHO, b"")
        }),
        Step::Reply(Reply {
            cut: Some(16),
            ..reply(PEER_ECHO, b"x")
        }),
    ]);
    assert_eq!((summary.skipped, summary.answered), (1, 1), "{summary:?}");
}

/// Divergence #79 on a reply topic: if the application subscribed it before
/// any request did, each request lists the reply callback again, until the
/// Rust node's `CALLBACKS`. Subscriptions outlive each input, so which
/// happens depends on the tests run before this one. Each reply answers once
/// either way.
#[test]
fn an_application_subscribed_reply_topic() {
    let summary = run(vec![
        Step::Subscribe(app(b"0b54ccce5c7978bf/echo/rep")),
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
        Step::Reply(reply(PEER_ECHO, b"one")),
        Step::Reply(reply(PEER_ECHO, b"two")),
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
        ask(PEER_ECHO, b"", Timeout::Seconds(3)),
    ]);
    assert_eq!(summary.answered, 2, "{summary:?}");
    assert_eq!(summary.asked + summary.skipped, 4, "{summary:?}");
}

/// A request a peer's request topic also reaches: the application hears it
/// from this node, and a peer's reply to the node's own echo request is
/// matched by id.
#[test]
fn requests_are_delivered_locally() {
    let summary = run(vec![
        Step::Subscribe(app(b"*")),
        Step::Subscribe(app(b"s")),
        ask(b"svc", b"q", Timeout::Seconds(1)),
        Step::Reply(reply(b"svc", b"a")),
    ]);
    assert_eq!((summary.skipped, summary.answered), (0, 1), "{summary:?}");
}

/// The C answers a request to its own service from its middleware task; the
/// Rust node does not, so such a request is outside the domain.
#[test]
fn asking_this_nodes_own_service_is_skipped() {
    let summary = run(vec![
        Step::Register(index(b"svc")),
        ask(b"svc", b"q", Timeout::Seconds(1)),
        Step::Unregister(index(b"svc")),
        ask(b"svc", b"q", Timeout::Seconds(1)),
    ]);
    assert_eq!((summary.skipped, summary.asked), (1, 1), "{summary:?}");
}

#[test]
fn too_large_a_request_is_refused_by_both() {
    let summary = run(vec![
        ask(PEER_ECHO, &[7; 1025], Timeout::Seconds(1)),
        ask(PEER_ECHO, &[7; 1024], Timeout::Seconds(1)),
    ]);
    assert_eq!((summary.skipped, summary.asked), (0, 1), "{summary:?}");
}

#[test]
fn the_request_table_keeps_a_slot_free() {
    let summary = run(vec![ask(PEER_ECHO, b"", Timeout::Seconds(3)); 9]);
    assert_eq!((summary.asked, summary.skipped), (7, 2), "{summary:?}");
}
