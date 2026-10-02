//! The STM32U575's RTC on LSE, as bm_protocol's `src/lib/drivers/stm32_rtc.c`
//! drives it: `bm_stack::Rtc` is `bm_rtc_get` and `bm_rtc_set`
//! (`src/lib/drivers/bm_rtc_wrapper.c`) over `rtcGet` and `rtcSet`.
//!
//! | `stm32_rtc.c` | Here |
//! |---|---|
//! | `rtcInit`: 24 h, prescalers 127/255, `BYPSHAD` (`:31-66`) | [`DevkitRtc::new`]: embassy's `Rtc::new` at 256 Hz, which writes the same and skips the write when they are already set |
//! | `isRTCSet`: backup register `DR0` is `RTC_SET_MAGIC` (`:68-76`) | [`DevkitRtc::is_set`]; `DR0` is `TAMP_BKP0R` on the U5 |
//! | `rtcGet` (`:155-199`) | [`Rtc::get`]: reread TR/DR until two reads agree, milliseconds from `calculate_rtc_ms`, `decrement_one_second` while `SSR > PREDIV_S` |
//! | `rtcSet` (`:217-267`) | [`Rtc::set`]: TR/DR with weekday Monday, then a `SHIFTR` advance of `ms`, then `DR0` |
//!
//! Where it differs:
//!
//! | C | Here |
//! |---|---|
//! | `rtcGet` reads `TR` and `DR` once per field, `SSR` twice | once per register per pass, `SSR` once |
//! | `rtcSet` with a year outside 2000-2099 writes its low BCD digits | refused: `set` returns `false` and leaves the clock |

use bm_stack::{Rtc, RtcTimeAndDate};
use embassy_stm32::Peri;
use embassy_stm32::pac::rtc::regs::Shiftr;
use embassy_stm32::pac::rtc::vals::Key;
use embassy_stm32::pac::{RTC, TAMP};
use embassy_stm32::peripherals;
use embassy_stm32::rtc::{DateTime, DayOfWeek, RtcConfig};

/// `RTC_SET_MAGIC`, written to backup register 0 once the clock is set.
pub const RTC_SET_MAGIC: u32 = 0x836A_20DD;

/// `SHIFTR.ADD1S`, `LL_RTC_SHIFT_SECOND_ADVANCE`.
const ADD1S: u32 = 1 << 31;

/// `monthDays` in `stm32_rtc.c`.
const MONTH_DAYS: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// The dev kit's RTC.
pub struct DevkitRtc {
    rtc: embassy_stm32::rtc::Rtc,
}

impl DevkitRtc {
    /// Enable the RTC and set its prescalers, as `rtcInit`. The calendar and
    /// `DR0` are in the backup domain, so a time set before a reset, by this
    /// firmware or the C, still reads.
    ///
    /// LSE must already clock the RTC: [`crate::config`] selects it.
    #[must_use]
    pub fn new(rtc: Peri<'static, peripherals::RTC>) -> Self {
        // 32768 Hz / 256 Hz: PREDIV_A 127, PREDIV_S 255, as `stm32_rtc.c:51`.
        let (rtc, _) = embassy_stm32::rtc::Rtc::new(rtc, RtcConfig::default());
        Self { rtc }
    }

    /// `isRTCSet`.
    #[must_use]
    pub fn is_set(&self) -> bool {
        TAMP.bkpr(0).read().bkp() == RTC_SET_MAGIC
    }
}

impl Rtc for DevkitRtc {
    fn get(&self) -> Option<RtcTimeAndDate> {
        if !self.is_set() {
            return None;
        }
        let mut previous = read_calendar();
        let mut reading = loop {
            let current = read_calendar();
            if current == previous {
                break current;
            }
            previous = current;
        };
        let ss = RTC.ssr().read().ss();
        let prediv = u32::from(RTC.prer().read().prediv_s());
        reading.ms = calculate_rtc_ms(ss, prediv);
        if ss > prediv {
            decrement_one_second(&mut reading);
        }
        Some(reading)
    }

    fn set(&mut self, t: &RtcTimeAndDate) -> bool {
        if !(2000..=2099).contains(&t.year) {
            return false;
        }
        let Ok(datetime) = DateTime::from(
            t.year,
            t.month,
            t.day,
            DayOfWeek::Monday,
            t.hour,
            t.minute,
            t.second,
            0,
        ) else {
            return false;
        };
        if self.rtc.set_datetime(datetime).is_err() {
            return false;
        }

        // Setting TR resets SSR to PREDIV_S, so the clock is advanced by `ms`:
        // add one second, subtract `adjust` ticks. `LL_RTC_TIME_Synchronize`
        // writes the sum unmasked.
        let pre = u32::from(RTC.prer().read().prediv_s()) + 1;
        let ms = u32::from(t.ms);
        let adjust = 1000u32.wrapping_mul(pre).wrapping_sub(ms.wrapping_mul(pre)) / 1000;
        RTC.wpr().write(|w| w.set_key(Key::Deactivate1));
        RTC.wpr().write(|w| w.set_key(Key::Deactivate2));
        RTC.shiftr().write_value(Shiftr(ADD1S | adjust));
        RTC.wpr().write(|w| w.set_key(Key::Activate));
        while RTC.icsr().read().shpf() {}

        TAMP.bkpr(0).write(|w| w.set_bkp(RTC_SET_MAGIC));
        true
    }
}

/// The calendar fields of `TR` and `DR`, `ms` zero.
fn read_calendar() -> RtcTimeAndDate {
    let tr = RTC.tr().read();
    let dr = RTC.dr().read();
    RtcTimeAndDate {
        year: u16::from(bcd(dr.yt(), dr.yu())) + 2000,
        month: bcd(u8::from(dr.mt()), dr.mu()),
        day: bcd(dr.dt(), dr.du()),
        hour: bcd(tr.ht(), tr.hu()),
        minute: bcd(tr.mnt(), tr.mnu()),
        second: bcd(tr.st(), tr.su()),
        ms: 0,
    }
}

fn bcd(tens: u8, units: u8) -> u8 {
    tens * 10 + units
}

/// `calculate_rtc_ms`, in its `uint32_t` arithmetic and `uint16_t` result.
/// While `ss > prediv` it counts from `2 * prediv`, not `2 * prediv + 1`, so
/// `ss` of `2 * prediv + 1` wraps.
fn calculate_rtc_ms(ss: u32, prediv: u32) -> u16 {
    let diff = if prediv < ss {
        (2 * prediv).wrapping_sub(ss)
    } else {
        prediv - ss
    };
    (1000u32.wrapping_mul(diff) / (prediv + 1)) as u16
}

/// `LEAP_YEAR(year - 1970)`.
fn leap_year(year: u16) -> bool {
    year > 0 && year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// `decrement_one_second`.
fn decrement_one_second(t: &mut RtcTimeAndDate) {
    if t.second > 0 {
        t.second -= 1;
        return;
    }
    t.second = 59;
    if t.minute > 0 {
        t.minute -= 1;
        return;
    }
    t.minute = 59;
    if t.hour > 0 {
        t.hour -= 1;
        return;
    }
    t.hour = 23;
    if t.day > 1 {
        t.day -= 1;
        return;
    }
    t.month = t.month.wrapping_sub(1);
    if t.month == 0 {
        t.month = 12;
        if t.year > 0 {
            t.year -= 1;
        }
    }
    // A month of 0 wraps to 255 in the C, which then reads past `monthDays`.
    t.day = if t.month == 2 && leap_year(t.year) {
        29
    } else {
        MONTH_DAYS
            .get(usize::from(t.month.wrapping_sub(1)))
            .copied()
            .unwrap_or(0)
    };
}
