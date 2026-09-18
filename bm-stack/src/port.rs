//! The seams bm_core leaves to the integrator, as traits.
//!
//! bm_core declares `bm_os.h`, `bm_ip.h`, `network_device.h` and friends and
//! expects the integrator to supply definitions at link time. That works, but
//! it means a program can only have one of each, and a test cannot have a
//! different one from the firmware. These are the same seams expressed as
//! traits, so a node is generic over them and a mock is just another
//! implementation.
//!
//! Only the two the heartbeat-and-neighbours milestone actually needs are here.
//! Configuration storage, the RTC and the DFU flash slot are seams too, and
//! they will arrive with the code that uses them rather than ahead of it.

use bm_wire::bcmp::DeviceInfo;

/// Where a frame should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Egress {
    /// Every port at once. The ADIN2111 can do this in one transfer, and
    /// bm_core uses it for global multicast.
    AllPorts,
    /// One port, 1-based.
    Port(u8),
}

/// A port-aware Ethernet PHY.
///
/// The port number is the whole reason this is not just a byte pipe.
/// Bristlemouth encodes the ingress port into the source address of every
/// frame it receives and picks an egress port per copy on transmit, so a
/// driver that hides which port a frame came from cannot carry the protocol.
/// `embassy-net-adin1110` currently does hide it — fixing that upstream is
/// what this trait is waiting for.
#[allow(async_fn_in_trait)]
pub trait Phy {
    /// Why a transfer failed.
    type Error: core::fmt::Debug;

    /// How many ports the device has. Ports are numbered 1..=`port_count`.
    fn port_count(&self) -> u8;

    /// Whether the link on `port` is up, as of the last time the driver
    /// serviced the PHY. Ports are 1-based; a port the device does not have
    /// reports `false`.
    ///
    /// A neighbour-table reply carries this for every port, which is the one
    /// place bm_core reads `bm_l2_get_port_state`.
    fn link_up(&self, port: u8) -> bool;

    /// Transmit one frame.
    async fn send(&mut self, frame: &[u8], egress: Egress) -> Result<(), Self::Error>;

    /// Wait for a frame, returning the ingress port and the length written
    /// into `buf`. A frame longer than `buf` is truncated to it.
    async fn receive(&mut self, buf: &mut [u8]) -> Result<(u8, usize), Self::Error>;
}

/// What this node says about itself when asked.
///
/// The equivalent of `common/device.h`'s `DeviceCfg`, minus the parts nothing
/// reads yet.
pub trait Identity {
    /// This node's 64-bit id. Its addresses and its MAC are derived from it.
    fn node_id(&self) -> u64;

    /// The fixed half of a device-info reply. `node_id` is overwritten with
    /// [`Self::node_id`], so an implementation may leave it zero.
    fn device_info(&self) -> DeviceInfo;

    /// Firmware version string. At most 255 bytes reach the wire.
    fn version_string(&self) -> &[u8] {
        b""
    }

    /// Device name. At most 255 bytes reach the wire.
    fn device_name(&self) -> &[u8] {
        b""
    }
}
