//! MCUboot v1.9.0's `bootutil`, compiled for the host with bm_protocol's
//! configuration and flash map over RAM. An oracle for what a bootloader on a
//! Bristlemouth node does with a slot's bytes. See `README.md`.
//!
//! Two builds are linked, each with its own flash: [`Build::Unsigned`] and
//! [`Build::Ed25519`]. [`lock`] is the only way in.

use std::ffi::c_int;
use std::sync::{Mutex, MutexGuard};

/// Internal flash page size.
pub const PAGE_SIZE: u32 = 0x2000;
/// `MCUBOOT_BOOT_MAX_ALIGN`: the write alignment, and `imgtool --align`.
pub const ALIGN: u32 = 16;
/// Size of each of the two image slots.
pub const SLOT_SIZE: u32 = 0xF2000;
/// `sizeof(struct image_header)`.
pub const HEADER_SIZE: usize = 32;

/// `BOOT_SWAP_TYPE_*`, what [`Oracle::swap_type`] returns.
pub mod swap_type {
    pub const NONE: i32 = 1;
    pub const TEST: i32 = 2;
    pub const PERM: i32 = 3;
    pub const REVERT: i32 = 4;
    pub const FAIL: i32 = 5;
    pub const PANIC: i32 = 0xff;
}

/// Which compilation of MCUboot to call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// No signature type: an image needs a SHA-256 TLV only. Dev kits.
    Unsigned,
    /// `MCUBOOT_SIGN_ED25519` with the public half of
    /// `testdata/test_ed25519_key.pem`.
    Ed25519,
}

/// A flash area of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Bootloader,
    /// Slot 1, the one that runs.
    Primary,
    /// Slot 2, the one DFU writes.
    Secondary,
    Scratch,
}

impl Area {
    /// The area's id in `sysflash.h`.
    pub const fn id(self) -> u8 {
        match self {
            Area::Bootloader => 0,
            Area::Primary => 1,
            Area::Secondary => 2,
            Area::Scratch => 3,
        }
    }

    /// Offset from the start of flash (`0x08000000` on the MCU).
    pub const fn offset(self) -> u32 {
        match self {
            Area::Bootloader => 0,
            Area::Primary => 0xC000,
            Area::Secondary => 0xC000 + SLOT_SIZE,
            Area::Scratch => 0xC000 + 2 * SLOT_SIZE,
        }
    }

    pub const fn size(self) -> u32 {
        match self {
            Area::Bootloader => 0xC000,
            Area::Primary | Area::Secondary => SLOT_SIZE,
            Area::Scratch => 0x10000,
        }
    }
}

/// Why a call did not succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The function's own non-zero return. `boot_go` returns 1, from
    /// `boot_validate_slot`, when slot 1 holds no image it accepts.
    Code(i32),
    /// A `bootutil` assert failed. The bootloader resets there.
    Asserted,
}

/// A successful `boot_go`: `struct boot_rsp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Booted {
    /// `br_image_off`: the booted header's offset from the start of flash.
    pub image_off: u32,
    /// `br_flash_dev_id`.
    pub flash_dev_id: u8,
    /// `*br_hdr`, as the bytes in flash.
    pub header: [u8; HEADER_SIZE],
}

static LOCK: Mutex<()> = Mutex::new(());

/// Take the oracle. Both builds share one lock; their flash and state are C
/// statics.
pub fn lock(build: Build) -> Oracle {
    Oracle {
        build,
        _guard: LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

/// One build of MCUboot and its flash, held exclusively.
///
/// Flash persists across locks; a test starts with [`Oracle::reset`].
pub struct Oracle {
    build: Build,
    _guard: MutexGuard<'static, ()>,
}

/// Call the build's copy of a C function.
macro_rules! call {
    ($self:ident, $name:ident ( $($arg:expr),* )) => {
        // SAFETY: the lock is held, and every pointer argument is a live
        // slice or local whose length the callee is given or fixed by
        // `bm_mcuboot.h`.
        unsafe {
            match $self.build {
                Build::Unsigned => ffi::unsigned::$name($($arg),*),
                Build::Ed25519 => ffi::ed25519::$name($($arg),*),
            }
        }
    };
}

fn refusal(rc: i32) -> Result<(), Refusal> {
    match rc {
        0 => Ok(()),
        ffi::ASSERTED => Err(Refusal::Asserted),
        rc => Err(Refusal::Code(rc)),
    }
}

impl Oracle {
    /// The other build, without releasing the lock: its flash as it was
    /// left.
    #[must_use]
    pub fn switch(self, build: Build) -> Self {
        Self { build, ..self }
    }

    pub fn build(&self) -> Build {
        self.build
    }

    /// Every byte of this build's flash to `0xFF`.
    pub fn reset(&mut self) {
        call!(self, bm_mcuboot_flash_reset())
    }

    /// Copy `buf.len()` bytes out of `area` from `off`.
    ///
    /// # Panics
    /// If the range is not inside the area.
    pub fn read(&self, area: Area, off: u32, buf: &mut [u8]) {
        let len = u32::try_from(buf.len()).expect("length fits the area");
        let rc = call!(
            self,
            bm_mcuboot_flash_load(area.id(), off, buf.as_mut_ptr(), len)
        );
        assert_eq!(rc, 0, "read of {len} at {off:#x} is outside {area:?}");
    }

    /// The whole of `area`.
    pub fn read_area(&self, area: Area) -> Vec<u8> {
        let mut buf = vec![0; area.size() as usize];
        self.read(area, 0, &mut buf);
        buf
    }

    /// Store `data` in `area` at `off`, overwriting what is there: no erase
    /// is needed and no alignment applies.
    ///
    /// # Panics
    /// If the range is not inside the area.
    pub fn write(&mut self, area: Area, off: u32, data: &[u8]) {
        let len = u32::try_from(data.len()).expect("length fits the area");
        let rc = call!(
            self,
            bm_mcuboot_flash_store(area.id(), off, data.as_ptr(), len)
        );
        assert_eq!(rc, 0, "write of {len} at {off:#x} is outside {area:?}");
    }

    /// `boot_set_pending(permanent)`, on the secondary slot.
    pub fn set_pending(&mut self, permanent: bool) -> Result<(), Refusal> {
        refusal(call!(self, bm_mcuboot_set_pending(c_int::from(permanent))))
    }

    /// `boot_set_confirmed()`, on the primary slot.
    pub fn set_confirmed(&mut self) -> Result<(), Refusal> {
        refusal(call!(self, bm_mcuboot_set_confirmed()))
    }

    /// `boot_swap_type()`: a [`swap_type`] constant.
    pub fn swap_type(&self) -> Result<i32, Refusal> {
        match call!(self, bm_mcuboot_swap_type()) {
            ffi::ASSERTED => Err(Refusal::Asserted),
            kind => Ok(kind),
        }
    }

    /// `boot_go()`: one boot of the bootloader, including any swap.
    pub fn boot_go(&mut self) -> Result<Booted, Refusal> {
        let mut booted = Booted {
            image_off: 0,
            flash_dev_id: 0,
            header: [0; HEADER_SIZE],
        };
        refusal(call!(
            self,
            bm_mcuboot_boot_go(
                &mut booted.image_off,
                &mut booted.flash_dev_id,
                booted.header.as_mut_ptr()
            )
        ))?;
        Ok(booted)
    }
}

/// tinycrypt's SHA-256, which `image_validate.c` hashes an image with. For
/// building test images without a dependency.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let len = u32::try_from(data.len()).expect("under 4 GiB");
    let mut digest = [0; 32];
    // SAFETY: pure; `data` is `len` bytes and `digest` is the 32 it writes.
    unsafe { ffi::unsigned::bm_mcuboot_sha256(data.as_ptr(), len, digest.as_mut_ptr()) };
    digest
}

/// `csrc/bm_mcuboot.h`, once per build.
mod ffi {
    /// `BM_MCUBOOT_ASSERTED`.
    pub const ASSERTED: i32 = i32::MIN;

    macro_rules! declare {
        ($module:ident, $prefix:literal) => {
            pub mod $module {
                use std::ffi::c_int;

                unsafe extern "C" {
                    #[link_name = concat!($prefix, "bm_mcuboot_flash_reset")]
                    pub fn bm_mcuboot_flash_reset();
                    #[link_name = concat!($prefix, "bm_mcuboot_flash_load")]
                    pub fn bm_mcuboot_flash_load(id: u8, off: u32, dst: *mut u8, len: u32)
                    -> c_int;
                    #[link_name = concat!($prefix, "bm_mcuboot_flash_store")]
                    pub fn bm_mcuboot_flash_store(
                        id: u8,
                        off: u32,
                        src: *const u8,
                        len: u32,
                    ) -> c_int;
                    #[link_name = concat!($prefix, "bm_mcuboot_set_pending")]
                    pub fn bm_mcuboot_set_pending(permanent: c_int) -> i32;
                    #[link_name = concat!($prefix, "bm_mcuboot_set_confirmed")]
                    pub fn bm_mcuboot_set_confirmed() -> i32;
                    #[link_name = concat!($prefix, "bm_mcuboot_swap_type")]
                    pub fn bm_mcuboot_swap_type() -> i32;
                    #[link_name = concat!($prefix, "bm_mcuboot_boot_go")]
                    pub fn bm_mcuboot_boot_go(
                        image_off: *mut u32,
                        flash_dev_id: *mut u8,
                        header: *mut u8,
                    ) -> i32;
                    #[allow(dead_code)]
                    #[link_name = concat!($prefix, "bm_mcuboot_sha256")]
                    pub fn bm_mcuboot_sha256(data: *const u8, len: u32, digest: *mut u8);
                }
            }
        };
    }

    declare!(unsigned, "");
    // build.rs's SIGNED_PREFIX.
    declare!(ed25519, "ed25519_");
}
