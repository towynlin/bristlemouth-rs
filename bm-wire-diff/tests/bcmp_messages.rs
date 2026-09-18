//! Device-info and neighbour-table messages, compared against bm_core by
//! asking its live stack for them.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives: bringing the
//! stack up installs `bm_linux.c`'s packet accessors, which would clobber the
//! ones `bm_wire_diff::bcmp` installs.

use bm_wire::bcmp::info::{DeviceInfo, DeviceInfoReply, DeviceInfoRequest};
use bm_wire::bcmp::neighbors::{
    NeighborInfo, NeighborTableReply, NeighborTableRequest, PortInfo, encode_neighbor_table_reply,
};
use bm_wire_diff::bcmp_messages::{BcmpMessagesInput, Request, Target, build_request, check};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::NUM_PORTS;

fn input(request: Request, target: Target, port: u8, global: bool) -> BcmpMessagesInput {
    BcmpMessagesInput {
        request,
        target,
        ingress_port: port,
        global_multicast: global,
        decode_probe: Vec::new(),
    }
}

#[test]
fn a_device_info_request_is_answered_from_the_device_config() {
    check(&input(Request::DeviceInfo, Target::All, 1, false));
}

#[test]
fn a_neighbor_table_request_is_answered_with_both_ports() {
    check(&input(Request::NeighborTable, Target::All, 1, false));
}

#[test]
fn a_request_for_this_node_is_answered_and_one_for_another_is_not() {
    for request in [Request::DeviceInfo, Request::NeighborTable] {
        check(&input(request, Target::ThisNode, 1, false));
        check(&input(request, Target::OtherNode, 1, false));
    }
}

#[test]
fn every_request_target_port_and_destination_combination() {
    for request in [Request::DeviceInfo, Request::NeighborTable] {
        for target in [Target::All, Target::ThisNode, Target::OtherNode] {
            for port in 1..=NUM_PORTS {
                for global in [false, true] {
                    check(&input(request, target, port, global));
                }
            }
        }
    }
}

/// A link-local request is answered once per port; a global-multicast one is
/// answered once, to every port at a time.
#[test]
fn the_reply_follows_the_request_destination() {
    check(&input(Request::DeviceInfo, Target::All, 1, true));
    check(&input(Request::DeviceInfo, Target::All, 2, true));
}

#[test]
fn the_injected_request_is_a_well_formed_bcmp_frame() {
    let frame = build_request(&input(Request::DeviceInfo, Target::All, 1, false));
    let mut copy = frame.clone();
    let received = bm_wire::bcmp::rx::accept(&mut copy).expect("our own request must validate");
    assert_eq!(
        received.header.message_type,
        bm_wire::bcmp::MessageType::DEVICE_INFO_REQUEST
    );
    assert_eq!(
        DeviceInfoRequest::decode(received.payload).unwrap(),
        DeviceInfoRequest { target_node_id: 0 }
    );
}

// ---------------------------------------------------------------------------
// The decoders on their own -- divergence #14 means there is no C to compare
// against for malformed input, so the property is the port's, not a diff.
// ---------------------------------------------------------------------------

#[test]
fn the_decoders_refuse_every_truncation_of_a_valid_message() {
    let reply = DeviceInfoReply {
        info: DeviceInfo {
            node_id: 0xDEAD_BEEF,
            ..DeviceInfo::default()
        },
        version_string: b"1.2.3",
        device_name: b"node",
    };
    let mut buf = vec![0u8; reply.encoded_len()];
    let len = reply.encode(&mut buf).unwrap();
    assert!(DeviceInfoReply::decode(&buf[..len]).is_ok());
    for shorter in 0..len {
        assert!(
            DeviceInfoReply::decode(&buf[..shorter]).is_err(),
            "a {shorter}-byte prefix of a {len}-byte reply must be refused"
        );
    }

    let ports = [PortInfo {
        state: 1,
        port_type: 0,
    }; 2];
    let neighbors = [NeighborInfo {
        node_id: 7,
        port: 1,
        online: 1,
    }; 3];
    let mut buf = vec![0u8; 128];
    let len = encode_neighbor_table_reply(&mut buf, 9, &ports, &neighbors).unwrap();
    assert!(NeighborTableReply::decode(&buf[..len]).is_ok());
    for shorter in 0..len {
        assert!(
            NeighborTableReply::decode(&buf[..shorter]).is_err(),
            "a {shorter}-byte prefix of a {len}-byte reply must be refused"
        );
    }
}

/// The shapes that would make bm_core read hundreds of kilobytes past the
/// frame. The port refuses them; nothing is handed to the C.
#[test]
fn saturated_length_fields_are_refused_rather_than_trusted() {
    let mut reply = vec![0u8; DeviceInfoReply::HEADER_LEN];
    reply[DeviceInfo::LEN] = u8::MAX;
    reply[DeviceInfo::LEN + 1] = u8::MAX;
    assert!(DeviceInfoReply::decode(&reply).is_err());

    let mut table = vec![0u8; NeighborTableReply::HEADER_LEN];
    table[8] = u8::MAX;
    table[9..11].copy_from_slice(&u16::MAX.to_le_bytes());
    assert!(NeighborTableReply::decode(&table).is_err());
}

#[test]
fn the_decoders_survive_arbitrary_bytes() {
    // A cheap deterministic sweep; the fuzz target does the thorough version.
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    for len in 0..300usize {
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) as u8
            })
            .collect();
        let mut probe = input(Request::DeviceInfo, Target::OtherNode, 1, false);
        probe.decode_probe = bytes;
        check(&probe);
    }
}

#[test]
fn requests_round_trip() {
    for target_node_id in [0u64, 1, u64::MAX, 0xC0FF_EE00_1234_5678] {
        let mut buf = [0u8; 8];
        DeviceInfoRequest { target_node_id }
            .encode(&mut buf)
            .unwrap();
        assert_eq!(
            DeviceInfoRequest::decode(&buf).unwrap().target_node_id,
            target_node_id
        );
        NeighborTableRequest { target_node_id }
            .encode(&mut buf)
            .unwrap();
        assert_eq!(
            NeighborTableRequest::decode(&buf).unwrap().target_node_id,
            target_node_id
        );
    }
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("bcmp_messages");
    assert!(
        replayed > 0,
        "no bcmp_messages seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} bcmp_messages seeds");
}
