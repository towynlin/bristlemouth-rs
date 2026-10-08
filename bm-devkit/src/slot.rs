//! MCUboot's slots in internal flash: [`DevkitSlot`], the DFU client's
//! [`bm_stack::DfuSlot`] on slot 2, as bm_protocol's `bm_dfu_wrapper.cpp`,
//! `port_flash.c` and `stm32_flash_u5.c` implement it. `README.md`, "DFU
//! slot", has the sources and the three cases that differ from the C.

use core::sync::atomic::{AtomicU32, Ordering};

use bm_mcuboot::{Flash as McubootFlash, FlashError, Trailer};
use bm_stack::{DfuSlot, NoInitRam};
use bm_wire::bcmp::dfu_core::RebootInfo;
use defmt::{info, warn};
use embassy_stm32::flash::{Blocking, Flash};
use embassy_time::Instant;

use crate::noinit::{self, NoInit, ResetReason};
use crate::watchdog;

/// Slot 1, the primary slot the running image is in, as an offset from
/// `FLASH_BASE` (`0x08000000`).
pub const SLOT1_OFFSET: u32 = 0xC000;
/// Slot 2, the secondary slot an update is written to, as an offset from
/// `FLASH_BASE`.
pub const SLOT2_OFFSET: u32 = 0xF_E000;
/// Either slot's size, `APP_SIZE`.
pub const SLOT_SIZE: u32 = 0xF_2000;
/// `FLASH_PAGE_SIZE`: the erase unit.
pub const PAGE_SIZE: u32 = 0x2000;
/// The U5's write unit, a quad-word; `MCUBOOT_BOOT_MAX_ALIGN`.
pub const WRITE_SIZE: usize = 16;

const ERASED: u8 = 0xFF;

/// The last erase's duration in ms, or `u32::MAX` once taken.
static ERASE_MS: AtomicU32 = AtomicU32::new(u32::MAX);

/// How long the last successful erase took, in ms, once: `None` until the
/// next. For an application to report over the bus, where no probe is
/// attached.
pub fn take_erase_ms() -> Option<u32> {
    match ERASE_MS.swap(u32::MAX, Ordering::Relaxed) {
        u32::MAX => None,
        ms => Some(ms),
    }
}

/// Writes between two progress lines in the log.
const PROGRESS_BYTES: u32 = 0x1_0000;

/// One slot of `flash`, at `base` from `FLASH_BASE`. Offsets are from the
/// start of the slot.
struct Area<'a> {
    flash: &'a mut Flash<'static, Blocking>,
    base: u32,
}

impl Area<'_> {
    fn in_range(off: u32, len: usize) -> bool {
        u32::try_from(len)
            .ok()
            .and_then(|len| off.checked_add(len))
            .is_some_and(|end| end <= SLOT_SIZE)
    }

    fn read(&mut self, off: u32, buf: &mut [u8]) -> bool {
        Self::in_range(off, buf.len()) && self.flash.blocking_read(self.base + off, buf).is_ok()
    }

    /// `flashErase`, then `flash_area_erase`'s read-back, one page at a time
    /// with the IWDG fed between pages.
    fn erase(&mut self, off: u32, len: u32) -> bool {
        if !off.is_multiple_of(PAGE_SIZE)
            || !len.is_multiple_of(PAGE_SIZE)
            || !Self::in_range(off, len as usize)
        {
            return false;
        }
        let started = Instant::now();
        let mut buf = [0u8; 256];
        for start in (off..off + len).step_by(PAGE_SIZE as usize) {
            watchdog::feed();
            let address = self.base + start;
            if self
                .flash
                .blocking_erase(address, address + PAGE_SIZE)
                .is_err()
            {
                warn!("slot: erase failed at {=u32:#x}", start);
                return false;
            }
            for chunk in (start..start + PAGE_SIZE).step_by(buf.len()) {
                if !self.read(chunk, &mut buf) || buf.iter().any(|&b| b != ERASED) {
                    warn!("slot: not erased at {=u32:#x}", chunk);
                    return false;
                }
            }
        }
        watchdog::feed();
        let ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX - 1);
        ERASE_MS.store(ms, Ordering::Relaxed);
        info!(
            "slot: erased {=u32:#x} bytes at {=u32:#x} in {=u32} ms",
            len, off, ms
        );
        true
    }

    /// `flashWrite`, a quad-word at a time, then `flash_area_write`'s
    /// read-back.
    fn write(&mut self, off: u32, data: &[u8]) -> bool {
        if !off.is_multiple_of(WRITE_SIZE as u32) || !Self::in_range(off, data.len()) {
            return false;
        }
        let mut address = self.base + off;
        for chunk in data.chunks(WRITE_SIZE) {
            let mut quad = [ERASED; WRITE_SIZE];
            quad[..chunk.len()].copy_from_slice(chunk);
            if self.flash.blocking_write(address, &quad).is_err() {
                warn!("slot: write failed at {=u32:#x}", address - self.base);
                return false;
            }
            address += WRITE_SIZE as u32;
        }
        let mut back = [0u8; WRITE_SIZE];
        let mut at = off;
        for chunk in data.chunks(WRITE_SIZE) {
            let back = &mut back[..chunk.len()];
            if !self.read(at, back) || back != chunk {
                warn!("slot: write did not verify at {=u32:#x}", at);
                return false;
            }
            at += WRITE_SIZE as u32;
        }
        true
    }
}

impl McubootFlash for Area<'_> {
    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
        Area::read(self, off, buf).then_some(()).ok_or(FlashError)
    }

    fn write(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError> {
        Area::write(self, off, data).then_some(()).ok_or(FlashError)
    }

    fn erase(&mut self, off: u32, len: u32) -> Result<(), FlashError> {
        Area::erase(self, off, len).then_some(()).ok_or(FlashError)
    }
}

/// [`DfuSlot`] on slot 2, and [`NoInitRam`] through [`NoInit`]: the `D` of a
/// [`crate::Devkit`].
pub struct DevkitSlot {
    flash: Flash<'static, Blocking>,
    noinit: NoInit,
    /// Bytes written since the last [`DfuSlot::erase`], for the log.
    written: u32,
}

impl DevkitSlot {
    /// The slots in `flash`.
    #[must_use]
    pub fn new(flash: Flash<'static, Blocking>) -> Self {
        Self {
            flash,
            noinit: NoInit,
            written: 0,
        }
    }

    fn slot1(&mut self) -> Area<'_> {
        Area {
            flash: &mut self.flash,
            base: SLOT1_OFFSET,
        }
    }

    fn slot2(&mut self) -> Area<'_> {
        Area {
            flash: &mut self.flash,
            base: SLOT2_OFFSET,
        }
    }
}

impl DfuSlot for DevkitSlot {
    fn open(&mut self) -> bool {
        true
    }

    fn close(&mut self) -> bool {
        true
    }

    fn size(&mut self) -> u32 {
        SLOT_SIZE
    }

    fn erase(&mut self, offset: u32, len: u32) -> bool {
        self.written = 0;
        self.slot2().erase(offset, len)
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> bool {
        if !self.slot2().write(offset, data) {
            return false;
        }
        let before = self.written / PROGRESS_BYTES;
        self.written = self.written.saturating_add(data.len() as u32);
        if self.written / PROGRESS_BYTES != before {
            info!("slot: {=u32} bytes written", self.written);
        }
        true
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> bool {
        self.slot2().read(offset, buf)
    }

    fn set_confirmed(&mut self) {
        match bm_mcuboot::set_confirmed(&mut self.slot1(), &Trailer::BM) {
            Ok(()) => info!("slot: image confirmed"),
            Err(error) => warn!("slot: confirm failed: {=i32}", error.code()),
        }
    }

    /// `boot_set_pending(0)`, its result ignored as the C ignores it.
    fn set_pending_and_reset(&mut self) {
        let result = bm_mcuboot::set_pending(&mut self.slot2(), &Trailer::BM, false);
        info!(
            "slot: pending ({=i32}), resetting",
            result.err().map_or(0, |error| error.code())
        );
        noinit::reset(ResetReason::Mcuboot)
    }

    fn fail_update_and_reset(&mut self) {
        warn!("slot: update failed, resetting");
        noinit::reset(ResetReason::UpdateFailed)
    }
}

impl NoInitRam for DevkitSlot {
    fn load(&mut self) -> RebootInfo {
        self.noinit.load()
    }

    fn store(&mut self, info: &RebootInfo) {
        self.noinit.store(info);
    }
}
