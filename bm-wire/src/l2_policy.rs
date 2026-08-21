//! Link-local RX routing policy, ported from `network/l2_policy.c`.
//!
//! Implements Bristlemouth specification section 5.4.4.3: encode the ingress
//! port into the source address, and decide which ports a frame is forwarded
//! to and whether it also travels up the local stack.

use crate::frame::{
    ETHERNET_TYPE_IPV6, IPV6_ADDRESS_SIZE, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET, MIN_FRAME_WITH_ADDRESSES,
    ethernet_type,
};
use crate::util::BmIpAddr;

/// Outcome of applying RX policy to a frame.
///
/// Field-for-field equivalent to the C `BmL2PolicyRxResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RxResult {
    /// Whether the frame (mutated in place) should also go up the local stack.
    pub should_submit: bool,
    /// Bitmask of ports to forward to. Zero means do not forward.
    pub egress_mask: u16,
    /// Ingress port decoded from the mask.
    ///
    /// Documented by the C as 1-15, with 0 meaning unknown — but it is computed
    /// with `ffs`, so a mask with a bit above 15 set yields 16. See divergence
    /// #4; this port reproduces the C rather than clamping.
    pub ingress_port_num: u8,
}

/// Decides forwarding for link-local traffic that is not `FF02::1`.
///
/// Mirrors the C `L2LinkLocalRoutingCb`. `src` is a mutable view of the frame's
/// source address: writes to it land in the frame, exactly as through the C's
/// pointer.
pub trait LinkLocalRouting {
    /// Return whether the frame should be submitted up the stack, and set
    /// `egress_mask` to the ports it should be forwarded to.
    fn route(
        &mut self,
        ingress_port: u8,
        egress_mask: &mut u16,
        src: &mut BmIpAddr,
        dst: &BmIpAddr,
    ) -> bool;
}

impl<F> LinkLocalRouting for F
where
    F: FnMut(u8, &mut u16, &mut BmIpAddr, &BmIpAddr) -> bool,
{
    fn route(
        &mut self,
        ingress_port: u8,
        egress_mask: &mut u16,
        src: &mut BmIpAddr,
        dst: &BmIpAddr,
    ) -> bool {
        self(ingress_port, egress_mask, src, dst)
    }
}

/// Apply RX policy to an Ethernet + IPv6 frame, mutating it in place.
///
/// * Encodes the ingress port number into the upper nibble of the source
///   address byte at offset 24.
/// * Floods global multicast to every port except the ingress port.
/// * Defers link-local non-neighbor traffic to `routing`, if provided.
///
/// Frames shorter than [`MIN_FRAME_WITH_ADDRESSES`], and frames whose EtherType
/// is not IPv6, pass through untouched with `should_submit` set — matching the
/// C, which must not misread an ARP payload as IPv6 addresses.
pub fn rx_apply(
    frame: &mut [u8],
    ingress_port_mask: u16,
    all_ports_mask: u16,
    routing: Option<&mut dyn LinkLocalRouting>,
) -> RxResult {
    let mut result = RxResult {
        should_submit: true,
        egress_mask: 0,
        ingress_port_num: 0,
    };

    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        // Too short to hold IPv6 addresses; policy does not act.
        return result;
    }

    if ethernet_type(frame) != Some(ETHERNET_TYPE_IPV6) {
        return result;
    }

    // `ffs` semantics: 1-based index of the lowest set bit, 0 if none.
    let ingress_port_num = if ingress_port_mask == 0 {
        return result;
    } else {
        (ingress_port_mask.trailing_zeros() + 1) as u8
    };
    result.ingress_port_num = ingress_port_num;

    // Ingress port goes in the upper nibble, preserving the egress nibble.
    let ports = &mut frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET];
    *ports = (*ports & 0x0F) | ((ingress_port_num & 0x0F) << 4);

    let dst = read_addr(frame, IPV6_DESTINATION_ADDRESS_OFFSET);

    // Global multicast floods every port but the one it arrived on.
    if dst.is_global_multicast() {
        result.egress_mask = all_ports_mask & !ingress_port_mask;
        return result;
    }

    // Link-local multicast that is not FF02::1 is the routing callback's call.
    if let Some(routing) = routing
        && !dst.is_link_local_neighbor_multicast()
    {
        let mut src = read_addr(frame, IPV6_SOURCE_ADDRESS_OFFSET);
        let mut egress = 0u16;

        result.should_submit = routing.route(ingress_port_num, &mut egress, &mut src, &dst);
        result.egress_mask = egress;

        // The C hands out a pointer into the frame, so anything the
        // callback wrote to `src` is already in the frame. Copy back to
        // reproduce that.
        frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + IPV6_ADDRESS_SIZE]
            .copy_from_slice(&src.0);
    }

    result
}

/// Prepare a *forwarded copy* for transmission.
///
/// Despite what the C header comment claims, this clears **both** nibbles of
/// the ports byte, not just the ingress one — see divergence #1. The code is
/// authoritative: that is what deployed nodes put on the wire.
///
/// Call this only on the forwarded copy. The original RX buffer keeps its port
/// information for the local stack.
pub fn prepare_forwarded_copy(frame: &mut [u8]) {
    if frame.len() < MIN_FRAME_WITH_ADDRESSES {
        return;
    }
    frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = 0;
}

fn read_addr(frame: &[u8], offset: usize) -> BmIpAddr {
    let mut addr = [0u8; IPV6_ADDRESS_SIZE];
    addr.copy_from_slice(&frame[offset..offset + IPV6_ADDRESS_SIZE]);
    BmIpAddr(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::ETHERNET_TYPE_OFFSET;

    fn ipv6_frame() -> [u8; MIN_FRAME_WITH_ADDRESSES] {
        let mut frame = [0u8; MIN_FRAME_WITH_ADDRESSES];
        frame[ETHERNET_TYPE_OFFSET] = 0x86;
        frame[ETHERNET_TYPE_OFFSET + 1] = 0xDD;
        frame
    }

    fn set_dst(frame: &mut [u8], addr: &BmIpAddr) {
        frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
            .copy_from_slice(&addr.0);
    }

    #[test]
    fn short_frames_pass_through_untouched() {
        let mut frame = [0xAAu8; MIN_FRAME_WITH_ADDRESSES - 1];
        let before = frame;
        let result = rx_apply(&mut frame, 0b1, 0b11, None);
        assert_eq!(frame, before);
        assert_eq!(
            result,
            RxResult {
                should_submit: true,
                egress_mask: 0,
                ingress_port_num: 0
            }
        );
    }

    #[test]
    fn non_ipv6_frames_are_not_mutated() {
        let mut frame = ipv6_frame();
        frame[ETHERNET_TYPE_OFFSET] = 0x08; // ARP
        frame[ETHERNET_TYPE_OFFSET + 1] = 0x06;
        let before = frame;
        let result = rx_apply(&mut frame, 0b1, 0b11, None);
        assert_eq!(frame, before, "an ARP payload must not be treated as IPv6");
        assert_eq!(result.ingress_port_num, 0);
    }

    #[test]
    fn zero_ingress_mask_leaves_the_frame_alone() {
        let mut frame = ipv6_frame();
        let before = frame;
        let result = rx_apply(&mut frame, 0, 0b11, None);
        assert_eq!(frame, before);
        assert_eq!(result.ingress_port_num, 0);
    }

    #[test]
    fn ingress_port_is_encoded_in_the_upper_nibble() {
        let mut frame = ipv6_frame();
        frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = 0x0A; // egress nibble preset
        let result = rx_apply(&mut frame, 0b10, 0b11, None);
        assert_eq!(result.ingress_port_num, 2);
        assert_eq!(
            frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 0x2A,
            "egress nibble preserved"
        );
    }

    #[test]
    fn global_multicast_floods_every_port_but_ingress() {
        let mut frame = ipv6_frame();
        set_dst(&mut frame, &BmIpAddr::GLOBAL_MULTICAST);
        let result = rx_apply(&mut frame, 0b0001, 0b1111, None);
        assert_eq!(result.egress_mask, 0b1110);
        assert!(result.should_submit);
    }

    #[test]
    fn neighbor_multicast_never_consults_the_callback() {
        let mut frame = ipv6_frame();
        set_dst(&mut frame, &BmIpAddr::LINK_LOCAL_MULTICAST);
        let mut called = false;
        let mut cb = |_: u8, _: &mut u16, _: &mut BmIpAddr, _: &BmIpAddr| {
            called = true;
            false
        };
        let result = rx_apply(&mut frame, 0b1, 0b11, Some(&mut cb));
        assert!(!called, "FF02::1 is handled without the routing callback");
        assert!(result.should_submit);
        assert_eq!(result.egress_mask, 0);
    }

    #[test]
    fn callback_decides_submission_and_egress() {
        let mut frame = ipv6_frame();
        let mut dst = BmIpAddr::LINK_LOCAL_MULTICAST;
        dst.0[15] = 0x05; // FF02::5, not the neighbor address
        set_dst(&mut frame, &dst);

        let mut cb = |port: u8, egress: &mut u16, _: &mut BmIpAddr, _: &BmIpAddr| {
            *egress = u16::from(port) << 2;
            false
        };
        let result = rx_apply(&mut frame, 0b1, 0b11, Some(&mut cb));
        assert!(!result.should_submit);
        assert_eq!(result.egress_mask, 0b100);
    }

    #[test]
    fn forwarded_copy_clears_the_whole_ports_byte() {
        let mut frame = ipv6_frame();
        frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = 0x35;
        prepare_forwarded_copy(&mut frame);
        assert_eq!(frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET], 0x00);
    }
}
