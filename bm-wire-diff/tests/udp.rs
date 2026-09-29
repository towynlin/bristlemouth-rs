//! UDP send and receive, compared against `network/bm_linux.c`.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives.

use bm_wire::addr::{self, LINK_LOCAL_PREFIX};
use bm_wire::checksum::ipv6_pseudo_checksum;
use bm_wire::frame::IP_PROTO_UDP;
use bm_wire::util::BmIpAddr;
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};
use bm_wire_diff::stack::{self, NODE_ID};
use bm_wire_diff::udp::{
    BOUND_PORTS, Dst, MIDDLEWARE_PORT, Receive, Send, Step, UdpInput, check, check_receive,
    check_send,
};

fn send(src_port: u8, dst: Dst, dst_port: u16, payload: &[u8]) -> Send {
    Send {
        src_port,
        dst,
        dst_port,
        payload: payload.to_vec(),
    }
}

fn receive(dst_port: Result<u8, u16>, payload: &[u8]) -> Receive {
    Receive {
        src: addr::nodeid_to_ip(0xFD00_0000, 0x0b54_ccce_5c79_78bf).0,
        src_port: MIDDLEWARE_PORT,
        dst_port,
        payload: payload.to_vec(),
        ethertype: None,
        next_header: None,
        ipv6_len: None,
        udp_len: None,
        trailing: Vec::new(),
        cut: 0,
    }
}

/// `mac_address` reads the node id `device_init` set, so it is compared here
/// rather than in the pure `addr` target.
#[test]
fn mac_address_agrees_with_the_c() {
    let _guard = stack::oracle();
    let mut c_mac = [0u8; 6];
    let err = unsafe { bm_wire_sys::mac_address(c_mac.as_mut_ptr(), 6) };
    assert_eq!(err, bm_wire_sys::BmErr_BmOK);
    assert_eq!(addr::mac_address(NODE_ID), c_mac);
}

/// Pub/sub's send: `ff03::1`, one frame to every port.
#[test]
fn a_publication_goes_out_once_to_every_port() {
    check_send(&send(0, Dst::Global, MIDDLEWARE_PORT, b"hello world"));
}

/// Stamped per port, with the UDP branch of the egress checksum patch.
#[test]
fn link_local_multicast_is_stamped_per_port() {
    for len in 0..64u8 {
        let payload: Vec<u8> = (0..len).map(|i| i.wrapping_mul(37)).collect();
        check_send(&send(len, Dst::LinkLocal, 0x1234, &payload));
    }
}

/// Byte 13 of the destination narrows the ports; a unicast destination is not
/// sent.
#[test]
fn other_destinations() {
    let mut narrowed = BmIpAddr::GLOBAL_MULTICAST.0;
    narrowed[13] = 2;
    let unicast = addr::nodeid_to_ip(0xFD00_0000, 7).0;
    for dst in [narrowed, unicast] {
        check_send(&send(3, Dst::Other(dst), 9, b"x"));
    }
}

/// A payload word that brings the sum to `0xFFFF` makes the checksum zero:
/// `bm_linux.c` sends 0, `bm_wire::udp` sends `0xFFFF` as lwIP does
/// (divergence #71). `check_send` asserts that is the only difference.
#[test]
fn a_checksum_of_zero_is_the_one_difference() {
    let src = addr::nodeid_to_ip(LINK_LOCAL_PREFIX, NODE_ID);
    let dst = BmIpAddr::GLOBAL_MULTICAST;
    let mut segment = [0u8; 10];
    segment[..2].copy_from_slice(&MIDDLEWARE_PORT.to_be_bytes());
    segment[2..4].copy_from_slice(&MIDDLEWARE_PORT.to_be_bytes());
    segment[4..6].copy_from_slice(&10u16.to_be_bytes());
    let word = ipv6_pseudo_checksum(&src, &dst, IP_PROTO_UDP, &segment).to_le_bytes();
    segment[8..].copy_from_slice(&word);
    assert_eq!(ipv6_pseudo_checksum(&src, &dst, IP_PROTO_UDP, &segment), 0);

    check_send(&send(0, Dst::Global, MIDDLEWARE_PORT, &word));
    check_send(&send(0, Dst::LinkLocal, MIDDLEWARE_PORT, &word));
}

#[test]
fn every_bound_port_receives() {
    for index in 1..BOUND_PORTS.len() as u8 {
        check_receive(&receive(Ok(index), b"payload"));
    }
    check_receive(&receive(Ok(2), b""));
}

#[test]
fn an_unbound_port_receives_nothing() {
    check_receive(&receive(Err(0x4000), b"payload"));
    check_receive(&receive(Err(MIDDLEWARE_PORT), b"payload"));
}

/// bm_linux.c delivers the UDP length field's worth and refuses a length under
/// 8 or past the IPv6 payload; lwIP delivers the IPv6 payload and ignores the
/// field (divergence #72). `check_receive` asserts both.
#[test]
fn the_udp_length_field() {
    for udp_len in [0u16, 7, 8, 9, 12, 15, 16, 0xFFFF] {
        let mut r = receive(Ok(3), b"12345678");
        r.udp_len = Some(udp_len);
        check_receive(&r);
    }
}

#[test]
fn malformed_frames_are_refused_by_both() {
    let base = receive(Ok(1), b"abcdef");
    let mut cases = Vec::new();
    for ethertype in [0x0800, 0x86DC] {
        cases.push(Receive {
            ethertype: Some(ethertype),
            ..base.clone()
        });
    }
    for next_header in [0u8, 6, 0xBC, 0xFF] {
        cases.push(Receive {
            next_header: Some(next_header),
            ..base.clone()
        });
    }
    for ipv6_len in [0u16, 7, 8, 13, 14, 15, 0xFFFF] {
        cases.push(Receive {
            ipv6_len: Some(ipv6_len),
            ..base.clone()
        });
    }
    for cut in [1u16, 6, 7, 8, 9, 20] {
        cases.push(Receive {
            cut,
            ..base.clone()
        });
    }
    cases.push(Receive {
        trailing: vec![0xEE; 5],
        ..base.clone()
    });
    for case in &cases {
        check_receive(case);
    }
}

#[test]
fn steps_interleave() {
    check(&UdpInput {
        steps: vec![
            Step::Receive(receive(Ok(4), b"first")),
            Step::Send(send(1, Dst::Global, 1, b"second")),
            Step::Receive(receive(Ok(3), b"third")),
        ],
    });
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("udp");
    assert!(
        replayed > 0,
        "no udp seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} udp seeds");
}
