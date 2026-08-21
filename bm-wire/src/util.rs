//! Ported from `common/util.c` and `common/util.h`.
//!
//! Everything here is pure. Several functions reproduce C behaviour that is
//! surprising on its face — wrapping arithmetic in [`time_remaining`], the
//! prefix-only multicast tests — because deployed nodes depend on it. See
//! `docs/c-divergences.md`.

/// A 128-bit IPv6 address, in wire order.
///
/// The C is `struct { uint8_t addr[16]; }`, so this is layout-compatible by
/// construction and the harness can transmute between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BmIpAddr(pub [u8; 16]);

impl BmIpAddr {
    /// The Bristlemouth global multicast address, `FF03::1`.
    pub const GLOBAL_MULTICAST: Self =
        Self([0xFF, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01]);

    /// The Bristlemouth link-local multicast address, `FF02::1`.
    pub const LINK_LOCAL_MULTICAST: Self =
        Self([0xFF, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01]);

    /// Whether this is the global multicast address `FF03::1`.
    ///
    /// Matches C: only bytes 0, 1 and 15 are examined, so `FF03:dead::1` also
    /// reports true.
    #[must_use]
    pub const fn is_global_multicast(&self) -> bool {
        self.0[0] == 0xFF && self.0[1] == 0x03 && self.0[15] == 0x01
    }

    /// Whether this is any link-local multicast address, `FF02::/16`.
    #[must_use]
    pub const fn is_link_local_multicast(&self) -> bool {
        self.0[0] == 0xFF && self.0[1] == 0x02
    }

    /// Whether this is the link-local neighbor multicast address `FF02::1`
    /// defined by Bristlemouth spec section 5.4.4.2.
    ///
    /// Matches C: bytes 2..15 are not examined.
    #[must_use]
    pub const fn is_link_local_neighbor_multicast(&self) -> bool {
        self.0[0] == 0xFF && self.0[1] == 0x02 && self.0[15] == 0x01
    }

    /// The 64-bit node ID carried in the low half of the address.
    ///
    /// The C reads the low 8 bytes as two `uint32_t`s and byte-swaps them,
    /// which is a big-endian read of the last 8 bytes — but only on a
    /// little-endian host. On big-endian it returns 0; see divergence #3.
    /// This port always performs the little-endian-host behaviour, which is
    /// what every shipped Bristlemouth target does.
    #[must_use]
    pub fn to_node_id(&self) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.0[8..16]);
        u64::from_be_bytes(bytes)
    }
}

/// Milliseconds left before `start + timeout` elapses, given `current`.
///
/// Reproduces the C exactly: the sum and difference wrap as `uint32_t`, and the
/// result is reinterpreted as a signed value so that an already-elapsed timeout
/// saturates at 0 rather than underflowing. Because the comparison is signed,
/// a `current` more than 2^31 ms past the deadline reads as "not yet elapsed".
#[must_use]
pub fn time_remaining(start: u32, current: u32, timeout: u32) -> u32 {
    let remaining = start.wrapping_add(timeout).wrapping_sub(current) as i32;
    if remaining < 0 { 0 } else { remaining as u32 }
}

/// Length of a NUL-terminated string, capped at `max_length`.
#[must_use]
pub fn bm_strnlen(s: &[u8], max_length: usize) -> usize {
    s.iter()
        .take(max_length)
        .position(|&b| b == 0)
        .unwrap_or_else(|| max_length.min(s.len()))
}

/// Match `str` against a `*`/`?` wildcard `pattern`.
///
/// `*` matches any run of characters (including none), `?` exactly one. Ported
/// verbatim from the C backtracking matcher, including its behaviour on an
/// empty string: an empty `str` matches any all-`*` pattern.
#[must_use]
pub fn bm_wildcard_match(s: &[u8], pattern: &[u8]) -> bool {
    let str_len = s.len();
    let pattern_len = pattern.len();

    let mut star_idx: Option<usize> = None;
    let mut match_idx: usize = 0;
    let mut i: usize = 0;
    let mut j: usize = 0;

    while i < str_len {
        if j < pattern_len && (pattern[j] == b'?' || s[i] == pattern[j]) {
            i += 1;
            j += 1;
        } else if j < pattern_len && pattern[j] == b'*' {
            star_idx = Some(j);
            j += 1;
            match_idx = i;
        } else if let Some(star) = star_idx {
            // No match, but a previous wildcard exists: backtrack to it.
            j = star + 1;
            match_idx += 1;
            i = match_idx;
        } else {
            break;
        }
    }

    // Trailing '*' in the pattern can match nothing.
    while j < pattern_len && pattern[j] == b'*' {
        j += 1;
    }

    j == pattern_len
}

/// A broken-down UTC timestamp. Mirrors `UtcDateTime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UtcDateTime {
    /// Full year, e.g. 2026.
    pub year: u16,
    /// Month, 1-12.
    pub month: u8,
    /// Day of month, starting at 1.
    pub day: u8,
    /// Hour, 0-23.
    pub hour: u8,
    /// Minute, 0-59.
    pub min: u8,
    /// Second, 0-59.
    pub sec: u8,
    /// Microseconds within the second.
    pub usec: u32,
}

const SECS_PER_MIN: u64 = 60;
const SECS_PER_HOUR: u64 = 3600;
const SECS_PER_DAY: u64 = SECS_PER_HOUR * 24;
const MICROSECONDS_PER_SECOND: u64 = 1_000_000;

const MONTH_DAYS: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const MONTH_DAYS_LEAP_YEAR: [u8; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// The C `leap_year(Y)` macro, which takes an offset from 1970 and immediately
/// adds it back, so it is really just a test on the absolute year.
const fn is_leap_year(year: u32) -> bool {
    year > 0 && year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn days_in_year(year: u32) -> u64 {
    if is_leap_year(year) { 366 } else { 365 }
}

const fn days_per_month(year: u32) -> &'static [u8; 12] {
    if is_leap_year(year) {
        &MONTH_DAYS_LEAP_YEAR
    } else {
        &MONTH_DAYS
    }
}

/// Seconds since the Unix epoch for a UTC date-time.
///
/// # Domain
///
/// `month` must be 1-12. The C indexes a 12-element table with `i - 1` for
/// every `i < month` without bounds-checking, so `month > 12` is an
/// out-of-bounds read there (divergence #5) with no defined value to match.
/// This port stops at the end of the table instead; the differential harness
/// only compares inputs inside the valid domain.
///
/// Every other field is used as-is. `day`, `hour`, `minute` and `second` are
/// not range-checked, and out-of-range values contribute by wrapping
/// arithmetic exactly as they do in C.
#[must_use]
pub fn utc_from_date_time(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> u32 {
    // Seconds from 1970 to Jan 1 00:00:00 of `year`. In C this is an `int`
    // times an `unsigned long`, so a year before 1970 wraps rather than
    // underflowing; reproduce that with a sign-extended wrapping multiply.
    // Truncation to u32 is deferred to the return, which is equivalent because
    // truncation distributes over addition.
    let years_since_epoch = i64::from(year) - 1970;
    let mut seconds = (years_since_epoch as u64).wrapping_mul(SECS_PER_DAY * 365);

    for y in 1970..u32::from(year) {
        if is_leap_year(y) {
            seconds = seconds.wrapping_add(SECS_PER_DAY);
        }
    }

    // Whole months elapsed this year. Note the C consults the non-leap table
    // here and special-cases February, rather than switching tables.
    let leap = is_leap_year(u32::from(year));
    for m in 1..u32::from(month).min(MONTH_DAYS.len() as u32 + 1) {
        let days = if m == 2 && leap {
            29
        } else {
            u64::from(MONTH_DAYS[(m - 1) as usize])
        };
        seconds = seconds.wrapping_add(SECS_PER_DAY.wrapping_mul(days));
    }

    // `day` is 1-based; day 0 underflows to a huge value in C too, so wrap
    // rather than panicking in a debug build.
    seconds = seconds.wrapping_add(((i64::from(day) - 1) as u64).wrapping_mul(SECS_PER_DAY));
    seconds = seconds.wrapping_add(u64::from(hour).wrapping_mul(SECS_PER_HOUR));
    seconds = seconds.wrapping_add(u64::from(minute).wrapping_mul(SECS_PER_MIN));
    seconds = seconds.wrapping_add(u64::from(second));

    seconds as u32
}

/// Break a Unix timestamp in microseconds down into a UTC date-time.
///
/// The year is accumulated in a `u16` as in C, so timestamps beyond year 65535
/// wrap the year field rather than saturating.
#[must_use]
pub fn date_time_from_utc(utc_us: u64) -> UtcDateTime {
    let total_secs = utc_us / MICROSECONDS_PER_SECOND;

    let mut days = total_secs / SECS_PER_DAY;
    let mut year: u16 = 1970;
    while days >= days_in_year(u32::from(year)) {
        days -= days_in_year(u32::from(year));
        year = year.wrapping_add(1);
    }

    let table = days_per_month(u32::from(year));
    let mut month: u8 = 1;
    while days >= u64::from(table[(month - 1) as usize]) {
        days -= u64::from(table[(month - 1) as usize]);
        month += 1;
    }

    let secs_remaining = total_secs % SECS_PER_DAY;

    UtcDateTime {
        year,
        month,
        day: (days + 1) as u8,
        hour: (secs_remaining / SECS_PER_HOUR) as u8,
        min: ((secs_remaining / SECS_PER_MIN) % SECS_PER_MIN) as u8,
        sec: (secs_remaining % SECS_PER_MIN) as u8,
        usec: (utc_us % MICROSECONDS_PER_SECOND) as u32,
    }
}

/// Big-endian read of two bytes. Mirrors the `uint8_to_uint16` static inline.
#[must_use]
pub const fn uint8_to_uint16(buf: &[u8; 2]) -> u16 {
    u16::from_be_bytes(*buf)
}

/// Big-endian read of four bytes. Mirrors the `uint8_to_uint32` static inline.
#[must_use]
pub const fn uint8_to_uint32(buf: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values below are taken from bm_core's own gtest suite rather than
    // invented: utc_from_date_time_test.cpp, util_test.cpp and
    // time_remaining_test.cpp, via bm-wire-sys/tests/smoke.rs.

    #[test]
    fn utc_matches_bm_core_vectors() {
        assert_eq!(utc_from_date_time(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(utc_from_date_time(2020, 2, 4, 9, 2, 3), 1_580_806_923);
    }

    #[test]
    fn utc_round_trips() {
        let dt = date_time_from_utc(1_580_806_923 * 1_000_000);
        assert_eq!(
            dt,
            UtcDateTime {
                year: 2020,
                month: 2,
                day: 4,
                hour: 9,
                min: 2,
                sec: 3,
                usec: 0,
            }
        );
    }

    #[test]
    fn wildcard_match_handles_stars_and_question_marks() {
        assert!(bm_wildcard_match(b"aaaa", b"a*a"));
        assert!(bm_wildcard_match(b"aaabxc_file.txt", b"*a*b?c*.txt"));
        assert!(bm_wildcard_match(b"alpha_betaXc123.txt", b"*a*b*c*.txt"));
        assert!(bm_wildcard_match(b"report-1925-diary", b"report-????-*y"));

        assert!(!bm_wildcard_match(b"alpha_betaXc123.txt", b"*a*b?c*.txt"));
        assert!(!bm_wildcard_match(b"report-2023-Xbad", b"report-????-*y"));
    }

    #[test]
    fn time_remaining_wraps_like_a_tick_counter() {
        assert_eq!(time_remaining(0, 40, 100), 60);
        assert_eq!(time_remaining(0, 100, 100), 0);
        assert_eq!(time_remaining(0, 250, 100), 0);
    }

    #[test]
    fn multicast_classification_reads_the_address_prefix() {
        let global = BmIpAddr::GLOBAL_MULTICAST;
        assert!(global.is_global_multicast());
        assert!(!global.is_link_local_multicast());

        let ll = BmIpAddr::LINK_LOCAL_MULTICAST;
        assert!(ll.is_link_local_multicast());
        assert!(ll.is_link_local_neighbor_multicast());
        assert!(!ll.is_global_multicast());

        let mut ll2 = BmIpAddr::LINK_LOCAL_MULTICAST;
        ll2.0[15] = 0x02;
        assert!(ll2.is_link_local_multicast());
        assert!(!ll2.is_link_local_neighbor_multicast());
    }

    #[test]
    fn node_id_is_a_big_endian_read_of_the_low_half() {
        let mut ip = BmIpAddr::default();
        ip.0[8..16].copy_from_slice(&0xDEAD_BEEF_1234_5678u64.to_be_bytes());
        assert_eq!(ip.to_node_id(), 0xDEAD_BEEF_1234_5678);
    }

    #[test]
    fn big_endian_helpers() {
        assert_eq!(uint8_to_uint16(&[0x12, 0x34]), 0x1234);
        assert_eq!(uint8_to_uint32(&[0x12, 0x34, 0x56, 0x78]), 0x1234_5678);
    }
}
