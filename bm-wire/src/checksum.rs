//! IPv6 pseudo-header checksum, ported from `network/bm_linux.c`.
//!
//! Must agree bit-for-bit with lwIP's `ip6_chksum_pseudo`: a host node and an
//! embedded node validate each other's BCMP checksums with it.

use crate::util::BmIpAddr;

/// RFC 2460 IPv6 pseudo-header checksum over `data`.
///
/// The one's-complement sum covers the 16-byte source address, the 16-byte
/// destination address, the 32-bit upper-layer length, the next-header byte,
/// and the upper-layer data, with an odd trailing byte padded high.
///
/// # Byte order
///
/// The C finishes with `ntohs(~sum)`, which byte-swaps on a little-endian host
/// and does nothing on a big-endian one — so the returned `u16` is
/// host-dependent by design, and is meant to be stored straight into a packed
/// header field. This port always performs the little-endian-host behaviour,
/// which is what the oracle does and what every Bristlemouth target does.
#[must_use]
pub fn ipv6_pseudo_checksum(src: &BmIpAddr, dst: &BmIpAddr, next_header: u8, data: &[u8]) -> u16 {
    let length = data.len() as u32;
    let mut sum: u32 = 0;

    for addr in [src, dst] {
        for pair in addr.0.as_chunks::<2>().0 {
            sum += u32::from(u16::from_be_bytes(*pair));
        }
    }

    // Upper-layer packet length, as a 32-bit value split into two 16-bit words.
    sum += (length >> 16) & 0xFFFF;
    sum += length & 0xFFFF;

    // Next header. The three preceding zero bytes contribute nothing.
    sum += u32::from(next_header);

    // Upper-layer data, two bytes at a time.
    let (pairs, remainder) = data.as_chunks::<2>();
    for pair in pairs {
        sum += u32::from(u16::from_be_bytes(*pair));
    }
    // An odd trailing byte is padded on the low side.
    if let [last] = remainder {
        sum += u32::from(*last) << 8;
    }

    // Fold the carries down into 16 bits.
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    (!(sum as u16)).swap_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured BCMP heartbeat, `ipv6_pseudo_checksum_real_packet2` in
    /// `bm_core/test/src/bm_linux_test.cpp`. The expected value came off a
    /// real link, so it checks the port against the wire and not merely
    /// against the C.
    #[test]
    fn real_packet_heartbeat() {
        let src = BmIpAddr([
            0xFE, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0xAA,
            0x00, 0x11,
        ]);
        let dst = BmIpAddr::LINK_LOCAL_MULTICAST;
        let data = [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30,
            0x7C, 0x71, 0x22, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00,
        ];
        assert_eq!(ipv6_pseudo_checksum(&src, &dst, 0xBC, &data), 0x3F0C);
    }

    #[test]
    fn empty_data_is_just_the_pseudo_header() {
        let zero = BmIpAddr::default();
        let with = ipv6_pseudo_checksum(&zero, &zero, 0, &[]);
        // All-zero pseudo-header: sum is 0, complement is 0xFFFF, swap is a
        // no-op on a palindrome.
        assert_eq!(with, 0xFFFF);
    }

    #[test]
    fn odd_length_pads_the_trailing_byte_high() {
        // Hand-computed: all-zero addresses contribute 0, the length word
        // contributes 1, next_header 0, and the lone data byte 0xAB00.
        // sum = 0xAB01, !sum = 0x54FE, swapped = 0xFE54.
        let zero = BmIpAddr::default();
        assert_eq!(ipv6_pseudo_checksum(&zero, &zero, 0, &[0xAB]), 0xFE54);

        // The same byte followed by an explicit zero differs only by the
        // length word, which is now 2: sum = 0xAB02, !sum = 0x54FD.
        assert_eq!(ipv6_pseudo_checksum(&zero, &zero, 0, &[0xAB, 0x00]), 0xFD54);
    }
}
