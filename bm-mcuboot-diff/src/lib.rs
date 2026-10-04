//! `bm-mcuboot` against `bm-mcuboot-sys`, MCUboot v1.9.0's `bootutil`.
//! Host-only. The tests are under `tests/`; this is what they share.
//!
//! A comparison starts both sides from the same bytes: a [`RamSlot`] is a
//! copy of one of the oracle's slots, with the oracle's flash rules.

use bm_mcuboot::image::BM_HDR_SIZE;
use bm_mcuboot::{Error, Flash, FlashError, Header, Version, tlv};
use bm_mcuboot_sys::{Area, Oracle, PAGE_SIZE, Refusal, sha256};

/// One slot's bytes, behaving as `bm-mcuboot-sys/csrc/bm_mcuboot.c`'s
/// `flash_area_*` do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RamSlot {
    pub bytes: Vec<u8>,
}

impl RamSlot {
    /// A copy of `area` as the oracle holds it.
    pub fn of(oracle: &Oracle, area: Area) -> Self {
        Self {
            bytes: oracle.read_area(area),
        }
    }

    fn range(&self, off: u32, len: usize) -> Result<std::ops::Range<usize>, FlashError> {
        let off = off as usize;
        match off.checked_add(len) {
            Some(end) if end <= self.bytes.len() => Ok(off..end),
            _ => Err(FlashError),
        }
    }
}

impl Flash for RamSlot {
    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        let range = self.range(off, buf.len())?;
        buf.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    /// Clears bits, then fails unless the bytes equal `data`.
    fn write(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError> {
        let range = self.range(off, data.len())?;
        let dst = &mut self.bytes[range];
        for (d, s) in dst.iter_mut().zip(data) {
            *d &= s;
        }
        if dst == data { Ok(()) } else { Err(FlashError) }
    }

    /// Whole pages at a page offset.
    fn erase(&mut self, off: u32, len: u32) -> Result<(), FlashError> {
        if !off.is_multiple_of(PAGE_SIZE) || !len.is_multiple_of(PAGE_SIZE) {
            return Err(FlashError);
        }
        let range = self.range(off, len as usize)?;
        self.bytes[range].fill(0xFF);
        Ok(())
    }
}

/// A `bm-mcuboot` result as the oracle reports the C's.
pub fn refusal(result: Result<(), Error>) -> Result<(), Refusal> {
    result.map_err(|e| Refusal::Code(e.code()))
}

/// An unsigned image as bm_protocol's build makes one, from `bm-mcuboot`'s
/// encoders and the oracle's SHA-256: the header, `0xFF` to `0x200`,
/// `body`, and a TLV area with the hash of all of that.
pub fn image(body: &[u8], version: Version) -> Vec<u8> {
    let header = Header::new(u32::try_from(body.len()).expect("fits a slot"), version);
    let mut image = header.encode().to_vec();
    image.resize(usize::from(BM_HDR_SIZE), 0xFF);
    image.extend_from_slice(body);
    let digest = sha256(&image);
    image.extend_from_slice(&tlv::encode_unsigned(&digest));
    image
}
