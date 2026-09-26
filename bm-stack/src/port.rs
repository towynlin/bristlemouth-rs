//! The seams bm_core leaves to the integrator, as traits.
//!
//! bm_core declares `bm_os.h`, `bm_ip.h`, `network_device.h` and friends and
//! expects definitions at link time, so a program can only have one of each
//! and a test cannot differ from the firmware. These are the same seams as
//! traits, so a node is generic over them and a mock is another
//! implementation.
//!
//! Only the seams the ported exchanges need are here. The DFU flash slot
//! arrives with the code that uses it.

use bm_wire::bcmp::DeviceInfo;
use bm_wire::configuration::{MAX_IMAGE_LEN, Partition};
use bm_wire::util::{date_time_from_utc, utc_from_date_time};

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
/// Bristlemouth encodes the ingress port into the source address of every
/// frame it receives and picks an egress port per copy on transmit, so a
/// driver that hides which port a frame came from cannot carry the protocol.
/// `embassy-net-adin1110` currently hides it; see
/// `docs/embassy-port-tracking-prompt.md`.
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
    /// A neighbour-table reply carries this for every port — the one place
    /// bm_core reads `bm_l2_get_port_state`.
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

/// A wall-clock reading, `RtcTimeAndDate` from `bcmp/bm_rtc.h`.
///
/// A plain value type, not a codec: the struct never leaves the device. The
/// wire carries [`bm_wire::bcmp::SystemTimeResponse::utc_time_us`], and
/// [`Self::to_utc_micros`] and [`Self::from_utc_micros`] convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RtcTimeAndDate {
    /// Full year, e.g. 2026.
    pub year: u16,
    /// Month, 1-12.
    pub month: u8,
    /// Day of month, starting at 1.
    pub day: u8,
    /// Hour, 0-23.
    pub hour: u8,
    /// Minute, 0-59.
    pub minute: u8,
    /// Second, 0-59.
    pub second: u8,
    /// Milliseconds within the second. The RTC's whole sub-second resolution:
    /// microseconds arriving over the wire are truncated to it.
    pub ms: u16,
}

impl RtcTimeAndDate {
    /// Microseconds since the Unix epoch — `bm_rtc_get_micro_seconds`.
    ///
    /// **bm_core does not define this.** It is declared in `bcmp/bm_rtc.h` and
    /// left to the integrator, so there is no authoritative C to match.
    /// `bm-wire-sys/csrc/bm_generic_shim.c` implements it as
    /// `utc_from_date_time(...) * 1_000_000 + ms * 1_000`; this is the same
    /// arithmetic, so the harness compares a node against a node rather than
    /// against a different clock.
    ///
    /// [`utc_from_date_time`] returns a `u32` — bm_core's choice, which runs
    /// out in 2106 — and the multiply is done in 64 bits afterwards, as the
    /// shim does it.
    #[must_use]
    pub fn to_utc_micros(&self) -> u64 {
        let seconds = u64::from(utc_from_date_time(
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
        ));
        seconds * 1_000_000 + u64::from(self.ms) * 1_000
    }

    /// The reading `bcmp_time_process_time_set_msg` writes to the RTC for a
    /// `BcmpSystemTimeSet` carrying `utc_us`.
    ///
    /// This half *is* bm_core's: `date_time_from_utc` followed by the
    /// field-by-field copy at `time.c:103-110`, including `usec / 1000`, which
    /// discards the sub-millisecond part. Setting a node's clock and reading it
    /// back does not round-trip, and the C's response echoes the *requested*
    /// microseconds rather than what the RTC kept.
    #[must_use]
    pub fn from_utc_micros(utc_us: u64) -> Self {
        let datetime = date_time_from_utc(utc_us);
        Self {
            year: datetime.year,
            month: datetime.month,
            day: datetime.day,
            hour: datetime.hour,
            minute: datetime.min,
            second: datetime.sec,
            // `(datetime.usec / 1000)` assigned to a `uint16_t`. usec is under
            // a million, so the quotient is under 1000 and the narrowing the C
            // does implicitly cannot lose anything.
            ms: (datetime.usec / 1000) as u16,
        }
    }
}

/// The node's real-time clock, `bcmp/bm_rtc.h`.
///
/// bm_core declares `bm_rtc_get`, `bm_rtc_set` and `bm_rtc_get_micro_seconds`
/// and defines none of them. As a trait, a node without a clock ([`NoRtc`])
/// and a node with one are different types rather than different link lines.
///
/// The system-time exchange stands on this: `0x10` is answered from
/// [`Self::get`], `0x12` applied through [`Self::set`], and a node whose clock
/// refuses either says nothing — what a C node does when `bm_rtc_get` returns
/// anything but `BmOK`.
pub trait Rtc {
    /// Read the clock — `bm_rtc_get`.
    ///
    /// `None` stands for every error the C can return, including the
    /// "never set" case: `bcmp_time_process_time_request_msg` only tests for
    /// `BmOK`, so one failure is as good as another.
    fn get(&self) -> Option<RtcTimeAndDate>;

    /// Set the clock — `bm_rtc_set`. `false` is any error.
    ///
    /// A failure is silent on the wire: the C logs and sends no response.
    fn set(&mut self, time_and_date: &RtcTimeAndDate) -> bool;
}

/// A node with no real-time clock.
///
/// Both operations fail, a state a C node can be in too: stubs that return an
/// error, or a board whose RTC has never been set. Such a node stays silent
/// when asked the time, and re-floods and drops time messages as usual.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoRtc;

impl Rtc for NoRtc {
    fn get(&self) -> Option<RtcTimeAndDate> {
        None
    }

    fn set(&mut self, _time_and_date: &RtcTimeAndDate) -> bool {
        false
    }
}

/// A clock kept in RAM, with no hardware behind it.
///
/// What `bm_rtc_set` and `bm_rtc_get` do in
/// `bm-wire-sys/csrc/bm_generic_shim.c`: remember what was set, refuse to be
/// read until something sets it. Two uses — a test needing a node whose clock
/// answers, and a board with no RTC part, whose time comes from a `0x12`
/// message and is lost on reset.
///
/// Not behind the `mock` feature, because nothing here is mocked: it does not
/// advance, so it reports the same time until set again. A firmware wanting a
/// clock that *runs* implements [`Rtc`] over its own peripheral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SoftRtc {
    reading: Option<RtcTimeAndDate>,
    read_only: bool,
}

impl SoftRtc {
    /// A clock nothing has set yet: [`Rtc::get`] fails until [`Rtc::set`]
    /// succeeds, which is the state the C shim comes up in.
    #[must_use]
    pub fn new() -> Self {
        Self {
            reading: None,
            read_only: false,
        }
    }

    /// A clock already reading `reading`.
    #[must_use]
    pub fn at(reading: RtcTimeAndDate) -> Self {
        Self {
            reading: Some(reading),
            read_only: false,
        }
    }

    /// A clock that reads but refuses to be set — the `bm_rtc_set` failure
    /// path, which makes a node adopt nothing and answer nothing.
    #[must_use]
    pub fn read_only(reading: RtcTimeAndDate) -> Self {
        Self {
            reading: Some(reading),
            read_only: true,
        }
    }

    /// What it currently reads, without going through [`Rtc::get`].
    #[must_use]
    pub fn reading(&self) -> Option<RtcTimeAndDate> {
        self.reading
    }
}

impl Rtc for SoftRtc {
    fn get(&self) -> Option<RtcTimeAndDate> {
        self.reading
    }

    fn set(&mut self, time_and_date: &RtcTimeAndDate) -> bool {
        if self.read_only {
            return false;
        }
        self.reading = Some(*time_and_date);
        true
    }
}

/// Non-volatile storage for the config partitions, `bcmp/bm_configs_generic.h`.
///
/// bm_core declares `bm_config_read`, `bm_config_write` and `bm_config_reset`
/// and defines none of them, so there is no oracle for this seam: the
/// harness gives the Rust store and the C the same bytes and compares what
/// each does with them. `configuration.c` always reads and writes one whole
/// image at offset 0, with a timeout of
/// [`bm_wire::configuration::CONFIG_LOAD_TIMEOUT_MS`].
pub trait ConfigStorage {
    /// `bm_config_read`: fill `buf` from `offset` in `partition`. Whatever is
    /// written to `buf` stays there even if this returns `false`, and the
    /// store then treats the partition as unloadable.
    fn read(&mut self, partition: Partition, offset: u32, buf: &mut [u8], timeout_ms: u32) -> bool;

    /// `bm_config_write`: store `buf` at `offset` in `partition`.
    fn write(&mut self, partition: Partition, offset: u32, buf: &[u8], timeout_ms: u32) -> bool;

    /// `bm_config_reset`, which bm_core documents as "reset the processor"
    /// and calls after a save that asked for a restart, so the saved
    /// configuration takes effect. On hardware it does not return.
    fn reset(&mut self);
}

/// Config partitions kept in RAM.
///
/// What bm-wire-sys's shim does, minus its `bm_config_reset`, which clears
/// every partition; here [`ConfigStorage::reset`] does nothing, since there is
/// no processor to reset. Contents are zero until written, and are lost with
/// the value.
#[derive(Clone)]
pub struct RamConfigStorage {
    partitions: [[u8; MAX_IMAGE_LEN]; 3],
}

impl core::fmt::Debug for RamConfigStorage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RamConfigStorage").finish_non_exhaustive()
    }
}

impl Default for RamConfigStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl RamConfigStorage {
    /// Three zeroed partitions.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            partitions: [[0; MAX_IMAGE_LEN]; 3],
        }
    }

    /// One partition's bytes.
    #[must_use]
    pub fn bytes(&self, partition: Partition) -> &[u8] {
        &self.partitions[partition as usize]
    }

    /// One partition's bytes, mutably — for corrupting an image in a test.
    pub fn bytes_mut(&mut self, partition: Partition) -> &mut [u8] {
        &mut self.partitions[partition as usize]
    }

    fn range(offset: u32, len: usize) -> Option<core::ops::Range<usize>> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len)?;
        (end <= MAX_IMAGE_LEN).then_some(start..end)
    }
}

impl ConfigStorage for RamConfigStorage {
    fn read(
        &mut self,
        partition: Partition,
        offset: u32,
        buf: &mut [u8],
        _timeout_ms: u32,
    ) -> bool {
        let Some(range) = Self::range(offset, buf.len()) else {
            return false;
        };
        buf.copy_from_slice(&self.partitions[partition as usize][range]);
        true
    }

    fn write(&mut self, partition: Partition, offset: u32, buf: &[u8], _timeout_ms: u32) -> bool {
        let Some(range) = Self::range(offset, buf.len()) else {
            return false;
        };
        self.partitions[partition as usize][range].copy_from_slice(buf);
        true
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion the shim performs, checked against a date a human can
    /// verify: 2026-09-21T00:00:00Z is 1 789 948 800 seconds after the epoch.
    #[test]
    fn a_reading_converts_to_the_microseconds_the_shim_would_report() {
        let reading = RtcTimeAndDate {
            year: 2026,
            month: 9,
            day: 21,
            hour: 0,
            minute: 0,
            second: 0,
            ms: 0,
        };
        assert_eq!(reading.to_utc_micros(), 1_789_948_800_000_000);

        let with_ms = RtcTimeAndDate { ms: 250, ..reading };
        assert_eq!(with_ms.to_utc_micros(), 1_789_948_800_250_000);
    }

    /// Sub-millisecond precision does not survive a set, so the round trip is
    /// lossy in one direction only.
    #[test]
    fn a_set_truncates_below_the_millisecond() {
        let utc_us = 1_789_948_800_250_999u64;
        let reading = RtcTimeAndDate::from_utc_micros(utc_us);
        assert_eq!(reading.ms, 250, "999 microseconds are dropped");
        assert_eq!(
            reading.to_utc_micros(),
            1_789_948_800_250_000,
            "and cannot be recovered"
        );
        assert_eq!(reading.year, 2026);
        assert_eq!((reading.month, reading.day), (9, 21));
    }

    #[test]
    fn a_node_without_a_clock_fails_both_ways() {
        let mut rtc = NoRtc;
        assert_eq!(rtc.get(), None);
        assert!(!rtc.set(&RtcTimeAndDate::default()));
    }

    #[test]
    fn a_soft_clock_answers_only_once_it_has_been_set() {
        let noon = RtcTimeAndDate {
            year: 2026,
            month: 9,
            day: 21,
            hour: 12,
            minute: 0,
            second: 0,
            ms: 0,
        };

        let mut rtc = SoftRtc::new();
        assert_eq!(rtc.get(), None, "unset, exactly as the C shim comes up");
        assert!(rtc.set(&noon));
        assert_eq!(rtc.get(), Some(noon));

        let mut refuses = SoftRtc::read_only(noon);
        assert_eq!(refuses.get(), Some(noon));
        assert!(!refuses.set(&RtcTimeAndDate::default()));
        assert_eq!(refuses.get(), Some(noon), "and kept what it had");

        assert_eq!(SoftRtc::at(noon).reading(), Some(noon));
    }
}
