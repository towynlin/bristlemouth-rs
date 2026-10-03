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
use bm_wire::configuration::{Key, Layout, MapError, Partition};
use bm_wire::crc::crc32_ieee;
use bm_wire::pubsub::{self, Subscriber, SubscriptionError};
use bm_wire::service::config_map::{self, ConfigMapReply, ConfigMapRequest, DecodedConfigMapReply};
use bm_wire::service::metrics::{self, Component, ComponentMut, Entry, Field};
use bm_wire::service::power_info::PowerInfoReply;
use bm_wire::service::sys_info::{DecodedSysInfoReply, SysInfoReply};
use bm_wire::service::{REPLY_DATA_LEN, ReplyHeader, RequestHeader};
use bm_wire::udp;

const NODE_ID: u64 = 0xC0FF_EE00_1234_5678;
const PEER_ID: u64 = 0x0000_0000_55AA_0011;
const ECHO: &[u8] = b"c0ffee0012345678/echo";
const METRICS: &[u8] = b"c0ffee0012345678/metrics";

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
/// Its power stats are `power`, counted in `power_calls`. Metrics off, so
/// the service list holds only what a test registers.
#[derive(Default)]
struct Reverser {
    calls: Vec<(Vec<u8>, Vec<u8>)>,
    power: Option<PowerInfoReply>,
    power_calls: usize,
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

    fn power_info(&mut self) -> Option<PowerInfoReply> {
        self.power_calls += 1;
        self.power
    }

    const METRICS: bool = false;
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
        node.service_table().iter().eq([
            (METRICS, ServiceHandler::Metrics),
            (SYS_INFO, ServiceHandler::SysInfo)
        ]),
        "metrics first, as `bristlemouth_init` lists it"
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

// ---------------------------------------------------------------------------
// config_map -- card E2.
// ---------------------------------------------------------------------------

const CONFIG_MAP: &[u8] = b"c0ffee0012345678/config_map";

fn config_map_request(partition_id: u32) -> Vec<u8> {
    let mut data = [0u8; 32];
    let len = ConfigMapRequest { partition_id }.encode(&mut data).unwrap();
    data[..len].to_vec()
}

/// `(node id, partition id, success, data)` of a reply body.
fn config_map_reply(body: &[u8]) -> (u64, u32, bool, Vec<u8>) {
    let header = ReplyHeader::decode(body).unwrap();
    let data = &body[ReplyHeader::LEN..];
    assert_eq!(header.data_size as usize, data.len());
    let mut d = DecodedConfigMapReply::default();
    d.decode_into(data).unwrap();
    let mut map = vec![0u8; d.cbor_encoded_map_len as usize];
    if let Some(s) = d.cbor_data {
        s.copy_to(&mut map).unwrap();
    }
    (d.node_id, d.partition_id, d.success, map)
}

#[test]
fn config_map_answers_with_the_partition_named() {
    let mut config = Config::load(Layout::LP64, RamConfigStorage::new());
    config
        .store
        .partition_mut(Partition::User)
        .set_uint(Key::new(b"sampleIntervalMs"), 60_000);
    let mut user = [0u8; 64];
    let len = config
        .store
        .partition(Partition::User)
        .cbor_map(&mut user)
        .unwrap();
    let user = user[..len].to_vec();
    let mut node: ConfigNode =
        Node::with_services(NamedIdentity, SoftRtc::new(), config, NoDfu, NoServices, 2);
    node.register_config_map_service().unwrap();
    assert!(
        node.service_table().iter().eq([
            (METRICS, ServiceHandler::Metrics),
            (CONFIG_MAP, ServiceHandler::ConfigMap)
        ]),
        "metrics first, as `bristlemouth_init` lists it"
    );

    let mut answer = |id: u32, request: &[u8]| {
        let mut frame = frames::service_request(PEER_ID, CONFIG_MAP, id, request);
        let owed = node.on_frame(0, 1, &mut frame);
        owed.reply.map(|reply| {
            let datagram = udp::accept(reply.frame()).unwrap();
            let publication = pubsub::decode(datagram.payload).unwrap();
            assert_eq!(publication.topic, b"c0ffee0012345678/config_map/rep");
            assert_eq!(&publication.data[8..12], &id.to_le_bytes(), "the id");
            config_map_reply(publication.data)
        })
    };
    assert_eq!(
        answer(1, &config_map_request(config_map::PARTITION_ID_USER)),
        Some((NODE_ID, 3, true, user))
    );
    assert_eq!(
        answer(2, &config_map_request(config_map::PARTITION_ID_SYS)),
        Some((NODE_ID, 1, true, vec![0xa0]))
    );
    // An unknown partition is answered, unsuccessfully.
    assert_eq!(
        answer(3, &config_map_request(0)),
        Some((NODE_ID, 0, false, vec![]))
    );
    // A request that does not decode is not.
    assert_eq!(answer(4, b""), None);
    assert_eq!(answer(5, b"\xa0"), None);
}

/// A map over the handler's buffer gets no reply (contract 8).
#[test]
fn config_map_sends_nothing_for_a_map_past_its_buffer() {
    let mut config = Config::load(Layout::LP64, RamConfigStorage::new());
    let hw = config.store.partition_mut(Partition::Hardware);
    for i in 0..30u8 {
        let key = [b'k', b'0' + i / 10, b'0' + i % 10];
        assert!(hw.set_string(Key::new(&key), &[b'v'; 40]));
    }
    let map = config
        .store
        .partition(Partition::Hardware)
        .cbor_map(&mut []);
    assert!(
        matches!(map, Err(MapError::TooSmall(n)) if n > REPLY_DATA_LEN),
        "{map:?}"
    );
    let mut node: ConfigNode =
        Node::with_services(NamedIdentity, SoftRtc::new(), config, NoDfu, NoServices, 2);
    node.register_config_map_service().unwrap();
    let mut frame = frames::service_request(
        PEER_ID,
        CONFIG_MAP,
        1,
        &config_map_request(config_map::PARTITION_ID_HW),
    );
    assert!(node.on_frame(0, 1, &mut frame).reply.is_none());
}

#[test]
fn config_map_without_a_store_sends_an_empty_map() {
    let mut node = node();
    node.register_config_map_service().unwrap();
    let (reply, _) = receive(
        &mut node,
        frames::service_request(
            PEER_ID,
            CONFIG_MAP,
            1,
            &config_map_request(config_map::PARTITION_ID_HW),
        ),
    );
    let (topic, body) = reply.unwrap();
    assert_eq!(topic, b"c0ffee0012345678/config_map/rep");
    assert_eq!(config_map_reply(&body), (NODE_ID, 2, true, vec![0xa0]));
}

#[test]
fn config_map_request_asks_the_target_for_a_partition() {
    let mut node = node();
    let (id, outbound) = node
        .config_map_request(0, PEER_ID, config_map::PARTITION_ID_USER, 5)
        .unwrap();
    let datagram = udp::accept(outbound.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(publication.topic, b"0000000055aa0011/config_map/req");
    let data = config_map_request(3);
    assert_eq!(
        RequestHeader::decode(publication.data),
        Ok(RequestHeader {
            id,
            data_size: data.len() as u32
        })
    );
    assert_eq!(&publication.data[RequestHeader::LEN..], &data[..]);

    let mut body = [0u8; 128];
    let len = ConfigMapReply {
        node_id: PEER_ID,
        partition_id: 3,
        success: true,
        cbor_data: b"\xa0",
    }
    .encode(&mut body)
    .unwrap();
    let reply = frames::service_reply(
        PEER_ID,
        b"0000000055aa0011/config_map",
        NODE_ID,
        id,
        &body[..len],
    );
    let answers = reply_to(&mut node, 10, reply);
    assert_eq!(answers.len(), 1);
    let (ack, answered, service, data) = &answers[0];
    assert!(*ack);
    assert_eq!(*answered, id);
    assert_eq!(service, b"0000000055aa0011/config_map");
    let mut d = DecodedConfigMapReply::default();
    d.decode_into(data).unwrap();
    assert_eq!((d.node_id, d.partition_id, d.success), (PEER_ID, 3, true));
}

// ---------------------------------------------------------------------------
// power_info -- card E3.
// ---------------------------------------------------------------------------

const POWER_INFO: &[u8] = b"bus_power_controller/timing";

const STATS: PowerInfoReply = PowerInfoReply {
    total_on_s: 310,
    remaining_on_s: 121,
    upcoming_off_s: 1500,
};

fn power_info_body(reply: PowerInfoReply) -> Vec<u8> {
    let mut body = [0u8; 64];
    let len = reply.encode(&mut body).unwrap();
    body[..len].to_vec()
}

#[test]
fn power_info_answers_an_empty_request_with_the_stats() {
    let mut node = node();
    node.register_power_info_service().unwrap();
    assert!(
        node.service_table()
            .iter()
            .eq([(POWER_INFO, ServiceHandler::PowerInfo)])
    );

    // No stats: no reply, as the C's handler with no callback.
    let (reply, _) = receive(
        &mut node,
        frames::service_request(PEER_ID, POWER_INFO, 1, b""),
    );
    assert_eq!(reply, None);
    assert_eq!(node.services().power_calls, 1);

    node.services_mut().power = Some(STATS);
    let (reply, _) = receive(
        &mut node,
        frames::service_request(PEER_ID, POWER_INFO, 2, b""),
    );
    let (topic, body) = reply.unwrap();
    assert_eq!(topic, b"bus_power_controller/timing/rep");
    let header = ReplyHeader::decode(&body).unwrap();
    assert_eq!((header.target_node_id, header.id), (PEER_ID, 2));
    let mut d = PowerInfoReply::default();
    d.decode_into(&body[ReplyHeader::LEN..]).unwrap();
    assert_eq!(d, STATS);
    assert_eq!(node.services().power_calls, 2);

    // A request with data is refused before the stats are read.
    let (reply, _) = receive(
        &mut node,
        frames::service_request(PEER_ID, POWER_INFO, 3, b"x"),
    );
    assert_eq!(reply, None);
    assert_eq!(node.services().power_calls, 2);
}

/// What a power_info request's events carried: `(id, reply)`, and every other
/// answer.
fn power_answers(events: &mut Vec<(u32, PowerInfoReply)>, other: &mut Vec<Answer>, e: Event<'_>) {
    match e {
        Event::PowerInfoReply { id, reply } => events.push((id, reply)),
        e => other.extend(answer(e)),
    }
}

fn power_reply(
    node: &mut TestNode,
    id: u32,
    body: &[u8],
) -> (Vec<(u32, PowerInfoReply)>, Vec<Answer>) {
    let mut frame = frames::service_reply(PEER_ID, POWER_INFO, NODE_ID, id, body);
    let (mut power, mut other) = (Vec::new(), Vec::new());
    node.on_frame_with(10, 1, &mut frame, |e| {
        power_answers(&mut power, &mut other, e)
    });
    (power, other)
}

#[test]
fn power_info_request_asks_the_bus() {
    let mut node = node();
    let (id, outbound) = node.power_info_request(0, 5).unwrap();
    let datagram = udp::accept(outbound.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(publication.topic, b"bus_power_controller/timing/req");
    assert_eq!(
        RequestHeader::decode(publication.data),
        Ok(RequestHeader { id, data_size: 0 })
    );
    assert_eq!(publication.data.len(), RequestHeader::LEN);
    assert!(node.power_info_callbacks().queued().eq([id]));

    let (power, other) = power_reply(&mut node, id, &power_info_body(STATS));
    assert_eq!(power, vec![(id, STATS)]);
    assert!(other.is_empty(), "no ServiceReply: {other:?}");
    assert_eq!(node.power_info_callbacks().queued().count(), 0);
    assert!(node.service_requests().is_empty());
}

/// Divergence #96: a reply to the second request is reported to the first's
/// callback; the first's reply then goes to the second's.
#[test]
fn power_info_replies_pair_with_requests_in_order() {
    let mut node = node();
    let (first, _) = node.power_info_request(0, 5).unwrap();
    let (second, _) = node.power_info_request(0, 5).unwrap();
    let late = PowerInfoReply {
        total_on_s: 2,
        ..STATS
    };
    let (power, _) = power_reply(&mut node, second, &power_info_body(late));
    assert_eq!(power, vec![(first, late)]);
    let (power, _) = power_reply(&mut node, first, &power_info_body(STATS));
    assert_eq!(power, vec![(second, STATS)]);
}

/// An expiry reports nothing and uses up the oldest callback; so does a
/// reply that does not decode. Other requests still report as before.
#[test]
fn power_info_expiry_and_bad_replies_report_nothing() {
    let mut node = node();
    let (first, _) = node.power_info_request(0, 1).unwrap();
    let (echo, _) = node.service_request(0, PEER_ECHO, b"", 1).unwrap();
    let (second, _) = node.power_info_request(0, 5).unwrap();
    let (third, _) = node.power_info_request(0, 5).unwrap();
    assert!(
        node.power_info_callbacks()
            .queued()
            .eq([first, second, third])
    );

    let (mut power, mut other) = (Vec::new(), Vec::new());
    node.on_service_expiry(1000, |e| power_answers(&mut power, &mut other, e));
    assert!(power.is_empty());
    assert_eq!(other, vec![(false, echo, PEER_ECHO.to_vec(), Vec::new())]);
    assert!(node.power_info_callbacks().queued().eq([second, third]));

    let (power, other) = power_reply(&mut node, third, b"\xa0");
    assert!(power.is_empty() && other.is_empty());
    assert!(node.power_info_callbacks().queued().eq([third]));
    assert!(node.power_info_callbacks().is_waiting(second));

    let (power, _) = power_reply(&mut node, second, &power_info_body(STATS));
    assert_eq!(power, vec![(third, STATS)]);
}

/// Reports one component, `memory`, of `free` bytes, or a `String` field
/// when `string`. Counts its calls.
#[derive(Default)]
struct Meter {
    free: u32,
    string: bool,
    calls: usize,
}

impl Services for Meter {
    fn metrics<R>(&mut self, encode: impl FnOnce(&[Component<'_>]) -> R) -> R {
        self.calls += 1;
        let fields = [
            Entry {
                key: "free_bytes",
                field: Field::U32(self.free),
            },
            Entry {
                key: "name",
                field: Field::String,
            },
        ];
        let fields = if self.string {
            &fields[..]
        } else {
            &fields[..1]
        };
        encode(&[Component {
            key: "memory",
            fields,
        }])
    }
}

type MeterNode = Node<TestIdentity, SoftRtc, 4, 4, 64, 8, 64, 16, 64, 4, 8, NoConfig, NoDfu, Meter>;

fn meter_node() -> MeterNode {
    let mut node = Node::with_services(
        TestIdentity,
        SoftRtc::new(),
        NoConfig,
        NoDfu,
        Meter {
            free: 4096,
            ..Meter::default()
        },
        2,
    );
    node.set_link_up(1, true);
    node.set_link_up(2, true);
    node
}

/// A metrics reply's `(id, reply, free_bytes)`.
fn metrics_reply(body: &[u8]) -> (u32, metrics::Reply, u32) {
    let header = ReplyHeader::decode(body).unwrap();
    assert_eq!(header.target_node_id, PEER_ID);
    let data = &body[ReplyHeader::LEN..];
    assert_eq!(header.data_size as usize, data.len());
    let mut reply = metrics::Reply::default();
    let mut fields = [Entry {
        key: "free_bytes",
        field: Field::U32(0),
    }];
    metrics::decode(
        data,
        &mut reply,
        &mut [ComponentMut {
            key: "memory",
            fields: &mut fields,
        }],
    )
    .unwrap();
    let Field::U32(free) = fields[0].field else {
        unreachable!()
    };
    (header.id, reply, free)
}

#[test]
fn metrics_is_listed_at_construction() {
    let node = meter_node();
    assert!(
        node.service_table()
            .iter()
            .eq([(METRICS, ServiceHandler::Metrics)])
    );
    let topic = b"c0ffee0012345678/metrics/req";
    assert_eq!(
        node.subscriptions().callbacks(topic),
        Some(&[Subscriber::Service][..])
    );
    assert!(
        node.resources()
            .iter(ResourceType::Subscriber)
            .eq([&topic[..]])
    );
}

#[test]
fn metrics_answers_with_the_components_and_the_uptime() {
    let mut node = meter_node();
    let mut frame = frames::service_request(PEER_ID, METRICS, 7, b"");
    let owed = node.on_frame(123_456, 1, &mut frame);
    let reply = owed.reply.unwrap();
    let datagram = udp::accept(reply.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(publication.topic, b"c0ffee0012345678/metrics/rep");
    assert_eq!(
        metrics_reply(publication.data),
        (
            7,
            metrics::Reply {
                version: metrics::VERSION,
                node_id: NODE_ID,
                uptime_ms: 123_456
            },
            4096
        )
    );
    assert_eq!(node.services_mut().calls, 1);
}

/// Divergence #97: unlike sys_info and power_info, a request carrying data
/// is answered.
#[test]
fn metrics_answers_a_request_carrying_data() {
    let mut node = meter_node();
    let mut frame = frames::service_request(PEER_ID, METRICS, 8, b"anything");
    let owed = node.on_frame(5, 1, &mut frame);
    let reply = owed.reply.unwrap();
    let datagram = udp::accept(reply.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(metrics_reply(publication.data).0, 8);
}

/// Divergence #85: a `String` field fails the encode, and the handler sends
/// nothing.
#[test]
fn metrics_sends_nothing_for_a_string_field() {
    let mut node = meter_node();
    node.services_mut().string = true;
    let mut frame = frames::service_request(PEER_ID, METRICS, 9, b"");
    assert!(node.on_frame(5, 1, &mut frame).reply.is_none());
    assert_eq!(node.services_mut().calls, 1);
}

#[test]
fn no_services_answers_metrics_with_no_components() {
    let mut node: Node<TestIdentity, SoftRtc, 4> = Node::new(TestIdentity, SoftRtc::new(), 2);
    node.set_link_up(1, true);
    let mut frame = frames::service_request(PEER_ID, METRICS, 1, b"");
    let owed = node.on_frame(0, 1, &mut frame);
    let reply = owed.reply.unwrap();
    let datagram = udp::accept(reply.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    let data = &publication.data[ReplyHeader::LEN..];
    assert_eq!(&data[data.len() - 5..], b"data\xa0", "an empty map");
}

#[test]
fn metrics_request_asks_the_target() {
    let mut node = meter_node();
    let (id, outbound) = node.metrics_request(100, PEER_ID, 5).unwrap();
    let datagram = udp::accept(outbound.frame()).unwrap();
    let publication = pubsub::decode(datagram.payload).unwrap();
    assert_eq!(publication.topic, b"0000000055aa0011/metrics/req");
    assert_eq!(
        RequestHeader::decode(publication.data),
        Ok(RequestHeader { id, data_size: 0 })
    );
    let request = node.service_requests().iter().next().unwrap();
    assert_eq!(request.service(), b"0000000055aa0011/metrics");
}
