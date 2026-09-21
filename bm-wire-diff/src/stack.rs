//! Bringing bm_core's whole stack up, once per process.
//!
//! Some of bm_core's behaviour is only observable from outside: the functions
//! that produce it are `static` inside their translation unit, so there is
//! nothing to call. The way to compare against those is to drive the real
//! stack — inject a frame, run the tasks, read what reached the network device
//! — and treat the wire boundary as the interface.
//!
//! # One stack per process
//!
//! `bm_shim_stack_init` calls `bm_ip_init`, which calls `packet_init` with
//! `network/bm_linux.c`'s accessors. [`crate::bcmp`] calls `packet_init` with
//! its own. Whichever runs second wins, and the loser then reads frames
//! through the wrong accessors. **Nothing that uses this module may share a
//! process with `crate::bcmp`.**
//!
//! In practice that means every comparator built on this is driven from its
//! own file under `bm-wire-diff/tests/`, which cargo runs as a separate
//! binary, and its seeds go in [`crate::replay::STACK_TARGETS`].
//!
//! `bm_shim_reset` must not be called either: only `bm_l2_deinit` exists
//! upstream, so every other module keeps its file-scope state for the life of
//! the process and a reset would free objects those statics still point at.

use std::sync::{Mutex, MutexGuard, OnceLock};

use bm_stack::port::{Egress, Identity, Phy, RtcTimeAndDate, SoftRtc};
use bm_stack::{Node, Outbound, Reflood};
use bm_wire::bcmp::DeviceInfo;

/// Ports the capture device reports, from `SHIM_NUM_PORTS` in
/// `bm-wire-sys/csrc/bm_net_device_shim.c`.
pub const NUM_PORTS: u8 = 2;

/// Node id the stack is brought up with.
pub const NODE_ID: u64 = 0xC0FF_EE00_1234_5678;

/// Vendor id the stack is brought up with.
pub const VENDOR_ID: u16 = 0xBEEF;
/// Product id the stack is brought up with.
pub const PRODUCT_ID: u16 = 0x0042;
/// Git SHA the stack is brought up with.
pub const GIT_SHA: u32 = 0x1234_5678;
/// Hardware revision the stack is brought up with.
pub const HW_VERSION: u8 = 3;
/// Firmware version the stack is brought up with.
pub const FIRMWARE_VERSION: (u8, u8, u8) = (1, 2, 4);
/// Serial number the stack is brought up with.
pub const SERIAL_NUMBER: [u8; 16] = *b"bm-wire-diff\0\0\0\0";
/// Device name the stack is brought up with.
pub const DEVICE_NAME: &[u8] = b"stack-oracle";
/// Version string the stack is brought up with.
pub const VERSION_STRING: &[u8] = b"0.0.0-diff";

static ORACLE: OnceLock<Mutex<()>> = OnceLock::new();

/// Bring the stack up if it is not up yet, then take the lock that serialises
/// every use of it.
///
/// # Panics
///
/// If `device_init` or `bm_shim_stack_init` fails.
pub fn oracle() -> MutexGuard<'static, ()> {
    let lock = ORACLE.get_or_init(|| {
        unsafe {
            let cfg = bm_wire_sys::DeviceCfg {
                node_id: NODE_ID,
                git_sha: GIT_SHA,
                device_name: c"stack-oracle".as_ptr(),
                version_string: c"0.0.0-diff".as_ptr(),
                vendor_id: VENDOR_ID,
                product_id: PRODUCT_ID,
                hw_ver: HW_VERSION,
                ver_major: FIRMWARE_VERSION.0,
                ver_minor: FIRMWARE_VERSION.1,
                ver_patch: FIRMWARE_VERSION.2,
                sn: SERIAL_NUMBER,
            };
            assert_eq!(bm_wire_sys::device_init(cfg), bm_wire_sys::BmErr_BmOK);
            assert_eq!(
                bm_wire_sys::bm_shim_stack_init(),
                bm_wire_sys::BmErr_BmOK,
                "stack init"
            );
            // Both ports up. The shim passes this index straight to l2.c's
            // link_change, which is zero-based, so 0 and 1 are ports 1 and 2.
            bm_wire_sys::bm_shim_link_change(0, true);
            bm_wire_sys::bm_shim_link_change(1, true);
            pump_until_quiet();
            // Bringing a link up makes BCMP emit a heartbeat. Drop it: a
            // comparator only wants the frames it asked for.
            drain();
        }
        Mutex::new(())
    });
    lock.lock().unwrap_or_else(|p| p.into_inner())
}

/// Run every registered task once, until it goes idle.
pub fn pump() {
    unsafe { bm_wire_sys::bm_shim_pump() };
}

/// Pump until the stack has settled.
///
/// One pump is not enough, and the reason is worth knowing: `bm_shim_pump`
/// runs each registered task in the order it was created, and a task that
/// finds its queue empty returns. L2 is created before BCMP, so a received
/// frame is queued *for* BCMP only after L2 has run — and the reply BCMP then
/// writes is queued back to L2, which has already gone idle for this pump. A
/// request therefore takes more than one pump to become a transmitted reply,
/// and for most of those pumps nothing is transmitted at all.
///
/// So "stop when no new frames appeared" is wrong: it stops before the first
/// one. Each round advances every task by one step, and a message crosses each
/// task at most once on its way to the device, so `task_count + 1` rounds is
/// the floor. Keep going past that while frames are still appearing.
///
/// # Panics
///
/// If the stack has not settled after a generous number of rounds, which would
/// mean something is generating traffic on its own — the virtual clock does
/// not advance here, so nothing should.
pub fn pump_until_quiet() {
    const MAX_ROUNDS: u32 = 32;
    let minimum = unsafe { bm_wire_sys::bm_shim_task_count() } + 1;

    let mut last = unsafe { bm_wire_sys::bm_shim_tx_count() };
    for round in 0..MAX_ROUNDS {
        pump();
        let now = unsafe { bm_wire_sys::bm_shim_tx_count() };
        if round + 1 >= minimum && now == last {
            return;
        }
        last = now;
    }
    panic!("the stack never went quiet");
}

/// Drain the capture ring, returning `(egress port, frame)` in transmit order.
///
/// A port of 0 is the device's "all ports" encoding, which L2 uses for global
/// multicast when the mask covers every port.
///
/// # Panics
///
/// If a captured frame is longer than the drain buffer.
pub fn drain() -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    loop {
        let mut buf = vec![0u8; 2048];
        let mut port = 0u8;
        let len =
            unsafe { bm_wire_sys::bm_shim_tx_pop(buf.as_mut_ptr(), buf.len() as u32, &mut port) };
        if len < 0 {
            return out;
        }
        let len = len as usize;
        assert!(
            len <= buf.len(),
            "captured frame truncated by the drain buffer"
        );
        buf.truncate(len);
        out.push((port, buf));
    }
}

/// Deliver `frame` to L2 as if it arrived on `port`, then run the tasks.
///
/// # Panics
///
/// If the injection does not reach L2, or the capture ring overflows.
pub fn inject(port: u8, frame: &[u8]) {
    unsafe {
        assert_eq!(
            bm_wire_sys::bm_shim_rx_inject(port, frame.as_ptr(), frame.len() as u32),
            bm_wire_sys::BmErr_BmOK,
            "injection should reach L2"
        );
        pump_until_quiet();
        assert_eq!(
            bm_wire_sys::bm_shim_tx_dropped(),
            0,
            "capture ring overflowed"
        );
    }
}

// ---------------------------------------------------------------------------
// The same node, in Rust
// ---------------------------------------------------------------------------

/// The identity the oracle's stack was brought up with, as a [`bm_stack`]
/// [`Identity`].
///
/// Comparators that put the same question to both nodes need them to have
/// nothing to differ about but their code, so both read their configuration
/// from the constants above.
#[derive(Debug, Clone, Copy)]
pub struct OracleIdentity;

impl Identity for OracleIdentity {
    fn node_id(&self) -> u64 {
        NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor_id: VENDOR_ID,
            product_id: PRODUCT_ID,
            serial_num: SERIAL_NUMBER,
            git_sha: GIT_SHA,
            ver_major: FIRMWARE_VERSION.0,
            ver_minor: FIRMWARE_VERSION.1,
            ver_rev: FIRMWARE_VERSION.2,
            ver_hw: HW_VERSION,
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        VERSION_STRING
    }

    fn device_name(&self) -> &[u8] {
        DEVICE_NAME
    }
}

/// A `bm-stack` node with the oracle's identity, port count, link state and
/// clock.
///
/// [`oracle`] brings both of the capture device's ports up before any
/// comparison, and a neighbour-table reply carries that, so the port sets have
/// to match too. The clock starts **unset**, which is the state
/// `bm-wire-sys/csrc/bm_generic_shim.c` brings the C's RTC up in: `bm_rtc_get`
/// returns `BmENODATA` until something sets it, and a node in that state
/// answers no system-time request. A comparator that wants the two clocks to
/// agree calls [`set_both_clocks`].
#[must_use]
pub fn node() -> Node<OracleIdentity, SoftRtc, 4> {
    node_with_clock(SoftRtc::new())
}

/// The same, with a clock of the caller's choosing.
#[must_use]
pub fn node_with_clock(rtc: SoftRtc) -> Node<OracleIdentity, SoftRtc, 4> {
    let mut node = Node::new(OracleIdentity, rtc, NUM_PORTS);
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    node
}

/// Set the oracle's RTC, and hand back a [`SoftRtc`] reading the same thing.
///
/// `bm_rtc_set` and `bm_rtc_get` are integrator hooks: bm_core declares them
/// and defines neither, so there is no authoritative C behaviour to compare
/// against — only `csrc/bm_generic_shim.c`'s, which is ours. What *is*
/// bm_core's, and what the comparators check, is everything downstream of the
/// reading: which messages provoke an answer, and what goes in it.
///
/// Like everything else the oracle owns, this is process-global and survives
/// every seed, so a comparator that cares must set it rather than assume it.
///
/// # Panics
///
/// If the C refuses the value, which it does only for a null pointer.
pub fn set_both_clocks(reading: RtcTimeAndDate) -> SoftRtc {
    let c_reading = bm_wire_sys::RtcTimeAndDate {
        year: reading.year,
        month: reading.month,
        day: reading.day,
        hour: reading.hour,
        minute: reading.minute,
        second: reading.second,
        ms: reading.ms,
    };
    unsafe {
        assert_eq!(
            bm_wire_sys::bm_rtc_set(&c_reading),
            bm_wire_sys::BmErr_BmOK,
            "the shim's RTC accepts any reading"
        );
    }
    SoftRtc::at(reading)
}

/// What `bm_rtc_get_micro_seconds` makes of the oracle's current reading.
///
/// Reads it back out of the C rather than recomputing it, so a
/// [`RtcTimeAndDate::to_utc_micros`] that drifted from the shim's arithmetic
/// fails a comparison instead of hiding inside both sides.
///
/// # Panics
///
/// If the oracle's clock has not been set.
#[must_use]
pub fn oracle_clock_micros() -> u64 {
    let mut reading = bm_wire_sys::RtcTimeAndDate::default();
    unsafe {
        assert_eq!(
            bm_wire_sys::bm_rtc_get(&mut reading),
            bm_wire_sys::BmErr_BmOK,
            "the oracle's clock has not been set"
        );
        bm_wire_sys::bm_rtc_get_micro_seconds(&mut reading)
    }
}

/// A PHY that records what it is given and never receives anything.
///
/// Enough to drive `bm_stack::transmit`, which is the composition a comparator
/// wants to check: the frames a real node hands its driver, with the egress
/// port each copy went to. `bm_stack::mock::MockPhy` would do as well, but it
/// needs an `embassy-time` driver in the test binary, and nothing here needs a
/// clock.
#[derive(Debug, Default)]
pub struct CapturePhy {
    /// Every frame handed to the PHY, as `(egress port, bytes)`, in order. A
    /// port of 0 is the device's "all ports" encoding, matching [`drain`].
    pub sent: Vec<(u8, Vec<u8>)>,
}

/// Why a [`CapturePhy`] transfer failed. It never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreachable;

impl Phy for CapturePhy {
    type Error = Unreachable;

    fn port_count(&self) -> u8 {
        NUM_PORTS
    }

    fn link_up(&self, port: u8) -> bool {
        (1..=NUM_PORTS).contains(&port)
    }

    async fn send(&mut self, frame: &[u8], egress: Egress) -> Result<(), Self::Error> {
        let port = match egress {
            Egress::AllPorts => 0,
            Egress::Port(port) => port,
        };
        self.sent.push((port, frame.to_vec()));
        Ok(())
    }

    async fn receive(&mut self, _buf: &mut [u8]) -> Result<(u8, usize), Self::Error> {
        Err(Unreachable)
    }
}

/// Transmit one frame through a fresh [`CapturePhy`] and return what it saw.
///
/// # Panics
///
/// Never: [`CapturePhy`] cannot fail.
#[must_use]
pub fn capture(outbound: Outbound<'_>) -> Vec<(u8, Vec<u8>)> {
    let mut phy = CapturePhy::default();
    embassy_futures::block_on(bm_stack::transmit(&mut phy, outbound, NUM_PORTS))
        .expect("CapturePhy cannot fail");
    phy.sent
}

/// Run a node's re-flood through a fresh [`CapturePhy`] and return what it saw.
///
/// `frame` is the frame the [`Reflood`] was produced from. One frame per port
/// that is not the ingress one, built and transmitted in turn, which is the
/// order `bcmp_ll_forward` puts them on the wire in.
///
/// # Panics
///
/// Never: [`CapturePhy`] cannot fail.
#[must_use]
pub fn capture_reflood<R: bm_stack::Rtc>(
    node: &mut Node<OracleIdentity, R, 4>,
    reflood: Reflood,
    frame: &[u8],
) -> Vec<(u8, Vec<u8>)> {
    let mut phy = CapturePhy::default();
    embassy_futures::block_on(node.reflood(&mut phy, reflood, frame))
        .expect("CapturePhy cannot fail");
    phy.sent
}
