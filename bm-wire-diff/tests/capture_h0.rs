//! What a deployed C node puts on the wire, from
//! `testdata/hello-pub-card-h0.pcap`.
//!
//! Captured with `bm_l2_register_pcap_callback` on a dev kit running
//! `bm_protocol`'s hello-world app, modified to call `spotter_log`,
//! `spotter_log_console` and `spotter_tx_data` every 10 s. `lwipopts.h`:
//! `CHECKSUM_GEN_UDP` 1, `CHECKSUM_CHECK_UDP` 0, `UDP_TTL` 255, no multicast
//! TTL option. The callback sees a received frame before
//! `bm_l2_policy_rx_apply` and a transmitted one as it goes to the device, so
//! no frame here carries an ingress nibble.
//!
//! | Node | Id | Role |
//! |---|---|---|
//! | dev kit | `0b54ccce5c7978bf` | the capturing node, two ports |
//! | bm soft module | `e5d14eea4fc2db6b` | temperature sensor |
//! | Spotter bridge | `e4ce8ae3662e97df` | issued `bm info 0` during the run |
//!
//! These tests pin the header fields where `network/bm_lwip.c` and lwIP differ
//! from `network/bm_linux.c` (divergence #70).

use std::collections::HashMap;

use bm_wire::addr::{self, LINK_LOCAL_PREFIX, UNIQUE_LOCAL_PREFIX};
use bm_wire::checksum::ipv6_pseudo_checksum;
use bm_wire::frame::{
    ETHERNET_DESTINATION_OFFSET, ETHERNET_SRC_OFFSET, ETHERNET_TYPE_IPV6, IP_PROTO_BCMP,
    IP_PROTO_UDP, IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_HOP_LIMIT_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET,
    IPV6_SOURCE_ADDRESS_OFFSET, IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET,
    MIN_FRAME_WITH_ADDRESSES, UDP_CHECKSUM_OFFSET, UDP_DESTINATION_PORT_OFFSET, UDP_LENGTH_OFFSET,
    UDP_SOURCE_PORT_OFFSET, ethernet_type,
};
use bm_wire::util::BmIpAddr;
use bm_wire_diff::pcap::{self, Record};

const CAPTURE: &[u8] = include_bytes!("../testdata/hello-pub-card-h0.pcap");

const DEV_KIT: u64 = 0x0b54_ccce_5c79_78bf;
const SOFT_MODULE: u64 = 0xe5d1_4eea_4fc2_db6b;
const BRIDGE: u64 = 0xe4ce_8ae3_662e_97df;

/// `BM_MIDDLEWARE_PORT`, pub/sub's source and destination port.
const MIDDLEWARE_PORT: u16 = 4321;

fn frames() -> Vec<Record<'static>> {
    pcap::records(CAPTURE)
}

fn addr_at(frame: &[u8], at: usize) -> BmIpAddr {
    BmIpAddr(frame[at..at + 16].try_into().unwrap())
}

fn src(frame: &[u8]) -> BmIpAddr {
    addr_at(frame, IPV6_SOURCE_ADDRESS_OFFSET)
}

fn dst(frame: &[u8]) -> BmIpAddr {
    addr_at(frame, IPV6_DESTINATION_ADDRESS_OFFSET)
}

fn u16_at(frame: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([frame[at], frame[at + 1]])
}

fn udp(frame: &[u8]) -> bool {
    frame[IPV6_NEXT_HEADER_OFFSET] == IP_PROTO_UDP
}

/// `src` with the ports byte cleared, as `nodeid_to_ip` would build it.
fn without_ports(addr: &BmIpAddr) -> BmIpAddr {
    let mut a = *addr;
    a.0[IPV6_INGRESS_EGRESS_PORTS_OFFSET - IPV6_SOURCE_ADDRESS_OFFSET] = 0;
    a
}

#[test]
fn the_capture_holds_three_nodes_udp_and_bcmp() {
    let frames = frames();
    assert_eq!(frames.len(), 2403);
    let mut nodes: Vec<u64> = frames.iter().map(|r| src(r.frame).to_node_id()).collect();
    nodes.sort_unstable();
    nodes.dedup();
    assert_eq!(nodes, [DEV_KIT, BRIDGE, SOFT_MODULE]);
    assert_eq!(frames.iter().filter(|r| udp(r.frame)).count(), 2300);
    assert_eq!(
        frames
            .iter()
            .filter(|r| r.frame[IPV6_NEXT_HEADER_OFFSET] == IP_PROTO_BCMP)
            .count(),
        103
    );
}

/// UDP and BCMP alike: version 6, zero traffic class and flow label, hop limit
/// 255 where `bm_linux.c` writes 64, the destination MAC `bm_linux.c` derives,
/// and a source MAC it does not.
#[test]
fn every_frame_has_hop_limit_255_and_the_device_c_source_mac() {
    for (i, r) in frames().iter().enumerate() {
        let f = r.frame;
        assert!(f.len() >= MIN_FRAME_WITH_ADDRESSES, "frame {i}");
        assert_eq!(ethernet_type(f), Some(ETHERNET_TYPE_IPV6), "frame {i}");
        assert_eq!(
            f[IPV6_VERSION_TRAFFIC_CLASS_FLOW_LABEL_OFFSET..][..4],
            [0x60, 0, 0, 0],
            "frame {i}"
        );
        assert_eq!(
            usize::from(u16_at(f, IPV6_PAYLOAD_LENGTH_OFFSET)),
            f.len() - MIN_FRAME_WITH_ADDRESSES,
            "frame {i}"
        );
        assert_eq!(f[IPV6_HOP_LIMIT_OFFSET], 255, "frame {i}");
        assert!(addr::is_multicast(&dst(f)), "frame {i}");
        assert_eq!(
            f[ETHERNET_DESTINATION_OFFSET..][..6],
            addr::multicast_mac_from_ipv6(&dst(f)),
            "frame {i}"
        );
        assert_eq!(
            f[ETHERNET_SRC_OFFSET..][..6],
            addr::mac_address(src(f).to_node_id()),
            "frame {i}"
        );
    }
}

/// lwIP picks `fd00::<id>` for `ff03::1`, where `bm_linux.c` sends from
/// `fe80::<id>`. No UDP frame carries a port nibble, and every checksum is
/// present and valid as captured. `bm_l2_policy_rx_apply` then writes the
/// ingress nibble without patching the checksum, which lwIP accepts only
/// because `CHECKSUM_CHECK_UDP` is 0.
#[test]
fn udp_is_from_the_unique_local_address_with_a_valid_checksum() {
    for (i, r) in frames().iter().filter(|r| udp(r.frame)).enumerate() {
        let f = r.frame;
        let (s, d) = (src(f), dst(f));
        assert_eq!(d, BmIpAddr::GLOBAL_MULTICAST, "udp frame {i}");
        assert_eq!(
            s,
            addr::nodeid_to_ip(UNIQUE_LOCAL_PREFIX, s.to_node_id()),
            "udp frame {i}"
        );
        assert_eq!(
            u16_at(f, UDP_SOURCE_PORT_OFFSET),
            MIDDLEWARE_PORT,
            "udp frame {i}"
        );
        assert_eq!(
            u16_at(f, UDP_DESTINATION_PORT_OFFSET),
            MIDDLEWARE_PORT,
            "udp frame {i}"
        );
        let body = &f[UDP_SOURCE_PORT_OFFSET..];
        assert_eq!(
            usize::from(u16_at(f, UDP_LENGTH_OFFSET)),
            body.len(),
            "udp frame {i}"
        );
        assert_ne!(u16_at(f, UDP_CHECKSUM_OFFSET), 0, "udp frame {i}");
        assert_eq!(
            ipv6_pseudo_checksum(&s, &d, IP_PROTO_UDP, body),
            0,
            "udp frame {i}"
        );
    }
}

/// `bm_wire::udp` builds every UDP frame in the capture, from all three nodes,
/// byte for byte from its source node id, ports and payload: source address,
/// source MAC, hop limit and checksum included.
#[test]
fn bm_wire_udp_rebuilds_every_udp_frame() {
    for (i, r) in frames().iter().filter(|r| udp(r.frame)).enumerate() {
        let f = r.frame;
        let d = dst(f);
        let payload = &f[bm_wire::udp::PAYLOAD_OFFSET..];
        let mut buf = vec![0u8; f.len()];
        let len = bm_wire::udp::build(
            &mut buf,
            &bm_wire::udp::source_address(src(f).to_node_id(), &d),
            &d,
            u16_at(f, UDP_SOURCE_PORT_OFFSET),
            u16_at(f, UDP_DESTINATION_PORT_OFFSET),
            payload,
        )
        .unwrap();
        assert_eq!(&buf[..len], f, "udp frame {i}");

        let datagram = bm_wire::udp::accept(f).unwrap();
        assert_eq!(datagram.source, src(f).to_node_id(), "udp frame {i}");
        assert_eq!(datagram.payload, payload, "udp frame {i}");
    }
}

/// BCMP keeps `fe80::<id>`. `ff03::1` goes out once with no port nibble;
/// `ff02::1` once per port with only the egress nibble set.
#[test]
fn bcmp_is_from_the_link_local_address() {
    let mut egress: HashMap<u64, Vec<u8>> = HashMap::new();
    for (i, r) in frames().iter().filter(|r| !udp(r.frame)).enumerate() {
        let f = r.frame;
        let s = src(f);
        assert_eq!(
            without_ports(&s),
            addr::nodeid_to_ip(LINK_LOCAL_PREFIX, s.to_node_id()),
            "bcmp frame {i}"
        );
        let ports = f[IPV6_INGRESS_EGRESS_PORTS_OFFSET];
        if dst(f) == BmIpAddr::GLOBAL_MULTICAST {
            assert_eq!(ports, 0, "bcmp frame {i}");
        } else {
            assert_eq!(dst(f), BmIpAddr::LINK_LOCAL_MULTICAST, "bcmp frame {i}");
            assert_eq!(ports >> 4, 0, "bcmp frame {i}");
            egress.entry(s.to_node_id()).or_default().push(ports);
        }
    }
    let mut seen: Vec<(u64, Vec<u8>)> = egress
        .into_iter()
        .map(|(id, mut p)| {
            p.sort_unstable();
            p.dedup();
            (id, p)
        })
        .collect();
    seen.sort_unstable();
    assert_eq!(
        seen,
        [
            (DEV_KIT, vec![1, 2]),
            (BRIDGE, vec![1]),
            (SOFT_MODULE, vec![2])
        ]
    );
}

/// The dev kit's own publications go to all ports as one frame
/// (`send_global_multicast_packet` with `device_all_ports`). A neighbour's is
/// captured twice: on receipt, then as L2's one-port relay, byte for byte the
/// same, because `bm_l2_policy_prepare_forwarded_copy` clears nibbles the
/// sender never set.
#[test]
fn own_publications_go_out_once_and_a_neighbours_are_relayed_unchanged() {
    let frames = frames();
    let mut count: HashMap<&[u8], Vec<u64>> = HashMap::new();
    for r in frames.iter().filter(|r| udp(r.frame)) {
        count.entry(r.frame).or_default().push(r.t_us);
    }
    let (mut own, mut relayed) = (0, 0);
    for (f, times) in &count {
        if src(f).to_node_id() == DEV_KIT {
            assert_eq!(times.len(), 1);
            own += 1;
        } else {
            assert_eq!(times.len(), 2);
            assert!(
                times[1] - times[0] <= 10_000,
                "relay {} us late",
                times[1] - times[0]
            );
            relayed += 1;
        }
    }
    // 17 ten-second rounds of spotter_log, spotter_log_console,
    // spotter_tx_data, plus the dev kit's `bm info 0` replies.
    assert_eq!(own, 60);
    assert_eq!(relayed, 1120);
}
