//! Differential comparators for `bm_wire::util`.

use arbitrary::{Arbitrary, Result, Unstructured};
use bm_wire::util::{BmIpAddr, UtcDateTime};

/// `time_remaining` arguments. The whole `u32` space is in the domain.
#[derive(Debug, Clone, Arbitrary)]
pub struct TimeRemainingInput {
    /// Tick the timeout started at.
    pub start: u32,
    /// Current tick.
    pub current: u32,
    /// Timeout in ticks.
    pub timeout: u32,
}

/// Assert `time_remaining` agrees with bm_core.
///
/// # Panics
///
/// If the Rust result differs from the C.
pub fn check_time_remaining(input: &TimeRemainingInput) {
    let TimeRemainingInput {
        start,
        current,
        timeout,
    } = *input;
    let c = unsafe { bm_wire_sys::time_remaining(start, current, timeout) };
    let rs = bm_wire::util::time_remaining(start, current, timeout);
    assert_eq!(
        c, rs,
        "time_remaining({start}, {current}, {timeout}) diverged"
    );
}

/// A calendar date-time, constrained to where the C is defined.
///
/// `month` is held to 1-12. Outside that range `utc_from_date_time` indexes its
/// 12-element `MONTH_DAYS` table out of bounds (divergence #5), so there is no
/// C behaviour to compare against — the C is simply undefined there. Every
/// other field spans its full range, including values that are nonsense as a
/// date (day 0, hour 250); those are well-defined wrapping arithmetic in C and
/// the port must match them.
#[derive(Debug, Clone)]
pub struct DateTimeInput {
    /// Full year.
    pub year: u16,
    /// Month, always 1-12 once constructed.
    pub month: u8,
    /// Day of month, unconstrained.
    pub day: u8,
    /// Hour, unconstrained.
    pub hour: u8,
    /// Minute, unconstrained.
    pub minute: u8,
    /// Second, unconstrained.
    pub second: u8,
    /// Microseconds since the epoch, for the reverse conversion.
    ///
    /// Masked to 58 bits so the year-accumulation loop stays bounded; the
    /// full-range behaviour is covered by a dedicated test instead of by the
    /// fuzzer, which would otherwise spend its budget spinning in that loop.
    pub utc_us: u64,
}

impl<'a> Arbitrary<'a> for DateTimeInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        Ok(Self {
            year: u.arbitrary()?,
            month: u.int_in_range(1..=12)?,
            day: u.arbitrary()?,
            hour: u.arbitrary()?,
            minute: u.arbitrary()?,
            second: u.arbitrary()?,
            utc_us: u.arbitrary::<u64>()? & ((1 << 58) - 1),
        })
    }
}

/// Assert both UTC conversions agree with bm_core.
///
/// # Panics
///
/// If either conversion diverges from the C.
pub fn check_date_time(input: &DateTimeInput) {
    let DateTimeInput {
        year,
        month,
        day,
        hour,
        minute,
        second,
        utc_us,
    } = *input;

    let c = unsafe { bm_wire_sys::utc_from_date_time(year, month, day, hour, minute, second) };
    let rs = bm_wire::util::utc_from_date_time(year, month, day, hour, minute, second);
    assert_eq!(
        c, rs,
        "utc_from_date_time({year}, {month}, {day}, {hour}, {minute}, {second}) diverged"
    );

    let mut c_dt = bm_wire_sys::UtcDateTime::default();
    unsafe { bm_wire_sys::date_time_from_utc(utc_us, &mut c_dt) };
    let rs_dt = bm_wire::util::date_time_from_utc(utc_us);
    assert_eq!(
        to_rust(&c_dt),
        rs_dt,
        "date_time_from_utc({utc_us}) diverged"
    );
}

fn to_rust(c: &bm_wire_sys::UtcDateTime) -> UtcDateTime {
    UtcDateTime {
        year: c.year,
        month: c.month,
        day: c.day,
        hour: c.hour,
        min: c.min,
        sec: c.sec,
        usec: c.usec,
    }
}

/// A string and a wildcard pattern.
///
/// Both lengths are what is actually passed to C, and both are capped at the
/// buffer length: the C indexes `str[i]` for every `i < str_len` with no
/// bounds check of its own, so a length exceeding the buffer would be an
/// out-of-bounds read in the oracle rather than a finding about the port.
#[derive(Debug, Clone, Arbitrary)]
pub struct WildcardInput {
    /// Subject bytes.
    pub s: Vec<u8>,
    /// Pattern bytes.
    pub pattern: Vec<u8>,
}

/// Assert `bm_wildcard_match` agrees with bm_core.
///
/// # Panics
///
/// If the match result differs from the C.
pub fn check_wildcard(input: &WildcardInput) {
    // u16 lengths in the C API; anything longer cannot be expressed.
    let s = &input.s[..input.s.len().min(u16::MAX as usize)];
    let pattern = &input.pattern[..input.pattern.len().min(u16::MAX as usize)];

    let c = unsafe {
        bm_wire_sys::bm_wildcard_match(
            s.as_ptr().cast(),
            s.len() as u16,
            pattern.as_ptr().cast(),
            pattern.len() as u16,
        )
    };
    let rs = bm_wire::util::bm_wildcard_match(s, pattern);
    assert_eq!(
        c,
        rs,
        "bm_wildcard_match({:?}, {:?}) diverged",
        String::from_utf8_lossy(s),
        String::from_utf8_lossy(pattern)
    );
}

/// A buffer plus the cap to pass to `bm_strnlen`.
#[derive(Debug, Clone, Arbitrary)]
pub struct StrnlenInput {
    /// Bytes to scan.
    pub buf: Vec<u8>,
    /// Requested cap; clamped to the buffer length before use, because the C
    /// would read out of bounds if allowed to scan past the end of an
    /// unterminated buffer.
    pub max_length: u16,
}

/// Assert `bm_strnlen` agrees with bm_core.
///
/// # Panics
///
/// If the length differs from the C.
pub fn check_strnlen(input: &StrnlenInput) {
    let max_length = (input.max_length as usize).min(input.buf.len());
    let c = unsafe { bm_wire_sys::bm_strnlen(input.buf.as_ptr().cast(), max_length) };
    let rs = bm_wire::util::bm_strnlen(&input.buf, max_length);
    assert_eq!(
        c, rs,
        "bm_strnlen(.., {max_length}) diverged on {:x?}",
        input.buf
    );
}

/// A raw IPv6 address.
#[derive(Debug, Clone, Arbitrary)]
pub struct AddrInput {
    /// The 16 address bytes.
    pub addr: [u8; 16],
}

/// Assert the multicast predicates and node-id extraction agree with bm_core.
///
/// # Panics
///
/// If any predicate or the node id differs from the C.
pub fn check_addr(input: &AddrInput) {
    let c_addr = bm_wire_sys::BmIpAddr { addr: input.addr };
    let rs_addr = BmIpAddr(input.addr);

    let c_global = unsafe { bm_wire_sys::is_global_multicast(&c_addr) };
    assert_eq!(
        c_global,
        rs_addr.is_global_multicast(),
        "is_global_multicast diverged"
    );

    let c_ll = unsafe { bm_wire_sys::is_link_local_multicast(&c_addr) };
    assert_eq!(
        c_ll,
        rs_addr.is_link_local_multicast(),
        "is_link_local_multicast diverged"
    );

    let c_nbr = unsafe { bm_wire_sys::is_link_local_neighbor_multicast(&c_addr) };
    assert_eq!(
        c_nbr,
        rs_addr.is_link_local_neighbor_multicast(),
        "is_link_local_neighbor_multicast diverged"
    );

    // ip_to_nodeid returns 0 on a big-endian host (divergence #3). The oracle
    // is little-endian, so this comparison is only meaningful there; assert the
    // precondition rather than silently testing nothing.
    assert!(
        unsafe { bm_wire_sys::is_little_endian() },
        "oracle must be little-endian"
    );
    let c_id = unsafe { bm_wire_sys::ip_to_nodeid(&c_addr) };
    assert_eq!(c_id, rs_addr.to_node_id(), "ip_to_nodeid diverged");

    let mut head = [input.addr[0], input.addr[1]];
    let c16 = unsafe { bm_wire_sys::uint8_to_uint16(head.as_mut_ptr()) };
    assert_eq!(
        c16,
        bm_wire::util::uint8_to_uint16(&head),
        "uint8_to_uint16 diverged"
    );

    let mut quad: [u8; 4] = input.addr[..4].try_into().expect("16 >= 4");
    let c32 = unsafe { bm_wire_sys::uint8_to_uint32(quad.as_mut_ptr()) };
    assert_eq!(
        c32,
        bm_wire::util::uint8_to_uint32(&quad),
        "uint8_to_uint32 diverged"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_remaining_edges() {
        for &(start, current, timeout) in &[
            (0u32, 40u32, 100u32),
            (0, 100, 100),
            (0, 250, 100),
            (u32::MAX, 0, 1),
            (0, u32::MAX, 0),
            (0, 0, u32::MAX),
            (u32::MAX, u32::MAX, u32::MAX),
        ] {
            check_time_remaining(&TimeRemainingInput {
                start,
                current,
                timeout,
            });
        }
    }

    fn date(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, utc_us: u64) {
        check_date_time(&DateTimeInput {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_us,
        });
    }

    #[test]
    fn utc_gold_vectors() {
        date(1970, 1, 1, 0, 0, 0, 0);
        date(2020, 2, 4, 9, 2, 3, 1_580_806_923 * 1_000_000);
    }

    #[test]
    fn utc_leap_year_boundaries() {
        for year in [1970u16, 1999, 2000, 2004, 2020, 2024, 2100, 2400] {
            for month in 1u8..=12 {
                date(year, month, 1, 0, 0, 0, 0);
                date(year, month, 28, 23, 59, 59, 0);
            }
        }
    }

    #[test]
    fn utc_accepts_out_of_range_day_and_time_fields() {
        // Not valid dates, but well-defined wrapping arithmetic in C.
        date(2020, 1, 0, 0, 0, 0, 0);
        date(2020, 12, 255, 255, 255, 255, 0);
        date(0, 1, 0, 0, 0, 0, 0);
        date(65535, 12, 31, 23, 59, 59, 0);
    }

    #[test]
    fn date_time_from_utc_extremes() {
        // Covers the range the fuzzer's 58-bit mask excludes, including the
        // year field wrapping past 65535.
        for utc_us in [0u64, 1, 999_999, 1_000_000, u64::MAX / 2, u64::MAX] {
            let mut c_dt = bm_wire_sys::UtcDateTime::default();
            unsafe { bm_wire_sys::date_time_from_utc(utc_us, &mut c_dt) };
            assert_eq!(
                to_rust(&c_dt),
                bm_wire::util::date_time_from_utc(utc_us),
                "date_time_from_utc({utc_us}) diverged"
            );
        }
    }

    #[test]
    fn wildcard_gold_vectors() {
        for (s, p) in [
            (&b"aaaa"[..], &b"a*a"[..]),
            (b"aaabxc_file.txt", b"*a*b?c*.txt"),
            (b"alpha_betaXc123.txt", b"*a*b*c*.txt"),
            (b"report-1925-diary", b"report-????-*y"),
            (b"alpha_betaXc123.txt", b"*a*b?c*.txt"),
            (b"report-2023-Xbad", b"report-????-*y"),
            (b"", b""),
            (b"", b"*"),
            (b"", b"***"),
            (b"a", b""),
            (b"abc", b"*"),
            (b"abc", b"?"),
        ] {
            check_wildcard(&WildcardInput {
                s: s.to_vec(),
                pattern: p.to_vec(),
            });
        }
    }

    #[test]
    fn strnlen_edges() {
        for (buf, max) in [
            (&b""[..], 0u16),
            (b"abc", 3),
            (b"abc", 10),
            (b"a\0c", 3),
            (b"\0", 1),
        ] {
            check_strnlen(&StrnlenInput {
                buf: buf.to_vec(),
                max_length: max,
            });
        }
    }

    #[test]
    fn addr_predicates() {
        let mut addrs = vec![[0u8; 16], [0xFFu8; 16]];
        addrs.push(bm_wire::util::BmIpAddr::GLOBAL_MULTICAST.0);
        addrs.push(bm_wire::util::BmIpAddr::LINK_LOCAL_MULTICAST.0);

        // FF02::2 and FF03::2 -- prefix matches, last byte does not.
        let mut a = bm_wire::util::BmIpAddr::LINK_LOCAL_MULTICAST.0;
        a[15] = 2;
        addrs.push(a);
        let mut b = bm_wire::util::BmIpAddr::GLOBAL_MULTICAST.0;
        b[15] = 2;
        addrs.push(b);

        // Middle bytes set: C only checks bytes 0, 1 and 15, so these must
        // still classify as multicast.
        let mut c = bm_wire::util::BmIpAddr::GLOBAL_MULTICAST.0;
        c[7] = 0xAB;
        addrs.push(c);

        for addr in addrs {
            check_addr(&AddrInput { addr });
        }
    }
}
