//! BCMP frames as a peer would put them on the wire, for scripting a
//! [`MockPhy`](super::MockPhy).
//!
//! Only what a receiving node reads is filled in: EtherType, IPv6 payload
//! length, next header, source and destination address, and the BCMP header
//! with its checksum. MAC addresses and the hop limit are zero.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use bm_wire::addr;
use bm_wire::bcmp::info::DeviceInfoReply;
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::{BCMP_HEADER_LEN, Heartbeat, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::neighbor::HEARTBEAT_PERIOD_S;
use bm_wire::util::BmIpAddr;

use crate::node::LINK_LOCAL_PREFIX;

/// A BCMP message from node `src` (at its `fe80::` address) to `dst`.
///
/// # Panics
///
/// If `body` does not fit an IPv6 payload length.
#[must_use]
pub fn bcmp(
    src: u64,
    dst: BmIpAddr,
    message_type: MessageType,
    seq_num: u32,
    body: &[u8],
) -> Vec<u8> {
    let payload_len = BCMP_HEADER_LEN + body.len();
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
    frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
    frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2].copy_from_slice(
        &u16::try_from(payload_len)
            .expect("body fits an IPv6 payload")
            .to_be_bytes(),
    );
    frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
    frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
        .copy_from_slice(&addr::nodeid_to_ip(LINK_LOCAL_PREFIX, src).0);
    frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
        .copy_from_slice(&dst.0);
    tx::serialize(&mut frame, message_type, seq_num, body).expect("frame is sized for the body");
    frame
}

/// A heartbeat from `src` to `FF02::1`, with bm_core's lease of
/// [`HEARTBEAT_PERIOD_S`].
#[must_use]
pub fn heartbeat(src: u64, time_since_boot_us: u64) -> Vec<u8> {
    let mut body = [0u8; Heartbeat::LEN];
    Heartbeat {
        time_since_boot_us,
        liveliness_lease_dur_s: HEARTBEAT_PERIOD_S,
    }
    .encode(&mut body)
    .expect("sized by Heartbeat::LEN");
    bcmp(
        src,
        BmIpAddr::LINK_LOCAL_MULTICAST,
        MessageType::HEARTBEAT,
        0,
        &body,
    )
}

/// An echo request from `src` to `dst`.
///
/// # Panics
///
/// If the payload is longer than [`EchoRequest::MAX_PAYLOAD_LEN`].
#[must_use]
pub fn echo_request(src: u64, dst: BmIpAddr, request: &EchoRequest<'_>) -> Vec<u8> {
    let mut body = vec![0u8; request.encoded_len()];
    request.encode(&mut body).expect("payload fits");
    bcmp(src, dst, MessageType::ECHO_REQUEST, 0, &body)
}

/// An echo reply from `src` to `FF02::1`, where bm_core sends it.
///
/// # Panics
///
/// If the payload is longer than [`EchoReply::MAX_PAYLOAD_LEN`].
#[must_use]
pub fn echo_reply(src: u64, reply: &EchoReply<'_>) -> Vec<u8> {
    let mut body = vec![0u8; reply.encoded_len()];
    reply.encode(&mut body).expect("payload fits");
    bcmp(
        src,
        BmIpAddr::LINK_LOCAL_MULTICAST,
        MessageType::ECHO_REPLY,
        0,
        &body,
    )
}

/// A device-info reply from `src` to `FF02::1`.
///
/// # Panics
///
/// If a string is longer than [`DeviceInfoReply::MAX_STRING_LEN`].
#[must_use]
pub fn device_info_reply(src: u64, reply: &DeviceInfoReply<'_>) -> Vec<u8> {
    let mut body = vec![0u8; reply.encoded_len()];
    reply.encode(&mut body).expect("strings fit");
    bcmp(
        src,
        BmIpAddr::LINK_LOCAL_MULTICAST,
        MessageType::DEVICE_INFO_REPLY,
        0,
        &body,
    )
}
