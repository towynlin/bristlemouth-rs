//! The seams bm_core leaves to the integrator, as traits.
//!
//! bm_core declares `bm_os.h`, `bm_ip.h`, `network_device.h` and friends and
//! expects the integrator to supply definitions at link time. That works, but
//! it means a program can only have one of each, and a test cannot have a
//! different one from the firmware. These are the same seams expressed as
//! traits, so a node is generic over them and a mock is just another
//! implementation.
//!
//! Only the seams the ported exchanges actually need are here. Configuration
//! storage and the DFU flash slot are seams too, and they will arrive with the
//! code that uses them rather than ahead of it.

use bm_wire::bcmp::DeviceInfo;
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

/// A wall-clock reading, `RtcTimeAndDate` from `bcmp/bm_rtc.h`.
///
/// Nothing in bm_core builds one of these from the wire: the field order here
/// is the C struct's, but the struct never leaves the device, so this is a
/// plain value type rather than a codec. The wire carries
/// [`bm_wire::bcmp::SystemTimeResponse::utc_time_us`] instead, and the two
/// conversions between them are [`Self::to_utc_micros`] and
/// [`Self::from_utc_micros`].
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
    /// left to the integrator, so unlike everything else in this repository
    /// there is no authoritative C to match: `bm-wire-sys/csrc/bm_generic_shim.c`
    /// implements it as `utc_from_date_time(...) * 1_000_000 + ms * 1_000`, and
    /// this is the same arithmetic so that the differential harness compares a
    /// node against a node rather than against a different clock.
    ///
    /// [`utc_from_date_time`] returns a `u32`, which is bm_core's own choice
    /// and runs out in 2106; the multiply is done in 64 bits afterwards, as the
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
    /// back therefore does not round-trip, and the response the C sends echoes
    /// the *requested* microseconds rather than what the RTC kept.
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
/// and defines none of them; a firmware links its own. Here it is a trait, so
/// a node without a clock ([`NoRtc`]) and a node with one are different types
/// rather than different link lines.
///
/// This is what the system-time exchange stands on: `0x10` is answered from
/// [`Self::get`], `0x12` is applied through [`Self::set`], and a node whose
/// clock refuses either says nothing at all — which is exactly what a C node
/// does when its `bm_rtc_get` returns anything but `BmOK`.
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
/// Both operations fail, which is a state a C node can be in too — an
/// integrator that links stubs returning an error, or a board whose RTC has
/// never been set. Such a node stays silent when asked for the time, and
/// re-floods and drops time messages exactly as it would otherwise.
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
/// Exactly what `bm_rtc_set` and `bm_rtc_get` do in
/// `bm-wire-sys/csrc/bm_generic_shim.c`: remember what was set, refuse to be
/// read until something has set it. Two uses, and both are real —
/// a test that needs a node whose clock answers, and a board with no RTC
/// part, whose time comes from a `0x12` message and is lost on reset.
///
/// It is not gated behind the `mock` feature because there is nothing to mock:
/// no clock advances on its own here, and a node built on this one reports the
/// same time until it is set again. A firmware wanting a clock that *runs*
/// implements [`Rtc`] over its own peripheral.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion the shim performs, checked against a date a human can
    /// verify: 2026-09-21T00:00:00Z is 1 758 412 800 seconds after the epoch.
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
