//! The service layer on `bm_stack::Node`, compared against the oracle's whole
//! stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::services::{
    APP_TOPICS, ECHO, NAMES, Request, RequestTopic, ServicesInput, Size, Step, Summary, budget,
    check,
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

/// Divergence #89 is outside the domain: echo would copy past its buffer.
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

/// Divergence #88: the C reads a short body's header past the datagram, so
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
/// service instead (divergence #88). `svc` was listed first, so `s` stays
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
    // `s?c` registered twice leaves one entry stuck; it shadows nothing, so
    // it runs while the budget lasts. Seeds replayed first may have spent it.
    let budget = budget();
    let summary = run(vec![
        Step::Register(index(b"s?c")),
        Step::Register(index(b"s?c")),
        request(b"s?c", b"z"),
    ]);
    assert_eq!(summary.skipped, usize::from(budget == 0));
    assert_eq!(summary.replies, 1);
}

/// Divergence #88: `<id>/e` prefixes `<id>/echo/req`, so listed first it
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
