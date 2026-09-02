//! [`bm_stack::Phy`] for the ADIN2111, over OPEN Alliance TC6 SPI.
//!
//! The adapter is thin, and deliberately so: `embassy-net-adin1110`'s `PortIo`
//! was shaped to fit this trait, so almost everything here is a rename. The
//! two things it does carry are the port-numbering convention and the error
//! type.
//!
//! # Ports are 1-based on both sides
//!
//! `bm-stack` numbers ports from 1 because the Bristlemouth specification does
//! — the number goes into a nibble of the IPv6 source address, where 0 means
//! "unknown". `PortIo` numbers them from 1 as well, and keeps the hardware's
//! 0-based vendor-specific encoding inside its protocol layer. So there is no
//! conversion here, which is the point: an off-by-one in this file would be
//! invisible until two nodes disagreed about which wire a frame came from.
//!
//! # Status
//!
//! The driver dependency is a **git branch**, not a release. See `Cargo.toml`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use bm_stack::{Egress, Phy};
use embassy_net_adin1110::{AdinError, PortIo, TxPort, new_tc6_port_io};
use embedded_hal_1::digital::OutputPin;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;

use embassy_net_adin1110 as adin;

/// Why a transfer failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum PhyError<E> {
    /// The driver reported an error.
    Adin(AdinError<E>),
    /// A port number the device does not have. Ports are 1-based.
    NoSuchPort(u8),
}

impl<E> From<AdinError<E>> for PhyError<E> {
    fn from(error: AdinError<E>) -> Self {
        Self::Adin(error)
    }
}

/// An ADIN2111 as a [`Phy`].
pub struct Adin2111Phy<SPI: SpiDevice, INT: Wait, RST: OutputPin> {
    io: PortIo<SPI, INT, RST>,
}

impl<SPI: SpiDevice, INT: Wait, RST: OutputPin> Adin2111Phy<SPI, INT, RST> {
    /// Bring the device up and wrap it.
    ///
    /// `append_fcs_on_tx` has the host compute the Ethernet FCS rather than
    /// the MAC; pass `false` unless the board needs otherwise.
    pub async fn new(
        mac_addr: [u8; 6],
        spi: SPI,
        int: INT,
        reset: RST,
        append_fcs_on_tx: bool,
    ) -> Self {
        Self {
            io: new_tc6_port_io(mac_addr, spi, int, reset, append_fcs_on_tx).await,
        }
    }

    /// The same, with the MAC derived from the node id the way bm_core derives
    /// it — locally administered, unicast, from the low 48 bits.
    ///
    /// Using this keeps the MAC on the wire consistent with the one a peer
    /// computes from the node id in our address.
    pub async fn for_node(
        node_id: u64,
        spi: SPI,
        int: INT,
        reset: RST,
        append_fcs_on_tx: bool,
    ) -> Self {
        Self::new(
            bm_wire::addr::mac_from_nodeid(node_id),
            spi,
            int,
            reset,
            append_fcs_on_tx,
        )
        .await
    }

    /// The driver underneath, for anything this adapter does not expose.
    pub fn port_io(&mut self) -> &mut PortIo<SPI, INT, RST> {
        &mut self.io
    }
}

/// Map an egress selection onto the driver's transmit port.
///
/// Split out and tested because it is the one place a port number changes
/// representation. `None` is a port the ADIN2111 does not have, which includes
/// port 0 — that is "unknown" in the Bristlemouth specification and is never a
/// transmit choice.
fn tx_port(egress: Egress) -> Option<TxPort> {
    match egress {
        Egress::AllPorts => Some(TxPort::Flood),
        Egress::Port(1) => Some(TxPort::Port1),
        Egress::Port(2) => Some(TxPort::Port2),
        Egress::Port(_) => None,
    }
}

impl<SPI: SpiDevice, INT: Wait, RST: OutputPin> Phy for Adin2111Phy<SPI, INT, RST> {
    type Error = PhyError<SPI::Error>;

    fn port_count(&self) -> u8 {
        self.io.port_count()
    }

    fn link_up(&self, port: u8) -> bool {
        self.io.link_up(port)
    }

    async fn send(&mut self, frame: &[u8], egress: Egress) -> Result<(), Self::Error> {
        let tx = tx_port(egress).ok_or_else(|| match egress {
            Egress::Port(port) => PhyError::NoSuchPort(port),
            Egress::AllPorts => unreachable!("AllPorts always maps to Flood"),
        })?;
        self.io.send(frame, tx).await.map_err(PhyError::Adin)
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<(u8, usize), Self::Error> {
        self.io.receive(buf).await.map_err(PhyError::Adin)
    }
}

/// The number of ports the driver reports for an ADIN2111.
pub const PORT_COUNT: u8 = adin::ADIN2111_PORT_COUNT;

/// Compile-time proof that this adapter satisfies what [`bm_stack::Node::run`]
/// asks of a PHY.
///
/// Implementing a trait is not the same as being usable: `run` is generic, so
/// nothing checks the bounds line up until something instantiates it. Nothing
/// on a host ever will — that needs a real SPI bus — so this stands in. It is
/// never called; type-checking it is the whole point.
#[allow(dead_code)]
async fn assert_drives_a_node<SPI, INT, RST, I, const N: usize>(
    node: &mut bm_stack::Node<I, N>,
    phy: &mut Adin2111Phy<SPI, INT, RST>,
) -> PhyError<SPI::Error>
where
    SPI: SpiDevice,
    INT: Wait,
    RST: OutputPin,
    I: bm_stack::Identity,
{
    node.run(phy).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_one_based_on_both_sides() {
        assert_eq!(PORT_COUNT, 2);
        assert_eq!(tx_port(Egress::Port(1)), Some(TxPort::Port1));
        assert_eq!(tx_port(Egress::Port(2)), Some(TxPort::Port2));
        assert_eq!(tx_port(Egress::AllPorts), Some(TxPort::Flood));
    }

    /// Port 0 is "unknown" in the Bristlemouth spec and is never an egress
    /// choice; anything above the port count is a caller bug. Neither may
    /// silently become port 1.
    #[test]
    fn ports_the_device_does_not_have_are_refused() {
        for port in [0u8, 3, 15, 255] {
            assert_eq!(tx_port(Egress::Port(port)), None, "port {port}");
        }
    }
}
