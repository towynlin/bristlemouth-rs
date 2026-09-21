//! Echo / ping, compared against bm_core's live stack in both directions.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives: bringing the
//! stack up installs `bm_linux.c`'s packet accessors, which would clobber the
//! ones `bm_wire_diff::bcmp` installs.
//!
//! It is also its own binary for a second reason, particular to ping:
//! `ping.c`'s `BCMP_SEQ` is a file-scope `static` with nothing that resets it,
//! so the number the *nth* ping carries is a property of the process. Every
//! test here shares one oracle and one `bm-stack` node, and the two counters
//! advance together — which is exactly what the comparison is for. A test that
//! asserted a specific sequence number would have to say where the counter had
//! got to; none does, because `check_request` compares the C's number against
//! ours rather than against a constant.

use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::{MessageType, rx};
use bm_wire_diff::ping::{
    MAX_PING_PAYLOAD, PEER_NODE_ID, PingInput, Target, build_request, check, check_reply,
    check_request,
};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::NUM_PORTS;

fn input(target: Target, port: u8, global: bool, payload: &[u8]) -> PingInput {
    PingInput {
        target,
        ingress_port: port,
        global_multicast: global,
        request_id: 0xBEEF,
        request_seq: 7,
        null_payload: false,
        payload: payload.to_vec(),
        decode_probe: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The request half: bcmp_send_ping_request against Node::ping.
// ---------------------------------------------------------------------------

#[test]
fn our_echo_request_is_byte_identical_to_the_c() {
    check_request(&input(Target::All, 1, false, b"ping"));
}

/// The id a request carries is the sender's node id truncated to sixteen bits
/// and the `seq_num` is `ping.c`'s own counter truncated the same way, neither
/// of which is the BCMP header's sequence number. Comparing whole frames pins
/// all three at once; this reads them back so a divergence says which.
#[test]
fn the_request_carries_a_truncated_node_id_and_a_sequence_space_of_its_own() {
    let i = input(Target::ThisNode, 1, false, b"abc");
    check_request(&i);

    let mut frame = build_request(&i);
    let received = rx::accept(&mut frame).expect("our own request validates");
    assert_eq!(
        received.header.seq_num, 0,
        "ping is registered unsequenced, so the header number is always zero"
    );

    // And in the body, which the comparator has already matched against the C.
    let request = EchoRequest::decode(received.payload).unwrap();
    assert_eq!(request.id, 0xBEEF, "the injected request's own id");
}

#[test]
fn a_ping_to_every_node_and_a_ping_to_one_node() {
    for target in [Target::All, Target::ThisNode, Target::OtherNode] {
        check_request(&input(target, 1, false, b"payload"));
    }
}

#[test]
fn a_ping_with_no_payload_at_all() {
    check_request(&input(Target::All, 1, false, b""));
}

/// `bcmp_send_ping_request` starts with `if (payload == NULL) payload_len = 0`,
/// so a caller that passes a length and no buffer gets a payload-free ping
/// rather than a crash.
#[test]
fn a_null_payload_pointer_makes_the_declared_length_zero() {
    let mut i = input(Target::All, 1, false, b"these bytes are never read");
    i.null_payload = true;
    check_request(&i);
}

#[test]
fn payloads_of_every_length_up_to_forty() {
    for len in 0..40usize {
        let payload: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37)).collect();
        check_request(&input(Target::All, 1, false, &payload));
    }
}

#[test]
fn the_longest_payload_the_domain_allows() {
    check_request(&input(Target::All, 1, false, &[0xC3; MAX_PING_PAYLOAD][..]));
}

/// A ping to `FF03::1` leaves once, on every port at a time; a ping to
/// `FF02::1` leaves once per port with that port stamped in.
#[test]
fn a_global_multicast_ping_goes_out_unstamped() {
    check_request(&input(Target::All, 1, true, b"global"));
}

// ---------------------------------------------------------------------------
// The reply half: bcmp_process_ping_request against Node::on_frame.
// ---------------------------------------------------------------------------

#[test]
fn our_echo_reply_is_byte_identical_to_the_c() {
    check_reply(&input(Target::All, 1, false, b"ping"));
}

#[test]
fn a_request_for_this_node_is_answered_and_one_for_another_is_not() {
    check_reply(&input(Target::ThisNode, 1, false, b"ping"));
    check_reply(&input(Target::OtherNode, 1, false, b"ping"));
}

#[test]
fn every_target_port_and_destination_combination() {
    for target in [Target::All, Target::ThisNode, Target::OtherNode] {
        for port in 1..=NUM_PORTS {
            for global in [false, true] {
                check_reply(&input(target, port, global, b"abcdefgh"));
            }
        }
    }
}

/// The reply is the request's own buffer with `target_node_id` overwritten, so
/// `id`, `seq_num` and every payload byte survive untouched. The comparator has
/// already matched the frame against the C's; this says what is in it.
#[test]
fn the_reply_echoes_the_request_unchanged_but_for_the_node_id() {
    let mut i = input(Target::All, 1, false, b"echo me");
    i.request_id = 0x1234;
    i.request_seq = 0xFEDC;
    check_reply(&i);

    // Rebuild what the node answered with, from the request it answered.
    let mut frame = build_request(&i);
    let received = rx::accept(&mut frame).expect("our own request validates");
    let request = EchoRequest::decode(received.payload).unwrap();
    let reply = request.into_reply(bm_wire_diff::stack::NODE_ID);
    assert_eq!(reply.node_id, bm_wire_diff::stack::NODE_ID);
    assert_eq!(reply.id, 0x1234);
    assert_eq!(reply.seq_num, 0xFEDC);
    assert_eq!(reply.payload, b"echo me");
}

#[test]
fn a_request_with_no_payload_is_answered_with_no_payload() {
    check_reply(&input(Target::All, 2, false, b""));
}

#[test]
fn reply_payloads_of_every_length_up_to_forty() {
    for len in 0..40usize {
        let payload: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_add(11)).collect();
        check_reply(&input(Target::ThisNode, 2, false, &payload));
    }
}

#[test]
fn the_longest_reply_the_domain_allows() {
    check_reply(&input(Target::All, 1, false, &[0x5A; MAX_PING_PAYLOAD][..]));
}

#[test]
fn the_injected_request_is_a_well_formed_bcmp_frame() {
    let mut frame = build_request(&input(Target::All, 1, false, b"xyz"));
    let received = rx::accept(&mut frame).expect("our own request must validate");
    assert_eq!(received.header.message_type, MessageType::ECHO_REQUEST);
    assert_eq!(received.src.to_node_id(), PEER_NODE_ID);
    let request = EchoRequest::decode(received.payload).unwrap();
    assert_eq!(request.target_node_id, 0);
    assert_eq!(request.payload, b"xyz");
}

// ---------------------------------------------------------------------------
// Both halves plus the decoders, which is what a fuzz iteration runs.
// ---------------------------------------------------------------------------

#[test]
fn the_decoders_survive_arbitrary_bytes() {
    // A cheap deterministic sweep; the fuzz target does the thorough version.
    let mut state = 0x0F1E_2D3C_4B5A_6978u64;
    for len in 0..300usize {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as u8
            })
            .collect();
        let mut probe = input(Target::OtherNode, 1, false, b"");
        probe.decode_probe = bytes;
        check(&probe);
    }
}

/// A saturated `payload_len` on a header-only body is the shape that would have
/// `bcmp_process_ping_request` echo 64 KiB of whatever follows the frame —
/// divergence #27. The port refuses it; nothing is handed to the C.
#[test]
fn a_saturated_payload_length_is_refused_rather_than_trusted() {
    let mut body = vec![0u8; EchoRequest::HEADER_LEN];
    body[12..14].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(EchoRequest::decode(&body).is_err());
    assert!(EchoReply::decode(&body).is_err());
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("ping");
    assert!(
        replayed > 0,
        "no ping seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} ping seeds");
}

/// Divergence #12's live case, reached here for the first time by a real
/// message rather than by a synthetic one.
///
/// `network_add_egress_port` patches a link-local frame's checksum instead of
/// recomputing it when it stamps the egress port, and loses the end-around
/// carry when the carry itself carries — 120 of 983 040 cases. A ping's payload
/// is the sender's to choose, so ping is the first exchange whose frames range
/// widely enough to land on one; the fuzzer found this payload within a couple
/// of minutes, and `seeds/ping/reply-checksum-double-carry` keeps it.
///
/// What is asserted is that both nodes produce the same wrong frame, and that
/// it *is* wrong: a node receiving it drops it. **If this test starts failing
/// on the last assertion, bm_core has been fixed and the port must follow.**
#[test]
fn a_reply_whose_stamped_checksum_double_carries_is_wrong_on_both_sides() {
    let mut i = input(Target::All, 1, false, &[0xD4, 0x3D]);
    i.request_id = 0x2F8C;
    i.request_seq = 25_061;

    // The frames agree, which is the comparison that matters.
    check_reply(&i);

    // And the frame they agree on is one no node will accept.
    let mut node = bm_wire_diff::stack::node();
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    let mut frame = build_request(&i);
    let mut ours = node
        .on_frame(0, 1, &mut frame)
        .reply
        .expect("answered")
        .frame()
        .to_vec();
    let stamped = bm_wire::l2::stamp_egress_port(&mut ours, 1).expect("stampable");
    let mut copy = stamped.to_vec();
    drop(stamped);
    assert!(
        rx::accept(&mut copy).is_err(),
        "the dropped carry is what makes this frame invalid; if it validates, \
         bm_core has fixed divergence #12 and l2::add_egress_port must follow"
    );
}
