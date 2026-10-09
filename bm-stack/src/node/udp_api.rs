//! UDP: binding ports and sending datagrams. Delivery is in [`Node::on_frame`].

use bm_wire::l2;
use bm_wire::pubsub;
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::config::Configuration;
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};
use crate::service::Services;

use super::{Node, Outbound, UdpBindError};

#[cfg(doc)]
use super::{Event, MTU, UDP_PORTS, transmit};

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// Bind `port`, so datagrams to it arrive as [`Event::Udp`] —
    /// `bm_udp_bind_port`.
    ///
    /// The C also takes a multicast group to join, which `bm_linux.c` ignores
    /// and `bm_lwip.c` passes to MLD; this node filters on no destination
    /// address, so it takes none.
    ///
    /// # Errors
    ///
    /// [`UdpBindError::InUse`] for a port already bound, including
    /// [`pubsub::PORT`], which pub/sub holds. lwIP's `udp_bind`
    /// refuses the same, and `bm_lwip.c` ignores its return, leaving a pcb
    /// that receives nothing. [`UdpBindError::Full`] once [`UDP_PORTS`] are
    /// bound.
    pub fn bind_udp(&mut self, port: u16) -> Result<(), UdpBindError> {
        if self.udp_bound(port) {
            return Err(UdpBindError::InUse);
        }
        let slot = self
            .udp_ports
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(UdpBindError::Full)?;
        *slot = Some(port);
        Ok(())
    }

    /// Unbind `port`, reporting whether it was bound. bm_core has no
    /// counterpart: its UDP list is never removed from.
    pub fn unbind_udp(&mut self, port: u16) -> bool {
        match self.udp_ports.iter_mut().find(|slot| **slot == Some(port)) {
            Some(slot) => {
                *slot = None;
                true
            }
            None => false,
        }
    }

    /// Whether `port` is bound. [`pubsub::PORT`] always is.
    #[must_use]
    pub fn udp_bound(&self, port: u16) -> bool {
        port == pubsub::PORT || self.udp_ports.contains(&Some(port))
    }

    /// Build a UDP datagram from `src_port` to `dst` port `dst_port` —
    /// `bm_udp_tx_perform`, then `bm_l2_link_output`.
    ///
    /// The source address is [`udp::source_address`], `fd00::<id>` for
    /// `FF03::1`, as lwIP chooses it. `src_port` need not be bound: the C sends
    /// from a pcb, which is always bound, and a caller here names the port
    /// instead.
    ///
    /// The mask is every port, unless byte 13 of `dst` requests one, which
    /// `bm_l2_link_output` reads and clears after the checksum is computed
    /// ([`l2::take_requested_egress_port`]). [`transmit`] then sends
    /// `FF03::1` once to all ports unstamped and link-local multicast once per
    /// port stamped, and drops anything else, as `bm_l2_process_tx_evt` does.
    ///
    /// Returns `None` if the frame would exceed [`MTU`]. lwIP would fragment
    /// it instead; `bm_middleware_net_tx` refuses a payload over
    /// `max_payload_len_udp` before it gets that far.
    pub fn send_udp(
        &mut self,
        src_port: u16,
        dst: &BmIpAddr,
        dst_port: u16,
        payload: &[u8],
    ) -> Option<Outbound<'_>> {
        let src = udp::source_address(self.identity.node_id(), dst);
        let end = udp::build(&mut self.tx[..], &src, dst, src_port, dst_port, payload).ok()?;
        let frame = &mut self.tx[..end];
        let mask = l2::take_requested_egress_port(frame, self.port_count).ok()?;
        Some(Outbound { frame, mask })
    }
}
