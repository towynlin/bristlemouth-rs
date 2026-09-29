use super::*;

/// Card H0's dev kit, the node that sent the frames below.
const DEV_KIT: u64 = 0x0b54_ccce_5c79_78bf;
/// `BM_MIDDLEWARE_PORT`, pub/sub's source and destination port.
const MIDDLEWARE_PORT: u16 = 4321;

// Frames from `bm-wire-diff/testdata/hello-pub-card-h0.pcap`, numbered from 0
// in file order: the dev kit's first round of publications.

/// Frame 84: `spotter_log` to `spotter/fprintf`.
const FRAME_84: [u8; 141] = [
    0x33, 0x33, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x5c, 0x79, 0x78, 0xbf, 0x86, 0xdd, 0x60, 0x00,
    0x00, 0x00, 0x00, 0x57, 0x11, 0xff, 0xfd, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x54,
    0xcc, 0xce, 0x5c, 0x79, 0x78, 0xbf, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x10, 0xe1, 0x10, 0xe1, 0x00, 0x57, 0x4f, 0x69, 0x00, 0x00,
    0x0f, 0x01, 0x02, 0x73, 0x70, 0x6f, 0x74, 0x74, 0x65, 0x72, 0x2f, 0x66, 0x70, 0x72, 0x69, 0x6e,
    0x74, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x24, 0x00, 0x01, 0x68,
    0x65, 0x6c, 0x6c, 0x6f, 0x2e, 0x6c, 0x6f, 0x67, 0x73, 0x70, 0x6f, 0x74, 0x74, 0x65, 0x72, 0x5f,
    0x6c, 0x6f, 0x67, 0x20, 0x66, 0x6f, 0x72, 0x20, 0x63, 0x61, 0x72, 0x64, 0x20, 0x48, 0x30, 0x2c,
    0x20, 0x63, 0x6f, 0x75, 0x6e, 0x74, 0x65, 0x72, 0x3d, 0x31, 0x30, 0x30, 0x00,
];

/// Frame 85: `spotter_log_console` to `spotter/printf`.
const FRAME_85: [u8; 139] = [
    0x33, 0x33, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x5c, 0x79, 0x78, 0xbf, 0x86, 0xdd, 0x60, 0x00,
    0x00, 0x00, 0x00, 0x55, 0x11, 0xff, 0xfd, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x54,
    0xcc, 0xce, 0x5c, 0x79, 0x78, 0xbf, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x10, 0xe1, 0x10, 0xe1, 0x00, 0x55, 0x4f, 0x08, 0x00, 0x00,
    0x0e, 0x01, 0x02, 0x73, 0x70, 0x6f, 0x74, 0x74, 0x65, 0x72, 0x2f, 0x70, 0x72, 0x69, 0x6e, 0x74,
    0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x01, 0x73, 0x70,
    0x6f, 0x74, 0x74, 0x65, 0x72, 0x5f, 0x6c, 0x6f, 0x67, 0x5f, 0x63, 0x6f, 0x6e, 0x73, 0x6f, 0x6c,
    0x65, 0x20, 0x66, 0x6f, 0x72, 0x20, 0x63, 0x61, 0x72, 0x64, 0x20, 0x48, 0x30, 0x2c, 0x20, 0x63,
    0x6f, 0x75, 0x6e, 0x74, 0x65, 0x72, 0x3d, 0x31, 0x30, 0x30, 0x00,
];

/// Frame 86: `spotter_tx_data` to `spotter/transmit-data`.
const FRAME_86: [u8; 93] = [
    0x33, 0x33, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x5c, 0x79, 0x78, 0xbf, 0x86, 0xdd, 0x60, 0x00,
    0x00, 0x00, 0x00, 0x27, 0x11, 0xff, 0xfd, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x54,
    0xcc, 0xce, 0x5c, 0x79, 0x78, 0xbf, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x10, 0xe1, 0x10, 0xe1, 0x00, 0x27, 0x05, 0xac, 0x00, 0x00,
    0x15, 0x01, 0x02, 0x73, 0x70, 0x6f, 0x74, 0x74, 0x65, 0x72, 0x2f, 0x74, 0x72, 0x61, 0x6e, 0x73,
    0x6d, 0x69, 0x74, 0x2d, 0x64, 0x61, 0x74, 0x61, 0x02, 0x64, 0x00, 0x00, 0x00,
];

fn rebuild<'a>(frame: &[u8], buf: &'a mut [u8; 256]) -> &'a [u8] {
    let payload = &frame[PAYLOAD_OFFSET..];
    let dst = BmIpAddr::GLOBAL_MULTICAST;
    let len = build(
        buf,
        &source_address(DEV_KIT, &dst),
        &dst,
        MIDDLEWARE_PORT,
        MIDDLEWARE_PORT,
        payload,
    )
    .unwrap();
    &buf[..len]
}

/// Gold vectors: a deployed node's publications, rebuilt from their payloads
/// byte for byte, headers and checksum included.
#[test]
fn build_reproduces_the_dev_kits_publications() {
    for frame in [&FRAME_84[..], &FRAME_85, &FRAME_86] {
        assert_eq!(rebuild(frame, &mut [0; 256]), frame);
    }
}

#[test]
fn accept_reads_the_dev_kits_publications() {
    let datagram = accept(&FRAME_86).unwrap();
    assert_eq!(datagram.src_port, MIDDLEWARE_PORT);
    assert_eq!(datagram.dst_port, MIDDLEWARE_PORT);
    assert_eq!(datagram.source, DEV_KIT);
    assert_eq!(datagram.payload, &FRAME_86[PAYLOAD_OFFSET..]);
    assert_eq!(&datagram.payload[5..26], b"spotter/transmit-data");
}

#[test]
fn source_address_follows_lwip_scope_selection() {
    let fe80 = addr::nodeid_to_ip(LINK_LOCAL_PREFIX, DEV_KIT);
    let fd00 = addr::nodeid_to_ip(UNIQUE_LOCAL_PREFIX, DEV_KIT);
    let ip = |s: &[u8]| {
        let mut a = [0u8; 16];
        a[..s.len()].copy_from_slice(s);
        BmIpAddr(a)
    };
    let mut loopback = [0u8; 16];
    loopback[15] = 1;
    for (dst, expected) in [
        (BmIpAddr::GLOBAL_MULTICAST, fd00),
        (BmIpAddr::LINK_LOCAL_MULTICAST, fe80),
        (ip(&[0xFF, 0x01]), fe80),
        (ip(&[0xFF, 0x00]), fe80),
        (ip(&[0xFF, 0x05]), fd00),
        (ip(&[0xFF, 0x0E]), fd00),
        (ip(&[0xFE, 0x80]), fe80),
        (ip(&[0xFE, 0xBF]), fe80),
        (ip(&[0xFE, 0xC0]), fd00),
        (ip(&[0xFD, 0x00]), fd00),
        (ip(&[0x20, 0x01]), fd00),
        (BmIpAddr(loopback), fe80),
        (BmIpAddr::default(), fd00),
    ] {
        assert_eq!(source_address(DEV_KIT, &dst), expected, "{dst:?}");
    }
}

/// A sum of `0xFFFF` complements to zero, which UDP reserves for "no
/// checksum"; lwIP sends `0xFFFF` in its place (divergence #71).
#[test]
fn a_checksum_of_zero_is_sent_as_ffff() {
    let src = fe80(1);
    let dst = BmIpAddr::LINK_LOCAL_MULTICAST;
    let mut buf = [0u8; PAYLOAD_OFFSET + 2];
    build(&mut buf, &src, &dst, 1, 2, &[0, 0]).unwrap();
    // A payload word equal to the checksum brings the sum to 0xFFFF.
    let word = [buf[UDP_CHECKSUM_OFFSET], buf[UDP_CHECKSUM_OFFSET + 1]];
    build(&mut buf, &src, &dst, 1, 2, &word).unwrap();
    assert_eq!(get_u16(&buf, UDP_CHECKSUM_OFFSET), 0xFFFF);

    let mut unset = buf;
    put_u16(&mut unset, UDP_CHECKSUM_OFFSET, 0);
    let segment = &unset[UDP_SOURCE_PORT_OFFSET..];
    assert_eq!(ipv6_pseudo_checksum(&src, &dst, IP_PROTO_UDP, segment), 0);
    let segment = &buf[UDP_SOURCE_PORT_OFFSET..];
    assert_eq!(
        ipv6_pseudo_checksum(&src, &dst, IP_PROTO_UDP, segment),
        0,
        "0xFFFF still verifies"
    );
}

fn fe80(id: u64) -> BmIpAddr {
    addr::nodeid_to_ip(LINK_LOCAL_PREFIX, id)
}

#[test]
fn build_refuses_a_short_buffer_and_an_oversized_payload() {
    let (src, dst) = (fe80(1), BmIpAddr::GLOBAL_MULTICAST);
    let mut buf = [0u8; PAYLOAD_OFFSET + 3];
    assert_eq!(
        build(&mut buf, &src, &dst, 1, 2, &[0; 4]),
        Err(BmWireError::Truncated)
    );
    assert_eq!(build(&mut buf, &src, &dst, 1, 2, &[0; 3]), Ok(buf.len()));
    let big = [0u8; MAX_PAYLOAD_LEN + 1];
    let mut buf = [0u8; PAYLOAD_OFFSET + MAX_PAYLOAD_LEN + 1];
    assert_eq!(
        build(&mut buf, &src, &dst, 1, 2, &big),
        Err(BmWireError::Invalid)
    );
    assert!(build(&mut buf, &src, &dst, 1, 2, &big[1..]).is_ok());
}

/// The payload runs to the end of the IPv6 payload whatever the UDP length
/// field says, and bytes past the IPv6 payload are ignored (divergence #72).
#[test]
fn accept_ignores_the_udp_length_field_and_trailing_bytes() {
    let mut frame = [0xEE; FRAME_86.len() + 4];
    frame[..FRAME_86.len()].copy_from_slice(&FRAME_86);
    for udp_len in [0u16, 7, 8, 9, 0xFFFF] {
        put_u16(&mut frame, UDP_LENGTH_OFFSET, udp_len);
        assert_eq!(accept(&frame).unwrap().payload, &FRAME_86[PAYLOAD_OFFSET..]);
    }
}

#[test]
fn accept_refuses_short_and_foreign_frames() {
    assert_eq!(
        accept(&FRAME_86[..MIN_FRAME_WITH_ADDRESSES - 1]),
        Err(BmWireError::Truncated)
    );
    assert_eq!(
        accept(&FRAME_86[..FRAME_86.len() - 1]),
        Err(BmWireError::Truncated),
        "shorter than the IPv6 payload length"
    );

    let mut frame = FRAME_86;
    frame[IPV6_NEXT_HEADER_OFFSET] = frame::IP_PROTO_BCMP;
    assert_eq!(accept(&frame), Err(BmWireError::Invalid));
    let mut frame = FRAME_86;
    frame[frame::ETHERNET_TYPE_OFFSET] = 0x08;
    assert_eq!(accept(&frame), Err(BmWireError::Invalid));

    // An IPv6 payload of seven bytes cannot hold a UDP header.
    let mut frame = FRAME_86;
    put_u16(&mut frame, IPV6_PAYLOAD_LENGTH_OFFSET, 7);
    assert_eq!(accept(&frame), Err(BmWireError::Truncated));
    put_u16(&mut frame, IPV6_PAYLOAD_LENGTH_OFFSET, 8);
    assert_eq!(accept(&frame).unwrap().payload, &[] as &[u8]);
}
