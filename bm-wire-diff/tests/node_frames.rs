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
use bm_stack::{Identity, Node};
use bm_wire::bcmp::{DeviceInfo, MessageType, rx};
use bm_wire::frame::IPV6_INGRESS_EGRESS_PORTS_OFFSET;
use bm_wire::l2;
use bm_wire_diff::bcmp_messages::{BcmpMessagesInput, Request, Target, build_request};
use bm_wire_diff::stack::{self, NUM_PORTS, drain, inject, oracle, pump_until_quiet};

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
fn node() -> Node<OracleIdentity, 4> {
    let mut node = Node::new(OracleIdentity, NUM_PORTS);
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
