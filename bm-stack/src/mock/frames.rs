//! BCMP and UDP frames as a peer would put them on the wire, for scripting a
//! [`MockPhy`](super::MockPhy).
//!
//! Every frame is [`bm_wire::bcmp::tx::build`]'s or [`bm_wire::udp::build`]'s,
//! as the node's own are.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use bm_wire::bcmp::info::DeviceInfoReply;
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::{BCMP_HEADER_LEN, Heartbeat, MessageType, tx};
use bm_wire::frame::MIN_FRAME_WITH_ADDRESSES;
use bm_wire::neighbor::HEARTBEAT_PERIOD_S;
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

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
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN + body.len()];
    let len = tx::build(&mut frame, src, &dst, message_type, seq_num, |at| {
        at[..body.len()].copy_from_slice(body);
        Ok(body.len())
    })
    .expect("body fits an IPv6 payload");
    debug_assert_eq!(len, frame.len());
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

/// A UDP datagram from node `src` to `dst`, from the address a deployed node
/// sends it from, [`udp::source_address`].
///
/// # Panics
///
/// If `payload` is longer than [`udp::MAX_PAYLOAD_LEN`].
#[must_use]
pub fn udp(src: u64, dst: BmIpAddr, src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0u8; udp::PAYLOAD_OFFSET + payload.len()];
    udp::build(
        &mut frame,
        &udp::source_address(src, &dst),
        &dst,
        src_port,
        dst_port,
        payload,
    )
    .expect("payload fits");
    frame
}
