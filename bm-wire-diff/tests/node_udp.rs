//! UDP through `bm_stack::Node`, compared against the oracle's whole stack.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire::util::BmIpAddr;
use bm_wire_diff::node_udp::{
    Arrival, NodeUdpInput, Step, check, check_receive, check_send, publication,
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
            b"\x06spotterhello",
        ));
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

#[test]
fn ports_bound_on_one_side_only() {
    // 0xFFFF is bound in the oracle only; 0x4000 on neither.
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
