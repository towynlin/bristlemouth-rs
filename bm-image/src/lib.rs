//! The files a Bristlemouth node's MCUboot bootloader and DFU take, as
//! bm_protocol's build makes them (`docs/history/mcuboot-todo.md`, contract 2).
//! Host-only.
//!
//! | For | Call |
//! |---|---|
//! | A `.dfu.bin` from an ELF | [`dfu`] |
//! | A `.dfu.bin` from a flat binary | [`from_body`], [`build`] |
//! | A `.unified.bin` | [`unified`] |
//! | Reading a `.dfu.bin` | [`Info::read`] |
//! | A signing key | [`Key::from_pem`] |
//!
//! `tests/gold.rs` compares the output with `imgtool`'s byte for byte;
//! `tests/oracle.rs` boots it on MCUboot's `bootutil`.
#![forbid(unsafe_code)]

pub mod elf;
mod image;
mod info;
mod key;
pub mod version;

pub use image::{
    BODY_BASE, BOOTLOADER_BASE, BOOTLOADER_SIZE, MAX_IMAGE_LEN, build, dfu, from_body, unified,
};
pub use info::Info;
pub use key::Key;
pub use version::VersionInfo;

use std::fmt;

/// Why a file could not be built or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not an ELF32 little-endian file, or one whose headers point outside
    /// it.
    Elf(&'static str),
    /// No loaded section starts at the address the image must start at.
    NothingAt(u32),
    /// A loaded section lies below the address the image starts at.
    SectionBelow { addr: u32, base: u32 },
    /// Two loaded sections share an address.
    Overlap(u32),
    /// The binary holds no `versionInfo_t`.
    NoVersion,
    /// The image would not fit: `len` bytes against `limit`.
    TooLong { len: u64, limit: u64 },
    /// The bootloader is longer than its area.
    BootloaderTooLong { len: u64 },
    /// The file does not start with an MCUboot header.
    NotAnImage,
    /// The file's TLV area does not read.
    Tlv(bm_mcuboot::TlvError),
    /// The key is not an unencrypted PKCS#8 PEM of an ed25519 private key.
    Key(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elf(why) => write!(f, "not a usable ELF: {why}"),
            Self::NothingAt(base) => write!(f, "no loaded section starts at {base:#010x}"),
            Self::SectionBelow { addr, base } => {
                write!(f, "a loaded section is at {addr:#010x}, below {base:#010x}")
            }
            Self::Overlap(addr) => write!(f, "two loaded sections overlap at {addr:#010x}"),
            Self::NoVersion => write!(
                f,
                "no versionInfo_t (magic {:#018x}) in the binary",
                version::MAGIC
            ),
            Self::TooLong { len, limit } => write!(
                f,
                "the image is {len:#x} bytes; the limit is {limit:#x}: header, body and TLVs \
                 in a {:#x}-byte slot, less the trailer imgtool reserves",
                bm_mcuboot::Trailer::BM.slot_size()
            ),
            Self::BootloaderTooLong { len } => write!(
                f,
                "the bootloader is {len:#x} bytes; its area is {BOOTLOADER_SIZE:#x}"
            ),
            Self::NotAnImage => write!(f, "not an MCUboot image"),
            Self::Tlv(e) => write!(f, "the TLV area does not read: {e:?}"),
            Self::Key(why) => write!(f, "not an unencrypted ed25519 PKCS#8 PEM: {why}"),
        }
    }
}

impl std::error::Error for Error {}
