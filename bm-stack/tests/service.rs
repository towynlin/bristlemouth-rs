//! Services on a node: registration, dispatch from `on_frame`, echo, and
//! service requests. `bm-wire-diff/tests/services.rs` compares the same
//! against the oracle.

use bm_stack::config::Config;
use bm_stack::mock::frames;
use bm_stack::node::SubscribeError;
use bm_stack::service::{
    RegisterError, SERVICE_REQUESTS, ServiceHandler, ServiceRequestError, UnregisterError,
};
use bm_stack::{
    Event, Identity, NoConfig, NoDfu, NoServices, Node, RamConfigStorage, Services, SoftRtc,
};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::bcmp::resource::ResourceType;
use bm_wire::configuration::{Key, Layout, Partition};
use bm_wire::crc::crc32_ieee;
use bm_wire::pubsub::{self, Subscriber, SubscriptionError};
use bm_wire::service::sys_info::{DecodedSysInfoReply, SysInfoReply};
use bm_wire::service::{REPLY_DATA_LEN, ReplyHeader, RequestHeader};
use bm_wire::udp;

const NODE_ID: u64 = 0xC0FF_EE00_1234_5678;
const PEER_ID: u64 = 0x0000_0000_55AA_0011;
const ECHO: &[u8] = b"c0ffee0012345678/echo";

struct TestIdentity;

impl Identity for TestIdentity {
    fn node_id(&self) -> u64 {
        NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo::default()
    }

    fn version_string(&self) -> &[u8] {
        b""
    }

    fn device_name(&self) -> &[u8] {
        b""
    }
}

/// Answers with the request reversed, and refuses a request starting `!`.
#[derive(Default)]
struct Reverser {
    calls: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Services for Reverser {
    fn handle(&mut self, service: &[u8], request: &[u8], reply: &mut [u8]) -> Option<usize> {
        self.calls.push((service.to_vec(), request.to_vec()));
        if request.first() == Some(&b'!') {
            return None;
        }
        for (out, byte) in reply.iter_mut().zip(request.iter().rev()) {
            *out = *byte;
        }
        Some(request.len())
    }
}

type TestNode =
    Node<TestIdentity, SoftRtc, 4, 4, 64, 8, 64, 16, 64, 4, 8, NoConfig, NoDfu, Reverser>;

fn node() -> TestNode {
    let mut node = Node::with_services(
        TestIdentity,
        SoftRtc::new(),
        NoConfig,
        NoDfu,
        Reverser::default(),
        2,
    );
    node.set_link_up(1, true);
    node.set_link_up(2, true);
    node
}

/// A received frame's reply, if any: its topic and body, and the
/// application's deliveries `(subscription, source, topic)`.
type Received = (Option<(Vec<u8>, Vec<u8>)>, Vec<(Vec<u8>, u64, Vec<u8>)>);

fn receive(node: &mut TestNode, mut frame: Vec<u8>) -> Received {
    let mut delivered = Vec::new();
    let owed = node.on_frame_with(0, 1, &mut frame, |event| {
        if let Event::Publication {
            subscription,
            source,
            topic,
            ..
        } = event
        {
            delivered.push((subscription.to_vec(), source, topic.to_vec()));
        }
    });
    let reply = owed.reply.map(|outbound| {
        assert_eq!(outbound.mask(), 0b11, "every port");
        let datagram = udp::accept(outbound.frame()).expect("a datagram");
        assert_eq!(
            (datagram.src_port, datagram.dst_port),
            (pubsub::PORT, pubsub::PORT)
        );
        let publication = pubsub::decode(datagram.payload).expect("a publication");
        assert_eq!(
            (publication.kind, publication.version),
            (0, pubsub::COMMON_VERSION)
        );
        (publication.topic.to_vec(), publication.data.to_vec())
    });
    (reply, delivered)
}

fn reply_body(id: u32, data: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8; ReplyHeader::LEN];
    ReplyHeader {
        target_node_id: PEER_ID,
        id,
        data_size: data.len() as u32,
    }
    .encode(&mut body)
    .unwrap();
    body.extend_from_slice(data);
    body
}

#[test]
fn echo_answers_with_the_request() {
    let mut node = node();
    node.register_echo_service().unwrap();
    assert!(
        node.service_table()
            .iter()
            .eq([(ECHO, ServiceHandler::Echo)])
    );
    let request_topic = b"c0ffee0012345678/echo/req";
    assert_eq!(
        node.subscriptions().callbacks(request_topic),
        Some(&[Subscriber::Service][..])
    );
    assert!(
        node.resources()
            .iter(ResourceType::Subscriber)
            .eq([&request_topic[..]])
    );

    let (reply, delivered) = receive(
        &mut node,
        frames::service_request(PEER_ID, ECHO, 7, b"hello"),
    );
    assert_eq!(
        reply,
        Some((
            b"c0ffee0012345678/echo/rep".to_vec(),
            reply_body(7, b"hello")
        ))
    );
    assert!(delivered.is_empty(), "the application subscribed nothing");
    assert!(
        node.resources()
            .iter(ResourceType::Publisher)
            .eq([&b"c0ffee0012345678/echo/rep"[..]])
    );
}

/// Divergence #90: the C copies past its buffer; here there is no reply.
#[test]
fn echo_refuses_what_does_not_fit_the_reply() {
    let mut node = node();
    node.register_echo_service().unwrap();
    let fits = vec![0xa5; REPLY_DATA_LEN];
    let (reply, _) = receive(&mut node, frames::service_request(PEER_ID, ECHO, 1, &fits));
    assert_eq!(reply.unwrap().1, reply_body(1, &fits));
    let over = vec![0xa5; REPLY_DATA_LEN + 1];
    let (reply, _) = receive(&mut node, frames::service_request(PEER_ID, ECHO, 1, &over));
    assert_eq!(reply, None);
}

#[test]
fn the_application_answers_its_services() {
    let mut node = node();
    node.register_service(b"svc").unwrap();
    let (reply, _) = receive(
        &mut node,
        frames::service_request(PEER_ID, b"svc", 9, b"abc"),
    );
    assert_eq!(reply, Some((b"svc/rep".to_vec(), reply_body(9, b"cba"))));
    let (reply, _) = receive(
        &mut node,
        frames::service_request(PEER_ID, b"svc", 9, b"!no"),
    );
    assert_eq!(reply, None, "a handler returning false");
    assert_eq!(
        node.services().calls,
        [
            (b"svc".to_vec(), b"abc".to_vec()),
            (b"svc".to_vec(), b"!no".to_vec())
        ]
    );
}

#[test]
fn a_malformed_request_calls_nothing() {
    let mut node = node();
    node.register_service(b"svc").unwrap();
    // `data_size` one more than the data.
    let mut body = vec![0, 0, 0, 0, 4, 0, 0, 0];
    body.extend_from_slice(b"abc");
    let frame = frames::publication(PEER_ID, b"svc/req", 0, 2, &body);
    assert_eq!(receive(&mut node, frame).0, None);
    // Shorter than the header.
    let frame = frames::publication(PEER_ID, b"svc/req", 0, 2, &body[..7]);
    assert_eq!(receive(&mut node, frame).0, None);
    assert!(node.services().calls.is_empty());
}

/// Divergence #89: `a` prefixes `ab/req`, and is listed first.
#[test]
fn a_prefixing_name_shadows_a_later_service() {
    let mut node = node();
    node.register_service(b"a").unwrap();
    node.register_service(b"ab").unwrap();
    let (reply, _) = receive(&mut node, frames::service_request(PEER_ID, b"ab", 1, b"x"));
    assert_eq!(reply, None);
    assert!(node.services().calls.is_empty());
    let (reply, _) = receive(&mut node, frames::service_request(PEER_ID, b"a", 1, b"x"));
    assert!(reply.is_some());
}

/// Divergence #89: unregistering `a` removes `ab`, and leaves `ab/req`
/// subscribed with nothing listed to answer it.
#[test]
fn unregistering_removes_the_first_service_the_name_prefixes() {
    let mut node = node();
    node.register_service(b"ab").unwrap();
    node.register_service(b"a").unwrap();
    node.unregister_service(b"a").unwrap();
    assert!(
        node.service_table()
            .iter()
            .eq([(&b"a"[..], ServiceHandler::Application)])
    );
    assert!(node.subscriptions().iter().eq([&b"ab/req"[..]]));
    assert_eq!(
        node.unregister_service(b"a"),
        Err(UnregisterError::Unsubscribe(
            SubscriptionError::NotSubscribed
        ))
    );
    assert_eq!(
        node.unregister_service(b"ab"),
        Err(UnregisterError::NotListed),
        "unsubscribed, but nothing starts with `ab`"
    );
    assert!(node.subscriptions().is_empty());
}

/// Divergence #79 through the service layer: the application subscribed
/// first, so each registration lists the service callback again. The C
/// replies once per listing; this replies once.
#[test]
fn a_service_listed_twice_on_a_topic_replies_once() {
    let mut node = node();
    node.subscribe(b"s/req").unwrap();
    node.register_service(b"s").unwrap();
    node.register_service(b"s").unwrap();
    assert_eq!(
        node.subscriptions().callbacks(b"s/req"),
        Some(
            &[
                Subscriber::Application,
                Subscriber::Service,
                Subscriber::Service
            ][..]
        )
    );
    assert_eq!(node.service_table().len(), 2);
    let (reply, delivered) = receive(&mut node, frames::service_request(PEER_ID, b"s", 3, b"q"));
    assert_eq!(reply, Some((b"s/rep".to_vec(), reply_body(3, b"q"))));
    assert_eq!(node.services().calls.len(), 1);
    assert_eq!(delivered, [(b"s/req".to_vec(), PEER_ID, b"s/req".to_vec())]);
}

/// The reply reaches the application's own matching subscriptions, after
/// the request does, from this node's id.
#[test]
fn the_reply_is_delivered_locally() {
    let mut node = node();
    node.register_service(b"s").unwrap();
    node.subscribe(b"s/").unwrap();
    let (reply, delivered) = receive(&mut node, frames::service_request(PEER_ID, b"s", 3, b"q"));
    assert!(reply.is_some());
    assert_eq!(
        delivered,
        [
            (b"s/".to_vec(), PEER_ID, b"s/req".to_vec()),
            (b"s/".to_vec(), NODE_ID, b"s/rep".to_vec())
        ]
    );
}

/// A service-only topic has no application callback to remove, and the
/// application subscribing after the service layer is listed after it,
/// twice if it subscribes twice (divergence #79).
#[test]
fn the_application_and_the_service_layer_share_topics() {
    let mut node = node();
    node.register_service(b"s").unwrap();
    assert_eq!(
        node.unsubscribe(b"s/req"),
        Err(SubscriptionError::NoSuchSubscriber)
    );
    node.subscribe(b"s/req").unwrap();
    node.subscribe(b"s/req").unwrap();
    let (_, delivered) = receive(&mut node, frames::service_request(PEER_ID, b"s", 3, b"q"));
    assert_eq!(delivered.len(), 2);
}

#[test]
fn registration_refuses_past_its_ceilings() {
    let mut node = node();
    assert_eq!(
        node.register_service(&[b'n'; 49]),
        Err(RegisterError::Full),
        "longer than SERVICE_NAME_BYTES"
    );
    for _ in 0..bm_stack::service::SERVICES {
        node.register_service(b"n").unwrap();
    }
    assert_eq!(node.register_service(b"n"), Err(RegisterError::Full));
    assert_eq!(node.service_table().len(), bm_stack::service::SERVICES);

    // Listed and subscribed, not advertised: the resource table holds 16.
    let mut node = self::node();
    for i in 0..16u8 {
        node.add_resource(&[b'r', i], ResourceType::Publisher)
            .unwrap();
    }
    assert_eq!(
        node.register_service(b"t"),
        Err(RegisterError::Subscribe(SubscribeError::NotAdvertised))
    );
    assert_eq!(node.service_table().len(), 1);
}

// ---------------------------------------------------------------------------
// Service requests -- card S2.
// ---------------------------------------------------------------------------

const PEER_ECHO: &[u8] = b"0000000055aa0011/echo";

/// What a service request's events carried: `(ack, id, service, data)`.
type Answer = (bool, u32, Vec<u8>, Vec<u8>);

fn answer(event: Event<'_>) -> Option<Answer> {
    match event {
        Event::ServiceReply { id, service, data } => {
            Some((true, id, service.to_vec(), data.to_vec()))
        }
        Event::ServiceTimeout { id, service } => Some((false, id, service.to_vec(), Vec::new())),
        _ => None,
    }
}

fn reply_to(node: &mut TestNode, now_ms: u32, mut frame: Vec<u8>) -> Vec<Answer> {
    let mut answers = Vec::new();
    let owed = node.on_frame_with(now_ms, 1, &mut frame, |e| answers.extend(answer(e)));
    assert!(owed.reply.is_none());
    answers
}

fn expire(node: &mut TestNode, now_ms: u32) -> Vec<Answer> {
    let mut answers = Vec::new();
    node.on_service_expiry(now_ms, |e| answers.extend(answer(e)));
    answers
}

#[test]
fn a_request_is_published_and_its_reply_reported() {
    let mut node = node();
    let (id, outbound) = node.service_request(100, PEER_ECHO, b"hi", 5).unwrap();
    assert_eq!(id, 0);
    assert_eq!(outbound.mask(), 0b11);
    let datagram = udp::accept(outbound.frame()).expect("a datagram");
    let publication = pubsub::decode(datagram.payload).expect("a publication");
    assert_eq!(publication.topic, b"0000000055aa0011/echo/req");
    assert_eq!(
        (publication.kind, publication.version),
        (0, pubsub::COMMON_VERSION)
    );
    assert_eq!(publication.data, b"\0\0\0\0\x02\0\0\0hi");
    assert_eq!(
        RequestHeader::decode(publication.data),
        Ok(RequestHeader {
            id: 0,
            data_size: 2
        })
    );

    let reply_topic = b"0000000055aa0011/echo/rep";
    assert_eq!(
        node.subscriptions().callbacks(reply_topic),
        Some(&[Subscriber::Reply][..])
    );
    assert!(
        node.resources()
            .iter(ResourceType::Subscriber)
            .eq([&reply_topic[..]])
    );
    assert!(
        node.resources()
            .iter(ResourceType::Publisher)
            .eq([&b"0000000055aa0011/echo/req"[..]])
    );
    let request = node.service_requests().iter().next().unwrap();
    assert_eq!(
        (
            request.id(),
            request.service(),
            request.start_ms(),
            request.timeout_ms()
        ),
        (0, PEER_ECHO, 100, 5000)
    );

    let reply = frames::service_reply(PEER_ID, PEER_ECHO, NODE_ID, 0, b"ok");
    assert_eq!(
        reply_to(&mut node, 200, reply.clone()),
        [(true, 0, PEER_ECHO.to_vec(), b"ok".to_vec())]
    );
    assert!(node.service_requests().is_empty());
    assert_eq!(reply_to(&mut node, 300, reply), [], "answered once");
    assert_eq!(
        node.subscriptions().callbacks(reply_topic),
        Some(&[Subscriber::Reply][..]),
        "the subscription stays"
    );
    assert_eq!(node.service_request(0, PEER_ECHO, b"", 5).unwrap().0, 1);
}

#[test]
fn a_reply_must_name_this_node_and_a_waiting_id() {
    let mut node = node();
    node.service_request(0, PEER_ECHO, b"", 5).unwrap();
    for frame in [
        frames::service_reply(PEER_ID, PEER_ECHO, PEER_ID, 0, b""),
        frames::service_reply(PEER_ID, PEER_ECHO, NODE_ID, 1, b""),
    ] {
        assert_eq!(reply_to(&mut node, 0, frame), []);
    }
    // Fifteen bytes of reply header.
    let mut short = frames::service_reply(PEER_ID, PEER_ECHO, NODE_ID, 0, b"");
    short.truncate(short.len() - 1);
    assert_eq!(reply_to(&mut node, 0, short), [], "divergence #92");
    assert_eq!(node.service_requests().len(), 1);
}

/// Divergence #92: matched on id and target, not topic; `data_size` is
/// not checked against what arrived.
#[test]
fn a_reply_is_matched_by_id_alone() {
    let mut node = node();
    node.service_request(0, b"a", b"", 5).unwrap();
    node.service_request(0, b"b", b"", 5).unwrap();
    let reply = frames::service_reply(PEER_ID, b"b", NODE_ID, 0, b"xyz");
    assert_eq!(
        reply_to(&mut node, 0, reply),
        [(true, 0, b"a".to_vec(), b"xyz".to_vec())]
    );

    let mut reply = frames::service_reply(PEER_ID, b"b", NODE_ID, 1, b"xyz");
    let size = reply.len() - 3 - 4;
    reply[size..size + 4].copy_from_slice(&1000u32.to_le_bytes());
    // The UDP checksum is not verified on receive (divergence #70).
    assert_eq!(
        reply_to(&mut node, 0, reply),
        [(true, 1, b"b".to_vec(), b"xyz".to_vec())]
    );
}

#[test]
fn an_unanswered_request_times_out_on_the_sweeps_grid() {
    let mut node = node();
    node.service_request(300, PEER_ECHO, b"", 1).unwrap();
    node.service_request(300, PEER_ECHO, b"", 0).unwrap();
    assert_eq!(expire(&mut node, 499), []);
    assert_eq!(
        expire(&mut node, 500),
        [(false, 1, PEER_ECHO.to_vec(), vec![])]
    );
    assert_eq!(expire(&mut node, 1000), [], "300 + 1000 is still ahead");
    assert_eq!(expire(&mut node, 1499), []);
    assert_eq!(
        expire(&mut node, 1500),
        [(false, 0, PEER_ECHO.to_vec(), vec![])]
    );
    assert!(node.service_requests().is_empty());
    let reply = frames::service_reply(PEER_ID, PEER_ECHO, NODE_ID, 0, b"late");
    assert_eq!(reply_to(&mut node, 1600, reply), []);
}

#[test]
fn on_tick_runs_the_service_sweep() {
    let mut node = node();
    node.service_request(0, PEER_ECHO, b"", 0).unwrap();
    let mut answers = Vec::new();
    node.on_tick_with(500, |e| answers.extend(answer(e)));
    assert_eq!(answers, [(false, 0, PEER_ECHO.to_vec(), vec![])]);
}

#[test]
fn a_request_is_delivered_to_the_application_not_to_a_service() {
    let mut node = node();
    node.register_service(b"svc").unwrap();
    node.subscribe(b"*").unwrap();
    let mut delivered = Vec::new();
    let (_, outbound) = node
        .service_request_with(0, b"svc", b"q", 5, |event| {
            if let Event::Publication { source, topic, .. } = event {
                delivered.push((source, topic.to_vec()));
            }
        })
        .unwrap();
    assert!(udp::accept(outbound.frame()).is_ok());
    assert_eq!(delivered, [(NODE_ID, b"svc/req".to_vec())]);
    assert!(node.services().calls.is_empty(), "not answered locally");
}

/// A reply this node publishes reaches its own reply subscription; it
/// answers a request only when it targets this node.
#[test]
fn a_local_reply_answers_a_request_naming_this_node() {
    let mut node = node();
    node.register_service(b"svc").unwrap();
    node.service_request(0, b"svc", b"", 5).unwrap();
    let mut answers = Vec::new();
    let mut frame = frames::service_request(NODE_ID, b"svc", 0, b"ab");
    let owed = node.on_frame_with(0, 1, &mut frame, |e| answers.extend(answer(e)));
    assert!(owed.reply.is_some());
    assert_eq!(answers, [(true, 0, b"svc".to_vec(), b"ba".to_vec())]);
}

#[test]
fn requests_refuse_past_their_ceilings() {
    let mut node = node();
    assert_eq!(
        node.service_request(0, PEER_ECHO, &[0; 1025], 5).err(),
        Some(ServiceRequestError::TooLarge)
    );
    assert_eq!(
        node.service_request(0, &[b'n'; 49], b"", 5).err(),
        Some(ServiceRequestError::Full)
    );
    for id in 0..SERVICE_REQUESTS as u32 {
        assert_eq!(node.service_request(0, b"n", &[0; 1024], 5).unwrap().0, id);
    }
    assert_eq!(
        node.service_request(0, b"n", b"", 5).err(),
        Some(ServiceRequestError::Full)
    );
    assert_eq!(node.service_requests().next_id(), SERVICE_REQUESTS as u32);
}

/// Divergence #91: a request whose reply topic is not advertised stays
/// listed, and times out.
#[test]
fn a_request_not_subscribed_stays_listed() {
    let mut node = node();
    for i in 0..16u8 {
        node.add_resource(&[b'r', i], ResourceType::Publisher)
            .unwrap();
    }
    assert_eq!(
        node.service_request(0, b"t", b"", 0).err(),
        Some(ServiceRequestError::NotSubscribed {
            id: 0,
            error: SubscribeError::NotAdvertised
        })
    );
    assert_eq!(expire(&mut node, 500), [(false, 0, b"t".to_vec(), vec![])]);
}

/// The run loop sweeps on its own arm.
#[test]
fn the_run_loop_times_a_request_out() {
    use bm_stack::mock::{MockError, MockPhy, Script};
    let mut node = node();
    node.service_request(0, PEER_ECHO, b"", 1).unwrap();
    let mut phy = MockPhy::new(2, vec![Script::Idle { ms: 50 }; 24]);
    let mut answers = Vec::new();
    let error = embassy_futures::block_on(node.run_with(&mut phy, |e| answers.extend(answer(e))));
    assert_eq!(error, MockError::ScriptFinished);
    assert_eq!(answers, [(false, 0, PEER_ECHO.to_vec(), vec![])]);
}

// ---------------------------------------------------------------------------
// sys_info -- card E1.
// ---------------------------------------------------------------------------

const SYS_INFO: &[u8] = b"c0ffee0012345678/sys_info";

struct NamedIdentity;

impl Identity for NamedIdentity {
    fn node_id(&self) -> u64 {
        NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            git_sha: 0x0bad_cafe,
            ..DeviceInfo::default()
        }
    }

    fn app_name(&self) -> &[u8] {
        b"bm_rs_test"
    }
}

type ConfigNode = Node<
    NamedIdentity,
    SoftRtc,
    4,
    4,
    64,
    8,
    64,
    16,
    64,
    4,
    8,
    Config<RamConfigStorage>,
    NoDfu,
    NoServices,
>;

fn decoded(body: &[u8]) -> (u64, u32, u32, u32, Vec<u8>) {
    let header = ReplyHeader::decode(body).unwrap();
    let data = &body[ReplyHeader::LEN..];
    assert_eq!(header.data_size as usize, data.len());
    let mut d = DecodedSysInfoReply::default();
    d.decode_into(data).unwrap();
    let name = d.app_name.unwrap();
    let mut buf = [0u8; 64];
    let len = name.copy_to(&mut buf).unwrap();
    (
        d.node_id,
        d.git_sha,
        d.sys_config_crc,
        d.app_name_strlen,
        buf[..len].to_vec(),
    )
}

#[test]
fn sys_info_answers_with_the_identity_and_the_system_partitions_crc() {
    let mut config = Config::load(Layout::LP64, RamConfigStorage::new());
    config
        .store
        .partition_mut(Partition::System)
        .set_uint(Key::new(b"sampleIntervalMs"), 60_000);
    let crc = config.store.partition(Partition::System).cbor_map_crc32();
    assert_ne!(crc, crc32_ieee(&[0xa0]), "not the empty partition's");
    let mut node: ConfigNode =
        Node::with_services(NamedIdentity, SoftRtc::new(), config, NoDfu, NoServices, 2);
    node.register_sys_info_service().unwrap();
    assert!(
        node.service_table()
            .iter()
            .eq([(SYS_INFO, ServiceHandler::SysInfo)])
    );
    assert_eq!(
        node.subscriptions()
            .callbacks(b"c0ffee0012345678/sys_info/req"),
        Some(&[Subscriber::Service][..])
    );

    let mut frame = frames::service_request(PEER_ID, SYS_INFO, 3, b"");
    let owed = node.on_frame(0, 1, &mut frame);
    let reply = owed.reply.unwrap();
    let datagram = udp::accept(reply.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(publication.topic, b"c0ffee0012345678/sys_info/rep");
    assert_eq!(&publication.data[8..12], &3u32.to_le_bytes(), "the id");
    assert_eq!(
        decoded(publication.data),
        (NODE_ID, 0x0bad_cafe, crc, 10, b"bm_rs_test".to_vec())
    );

    // Any data: no reply.
    let mut frame = frames::service_request(PEER_ID, SYS_INFO, 4, b"x");
    assert!(node.on_frame(0, 1, &mut frame).reply.is_none());
}

#[test]
fn sys_info_without_a_store_sends_the_empty_partitions_crc() {
    let mut node = node();
    node.register_sys_info_service().unwrap();
    let (reply, _) = receive(
        &mut node,
        frames::service_request(PEER_ID, SYS_INFO, 1, b""),
    );
    let (topic, body) = reply.unwrap();
    assert_eq!(topic, b"c0ffee0012345678/sys_info/rep");
    assert_eq!(
        decoded(&body),
        (NODE_ID, 0, crc32_ieee(&[0xa0]), 0, Vec::new())
    );
}

#[test]
fn sys_info_request_asks_the_target_with_no_data() {
    let mut node = node();
    let (id, outbound) = node.sys_info_request(0, PEER_ID, 5).unwrap();
    let datagram = udp::accept(outbound.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(publication.topic, b"0000000055aa0011/sys_info/req");
    assert_eq!(
        RequestHeader::decode(publication.data),
        Ok(RequestHeader { id, data_size: 0 })
    );
    assert_eq!(publication.data.len(), RequestHeader::LEN);

    let mut body = [0u8; 128];
    let len = SysInfoReply::new(PEER_ID, 1, 2, b"peer")
        .encode(&mut body)
        .unwrap();
    let reply = frames::service_reply(
        PEER_ID,
        b"0000000055aa0011/sys_info",
        NODE_ID,
        id,
        &body[..len],
    );
    let answers = reply_to(&mut node, 10, reply);
    assert_eq!(answers.len(), 1);
    let (ack, answered, service, data) = &answers[0];
    assert!(*ack);
    assert_eq!(*answered, id);
    assert_eq!(service, b"0000000055aa0011/sys_info");
    let mut d = DecodedSysInfoReply::default();
    d.decode_into(data).unwrap();
    assert_eq!((d.node_id, d.git_sha, d.sys_config_crc), (PEER_ID, 1, 2));
}

/// `git_sha` reaches a device-info reply, as `git_sha()` does in the C.
#[test]
fn identity_git_sha_defaults_to_the_device_infos() {
    assert_eq!(NamedIdentity.git_sha(), 0x0bad_cafe);
    assert_eq!(TestIdentity.app_name(), b"");
}
