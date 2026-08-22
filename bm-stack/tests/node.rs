//! The node, driven with a mock PHY.

use bm_stack::mock::{MockPhy, Script};
use bm_stack::node::{HOP_LIMIT, LINK_LOCAL_PREFIX};
use bm_stack::{Egress, Identity, Node, transmit};
use bm_wire::addr;
use bm_wire::bcmp::info::{DeviceInfoReply, DeviceInfoRequest};
use bm_wire::bcmp::neighbors::{NeighborTableReply, NeighborTableRequest};
use bm_wire::bcmp::{BCMP_HEADER_LEN, DeviceInfo, Heartbeat, MessageType, rx, tx};
use bm_wire::frame::*;
use bm_wire::neighbor::HEARTBEAT_PERIOD_S;
use bm_wire::util::BmIpAddr;
use embassy_futures::block_on;

const NODE_ID: u64 = 0xC0FF_EE00_1234_5678;
const PEER_ID: u64 = 0x0000_0000_55AA_0011;
const PORTS: u8 = 2;

struct TestIdentity;

impl Identity for TestIdentity {
    fn node_id(&self) -> u64 {
        NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor_id: 0xBEEF,
            product_id: 0x0042,
            serial_num: *b"bm-stack-test\0\0\0",
            git_sha: 0x1234_5678,
            ver_major: 1,
            ver_minor: 2,
            ver_rev: 4,
            ver_hw: 3,
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        b"0.1.0-bm-stack"
    }

    fn device_name(&self) -> &[u8] {
        b"rust-node"
    }
}

fn node() -> Node<TestIdentity, 4> {
    Node::new(TestIdentity, PORTS)
}

/// A BCMP frame from the peer, as the wire would deliver it.
fn peer_frame(message_type: MessageType, body: &[u8], dst: BmIpAddr) -> Vec<u8> {
    let payload_len = BCMP_HEADER_LEN + body.len();
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
    frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
    frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
        .copy_from_slice(&(payload_len as u16).to_be_bytes());
    frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
    frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
        .copy_from_slice(&addr::nodeid_to_ip(LINK_LOCAL_PREFIX, PEER_ID).0);
    frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
        .copy_from_slice(&dst.0);
    tx::serialize(&mut frame, message_type, 0, body).unwrap();
    frame
}

fn heartbeat_frame(uptime_us: u64) -> Vec<u8> {
    let mut body = [0u8; Heartbeat::LEN];
    Heartbeat {
        time_since_boot_us: uptime_us,
        liveliness_lease_dur_s: HEARTBEAT_PERIOD_S,
    }
    .encode(&mut body)
    .unwrap();
    peer_frame(
        MessageType::HEARTBEAT,
        &body,
        BmIpAddr::LINK_LOCAL_MULTICAST,
    )
}

#[test]
fn the_heartbeat_we_emit_is_a_well_formed_frame() {
    let mut node = node();
    let outbound = node.on_tick(1_234_567).expect("a tick emits a heartbeat");
    let mut frame = outbound.frame().to_vec();

    // Ethernet: 33:33 multicast MAC for ff02::1, our derived MAC, IPv6.
    assert_eq!(&frame[0..6], &[0x33, 0x33, 0x00, 0x00, 0x00, 0x01]);
    assert_eq!(&frame[6..12], &addr::mac_from_nodeid(NODE_ID));
    assert_eq!(
        u16::from_be_bytes([frame[12], frame[13]]),
        ETHERNET_TYPE_IPV6
    );

    // IPv6: version 6, no traffic class or flow label, BCMP, hop limit 64.
    assert_eq!(&frame[14..18], &[0x60, 0x00, 0x00, 0x00]);
    assert_eq!(frame[IPV6_NEXT_HEADER_OFFSET], IP_PROTO_BCMP);
    assert_eq!(frame[IPV6_HOP_LIMIT_OFFSET], HOP_LIMIT);
    assert_eq!(
        &frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16],
        &addr::nodeid_to_ip(LINK_LOCAL_PREFIX, NODE_ID).0
    );

    // And a peer would accept it.
    let received = rx::accept(&mut frame).expect("our own heartbeat must validate");
    assert_eq!(received.header.message_type, MessageType::HEARTBEAT);
    let heartbeat = Heartbeat::decode(received.payload).unwrap();
    assert_eq!(heartbeat.time_since_boot_us, 1_234_567_000);
    assert_eq!(heartbeat.liveliness_lease_dur_s, HEARTBEAT_PERIOD_S);
}

#[test]
fn a_heartbeat_from_a_new_peer_is_answered_with_an_info_request() {
    let mut node = node();
    let mut frame = heartbeat_frame(1_000_000);
    let outbound = node
        .on_frame(1000, 1, &mut frame)
        .expect("a new neighbour is asked for its info");

    let mut reply = outbound.frame().to_vec();
    let received = rx::accept(&mut reply).unwrap();
    assert_eq!(
        received.header.message_type,
        MessageType::DEVICE_INFO_REQUEST
    );
    assert_eq!(
        DeviceInfoRequest::decode(received.payload).unwrap(),
        DeviceInfoRequest {
            target_node_id: PEER_ID
        }
    );
    assert_eq!(
        &reply[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16],
        &BmIpAddr::LINK_LOCAL_MULTICAST.0,
        "bm_core asks on the link-local address, not the one it heard on"
    );

    // The neighbour is recorded, on the port it arrived on.
    let neighbor = node.neighbors().find(PEER_ID).expect("recorded");
    assert_eq!(neighbor.port, 1);
    assert!(neighbor.online);
}

#[test]
fn a_second_heartbeat_from_a_known_peer_says_nothing() {
    let mut node = node();
    node.on_frame(1000, 1, &mut heartbeat_frame(1_000_000));
    assert!(
        node.on_frame(2000, 1, &mut heartbeat_frame(2_000_000))
            .is_none(),
        "a known neighbour needs no reply"
    );
}

#[test]
fn a_device_info_request_is_answered_with_our_identity() {
    for target in [0u64, NODE_ID] {
        let mut node = node();
        let mut frame = peer_frame(
            MessageType::DEVICE_INFO_REQUEST,
            &target.to_le_bytes(),
            BmIpAddr::LINK_LOCAL_MULTICAST,
        );
        let outbound = node.on_frame(1000, 1, &mut frame).expect("answered");
        let mut reply = outbound.frame().to_vec();
        let received = rx::accept(&mut reply).unwrap();
        assert_eq!(received.header.message_type, MessageType::DEVICE_INFO_REPLY);

        let decoded = DeviceInfoReply::decode(received.payload).unwrap();
        assert_eq!(decoded.info.node_id, NODE_ID);
        assert_eq!(decoded.info.vendor_id, 0xBEEF);
        assert_eq!(decoded.info.product_id, 0x0042);
        assert_eq!(decoded.info.git_sha, 0x1234_5678);
        assert_eq!(
            (
                decoded.info.ver_major,
                decoded.info.ver_minor,
                decoded.info.ver_rev,
                decoded.info.ver_hw
            ),
            (1, 2, 4, 3)
        );
        assert_eq!(decoded.version_string, b"0.1.0-bm-stack");
        assert_eq!(decoded.device_name, b"rust-node");
    }
}

#[test]
fn a_request_for_another_node_is_ignored() {
    let mut node = node();
    let mut frame = peer_frame(
        MessageType::DEVICE_INFO_REQUEST,
        &0x1234_u64.to_le_bytes(),
        BmIpAddr::LINK_LOCAL_MULTICAST,
    );
    assert!(node.on_frame(1000, 1, &mut frame).is_none());
}

#[test]
fn a_neighbor_table_request_reports_our_ports_and_neighbours() {
    let mut node = node();
    node.on_frame(1000, 2, &mut heartbeat_frame(1_000_000));

    let mut frame = peer_frame(
        MessageType::NEIGHBOR_TABLE_REQUEST,
        &0u64.to_le_bytes(),
        BmIpAddr::LINK_LOCAL_MULTICAST,
    );
    let outbound = node.on_frame(2000, 1, &mut frame).expect("answered");
    let mut reply = outbound.frame().to_vec();
    let received = rx::accept(&mut reply).unwrap();
    assert_eq!(
        received.header.message_type,
        MessageType::NEIGHBOR_TABLE_REPLY
    );

    let table = NeighborTableReply::decode(received.payload).unwrap();
    assert_eq!(table.node_id, NODE_ID);
    assert_eq!(table.port_count(), PORTS);
    assert!(table.ports().all(|p| p.is_up()));
    assert_eq!(table.neighbor_count(), 1);
    let neighbor = table.neighbors().next().unwrap();
    assert_eq!(neighbor.node_id, PEER_ID);
    assert_eq!(neighbor.port, 2);
    assert!(neighbor.is_online());

    // And the request itself round-trips, so the decoder agrees with what we
    // fed it.
    assert_eq!(
        NeighborTableRequest::decode(&0u64.to_le_bytes()).unwrap(),
        NeighborTableRequest { target_node_id: 0 }
    );
}

#[test]
fn garbage_is_dropped_without_a_reply() {
    let mut node = node();
    for frame in [
        vec![0u8; 4],
        vec![0xAA; 64],
        vec![0xFF; MIN_FRAME_WITH_ADDRESSES],
    ] {
        let mut frame = frame;
        assert!(node.on_frame(1000, 1, &mut frame).is_none());
    }
    // A valid frame with a checksum one bit out.
    let mut frame = heartbeat_frame(1_000_000);
    frame[MIN_FRAME_WITH_ADDRESSES + 2] ^= 0x01;
    assert!(node.on_frame(1000, 1, &mut frame).is_none());
}

#[test]
fn a_link_local_frame_is_stamped_once_per_port() {
    let mut node = node();
    let outbound = node.on_tick(1000).unwrap();
    let mut phy = MockPhy::new(PORTS, Vec::new());
    block_on(transmit(&mut phy, outbound, PORTS)).unwrap();

    assert_eq!(phy.sent.len(), usize::from(PORTS));
    for (index, sent) in phy.sent.iter().enumerate() {
        let port = index as u8 + 1;
        assert_eq!(sent.egress, Egress::Port(port));
        assert_eq!(
            sent.frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F,
            port,
            "each copy carries its own egress port"
        );
        // And each copy still validates, which is the whole point of the
        // checksum patch.
        let mut copy = sent.frame.clone();
        assert!(rx::accept(&mut copy).is_ok());
    }
}

#[test]
fn a_global_multicast_frame_goes_out_once_unstamped() {
    let mut node = node();
    // A device-info reply goes back to the address it was asked on.
    let mut frame = peer_frame(
        MessageType::DEVICE_INFO_REQUEST,
        &0u64.to_le_bytes(),
        BmIpAddr::GLOBAL_MULTICAST,
    );
    let outbound = node.on_frame(1000, 1, &mut frame).unwrap();
    let mut phy = MockPhy::new(PORTS, Vec::new());
    block_on(transmit(&mut phy, outbound, PORTS)).unwrap();

    assert_eq!(phy.sent.len(), 1);
    assert_eq!(phy.sent[0].egress, Egress::AllPorts);
    assert_eq!(phy.sent[0].frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 0);
}

/// The whole loop: a peer appears, we ask for its info, time passes, and we
/// heartbeat on schedule.
#[test]
fn the_run_loop_answers_and_heartbeats() {
    let mut node = node();
    let mut phy = MockPhy::new(
        PORTS,
        vec![
            Script::Receive {
                port: 1,
                frame: heartbeat_frame(1_000_000),
            },
            // Two heartbeat periods, so the ticker fires twice.
            Script::Idle {
                ms: u64::from(HEARTBEAT_PERIOD_S) * 1000 + 100,
            },
            Script::Idle {
                ms: u64::from(HEARTBEAT_PERIOD_S) * 1000 + 100,
            },
        ],
    );

    let error = block_on(node.run(&mut phy));
    assert_eq!(error, bm_stack::mock::MockError::ScriptFinished);

    // The peer was recorded.
    assert!(node.neighbors().find(PEER_ID).is_some());

    // What went out: an info request for the new peer, then heartbeats. Both
    // are link-local, so each appears once per port.
    let mut kinds = Vec::new();
    for sent in &phy.sent {
        let mut frame = sent.frame.clone();
        let received = rx::accept(&mut frame).expect("everything we send must validate");
        kinds.push(received.header.message_type);
    }
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == MessageType::DEVICE_INFO_REQUEST)
            .count(),
        usize::from(PORTS),
        "one info request, on every port"
    );
    assert!(
        kinds
            .iter()
            .filter(|k| **k == MessageType::HEARTBEAT)
            .count()
            >= usize::from(PORTS),
        "at least one heartbeat, on every port"
    );
}
