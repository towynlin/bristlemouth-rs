//! The no-init RAM a C image, a Rust image and the bootloader share: the top
//! 512 bytes of RAM, which `memory.x` leaves out of `RAM` and no startup code
//! zeroes.
//!
//! | Address | bm_protocol symbol | Here |
//! |---|---|---|
//! | `0x200BFE00` | `ulBootloaderMagic` | never written |
//! | `0x200BFE04` | `resetReason` | [`reset`], [`take_reset_reason`] |
//! | `0x200BFE08` | `ulResetReasonMagic` | [`reset`], [`take_reset_reason`] |
//! | `0x200BFE0C` | memfault `s_reboot_tracking`, `0x40` bytes | never written |
//! | `0x200BFE4C` | `client_update_reboot_info`, 18 bytes | [`NoInit`] |
//!
//! `README.md`, "No-init RAM", has the sources.

use bm_stack::NoInitRam;
use bm_wire::bcmp::dfu_core::RebootInfo;
use cortex_m::peripheral::SCB;

/// `resetReason`: a `ResetReason_t`, 4 bytes.
pub const RESET_REASON_ADDR: usize = 0x200B_FE04;
/// `ulResetReasonMagic`.
pub const RESET_REASON_MAGIC_ADDR: usize = 0x200B_FE08;
/// `client_update_reboot_info`, in [`RebootInfo::encode`]'s layout.
pub const REBOOT_INFO_ADDR: usize = 0x200B_FE4C;

/// `RESET_REASON_MAGIC`: [`RESET_REASON_ADDR`] holds a reason written before
/// the last reset.
pub const RESET_REASON_MAGIC: u32 = 0xB827_8F7D;

/// `ResetReason_t`, with its C values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, defmt::Format)]
#[repr(u32)]
pub enum ResetReason {
    /// `RESET_REASON_NONE`.
    None = 0,
    /// `RESET_REASON_DEBUG_RESET`.
    DebugReset = 1,
    /// `RESET_REASON_MEM_FAULT`.
    MemFault = 2,
    /// `RESET_REASON_BOOTLOADER`: into the ROM bootloader.
    Bootloader = 3,
    /// `RESET_REASON_MCUBOOT`: a DFU client booting the image it received.
    Mcuboot = 4,
    /// `RESET_REASON_CONFIG`: a config partition was committed.
    Config = 5,
    /// `RESET_REASON_UPDATE_FAILED`: a DFU client going back to its previous
    /// image.
    UpdateFailed = 6,
    /// `RESET_REASON_MICROPYTHON`.
    Micropython = 7,
    /// `RESET_REASON_INVALID`: no reason was written, as after power-on, a
    /// pin reset, a watchdog reset or a probe's reset.
    Invalid = 8,
}

impl ResetReason {
    /// The reason with C value `raw`; [`Self::Invalid`] for any other.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::None,
            1 => Self::DebugReset,
            2 => Self::MemFault,
            3 => Self::Bootloader,
            4 => Self::Mcuboot,
            5 => Self::Config,
            6 => Self::UpdateFailed,
            7 => Self::Micropython,
            _ => Self::Invalid,
        }
    }
}

fn read_u32(addr: usize) -> u32 {
    // SAFETY: `addr` is one of this module's word-aligned constants, inside
    // RAM the linker places nothing in.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn write_u32(addr: usize, value: u32) {
    // SAFETY: as `read_u32`.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

/// `resetSystem`: write the magic and `reason`, then reset.
pub fn reset(reason: ResetReason) -> ! {
    write_u32(RESET_REASON_MAGIC_ADDR, RESET_REASON_MAGIC);
    write_u32(RESET_REASON_ADDR, reason as u32);
    SCB::sys_reset()
}

/// `checkResetReason`, first call: the reason [`reset`] or a C image's
/// `resetSystem` wrote before this boot, or [`ResetReason::Invalid`] without
/// the magic. Clears the magic and sets the stored reason to
/// [`ResetReason::Invalid`], so a second call returns [`ResetReason::Invalid`]
/// where the C returns its cached value. [`crate::start`] calls it;
/// [`crate::Board::reset_reason`] is the result.
pub fn take_reset_reason() -> ResetReason {
    let magic = read_u32(RESET_REASON_MAGIC_ADDR);
    write_u32(RESET_REASON_MAGIC_ADDR, 0);
    let reason = if magic == RESET_REASON_MAGIC {
        ResetReason::from_raw(read_u32(RESET_REASON_ADDR))
    } else {
        ResetReason::Invalid
    };
    write_u32(RESET_REASON_ADDR, ResetReason::Invalid as u32);
    reason
}

/// [`NoInitRam`] at [`REBOOT_INFO_ADDR`].
///
/// After power-on the bytes are whatever RAM holds, which [`NoInitRam::load`]
/// returns as it is; the DFU client acts only on `DFU_REBOOT_MAGIC`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoInit;

impl NoInitRam for NoInit {
    fn load(&mut self) -> RebootInfo {
        let mut bytes = [0u8; RebootInfo::LEN];
        for (i, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: `RebootInfo::LEN` bytes from `REBOOT_INFO_ADDR` are RAM
            // the linker places nothing in.
            *byte = unsafe { core::ptr::read_volatile((REBOOT_INFO_ADDR + i) as *const u8) };
        }
        RebootInfo::decode(&bytes).expect("LEN bytes")
    }

    fn store(&mut self, info: &RebootInfo) {
        let mut bytes = [0u8; RebootInfo::LEN];
        info.encode(&mut bytes).expect("LEN bytes");
        for (i, byte) in bytes.iter().enumerate() {
            // SAFETY: as `load`.
            unsafe { core::ptr::write_volatile((REBOOT_INFO_ADDR + i) as *mut u8, *byte) };
        }
    }
}
