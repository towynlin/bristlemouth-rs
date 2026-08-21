//! CRC routines, ported from `third_party/crc/`.
//!
//! bm_core vendors Zephyr's implementations. `crc16_ccitt` is the reflected
//! poly-0x1021 variant (seed 0 gives CRC-16/KERMIT); `crc32_ieee` is the
//! nibble-table poly-0xedb88320 variant.

/// Reflected CRC-16/CCITT over `data`, continuing from `seed`.
///
/// A `seed` of 0 yields CRC-16/KERMIT. bm_core uses this to accumulate a
/// running CRC over a DFU image, so it must chain across chunk boundaries
/// exactly as the C does.
#[must_use]
pub fn crc16_ccitt(seed: u16, data: &[u8]) -> u16 {
    let mut crc = seed;
    for &byte in data {
        let e = (crc ^ u16::from(byte)) as u8;
        let f = e ^ (e << 4);
        let f = u16::from(f);
        crc = (crc >> 8) ^ (f << 8) ^ (f << 3) ^ (f >> 4);
    }
    crc
}

/// CRC-32/IEEE over `data`.
#[must_use]
pub fn crc32_ieee(data: &[u8]) -> u32 {
    crc32_ieee_update(0, data)
}

/// CRC-32/IEEE over `data`, continuing from a previous result.
#[must_use]
pub fn crc32_ieee_update(crc: u32, data: &[u8]) -> u32 {
    /// Generated from polynomial 0xedb88320, indexed by nibble.
    const TABLE: [u32; 16] = [
        0x0000_0000,
        0x1db7_1064,
        0x3b6e_20c8,
        0x26d9_30ac,
        0x76dc_4190,
        0x6b6b_51f4,
        0x4db2_6158,
        0x5005_713c,
        0xedb8_8320,
        0xf00f_9344,
        0xd6d6_a3e8,
        0xcb61_b38c,
        0x9b64_c2b0,
        0x86d3_d2d4,
        0xa00a_e278,
        0xbdbd_f21c,
    ];

    let mut crc = !crc;
    for &byte in data {
        let byte = u32::from(byte);
        crc = (crc >> 4) ^ TABLE[((crc ^ byte) & 0x0f) as usize];
        crc = (crc >> 4) ^ TABLE[((crc ^ (byte >> 4)) & 0x0f) as usize];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values asserted by bm-wire-sys/tests/smoke.rs, which took them from the
    // standard "123456789" check vectors.
    const CHECK: &[u8] = b"123456789";

    #[test]
    fn matches_reference_vectors() {
        assert_eq!(crc16_ccitt(0, CHECK), 0x2189);
        assert_eq!(crc32_ieee(CHECK), 0xCBF4_3926);
    }

    #[test]
    fn crc32_update_chains_across_a_split() {
        let (head, tail) = CHECK.split_at(4);
        assert_eq!(crc32_ieee_update(crc32_ieee(head), tail), crc32_ieee(CHECK));
    }

    #[test]
    fn empty_input_returns_the_seed() {
        assert_eq!(crc16_ccitt(0xABCD, &[]), 0xABCD);
        assert_eq!(crc32_ieee(&[]), 0);
    }
}
