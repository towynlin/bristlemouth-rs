//! MCUboot v1.9.0's image header, TLV area and slot trailer, as bm_protocol's
//! bootloader builds it: swap with scratch, `BOOT_MAX_ALIGN` and write
//! alignment 16, erased value `0xFF`.
//!
//! Layout, encode and decode only. No SHA-256 and no signing.
//!
//! | For | Call |
//! |---|---|
//! | Building an image | [`Header::encode`], [`tlv::encode_unsigned`], [`tlv::encode_ed25519`] |
//! | Reading one | [`Header::decode`], [`TlvArea::parse`] |
//! | Marking slot 2 for a swap | [`set_pending`] |
//! | Keeping the image in slot 1 | [`set_confirmed`] |
//! | What the bootloader will do | [`read_swap_state`], [`swap_type`] |
//!
//! `bm-mcuboot-diff` compares the trailer functions with the C over the
//! same flash.
#![no_std]
#![forbid(unsafe_code)]

pub mod image;
pub mod tlv;
pub mod trailer;

pub use image::{Header, Version};
pub use tlv::{Tlv, TlvArea, TlvError};
pub use trailer::{
    Error, Flag, Flash, FlashError, Magic, SwapState, SwapType, Trailer, read_swap_state,
    set_confirmed, set_pending, swap_type,
};
