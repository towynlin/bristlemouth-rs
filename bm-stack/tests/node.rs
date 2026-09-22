//! The node, driven with a mock PHY.

use bm_stack::mock::{MockPhy, Script, Sent};
use bm_stack::node::{EXPIRY_PERIOD_MS, HOP_LIMIT, LINK_LOCAL_PREFIX};
use bm_stack::{Egress, Event, Identity, Node, Rtc, RtcTimeAndDate, SoftRtc, deliver, transmit};
use bm_wire::addr;
use bm_wire::bcmp::info::{DeviceInfoReply, DeviceInfoRequest, InfoRequestKind};
use bm_wire::bcmp::neighbors::{NeighborTableReply, NeighborTableRequest};
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::registry::PacketCfg;
use bm_wire::bcmp::time::{SystemTimeHeader, SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
use bm_wire::bcmp::{
    BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, DeviceInfo, Heartbeat, MessageType, rx, tx,
};
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

fn node() -> Node<TestIdentity, SoftRtc, 4> {
    Node::new(TestIdentity, SoftRtc::new(), PORTS)
}

/// A BCMP frame from the peer, as the wire would deliver it.
fn peer_frame(message_type: MessageType, body: &[u8], dst: BmIpAddr) -> Vec<u8> {
    peer_frame_seq(message_type, body, dst, 0)
}

/// The same, carrying a sequence number — what a reply to one of our requests
/// looks like.
fn peer_frame_seq(message_type: MessageType, body: &[u8], dst: BmIpAddr, seq_num: u32) -> Vec<u8> {
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
    tx::serialize(&mut frame, message_type, seq_num, body).unwrap();
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
    let owed = node.on_frame(1000, 1, &mut frame);
    assert!(owed.relay.is_none(), "FF02::1 is never relayed");
    let outbound = owed.reply.expect("a new neighbour is asked for its info");

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
            .is_empty(),
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
        let outbound = node.on_frame(1000, 1, &mut frame).reply.expect("answered");
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
    assert!(node.on_frame(1000, 1, &mut frame).is_empty());
}

#[test]
fn a_neighbor_table_request_reports_our_ports_and_neighbours() {
    let mut node = node();
    for port in 1..=PORTS {
        node.set_link_up(port, true);
    }
    node.on_frame(1000, 2, &mut heartbeat_frame(1_000_000));

    let mut frame = peer_frame(
        MessageType::NEIGHBOR_TABLE_REQUEST,
        &0u64.to_le_bytes(),
        BmIpAddr::LINK_LOCAL_MULTICAST,
    );
    let outbound = node.on_frame(2000, 1, &mut frame).reply.expect("answered");
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

/// The reply carries the link state the node was told about, not a constant.
#[test]
fn a_neighbor_table_reply_reports_a_down_port_as_down() {
    let mut node = node();
    node.set_link_up(1, true);
    node.set_link_up(2, false);
    assert!(node.link_up(1) && !node.link_up(2));

    let mut frame = peer_frame(
        MessageType::NEIGHBOR_TABLE_REQUEST,
        &0u64.to_le_bytes(),
        BmIpAddr::LINK_LOCAL_MULTICAST,
    );
    let outbound = node.on_frame(1000, 1, &mut frame).reply.expect("answered");
    let mut reply = outbound.frame().to_vec();
    let received = rx::accept(&mut reply).unwrap();
    let table = NeighborTableReply::decode(received.payload).unwrap();

    let states: Vec<bool> = table.ports().map(|p| p.is_up()).collect();
    assert_eq!(states, vec![true, false]);
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
        assert!(node.on_frame(1000, 1, &mut frame).is_empty());
    }
    // A valid frame with a checksum one bit out.
    let mut frame = heartbeat_frame(1_000_000);
    frame[MIN_FRAME_WITH_ADDRESSES + 2] ^= 0x01;
    assert!(node.on_frame(1000, 1, &mut frame).is_empty());
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
    let owed = node.on_frame(1000, 1, &mut frame);
    // FF03::1 is relayed out the other port as well as answered; the reply is
    // what this test is about, and the relay has tests of its own below.
    assert!(owed.relay.is_some(), "global multicast is flooded onward");
    let outbound = owed.reply.unwrap();
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

// ---------------------------------------------------------------------------
// Forwarding between ports -- card I2.
// ---------------------------------------------------------------------------

/// The frames a node put on the wire for one received frame, in order.
fn relay_through(
    node: &mut Node<TestIdentity, SoftRtc, 4>,
    ingress_port: u8,
    frame: &mut [u8],
) -> MockPhy {
    let mut phy = MockPhy::new(PORTS, Vec::new());
    let owed = node.on_frame(1000, ingress_port, frame);
    block_on(deliver(&mut phy, owed, PORTS)).unwrap();
    phy
}

/// A global-multicast frame is flooded out every port but the one it arrived on,
/// unstamped, and with both port nibbles cleared.
#[test]
fn a_global_multicast_frame_is_relayed_out_the_other_port() {
    for ingress in 1..=PORTS {
        let original = peer_frame(
            MessageType(0xFFFF), // nothing registers it, so nothing replies
            &[7u8; 8],
            BmIpAddr::GLOBAL_MULTICAST,
        );
        let mut frame = original.clone();
        let mut node = node();
        let phy = relay_through(&mut node, ingress, &mut frame);

        let expected: Vec<u8> = (1..=PORTS).filter(|p| *p != ingress).collect();
        assert_eq!(
            phy.sent
                .iter()
                .map(|s| s.egress)
                .collect::<Vec<_>>()
                .as_slice(),
            expected
                .iter()
                .map(|p| Egress::Port(*p))
                .collect::<Vec<_>>()
                .as_slice(),
            "flooded to every port but the ingress one, one frame each"
        );
        for sent in &phy.sent {
            assert_eq!(
                sent.frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 0,
                "a forwarded copy carries neither port"
            );
            // Only that one byte differs from what arrived, and global
            // multicast is never stamped, so the checksum still holds.
            let mut expected = original.clone();
            expected[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = 0;
            assert_eq!(sent.frame, expected);
            let mut copy = sent.frame.clone();
            assert!(rx::accept(&mut copy).is_ok(), "and a peer accepts it");
        }
    }
}

/// `FF02::1` stops here: it is the neighbour address, so a node consumes it and
/// relays nothing. Without that, every heartbeat would loop the network.
#[test]
fn a_link_local_neighbor_frame_is_never_relayed() {
    let mut frame = heartbeat_frame(1_000_000);
    let mut node = node();
    let phy = relay_through(&mut node, 1, &mut frame);
    // The info request the heartbeat provokes goes out both ports; nothing else.
    assert_eq!(phy.sent.len(), usize::from(PORTS));
    for sent in &phy.sent {
        let mut copy = sent.frame.clone();
        let received = rx::accept(&mut copy).unwrap();
        assert_eq!(
            received.header.message_type,
            MessageType::DEVICE_INFO_REQUEST
        );
    }
}

/// A link-local multicast that is *not* `FF02::1` has no routing callback to
/// consult, so bm_core submits it and forwards it nowhere.
#[test]
fn a_link_local_frame_that_is_not_the_neighbor_address_is_not_relayed() {
    let mut dst = BmIpAddr::LINK_LOCAL_MULTICAST;
    dst.0[15] = 0x05; // FF02::5
    let mut frame = peer_frame(MessageType(0xFFFF), &[1u8; 4], dst);
    let mut node = node();
    let phy = relay_through(&mut node, 1, &mut frame);
    assert!(phy.sent.is_empty());
}

/// The relay comes before the reply, because L2 queues the forwarded copy
/// before it submits the frame up the stack.
#[test]
fn a_relay_goes_out_before_the_reply_it_shares_a_frame_with() {
    let mut frame = peer_frame(
        MessageType::DEVICE_INFO_REQUEST,
        &0u64.to_le_bytes(),
        BmIpAddr::GLOBAL_MULTICAST,
    );
    let mut node = node();
    let phy = relay_through(&mut node, 1, &mut frame);

    assert_eq!(phy.sent.len(), 2, "one relay, one reply");
    assert_eq!(
        phy.sent[0].egress,
        Egress::Port(2),
        "the relay, port by port"
    );
    assert_eq!(
        phy.sent[1].egress,
        Egress::AllPorts,
        "then the reply, to every port at once"
    );
    let mut relayed = phy.sent[0].frame.clone();
    assert_eq!(
        rx::accept(&mut relayed).unwrap().header.message_type,
        MessageType::DEVICE_INFO_REQUEST,
        "the relay is the request, forwarded onward"
    );
    let mut reply = phy.sent[1].frame.clone();
    assert_eq!(
        rx::accept(&mut reply).unwrap().header.message_type,
        MessageType::DEVICE_INFO_REPLY
    );
}

/// The forwarded copy is taken before the receive path rewrites the frame, so
/// the legacy port bytes go out as they arrived even though `accept` cleared
/// them to checksum -- divergence #9's clear must not leak into the relay.
#[test]
fn a_relayed_copy_keeps_the_bytes_the_receive_path_cleared() {
    let mut original = peer_frame(MessageType(0xFFFF), &[3u8; 8], BmIpAddr::GLOBAL_MULTICAST);
    // Bytes 26 and 27 are `clear_ports_legacy`'s target. A frame carrying them
    // fails the local checksum, which is exactly the point: the relay does not
    // care, and must carry them onward.
    original[IPV6_SOURCE_ADDRESS_OFFSET + 4] = 0xAB;
    original[IPV6_SOURCE_ADDRESS_OFFSET + 5] = 0xCD;

    let mut frame = original.clone();
    let mut node = node();
    let phy = relay_through(&mut node, 1, &mut frame);

    assert_eq!(phy.sent.len(), 1, "relayed, not answered");
    let relayed = &phy.sent[0].frame;
    assert_eq!(relayed[IPV6_SOURCE_ADDRESS_OFFSET + 4], 0xAB);
    assert_eq!(relayed[IPV6_SOURCE_ADDRESS_OFFSET + 5], 0xCD);
    let mut expected = original.clone();
    expected[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = 0;
    assert_eq!(relayed, &expected);
}

/// Three nodes in a chain: what leaves the middle node's far port is what the
/// third node receives, and it is still the message the first node sent.
#[test]
fn a_chain_of_nodes_relays_a_global_multicast_message() {
    let origin = peer_frame(MessageType(0xFFFF), b"chain", BmIpAddr::GLOBAL_MULTICAST);

    // Middle node: in on port 1, out on port 2.
    let mut middle = node();
    let mut frame = origin.clone();
    let phy = relay_through(&mut middle, 1, &mut frame);
    assert_eq!(phy.sent.len(), 1);
    assert_eq!(phy.sent[0].egress, Egress::Port(2));

    // Far node: in on port 1 again, out on port 2 again, unchanged.
    let mut far = Node::<TestIdentity, SoftRtc, 4>::new(TestIdentity, SoftRtc::new(), PORTS);
    let mut hop = phy.sent[0].frame.clone();
    let phy = relay_through(&mut far, 1, &mut hop);
    assert_eq!(phy.sent.len(), 1);
    assert_eq!(phy.sent[0].egress, Egress::Port(2));

    let mut arrived = phy.sent[0].frame.clone();
    let received = rx::accept(&mut arrived).expect("still a valid frame two hops on");
    assert_eq!(received.header.message_type, MessageType(0xFFFF));
    assert_eq!(received.payload, b"chain");
}

/// `bcmp_ll_forward`: one fresh frame per port, from this node, carrying the
/// received message unchanged.
#[test]
fn a_link_local_message_is_re_flooded_as_a_fresh_frame_per_port() {
    let mut arriving = peer_frame(
        MessageType::SYSTEM_TIME_REQUEST,
        &[0xEE; 16],
        BmIpAddr::LINK_LOCAL_MULTICAST,
    );
    let received = rx::accept(&mut arriving).unwrap();
    let payload_len = BCMP_HEADER_LEN + received.payload.len();
    let bcmp = arriving[BCMP_HEADER_OFFSET..BCMP_HEADER_OFFSET + payload_len].to_vec();

    let mut node = node();
    let ingress = 1u8;
    let mut ports = Vec::new();
    for port in bm_wire::bcmp::forward::egress_ports(PORTS, ingress) {
        let mut phy = MockPhy::new(PORTS, Vec::new());
        let outbound = node
            .forward_link_local(port, &bcmp)
            .expect("a forward fits the transmit buffer");
        assert_eq!(outbound.mask(), 1u16 << (port - 1), "one port only");
        block_on(transmit(&mut phy, outbound, PORTS)).unwrap();

        assert_eq!(phy.sent.len(), 1);
        assert_eq!(phy.sent[0].egress, Egress::Port(port));
        let sent = &phy.sent[0].frame;

        // The port rides in the multicast MAC -- divergence #24.
        assert_eq!(
            &sent[0..6],
            &[0x33, 0x33, 0x00, port, 0x00, 0x01],
            "the egress port reaches the wire inside the destination MAC"
        );
        assert_eq!(sent[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F, port);

        let mut copy = sent.clone();
        let forwarded = rx::accept(&mut copy).expect("a peer accepts the forward");
        assert_eq!(
            forwarded.header.message_type,
            MessageType::SYSTEM_TIME_REQUEST
        );
        assert_eq!(forwarded.payload, &[0xEE; 16]);
        assert_eq!(
            forwarded.src.to_node_id(),
            NODE_ID,
            "the forwarder is the source now, not the originator"
        );
        assert_eq!(
            &copy[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16],
            &BmIpAddr::LINK_LOCAL_MULTICAST.0,
            "the port request never reaches the wire in the address"
        );
        ports.push(port);
    }
    assert_eq!(ports, vec![2], "port 1 is where it came from");
}

// ---------------------------------------------------------------------------
// Requests, replies and timeouts -- card I3.
// ---------------------------------------------------------------------------

/// An [`Event`], with the payload copied out so a test can keep it.
///
/// The real thing borrows the frame it arrived in, which is what makes the
/// node allocation-free and what makes a collected event need a shape of its
/// own.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Reply {
        /// What we asked, which the C does not compare against what answered.
        request_type: MessageType,
        /// What answered.
        message_type: MessageType,
        source: u64,
        payload: Vec<u8>,
    },
    Timeout {
        request_type: MessageType,
        seq_num: u32,
    },
    Message {
        message_type: MessageType,
        seq_num: u32,
        source: u64,
    },
    EchoReply {
        source: u64,
        id: u16,
        seq_num: u16,
        payload: Vec<u8>,
        round_trip_ms: u32,
    },
}

fn seen(event: Event<'_>) -> Seen {
    match event {
        Event::Reply {
            request,
            message_type,
            source,
            payload,
        } => Seen::Reply {
            request_type: request.message_type,
            message_type,
            source,
            payload: payload.to_vec(),
        },
        Event::Timeout { request } => Seen::Timeout {
            request_type: request.message_type,
            seq_num: request.seq_num,
        },
        Event::Message {
            message_type,
            seq_num,
            source,
            ..
        } => Seen::Message {
            message_type,
            seq_num,
            source,
        },
        Event::EchoReply {
            source,
            reply,
            round_trip_ms,
        } => Seen::EchoReply {
            source,
            id: reply.id,
            seq_num: reply.seq_num,
            payload: reply.payload.to_vec(),
            round_trip_ms,
        },
        _ => unreachable!("Event is non_exhaustive; this test knows all of it"),
    }
}

/// `bcmp/config.c` is the only module in bm_core that issues sequenced
/// requests, so its types are the ones a test has to borrow to exercise the
/// machinery at all. Nothing here parses a config body: what is under test is
/// the sequence number and the routing, not the payload.
const REQUEST_TYPE: MessageType = MessageType::CONFIG_GET;
const REPLY_TYPE: MessageType = MessageType::CONFIG_VALUE;

/// A node that also knows the two config types, registered with the flags
/// `bcmp_config_init` gives them.
fn requesting_node() -> Node<TestIdentity, SoftRtc, 4> {
    let mut node = node();
    node.register(REQUEST_TYPE, PacketCfg::REQUEST).unwrap();
    node.register(REPLY_TYPE, PacketCfg::REPLY).unwrap();
    node
}

/// The frame a node built, parsed back.
fn sent_header(outbound: &bm_stack::Outbound<'_>) -> bm_wire::bcmp::BcmpHeader {
    let mut frame = outbound.frame().to_vec();
    rx::accept(&mut frame)
        .expect("everything we send must validate")
        .header
}

#[test]
fn a_message_of_an_unregistered_type_is_never_sent() {
    let mut node = node();
    assert!(
        node.request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[1, 2, 3])
            .is_none(),
        "serialize returns BmENODEV and bcmp_tx transmits nothing"
    );
    assert_eq!(node.registry().pending_len(), 0);
}

#[test]
fn a_sequenced_request_takes_the_next_number_and_waits_for_its_reply() {
    let mut node = requesting_node();
    for expected in 0..3u32 {
        let outbound = node
            .request(100, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
            .expect("a registered type is sent");
        assert_eq!(sent_header(&outbound).seq_num, expected);
    }
    assert_eq!(
        node.registry().pending_len(),
        3,
        "all three are outstanding"
    );
    let first = node.registry().pending().next().copied().expect("one");
    assert_eq!(first.message_type, REQUEST_TYPE);
    assert_eq!(first.timestamp_ms, 100);
}

/// Outside `bcmp/config.c` nothing in bm_core is sequenced, so everything a
/// node says today goes out with a sequence number of zero and is never waited
/// on. If this ever stops being true, every `node_frames` comparison moves.
#[test]
fn everything_else_goes_out_unsequenced_and_untracked() {
    let mut node = node();
    let heartbeat = node.on_tick(1000).expect("a tick emits a heartbeat");
    assert_eq!(sent_header(&heartbeat).seq_num, 0);

    let info_request = node
        .on_frame(1000, 1, &mut heartbeat_frame(1_000_000))
        .reply
        .expect("a new neighbour is asked for its info");
    assert_eq!(sent_header(&info_request).seq_num, 0);
    assert_eq!(node.registry().pending_len(), 0);
}

/// A reply the node was waiting for is reported once, with its payload, and
/// the request is no longer outstanding.
#[test]
fn a_reply_answers_the_request_it_matches() {
    let mut node = requesting_node();
    node.request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
        .expect("sent");

    let mut frame = peer_frame_seq(
        REPLY_TYPE,
        &[0xDE, 0xAD, 0xBE, 0xEF],
        BmIpAddr::LINK_LOCAL_MULTICAST,
        0,
    );
    let mut events = Vec::new();
    let owed = node.on_frame_with(10, 1, &mut frame, |e| events.push(seen(e)));

    assert!(
        owed.is_empty(),
        "a reply to our own request needs no answer"
    );
    assert_eq!(
        events,
        vec![Seen::Reply {
            request_type: REQUEST_TYPE,
            message_type: REPLY_TYPE,
            source: PEER_ID,
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        }]
    );
    assert_eq!(node.registry().pending_len(), 0, "the request is answered");
}

/// Divergence #21: the C matches a reply on its sequence number alone. It
/// records the request's type and never compares it, so a reply of an
/// unrelated type answers the request and the requester is handed a body of a
/// shape it never asked for.
#[test]
fn a_reply_of_the_wrong_type_answers_the_request_anyway() {
    let mut node = requesting_node();
    node.register(MessageType::NEIGHBOR_PROTO_REPLY, PacketCfg::REPLY)
        .unwrap();
    node.request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
        .expect("sent");

    let mut frame = peer_frame_seq(
        MessageType::NEIGHBOR_PROTO_REPLY,
        &[0x11, 0x22],
        BmIpAddr::LINK_LOCAL_MULTICAST,
        0,
    );
    let mut events = Vec::new();
    node.on_frame_with(10, 1, &mut frame, |e| events.push(seen(e)));

    assert_eq!(
        events,
        vec![Seen::Reply {
            request_type: REQUEST_TYPE,
            message_type: MessageType::NEIGHBOR_PROTO_REPLY,
            source: PEER_ID,
            payload: vec![0x11, 0x22],
        }],
        "a config get is answered by a neighbour-proto reply, because the \
         numbers line up"
    );
}

/// A reply that answers nothing is not dropped: it goes to its type's own
/// processor, which is where an unsolicited message has always gone.
#[test]
fn a_reply_with_no_outstanding_request_is_delivered_as_a_message() {
    let mut node = requesting_node();
    let mut frame = peer_frame_seq(REPLY_TYPE, &[0x01], BmIpAddr::LINK_LOCAL_MULTICAST, 7);
    let mut events = Vec::new();
    node.on_frame_with(10, 1, &mut frame, |e| events.push(seen(e)));

    assert_eq!(
        events,
        vec![Seen::Message {
            message_type: REPLY_TYPE,
            seq_num: 7,
            source: PEER_ID,
        }]
    );
}

/// An unregistered type is dropped before its body is looked at, with no event
/// at all -- the C's `BmENODEV`, returned before `cfg->process` is reached.
/// Unregistering a type the node answers is therefore enough to stop it
/// answering.
#[test]
fn an_unregistered_type_is_dropped_without_an_event() {
    let mut node = node();
    let mut frame = peer_frame(MessageType(0xFFFF), &[0x01], BmIpAddr::LINK_LOCAL_MULTICAST);
    let mut events = Vec::new();
    let owed = node.on_frame_with(10, 1, &mut frame, |e| events.push(seen(e)));
    assert!(owed.is_empty());
    assert!(events.is_empty());

    assert!(node.unregister(MessageType::DEVICE_INFO_REQUEST));
    let mut frame = peer_frame(
        MessageType::DEVICE_INFO_REQUEST,
        &NODE_ID.to_le_bytes(),
        BmIpAddr::LINK_LOCAL_MULTICAST,
    );
    let owed = node.on_frame_with(10, 1, &mut frame, |e| events.push(seen(e)));
    assert!(owed.is_empty(), "nothing is registered to answer it now");
    assert!(events.is_empty());
}

/// `process_received_message` runs the request's callback *instead of* the
/// type's processor, never both. There is one processor to observe that with
/// -- the one that answers a device-info request -- so this registers that
/// type as a reply and lets it match an outstanding request: the request is
/// answered, and the device-info processor never runs.
#[test]
fn a_matched_reply_replaces_the_processing_its_type_would_have_had() {
    let mut node = requesting_node();
    assert!(node.unregister(MessageType::DEVICE_INFO_REQUEST));
    node.register(MessageType::DEVICE_INFO_REQUEST, PacketCfg::REPLY)
        .unwrap();
    node.request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
        .expect("sent");

    let mut frame = peer_frame_seq(
        MessageType::DEVICE_INFO_REQUEST,
        &NODE_ID.to_le_bytes(),
        BmIpAddr::LINK_LOCAL_MULTICAST,
        0,
    );
    let mut events = Vec::new();
    let owed = node.on_frame_with(10, 1, &mut frame, |e| events.push(seen(e)));

    assert!(
        owed.is_empty(),
        "the request's callback ran, so the device-info processor did not"
    );
    assert_eq!(
        events,
        vec![Seen::Reply {
            request_type: REQUEST_TYPE,
            message_type: MessageType::DEVICE_INFO_REQUEST,
            source: PEER_ID,
            payload: NODE_ID.to_le_bytes().to_vec(),
        }]
    );
}

/// Divergence #22: the request is stamped with a 24 ms timeout and nothing
/// applies it except a 150 ms sweep, so this one — sent on the sweep's phase —
/// lives for 150 ms, not 24.
#[test]
fn an_unanswered_request_dies_on_the_sweep_rather_than_on_its_timeout() {
    let mut node = requesting_node();
    node.request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
        .expect("sent");

    let mut events = Vec::new();
    for now in 0..EXPIRY_PERIOD_MS {
        node.on_expiry(now, |e| events.push(seen(e)));
    }
    assert!(
        events.is_empty(),
        "still outstanding at {} ms, well past its 24 ms timeout",
        EXPIRY_PERIOD_MS - 1
    );
    assert_eq!(node.registry().pending_len(), 1);

    node.on_expiry(EXPIRY_PERIOD_MS, |e| events.push(seen(e)));
    assert_eq!(
        events,
        vec![Seen::Timeout {
            request_type: REQUEST_TYPE,
            seq_num: 0,
        }]
    );
    assert_eq!(node.registry().pending_len(), 0);
}

/// The other half of divergence #22: a reply that arrives after the sweep has
/// given up is reported to the application a second time, now as unsolicited
/// traffic. One exchange, two notifications, and the first of them says it
/// failed.
#[test]
fn a_reply_that_arrives_after_the_timeout_is_reported_twice() {
    let mut node = requesting_node();
    node.request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
        .expect("sent");

    let mut events = Vec::new();
    node.on_expiry(EXPIRY_PERIOD_MS, |e| events.push(seen(e)));

    let mut frame = peer_frame_seq(REPLY_TYPE, &[0x42], BmIpAddr::LINK_LOCAL_MULTICAST, 0);
    node.on_frame_with(EXPIRY_PERIOD_MS + 1, 1, &mut frame, |e| {
        events.push(seen(e))
    });

    assert_eq!(
        events,
        vec![
            Seen::Timeout {
                request_type: REQUEST_TYPE,
                seq_num: 0,
            },
            Seen::Message {
                message_type: REPLY_TYPE,
                seq_num: 0,
                source: PEER_ID,
            },
        ]
    );
}

/// `bcmp_tx` checks the size before `serialize` runs, so a message too large to
/// send never becomes an outstanding request -- and does not consume a
/// sequence number either.
///
/// The ceiling is the transmit buffer: 1514 less the 54 bytes of Ethernet and
/// IPv6 headers and the 13-byte BCMP header. The C's guard admits one byte
/// more and then builds a frame a byte over the MTU, which is divergence #8.
#[test]
fn an_oversized_request_is_refused_before_it_is_recorded() {
    let largest = bm_stack::MTU - MIN_FRAME_WITH_ADDRESSES - BCMP_HEADER_LEN;
    assert_eq!(largest, 1447);

    let mut node = requesting_node();
    assert!(
        node.request(
            0,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            REQUEST_TYPE,
            &vec![0u8; largest + 1]
        )
        .is_none()
    );
    assert_eq!(node.registry().pending_len(), 0);
    assert_eq!(
        node.request(
            0,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            REQUEST_TYPE,
            &vec![0u8; largest]
        )
        .map(|o| (o.frame().len(), sent_header(&o).seq_num)),
        Some((bm_stack::MTU, 0)),
        "the largest that fits goes out, and takes the first sequence number"
    );
}

/// The whole loop, on the mock clock: a request goes out, nothing answers it,
/// and the expiry ticker reports it. `packet.c`'s sweep is a timer of its own,
/// so this must not have to wait for the ten-second heartbeat.
#[test]
fn the_run_loop_times_out_an_unanswered_request() {
    let mut node = requesting_node();
    let mut phy = MockPhy::new(
        PORTS,
        // Comfortably past one sweep, in steps too small to reach a heartbeat.
        vec![
            Script::Idle { ms: 60 },
            Script::Idle { ms: 60 },
            Script::Idle { ms: 60 },
            Script::Idle { ms: 60 },
            Script::Idle { ms: 60 },
        ],
    );

    let outbound = node
        .request(0, &BmIpAddr::LINK_LOCAL_MULTICAST, REQUEST_TYPE, &[0xAB])
        .expect("sent");
    block_on(transmit(&mut phy, outbound, PORTS)).unwrap();
    assert_eq!(phy.sent.len(), usize::from(PORTS), "once per port");

    let mut events = Vec::new();
    let error = block_on(node.run_with(&mut phy, |e| events.push(seen(e))));
    assert_eq!(error, bm_stack::mock::MockError::ScriptFinished);

    assert!(
        events.contains(&Seen::Timeout {
            request_type: REQUEST_TYPE,
            seq_num: 0,
        }),
        "the expiry ticker gave up on the request: {events:?}"
    );
    assert_eq!(node.registry().pending_len(), 0);
}

// ---------------------------------------------------------------------------
// Echo / ping -- card M1.
//
// The frames are compared against bm_core byte for byte in
// `bm-wire-diff/tests/ping.rs`. What is here is the half that comparison
// cannot reach: `bcmp_process_ping_reply` is static, transmits nothing, and
// reports to nobody, so the acceptance rule has no oracle and is asserted from
// the reading instead. See divergence #32.
// ---------------------------------------------------------------------------

/// The low sixteen bits of [`NODE_ID`], which is the whole of the `id` a ping
/// from this node carries and the whole of what a reply is matched on.
const OUR_PING_ID: u16 = NODE_ID as u16;

fn echo_request_frame(target_node_id: u64, id: u16, seq_num: u16, payload: &[u8]) -> Vec<u8> {
    echo_frame(
        MessageType::ECHO_REQUEST,
        target_node_id,
        id,
        seq_num,
        payload,
    )
}

fn echo_reply_frame(node_id: u64, id: u16, seq_num: u16, payload: &[u8]) -> Vec<u8> {
    echo_frame(MessageType::ECHO_REPLY, node_id, id, seq_num, payload)
}

/// Both messages are the same fourteen bytes, so one builder does for both.
fn echo_frame(
    message_type: MessageType,
    node_id: u64,
    id: u16,
    seq_num: u16,
    payload: &[u8],
) -> Vec<u8> {
    let request = EchoRequest {
        target_node_id: node_id,
        id,
        seq_num,
        payload,
    };
    let mut body = vec![0u8; request.encoded_len()];
    request.encode(&mut body).unwrap();
    peer_frame(message_type, &body, BmIpAddr::LINK_LOCAL_MULTICAST)
}

/// The body of whatever the node built, parsed back as an echo reply.
fn sent_echo_reply(outbound: &bm_stack::Outbound<'_>) -> (MessageType, u32, Vec<u8>) {
    let mut frame = outbound.frame().to_vec();
    let received = rx::accept(&mut frame).expect("our own frame validates");
    (
        received.header.message_type,
        received.header.seq_num,
        received.payload.to_vec(),
    )
}

#[test]
fn the_ping_we_emit_carries_a_truncated_node_id_and_its_own_counter() {
    let mut node = node();
    assert_eq!(node.ping_sequence(), 0);

    let outbound = node
        .ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"ping")
        .expect("a registered type is sent");
    let (message_type, header_seq, body) = sent_echo_reply(&outbound);

    assert_eq!(message_type, MessageType::ECHO_REQUEST);
    assert_eq!(
        header_seq, 0,
        "ping_init registers both types unsequenced, so the header number is zero"
    );

    let request = EchoRequest::decode(&body).unwrap();
    assert_eq!(request.target_node_id, PEER_ID);
    assert_eq!(request.id, OUR_PING_ID, "(uint16_t)node_id()");
    assert_eq!(request.seq_num, 0, "BCMP_SEQ, before the increment");
    assert_eq!(request.payload, b"ping");
    assert_eq!(node.expected_ping_payload(), Some(&b"ping"[..]));
    assert_eq!(node.ping_sequence(), 1);
}

/// `BCMP_SEQ` is `ping.c`'s own counter, not `packet.c`'s `message_count`: it
/// advances per ping while the header's sequence number stays at zero, and it
/// is the *body* field that carries it.
#[test]
fn successive_pings_advance_a_sequence_space_of_their_own() {
    let mut node = node();
    for expected in 0..4u16 {
        let outbound = node
            .ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, 0, b"x")
            .expect("sent");
        let (_, header_seq, body) = sent_echo_reply(&outbound);
        assert_eq!(header_seq, 0, "the header never counts");
        assert_eq!(EchoRequest::decode(&body).unwrap().seq_num, expected);
    }
    assert_eq!(node.ping_sequence(), 4);
    // And nothing was recorded as outstanding: unsequenced means packet.c
    // never hears about it.
    assert_eq!(node.registry().pending_len(), 0);
}

#[test]
fn an_echo_request_is_answered_with_the_same_bytes_back() {
    let mut node = node();
    let mut frame = echo_request_frame(NODE_ID, 0x1234, 0xFEDC, b"echo me");
    let owed = node.on_frame(0, 1, &mut frame);
    let outbound = owed.reply.expect("a ping to us must be answered");
    let (message_type, header_seq, body) = sent_echo_reply(&outbound);

    assert_eq!(message_type, MessageType::ECHO_REPLY);
    assert_eq!(
        header_seq, 0,
        "bcmp_send_ping_reply asks bcmp_tx to echo the body's seq_num, and \
         serialize throws it away because ping is unsequenced -- divergence #31"
    );

    let reply = EchoReply::decode(&body).unwrap();
    assert_eq!(reply.node_id, NODE_ID, "target_node_id becomes ours");
    assert_eq!(reply.id, 0x1234, "everything else rides back out unchanged");
    assert_eq!(reply.seq_num, 0xFEDC);
    assert_eq!(reply.payload, b"echo me");
}

#[test]
fn a_ping_to_every_node_is_answered_and_one_to_another_node_is_not() {
    let mut node = node();
    let mut broadcast = echo_request_frame(0, 1, 1, b"");
    assert!(node.on_frame(0, 1, &mut broadcast).reply.is_some());

    let mut elsewhere = echo_request_frame(PEER_ID, 1, 1, b"");
    assert!(node.on_frame(0, 1, &mut elsewhere).reply.is_none());
}

/// bm_core answers a ping by overwriting `target_node_id` in the *received*
/// buffer and casting it, but L2 has already taken its forwarding copy by
/// then. The port builds a fresh reply instead, so the relayed copy of a
/// global-multicast ping still says what the sender said.
#[test]
fn answering_a_ping_does_not_disturb_the_copy_being_relayed() {
    let mut node = node();
    let request = EchoRequest {
        target_node_id: 0,
        id: 7,
        seq_num: 7,
        payload: b"relay me",
    };
    let mut body = vec![0u8; request.encoded_len()];
    request.encode(&mut body).unwrap();
    let mut frame = peer_frame(MessageType::ECHO_REQUEST, &body, BmIpAddr::GLOBAL_MULTICAST);

    let owed = node.on_frame(0, 1, &mut frame);
    assert!(owed.reply.is_some(), "a broadcast ping is answered");
    let relayed = owed
        .relay
        .expect("global multicast is relayed")
        .frame()
        .to_vec();

    let mut copy = relayed;
    let received = rx::accept(&mut copy).expect("the relayed copy validates");
    let relayed_request = EchoRequest::decode(received.payload).unwrap();
    assert_eq!(
        relayed_request.target_node_id, 0,
        "the relayed copy must still carry the sender's target, not ours"
    );
    assert_eq!(relayed_request.payload, b"relay me");
}

#[test]
fn a_matching_echo_reply_is_reported_and_the_dispatch_is_reported_too() {
    let mut node = node();
    node.ping(1_000, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"ping")
        .expect("sent");

    let mut frame = echo_reply_frame(PEER_ID, OUR_PING_ID, 0, b"ping");
    let mut events = Vec::new();
    node.on_frame_with(1_042, 1, &mut frame, |e| events.push(seen(e)));

    assert_eq!(
        events,
        vec![
            Seen::Message {
                message_type: MessageType::ECHO_REPLY,
                seq_num: 0,
                source: PEER_ID,
            },
            Seen::EchoReply {
                source: PEER_ID,
                id: OUR_PING_ID,
                seq_num: 0,
                payload: b"ping".to_vec(),
                round_trip_ms: 42,
            },
        ],
        "the dispatch first, then ping.c's verdict on it"
    );
}

#[test]
fn a_reply_with_the_wrong_id_length_or_bytes_is_not_reported() {
    for (what, frame) in [
        ("id", echo_reply_frame(PEER_ID, OUR_PING_ID ^ 1, 0, b"ping")),
        ("length", echo_reply_frame(PEER_ID, OUR_PING_ID, 0, b"pin")),
        ("bytes", echo_reply_frame(PEER_ID, OUR_PING_ID, 0, b"pong")),
    ] {
        let mut node = node();
        node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"ping")
            .expect("sent");
        let mut frame = frame;
        let mut events = Vec::new();
        node.on_frame_with(0, 1, &mut frame, |e| events.push(seen(e)));
        assert!(
            !events.iter().any(|e| matches!(e, Seen::EchoReply { .. })),
            "a reply with the wrong {what} must not be accepted: {events:?}"
        );
    }
}

/// Divergence #30: the reply's own `node_id` and `seq_num` are never compared,
/// so a reply from a node that was never pinged answers just as well.
#[test]
fn a_reply_from_the_wrong_node_carrying_the_wrong_counter_still_answers() {
    let mut node = node();
    node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"ping")
        .expect("sent");

    let mut frame = echo_reply_frame(0xDEAD_BEEF_DEAD_BEEF, OUR_PING_ID, 999, b"ping");
    let mut events = Vec::new();
    node.on_frame_with(0, 1, &mut frame, |e| events.push(seen(e)));
    assert!(
        events.iter().any(|e| matches!(e, Seen::EchoReply { .. })),
        "the C compares neither field, and neither does the port: {events:?}"
    );
}

/// Divergence #32: nothing ever clears `EXPECTED_PAYLOAD`. A matched reply
/// does not, and no timer does, so the last ping's payload keeps answering.
#[test]
fn the_last_pings_payload_answers_for_as_long_as_the_node_runs() {
    let mut node = node();
    node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"ping")
        .expect("sent");

    for now_ms in [10u32, 100_000, 3_600_000] {
        let mut frame = echo_reply_frame(PEER_ID, OUR_PING_ID, 0, b"ping");
        let mut events = Vec::new();
        node.on_frame_with(now_ms, 1, &mut frame, |e| events.push(seen(e)));
        assert!(
            events.iter().any(|e| matches!(e, Seen::EchoReply { .. })),
            "still accepted at {now_ms} ms: {events:?}"
        );
    }
    assert_eq!(node.expected_ping_payload(), Some(&b"ping"[..]));
}

/// A payload-free ping leaves `EXPECTED_PAYLOAD` null, and the C then compares
/// nothing but the length and the id — so any empty reply answers it.
#[test]
fn a_payload_free_ping_is_answered_by_any_empty_reply() {
    let mut node = node();
    node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, 0, b"")
        .expect("sent");
    assert_eq!(node.expected_ping_payload(), None, "the C's NULL");

    let mut frame = echo_reply_frame(0, OUR_PING_ID, 4242, b"");
    let mut events = Vec::new();
    node.on_frame_with(0, 1, &mut frame, |e| events.push(seen(e)));
    assert!(events.iter().any(|e| matches!(e, Seen::EchoReply { .. })));
}

/// A second ping replaces the first's expectations, as
/// `bcmp_send_ping_request` does by freeing and reallocating: only one ping is
/// ever outstanding.
#[test]
fn a_second_ping_forgets_the_first() {
    let mut node = node();
    node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"first")
        .expect("sent");
    node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"second")
        .expect("sent");
    assert_eq!(node.expected_ping_payload(), Some(&b"second"[..]));

    let mut stale = echo_reply_frame(PEER_ID, OUR_PING_ID, 0, b"first");
    let mut events = Vec::new();
    node.on_frame_with(0, 1, &mut stale, |e| events.push(seen(e)));
    assert!(
        !events.iter().any(|e| matches!(e, Seen::EchoReply { .. })),
        "the first ping's reply no longer matches anything: {events:?}"
    );
}

/// The one place ping's behaviour here is a choice rather than a port: there is
/// no heap to grow the expectation slot, so a payload that will not fit is
/// refused outright — and refused *before* anything is disturbed.
#[test]
fn a_ping_longer_than_the_slot_is_refused_and_changes_nothing() {
    let mut node: Node<TestIdentity, SoftRtc, 4, 4, 8> =
        Node::new(TestIdentity, SoftRtc::new(), PORTS);
    node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"eight!!!")
        .expect("exactly the slot size fits");
    assert_eq!(node.ping_sequence(), 1);

    assert!(
        node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"nine more")
            .is_none(),
        "one byte past the slot"
    );
    assert_eq!(node.ping_sequence(), 1, "the counter did not advance");
    assert_eq!(
        node.expected_ping_payload(),
        Some(&b"eight!!!"[..]),
        "and the outstanding ping was left alone"
    );
}

#[test]
fn an_unregistered_echo_type_is_neither_sent_nor_answered() {
    let mut node = node();
    assert!(node.unregister(MessageType::ECHO_REQUEST));
    assert!(
        node.ping(0, &BmIpAddr::LINK_LOCAL_MULTICAST, PEER_ID, b"x")
            .is_none(),
        "the C's BmENODEV: serialize writes nothing and bcmp_tx sends nothing"
    );
    // But the counter and the payload moved first, exactly as they do in the C,
    // where BCMP_SEQ++ and the copy both happen before bcmp_tx is called.
    assert_eq!(node.ping_sequence(), 1);
    assert_eq!(node.expected_ping_payload(), Some(&b"x"[..]));

    let mut frame = echo_request_frame(NODE_ID, 1, 1, b"x");
    assert!(node.on_frame(0, 1, &mut frame).reply.is_none());
}

// ---------------------------------------------------------------------------
// System time -- card M2.
// ---------------------------------------------------------------------------

/// 2026-09-21T12:34:56.789Z, a reading a human can check.
const NOON_ISH: RtcTimeAndDate = RtcTimeAndDate {
    year: 2026,
    month: 9,
    day: 21,
    hour: 12,
    minute: 34,
    second: 56,
    ms: 789,
};

/// A node whose clock already reads [`NOON_ISH`].
fn node_with_a_clock() -> Node<TestIdentity, SoftRtc, 4> {
    Node::new(TestIdentity, SoftRtc::at(NOON_ISH), PORTS)
}

fn time_frame(message_type: MessageType, target_node_id: u64, utc_time_us: u64) -> Vec<u8> {
    let header = SystemTimeHeader {
        target_node_id,
        source_node_id: PEER_ID,
    };
    let body = match message_type {
        MessageType::SYSTEM_TIME_REQUEST => {
            let mut b = vec![0u8; SystemTimeRequest::LEN];
            SystemTimeRequest { header }.encode(&mut b).unwrap();
            b
        }
        MessageType::SYSTEM_TIME_RESPONSE => {
            let mut b = vec![0u8; SystemTimeResponse::LEN];
            SystemTimeResponse {
                header,
                utc_time_us,
            }
            .encode(&mut b)
            .unwrap();
            b
        }
        _ => {
            let mut b = vec![0u8; SystemTimeSet::LEN];
            SystemTimeSet {
                header,
                utc_time_us,
            }
            .encode(&mut b)
            .unwrap();
            b
        }
    };
    peer_frame(message_type, &body, BmIpAddr::LINK_LOCAL_MULTICAST)
}

/// The body of whatever the node replied, parsed back as a `0x11`.
fn response_body(
    node: &mut Node<TestIdentity, SoftRtc, 4>,
    frame: &mut [u8],
) -> SystemTimeResponse {
    let owed = node.on_frame(1000, 1, frame);
    assert!(owed.forward.is_none(), "addressed to us, so not forwarded");
    let mut reply = owed
        .reply
        .expect("this message calls for a response")
        .frame()
        .to_vec();
    let received = rx::accept(&mut reply).expect("our own reply validates");
    assert_eq!(
        received.header.message_type,
        MessageType::SYSTEM_TIME_RESPONSE
    );
    assert_eq!(
        received.dst,
        BmIpAddr::LINK_LOCAL_MULTICAST,
        "bcmp_time_send_response always passes multicast_ll_addr"
    );
    assert_eq!(
        received.header.seq_num, 0,
        "time.c registers it unsequenced"
    );
    SystemTimeResponse::decode(received.payload).expect("decodes")
}

#[test]
fn a_time_request_for_us_is_answered_from_the_clock() {
    let mut node = node_with_a_clock();
    let mut frame = time_frame(MessageType::SYSTEM_TIME_REQUEST, NODE_ID, 0);
    let response = response_body(&mut node, &mut frame);

    assert_eq!(response.header.source_node_id, NODE_ID);
    assert_eq!(
        response.header.target_node_id, PEER_ID,
        "the body's source_node_id is answered, not the frame's address"
    );
    assert_eq!(response.utc_time_us, NOON_ISH.to_utc_micros());
}

#[test]
fn a_time_request_for_us_goes_unanswered_when_the_clock_is_unset() {
    let mut node = node();
    let mut frame = time_frame(MessageType::SYSTEM_TIME_REQUEST, NODE_ID, 0);
    let owed = node.on_frame(1000, 1, &mut frame);
    assert!(owed.is_empty(), "bm_rtc_get failing means silence");
}

/// Divergence #27: `target_node_id == 0` reaches the switch and is dropped by
/// an inner exact-match test, so broadcasting a time request asks nobody.
#[test]
fn a_broadcast_time_request_is_silently_dropped() {
    let mut node = node_with_a_clock();
    let mut frame = time_frame(MessageType::SYSTEM_TIME_REQUEST, 0, 0);
    let owed = node.on_frame(1000, 1, &mut frame);
    assert!(owed.reply.is_none(), "nobody answers a broadcast 0x10");
    assert!(owed.forward.is_none(), "and zero is not somebody else");

    // The application still hears it, which is more than a C node offers.
    let mut seen = 0;
    let mut frame = time_frame(MessageType::SYSTEM_TIME_REQUEST, 0, 0);
    node.on_frame_with(1000, 1, &mut frame, |event| {
        if let Event::Message { message_type, .. } = event {
            assert_eq!(message_type, MessageType::SYSTEM_TIME_REQUEST);
            seen += 1;
        }
    });
    assert_eq!(seen, 1);
}

/// And the other half of #27: a broadcast *set* is honoured by everybody.
#[test]
fn a_broadcast_time_set_is_honoured_and_answered() {
    let utc_time_us = 1_789_948_800_250_999;
    for target in [0, NODE_ID] {
        let mut node = node_with_a_clock();
        let mut frame = time_frame(MessageType::SYSTEM_TIME_SET, target, utc_time_us);
        let response = response_body(&mut node, &mut frame);
        assert_eq!(
            response.utc_time_us, utc_time_us,
            "target {target:#x}: the echo is the requested value, to the microsecond"
        );
        assert_eq!(
            node.rtc().get().unwrap(),
            RtcTimeAndDate::from_utc_micros(utc_time_us),
            "target {target:#x}: and the clock moved"
        );
        assert_eq!(
            node.rtc().get().unwrap().ms,
            250,
            "target {target:#x}: keeping only the millisecond"
        );
    }
}

/// A clock that refuses to be set leaves the node silent, as `bm_rtc_set`
/// returning an error does in the C.
#[test]
fn a_set_that_the_clock_refuses_is_not_answered() {
    let mut node =
        Node::<TestIdentity, SoftRtc, 4>::new(TestIdentity, SoftRtc::read_only(NOON_ISH), PORTS);
    let mut frame = time_frame(MessageType::SYSTEM_TIME_SET, NODE_ID, 1_000_000);
    let owed = node.on_frame(1000, 1, &mut frame);
    assert!(owed.reply.is_none());
    assert_eq!(node.rtc().get(), Some(NOON_ISH), "and nothing moved");
}

/// A `0x11` addressed to us is logged by the C and nothing more. Here it
/// reaches the application as an ordinary message and produces no frame.
#[test]
fn a_time_response_for_us_is_reported_and_not_answered() {
    let mut node = node_with_a_clock();
    let mut frame = time_frame(MessageType::SYSTEM_TIME_RESPONSE, NODE_ID, 42_000_000);
    let mut seen = None;
    let owed = node.on_frame_with(1000, 1, &mut frame, |event| {
        if let Event::Message {
            message_type,
            payload,
            ..
        } = event
        {
            seen = Some((message_type, SystemTimeResponse::decode(payload).unwrap()));
        }
    });
    assert!(owed.is_empty(), "a response provokes nothing");
    let (message_type, response) = seen.expect("the application hears it");
    assert_eq!(message_type, MessageType::SYSTEM_TIME_RESPONSE);
    assert_eq!(response.utc_time_us, 42_000_000);
    assert_eq!(
        node.rtc().get(),
        Some(NOON_ISH),
        "and a C node does not adopt the time it was told"
    );
}

/// A time message for a third node is re-flooded out every other port, as a
/// fresh frame from this node — `bcmp_ll_forward`.
#[test]
fn a_time_message_for_another_node_is_re_flooded() {
    const THIRD_ID: u64 = 0x0000_0000_0BAD_F00D;

    for message_type in [
        MessageType::SYSTEM_TIME_REQUEST,
        MessageType::SYSTEM_TIME_RESPONSE,
        MessageType::SYSTEM_TIME_SET,
    ] {
        let mut node = node_with_a_clock();
        let mut frame = time_frame(message_type, THIRD_ID, 7_000_000);
        let original = frame.clone();

        let owed = node.on_frame(1000, 1, &mut frame);
        assert!(owed.reply.is_none(), "{message_type:?}: not ours to answer");
        assert!(
            owed.relay.is_none(),
            "{message_type:?}: FF02::1 is consumed by L2, not relayed"
        );
        let reflood = owed.forward.expect("{message_type:?}: must be re-flooded");

        // The range is the BCMP header and body, exactly as they arrived.
        assert_eq!(reflood.start, BCMP_HEADER_OFFSET);
        assert_eq!(reflood.end, original.len());
        assert_eq!(
            reflood.ingress_port, 1,
            "the nibble the sender's L2 stamped"
        );

        let mut phy = MockPhy::new(PORTS, Vec::new());
        block_on(node.reflood(&mut phy, reflood, &frame)).unwrap();
        assert_eq!(phy.sent.len(), 1, "{message_type:?}: one copy, on port 2");
        assert_eq!(phy.sent[0].egress, Egress::Port(2));

        let mut forwarded = phy.sent[0].frame.clone();
        let received = rx::accept(&mut forwarded).expect("a peer accepts the re-flood");
        assert_eq!(received.header.message_type, message_type);
        assert_eq!(
            received.src.to_node_id(),
            NODE_ID,
            "{message_type:?}: the forwarder claims it -- divergence #23"
        );
        let header = SystemTimeHeader::decode(received.payload).unwrap();
        assert_eq!(header.target_node_id, THIRD_ID, "{message_type:?}");
        assert_eq!(
            header.source_node_id, PEER_ID,
            "{message_type:?}: the originator survives only in the body"
        );

        assert_eq!(
            node.rtc().get(),
            Some(NOON_ISH),
            "{message_type:?}: a forwarded set is not applied on the way past"
        );
    }
}

/// A body too short for the C to read without going out of bounds is refused
/// here rather than guessed at. See divergence #14 for why that is a domain
/// limit and not a behaviour to reproduce.
#[test]
fn a_time_message_shorter_than_its_header_is_dropped() {
    for len in 0..SystemTimeHeader::LEN {
        let mut frame = peer_frame(
            MessageType::SYSTEM_TIME_REQUEST,
            &vec![0u8; len],
            BmIpAddr::LINK_LOCAL_MULTICAST,
        );
        let mut node = node_with_a_clock();
        assert!(
            node.on_frame(1000, 1, &mut frame).is_empty(),
            "a {len}-byte body must not be acted on"
        );
    }
    // A set needs the whole 24: the header alone gets it past the forwarding
    // test and then falls short of `utc_time_us`.
    for len in SystemTimeHeader::LEN..SystemTimeSet::LEN {
        let mut body = vec![0u8; len];
        body[..8].copy_from_slice(&NODE_ID.to_le_bytes());
        let mut frame = peer_frame(
            MessageType::SYSTEM_TIME_SET,
            &body,
            BmIpAddr::LINK_LOCAL_MULTICAST,
        );
        let mut node = node_with_a_clock();
        assert!(
            node.on_frame(1000, 1, &mut frame).is_empty(),
            "a {len}-byte set must not be acted on"
        );
        assert_eq!(node.rtc().get(), Some(NOON_ISH));
    }
}

/// The requester half, `bcmp_time_get_time` and `bcmp_time_set_time`: both go
/// to `FF02::1` with a sequence number of zero.
#[test]
fn the_requests_we_issue_carry_what_the_c_puts_in_them() {
    let mut node = node_with_a_clock();
    let utc_time_us = 1_789_948_800_250_999;

    let mut frame = node
        .request_system_time(1000, PEER_ID)
        .expect("a registered type is sent")
        .frame()
        .to_vec();
    let received = rx::accept(&mut frame).expect("validates");
    assert_eq!(
        received.header.message_type,
        MessageType::SYSTEM_TIME_REQUEST
    );
    assert_eq!(received.header.seq_num, 0);
    assert_eq!(received.dst, BmIpAddr::LINK_LOCAL_MULTICAST);
    assert_eq!(received.payload.len(), SystemTimeRequest::LEN);
    let request = SystemTimeRequest::decode(received.payload).unwrap();
    assert_eq!(request.header.target_node_id, PEER_ID);
    assert_eq!(request.header.source_node_id, NODE_ID);

    let mut frame = node
        .set_system_time(1000, 0, utc_time_us)
        .expect("a registered type is sent")
        .frame()
        .to_vec();
    let received = rx::accept(&mut frame).expect("validates");
    assert_eq!(received.header.message_type, MessageType::SYSTEM_TIME_SET);
    assert_eq!(received.payload.len(), SystemTimeSet::LEN);
    let set = SystemTimeSet::decode(received.payload).unwrap();
    assert_eq!(
        set.header.target_node_id, 0,
        "a broadcast set is meaningful"
    );
    assert_eq!(set.header.source_node_id, NODE_ID);
    assert_eq!(set.utc_time_us, utc_time_us);

    // And our own clock is untouched by asking somebody else to change theirs.
    assert_eq!(node.rtc().get(), Some(NOON_ISH));
}

/// The whole loop: a `0x10` arrives on the PHY and a `0x11` goes back out
/// without anything synchronous being driven by hand.
#[test]
fn the_run_loop_answers_a_time_request() {
    let request = time_frame(MessageType::SYSTEM_TIME_REQUEST, NODE_ID, 0);
    let mut phy = MockPhy::new(
        PORTS,
        vec![Script::Receive {
            port: 1,
            frame: request,
        }],
    );
    let mut node = node_with_a_clock();
    block_on(node.run(&mut phy));

    let responses: Vec<&Sent> = phy
        .sent
        .iter()
        .filter(|sent| {
            let mut copy = sent.frame.clone();
            rx::accept(&mut copy)
                .map(|r| r.header.message_type == MessageType::SYSTEM_TIME_RESPONSE)
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        responses.len(),
        usize::from(PORTS),
        "a link-local response is stamped once per port"
    );

    let mut copy = responses[0].frame.clone();
    let received = rx::accept(&mut copy).unwrap();
    let response = SystemTimeResponse::decode(received.payload).unwrap();
    assert_eq!(response.utc_time_us, NOON_ISH.to_utc_micros());
}

// ---------------------------------------------------------------------------
// Device-information replies -- card M3.
// ---------------------------------------------------------------------------

/// A heartbeat frame from `node_id` rather than from the peer.
fn heartbeat_frame_from(node_id: u64, uptime_us: u64) -> Vec<u8> {
    let mut frame = heartbeat_frame(uptime_us);
    frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
        .copy_from_slice(&addr::nodeid_to_ip(LINK_LOCAL_PREFIX, node_id).0);
    // The source address is inside the checksum, so it has to be rebuilt.
    let body = frame[BCMP_HEADER_OFFSET + BCMP_HEADER_LEN..].to_vec();
    tx::serialize(&mut frame, MessageType::HEARTBEAT, 0, &body).unwrap();
    frame
}

/// A device-info reply frame from the peer, claiming `node_id`.
fn info_reply_frame(node_id: u64, version: &[u8], name: &[u8]) -> Vec<u8> {
    let reply = DeviceInfoReply {
        info: DeviceInfo {
            node_id,
            vendor_id: 0x1234,
            product_id: 0x5678,
            serial_num: *b"peer-serial-0001",
            git_sha: 0xABCD_EF01,
            ver_major: 9,
            ver_minor: 8,
            ver_rev: 7,
            ver_hw: 6,
        },
        version_string: version,
        device_name: name,
    };
    let mut body = vec![0u8; reply.encoded_len()];
    reply.encode(&mut body).unwrap();
    peer_frame(
        MessageType::DEVICE_INFO_REPLY,
        &body,
        BmIpAddr::LINK_LOCAL_MULTICAST,
    )
}

/// The heartbeat asks, the reply answers, and the node keeps the answer.
#[test]
fn a_reply_to_the_request_a_heartbeat_provoked_is_cached() {
    let mut node = node();
    let mut heartbeat = heartbeat_frame(1_000_000);
    assert!(
        node.on_frame(0, 1, &mut heartbeat).reply.is_some(),
        "a new neighbour is asked for its information"
    );
    assert!(node.info_requests().contains(PEER_ID));
    assert!(node.device_info(PEER_ID).is_none());

    let mut reply = info_reply_frame(PEER_ID, b"1.2.3", b"peer");
    assert!(
        node.on_frame(10, 1, &mut reply).is_empty(),
        "a reply is answered with nothing"
    );

    let cached = node.device_info(PEER_ID).expect("the reply was kept");
    assert_eq!(cached.info.node_id, PEER_ID);
    assert_eq!(cached.info.git_sha, 0xABCD_EF01);
    assert_eq!(cached.version_string, b"1.2.3");
    assert_eq!(cached.device_name, b"peer");
    assert!(
        !node.info_requests().contains(PEER_ID),
        "and the request is no longer outstanding"
    );
}

/// `ll_get_item` misses, so `bcmp_process_info_reply` returns before it reaches
/// the neighbour table.
#[test]
fn a_reply_nothing_asked_for_is_dropped() {
    let mut node = node();
    let mut heartbeat = heartbeat_frame(1_000_000);
    node.on_frame(0, 1, &mut heartbeat);
    let mut first = info_reply_frame(PEER_ID, b"1.2.3", b"peer");
    node.on_frame(10, 1, &mut first);

    // Nothing asked a second time, so nothing is taken from the second reply.
    let mut second = info_reply_frame(PEER_ID, b"9.9.9", b"other");
    node.on_frame(20, 1, &mut second);
    let cached = node.device_info(PEER_ID).unwrap();
    assert_eq!(cached.version_string, b"1.2.3");
    assert_eq!(cached.device_name, b"peer");
}

/// The cache belongs to the neighbour table: a reply from a node that is not a
/// neighbour is matched, consumed and kept nowhere.
#[test]
fn a_reply_from_a_node_that_is_not_a_neighbour_is_kept_nowhere() {
    let mut node = node();
    node.request_device_info(0, PEER_ID, InfoRequestKind::Cache)
        .expect("a registered type is sent");
    assert!(node.info_requests().contains(PEER_ID));

    let mut reply = info_reply_frame(PEER_ID, b"1.2.3", b"peer");
    node.on_frame(10, 1, &mut reply);
    assert!(node.device_info(PEER_ID).is_none());
    assert!(
        !node.info_requests().contains(PEER_ID),
        "but the request is consumed all the same"
    );
}

/// A request carrying a callback reports and caches nothing.
#[test]
fn a_reported_request_reaches_the_application_instead_of_the_cache() {
    let mut node = node();
    let mut heartbeat = heartbeat_frame(1_000_000);
    node.on_frame(0, 1, &mut heartbeat);
    // Consume the request the heartbeat made, so the next reply answers ours.
    let mut first = info_reply_frame(PEER_ID, b"1.2.3", b"peer");
    node.on_frame(10, 1, &mut first);

    node.request_device_info(20, PEER_ID, InfoRequestKind::Report)
        .expect("a registered type is sent");
    let mut reply = info_reply_frame(PEER_ID, b"4.5.6", b"renamed");
    let mut reported = Vec::new();
    node.on_frame_with(30, 1, &mut reply, |event| {
        if let Event::DeviceInfo { source, reply } = event {
            reported.push((source, reply.version_string.to_vec()));
        }
    });

    assert_eq!(reported, [(PEER_ID, b"4.5.6".to_vec())]);
    let cached = node.device_info(PEER_ID).unwrap();
    assert_eq!(
        cached.version_string, b"1.2.3",
        "the callback branch leaves the cache alone"
    );
}

/// The restart path asks about `neighbor->info.node_id`, which is zero until a
/// reply has been cached. Divergence #34.
#[test]
fn a_restart_asks_about_the_node_id_the_cache_holds() {
    fn target_of(outbound: &bm_stack::Outbound<'_>) -> u64 {
        let mut frame = outbound.frame().to_vec();
        let received = rx::accept(&mut frame).unwrap();
        assert_eq!(
            received.header.message_type,
            MessageType::DEVICE_INFO_REQUEST
        );
        DeviceInfoRequest::decode(received.payload)
            .unwrap()
            .target_node_id
    }

    let mut node = node();
    let mut heartbeat = heartbeat_frame(9_000_000);
    let owed = node.on_frame(0, 1, &mut heartbeat);
    assert_eq!(
        target_of(&owed.reply.unwrap()),
        PEER_ID,
        "bcmp_update_neighbor asks about the node it was given"
    );

    // Uptime goes backwards with nothing cached: the C reads a zeroed
    // `info.node_id` and broadcasts.
    let mut restart = heartbeat_frame(1_000);
    let owed = node.on_frame(10, 1, &mut restart);
    assert_eq!(
        target_of(&owed.reply.unwrap()),
        0,
        "nothing is cached, so the request names nobody"
    );

    // Cache something, and the same restart names the peer.
    let mut reply = info_reply_frame(PEER_ID, b"1.2.3", b"peer");
    node.on_frame(20, 1, &mut reply);
    let mut again = heartbeat_frame(500);
    let owed = node.on_frame(30, 1, &mut again);
    assert_eq!(target_of(&owed.reply.unwrap()), PEER_ID);
}

/// One neighbour per port, and the information goes with the entry.
#[test]
fn evicting_a_neighbour_forgets_what_it_reported() {
    const OTHER_ID: u64 = 0x0000_0000_55AA_0022;

    let mut node = node();
    let mut heartbeat = heartbeat_frame(1_000_000);
    node.on_frame(0, 1, &mut heartbeat);
    let mut reply = info_reply_frame(PEER_ID, b"1.2.3", b"peer");
    node.on_frame(10, 1, &mut reply);
    assert!(node.device_info(PEER_ID).is_some());

    // A different node takes port 1.
    let mut other = heartbeat_frame_from(OTHER_ID, 1_000_000);
    node.on_frame(20, 1, &mut other);

    assert!(node.neighbors().find(PEER_ID).is_none(), "it lost the port");
    assert!(
        node.device_info(PEER_ID).is_none(),
        "and its strings went with it"
    );
}

/// A request for an unregistered type is never sent, and leaves the list as it
/// found it.
#[test]
fn an_unregistered_request_type_records_nothing() {
    let mut node = node();
    assert!(node.unregister(MessageType::DEVICE_INFO_REQUEST));
    assert!(
        node.request_device_info(0, PEER_ID, InfoRequestKind::Cache)
            .is_none()
    );
    assert!(node.info_requests().is_empty());
}
