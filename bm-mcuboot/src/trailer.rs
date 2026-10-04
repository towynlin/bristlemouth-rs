//! The end of a slot: the fields `boot/bootutil/src/bootutil_public.c` reads
//! and writes, and its `boot_set_pending` and `boot_set_confirmed`.
//!
//! Offsets are from the start of the slot. With swap with scratch and no
//! encryption, from the end of a slot of alignment `a` (`bootutil_priv.h`):
//!
//! | Field | Offset from the end, `a` = 16 | Size |
//! |---|---|---|
//! | swap status | above `swap_size` | `BOOT_MAX_IMG_SECTORS * 3 * a` |
//! | `swap_size` | 80 | `a` |
//! | `swap_info` | 64 | 1, then `0xFF` to `a` |
//! | `copy_done` | 48 | 1, then `0xFF` to `a` |
//! | `image_ok` | 32 | 1, then `0xFF` to `a` |
//! | magic | 16 | 16 |
//!
//! What the C does that its comments do not say, reproduced here:
//!
//! | Case | Behaviour |
//! |---|---|
//! | `set_pending`, magic already good | Returns 0 and writes nothing, also when `permanent` is asked of a slot pending a test. |
//! | `set_pending`, magic neither good nor erased | Erases the whole slot, ignores the erase's result, returns `BOOT_EBADIMAGE`. |
//! | `set_pending`, a write fails after the magic | Returns `BOOT_EFLASH` with the magic in place: the slot is pending, and a second call returns 0. |
//! | `set_confirmed`, magic erased | Returns 0 and writes nothing: a slot programmed directly, or reverted, has no trailer. |
//! | `set_confirmed`, `image_ok` neither `0x01` nor erased | Returns 0 and writes nothing. `boot_swap_type` then does not revert. |
//! | `swap_info` whose low nibble is over `BOOT_SWAP_TYPE_REVERT` | Read as `BOOT_SWAP_TYPE_NONE`, image 0. |
//!
//! Not reproduced: `boot_read_swap_state` treats a positive return from
//! `flash_area_read` as success; [`Flash::read`] has one failure.

/// `BOOT_MAGIC_SZ`.
pub const MAGIC_SIZE: usize = 16;
/// `flash_area_erased_val`.
pub const ERASED: u8 = 0xFF;
/// `MCUBOOT_MAX_IMG_SECTORS` in bm_protocol's `mcuboot_config.h`.
pub const BM_MAX_IMG_SECTORS: u32 = 121;

/// `BOOT_FLAG_SET`, the byte written to flash.
const FLAG_SET: u8 = 1;
/// The largest `BOOT_MAX_ALIGN` `bootutil_public.h` accepts.
const MAX_ALIGN: usize = 32;

/// `boot_img_magic` when `BOOT_MAX_ALIGN` is 8.
const MAGIC_ALIGN_8: [u8; MAGIC_SIZE] = [
    0x77, 0xc2, 0x95, 0xf3, 0x60, 0xd2, 0xef, 0x7f, 0x35, 0x52, 0x50, 0x0f, 0x2c, 0xb6, 0x79, 0x80,
];
/// `boot_img_magic.magic` otherwise; `BOOT_MAX_ALIGN` as a `u16` precedes it.
const MAGIC_TAIL: [u8; MAGIC_SIZE - 2] = [
    0x2d, 0xe1, 0x5d, 0x29, 0x41, 0x0b, 0x8d, 0x77, 0x67, 0x9c, 0x11, 0x0f, 0x1f, 0x8a,
];

/// A flash operation failed. `bootutil` reports every one as `BOOT_EFLASH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashError;

/// One slot's flash. Offsets are from the start of the slot.
///
/// `bm-mcuboot-diff` compares against an oracle whose `write` clears bits
/// and then fails unless the bytes read back as given, as bm_protocol's
/// `port_flash.c` verifies. An implementation that does not verify returns
/// `Ok` where the C returns `BOOT_EFLASH`.
pub trait Flash {
    /// Fill `buf` from `off`.
    fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError>;
    /// Program `data` at `off`. Every call here is one `align`-sized block
    /// at a multiple of `align`, or the magic's.
    fn write(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError>;
    /// Erase `len` bytes from `off`. Called only by [`set_pending`], with
    /// the whole slot, when the magic is bad.
    fn erase(&mut self, off: u32, len: u32) -> Result<(), FlashError>;
}

/// A non-zero return of `boot_set_pending` or `boot_set_confirmed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `BOOT_EFLASH`: a read or write failed.
    Flash,
    /// `BOOT_EBADIMAGE`: [`set_pending`] found a bad magic and erased the
    /// slot.
    BadImage,
    /// `BOOT_EBADVECT`: [`set_confirmed`] found a bad magic.
    BadVect,
}

impl Error {
    /// The C's return value.
    pub const fn code(self) -> i32 {
        match self {
            Error::Flash => 1,
            Error::BadImage => 3,
            Error::BadVect => 4,
        }
    }
}

impl From<FlashError> for Error {
    fn from(_: FlashError) -> Self {
        Error::Flash
    }
}

/// Where a slot's trailer fields are.
///
/// `align` is both `BOOT_MAX_ALIGN` and `flash_area_align`, which
/// bm_protocol sets from the one `MCUBOOT_BOOT_MAX_ALIGN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trailer {
    slot_size: u32,
    align: u32,
}

impl Trailer {
    /// bm_protocol's slots: `0xF2000` bytes, alignment 16.
    pub const BM: Trailer = Trailer {
        slot_size: 0xF2000,
        align: 16,
    };

    /// `None` unless `align` is 8, 16 or 32, `slot_size` is a multiple of
    /// it, and the slot holds the fields.
    ///
    /// The C's offsets wrap in a smaller slot, and in a slot that is not a
    /// multiple of `align` `boot_write_magic` writes the magic below where
    /// `boot_read_swap_state` reads it.
    pub const fn new(slot_size: u32, align: u32) -> Option<Self> {
        if !matches!(align, 8 | 16 | 32) || !slot_size.is_multiple_of(align) {
            return None;
        }
        let trailer = Self { slot_size, align };
        if slot_size < trailer.info_size() {
            return None;
        }
        Some(trailer)
    }

    pub const fn slot_size(&self) -> u32 {
        self.slot_size
    }

    pub const fn align(&self) -> u32 {
        self.align
    }

    /// `BOOT_MAGIC_ALIGN_SIZE`.
    const fn magic_align_size(&self) -> u32 {
        if self.align > MAGIC_SIZE as u32 {
            self.align
        } else {
            MAGIC_SIZE as u32
        }
    }

    /// `boot_trailer_info_sz`: everything below the swap status.
    pub const fn info_size(&self) -> u32 {
        self.align * 4 + self.magic_align_size()
    }

    /// `boot_trailer_sz` for `max_sectors` (`BOOT_MAX_IMG_SECTORS`):
    /// the swap status and [`Trailer::info_size`].
    pub const fn size(&self, max_sectors: u32) -> Option<u32> {
        match max_sectors.checked_mul(3 * self.align) {
            Some(status) => status.checked_add(self.info_size()),
            None => None,
        }
    }

    /// `boot_status_off` for a slot: where the trailer starts.
    pub const fn status_off(&self, max_sectors: u32) -> Option<u32> {
        match self.size(max_sectors) {
            Some(size) => self.slot_size.checked_sub(size),
            None => None,
        }
    }

    /// `boot_magic_off`.
    pub const fn magic_off(&self) -> u32 {
        self.slot_size - MAGIC_SIZE as u32
    }

    /// `boot_image_ok_off`.
    pub const fn image_ok_off(&self) -> u32 {
        (self.magic_off() - self.align) & !(self.align - 1)
    }

    /// `boot_copy_done_off`.
    pub const fn copy_done_off(&self) -> u32 {
        self.image_ok_off() - self.align
    }

    /// `boot_swap_info_off`.
    pub const fn swap_info_off(&self) -> u32 {
        self.copy_done_off() - self.align
    }

    /// `boot_swap_size_off`.
    pub const fn swap_size_off(&self) -> u32 {
        self.swap_info_off() - self.align
    }

    /// `BOOT_IMG_MAGIC`: the 16 bytes at [`Trailer::magic_off`] of a slot
    /// whose magic is good.
    pub const fn magic(&self) -> [u8; MAGIC_SIZE] {
        if self.align == 8 {
            return MAGIC_ALIGN_8;
        }
        let mut magic = [0; MAGIC_SIZE];
        let align = (self.align as u16).to_le_bytes();
        magic[0] = align[0];
        magic[1] = align[1];
        let mut i = 0;
        while i < MAGIC_TAIL.len() {
            magic[2 + i] = MAGIC_TAIL[i];
            i += 1;
        }
        magic
    }
}

/// `BOOT_MAGIC_GOOD`, `_BAD`, `_UNSET`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magic {
    Good,
    /// Neither the magic nor erased.
    Bad,
    /// All 16 bytes erased.
    Unset,
}

/// `BOOT_FLAG_SET`, `_BAD`, `_UNSET`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    /// `0x01`.
    Set,
    /// Neither `0x01` nor erased.
    Bad,
    /// Erased.
    Unset,
}

/// `BOOT_SWAP_TYPE_*`, less `FAIL` and `PANIC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapType {
    None = 1,
    Test = 2,
    Perm = 3,
    Revert = 4,
}

/// `struct boot_swap_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapState {
    pub magic: Magic,
    /// The low nibble of `swap_info`, a `BOOT_SWAP_TYPE_*` value; 1
    /// (`NONE`) if the byte is erased or the nibble is over 4 (`REVERT`).
    /// A nibble of 0 is none of the C's constants and is kept.
    pub swap_type: u8,
    /// The high nibble of `swap_info`; 0 where `swap_type` was replaced.
    pub image_num: u8,
    pub copy_done: Flag,
    pub image_ok: Flag,
}

/// `boot_read_swap_state`.
pub fn read_swap_state<F: Flash>(flash: &mut F, trailer: &Trailer) -> Result<SwapState, Error> {
    let mut magic = [0; MAGIC_SIZE];
    flash.read(trailer.magic_off(), &mut magic)?;
    let magic = if magic == [ERASED; MAGIC_SIZE] {
        Magic::Unset
    } else if magic == trailer.magic() {
        Magic::Good
    } else {
        Magic::Bad
    };

    let mut swap_info = [0];
    flash.read(trailer.swap_info_off(), &mut swap_info)?;
    let swap_info = swap_info[0];
    // BOOT_GET_SWAP_TYPE and BOOT_GET_IMAGE_NUM.
    let (mut swap_type, mut image_num) = (swap_info & 0x0F, swap_info >> 4);
    if swap_info == ERASED || swap_type > SwapType::Revert as u8 {
        swap_type = SwapType::None as u8;
        image_num = 0;
    }

    let copy_done = read_flag(flash, trailer.copy_done_off())?;
    let image_ok = read_flag(flash, trailer.image_ok_off())?;
    Ok(SwapState {
        magic,
        swap_type,
        image_num,
        copy_done,
        image_ok,
    })
}

/// `boot_read_flag`.
fn read_flag<F: Flash>(flash: &mut F, off: u32) -> Result<Flag, Error> {
    let mut flag = [0];
    flash.read(off, &mut flag)?;
    Ok(match flag[0] {
        ERASED => Flag::Unset,
        FLAG_SET => Flag::Set,
        _ => Flag::Bad,
    })
}

/// `boot_write_magic`: the magic, preceded by `0xFF` to a write boundary.
fn write_magic<F: Flash>(flash: &mut F, trailer: &Trailer) -> Result<(), Error> {
    let len = trailer.magic_align_size() as usize;
    let mut buf = [ERASED; MAX_ALIGN];
    buf[len - MAGIC_SIZE..len].copy_from_slice(&trailer.magic());
    let pad_off = trailer.magic_off() & !(trailer.align - 1);
    Ok(flash.write(pad_off, &buf[..len])?)
}

/// `boot_write_trailer` with one byte: `value`, then `0xFF` to `align`.
fn write_byte<F: Flash>(
    flash: &mut F,
    trailer: &Trailer,
    off: u32,
    value: u8,
) -> Result<(), Error> {
    let mut buf = [ERASED; MAX_ALIGN];
    buf[0] = value;
    Ok(flash.write(off, &buf[..trailer.align as usize])?)
}

/// `boot_set_pending(permanent)` on the secondary slot: the magic, then
/// `image_ok` if `permanent`, then `swap_info`.
///
/// Writes only when the magic is erased. See the module's table.
pub fn set_pending<F: Flash>(
    flash: &mut F,
    trailer: &Trailer,
    permanent: bool,
) -> Result<(), Error> {
    match read_swap_state(flash, trailer)?.magic {
        Magic::Good => Ok(()),
        Magic::Unset => {
            write_magic(flash, trailer)?;
            if permanent {
                write_byte(flash, trailer, trailer.image_ok_off(), FLAG_SET)?;
            }
            let swap_type = if permanent {
                SwapType::Perm
            } else {
                SwapType::Test
            };
            // BOOT_SET_SWAP_INFO with image 0.
            write_byte(flash, trailer, trailer.swap_info_off(), swap_type as u8)
        }
        Magic::Bad => {
            let _ = flash.erase(0, trailer.slot_size);
            Err(Error::BadImage)
        }
    }
}

/// `boot_set_confirmed()` on the primary slot: `image_ok`.
///
/// Writes only when the magic is good and `image_ok` is erased. See the
/// module's table.
pub fn set_confirmed<F: Flash>(flash: &mut F, trailer: &Trailer) -> Result<(), Error> {
    let state = read_swap_state(flash, trailer)?;
    match state.magic {
        Magic::Good => {}
        Magic::Unset => return Ok(()),
        Magic::Bad => return Err(Error::BadVect),
    }
    if state.image_ok != Flag::Unset {
        return Ok(());
    }
    write_byte(flash, trailer, trailer.image_ok_off(), FLAG_SET)
}

/// `boot_swap_type`: what the bootloader does next, from both slots'
/// trailers. `boot_swap_tables`, first match.
pub const fn swap_type(primary: &SwapState, secondary: &SwapState) -> SwapType {
    match (secondary.magic, secondary.image_ok) {
        (Magic::Good, Flag::Unset) => return SwapType::Test,
        (Magic::Good, Flag::Set) => return SwapType::Perm,
        _ => {}
    }
    if matches!(primary.magic, Magic::Good)
        && matches!(secondary.magic, Magic::Unset)
        && matches!(primary.image_ok, Flag::Unset)
        && matches!(primary.copy_done, Flag::Set)
    {
        return SwapType::Revert;
    }
    SwapType::None
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// `boot_img_magic` for `BOOT_MAX_ALIGN` 16.
    const MAGIC_16: [u8; 16] = [
        0x10, 0x00, 0x2d, 0xe1, 0x5d, 0x29, 0x41, 0x0b, 0x8d, 0x77, 0x67, 0x9c, 0x11, 0x0f, 0x1f,
        0x8a,
    ];

    /// A slot that records what is asked of it. `write` stores the bytes.
    struct Slot {
        bytes: Vec<u8>,
        writes: Vec<(u32, Vec<u8>)>,
        erases: Vec<(u32, u32)>,
        /// Reads allowed before one fails.
        reads_left: usize,
        fail_writes: bool,
    }

    impl Slot {
        fn erased(size: usize) -> Self {
            Self {
                bytes: vec![ERASED; size],
                writes: Vec::new(),
                erases: Vec::new(),
                reads_left: usize::MAX,
                fail_writes: false,
            }
        }
    }

    impl Flash for Slot {
        fn read(&mut self, off: u32, buf: &mut [u8]) -> Result<(), FlashError> {
            if self.reads_left == 0 {
                return Err(FlashError);
            }
            self.reads_left -= 1;
            let off = off as usize;
            buf.copy_from_slice(&self.bytes[off..off + buf.len()]);
            Ok(())
        }

        fn write(&mut self, off: u32, data: &[u8]) -> Result<(), FlashError> {
            self.writes.push((off, data.to_vec()));
            if self.fail_writes {
                return Err(FlashError);
            }
            let at = off as usize;
            self.bytes[at..at + data.len()].copy_from_slice(data);
            Ok(())
        }

        fn erase(&mut self, off: u32, len: u32) -> Result<(), FlashError> {
            self.erases.push((off, len));
            Err(FlashError)
        }
    }

    fn block(first: u8, len: usize) -> Vec<u8> {
        let mut block = vec![ERASED; len];
        block[0] = first;
        block
    }

    #[test]
    fn bm_protocols_offsets() {
        let t = Trailer::BM;
        assert_eq!(Trailer::new(0xF2000, 16), Some(t));
        assert_eq!(t.magic(), MAGIC_16);
        assert_eq!(t.magic_off(), 0xF2000 - 16);
        assert_eq!(t.image_ok_off(), 0xF2000 - 32);
        assert_eq!(t.copy_done_off(), 0xF2000 - 48);
        assert_eq!(t.swap_info_off(), 0xF2000 - 64);
        assert_eq!(t.swap_size_off(), 0xF2000 - 80);
        assert_eq!(t.info_size(), 80);
        // 121 sectors of three 16-byte states, and the 80 above.
        assert_eq!(t.size(BM_MAX_IMG_SECTORS), Some(5888));
        assert_eq!(t.status_off(BM_MAX_IMG_SECTORS), Some(0xF2000 - 5888));
        assert_eq!(t.size(u32::MAX), None);
        assert_eq!(t.status_off(0x10000), None);
    }

    #[test]
    fn other_alignments() {
        let t = Trailer::new(0x1000, 8).unwrap();
        assert_eq!(t.magic(), MAGIC_ALIGN_8);
        assert_eq!(
            [
                t.magic_off(),
                t.image_ok_off(),
                t.copy_done_off(),
                t.swap_info_off(),
                t.swap_size_off()
            ],
            [
                0x1000 - 16,
                0x1000 - 24,
                0x1000 - 32,
                0x1000 - 40,
                0x1000 - 48
            ]
        );
        assert_eq!(t.info_size(), 48);

        let t = Trailer::new(0x1000, 32).unwrap();
        assert_eq!(t.magic()[..2], [0x20, 0x00]);
        assert_eq!(t.magic()[2..], MAGIC_16[2..]);
        assert_eq!(
            [
                t.magic_off(),
                t.image_ok_off(),
                t.copy_done_off(),
                t.swap_info_off(),
                t.swap_size_off()
            ],
            [
                0x1000 - 16,
                0x1000 - 64,
                0x1000 - 96,
                0x1000 - 128,
                0x1000 - 160
            ]
        );
        assert_eq!(t.info_size(), 160);

        for (size, align) in [
            (0x1000, 4),
            (0x1000, 12),
            (0x1000, 64),
            (0x1008, 16),
            (64, 16),
        ] {
            assert_eq!(Trailer::new(size, align), None, "{size:#x} {align}");
        }
        assert!(Trailer::new(80, 16).is_some());
    }

    #[test]
    fn set_pending_writes_the_magic_then_swap_info() {
        let t = Trailer::BM;
        let mut slot = Slot::erased(0xF2000);
        assert_eq!(set_pending(&mut slot, &t, false), Ok(()));
        assert_eq!(
            slot.writes,
            [
                (0xF2000 - 16, MAGIC_16.to_vec()),
                (0xF2000 - 64, block(0x02, 16))
            ]
        );
        let state = read_swap_state(&mut slot, &t).unwrap();
        assert_eq!(
            state,
            SwapState {
                magic: Magic::Good,
                swap_type: 2,
                image_num: 0,
                copy_done: Flag::Unset,
                image_ok: Flag::Unset,
            }
        );

        // Already pending: nothing more, permanent or not.
        slot.writes.clear();
        assert_eq!(set_pending(&mut slot, &t, true), Ok(()));
        assert_eq!(slot.writes, []);
    }

    #[test]
    fn set_pending_permanent_writes_image_ok_between() {
        let t = Trailer::BM;
        let mut slot = Slot::erased(0xF2000);
        assert_eq!(set_pending(&mut slot, &t, true), Ok(()));
        assert_eq!(
            slot.writes,
            [
                (0xF2000 - 16, MAGIC_16.to_vec()),
                (0xF2000 - 32, block(0x01, 16)),
                (0xF2000 - 64, block(0x03, 16))
            ]
        );
    }

    #[test]
    fn set_pending_erases_a_slot_with_a_bad_magic() {
        let t = Trailer::BM;
        let mut slot = Slot::erased(0xF2000);
        slot.bytes[0xF2000 - 1] = 0x00;
        // The erase fails here, and the result is the same.
        assert_eq!(set_pending(&mut slot, &t, false), Err(Error::BadImage));
        assert_eq!(slot.erases, [(0, 0xF2000)]);
        assert_eq!(slot.writes, []);
        assert_eq!(Error::BadImage.code(), 3);
    }

    #[test]
    fn set_confirmed_writes_image_ok_once() {
        let t = Trailer::BM;
        let mut slot = Slot::erased(0xF2000);

        // No trailer: nothing to confirm.
        assert_eq!(set_confirmed(&mut slot, &t), Ok(()));
        assert_eq!(slot.writes, []);

        slot.bytes[0xF2000 - 16..].copy_from_slice(&MAGIC_16);
        assert_eq!(set_confirmed(&mut slot, &t), Ok(()));
        assert_eq!(slot.writes, [(0xF2000 - 32, block(0x01, 16))]);

        slot.writes.clear();
        assert_eq!(set_confirmed(&mut slot, &t), Ok(()));
        // A bad image_ok is left as it is.
        slot.bytes[0xF2000 - 32] = 0x00;
        assert_eq!(set_confirmed(&mut slot, &t), Ok(()));
        assert_eq!(slot.writes, []);

        slot.bytes[0xF2000 - 16] = 0x11;
        assert_eq!(set_confirmed(&mut slot, &t), Err(Error::BadVect));
        assert_eq!(slot.writes, []);
        assert_eq!(Error::BadVect.code(), 4);
    }

    #[test]
    fn flash_failures_are_eflash() {
        let t = Trailer::BM;
        // boot_read_swap_state makes four reads.
        for reads in 0..4 {
            let mut slot = Slot::erased(0xF2000);
            slot.reads_left = reads;
            assert_eq!(read_swap_state(&mut slot, &t), Err(Error::Flash));
            slot.reads_left = reads;
            assert_eq!(set_pending(&mut slot, &t, false), Err(Error::Flash));
            slot.reads_left = reads;
            assert_eq!(set_confirmed(&mut slot, &t), Err(Error::Flash));
            assert_eq!(slot.writes, []);
        }

        // The first failed write ends the call.
        let mut slot = Slot::erased(0xF2000);
        slot.fail_writes = true;
        assert_eq!(set_pending(&mut slot, &t, true), Err(Error::Flash));
        assert_eq!(slot.writes.len(), 1);
        slot.bytes[0xF2000 - 16..].copy_from_slice(&MAGIC_16);
        assert_eq!(set_confirmed(&mut slot, &t), Err(Error::Flash));
        assert_eq!(Error::Flash.code(), 1);
    }

    #[test]
    fn wider_alignments_pad_the_writes() {
        let t = Trailer::new(0x1000, 32).unwrap();
        let mut slot = Slot::erased(0x1000);
        assert_eq!(set_pending(&mut slot, &t, false), Ok(()));
        let mut magic = vec![ERASED; 16];
        magic.extend_from_slice(&t.magic());
        assert_eq!(
            slot.writes,
            [(0x1000 - 32, magic), (0x1000 - 128, block(0x02, 32))]
        );

        let t = Trailer::new(0x1000, 8).unwrap();
        let mut slot = Slot::erased(0x1000);
        assert_eq!(set_pending(&mut slot, &t, true), Ok(()));
        assert_eq!(
            slot.writes,
            [
                (0x1000 - 16, MAGIC_ALIGN_8.to_vec()),
                (0x1000 - 24, block(0x01, 8)),
                (0x1000 - 40, block(0x03, 8))
            ]
        );
    }

    #[test]
    fn swap_info_nibbles() {
        let t = Trailer::BM;
        let mut slot = Slot::erased(0xF2000);
        for (byte, swap_type, image_num) in [
            (0xFF, 1, 0),
            (0x00, 0, 0),
            (0x30, 0, 3),
            (0x21, 1, 2),
            (0x12, 2, 1),
            (0x03, 3, 0),
            (0xF4, 4, 15),
            (0x25, 1, 0),
            (0x1F, 1, 0),
        ] {
            slot.bytes[t.swap_info_off() as usize] = byte;
            let state = read_swap_state(&mut slot, &t).unwrap();
            assert_eq!((state.swap_type, state.image_num), (swap_type, image_num));
        }
    }

    #[test]
    fn swap_type_table() {
        let state = |magic, image_ok, copy_done| SwapState {
            magic,
            swap_type: 1,
            image_num: 0,
            copy_done,
            image_ok,
        };
        let empty = state(Magic::Unset, Flag::Unset, Flag::Unset);
        let swapped = state(Magic::Good, Flag::Unset, Flag::Set);
        let confirmed = state(Magic::Good, Flag::Set, Flag::Set);

        assert_eq!(swap_type(&empty, &empty), SwapType::None);
        assert_eq!(
            swap_type(&empty, &state(Magic::Good, Flag::Unset, Flag::Unset)),
            SwapType::Test
        );
        assert_eq!(
            swap_type(&empty, &state(Magic::Good, Flag::Set, Flag::Unset)),
            SwapType::Perm
        );
        assert_eq!(
            swap_type(&empty, &state(Magic::Good, Flag::Bad, Flag::Unset)),
            SwapType::None
        );
        assert_eq!(swap_type(&swapped, &empty), SwapType::Revert);
        assert_eq!(swap_type(&confirmed, &empty), SwapType::None);
        // The secondary slot's row comes first.
        assert_eq!(swap_type(&swapped, &swapped), SwapType::Test);
        assert_eq!(
            swap_type(&swapped, &state(Magic::Bad, Flag::Unset, Flag::Unset)),
            SwapType::None
        );
    }
}
