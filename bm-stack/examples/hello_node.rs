//! A BCMP node on the mock PHY, using only the public API.
//!
//! A scripted neighbour heartbeats and describes itself; an [`App`] subscribes
//! to `hello/*` and pings every node on its own timer, then publishes on the
//! next tick, then logs `hello world` to the Spotter console with
//! `spotter_log`. The neighbour answers the ping and publishes to
//! `hello/peer`. The example prints the neighbour table, the echo reply, the
//! publication received and what the node transmitted, and panics if any of
//! them is missing.
//!
//! ```text
//! cargo run -p bm-stack --example hello_node
//! ```
//!
//! Peer frames come from [`bm_stack::mock::frames`]. The `mock` feature comes
//! from `bm-stack`'s own dev-dependency on itself, as it does for the
//! integration tests.

use bm_stack::mock::{MockError, MockPhy, Script, frames};
use bm_stack::{App, Event, Identity, Node, Outbound, SoftRtc};
use bm_wire::bcmp::info::DeviceInfoReply;
use bm_wire::bcmp::ping::EchoReply;
use bm_wire::bcmp::{DeviceInfo, rx};
use bm_wire::spotter::USE_TIMESTAMP;
use bm_wire::util::BmIpAddr;
use bm_wire::{pubsub, udp};
use embassy_futures::block_on;
use embassy_time::{Duration, Ticker};

const NODE_ID: u64 = 0x0000_0000_0000_1234;
const PEER_ID: u64 = 0x0000_0000_0000_5678;
const PORTS: u8 = 2;
const PING_PAYLOAD: &[u8] = b"hello";
const SUBSCRIPTION: &[u8] = b"hello/*";

struct ExampleIdentity;

impl Identity for ExampleIdentity {
    fn node_id(&self) -> u64 {
        NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor_id: 0xBEEF,
            product_id: 0x0001,
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        b"0.1.0"
    }

    fn device_name(&self) -> &[u8] {
        b"hello-node"
    }
}

type ExampleNode = Node<ExampleIdentity, SoftRtc, 4>;

/// On the first tick of its own ticker, subscribes to [`SUBSCRIPTION`] and
/// pings every node; on the second, publishes; on the third, logs to the
/// Spotter console. Keeps what comes back.
struct Hello {
    ticker: Ticker,
    ticks: u8,
    /// `(source, round_trip_ms, payload)`.
    reply: Option<(u64, u32, Vec<u8>)>,
    /// `(source, topic, data)`.
    received: Vec<(u64, Vec<u8>, Vec<u8>)>,
}

impl App<ExampleNode> for Hello {
    async fn ready(&mut self) {
        if self.ticks == 3 {
            core::future::pending::<()>().await;
        }
        // Cancel-safe: the deadline lives in the ticker, not in this future.
        self.ticker.next().await;
    }

    fn act<'n>(&mut self, node: &'n mut ExampleNode, now_ms: u32) -> Option<Outbound<'n>> {
        self.ticks += 1;
        if self.ticks == 1 {
            println!("{now_ms:>5} ms  subscribe to hello/*, ping every node");
            node.subscribe(SUBSCRIPTION)
                .expect("room for one subscription");
            node.ping(now_ms, &BmIpAddr::LINK_LOCAL_MULTICAST, 0, PING_PAYLOAD)
        } else if self.ticks == 2 {
            println!("{now_ms:>5} ms  publish to hello/rust");
            node.publish(b"hello/rust", 1, pubsub::COMMON_VERSION, b"hello world")
                .ok()
        } else {
            println!("{now_ms:>5} ms  spotter_log_console: hello world");
            node.spotter_log(0, None, USE_TIMESTAMP, b"hello world")
                .ok()
        }
    }

    fn on_event(&mut self, event: Event<'_>) {
        match event {
            Event::EchoReply {
                source,
                reply,
                round_trip_ms,
            } => {
                println!("          echo reply from {source:016x} in {round_trip_ms} ms");
                self.reply = Some((source, round_trip_ms, reply.payload.to_vec()));
            }
            Event::Publication {
                source,
                topic,
                data,
                ..
            } => {
                println!(
                    "          {} from {source:016x}: {}",
                    String::from_utf8_lossy(topic),
                    String::from_utf8_lossy(data)
                );
                self.received.push((source, topic.to_vec(), data.to_vec()));
            }
            _ => {}
        }
    }
}

/// The peer describing itself, in answer to the info request its heartbeat
/// provokes.
fn device_info_reply() -> Vec<u8> {
    frames::device_info_reply(
        PEER_ID,
        &DeviceInfoReply {
            info: DeviceInfo {
                node_id: PEER_ID,
                vendor_id: 0xBEEF,
                product_id: 0x0002,
                ..DeviceInfo::default()
            },
            version_string: b"1.0.0",
            device_name: b"peer",
        },
    )
}

/// The peer's answer to our first ping: the id is the low 16 bits of our node
/// id, and the sequence number is the first one.
fn echo_reply() -> Vec<u8> {
    frames::echo_reply(
        PEER_ID,
        &EchoReply {
            node_id: PEER_ID,
            id: NODE_ID as u16,
            seq_num: 0,
            payload: PING_PAYLOAD,
        },
    )
}

fn main() {
    let mut node = ExampleNode::new(ExampleIdentity, SoftRtc::new(), PORTS);

    // Idle steps are short so the loop's tickers never have a backlog: a burst
    // would let `receive` take the echo reply before the ping went out.
    let mut script = vec![
        Script::Receive {
            port: 1,
            frame: frames::heartbeat(PEER_ID, 5_000_000),
        },
        Script::Receive {
            port: 1,
            frame: device_info_reply(),
        },
    ];
    script.extend(vec![Script::Idle { ms: 50 }; 12]);
    script.push(Script::Receive {
        port: 1,
        frame: echo_reply(),
    });
    script.extend(vec![Script::Idle { ms: 50 }; 12]);
    script.push(Script::Receive {
        port: 1,
        frame: frames::publication(PEER_ID, b"hello/peer", 1, 2, b"hello from the peer"),
    });
    script.extend(vec![Script::Idle { ms: 50 }; 14]);
    let mut phy = MockPhy::new(PORTS, script);

    let mut app = Hello {
        ticker: Ticker::every(Duration::from_millis(500)),
        ticks: 0,
        reply: None,
        received: Vec::new(),
    };

    let stopped = block_on(node.run_app(&mut phy, &mut app));
    assert_eq!(stopped, MockError::ScriptFinished);

    println!("\nneighbours of {NODE_ID:016x}:");
    for neighbor in node.neighbors().neighbors() {
        let name = node
            .device_info(neighbor.node_id)
            .map(|info| String::from_utf8_lossy(info.device_name).into_owned())
            .unwrap_or_default();
        println!(
            "  {:016x}  port {}  {}  {name}",
            neighbor.node_id,
            neighbor.port,
            if neighbor.online { "online" } else { "offline" },
        );
    }

    println!("\ntransmitted:");
    for sent in &phy.sent {
        if let Ok(datagram) = udp::accept(&sent.frame) {
            let publication = pubsub::decode(datagram.payload).expect("only publications");
            println!(
                "  publication to {} to {:?}",
                String::from_utf8_lossy(publication.topic),
                sent.egress
            );
            continue;
        }
        let mut frame = sent.frame.clone();
        let received = rx::accept(&mut frame).expect("everything the node sends validates");
        println!("  {:?} to {:?}", received.header.message_type, sent.egress);
    }

    let peer = node
        .neighbors()
        .find(PEER_ID)
        .expect("the peer is a neighbour");
    assert!(peer.online);
    assert_eq!(
        node.device_info(PEER_ID).map(|info| info.device_name),
        Some(&b"peer"[..])
    );
    let (source, _, payload) = app.reply.expect("the ping was answered");
    assert_eq!(source, PEER_ID);
    assert_eq!(payload, PING_PAYLOAD);
    assert_eq!(
        app.received,
        [(
            PEER_ID,
            b"hello/peer".to_vec(),
            b"hello from the peer".to_vec()
        )]
    );
    assert!(
        phy.sent
            .iter()
            .any(|sent| sent.frame
                == frames::publication(NODE_ID, b"hello/rust", 1, 2, b"hello world")),
        "the publication was sent"
    );
    assert!(
        phy.sent.iter().any(|sent| sent.frame
            == frames::spotter_log(NODE_ID, 0, None, USE_TIMESTAMP, b"hello world")),
        "the spotter_log line was sent"
    );
}
