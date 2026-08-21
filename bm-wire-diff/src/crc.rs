//! Differential comparators for `bm_wire::crc`.

use arbitrary::Arbitrary;

/// A CRC run: a seed plus the bytes to fold in, optionally split so the
/// chained-update path is exercised too.
#[derive(Debug, Clone, Arbitrary)]
pub struct CrcInput {
    /// Seed for `crc16_ccitt` and the starting value for `crc32_ieee_update`.
    pub seed: u32,
    /// Where to split `data` when testing the chained update. Taken modulo
    /// `data.len() + 1` so any value is usable.
    pub split: u16,
    /// Payload.
    pub data: Vec<u8>,
}

/// Assert the Rust CRCs agree with bm_core's for this input.
///
/// # Panics
///
/// If any CRC diverges from the C.
pub fn check(input: &CrcInput) {
    let CrcInput { seed, split, data } = input;
    let len = data.len();
    let ptr = data.as_ptr();

    let c16 = unsafe { bm_wire_sys::crc16_ccitt(*seed as u16, ptr, len) };
    let rs16 = bm_wire::crc::crc16_ccitt(*seed as u16, data);
    assert_eq!(
        c16, rs16,
        "crc16_ccitt diverged (seed {seed:#06x}, {len} bytes)"
    );

    let c32 = unsafe { bm_wire_sys::crc32_ieee(ptr, len) };
    let rs32 = bm_wire::crc::crc32_ieee(data);
    assert_eq!(c32, rs32, "crc32_ieee diverged ({len} bytes)");

    let c32u = unsafe { bm_wire_sys::crc32_ieee_update(*seed, ptr, len) };
    let rs32u = bm_wire::crc::crc32_ieee_update(*seed, data);
    assert_eq!(c32u, rs32u, "crc32_ieee_update diverged (crc {seed:#010x})");

    // Chained update across a split must equal the one-shot value on both
    // sides. This catches a port that folds the final complement in twice.
    let at = (*split as usize) % (len + 1);
    let (head, tail) = data.split_at(at);
    let c_chained = unsafe {
        let h = bm_wire_sys::crc32_ieee(head.as_ptr(), head.len());
        bm_wire_sys::crc32_ieee_update(h, tail.as_ptr(), tail.len())
    };
    let rs_chained = bm_wire::crc::crc32_ieee_update(bm_wire::crc::crc32_ieee(head), tail);
    assert_eq!(
        c_chained, rs_chained,
        "chained crc32 diverged (split at {at})"
    );
    assert_eq!(
        c_chained, c32,
        "C's own chained crc32 disagreed with one-shot"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(seed: u32, split: u16, data: &[u8]) {
        check(&CrcInput {
            seed,
            split,
            data: data.to_vec(),
        });
    }

    #[test]
    fn reference_vector() {
        run(0, 4, b"123456789");
    }

    #[test]
    fn empty_and_short_inputs() {
        run(0, 0, b"");
        run(0xFFFF_FFFF, 1, b"\x00");
        run(0x1234, 0, b"\xff\xff\xff\xff");
    }

    #[test]
    fn every_single_byte_with_every_seed_nibble() {
        for byte in 0u8..=255 {
            run(u32::from(byte) * 0x0101_0101, 0, &[byte]);
        }
    }
}
