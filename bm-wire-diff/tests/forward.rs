//! Forwarding between ports, compared against bm_core's live stack.
//!
//! Its own binary because `bm_wire_diff::forward` brings bm_core's stack up; see
//! `bm_wire_diff::stack` for the contract that forces it.
//!
//! The headline is [`a_frame_on_port_one_leaves_port_two_byte_for_byte`]: that
//! is card I2's "done when", and it is the reason a three-node chain can relay
//! at all.

use bm_wire::bcmp::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, MessageType, forward, rx};
use bm_wire::frame::{
    ETHERNET_DESTINATION_OFFSET, IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_INGRESS_EGRESS_PORTS_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET,
};
use bm_wire::util::BmIpAddr;
use bm_wire_diff::forward::{
    Destination, ForwardInput, Message, build_frame, check, check_ll_forward, check_relay,
};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::{self, NUM_PORTS, capture, drain, inject, node, oracle};

fn input(
    message: Message,
    destination: Destination,
    ingress_port: u8,
    body_len: usize,
) -> ForwardInput {
    ForwardInput {
        message,
        destination,
        ingress_port,
        sender_egress_nibble: ingress_port,
        legacy_ports: [0, 0],
        body: (0..body_len).map(|i| (i as u8).wrapping_mul(31)).collect(),
    }
}

/// Card I2's acceptance test, spelled out rather than only implied by `check`.
///
/// A global-multicast frame injected on port 1 of bm_core's stack and of a
/// `bm-stack` node has to produce the same bytes on port 2, or a chain of nodes
/// relays different traffic depending on whose firmware is in the middle.
#[test]
fn a_frame_on_port_one_leaves_port_two_byte_for_byte() {
    let mut input = input(Message::Unregistered, Destination::GlobalMulticast, 1, 24);
    // A real FF03::1 frame carries no egress nibble: L2 stamps only link-local
    // multicast, so byte 24 is zero on the wire and the checksum was computed
    // that way. `prepare_forwarded_copy` clears the byte, which is therefore a
    // no-op here -- and is what makes the relayed frame still verifiable.
    input.sender_egress_nibble = 0;
    let frame = build_frame(&input);

    let _guard = oracle();
    drain();
    inject(1, &frame);
    let c = drain();
    assert_eq!(c.len(), 1, "one copy, on the one other port");
    assert_eq!(c[0].0, 2, "port 2");

    let mut node = node();
    let mut ours = frame.clone();
    let owed = node.on_frame(0, 1, &mut ours);
    assert!(owed.reply.is_none(), "nothing registers this message type");
    let rs = capture(owed.relay.expect("a global multicast is flooded onward"));

    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].0, 2);
    assert_eq!(
        c[0].1, rs[0].1,
        "the relayed frame must be byte-identical\n  C:        {:02x?}\n  bm-stack: {:02x?}",
        c[0].1, rs[0].1
    );

    // And what came out is still the message that went in, with both port
    // nibbles cleared -- which is what lets the next hop stamp its own.
    let mut arrived = rs[0].1.clone();
    assert_eq!(arrived[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 0);
    let received = rx::accept(&mut arrived).expect("a peer accepts the relayed frame");
    assert_eq!(received.header.message_type, MessageType(0xFFFE));
}

#[test]
fn every_destination_and_message_on_every_port() {
    for message in [
        Message::Unregistered,
        Message::DeviceInfoForUs,
        Message::DeviceInfoForAnother,
        Message::NeighborTableForUs,
        Message::SystemTime,
    ] {
        for destination in [
            Destination::GlobalMulticast,
            Destination::LinkLocalNeighbor,
            Destination::LinkLocalOther,
        ] {
            for port in 1..=NUM_PORTS {
                check(&input(message, destination, port, 16));
            }
        }
    }
}

#[test]
fn bodies_of_every_length_up_to_sixty_four() {
    for len in 0..64usize {
        check_relay(&input(
            Message::Unregistered,
            Destination::GlobalMulticast,
            1,
            len,
        ));
        check_ll_forward(&input(
            Message::Unregistered,
            Destination::LinkLocalNeighbor,
            1,
            len,
        ));
    }
}

/// The largest body the comparator builds, which is also the case most likely
/// to trip a length or buffer bound on either side.
#[test]
fn a_maximum_length_body_forwards() {
    check(&input(
        Message::Unregistered,
        Destination::GlobalMulticast,
        1,
        bm_wire_diff::forward::MAX_BODY,
    ));
}

/// A previous hop's egress nibble is part of the checksum, and is cleared out of
/// the forwarded copy. Every value of it has to agree.
#[test]
fn any_egress_nibble_the_previous_hop_stamped_still_agrees() {
    for nibble in 0..=0x0Fu8 {
        let mut i = input(Message::Unregistered, Destination::GlobalMulticast, 1, 12);
        i.sender_egress_nibble = nibble;
        check(&i);
    }
}

/// The interaction worth pinning: a frame carrying the legacy port bytes fails
/// its checksum locally (divergence #9) and is relayed anyway, with those bytes
/// intact — because bm_core copies the frame for forwarding before
/// `process_received_message` clears them.
#[test]
fn a_frame_that_fails_its_checksum_is_still_relayed_with_its_legacy_bytes() {
    let mut i = input(
        Message::DeviceInfoForUs,
        Destination::GlobalMulticast,
        1,
        12,
    );
    i.legacy_ports = [0xAB, 0xCD];
    check(&i);

    // And what that looks like: one relayed frame, no reply, legacy bytes on.
    let frame = build_frame(&i);
    let _guard = oracle();
    drain();
    inject(1, &frame);
    let c = drain();
    assert_eq!(c.len(), 1, "relayed but not answered");
    assert_eq!(c[0].1[IPV6_SOURCE_ADDRESS_OFFSET + 4], 0xAB);
    assert_eq!(c[0].1[IPV6_SOURCE_ADDRESS_OFFSET + 5], 0xCD);

    // The same frame without them is answered, so the assertion above is about
    // the legacy bytes and not about the request.
    let mut clean = i.clone();
    clean.legacy_ports = [0, 0];
    drain();
    inject(1, &build_frame(&clean));
    assert_eq!(drain().len(), 2, "relayed and answered");
}

// ---------------------------------------------------------------------------
// bcmp_ll_forward's own quirks -- divergences #23 and #24.
// ---------------------------------------------------------------------------

/// The forwarded copy claims the forwarder as its IPv6 source, not the node that
/// sent the message: divergence #23. Asserted against the C, so if upstream
/// changes it the comparator fails first and this says why.
#[test]
fn the_c_puts_its_own_address_on_a_forwarded_message() {
    let input = input(Message::SystemTime, Destination::LinkLocalNeighbor, 1, 16);
    check_ll_forward(&input);

    let frame = build_frame(&input);
    let bcmp = frame[BCMP_HEADER_OFFSET..].to_vec();
    let mut node = node();
    let forwarded = capture(
        node.forward_link_local(2, &bcmp)
            .expect("a forward fits the buffer"),
    );
    assert_eq!(forwarded.len(), 1);

    let mut copy = forwarded[0].1.clone();
    let received = rx::accept(&mut copy).expect("a peer accepts the forward");
    assert_eq!(
        received.src.to_node_id(),
        stack::NODE_ID,
        "the forwarder, not the originator"
    );
    assert_ne!(
        received.src.to_node_id(),
        bm_wire_diff::forward::PEER_NODE_ID
    );
    // The originator survives only inside the body, which is untouched.
    assert_eq!(
        &copy[BCMP_HEADER_OFFSET + BCMP_HEADER_LEN..],
        &bcmp[BCMP_HEADER_LEN..]
    );
}

/// The egress port reaches the wire inside the Ethernet destination MAC:
/// divergence #24. The IPv6 destination is clean again by then, so the MAC and
/// the address disagree.
#[test]
fn the_egress_port_survives_in_the_multicast_mac() {
    let input = input(Message::SystemTime, Destination::LinkLocalNeighbor, 1, 8);
    check_ll_forward(&input);

    let frame = build_frame(&input);
    let bcmp = frame[BCMP_HEADER_OFFSET..].to_vec();
    let mut node = node();
    let forwarded = capture(node.forward_link_local(2, &bcmp).expect("fits"));
    let sent = &forwarded[0].1;

    assert_eq!(
        &sent[ETHERNET_DESTINATION_OFFSET..ETHERNET_DESTINATION_OFFSET + 6],
        &[0x33, 0x33, 0x00, 0x02, 0x00, 0x01],
        "byte 3 of the MAC is the egress port, where a conforming node expects 0"
    );
    assert_eq!(
        &sent[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16],
        &BmIpAddr::LINK_LOCAL_MULTICAST.0,
        "while the address the MAC was derived from is FF02::1 again"
    );
    // The C agrees, which is the only reason bm-wire keeps doing it.
    assert_eq!(
        forward::port_specific_destination(2).0[13],
        2,
        "and the port is where bm_l2_link_output reads it"
    );
}

/// Only this binary's own target, because each stack target needs its own
/// process. `replay::STACK_TARGETS` is checked against the files under `tests/`
/// so none can go without one.
#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("forward");
    assert!(
        replayed > 0,
        "no forward seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} forward seeds");
}
