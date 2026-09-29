//! BCMP and UDP frames a peer would send, for the comparators to inject.
//!
//! [`bm_wire::bcmp::tx::build`] or [`bm_wire::udp::build`] into a `Vec`, as
//! `bm_stack::mock::frames` is for scripted peers. A comparator that needs specific header bytes writes
//! the headers with [`bm_wire::frame::write_headers`] and mutates them.

use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, tx};
use bm_wire::frame::MIN_FRAME_WITH_ADDRESSES;
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
    dst: &BmIpAddr,
    message_type: MessageType,
    seq_num: u32,
    body: &[u8],
) -> Vec<u8> {
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + BCMP_HEADER_LEN + body.len()];
    tx::build(&mut frame, src, dst, message_type, seq_num, |at| {
        at[..body.len()].copy_from_slice(body);
        Ok(body.len())
    })
    .expect("body fits an IPv6 payload");
    frame
}

/// A UDP datagram from node `src` to `dst`, from the address a deployed node
/// sends it from, [`udp::source_address`].
///
/// # Panics
///
/// If `payload` is longer than [`udp::MAX_PAYLOAD_LEN`].
#[must_use]
pub fn udp(src: u64, dst: &BmIpAddr, src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0u8; udp::PAYLOAD_OFFSET + payload.len()];
    udp::build(
        &mut frame,
        &udp::source_address(src, dst),
        dst,
        src_port,
        dst_port,
        payload,
    )
    .expect("payload fits");
    frame
}
