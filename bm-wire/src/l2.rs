//! L2 egress: stamping the outgoing port into a frame, ported from
//! `network/l2.c`.
//!
//! Bristlemouth encodes the port a frame leaves on into the low nibble of the
//! source address byte at [`IPV6_INGRESS_EGRESS_PORTS_OFFSET`], per
//! specification section 5.4.4.1. That byte is inside the IPv6 source address,
//! which the upper-layer checksum covers — so stamping it invalidates a
//! checksum that was computed before the egress port was known.
//!
//! bm_core does not recompute the checksum. It *patches* it: the ports byte is
//! the high half of a 16-bit word in the one's-complement sum, so adding the
//! port number to the high byte of the checksum is enough. That is
//! `network_add_egress_port`, and [`add_egress_port`] reproduces it — including
//! the place where the patch is wrong, which is divergence #12.
//!
//! # Order matters
//!
//! A frame going out several ports is stamped, sent, cleared and un-patched
//! once per port, in that order, reusing one buffer. Getting the order wrong
//! leaves the next port's frame with a checksum for the previous port, which
//! the receiving node silently drops. [`with_egress_port`] exists so that
//! sequence lives in one place instead of at every call site.

use crate::BmWireError;
use crate::bcmp::header::{BCMP_HEADER_OFFSET, CHECKSUM_FIELD_OFFSET};
use crate::frame::{
    ETHERNET_TYPE_IPV6, IP_PROTO_BCMP, IP_PROTO_UDP, IPV6_ADDRESS_SIZE,
    IPV6_DESTINATION_ADDRESS_OFFSET, IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_NEXT_HEADER_OFFSET,
    MIN_FRAME_WITH_ADDRESSES, UDP_CHECKSUM_OFFSET, ethernet_type,
};
use crate::util::BmIpAddr;

/// Byte of the BCMP header holding the checksum, as a frame offset.
const BCMP_CHECKSUM_OFFSET: usize = BCMP_HEADER_OFFSET + CHECKSUM_FIELD_OFFSET;

/// Shortest frame [`add_egress_port`] can act on.
///
/// The C checks neither the length nor the buffer bounds before reading the
/// EtherType and the checksum, so a shorter frame is an out-of-bounds read
/// there with no defined behaviour to match. This port refuses instead.
pub const MIN_STAMPABLE_FRAME: usize = MIN_FRAME_WITH_ADDRESSES + 8;

/// How L2 transmits a frame, decided by its destination address.
///
/// From `bm_l2_process_tx_evt`. The asymmetry is deliberate in the C and is
/// easy to miss: **only link-local multicast is stamped**.
///
/// Deliberately *not* `non_exhaustive`: the three branches are complete by
/// construction, and a caller driving a PHY wants the compiler to tell it if
/// bm_core ever grows a fourth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    /// Global multicast, `FF03::1`. Sent to the whole port mask with no egress
    /// stamp and no checksum patch — to every port at once when the mask is
    /// every port, otherwise port by port.
    GlobalMulticast,
    /// Link-local multicast. Sent port by port, each copy stamped with its
    /// egress port and the checksum patched to match.
    LinkLocalMulticast,
    /// Anything else. bm_core's L2 drops it: `bm_l2_process_tx_evt` has no
    /// branch for a unicast destination, so the buffer is freed unsent.
    Dropped,
}

/// Classify a frame by its destination address.
///
/// Returns [`TxKind::Dropped`] for a frame too short to hold a destination
/// address, which is also what the C does with it — nothing.
#[must_use]
pub fn tx_kind(frame: &[u8]) -> TxKind {
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return TxKind::Dropped;
    }
    let mut addr = [0u8; IPV6_ADDRESS_SIZE];
    addr.copy_from_slice(
        &frame
            [IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE],
    );
    let dst = BmIpAddr(addr);
    if dst.is_global_multicast() {
        TxKind::GlobalMulticast
    } else if dst.is_link_local_multicast() {
        TxKind::LinkLocalMulticast
    } else {
        TxKind::Dropped
    }
}

/// Offset within the destination address where an application asks for a
/// specific egress port, from `bm_l2_link_output`.
pub const REQUESTED_EGRESS_PORT_OFFSET: usize = IPV6_DESTINATION_ADDRESS_OFFSET + 13;

/// Read and clear the egress port an application requested in the destination
/// address, returning the port mask to transmit on.
///
/// Ported from `bm_l2_link_output`. A value of 1..=`num_ports` selects that one
/// port; anything else — including zero, and including a port number the device
/// does not have — means every port. The byte is cleared either way, because it
/// is a request channel rather than part of the address, and it must not reach
/// the wire.
///
/// Note that the byte is cleared *after* the upper layer computed its checksum
/// and *before* [`add_egress_port`] patches it, so a non-zero value here
/// corrupts the checksum. `bcmp_ll_forward` is careful to set it only on a
/// destination whose checksum was computed with it clear.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame cannot hold a destination address.
pub fn take_requested_egress_port(frame: &mut [u8], num_ports: u8) -> Result<u16, BmWireError> {
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return Err(BmWireError::Truncated);
    }
    let requested = frame[REQUESTED_EGRESS_PORT_OFFSET];
    frame[REQUESTED_EGRESS_PORT_OFFSET] = 0;

    let all_ports = (1u16 << num_ports) - 1;
    if requested > 0 && requested <= num_ports {
        Ok(1u16 << (requested - 1))
    } else {
        Ok(all_ports)
    }
}

/// Stamp `port` into the frame's egress nibble and patch the checksum to match.
///
/// Ported from `network_add_egress_port`. `port` is OR-ed in, so this is only
/// equivalent to an addition — which is what the checksum patch assumes — when
/// the egress nibble is already clear and `port` is 1..=15. The C makes the
/// same assumption without checking it.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame is shorter than
/// [`MIN_STAMPABLE_FRAME`].
pub fn add_egress_port(frame: &mut [u8], port: u8) -> Result<(), BmWireError> {
    patch(frame, port, Patch::Add)
}

/// Undo [`add_egress_port`]'s checksum patch, ported from
/// `network_revert_checksum`.
///
/// This restores the checksum only. The ports byte is cleared separately by
/// [`clear_ports`], which the C calls first — see [`with_egress_port`].
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame is shorter than
/// [`MIN_STAMPABLE_FRAME`].
pub fn revert_checksum(frame: &mut [u8], port: u8) -> Result<(), BmWireError> {
    patch(frame, port, Patch::Subtract)
}

/// Zero both nibbles of the ports byte, ported from the C's `clear_ports`.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame cannot hold a source address.
pub fn clear_ports(frame: &mut [u8]) -> Result<(), BmWireError> {
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return Err(BmWireError::Truncated);
    }
    frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = 0;
    Ok(())
}

/// Stamp `port`, hand the frame to `send`, then put the frame back as it was.
///
/// This is `bm_l2_process_tx_evt`'s inner loop, which stamps, sends, clears and
/// un-patches once per egress port over a single shared buffer. Wrapping it
/// means a caller transmitting on several ports cannot leave the buffer
/// carrying the previous port's stamp or checksum.
///
/// The frame is restored even if `send` fails, because the C restores it
/// unconditionally too — `send_to_port` logs an error and returns.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame is shorter than
/// [`MIN_STAMPABLE_FRAME`]. `send` is not called in that case.
pub fn with_egress_port<R>(
    frame: &mut [u8],
    port: u8,
    send: impl FnOnce(&[u8]) -> R,
) -> Result<R, BmWireError> {
    let stamped = stamp_egress_port(frame, port)?;
    Ok(send(&stamped))
}

/// A frame stamped for one egress port, restored when the stamp is dropped.
///
/// [`with_egress_port`] cannot help a caller whose transmit is `async`, since
/// the send happens inside a closure. This is the same guarantee in a form
/// that can be held across an `await`: stamp, transmit, and let the scope end.
#[derive(Debug)]
pub struct Stamped<'a> {
    frame: &'a mut [u8],
    port: u8,
}

impl core::ops::Deref for Stamped<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.frame
    }
}

impl Drop for Stamped<'_> {
    fn drop(&mut self) {
        // Neither can fail: `stamp_egress_port` already established that the
        // frame is long enough, and nothing here can shorten it.
        let _ = clear_ports(self.frame);
        let _ = revert_checksum(self.frame, self.port);
    }
}

/// Stamp `port` into `frame`, giving back a view that restores it on drop.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if the frame is shorter than
/// [`MIN_STAMPABLE_FRAME`].
pub fn stamp_egress_port(frame: &mut [u8], port: u8) -> Result<Stamped<'_>, BmWireError> {
    add_egress_port(frame, port)?;
    Ok(Stamped { frame, port })
}

#[derive(Clone, Copy)]
enum Patch {
    Add,
    Subtract,
}

/// The shared body of `network_add_egress_port` and `network_revert_checksum`.
///
/// Both are the same three steps — undo the one's complement, apply the port
/// number, redo it — differing only in the sign and in whether the ports byte
/// is touched.
fn patch(frame: &mut [u8], port: u8, op: Patch) -> Result<(), BmWireError> {
    if frame.len() < MIN_STAMPABLE_FRAME {
        return Err(BmWireError::Truncated);
    }

    if matches!(op, Patch::Add) {
        frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= port;
    }

    if ethernet_type(frame) != Some(ETHERNET_TYPE_IPV6) {
        return Ok(());
    }

    match frame[IPV6_NEXT_HEADER_OFFSET] {
        IP_PROTO_UDP => {
            // Eight-bit arithmetic, exactly as the C has it. The C writes
            // `payload[udp_checksum_offset] ^= 0xFFFF` on a `uint8_t` lvalue,
            // which truncates to `^= 0xFF`, and the addition drops the carry
            // that a one's-complement sum has to wrap around. That is
            // divergence #12: the checksum is wrong whenever the byte carries.
            let byte = &mut frame[UDP_CHECKSUM_OFFSET];
            *byte ^= 0xFF;
            *byte = match op {
                Patch::Add => byte.wrapping_add(port),
                Patch::Subtract => byte.wrapping_sub(port),
            };
            *byte ^= 0xFF;
        }
        IP_PROTO_BCMP => {
            // Sixteen-bit arithmetic, because the C's lvalue here is the
            // `uint16_t` field of a BcmpHeader rather than a byte. Read as
            // little-endian to reproduce what the C does on the only kind of
            // host it runs on; the value is byte-swapped relative to the wire,
            // so the low half of this `u16` is the high byte of the checksum,
            // and a carry out of it lands where the one's-complement wrap
            // belongs.
            let bytes: [u8; 2] = frame[BCMP_CHECKSUM_OFFSET..BCMP_CHECKSUM_OFFSET + 2]
                .try_into()
                .expect("two bytes");
            let mut checksum = u16::from_le_bytes(bytes);
            checksum ^= 0xFFFF;
            checksum = match op {
                Patch::Add => checksum.wrapping_add(u16::from(port)),
                Patch::Subtract => checksum.wrapping_sub(u16::from(port)),
            };
            checksum ^= 0xFFFF;
            frame[BCMP_CHECKSUM_OFFSET..BCMP_CHECKSUM_OFFSET + 2]
                .copy_from_slice(&checksum.to_le_bytes());
        }
        _ => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bcmp::{MessageType, tx};
    use crate::frame::{
        ETHERNET_TYPE_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    };

    fn bcmp_frame(body_len: usize) -> [u8; 128] {
        let mut frame = [0u8; 128];
        frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
            .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        let payload_len = crate::bcmp::BCMP_HEADER_LEN + body_len;
        frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
            .copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
            .copy_from_slice(&crate::addr::nodeid_to_ip(0xFE80_0000, 0x55AA_0011).0);
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&BmIpAddr::LINK_LOCAL_MULTICAST.0);
        let end = MIN_FRAME_WITH_ADDRESSES + payload_len;
        tx::serialize(
            &mut frame[..end],
            MessageType::HEARTBEAT,
            0,
            &[7u8; 12][..body_len],
        )
        .unwrap();
        frame
    }

    #[test]
    fn stamping_then_reverting_restores_the_frame() {
        for port in 1..=15u8 {
            let original = bcmp_frame(12);
            let mut frame = original;
            add_egress_port(&mut frame, port).unwrap();
            assert_ne!(frame, original, "stamping must change something");
            clear_ports(&mut frame).unwrap();
            revert_checksum(&mut frame, port).unwrap();
            assert_eq!(frame, original, "port {port} did not round-trip");
        }
    }

    #[test]
    fn with_egress_port_leaves_the_buffer_reusable() {
        let original = bcmp_frame(12);
        let mut frame = original;
        let mut sent: [Option<[u8; 128]>; 2] = [None, None];
        for (index, port) in [1u8, 2].into_iter().enumerate() {
            with_egress_port(&mut frame, port, |out| {
                let mut copy = [0u8; 128];
                copy.copy_from_slice(out);
                sent[index] = Some(copy);
            })
            .unwrap();
        }
        assert_eq!(frame, original, "the shared buffer must come back clean");

        let first = sent[0].unwrap();
        let second = sent[1].unwrap();
        assert_eq!(first[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F, 1);
        assert_eq!(second[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F, 2);
        assert_ne!(
            first[BCMP_CHECKSUM_OFFSET..BCMP_CHECKSUM_OFFSET + 2],
            second[BCMP_CHECKSUM_OFFSET..BCMP_CHECKSUM_OFFSET + 2],
            "each port's copy carries its own checksum"
        );
    }

    /// The point of the whole patch, checked against the real receive path.
    ///
    /// A receiver clears only the *ingress* nibble before verifying — the
    /// egress nibble the sender's L2 stamped stays in the frame and stays in
    /// the checksum. So the patch is not an optimisation: without it every
    /// frame leaving a port would be rejected by the node at the other end.
    #[test]
    fn a_stamped_frame_is_accepted_by_the_receive_path() {
        for body_len in 0..12usize {
            for egress in 1..=15u8 {
                let mut frame = bcmp_frame(body_len);
                let end = MIN_FRAME_WITH_ADDRESSES + crate::bcmp::BCMP_HEADER_LEN + body_len;

                add_egress_port(&mut frame[..end], egress).unwrap();
                // The node at the other end stamps its ingress port on arrival.
                for ingress in 0..=15u8 {
                    let mut arrived = frame;
                    arrived[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= ingress << 4;
                    let received =
                        crate::bcmp::rx::accept(&mut arrived[..end]).unwrap_or_else(|e| {
                            panic!("body {body_len} egress {egress} ingress {ingress}: {e}")
                        });
                    assert_eq!(received.ingress_port, ingress);
                }
            }
        }
    }

    /// The same frame *without* the patch is rejected, which is what makes the
    /// test above meaningful rather than vacuous.
    #[test]
    fn an_unpatched_stamp_is_rejected_by_the_receive_path() {
        let mut frame = bcmp_frame(12);
        let end = MIN_FRAME_WITH_ADDRESSES + crate::bcmp::BCMP_HEADER_LEN + 12;
        frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] |= 1; // stamped, checksum untouched
        assert_eq!(
            crate::bcmp::rx::accept(&mut frame[..end]),
            Err(crate::bcmp::RxError::BadChecksum)
        );
    }

    #[test]
    fn a_stamp_restores_the_frame_when_it_is_dropped() {
        let original = bcmp_frame(12);
        let mut frame = original;
        {
            let stamped = stamp_egress_port(&mut frame, 2).unwrap();
            assert_eq!(stamped[IPV6_INGRESS_EGRESS_PORTS_OFFSET] & 0x0F, 2);
            assert_ne!(&stamped[..], &original[..]);
        }
        assert_eq!(frame, original);
    }

    #[test]
    fn only_multicast_destinations_are_transmitted() {
        let mut frame = bcmp_frame(12);
        assert_eq!(tx_kind(&frame), TxKind::LinkLocalMulticast);

        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&BmIpAddr::GLOBAL_MULTICAST.0);
        assert_eq!(tx_kind(&frame), TxKind::GlobalMulticast);

        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&crate::addr::nodeid_to_ip(0xFD00_0000, 42).0);
        assert_eq!(
            tx_kind(&frame),
            TxKind::Dropped,
            "bm_core's L2 has no branch for a unicast destination"
        );

        assert_eq!(tx_kind(&[0u8; 10]), TxKind::Dropped);
    }

    #[test]
    fn a_requested_egress_port_selects_one_port_and_is_cleared() {
        let mut frame = bcmp_frame(12);
        frame[REQUESTED_EGRESS_PORT_OFFSET] = 2;
        assert_eq!(take_requested_egress_port(&mut frame, 2).unwrap(), 0b10);
        assert_eq!(
            frame[REQUESTED_EGRESS_PORT_OFFSET], 0,
            "must not reach the wire"
        );

        // Out of range, and zero, both mean every port.
        for requested in [0u8, 3, 255] {
            let mut frame = bcmp_frame(12);
            frame[REQUESTED_EGRESS_PORT_OFFSET] = requested;
            assert_eq!(take_requested_egress_port(&mut frame, 2).unwrap(), 0b11);
            assert_eq!(frame[REQUESTED_EGRESS_PORT_OFFSET], 0);
        }
    }

    #[test]
    fn frames_too_short_to_stamp_are_refused() {
        let mut short = [0u8; MIN_STAMPABLE_FRAME - 1];
        assert_eq!(add_egress_port(&mut short, 1), Err(BmWireError::Truncated));
        assert_eq!(revert_checksum(&mut short, 1), Err(BmWireError::Truncated));
        let mut called = false;
        assert_eq!(
            with_egress_port(&mut short, 1, |_| called = true),
            Err(BmWireError::Truncated)
        );
        assert!(!called, "send must not run on a frame we refused");
    }

    #[test]
    fn a_non_ipv6_frame_is_stamped_but_has_no_checksum_to_patch() {
        let mut frame = bcmp_frame(12);
        frame[ETHERNET_TYPE_OFFSET] = 0x08;
        frame[ETHERNET_TYPE_OFFSET + 1] = 0x06; // ARP
        let before = frame;
        add_egress_port(&mut frame, 3).unwrap();
        assert_eq!(frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 3);
        assert_eq!(
            frame[BCMP_CHECKSUM_OFFSET..BCMP_CHECKSUM_OFFSET + 2],
            before[BCMP_CHECKSUM_OFFSET..BCMP_CHECKSUM_OFFSET + 2]
        );
    }
}
