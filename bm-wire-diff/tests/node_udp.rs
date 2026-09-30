//! UDP through `bm_stack::Node`, compared against the oracle's whole stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire::pubsub;
use bm_wire::util::{BmIpAddr, bm_wildcard_match};
use bm_wire_diff::node_udp::{
    Arrival, NodeUdpInput, Publish, Published, Step, check, check_publish, check_receive,
    check_send, expected_publication, metrics_request_topic, oracle_delivers, publication,
};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::udp::{BOUND_PORTS, Dst, MIDDLEWARE_PORT, Send};

const PEER: u64 = 0x0b54_ccce_5c79_78bf;

fn send(src_port: u8, dst: Dst, dst_port: u16, payload: &[u8]) -> Send {
    Send {
        src_port,
        dst,
        dst_port,
        payload: payload.to_vec(),
    }
}

fn arrival(ingress: u8, dst: Dst, src_port: u16, dst_port: u16, payload: &[u8]) -> Arrival {
    Arrival {
        ingress,
        src: PEER,
        dst,
        src_port: Err(src_port),
        dst_port: Err(dst_port),
        payload: payload.to_vec(),
    }
}

/// An oracle publication to `FF03::1` reaches a Rust node's port 4321.
#[test]
fn an_oracle_publication_reaches_a_rust_node() {
    check_send(&send(
        0,
        Dst::Global,
        MIDDLEWARE_PORT,
        &publication(b"\x02u2hi"),
    ));
}

#[test]
fn oracle_datagrams_to_every_port() {
    for (index, port) in BOUND_PORTS.iter().enumerate() {
        for dst in [Dst::Global, Dst::LinkLocal] {
            check_send(&send(index as u8, dst, *port, b"payload"));
        }
    }
    check_send(&send(1, Dst::Global, 0x4000, b"unbound"));
}

/// A Rust node's publication reaches `bm_middleware_rx` and the oracle's
/// subscriber, on either port, and is relayed out the other.
#[test]
fn a_rust_publication_reaches_the_oracle_subscriber() {
    for ingress in 0..2 {
        check_receive(&arrival(
            ingress,
            Dst::Global,
            MIDDLEWARE_PORT,
            MIDDLEWARE_PORT,
            &publication(b"\x07spotterhello"),
        ));
    }
}

/// Divergence #75: a `topic_len` past the payload wraps the data length
/// `bm_handle_msg` hands the subscriber. `check_receive` asserts the oracle's
/// callback sees the wrapped length.
#[test]
fn a_topic_past_the_payload_wraps_the_data_length() {
    let payload = b"\0\0\x0a\x01\x02#";
    assert!(pubsub::decode(payload).is_err());
    assert_eq!(
        expected_publication(PEER, payload),
        Published::Wrapped(PEER, 10, 6u16.wrapping_sub(15), 1, 2)
    );
    for payload in [&payload[..], b"", b"\0\0\xff\x01\x02"] {
        check_receive(&arrival(
            0,
            Dst::Global,
            MIDDLEWARE_PORT,
            MIDDLEWARE_PORT,
            payload,
        ));
    }
}

/// `bm_pub_wl` against `pubsub::encode`: the shortest and longest topics, the
/// two it refuses, and data either side of `MAX_MESSAGE_LEN`.
#[test]
fn the_oracle_publishes_what_pubsub_encodes() {
    let publish = |topic_len: u8, data_len: usize| Publish {
        topic_len,
        kind: 1,
        version: pubsub::COMMON_VERSION,
        data: (0..data_len).map(|i| i as u8).collect(),
    };
    for topic_len in [0, 1, 14, 254, 255] {
        check_publish(&publish(topic_len, 4));
    }
    let fits = pubsub::MAX_MESSAGE_LEN - pubsub::HEADER_LEN - 14;
    check_publish(&publish(14, fits));
    check_publish(&publish(14, fits + 1));
    check_publish(&publish(14, 0));
}

/// Divergence #74, through the oracle's `bm_handle_msg`: a subscription with
/// no `*` receives every topic it prefixes.
#[test]
fn a_subscription_receives_topics_it_prefixes() {
    let service = metrics_request_topic();
    let pattern = &service[..service.len() - "/req".len()];
    let with = |suffix: &[u8]| [pattern, suffix].concat();
    for (topic, delivered) in [
        (with(b""), true),
        (with(b"/rep"), true),
        (with(b"X"), true),
        (pattern[..pattern.len() - 1].to_vec(), false),
    ] {
        assert_eq!(oracle_delivers(pattern, &topic), delivered, "{topic:?}");
        assert_eq!(bm_wildcard_match(&topic, pattern), delivered, "{topic:?}");
    }
}

/// Divergence #73: `middleware_net_task` looks the application up by the
/// source port, so a datagram to 4321 from any other port reaches no
/// subscriber. `check_receive` asserts the C delivers nothing.
#[test]
fn the_middleware_dispatches_on_the_source_port() {
    for src_port in [MIDDLEWARE_PORT - 1, MIDDLEWARE_PORT + 1, 0, 0x1234] {
        check_receive(&arrival(
            0,
            Dst::Global,
            src_port,
            MIDDLEWARE_PORT,
            &publication(b"\x02u2hi"),
        ));
    }
}

/// Link-local multicast is not relayed, `FF02::1` or otherwise.
#[test]
fn link_local_datagrams_are_not_relayed() {
    let mut other = BmIpAddr::LINK_LOCAL_MULTICAST.0;
    other[15] = 2;
    for dst in [Dst::LinkLocal, Dst::Other(other)] {
        check_receive(&arrival(1, dst, 1, 0x1234, b"ll"));
    }
}

/// Every port but pub/sub's is bound on both sides; 0x4000 on neither.
#[test]
fn bound_and_unbound_ports() {
    for dst_port in [0xFFFF, 0x4000] {
        check_receive(&arrival(0, Dst::Global, 7, dst_port, b"x"));
    }
}

#[test]
fn steps_interleave() {
    check(&NodeUdpInput {
        steps: vec![
            Step::Receive(arrival(0, Dst::Global, 4321, 4321, b"\x01ab")),
            Step::Send(send(2, Dst::LinkLocal, 1, b"second")),
            Step::Receive(arrival(1, Dst::Global, 9, 0, b"third")),
            Step::Publish(Publish {
                topic_len: 7,
                kind: 0,
                version: 0,
                data: b"fourth".to_vec(),
            }),
        ],
    });
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("node_udp");
    assert!(
        replayed > 0,
        "no node_udp seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} node_udp seeds");
}
