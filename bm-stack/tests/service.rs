//! Services on a node: registration, dispatch from `on_frame`, echo.
//! `bm-wire-diff/tests/services.rs` compares the same against the oracle.

use bm_stack::mock::frames;
use bm_stack::node::SubscribeError;
use bm_stack::service::{RegisterError, ServiceHandler, UnregisterError};
use bm_stack::{Event, Identity, NoConfig, NoDfu, Node, Services, SoftRtc};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::bcmp::resource::ResourceType;
use bm_wire::pubsub::{self, Subscriber, SubscriptionError};
use bm_wire::service::{REPLY_DATA_LEN, ReplyHeader};
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
