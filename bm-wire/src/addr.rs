//! Node-id, IPv6 and MAC derivation, ported from `network/bm_linux.c`.

use crate::util::BmIpAddr;

/// Length of an Ethernet MAC address.
pub const MAC_LEN: usize = 6;

/// Build an address from a 32-bit prefix and a 64-bit node id.
///
/// Bytes 0-3 carry the prefix, bytes 4-7 are zero, and bytes 8-15 carry the
/// node id — all big-endian, so [`BmIpAddr::to_node_id`] reverses it.
#[must_use]
pub fn nodeid_to_ip(prefix: u32, id: u64) -> BmIpAddr {
    let mut addr = [0u8; 16];
    addr[0..4].copy_from_slice(&prefix.to_be_bytes());
    addr[8..16].copy_from_slice(&id.to_be_bytes());
    BmIpAddr(addr)
}

/// Derive a locally-administered unicast MAC from a node id.
///
/// Takes the low 48 bits of the id, then forces the locally-administered bit
/// and clears the multicast bit in byte 0 — so the MAC is not a faithful
/// reflection of those id bits, and two ids differing only in bits 40 and 41
/// collide.
#[must_use]
pub fn mac_from_nodeid(id: u64) -> [u8; MAC_LEN] {
    let mut mac = [0u8; MAC_LEN];
    mac.copy_from_slice(&id.to_be_bytes()[2..8]);
    mac[0] |= 0x02; // locally administered
    mac[0] &= !0x01; // unicast
    mac
}

/// Map an IPv6 multicast address onto its Ethernet MAC: `33:33` followed by
/// the last four bytes of the address.
#[must_use]
pub fn multicast_mac_from_ipv6(dst: &BmIpAddr) -> [u8; MAC_LEN] {
    let mut mac = [0x33u8; MAC_LEN];
    mac[2..6].copy_from_slice(&dst.0[12..16]);
    mac
}

/// Whether an address is any IPv6 multicast address.
#[must_use]
pub const fn is_multicast(addr: &BmIpAddr) -> bool {
    addr.0[0] == 0xFF
}

/// Longest possible rendering of an IPv6 address, plus room to spare.
///
/// The C requires a 40-byte buffer; the same size is used here so the two
/// cannot disagree about capacity.
pub const IPV6_STR_LEN: usize = 40;

/// A rendered IPv6 address, stack-allocated.
///
/// Exists so [`format_ipv6`] can return a string without an allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv6Str {
    buf: [u8; IPV6_STR_LEN],
    len: usize,
}

impl Ipv6Str {
    /// The formatted address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only ASCII hex and ':' are ever written.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// The formatted address as bytes, without a trailing NUL.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Display for Ipv6Str {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Format an address RFC 5952 style, compressing the longest run of two or
/// more zero words to `::`.
///
/// Matches the C's tie-breaking: `>` rather than `>=` when tracking the longest
/// run, so the *first* run of a given length wins.
#[must_use]
pub fn format_ipv6(addr: &BmIpAddr) -> Ipv6Str {
    let mut w = [0u16; 8];
    for (i, word) in w.iter_mut().enumerate() {
        *word = u16::from_be_bytes([addr.0[i * 2], addr.0[i * 2 + 1]]);
    }

    // Longest run of consecutive zero words.
    let mut zero_start: isize = -1;
    let mut zero_len = 0usize;
    let mut cur_start: isize = -1;
    let mut cur_len = 0usize;
    for (i, &word) in w.iter().enumerate() {
        if word == 0 {
            if cur_start < 0 {
                cur_start = i as isize;
                cur_len = 0;
            }
            cur_len += 1;
            if cur_len > zero_len {
                zero_start = cur_start;
                zero_len = cur_len;
            }
        } else {
            cur_start = -1;
            cur_len = 0;
        }
    }
    if zero_len < 2 {
        zero_start = -1; // only runs of 2+ are compressed
    }

    let mut out = Ipv6Str {
        buf: [0; IPV6_STR_LEN],
        len: 0,
    };
    let mut need_colon = false;
    let mut i = 0usize;
    while i < 8 {
        if i as isize == zero_start {
            out.push(b':');
            out.push(b':');
            i += zero_len;
            need_colon = false;
            continue;
        }
        if need_colon {
            out.push(b':');
        }
        out.push_hex(w[i]);
        need_colon = true;
        i += 1;
    }
    out
}

impl Ipv6Str {
    fn push(&mut self, byte: u8) {
        if self.len < IPV6_STR_LEN {
            self.buf[self.len] = byte;
            self.len += 1;
        }
    }

    /// Lowercase hex with no leading zeros, matching `printf("%x", w)`.
    fn push_hex(&mut self, word: u16) {
        if word == 0 {
            self.push(b'0');
            return;
        }
        let mut started = false;
        for shift in [12, 8, 4, 0] {
            let nibble = ((word >> shift) & 0xF) as u8;
            if nibble != 0 || started {
                started = true;
                self.push(if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodeid_round_trips_through_an_address() {
        let id = 0xDEAD_BEEF_1234_5678u64;
        for prefix in [0xFE80_0000u32, 0xFD00_0000] {
            let ip = nodeid_to_ip(prefix, id);
            assert_eq!(ip.to_node_id(), id);
            assert_eq!(&ip.0[0..4], &prefix.to_be_bytes());
            assert_eq!(&ip.0[4..8], &[0, 0, 0, 0]);
        }
    }

    #[test]
    fn mac_is_locally_administered_unicast() {
        let mac = mac_from_nodeid(0xDEAD_BEEF_1234_5678);
        assert_eq!(mac[0] & 0x02, 0x02, "locally administered bit set");
        assert_eq!(mac[0] & 0x01, 0x00, "multicast bit clear");
        assert_eq!(&mac[1..], &[0xEF, 0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn multicast_mac_uses_the_thirty_three_prefix() {
        let addr = BmIpAddr::LINK_LOCAL_MULTICAST;
        assert_eq!(multicast_mac_from_ipv6(&addr), [0x33, 0x33, 0, 0, 0, 1]);
    }

    #[test]
    fn multicast_is_the_first_byte() {
        assert!(is_multicast(&BmIpAddr::GLOBAL_MULTICAST));
        assert!(is_multicast(&BmIpAddr::LINK_LOCAL_MULTICAST));
        assert!(!is_multicast(&BmIpAddr::default()));
    }

    #[test]
    fn formatting_compresses_zero_runs() {
        assert_eq!(format_ipv6(&BmIpAddr::default()).as_str(), "::");
        assert_eq!(
            format_ipv6(&BmIpAddr::LINK_LOCAL_MULTICAST).as_str(),
            "ff02::1"
        );
        assert_eq!(format_ipv6(&BmIpAddr::GLOBAL_MULTICAST).as_str(), "ff03::1");

        let full = BmIpAddr([
            0xfe, 0x80, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x62, 0x32, 0x67, 0x60, 0xda, 0x4e,
            0x23, 0x7a,
        ]);
        assert_eq!(format_ipv6(&full).as_str(), "fe80:300::6232:6760:da4e:237a");
    }
}
