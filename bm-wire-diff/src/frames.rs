//! BCMP, UDP and publication frames a peer would send, for the comparators to
//! inject.
//!
//! [`bm_wire::bcmp::tx::build`] or [`bm_wire::udp::build`] into a `Vec`, as
//! `bm_stack::mock::frames` is for scripted peers. A comparator that needs specific header bytes writes
//! the headers with [`bm_wire::frame::write_headers`] and mutates them.

use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, tx};
use bm_wire::frame::MIN_FRAME_WITH_ADDRESSES;
use bm_wire::util::BmIpAddr;
use bm_wire::{pubsub, spotter, udp};

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

/// A publication from node `src`, as `bm_pub_wl` sends it: [`pubsub::encode`]
/// in a datagram to `FF03::1` from and to [`pubsub::PORT`].
///
/// # Panics
///
/// If `topic` is empty or [`pubsub::TOPIC_MAX_LEN`] bytes or longer.
#[must_use]
pub fn publication(src: u64, topic: &[u8], kind: u8, version: u8, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![0u8; pubsub::HEADER_LEN + topic.len() + data.len()];
    pubsub::encode(&mut payload, topic, kind, version, data).expect("a topic bm_pub_wl sends");
    udp(
        src,
        &BmIpAddr::GLOBAL_MULTICAST,
        pubsub::PORT,
        pubsub::PORT,
        &payload,
    )
}

/// A `spotter_log` publication from node `src`: [`spotter::encode_log`] in
/// [`publication`] to [`spotter::log_topic`].
///
/// # Panics
///
/// If `spotter_log` would refuse the arguments.
#[must_use]
pub fn spotter_log(
    src: u64,
    target_node_id: u64,
    file_name: Option<&[u8]>,
    print_time: u8,
    text: &[u8],
) -> Vec<u8> {
    let mut body = vec![0u8; spotter::MAX_LOG_LEN];
    let len = spotter::encode_log(&mut body, target_node_id, file_name, print_time, text)
        .expect("arguments spotter_log accepts");
    publication(
        src,
        spotter::log_topic(file_name),
        spotter::KIND,
        pubsub::COMMON_VERSION,
        &body[..len],
    )
}

/// A `spotter_tx_data` publication from node `src`:
/// [`spotter::encode_tx_data`] in [`publication`] to
/// [`spotter::TRANSMIT_DATA_TOPIC`].
///
/// # Panics
///
/// If `data` is longer than [`spotter::NetworkType::max_len`].
#[must_use]
pub fn spotter_tx_data(src: u64, data: &[u8], network: spotter::NetworkType) -> Vec<u8> {
    let mut body = vec![0u8; spotter::MAX_TX_LEN];
    let len = spotter::encode_tx_data(&mut body, network, data).expect("data within the limit");
    publication(
        src,
        spotter::TRANSMIT_DATA_TOPIC,
        spotter::KIND,
        pubsub::COMMON_VERSION,
        &body[..len],
    )
}
