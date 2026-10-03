//! The service layer on `bm_stack::Node`, compared against the oracle's whole
//! stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire::configuration::Partition;
use bm_wire::service::config_map::{self, ConfigMapReply, ConfigMapRequest};
use bm_wire::service::sys_info::SysInfoReply;
use bm_wire_diff::config::Seed;
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::services::PEERS;
use bm_wire_diff::services::{
    APP_TOPICS, ASKED, Ask, CONFIG_MAP, ECHO, NAMES, PEER_CONFIG_MAP, PEER_SYS_INFO, PartitionId,
    Reply, ReplyId, ReplyTopic, Request, RequestTopic, SYS_INFO, ServicesInput, Size, Step,
    Summary, Target, Timeout, budget, check,
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
    assert_eq!(
        SYS_INFO,
        format!("{:016x}/sys_info", bm_wire_diff::stack::NODE_ID).as_bytes()
    );
    assert_eq!(
        PEER_SYS_INFO,
        format!("{:016x}/sys_info", bm_wire_diff::services::PEERS[0]).as_bytes()
    );
    assert_eq!(
        CONFIG_MAP,
        format!("{:016x}/config_map", bm_wire_diff::stack::NODE_ID).as_bytes()
    );
    assert_eq!(
        PEER_CONFIG_MAP,
        format!("{:016x}/config_map", bm_wire_diff::services::PEERS[0]).as_bytes()
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
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert!(summary.replies >= 2 * 5, "{summary:?}");
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

/// Divergence #89: `s` prefixes `svc/req`, so listed first it shadows
/// `svc`; listed after, it does not.
#[test]
fn a_prefixing_name_shadows_a_later_service() {
    let summary = run(vec![
        Step::Register(index(b"s")),
        Step::Register(index(b"svc")),
        request(b"svc", b"shadowed"),
        request(b"s", b"answered"),
    ]);
    assert_eq!((summary.skipped, summary.replies), (0, 1));
    let summary = run(vec![
        Step::Register(index(b"svc")),
        Step::Register(index(b"s")),
        request(b"svc", b"answered"),
        request(b"s", b"answered"),
    ]);
    assert_eq!((summary.skipped, summary.replies), (0, 2));
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

const PEER_PATTERN: &[u8] = b"0*";

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
        ask(PEER_SYS_INFO, b"a", Timeout::Seconds(3)),
        ask(PEER_SYS_INFO, b"b", Timeout::Wrapped(2)),
    ]);
    let summary = run(vec![ask(PEER_SYS_INFO, b"c", Timeout::Seconds(0))]);
    assert_eq!(summary.asked, 1);
}

/// Divergence #91: a timeout that wraps, and one past `i32::MAX` ms.
#[test]
fn long_timeouts_wrap() {
    let summary = run(vec![
        ask(PEER_SYS_INFO, b"", Timeout::Wrapped(0)),
        ask(PEER_SYS_INFO, b"", Timeout::Overdue(0)),
        ask(PEER_SYS_INFO, b"", Timeout::Overdue(u32::MAX)),
        Step::Wait(600),
        Step::Wait(700),
    ]);
    assert_eq!(summary.timeouts, 3, "{summary:?}");
}

/// Divergence #92: matched by id and target, not by topic; a reply to
/// the pattern's request arrives on sys_info's reply topic, which both reply
/// subscriptions match (divergence #74).
#[test]
fn a_reply_is_matched_by_id_not_topic() {
    let summary = run(vec![
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
        ask(PEER_PATTERN, b"", Timeout::Seconds(3)),
        Step::Reply(Reply {
            id: ReplyId::Waiting(1),
            ..reply(PEER_SYS_INFO, b"for the pattern")
        }),
        Step::Reply(Reply {
            id: ReplyId::Waiting(0),
            ..reply(PEER_PATTERN, b"for sys_info")
        }),
    ]);
    assert_eq!((summary.skipped, summary.answered), (0, 2), "{summary:?}");
}

/// Divergence #92: `data_size` is passed unchecked; data is compared up to
/// what arrived.
#[test]
fn a_reply_claiming_more_data_than_it_carries() {
    let summary = run(vec![
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
        Step::Reply(Reply {
            size: Size::Off(100),
            ..reply(PEER_SYS_INFO, b"short")
        }),
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
        Step::Reply(Reply {
            size: Size::Off(-3),
            ..reply(PEER_SYS_INFO, b"longer")
        }),
    ]);
    assert_eq!((summary.skipped, summary.answered), (0, 2), "{summary:?}");
}

#[test]
fn replies_that_answer_nothing() {
    let summary = run(vec![
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(1)),
        Step::Reply(Reply {
            target: Target::Peer(false),
            ..reply(PEER_SYS_INFO, b"")
        }),
        Step::Reply(Reply {
            target: Target::Raw(0),
            ..reply(PEER_SYS_INFO, b"")
        }),
        Step::Reply(Reply {
            id: ReplyId::Raw(u32::MAX),
            ..reply(PEER_SYS_INFO, b"")
        }),
        Step::Reply(Reply {
            topic: ReplyTopic::Raw(b"0b54ccce5c7978bf/sys_info/rep/more".to_vec()),
            ..reply(PEER_SYS_INFO, b"prefixed")
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
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(0)),
        Step::Reply(Reply {
            cut: Some(15),
            ..reply(PEER_SYS_INFO, b"")
        }),
        Step::Reply(Reply {
            cut: Some(16),
            ..reply(PEER_SYS_INFO, b"x")
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
        Step::Subscribe(app(b"0b54ccce5c7978bf/sys_info/rep")),
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
        Step::Reply(reply(PEER_SYS_INFO, b"one")),
        Step::Reply(reply(PEER_SYS_INFO, b"two")),
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
        ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)),
    ]);
    assert_eq!(summary.answered, 2, "{summary:?}");
    assert_eq!(summary.asked + summary.skipped, 4, "{summary:?}");
}

/// A request a peer's request topic also reaches: the application hears it
/// from this node, and a peer's reply is matched by id.
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
        ask(PEER_SYS_INFO, &[7; 1025], Timeout::Seconds(1)),
        ask(PEER_SYS_INFO, &[7; 1024], Timeout::Seconds(1)),
    ]);
    assert_eq!((summary.skipped, summary.asked), (0, 1), "{summary:?}");
}

#[test]
fn the_request_table_keeps_a_slot_free() {
    let summary = run(vec![ask(PEER_SYS_INFO, b"", Timeout::Seconds(3)); 9]);
    assert_eq!((summary.asked, summary.skipped), (7, 2), "{summary:?}");
}

/// Every topic a step can subscribe, at once, fits `bm_get_subs`'s buffer
/// (divergence #78): the fuzzer's first S2 crash was the harness reading
/// the oracle's list past it.
#[test]
fn every_subscription_at_once_fits_bm_get_subs() {
    let mut steps: Vec<Step> = NAMES.iter().map(|n| Step::Register(index(n))).collect();
    steps.extend(APP_TOPICS.iter().map(|t| Step::Subscribe(app(t))));
    steps.extend(ASKED.iter().map(|n| ask(n, b"", Timeout::Seconds(0))));
    steps.push(Step::Wait(500));
    run(steps);
}

// ---------------------------------------------------------------------------
// sys_info -- card E1.
// ---------------------------------------------------------------------------

/// The reply carries the system partition's CRC as the C computes it, before
/// and after keys are stored; a user key leaves it alone. A request with
/// data is not answered.
#[test]
fn sys_info_answers_with_the_system_partitions_crc() {
    let summary = run(vec![
        Step::Register(index(SYS_INFO)),
        request(SYS_INFO, b""),
        Step::Configure(Seed::uint(Partition::System, b"foo", 7)),
        Step::Configure(Seed::str(Partition::System, b"bar", b"text")),
        request(SYS_INFO, b""),
        Step::Configure(Seed::int(Partition::System, b"foo", -7)),
        Step::Configure(Seed::uint(Partition::User, b"baz", 1)),
        request(SYS_INFO, b""),
        request(SYS_INFO, b"x"),
        Step::Unregister(index(SYS_INFO)),
        request(SYS_INFO, b""),
    ]);
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert_eq!(
        (summary.replies, summary.sys_info_replies),
        (3, 3),
        "{summary:?}"
    );
}

/// Each input starts from an empty store on both sides.
#[test]
fn the_store_is_emptied_between_inputs() {
    run(vec![Step::Configure(Seed::uint(
        Partition::System,
        b"quux",
        1,
    ))]);
    let summary = run(vec![
        Step::Register(index(SYS_INFO)),
        request(SYS_INFO, b""),
        Step::Register(index(ECHO)),
        request(ECHO, b"after"),
    ]);
    assert_eq!(summary.sys_info_replies, 1, "{summary:?}");
}

/// `sys_info_service_request` against `Node::sys_info_request_with`: a reply
/// the requester decodes, and a timeout.
#[test]
fn sys_info_request_answered_and_timed_out() {
    let mut body = [0u8; 128];
    let len = SysInfoReply::new(PEERS[0], 0xfeed, 0x1234_5678, b"peer_app")
        .encode(&mut body)
        .unwrap();
    let summary = run(vec![
        Step::AskSysInfo(Timeout::Seconds(1)),
        Step::Reply(reply(PEER_SYS_INFO, &body[..len])),
        Step::AskSysInfo(Timeout::Seconds(1)),
        Step::Wait(1500),
    ]);
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert_eq!(
        (
            summary.asked,
            summary.answered,
            summary.sys_info_decoded,
            summary.timeouts
        ),
        (2, 1, 1, 1),
        "{summary:?}"
    );
}

// ---------------------------------------------------------------------------
// config_map -- card E2.
// ---------------------------------------------------------------------------

fn config_map_request(partition_id: u32) -> Vec<u8> {
    let mut data = [0u8; 32];
    let len = ConfigMapRequest { partition_id }.encode(&mut data).unwrap();
    data[..len].to_vec()
}

/// Each partition id, before and after keys are stored; an unknown id is
/// answered with `success` 0 (the suspected defect, confirmed).
#[test]
fn config_map_answers_each_partition() {
    let mut steps = vec![Step::Register(index(CONFIG_MAP))];
    for id in [0, 1, 2, 3, 4, u32::MAX] {
        steps.push(request(CONFIG_MAP, &config_map_request(id)));
    }
    steps.extend([
        Step::Configure(Seed::uint(Partition::System, b"foo", 7)),
        Step::Configure(Seed::int(Partition::Hardware, b"bar", -7)),
        Step::Configure(Seed::str(Partition::User, b"baz", b"text")),
        Step::Configure(Seed::uint(Partition::User, b"quux", 70_000)),
    ]);
    for id in 1..=3 {
        steps.push(request(CONFIG_MAP, &config_map_request(id)));
    }
    steps.push(Step::RequestConfigMap {
        ingress: 1,
        peer: true,
        id: 9,
        partition_id: PartitionId::Small(3),
    });
    let summary = run(steps);
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert_eq!(
        (summary.config_map_replies, summary.config_map_successes),
        (10, 7),
        "{summary:?}"
    );
}

/// A request that does not decode gets no reply. Bytes after the map are not
/// read; a `partition_id` of another type is read for its head's argument,
/// as a release build does (divergence #82); a tagged one is outside the
/// domain.
#[test]
fn config_map_requests_that_do_not_decode() {
    let summary = run(vec![
        Step::Register(index(CONFIG_MAP)),
        request(CONFIG_MAP, b""),
        request(CONFIG_MAP, b"\xa0"),
        request(CONFIG_MAP, b"\xa1\x61p\x01\x00"),
        request(CONFIG_MAP, b"\xa1\x01\x01"),
        request(CONFIG_MAP, b"\xa1\x61p\x62ab"),
        request(CONFIG_MAP, b"\xa1\x61p\xc6\x01"),
    ]);
    assert_eq!(summary.skipped, 1, "{summary:?}");
    assert_eq!(
        (summary.config_map_replies, summary.config_map_successes),
        (2, 2),
        "{summary:?}"
    );
}

/// A partition whose map does not fit the handler's 1008 bytes with the
/// reply's other fields gets no reply on either side (contract 8).
#[test]
fn config_map_past_its_buffer_is_no_reply() {
    let mut steps = vec![Step::Register(index(CONFIG_MAP))];
    let keys: Vec<Vec<u8>> = (0..20).map(|i| format!("key{i:02}").into_bytes()).collect();
    for key in &keys {
        steps.push(Step::Configure(Seed::str(
            Partition::Hardware,
            key,
            &[b'v'; 40],
        )));
        steps.push(request(
            CONFIG_MAP,
            &config_map_request(config_map::PARTITION_ID_HW),
        ));
    }
    let summary = run(steps);
    assert_eq!(summary.skipped, 0, "{summary:?}");
    // The fields before the map take 78 bytes, leaving 930. Each key adds
    // 6 + 42 bytes to the map: 19 keys' 913 fit, 20 keys' 961 do not.
    assert_eq!(summary.config_map_replies, 19, "{summary:?}");
}

/// `config_cbor_map_service_request` against `Node::config_map_request_with`:
/// a reply the requester decodes, and a timeout.
#[test]
fn config_map_request_answered_and_timed_out() {
    let mut body = [0u8; 128];
    let len = ConfigMapReply {
        node_id: PEERS[0],
        partition_id: 3,
        success: true,
        cbor_data: b"\xa1\x61k\x01",
    }
    .encode(&mut body)
    .unwrap();
    let summary = run(vec![
        Step::AskConfigMap(PartitionId::Small(3), Timeout::Seconds(1)),
        Step::Reply(reply(PEER_CONFIG_MAP, &body[..len])),
        Step::AskConfigMap(PartitionId::Raw(u32::MAX), Timeout::Seconds(1)),
        Step::Wait(1500),
    ]);
    assert_eq!(summary.skipped, 0, "{summary:?}");
    assert_eq!(
        (
            summary.asked,
            summary.answered,
            summary.config_map_decoded,
            summary.timeouts
        ),
        (2, 1, 1, 1),
        "{summary:?}"
    );
}
