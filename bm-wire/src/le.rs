//! Little-endian field reads shared by the codecs.

use crate::BmWireError;

/// The first `N` bytes of `buf`, or [`BmWireError::Truncated`].
pub(crate) fn prefix<const N: usize>(buf: &[u8]) -> Result<&[u8; N], BmWireError> {
    buf.first_chunk().ok_or(BmWireError::Truncated)
}

/// `buf[at..at + N]`. Panics as indexing does if `buf` is too short.
fn array_at<const N: usize>(buf: &[u8], at: usize) -> [u8; N] {
    let mut bytes = [0; N];
    bytes.copy_from_slice(&buf[at..at + N]);
    bytes
}

/// The `u16` at `buf[at..]`. Panics as indexing does if `buf` is too short.
pub(crate) fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(array_at(buf, at))
}

/// The `u32` at `buf[at..]`. Panics as indexing does if `buf` is too short.
pub(crate) fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(array_at(buf, at))
}

/// The `u64` at `buf[at..]`. Panics as indexing does if `buf` is too short.
pub(crate) fn u64_at(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(array_at(buf, at))
}
