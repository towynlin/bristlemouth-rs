//! [`bm_stack::Phy`] for the ADIN2111, over OPEN Alliance TC6 SPI.
//!
//! Built on the per-port frame I/O proposed in
//! [embassy-rs/embassy#7024](https://github.com/embassy-rs/embassy/pull/7024),
//! which carries the port of each frame in `PacketMeta::id`: the driver sets it
//! on receive, and reads it on transmit to pick an egress port.
//!
//! # Shape
//!
//! Unlike an ordinary `embassy-net` driver, nothing here speaks IP. A
//! Bristlemouth node does its own IPv6 and BCMP in [`bm_wire`], so what it
//! wants from the driver is raw frames with a port attached. [`new`] therefore
//! hands back the `Device` wrapped as a [`Phy`] and the driver's `Runner`
//! separately: **the runner must be spawned**, or nothing moves on the SPI bus.
//!
//! ```ignore
//! let (phy, runner) = bm_phy_adin2111::new(mac, &mut STATE, spi, int, reset, false).await;
//! spawner.must_spawn(adin_task(runner));
//! node.run(&mut phy).await;
//! ```
//!
//! # Ports are 1-based on both sides
//!
//! `bm-stack` numbers ports from 1 because the Bristlemouth specification does
//! — the number goes into a nibble of the IPv6 source address, where 0 means
//! "unknown". The driver's `PACKET_ID_PORT1`/`PACKET_ID_PORT2` are 1 and 2, and
//! it reports the same on receive. [`packet_id`] and [`ingress_port`] are split
//! out and tested because they are the only places a port number changes
//! representation, and an off-by-one there would be invisible until two nodes
//! disagreed about which wire a frame came from.
//!
//! # Status
//!
//! The driver dependency is a **git branch**, not a release. See `Cargo.toml`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use core::future::poll_fn;
use core::task::Poll;

use bm_stack::{Egress, Phy};
use embassy_net_adin1110::{
    Device, PACKET_ID_ALL_PORTS, PACKET_ID_PORT_MASK, PACKET_ID_PORT1, PACKET_ID_PORT2, PortLinks,
    TxPort, new_tc6,
};
use embedded_hal_1::digital::OutputPin;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;
use xarxa_driver::{Driver, PacketBuf};

/// Re-exported from the driver: a caller needs `State` to declare the storage
/// [`new`] borrows, and `Runner` and `Tc6` to name the type of the task it has
/// to spawn.
pub use embassy_net_adin1110::{MTU, Runner, State, Tc6};

/// Why a transfer failed.
///
/// Note that SPI errors do not appear here: the driver's runner owns the bus,
/// so a bus failure surfaces there rather than on a send or a receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PhyError {
    /// A port number the device does not have. Ports are 1-based, and 0 means
    /// "unknown" in the Bristlemouth specification, so it is never a transmit
    /// choice.
    NoSuchPort(u8),
    /// The frame is longer than a packet buffer.
    FrameTooLarge {
        /// Length of the frame.
        len: usize,
        /// What a buffer holds.
        capacity: usize,
    },
    /// The packet pool is empty. Size it with `XARXA_PACKET_BUF_COUNT` or the
    /// `packet-buf-count-*` features of `xarxa-driver`.
    OutOfBuffers,
}

/// An ADIN2111 as a [`Phy`].
pub struct Adin2111Phy<'d> {
    device: Device<'d>,
    links: PortLinks<'d>,
}

/// Bring the device up, returning it as a [`Phy`] and the runner that drives it.
///
/// **The runner must be spawned.** It owns the SPI bus and the interrupt line;
/// until it runs, no frame is transmitted or received.
///
/// `append_fcs_on_tx` has the host compute the Ethernet FCS rather than the
/// MAC; pass `false` unless the board needs otherwise.
pub async fn new<'d, const N_RX: usize, const N_TX: usize, SPI, INT, RST>(
    mac_addr: [u8; 6],
    state: &'d mut State<N_RX, N_TX>,
    spi: SPI,
    int: INT,
    reset: RST,
    append_fcs_on_tx: bool,
) -> (Adin2111Phy<'d>, Runner<'d, Tc6<SPI>, INT, RST>)
where
    SPI: SpiDevice,
    INT: Wait,
    RST: OutputPin,
{
    // Every frame names its own port in `PacketMeta::id`, so the configured
    // default is only ever used for `PACKET_ID_DEFAULT_PORT`, which this crate
    // never sends. `Port1` is the safe choice: `Port2` panics on an ADIN1110.
    let (device, runner) = new_tc6(
        mac_addr,
        state,
        spi,
        int,
        reset,
        append_fcs_on_tx,
        TxPort::Port1,
    )
    .await;
    let links = runner.port_links();
    (Adin2111Phy { device, links }, runner)
}

/// The same, with the MAC derived from the node id the way bm_core derives it
/// — locally administered, unicast, from the low 48 bits.
///
/// Using this keeps the MAC on the wire consistent with the one a peer computes
/// from the node id in our address.
pub async fn for_node<'d, const N_RX: usize, const N_TX: usize, SPI, INT, RST>(
    node_id: u64,
    state: &'d mut State<N_RX, N_TX>,
    spi: SPI,
    int: INT,
    reset: RST,
    append_fcs_on_tx: bool,
) -> (Adin2111Phy<'d>, Runner<'d, Tc6<SPI>, INT, RST>)
where
    SPI: SpiDevice,
    INT: Wait,
    RST: OutputPin,
{
    new(
        bm_wire::addr::mac_from_nodeid(node_id),
        state,
        spi,
        int,
        reset,
        append_fcs_on_tx,
    )
    .await
}

/// The `PacketMeta::id` that asks the driver for this egress.
///
/// `None` is a port the ADIN2111 does not have, which includes port 0.
#[must_use]
pub fn packet_id(egress: Egress) -> Option<u32> {
    match egress {
        Egress::AllPorts => Some(PACKET_ID_ALL_PORTS),
        Egress::Port(1) => Some(PACKET_ID_PORT1),
        Egress::Port(2) => Some(PACKET_ID_PORT2),
        Egress::Port(_) => None,
    }
}

/// The 1-based ingress port a received `PacketMeta::id` reports.
///
/// Anything the device could not have sent becomes 0, which is what the
/// Bristlemouth specification uses for "unknown" — a receiver that cannot tell
/// which wire a frame came from must not guess, because the number goes into
/// the source address and a neighbour is tracked per port.
#[must_use]
pub fn ingress_port(id: u32, port_count: u8) -> u8 {
    let port = (id & PACKET_ID_PORT_MASK) as u8;
    if port >= 1 && port <= port_count {
        port
    } else {
        0
    }
}

impl Phy for Adin2111Phy<'_> {
    type Error = PhyError;

    fn port_count(&self) -> u8 {
        self.links.port_count()
    }

    fn link_up(&self, port: u8) -> bool {
        // Guarded: `PortLinks::link_up` indexes `[AtomicBool; 2]` with
        // `port - 1` and panics for 0 or for a port above the array. The trait
        // here promises `false` instead.
        if port == 0 || port > self.links.port_count() {
            return false;
        }
        self.links.link_up(port)
    }

    async fn send(&mut self, frame: &[u8], egress: Egress) -> Result<(), Self::Error> {
        let id = packet_id(egress).ok_or(match egress {
            Egress::Port(port) => PhyError::NoSuchPort(port),
            Egress::AllPorts => PhyError::NoSuchPort(0),
        })?;

        let mut buf = PacketBuf::try_new().ok_or(PhyError::OutOfBuffers)?;
        if frame.len() > buf.capacity() {
            return Err(PhyError::FrameTooLarge {
                len: frame.len(),
                capacity: buf.capacity(),
            });
        }
        buf.set_len(frame.len());
        buf.copy_from_slice(frame);
        buf.meta_mut().id = id;

        let mut pending = Some(buf);
        poll_fn(|cx| {
            let buf = pending.take().expect("poll_fn polled after it was ready");
            match self.device.transmit(buf) {
                Ok(()) => return Poll::Ready(()),
                Err(returned) => pending = Some(returned),
            }
            let _ = self.device.register_waker(cx.waker());
            // Try again now the waker is registered: the queue may have drained
            // between the attempt above and the registration, and that wake is
            // already gone.
            let buf = pending.take().expect("put back just above");
            match self.device.transmit(buf) {
                Ok(()) => Poll::Ready(()),
                Err(returned) => {
                    pending = Some(returned);
                    Poll::Pending
                }
            }
        })
        .await;
        Ok(())
    }

    async fn receive(&mut self, out: &mut [u8]) -> Result<(u8, usize), Self::Error> {
        let packet = poll_fn(|cx| {
            if let Some(packet) = self.device.receive() {
                return Poll::Ready(packet);
            }
            let _ = self.device.register_waker(cx.waker());
            // Same reason as in `send`: re-check after registering.
            match self.device.receive() {
                Some(packet) => Poll::Ready(packet),
                None => Poll::Pending,
            }
        })
        .await;

        let port = ingress_port(packet.meta().id, self.links.port_count());
        let len = packet.len().min(out.len());
        out[..len].copy_from_slice(&packet[..len]);
        Ok((port, len))
    }
}

/// Compile-time proof that this adapter satisfies what [`bm_stack::Node::run`]
/// asks of a PHY.
///
/// Implementing a trait is not the same as being usable: `run` is generic, so
/// nothing checks the bounds line up until something instantiates it. Nothing
/// on a host ever will — that needs a real SPI bus — so this stands in. It is
/// never called; type-checking it is the whole point.
#[allow(dead_code)]
async fn assert_drives_a_node<I: bm_stack::Identity, const N: usize>(
    node: &mut bm_stack::Node<I, N>,
    phy: &mut Adin2111Phy<'_>,
) -> PhyError {
    node.run(phy).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORT_COUNT: u8 = 2;

    #[test]
    fn ports_are_one_based_on_both_sides() {
        assert_eq!(packet_id(Egress::Port(1)), Some(PACKET_ID_PORT1));
        assert_eq!(packet_id(Egress::Port(2)), Some(PACKET_ID_PORT2));
        assert_eq!(packet_id(Egress::AllPorts), Some(PACKET_ID_ALL_PORTS));

        assert_eq!(ingress_port(PACKET_ID_PORT1, PORT_COUNT), 1);
        assert_eq!(ingress_port(PACKET_ID_PORT2, PORT_COUNT), 2);
    }

    /// Port 0 is "unknown" in the Bristlemouth specification and is never an
    /// egress choice; anything above the port count is a caller bug. Neither
    /// may silently become port 1.
    #[test]
    fn ports_the_device_does_not_have_are_refused() {
        for port in [0u8, 3, 15, 255] {
            assert_eq!(packet_id(Egress::Port(port)), None, "port {port}");
        }
    }

    /// A received id the device could not have produced is reported as
    /// unknown rather than guessed at.
    #[test]
    fn an_unexpected_received_id_is_reported_as_unknown() {
        for id in [0u32, 3, 0xFF, 0x1234] {
            assert_eq!(ingress_port(id, PORT_COUNT), 0, "id {id:#x}");
        }
        // And on a one-port ADIN1110, port 2 is not a thing either.
        assert_eq!(ingress_port(PACKET_ID_PORT2, 1), 0);
        assert_eq!(ingress_port(PACKET_ID_PORT1, 1), 1);
    }

    /// The reserved bits above the port mask must not change the answer.
    #[test]
    fn bits_outside_the_port_mask_are_ignored() {
        let reserved = !PACKET_ID_PORT_MASK;
        assert_eq!(ingress_port(PACKET_ID_PORT1 | reserved, PORT_COUNT), 1);
        assert_eq!(ingress_port(PACKET_ID_PORT2 | reserved, PORT_COUNT), 2);
    }
}
