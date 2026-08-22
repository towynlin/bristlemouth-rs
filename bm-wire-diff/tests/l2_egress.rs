//! The L2 egress path, compared against bm_core through the TX capture ring.
//!
//! Its own binary because `bm_wire_diff::l2_egress` brings bm_core's stack up,
//! which calls `packet_init` with `bm_linux.c`'s accessors and would clobber
//! the ones `bm_wire_diff::bcmp` installs. Cargo runs each integration test
//! file as its own process, which is what keeps the two apart. See the module
//! docs for the rest of the contract.

use bm_wire::bcmp::{self, BCMP_HEADER_LEN};
use bm_wire::frame::{
    IP_PROTO_UDP, IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_INGRESS_EGRESS_PORTS_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES, UDP_CHECKSUM_OFFSET,
};
use bm_wire::util::BmIpAddr;
use bm_wire_diff::l2_egress::{Destination, L2EgressInput, Upper, build_frame, check};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::NUM_PORTS;

fn input(
    upper: Upper,
    destination: Destination,
    requested_port: u8,
    body_len: usize,
) -> L2EgressInput {
    L2EgressInput {
        upper,
        destination,
        requested_port,
        source_node_id: 0x0000_0000_55AA_0011,
        udp_ports: (4321, 4321),
        body: (0..body_len).map(|i| (i as u8).wrapping_mul(29)).collect(),
    }
}

#[test]
fn link_local_bcmp_is_stamped_on_every_port() {
    check(&input(Upper::Bcmp, Destination::LinkLocalNeighbor, 0, 12));
}

#[test]
fn a_requested_port_narrows_the_transmission() {
    for port in 1..=NUM_PORTS {
        check(&input(
            Upper::Bcmp,
            Destination::LinkLocalNeighbor,
            port,
            12,
        ));
        check(&input(Upper::Udp, Destination::LinkLocalNeighbor, port, 12));
    }
}

#[test]
fn an_out_of_range_request_means_every_port() {
    for port in [0u8, NUM_PORTS + 1, 15, 255] {
        check(&input(Upper::Bcmp, Destination::LinkLocalNeighbor, port, 8));
    }
}

#[test]
fn global_multicast_is_not_stamped() {
    check(&input(Upper::Bcmp, Destination::GlobalMulticast, 0, 16));
    for port in 1..=NUM_PORTS {
        check(&input(Upper::Bcmp, Destination::GlobalMulticast, port, 16));
    }
}

#[test]
fn a_unicast_destination_is_dropped_unsent() {
    check(&input(Upper::Bcmp, Destination::Unicast, 0, 12));
    check(&input(Upper::Udp, Destination::Unicast, 1, 12));
}

#[test]
fn every_destination_and_protocol_combination() {
    for upper in [Upper::Bcmp, Upper::Udp] {
        for destination in [
            Destination::GlobalMulticast,
            Destination::LinkLocalNeighbor,
            Destination::LinkLocalOther,
            Destination::Unicast,
        ] {
            for port in 0..=NUM_PORTS + 1 {
                check(&input(upper, destination, port, 12));
            }
        }
    }
}

#[test]
fn bodies_of_every_length_up_to_sixty_four() {
    for len in 0..64usize {
        check(&input(Upper::Bcmp, Destination::LinkLocalNeighbor, 0, len));
        check(&input(Upper::Udp, Destination::LinkLocalOther, 0, len));
    }
}

#[test]
fn source_addresses_across_the_bit_range() {
    for bit in 0..64 {
        let mut i = input(Upper::Bcmp, Destination::LinkLocalNeighbor, 0, 12);
        i.source_node_id = 1u64 << bit;
        check(&i);
    }
}

// ---------------------------------------------------------------------------
// The checksum patch where it goes wrong -- divergence #12.
// ---------------------------------------------------------------------------

/// Find a source node id whose frame's checksum makes the patch carry.
///
/// The stored checksum is big-endian `~S`, so its first byte is `~(S >> 8)`.
/// Adding `port` to `S >> 8` carries out of that byte exactly when the stored
/// first byte is less than `port`. `also_low_zero` additionally demands the
/// second byte be zero, which means `S & 0xFF == 0xFF` -- the case where even
/// the sixteen-bit patch loses a carry.
fn find_carrying_source(
    upper: Upper,
    port: u8,
    also_low_zero: bool,
    checksum_offset: usize,
) -> L2EgressInput {
    for node_id in 0u64..8_000_000 {
        let mut candidate = input(upper, Destination::LinkLocalNeighbor, port, 12);
        candidate.source_node_id = node_id;
        let frame = build_frame(&candidate);
        if frame[checksum_offset] < port && (!also_low_zero || frame[checksum_offset + 1] == 0) {
            return candidate;
        }
    }
    panic!("no carrying source address found");
}

/// Stamp a frame the way L2 does, without transmitting it.
fn stamp(input: &L2EgressInput, port: u8) -> Vec<u8> {
    let mut frame = build_frame(input);
    bm_wire::l2::take_requested_egress_port(&mut frame, NUM_PORTS).unwrap();
    bm_wire::l2::add_egress_port(&mut frame, port).unwrap();
    frame
}

/// Would the node at the other end accept this BCMP frame as it left the port?
fn bcmp_frame_is_accepted(frame: &[u8]) -> bool {
    let mut arrived = frame.to_vec();
    // That node stamps its own ingress port on arrival; accept() clears it.
    arrived[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= 1 << 4;
    bcmp::rx::accept(&mut arrived).is_ok()
}

/// The checksum a receiver would compute for a UDP frame as stamped.
fn correct_udp_checksum(frame: &[u8]) -> u16 {
    let mut probe = frame.to_vec();
    probe[UDP_CHECKSUM_OFFSET] = 0;
    probe[UDP_CHECKSUM_OFFSET + 1] = 0;
    let mut src = [0u8; 16];
    src.copy_from_slice(&probe[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]);
    let mut dst = [0u8; 16];
    dst.copy_from_slice(
        &probe[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16],
    );
    bm_wire::checksum::ipv6_pseudo_checksum(
        &BmIpAddr(src),
        &BmIpAddr(dst),
        IP_PROTO_UDP,
        &probe[MIN_FRAME_WITH_ADDRESSES..],
    )
}

#[test]
fn the_port_matches_the_c_even_where_the_patch_carries() {
    for port in 1..=NUM_PORTS {
        check(&find_carrying_source(
            Upper::Udp,
            port,
            false,
            UDP_CHECKSUM_OFFSET,
        ));
        check(&find_carrying_source(
            Upper::Bcmp,
            port,
            false,
            MIN_FRAME_WITH_ADDRESSES + 2,
        ));
    }
}

/// The eight-bit UDP patch drops the end-around carry, so the frame that leaves
/// the port carries a checksum no receiver will accept.
#[test]
fn the_udp_patch_emits_an_invalid_checksum_when_it_carries() {
    let port = NUM_PORTS;
    let carrying = find_carrying_source(Upper::Udp, port, false, UDP_CHECKSUM_OFFSET);
    let stamped = stamp(&carrying, port);

    let emitted = u16::from_le_bytes([
        stamped[UDP_CHECKSUM_OFFSET],
        stamped[UDP_CHECKSUM_OFFSET + 1],
    ]);
    let correct = correct_udp_checksum(&stamped);
    assert_ne!(
        emitted, correct,
        "expected the eight-bit patch to be wrong here; if this starts passing, \
         divergence #12 has been fixed upstream and bm-wire must follow"
    );

    // And an ordinary frame, where nothing carries, is patched correctly -- so
    // the assertion above is about the carry and not about everything.
    let ordinary = stamp(
        &input(Upper::Udp, Destination::LinkLocalNeighbor, port, 12),
        port,
    );
    let emitted = u16::from_le_bytes([
        ordinary[UDP_CHECKSUM_OFFSET],
        ordinary[UDP_CHECKSUM_OFFSET + 1],
    ]);
    assert_eq!(emitted, correct_udp_checksum(&ordinary));
}

/// The sixteen-bit BCMP patch is right except when the carry itself carries,
/// which is rarer -- but BCMP is the protocol that actually takes this path.
#[test]
fn the_bcmp_patch_emits_an_invalid_checksum_on_a_double_carry() {
    let checksum_offset = MIN_FRAME_WITH_ADDRESSES + 2;
    let port = NUM_PORTS;
    let carrying = find_carrying_source(Upper::Bcmp, port, true, checksum_offset);

    // It agrees with the C. That is the requirement; the rest is the finding.
    check(&carrying);

    assert!(
        !bcmp_frame_is_accepted(&stamp(&carrying, port)),
        "expected the double carry to produce a frame a receiver rejects; if this \
         starts passing, divergence #12 has been fixed upstream and bm-wire must follow"
    );

    let ordinary = stamp(
        &input(Upper::Bcmp, Destination::LinkLocalNeighbor, port, 12),
        port,
    );
    assert!(
        bcmp_frame_is_accepted(&ordinary),
        "an ordinary stamped frame must still be accepted"
    );
    assert_eq!(
        ordinary.len(),
        MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN + 12
    );
}

/// Only this binary's own target: each stack target has its own test binary,
/// because each needs its own process. `replay::STACK_TARGETS` is checked
/// against the files under `tests/` so none can go without one.
#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("l2_egress");
    assert!(
        replayed > 0,
        "no l2_egress seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} l2_egress seeds");
}
