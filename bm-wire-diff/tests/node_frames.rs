//! Are the frames `bm-stack` builds the frames bm_core builds?
//!
//! Everything else in this harness compares a function against a function.
//! This compares the *node* against the node: the oracle is asked to emit a
//! message, `bm-stack` is asked to emit the same one from the same identity,
//! and the two are compared byte for byte — Ethernet header, IPv6 header, BCMP
//! header, checksum and body.
//!
//! If these agree, a node running this firmware is indistinguishable on the
//! wire from one running the C, which is the whole point of the exercise.
//!
//! Its own binary because it brings the stack up; see `bm_wire_diff::stack`.

use bm_stack::node::LINK_LOCAL_PREFIX;
use bm_stack::port::RtcTimeAndDate;
use bm_stack::{Identity, Node, SoftRtc};
use bm_wire::bcmp::info::DeviceInfoRequest;
use bm_wire::bcmp::registry::PacketCfg;
use bm_wire::bcmp::{DeviceInfo, MessageType, rx};
use bm_wire::frame::IPV6_INGRESS_EGRESS_PORTS_OFFSET;
use bm_wire::l2;
use bm_wire::util::BmIpAddr;
use bm_wire_diff::bcmp_messages::{BcmpMessagesInput, Request, Target, build_request};
use bm_wire_diff::stack::{self, NUM_PORTS, drain, inject, oracle, pump_until_quiet};
use bm_wire_diff::time::{self, TimeInput, TimeMessage};

/// The same identity the oracle's stack was brought up with, so the two nodes
/// have nothing to differ about but their code.
struct OracleIdentity;

impl Identity for OracleIdentity {
    fn node_id(&self) -> u64 {
        stack::NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor_id: stack::VENDOR_ID,
            product_id: stack::PRODUCT_ID,
            serial_num: stack::SERIAL_NUMBER,
            git_sha: stack::GIT_SHA,
            ver_major: stack::FIRMWARE_VERSION.0,
            ver_minor: stack::FIRMWARE_VERSION.1,
            ver_rev: stack::FIRMWARE_VERSION.2,
            ver_hw: stack::HW_VERSION,
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        stack::VERSION_STRING
    }

    fn device_name(&self) -> &[u8] {
        stack::DEVICE_NAME
    }
}

/// A node with the same link state the oracle has: `stack::oracle` brings both
/// ports up before any comparison, and a neighbour-table reply carries that.
fn node() -> Node<OracleIdentity, SoftRtc, 4> {
    let mut node = Node::new(OracleIdentity, SoftRtc::new(), NUM_PORTS);
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    node
}

fn assert_same_frame(what: &str, port: u8, c: &[u8], rs: &[u8]) {
    if c == rs {
        return;
    }
    let at = c
        .iter()
        .zip(rs)
        .position(|(a, b)| a != b)
        .unwrap_or(c.len().min(rs.len()));
    panic!(
        "{what} on port {port} diverged at byte {at}: C {:#04x?}, bm-stack {:#04x?}\n  C:        {c:02x?}\n  bm-stack: {rs:02x?}",
        c.get(at),
        rs.get(at),
    );
}

/// Stamp our frame the way L2 would for `port`, then compare.
fn compare_stamped(what: &str, captured: &[(u8, Vec<u8>)], mut ours: Vec<u8>) {
    assert_eq!(
        captured.len(),
        usize::from(NUM_PORTS),
        "{what}: expected one copy per port, got {}",
        captured.len()
    );
    for (port, c_frame) in captured {
        let stamped = l2::stamp_egress_port(&mut ours, *port).expect("stampable");
        assert_same_frame(what, *port, c_frame, &stamped);
    }
}

#[test]
fn our_heartbeat_is_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    let uptime_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
    unsafe {
        assert_eq!(
            bm_wire_sys::bcmp_send_heartbeat(bm_wire::neighbor::HEARTBEAT_PERIOD_S),
            bm_wire_sys::BmErr_BmOK
        );
    }
    pump_until_quiet();
    let captured = drain();

    let mut node = node();
    let ours = node
        .on_tick(uptime_ms)
        .expect("a tick emits a heartbeat")
        .frame()
        .to_vec();

    compare_stamped("heartbeat", &captured, ours);
}

#[test]
fn our_device_info_reply_is_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    let request = build_request(&BcmpMessagesInput {
        request: Request::DeviceInfo,
        target: Target::All,
        ingress_port: 1,
        global_multicast: false,
        decode_probe: Vec::new(),
    });
    inject(1, &request);
    let captured = drain();
    assert!(!captured.is_empty(), "the oracle answered nothing");

    // Ask our node the same question, with the frame the oracle was given.
    let mut node = node();
    let mut frame = request.clone();
    let ours = node
        .on_frame(0, 1, &mut frame)
        .reply
        .expect("a device-info request must be answered")
        .frame()
        .to_vec();

    compare_stamped("device info reply", &captured, ours);
}

#[test]
fn our_neighbor_table_reply_is_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    let request = build_request(&BcmpMessagesInput {
        request: Request::NeighborTable,
        target: Target::ThisNode,
        ingress_port: 2,
        global_multicast: false,
        decode_probe: Vec::new(),
    });
    inject(2, &request);
    let captured = drain();
    assert!(!captured.is_empty(), "the oracle answered nothing");

    let mut node = node();
    let mut frame = request.clone();
    let ours = node
        .on_frame(0, 2, &mut frame)
        .reply
        .expect("a neighbour-table request must be answered")
        .frame()
        .to_vec();

    compare_stamped("neighbour table reply", &captured, ours);
}

/// A reply to a global-multicast request goes out unstamped, once.
#[test]
fn our_global_multicast_reply_is_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    let request = build_request(&BcmpMessagesInput {
        request: Request::DeviceInfo,
        target: Target::All,
        ingress_port: 1,
        global_multicast: true,
        decode_probe: Vec::new(),
    });
    inject(1, &request);
    let captured = drain();

    // The oracle also forwards the global-multicast request out its other
    // port; the reply is the one addressed to us.
    let replies: Vec<&(u8, Vec<u8>)> = captured
        .iter()
        .filter(|(_, frame)| {
            let mut copy = frame.clone();
            rx::accept(&mut copy)
                .map(|r| r.header.message_type == MessageType::DEVICE_INFO_REPLY)
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(replies.len(), 1, "one reply, to every port at once");

    let mut node = node();
    let mut frame = request.clone();
    let ours = node
        .on_frame(0, 1, &mut frame)
        .reply
        .expect("answered")
        .frame()
        .to_vec();

    assert_eq!(
        ours[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 0,
        "a global multicast reply is never stamped"
    );
    assert_same_frame(
        "global device info reply",
        replies[0].0,
        &replies[0].1,
        &ours,
    );
}

// ---------------------------------------------------------------------------
// System time -- card M2.
// ---------------------------------------------------------------------------

/// A reading a human can check: 2026-09-21T12:34:56.789Z.
const NOON_ISH: RtcTimeAndDate = RtcTimeAndDate {
    year: 2026,
    month: 9,
    day: 21,
    hour: 12,
    minute: 34,
    second: 56,
    ms: 789,
};

/// Card M2's "done when": a node with a clock answers a `0x10` with the frame
/// bm_core would have sent, byte for byte.
///
/// `bm_wire_diff::time` compares this for every message, target and port; this
/// is the one case spelled out, in the file where node-against-node lives.
#[test]
fn our_system_time_response_is_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    // The same reading on both clocks. `bm_rtc_*` are integrator hooks rather
    // than bm_core code -- see `bm_wire_diff::stack::set_both_clocks` -- so the
    // reading is a shared input here, not something to compare.
    let rtc = stack::set_both_clocks(NOON_ISH);

    let request = time::build_frame(&TimeInput {
        message: TimeMessage::Request,
        target: time::Target::ThisNode,
        ingress_port: 1,
        global_multicast: false,
        clock_us: NOON_ISH.to_utc_micros(),
        utc_time_us: 0,
        trailing: Vec::new(),
        decode_probe: Vec::new(),
    });
    inject(1, &request);
    let captured = drain();
    assert!(!captured.is_empty(), "the oracle answered nothing");

    let mut node: Node<OracleIdentity, SoftRtc, 4> = Node::new(OracleIdentity, rtc, NUM_PORTS);
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    let mut frame = request.clone();
    let ours = node
        .on_frame(0, 1, &mut frame)
        .reply
        .expect("a system-time request must be answered")
        .frame()
        .to_vec();

    compare_stamped("system time response", &captured, ours);
}

/// The requester half: `bcmp_time_get_time` and `bcmp_time_set_time`.
///
/// Neither is sequenced -- `time_init` registers all three types
/// `{false, false}` -- so this does not disturb the `message_count` the
/// sequenced-request test below relies on being untouched.
#[test]
fn our_system_time_requests_are_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    let mut node = node();

    let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
    unsafe {
        assert_eq!(
            bm_wire_sys::bcmp_time_get_time(PEER_ID),
            bm_wire_sys::BmErr_BmOK
        );
    }
    pump_until_quiet();
    let captured = drain();
    assert!(!captured.is_empty(), "the oracle sent nothing");
    let ours = node
        .request_system_time(now_ms, PEER_ID)
        .expect("a registered type is sent")
        .frame()
        .to_vec();
    compare_stamped("system time request", &captured, ours);

    let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
    let utc_time_us = NOON_ISH.to_utc_micros();
    unsafe {
        assert_eq!(
            bm_wire_sys::bcmp_time_set_time(PEER_ID, utc_time_us),
            bm_wire_sys::BmErr_BmOK
        );
    }
    pump_until_quiet();
    let captured = drain();
    assert!(!captured.is_empty(), "the oracle sent nothing");
    let ours = node
        .set_system_time(now_ms, PEER_ID, utc_time_us)
        .expect("a registered type is sent")
        .frame()
        .to_vec();
    compare_stamped("system time set", &captured, ours);
}

/// Our link-local address and MAC are derived the way bm_core derives them.
#[test]
fn our_addresses_match_the_oracles() {
    let node = node();
    assert_eq!(
        node.link_local().0,
        bm_wire::addr::nodeid_to_ip(LINK_LOCAL_PREFIX, stack::NODE_ID).0
    );

    let mut c_ip = bm_wire_sys::BmIpAddr::default();
    let mut c_mac = [0u8; 6];
    unsafe {
        bm_wire_sys::nodeid_to_ip(&mut c_ip, LINK_LOCAL_PREFIX, stack::NODE_ID);
        bm_wire_sys::mac_from_nodeid(c_mac.as_mut_ptr(), stack::NODE_ID);
    }
    assert_eq!(node.link_local().0, c_ip.addr);
    assert_eq!(bm_wire::addr::mac_from_nodeid(stack::NODE_ID), c_mac);
}

// ---------------------------------------------------------------------------
// The requests a node issues -- card I3.
// ---------------------------------------------------------------------------

/// Some other node, the one a request is aimed at.
const PEER_ID: u64 = 0x0000_0000_55AA_0011;

/// `bcmp_request_info` is what a C node sends on discovering a neighbour, and
/// `Node::request` is the path ours sends it on.
#[test]
fn our_device_info_request_is_byte_identical_to_the_c() {
    let _guard = oracle();
    drain();

    let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
    unsafe {
        assert_eq!(
            bm_wire_sys::bcmp_request_info(
                PEER_ID,
                (&raw const bm_wire_sys::multicast_ll_addr).cast(),
                None,
            ),
            bm_wire_sys::BmErr_BmOK
        );
    }
    pump_until_quiet();
    let captured = drain();
    assert!(!captured.is_empty(), "the oracle sent nothing");

    let mut body = [0u8; DeviceInfoRequest::LEN];
    DeviceInfoRequest {
        target_node_id: PEER_ID,
    }
    .encode(&mut body)
    .unwrap();

    let mut node = node();
    let ours = node
        .request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::DEVICE_INFO_REQUEST,
            &body,
        )
        .expect("a registered type is sent")
        .frame()
        .to_vec();

    compare_stamped("device info request", &captured, ours);
}

/// The sequenced path, which nothing ported issues yet: `bcmp/config.c` is the
/// only module in bm_core that registers a `sequenced_request`, so its
/// `BcmpConfigGetMessage` is what proves a request carries the number the C
/// would have given it — in the header, and in the checksum over it.
///
/// Both counters start at zero here because this is the **only** test in this
/// binary that issues a sequenced request: `message_count` is a function-level
/// `static` inside `serialize` with nothing that resets it, so a second such
/// test would have to say where the C had got to. Card C3, which ports config,
/// is where that will matter.
#[test]
fn our_sequenced_request_carries_the_number_the_c_would_have_given_it() {
    let _guard = oracle();
    drain();

    let mut node = node();
    node.register(MessageType::CONFIG_GET, PacketCfg::REQUEST)
        .expect("room for one more type");

    // A `BmConfigGet`: the 16-byte config header, a partition, a key length
    // and the key. Its contents do not matter to either side -- `serialize`
    // copies the body verbatim on a little-endian host -- but a plausible one
    // keeps the length honest.
    let mut body = Vec::new();
    body.extend_from_slice(&stack::NODE_ID.to_le_bytes());
    body.extend_from_slice(&PEER_ID.to_le_bytes());
    body.push(0);
    body.push(3);
    body.extend_from_slice(b"key");

    for expected_seq in 0..3u32 {
        let now_ms = unsafe { bm_wire_sys::bm_shim_tick_count() };
        unsafe {
            assert_eq!(
                bm_wire_sys::bcmp_tx(
                    &raw const bm_wire_sys::multicast_ll_addr,
                    bm_wire_sys::BcmpMessageType_BcmpConfigGetMessage,
                    body.as_mut_ptr(),
                    body.len() as u16,
                    0,
                    None,
                ),
                bm_wire_sys::BmErr_BmOK
            );
        }
        pump_until_quiet();
        let captured = drain();
        assert!(!captured.is_empty(), "the oracle sent nothing");

        let ours = node
            .request(
                now_ms,
                &BmIpAddr::LINK_LOCAL_MULTICAST,
                MessageType::CONFIG_GET,
                &body,
            )
            .expect("a registered type is sent");
        let frame = ours.frame().to_vec();
        let mut parsed = frame.clone();
        assert_eq!(
            rx::accept(&mut parsed)
                .expect("our own request validates")
                .header
                .seq_num,
            expected_seq,
            "the nth sequenced request carries n, as packet_test.cpp asserts"
        );
        compare_stamped("config get", &captured, frame);
    }

    assert_eq!(
        node.registry().pending_len(),
        3,
        "and all three are waiting for a reply, as the C's sequence list is"
    );
}
