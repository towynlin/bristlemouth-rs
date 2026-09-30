//! UDP over IPv6: building a datagram's frame and accepting a received one.
//!
//! The oracle's UDP is `bm_udp_tx_perform` and the UDP branch of
//! `bm_l2_submit` in `network/bm_linux.c`. Deployed nodes use `bm_lwip.c` and
//! lwIP instead, and where the two differ this module follows lwIP:
//!
//! | Field | `bm_linux.c` | Here, as lwIP | Divergence |
//! |---|---|---|---|
//! | Source address | `fe80::<id>` always | [`source_address`] | #70 |
//! | Source MAC, hop limit | `mac_from_nodeid`, 64 | [`addr::mac_address`], [`HOP_LIMIT`] | #70 |
//! | Checksum that computes to 0 | sent as 0 | sent as `0xFFFF` | #71 |
//! | Received payload | UDP length field less 8; rejected if that field is under 8 or past the IPv6 payload | the IPv6 payload after the UDP header; UDP length field ignored | #72 |
//!
//! Neither checks a received checksum: `bm_linux.c` never does and
//! `bm_protocol` sets `CHECKSUM_CHECK_UDP` 0.

use crate::BmWireError;
use crate::addr::{self, LINK_LOCAL_PREFIX, UNIQUE_LOCAL_PREFIX};
use crate::checksum::ipv6_pseudo_checksum;
use crate::frame::{
    self, ETHERNET_TYPE_IPV6, HOP_LIMIT, IP_PROTO_UDP, IPV6_ADDRESS_SIZE, IPV6_NEXT_HEADER_OFFSET,
    IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
    UDP_CHECKSUM_OFFSET, UDP_DESTINATION_PORT_OFFSET, UDP_HEADER_LEN, UDP_LENGTH_OFFSET,
    UDP_SOURCE_PORT_OFFSET, ethernet_type,
};
use crate::util::BmIpAddr;

/// Offset of a datagram's payload in its frame.
pub const PAYLOAD_OFFSET: usize = UDP_SOURCE_PORT_OFFSET + UDP_HEADER_LEN;

/// Largest payload a datagram can carry: the UDP length field is 16 bits and
/// counts the header.
pub const MAX_PAYLOAD_LEN: usize = u16::MAX as usize - UDP_HEADER_LEN;

/// lwIP's multicast scope for link-local, `IP6_MULTICAST_SCOPE_LINK_LOCAL`.
const SCOPE_LINK_LOCAL: u8 = 2;

/// The address a deployed node sends a datagram to `dst` from.
///
/// `bm_lwip.c` gives the netif two addresses, `fe80::<id>` and `fd00::<id>`,
/// and lwIP's `ip6_select_source_address` picks between them by comparing
/// scopes. Over those two candidates its rules reduce to: the link-local
/// address when `dst` has link-local scope or narrower — `fe80::/10`, `::1`,
/// or multicast with a scope nibble of 2 or less, such as `ff02::1` — and the
/// unique-local address otherwise, such as for `ff03::1`.
///
/// `bm_linux.c` sends from `fe80::<id>` whatever the destination
/// (divergence #70).
#[must_use]
pub fn source_address(node_id: u64, dst: &BmIpAddr) -> BmIpAddr {
    let prefix = if has_link_local_scope(dst) {
        LINK_LOCAL_PREFIX
    } else {
        UNIQUE_LOCAL_PREFIX
    };
    addr::nodeid_to_ip(prefix, node_id)
}

/// Whether lwIP's `ip6_select_source_address` gives `dst` a scope no wider
/// than link-local.
///
/// Its tests run in this order: global unicast (`2000::/3`), link-local
/// unicast or loopback, unique-local, multicast (by scope nibble), site-local
/// (`fec0::/10`), anything else global. Only the second and fourth can yield a
/// scope of 2 or less.
fn has_link_local_scope(dst: &BmIpAddr) -> bool {
    let a = &dst.0;
    let link_local = a[0] == 0xFE && a[1] & 0xC0 == 0x80;
    let loopback = a[..15].iter().all(|b| *b == 0) && a[15] == 1;
    let multicast = a[0] == 0xFF && a[1] & 0x0F <= SCOPE_LINK_LOCAL;
    link_local || loopback || multicast
}

/// Build a UDP datagram's frame into `buf`: Ethernet and IPv6 headers from
/// [`frame::write_headers`] with [`HOP_LIMIT`], then the UDP header, then
/// `payload`.
///
/// `src` is the caller's; a node sends from [`source_address`]. The checksum
/// is [`ipv6_pseudo_checksum`] over the UDP header and payload, sent as
/// `0xFFFF` where it computes to zero, as lwIP's `udp_sendto_if_chksum` does
/// (divergence #71). No egress port is stamped: L2 does that, and only for
/// link-local multicast.
///
/// Returns the frame length.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `buf` cannot hold the frame;
/// [`BmWireError::Invalid`] if `payload` is longer than
/// [`MAX_PAYLOAD_LEN`]. `bm_linux.c` writes the length fields truncated to 16
/// bits for such a payload; lwIP's `udp_sendto_if` refuses it with `ERR_MEM`.
pub fn build(
    buf: &mut [u8],
    src: &BmIpAddr,
    dst: &BmIpAddr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Result<usize, BmWireError> {
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(BmWireError::Invalid);
    }
    build_with(buf, src, dst, src_port, dst_port, |out| {
        let out = out.get_mut(..payload.len()).ok_or(BmWireError::Truncated)?;
        out.copy_from_slice(payload);
        Ok(payload.len())
    })
}

/// [`build`], with the payload written in place by `payload`, which is given
/// the buffer from [`PAYLOAD_OFFSET`] on and returns how much it wrote.
///
/// # Errors
///
/// As [`build`], and whatever `payload` returns.
pub fn build_with<F>(
    buf: &mut [u8],
    src: &BmIpAddr,
    dst: &BmIpAddr,
    src_port: u16,
    dst_port: u16,
    payload: F,
) -> Result<usize, BmWireError>
where
    F: FnOnce(&mut [u8]) -> Result<usize, BmWireError>,
{
    let payload_len = payload(
        buf.get_mut(PAYLOAD_OFFSET..)
            .ok_or(BmWireError::Truncated)?,
    )?;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(BmWireError::Invalid);
    }
    let udp_len = UDP_HEADER_LEN + payload_len;
    let end = MIN_FRAME_WITH_ADDRESSES + udp_len;
    let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;
    frame::write_headers(buf, src, dst, IP_PROTO_UDP, HOP_LIMIT, udp_len)?;

    let udp_len = u16::try_from(udp_len).map_err(|_| BmWireError::Invalid)?;
    put_u16(buf, UDP_SOURCE_PORT_OFFSET, src_port);
    put_u16(buf, UDP_DESTINATION_PORT_OFFSET, dst_port);
    put_u16(buf, UDP_LENGTH_OFFSET, udp_len);
    put_u16(buf, UDP_CHECKSUM_OFFSET, 0);

    // `ipv6_pseudo_checksum` returns the checksum byte-swapped for a
    // little-endian store; swapping back gives the wire value.
    let checksum =
        ipv6_pseudo_checksum(src, dst, IP_PROTO_UDP, &buf[UDP_SOURCE_PORT_OFFSET..]).swap_bytes();
    let checksum = if checksum == 0 { 0xFFFF } else { checksum };
    put_u16(buf, UDP_CHECKSUM_OFFSET, checksum);
    Ok(end)
}

/// A datagram [`accept`] took from a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram<'a> {
    /// The sender's port.
    pub src_port: u16,
    /// The port the datagram is addressed to.
    pub dst_port: u16,
    /// The node id in the low half of the source address, `ip_to_nodeid`.
    pub source: u64,
    /// Everything after the UDP header, to the end of the IPv6 payload.
    pub payload: &'a [u8],
}

/// Take a UDP datagram out of a received frame.
///
/// What lwIP's `ip6_input` and `udp_input` deliver to `bm_lwip.c`'s
/// `udp_recv_cb`, which passes the source port, the source node id and the
/// payload on. The frame's IPv6 payload length must fit the frame, as
/// `bm_l2_submit` and `ip6_input` both require; anything past it is trailing
/// padding and ignored. The UDP length field and the checksum are not read
/// (divergence #72).
///
/// Nothing here decides whether the datagram is for this node: `bm_linux.c`
/// dispatches on the destination port alone.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame is shorter than its headers or its
/// IPv6 payload length says, or that payload is shorter than a UDP header;
/// [`BmWireError::Invalid`] if the frame is not IPv6 or not UDP.
pub fn accept(frame: &[u8]) -> Result<Datagram<'_>, BmWireError> {
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return Err(BmWireError::Truncated);
    }
    if ethernet_type(frame) != Some(ETHERNET_TYPE_IPV6) {
        return Err(BmWireError::Invalid);
    }
    let ipv6_payload_len = usize::from(get_u16(frame, IPV6_PAYLOAD_LENGTH_OFFSET));
    let end = MIN_FRAME_WITH_ADDRESSES + ipv6_payload_len;
    let frame = frame.get(..end).ok_or(BmWireError::Truncated)?;
    if frame[IPV6_NEXT_HEADER_OFFSET] != IP_PROTO_UDP {
        return Err(BmWireError::Invalid);
    }
    if ipv6_payload_len < UDP_HEADER_LEN {
        return Err(BmWireError::Truncated);
    }

    let mut src = [0u8; IPV6_ADDRESS_SIZE];
    src.copy_from_slice(&frame[IPV6_SOURCE_ADDRESS_OFFSET..][..IPV6_ADDRESS_SIZE]);
    Ok(Datagram {
        src_port: get_u16(frame, UDP_SOURCE_PORT_OFFSET),
        dst_port: get_u16(frame, UDP_DESTINATION_PORT_OFFSET),
        source: BmIpAddr(src).to_node_id(),
        payload: &frame[PAYLOAD_OFFSET..],
    })
}

fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

fn get_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

#[cfg(test)]
mod tests;
