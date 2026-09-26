//! The local config store, ported from `bcmp/configuration.c`.
//!
//! Not a wire module: a node keeps three partitions of up to [`MAX_NUM_KV`]
//! typed key/value pairs in RAM, and saves each to flash as one image with a
//! CRC-32 header. The config messages (`0xA0`–`0xA9`) read and write it.
//!
//! # The image is the state
//!
//! bm_core keeps each partition as a packed `ConfigPartition` struct and
//! writes that struct to flash byte for byte, so everything in it — stale key
//! bytes past a NUL, the tail of a value slot a shorter value did not
//! overwrite, the slot one past the last key — is saved and CRC'd.
//! [`ConfigPartition`] therefore holds the struct's bytes and every operation
//! edits them exactly where the C does. The differential harness compares
//! whole images after every step.
//!
//! # Layout depends on the compiler
//!
//! `ConfigKey` holds a `size_t` and an enum, so its size is the ABI's
//! (divergence #44). [`Layout`] names the widths; [`Layout::LP64`] is the
//! oracle's and `bm_sbc`'s, [`Layout::ARM_EABI_GCC`] is a dev kit's.
//!
//! # Keys are C strings with a separate length
//!
//! Every function takes a `key` pointer and a `key_len`. Lookup compares
//! `key_len` and then `strncmp`s; storing a key `snprintf`s it with `%s`,
//! which ignores `key_len` and reads to a NUL (divergence #45). [`Key`]
//! carries both.
//!
//! # Values are CBOR, read the way tinycbor reads them
//!
//! A stored value is one CBOR item in a 50-byte slot. The getters parse it as
//! tinycbor 0.6's parser does: [`Head`] is `preparse_value`, and
//! [`copy_string`] is `iterate_string_chunks` including its partial copies.
//! Writing uses [`cbor2`] for integer heads and
//! [`crate::cbor::push_f32_wide`] for floats.

use cbor2::core::{Encoder, Header};

use crate::crc::crc32_ieee;

/// Keys a partition can hold. `MAX_NUM_KV`.
pub const MAX_NUM_KV: usize = 50;
/// Longest key accepted, in bytes. `MAX_KEY_LEN_BYTES`. A key this long is
/// accepted but stored truncated; see [`Key`].
pub const MAX_KEY_LEN_BYTES: usize = 32;
/// Size of every value slot. `MAX_CONFIG_BUFFER_SIZE_BYTES`.
pub const MAX_CONFIG_BUFFER_SIZE_BYTES: usize = 50;
/// What `config_init` writes into the header of a partition it could not
/// load. `CONFIG_VERSION`.
pub const CONFIG_VERSION: u32 = 0;
/// `sizeof(ConfigPartitionHeader)`: `crc32`, `version`, `numKeys`.
pub const HEADER_LEN: usize = 9;
/// The largest [`Layout::image_len`], [`Layout::LP64`]'s.
pub const MAX_IMAGE_LEN: usize = Layout::LP64.image_len();

/// `CONFIG_LOAD_TIMEOUT_MS`, which `configuration.c` passes to both
/// `bm_config_read` and `bm_config_write`.
pub const CONFIG_LOAD_TIMEOUT_MS: u32 = 5000;

const KEY_BUF_LEN: usize = MAX_KEY_LEN_BYTES;
const SLOT: usize = MAX_CONFIG_BUFFER_SIZE_BYTES;

/// `BmConfigPartition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Partition {
    /// `BM_CFG_PARTITION_USER`.
    User = 0,
    /// `BM_CFG_PARTITION_SYSTEM`.
    System = 1,
    /// `BM_CFG_PARTITION_HARDWARE`.
    Hardware = 2,
}

impl Partition {
    /// All three, in `BmConfigPartition` order, which is the order
    /// `config_init` loads them in.
    pub const ALL: [Self; 3] = [Self::User, Self::System, Self::Hardware];

    /// The partition numbered `n` on the wire, if there is one.
    #[must_use]
    pub fn from_u8(n: u8) -> Option<Self> {
        Self::ALL.get(usize::from(n)).copied()
    }
}

/// `ConfigDataTypes`, as stored in a key's `value_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ValueType {
    /// `UINT32`.
    Uint32 = 0,
    /// `INT32`.
    Int32 = 1,
    /// `FLOAT`.
    Float = 2,
    /// `STR`.
    Str = 3,
    /// `BYTES`.
    Bytes = 4,
    /// `ARRAY`.
    Array = 5,
}

/// The widths of `ConfigKey`'s `size_t key_len` and enum `value_type`, which
/// fix the partition image's layout (divergence #44).
///
/// A store must use the layout of the firmware whose saved images it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Layout {
    key_len_width: usize,
    value_type_width: usize,
}

impl Layout {
    /// 64-bit Linux (x86-64, AArch64): 8-byte `size_t`, 4-byte enum. 44-byte
    /// keys, 4709-byte image. The oracle's layout, and `bm_sbc`'s.
    pub const LP64: Self = Self {
        key_len_width: 8,
        value_type_width: 4,
    };

    /// `arm-none-eabi-gcc` defaults: 4-byte `size_t`, and `-fshort-enums`, so
    /// a 1-byte enum. 37-byte keys, 4359-byte image. rustc's thumb targets
    /// size a `repr(C)` enum the same way.
    ///
    /// Firmware built with `-fno-short-enums`, or with clang, which does not
    /// shorten enums for these targets, has `new(4, 4)`: 40-byte keys.
    pub const ARM_EABI_GCC: Self = Self {
        key_len_width: 4,
        value_type_width: 1,
    };

    /// A layout with these widths, if they are ones a C ABI uses: `size_t` of
    /// 4 or 8 bytes, an enum of 1, 2 or 4.
    #[must_use]
    pub const fn new(key_len_width: usize, value_type_width: usize) -> Option<Self> {
        let size_ok = matches!(key_len_width, 4 | 8);
        let enum_ok = matches!(value_type_width, 1 | 2 | 4);
        if size_ok && enum_ok {
            Some(Self {
                key_len_width,
                value_type_width,
            })
        } else {
            None
        }
    }

    /// `sizeof(ConfigKey)`.
    #[must_use]
    pub const fn key_size(self) -> usize {
        KEY_BUF_LEN + self.key_len_width + self.value_type_width
    }

    /// `sizeof(ConfigPartition)`: what is saved to and loaded from flash.
    #[must_use]
    pub const fn image_len(self) -> usize {
        HEADER_LEN + MAX_NUM_KV * self.key_size() + MAX_NUM_KV * SLOT
    }

    /// Offset of `keys[i]`.
    #[must_use]
    pub const fn key_offset(self, i: usize) -> usize {
        HEADER_LEN + i * self.key_size()
    }

    /// Offset of `values[i]`.
    #[must_use]
    pub const fn value_offset(self, i: usize) -> usize {
        HEADER_LEN + MAX_NUM_KV * self.key_size() + i * SLOT
    }
}

/// A key as bm_core's functions receive it: a `const char *` and a `key_len`.
///
/// `text` is the memory at the pointer, as far as the caller's buffer goes.
/// The C reads it as a NUL-terminated string when it stores a key and as
/// `key_len` characters when it compares one; a NUL inside `text`, or the end
/// of `text`, ends the string, and every byte past the end reads as NUL.
///
/// [`Key::new`] is the usual case, a key with nothing after it. `bcmp/config.c`
/// passes a key directly followed by the message's CBOR value with no NUL
/// between; [`Key::with_len`] expresses that, and the stored key then carries
/// the value's leading bytes (divergence #45).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key<'a> {
    text: &'a [u8],
    len: usize,
}

impl<'a> Key<'a> {
    /// `key` with `key_len == key.len()`.
    #[must_use]
    pub const fn new(key: &'a [u8]) -> Self {
        Self {
            text: key,
            len: key.len(),
        }
    }

    /// The bytes at the key pointer, and a `key_len` that may be shorter or
    /// longer than them.
    #[must_use]
    pub const fn with_len(text: &'a [u8], len: usize) -> Self {
        Self { text, len }
    }

    /// `key_len`.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether `key_len` is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bytes at the key pointer.
    #[must_use]
    pub const fn text(&self) -> &'a [u8] {
        self.text
    }

    /// `key[i]`, reading past the end of `text` as NUL.
    fn byte(&self, i: usize) -> u8 {
        self.text.get(i).copied().unwrap_or(0)
    }

    /// `strlen(key)`.
    fn strlen(&self) -> usize {
        self.text
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.text.len())
    }

    /// `is_key_valid`: alphanumerics, `_` and NUL, over `key_len` bytes.
    fn is_valid(&self) -> bool {
        // The C narrows `key_len` to `uint32_t` here. Every caller has already
        // refused `key_len > MAX_KEY_LEN_BYTES`, so it cannot matter.
        (0..self.len).all(|i| {
            let b = self.byte(i);
            b.is_ascii_alphanumeric() || b == b'_' || b == 0
        })
    }
}

/// One entry of `get_stored_keys`: a `ConfigKey` read out of the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredKey {
    /// `key_buf`: what `snprintf` left there, NUL and trailing bytes included.
    pub key_buf: [u8; KEY_BUF_LEN],
    /// `key_len`, widened. A loaded image can hold any value here.
    pub key_len: u64,
    /// `value_type`, widened. A loaded image can hold any value here.
    pub value_type: u32,
}

/// Why a string or byte-string get returned `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyError {
    /// No such key, the slot does not parse, the value is another type, or the
    /// string is malformed. The C leaves `*value_len` alone; earlier chunks
    /// may already have been copied.
    Refused,
    /// The string is well-formed but longer than the buffer —
    /// `CborErrorOutOfMemory`. The C sets `*value_len` to the full length,
    /// and chunks that fit before the first one that did not were copied.
    TooSmall(usize),
}

/// A CBOR item head as `preparse_value` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    /// Major type, 0–7.
    pub major: u8,
    /// The additional-information bits.
    pub info: u8,
    /// The argument, zero for an indefinite length.
    pub arg: u64,
}

impl Head {
    /// `cbor_parser_init` on `buf`: the first item's head, or `None` where
    /// tinycbor returns an error.
    ///
    /// Only the head is checked; a string's body may run past the end of
    /// `buf` (divergence #49).
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let &descriptor = buf.first()?;
        let major = descriptor >> 5;
        let info = descriptor & 0x1f;
        if info > 27 {
            // 28-30 are reserved; 31 is an indefinite length, which only
            // strings, arrays and maps may have. A top-level break is
            // `CborErrorUnexpectedBreak`.
            return (info == 31 && matches!(major, 2..=5)).then_some(Self {
                major,
                info,
                arg: 0,
            });
        }
        let arg = if info < 24 {
            u64::from(info)
        } else {
            let n = 1usize << (info - 24);
            let bytes = buf.get(1..=n)?;
            bytes.iter().fold(0, |a, &b| (a << 8) | u64::from(b))
        };
        // `f8 xx` with xx < 32 is `CborErrorIllegalSimpleType`.
        if major == 7 && info == 24 && arg < 32 {
            return None;
        }
        Some(Self { major, info, arg })
    }

    /// Whether the length is encoded in the head.
    #[must_use]
    pub fn is_length_known(&self) -> bool {
        self.info != 31
    }

    /// `cbor_type_to_config`: the type `set_config_cbor` stores this under,
    /// or `None` if it refuses the value.
    ///
    /// Only the 5-byte float is a `FLOAT` (divergence #43), and a map, a tag,
    /// a simple value, a half or a double is nothing.
    #[must_use]
    pub fn config_type(&self) -> Option<ValueType> {
        match (self.major, self.info) {
            (0, _) => Some(ValueType::Uint32),
            (1, _) => Some(ValueType::Int32),
            (2, _) => Some(ValueType::Bytes),
            (3, _) => Some(ValueType::Str),
            (7, 26) => Some(ValueType::Float),
            (4, _) => Some(ValueType::Array),
            _ => None,
        }
    }

    /// `cbor_value_get_string_length` / `cbor_value_get_array_length`: the
    /// definite length, if it is one and fits a `size_t`.
    fn length(&self) -> Option<usize> {
        if !self.is_length_known() {
            return None;
        }
        usize::try_from(self.arg).ok()
    }
}

/// `_cbor_value_copy_string` over the string whose head starts `slot`,
/// copying into `out`.
///
/// Chunks are copied as they are read, so a failure part-way leaves the
/// earlier ones in `out`. A NUL is appended when there is room after a
/// complete copy.
///
/// # Errors
///
/// [`CopyError::TooSmall`] with the whole length when the string is
/// well-formed and does not fit; [`CopyError::Refused`] when it is malformed.
pub fn copy_string(slot: &[u8], out: &mut [u8]) -> Result<usize, CopyError> {
    let head = Head::parse(slot).ok_or(CopyError::Refused)?;
    let kind = head.major << 5;
    let chunked = !head.is_length_known();
    // `_cbor_value_begin_string_iteration` steps over a chunked string's head.
    let mut pos = usize::from(chunked);
    let mut total = 0usize;
    let mut fits = true;
    let mut first = true;

    loop {
        // `get_string_chunk_size`.
        if !chunked && !first {
            break;
        }
        first = false;
        let &descriptor = slot.get(pos).ok_or(CopyError::Refused)?;
        if descriptor == 0xff {
            break;
        }
        if descriptor & 0xe0 != kind {
            return Err(CopyError::Refused);
        }
        let info = descriptor & 0x1f;
        let (len, head_len) = if info < 24 {
            (u64::from(info), 1)
        } else if info > 27 {
            return Err(CopyError::Refused);
        } else {
            let n = 1usize << (info - 24);
            let bytes = slot.get(pos + 1..=pos + n).ok_or(CopyError::Refused)?;
            (bytes.iter().fold(0, |a, &b| (a << 8) | u64::from(b)), 1 + n)
        };
        // `CborErrorDataTooLarge` where `size_t` is narrower than the length.
        let len = usize::try_from(len).map_err(|_| CopyError::Refused)?;
        // `transfer_string`.
        let start = pos + head_len;
        let body = slot
            .get(start..)
            .and_then(|rest| rest.get(..len))
            .ok_or(CopyError::Refused)?;
        pos = start + len;

        let new_total = total.checked_add(len).ok_or(CopyError::Refused)?;
        if fits && out.len() >= new_total {
            out[total..new_total].copy_from_slice(body);
        } else {
            fits = false;
        }
        total = new_total;
    }

    if fits && out.len() > total {
        out[total] = 0;
    }
    // `_cbor_value_finish_string_iteration` then parses the next item; at the
    // top level `remaining` reaches zero first, so it cannot fail.
    if fits {
        Ok(total)
    } else {
        Err(CopyError::TooSmall(total))
    }
}

/// One partition as bm_core keeps it in RAM: `CONFIGS[partition]`.
#[derive(Clone)]
pub struct ConfigPartition {
    layout: Layout,
    image: [u8; MAX_IMAGE_LEN],
    needs_commit: bool,
}

impl core::fmt::Debug for ConfigPartition {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConfigPartition")
            .field("layout", &self.layout)
            .field("num_keys", &self.num_keys())
            .field("needs_commit", &self.needs_commit)
            .finish_non_exhaustive()
    }
}

impl ConfigPartition {
    /// A zeroed partition: `CONFIGS` before `config_init` runs.
    #[must_use]
    pub const fn new(layout: Layout) -> Self {
        Self {
            layout,
            image: [0; MAX_IMAGE_LEN],
            needs_commit: false,
        }
    }

    /// The layout this partition was built with.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The `ConfigPartition` struct's bytes, [`Layout::image_len`] long.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.image[..self.layout.image_len()]
    }

    /// `header.crc32`, as last loaded or sealed.
    #[must_use]
    pub fn crc32(&self) -> u32 {
        self.u32_at(0)
    }

    /// `header.version`.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.u32_at(4)
    }

    /// `header.numKeys`.
    #[must_use]
    pub fn num_keys(&self) -> u8 {
        self.image[8]
    }

    /// `needs_commit`: whether anything changed since the last save.
    /// `config_init` does not touch it.
    #[must_use]
    pub fn needs_commit(&self) -> bool {
        self.needs_commit
    }

    /// `load_and_verify_nvm_config`, then `config_init`'s fallback.
    ///
    /// `read` is `bm_config_read`: it fills the image in place, all
    /// [`Layout::image_len`] bytes of it, and reports whether it succeeded.
    /// Whatever it wrote stays, success or not.
    ///
    /// On a failure the header's `numKeys` and `version` are reset and
    /// everything else — including the CRC and every byte of a corrupt image
    /// just read — is kept (divergence #47). An image whose CRC checks but
    /// which claims more than [`MAX_NUM_KV`] keys is refused here; the C
    /// accepts it and reads past its key array (divergence #48).
    ///
    /// Returns whether the image was accepted.
    pub fn load_with(&mut self, read: impl FnOnce(&mut [u8]) -> bool) -> bool {
        let len = self.layout.image_len();
        let ok = read(&mut self.image[..len])
            && crc32_ieee(&self.image[4..len]) == self.crc32()
            && usize::from(self.num_keys()) <= MAX_NUM_KV;
        if !ok {
            self.image[8] = 0;
            self.put_u32(4, CONFIG_VERSION);
        }
        ok
    }

    /// `get_stored_keys`: the first `numKeys` key slots.
    pub fn stored_keys(&self) -> impl Iterator<Item = StoredKey> + '_ {
        (0..usize::from(self.num_keys())).map(|i| self.stored_key(i))
    }

    fn stored_key(&self, i: usize) -> StoredKey {
        let off = self.layout.key_offset(i);
        let mut key_buf = [0; KEY_BUF_LEN];
        key_buf.copy_from_slice(&self.image[off..off + KEY_BUF_LEN]);
        let kw = self.layout.key_len_width;
        let vw = self.layout.value_type_width;
        let key_len = read_le(&self.image[off + KEY_BUF_LEN..][..kw]);
        let value_type = read_le(&self.image[off + KEY_BUF_LEN + kw..][..vw]) as u32;
        StoredKey {
            key_buf,
            key_len,
            value_type,
        }
    }

    /// `get_config_uint`: an unsigned integer, truncated to 32 bits.
    #[must_use]
    pub fn get_uint(&self, key: Key<'_>) -> Option<u32> {
        let head = self.parsed(key)?.1;
        (head.major == 0).then_some(head.arg as u32)
    }

    /// `get_config_int`: either integer major type, as `cbor_value_get_int64`
    /// computes it, truncated to 32 bits.
    ///
    /// For `3b 8000000000000000` the C negates `INT64_MIN`, which is undefined
    /// (divergence #41); this wraps, giving `-1`.
    #[must_use]
    pub fn get_int(&self, key: Key<'_>) -> Option<i32> {
        let head = self.parsed(key)?.1;
        let v = head.arg as i64;
        match head.major {
            0 => Some(v as i32),
            1 => Some(v.wrapping_neg().wrapping_sub(1) as i32),
            _ => None,
        }
    }

    /// `get_config_float`: only the 5-byte encoding, bits intact.
    #[must_use]
    pub fn get_float(&self, key: Key<'_>) -> Option<f32> {
        let head = self.parsed(key)?.1;
        (head.major == 7 && head.info == 26).then(|| f32::from_bits(head.arg as u32))
    }

    /// `get_config_string`, copying into `out` (`*value_len` in is
    /// `out.len()`).
    ///
    /// # Errors
    ///
    /// See [`CopyError`]; `out` may have been written either way.
    pub fn get_string(&self, key: Key<'_>, out: &mut [u8]) -> Result<usize, CopyError> {
        self.get_str_like(key, 3, out)
    }

    /// `get_config_buffer`, the byte-string twin of [`Self::get_string`].
    ///
    /// # Errors
    ///
    /// See [`CopyError`]; `out` may have been written either way.
    pub fn get_buffer(&self, key: Key<'_>, out: &mut [u8]) -> Result<usize, CopyError> {
        self.get_str_like(key, 2, out)
    }

    fn get_str_like(&self, key: Key<'_>, major: u8, out: &mut [u8]) -> Result<usize, CopyError> {
        let (idx, head) = self.parsed(key).ok_or(CopyError::Refused)?;
        if head.major != major {
            return Err(CopyError::Refused);
        }
        copy_string(self.slot(idx), out)
    }

    /// `get_config_cbor`: the whole 50-byte slot, stale tail included, if the
    /// first byte starts an item and `out` holds at least 50 bytes
    /// (divergence #49).
    pub fn get_cbor(&self, key: Key<'_>, out: &mut [u8]) -> Option<usize> {
        let (idx, _) = self.parsed(key)?;
        let dst = out.get_mut(..SLOT)?;
        dst.copy_from_slice(self.slot(idx));
        Some(SLOT)
    }

    /// `get_value_size`: 4 for the scalar types, the definite length of a
    /// string or array, `None` for anything `cbor_type_to_config` refuses or
    /// an indefinite length.
    ///
    /// The length is the head's claim, not what the slot holds.
    #[must_use]
    pub fn value_size(&self, key: Key<'_>) -> Option<usize> {
        let head = self.parsed(key)?.1;
        match head.config_type()? {
            ValueType::Uint32 | ValueType::Int32 | ValueType::Float => Some(4),
            ValueType::Str | ValueType::Bytes | ValueType::Array => head.length(),
        }
    }

    /// `set_config_uint`.
    pub fn set_uint(&mut self, key: Key<'_>, value: u32) -> bool {
        self.set_typed(key, ValueType::Uint32, |slot| {
            push(slot, Header::Positive(u64::from(value)))
        })
    }

    /// `set_config_int`.
    pub fn set_int(&mut self, key: Key<'_>, value: i32) -> bool {
        let value = i64::from(value);
        let header = if value < 0 {
            Header::Negative(!value as u64)
        } else {
            Header::Positive(value as u64)
        };
        self.set_typed(key, ValueType::Int32, |slot| push(slot, header))
    }

    /// `set_config_float`, always the 5-byte form.
    pub fn set_float(&mut self, key: Key<'_>, value: f32) -> bool {
        self.set_typed(key, ValueType::Float, |slot| {
            let mut tail: &mut [u8] = slot;
            crate::cbor::push_f32_wide(&mut Encoder::from(&mut tail), value).is_ok()
        })
    }

    /// `set_config_string`. A string too long for the slot is refused after
    /// its head has been written over the slot (divergence #46).
    pub fn set_string(&mut self, key: Key<'_>, value: &[u8]) -> bool {
        self.set_typed(key, ValueType::Str, |slot| {
            push_string(slot, Header::Text(Some(value.len())), value)
        })
    }

    /// `set_config_buffer`, the byte-string twin of [`Self::set_string`].
    pub fn set_buffer(&mut self, key: Key<'_>, value: &[u8]) -> bool {
        self.set_typed(key, ValueType::Bytes, |slot| {
            push_string(slot, Header::Bytes(Some(value.len())), value)
        })
    }

    /// `set_config_cbor`: store up to 50 bytes of CBOR as they are, typed by
    /// their first item's head.
    ///
    /// The bytes after the head are not checked, and a shorter value leaves
    /// the previous value's tail in the slot. Unlike the typed setters, this
    /// overwrites an existing key when the partition is full (divergence
    /// #46).
    pub fn set_cbor(&mut self, key: Key<'_>, value: &[u8]) -> bool {
        if key.len > MAX_KEY_LEN_BYTES {
            return false;
        }
        if value.len() > SLOT || value.is_empty() {
            return false;
        }
        let Some(head) = Head::parse(value) else {
            return false;
        };
        let num_keys = usize::from(self.num_keys());
        let idx = match self.find(key) {
            Some(idx) => idx,
            None => {
                if !key.is_valid() || num_keys >= MAX_NUM_KV {
                    return false;
                }
                self.write_key_buf(num_keys, key);
                num_keys
            }
        };
        let Some(ty) = head.config_type() else {
            return false;
        };
        self.write_key_meta(idx, key, ty);
        let off = self.layout.value_offset(idx);
        self.image[off..off + value.len()].copy_from_slice(value);
        if idx == num_keys {
            self.image[8] += 1;
        }
        self.needs_commit = true;
        true
    }

    /// `remove_key`. `key_len` is not bounded here, unlike every other
    /// entry point. Later keys and values shift down; the last slot keeps a
    /// copy of what was there.
    pub fn remove_key(&mut self, key: Key<'_>) -> bool {
        let Some(idx) = self.find(key) else {
            return false;
        };
        let num_keys = usize::from(self.num_keys());
        if num_keys - 1 > idx {
            let ks = self.layout.key_size();
            let from = self.layout.key_offset(idx + 1);
            self.image
                .copy_within(from..from + (num_keys - 1 - idx) * ks, from - ks);
            let from = self.layout.value_offset(idx + 1);
            self.image
                .copy_within(from..from + (num_keys - 1 - idx) * SLOT, from - SLOT);
        }
        self.image[8] -= 1;
        self.needs_commit = true;
        true
    }

    /// `clear_partition`: zero everything, CRC included.
    pub fn clear(&mut self) {
        self.image = [0; MAX_IMAGE_LEN];
        self.put_u32(4, CONFIG_VERSION);
        self.needs_commit = true;
    }

    /// The first half of `save_config`: write the CRC into the header and
    /// return the image to hand to `bm_config_write`. The C writes the CRC
    /// whether or not the write then succeeds.
    pub fn seal(&mut self) -> &[u8] {
        let len = self.layout.image_len();
        let crc = crc32_ieee(&self.image[4..len]);
        self.put_u32(0, crc);
        &self.image[..len]
    }

    /// The second half of `save_config`, after a successful write.
    pub fn mark_saved(&mut self) {
        self.needs_commit = false;
    }

    /// `prepare_cbor_parser`: the key's index and its slot's head.
    fn parsed(&self, key: Key<'_>) -> Option<(usize, Head)> {
        if key.len > MAX_KEY_LEN_BYTES {
            return None;
        }
        let idx = self.find(key)?;
        Some((idx, Head::parse(self.slot(idx))?))
    }

    /// `prepare_cbor_encoder` and the typed setters' common tail.
    ///
    /// The full-partition check comes before the lookup, so an existing key
    /// cannot be overwritten once there are 50 (divergence #46). The key is
    /// written before the encode, and survives it failing.
    fn set_typed(
        &mut self,
        key: Key<'_>,
        ty: ValueType,
        encode: impl FnOnce(&mut [u8]) -> bool,
    ) -> bool {
        if key.len > MAX_KEY_LEN_BYTES {
            return false;
        }
        let num_keys = usize::from(self.num_keys());
        if num_keys >= MAX_NUM_KV {
            return false;
        }
        let (idx, exists) = match self.find(key) {
            Some(idx) => (idx, true),
            None if key.is_valid() => (num_keys, false),
            None => return false,
        };
        self.write_key_buf(idx, key);
        let off = self.layout.value_offset(idx);
        if !encode(&mut self.image[off..off + SLOT]) {
            return false;
        }
        self.write_key_meta(idx, key, ty);
        if !exists {
            self.image[8] += 1;
        }
        self.needs_commit = true;
        true
    }

    /// `find_key_idx`: the first of `numKeys` slots whose `key_len` equals
    /// the key's and whose `key_buf` `strncmp`s equal over `key_len` bytes.
    ///
    /// `strncmp` stops at a NUL in either string, so `"ab\0c"` and `"ab\0d"`
    /// are the same key. A loaded image may hold a `key_buf` with no NUL, and
    /// then the comparison reads on into the fields after it, as the C does;
    /// past the image the C's RAM buffer is zero, and so is this.
    fn find(&self, key: Key<'_>) -> Option<usize> {
        (0..usize::from(self.num_keys())).find(|&i| {
            let off = self.layout.key_offset(i);
            let stored = self.stored_key(i);
            if stored.key_len != key.len as u64 {
                return false;
            }
            for j in 0..key.len {
                let a = key.byte(j);
                let b = self.image.get(off + j).copied().unwrap_or(0);
                if a != b {
                    return false;
                }
                if a == 0 {
                    break;
                }
            }
            true
        })
    }

    /// `snprintf(key_buf, 32, "%s", key)`: up to 31 bytes, then a NUL. The
    /// rest of `key_buf` is left as it was.
    fn write_key_buf(&mut self, idx: usize, key: Key<'_>) {
        let off = self.layout.key_offset(idx);
        let n = key.strlen().min(KEY_BUF_LEN - 1);
        self.image[off..off + n].copy_from_slice(&key.text[..n]);
        self.image[off + n] = 0;
    }

    fn write_key_meta(&mut self, idx: usize, key: Key<'_>, ty: ValueType) {
        let off = self.layout.key_offset(idx) + KEY_BUF_LEN;
        let kw = self.layout.key_len_width;
        let vw = self.layout.value_type_width;
        write_le(&mut self.image[off + kw..off + kw + vw], ty as u64);
        write_le(&mut self.image[off..off + kw], key.len as u64);
    }

    fn slot(&self, idx: usize) -> &[u8] {
        let off = self.layout.value_offset(idx);
        &self.image[off..off + SLOT]
    }

    fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.image[off],
            self.image[off + 1],
            self.image[off + 2],
            self.image[off + 3],
        ])
    }

    fn put_u32(&mut self, off: usize, v: u32) {
        self.image[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// The three partitions: `CONFIGS`.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    partitions: [ConfigPartition; 3],
}

impl ConfigStore {
    /// Three zeroed partitions.
    #[must_use]
    pub const fn new(layout: Layout) -> Self {
        Self {
            partitions: [
                ConfigPartition::new(layout),
                ConfigPartition::new(layout),
                ConfigPartition::new(layout),
            ],
        }
    }

    /// One partition.
    #[must_use]
    pub fn partition(&self, p: Partition) -> &ConfigPartition {
        &self.partitions[p as usize]
    }

    /// One partition, mutably.
    pub fn partition_mut(&mut self, p: Partition) -> &mut ConfigPartition {
        &mut self.partitions[p as usize]
    }
}

/// Encode one head into `slot`. Every integer head fits a 50-byte slot.
fn push(slot: &mut [u8], header: Header) -> bool {
    let mut tail: &mut [u8] = slot;
    Encoder::from(&mut tail).push(header).is_ok()
}

/// tinycbor's `encode_string`: the head is written, then the body only if it
/// fits.
fn push_string(slot: &mut [u8], header: Header, body: &[u8]) -> bool {
    let mut head = [0u8; 9];
    let head_len = {
        let mut tail: &mut [u8] = &mut head;
        if Encoder::from(&mut tail).push(header).is_err() {
            return false;
        }
        9 - tail.len()
    };
    slot[..head_len].copy_from_slice(&head[..head_len]);
    match slot.get_mut(head_len..head_len + body.len()) {
        Some(dst) => {
            dst.copy_from_slice(body);
            true
        }
        None => false,
    }
}

fn read_le(bytes: &[u8]) -> u64 {
    bytes.iter().rev().fold(0, |a, &b| (a << 8) | u64::from(b))
}

fn write_le(dst: &mut [u8], v: u64) {
    for (i, b) in dst.iter_mut().enumerate() {
        *b = (v >> (8 * i)) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SILLY: &[u8] = b"The quick brown fox jumps over the lazy dog";
    const BYTES: &[u8] = &[0xde, 0xad, 0xbe, 0xef, 0x5a, 0xad, 0xda, 0xad, 0xb0, 0xdd];
    /// The gtest's `float baz = 3.14159;`, as the bits that literal rounds to.
    const BAZ: f32 = f32::from_bits(0x4049_0fd0);

    fn fresh() -> ConfigPartition {
        let mut p = ConfigPartition::new(Layout::LP64);
        assert!(!p.load_with(|_| false));
        p
    }

    /// `get_stored_keys` names exactly `names`, in order.
    fn assert_keys(p: &ConfigPartition, names: &[&[u8]]) {
        assert_eq!(p.stored_keys().count(), names.len());
        for (k, name) in p.stored_keys().zip(names) {
            assert_eq!(&k.key_buf[..name.len()], *name);
            assert_eq!(k.key_buf[name.len()], 0);
        }
    }

    #[test]
    fn layout_sizes() {
        assert_eq!(Layout::LP64.key_size(), 44);
        assert_eq!(Layout::LP64.image_len(), 4709);
        assert_eq!(Layout::ARM_EABI_GCC.key_size(), 37);
        assert_eq!(Layout::ARM_EABI_GCC.image_len(), 4359);
        assert_eq!(Layout::new(4, 4).unwrap().image_len(), 4509);
        assert_eq!(Layout::new(3, 4), None);
    }

    /// The oracle only has LP64, so the other layouts are checked for
    /// self-consistency: 50 keys fill without overlapping, and a removal
    /// shifts keys and values together.
    #[test]
    fn narrow_layouts_hold_fifty_keys() {
        for layout in [Layout::ARM_EABI_GCC, Layout::new(4, 4).unwrap()] {
            let mut p = ConfigPartition::new(layout);
            let name = |i: u32| [b'k', b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
            for i in 0..50 {
                assert!(p.set_int(Key::new(&name(i)), -(i as i32)));
            }
            assert!(!p.set_int(Key::new(b"k50"), 0));
            assert!(p.remove_key(Key::new(&name(7))));
            assert_eq!(p.num_keys(), 49);
            for i in (0..50).filter(|&i| i != 7) {
                assert_eq!(p.get_int(Key::new(&name(i))), Some(-(i as i32)));
            }
            let last = p.stored_keys().last().unwrap();
            assert_eq!(&last.key_buf[..4], b"k49\0");
            assert_eq!(
                (last.key_len, last.value_type),
                (3, ValueType::Int32 as u32)
            );
            assert_eq!(p.seal().len(), layout.image_len());
        }
    }

    /// `configuration_test.cpp`, `BasicTest`.
    #[test]
    fn basic_test() {
        let mut p = fresh();
        assert_eq!(p.num_keys(), 0);

        assert!(p.set_uint(Key::new(b"foo"), 42));
        assert_eq!(p.get_uint(Key::new(b"foo")), Some(42));
        assert!(p.set_uint(Key::new(b"foo"), 999));
        assert_eq!(p.get_uint(Key::new(b"foo")), Some(999));
        assert_eq!(p.num_keys(), 1);

        assert!(p.set_int(Key::new(b"bar"), -1000));
        assert_eq!(p.get_int(Key::new(b"bar")), Some(-1000));
        assert!(p.set_float(Key::new(b"baz"), BAZ));
        assert_eq!(p.get_float(Key::new(b"baz")), Some(BAZ));

        assert!(p.set_string(Key::new(b"silly"), SILLY));
        let mut out = [0u8; 100];
        assert_eq!(p.get_string(Key::new(b"silly"), &mut out), Ok(SILLY.len()));
        assert_eq!(&out[..SILLY.len()], SILLY);

        assert!(p.set_buffer(Key::new(b"bytes"), BYTES));
        let mut out = [0u8; 10];
        assert_eq!(p.get_buffer(Key::new(b"bytes"), &mut out), Ok(10));
        assert_eq!(out, BYTES);
        assert_keys(&p, &[b"foo", b"bar", b"baz", b"silly", b"bytes"]);

        assert!(p.remove_key(Key::new(b"foo")));
        assert_eq!(p.get_uint(Key::new(b"foo")), None);
        assert_keys(&p, &[b"bar", b"baz", b"silly", b"bytes"]);
        assert_eq!(p.get_int(Key::new(b"bar")), Some(-1000));
        assert_eq!(p.get_float(Key::new(b"baz")), Some(BAZ));
        let mut out = [0u8; 10];
        assert_eq!(p.get_buffer(Key::new(b"bytes"), &mut out), Ok(10));
        assert_eq!(out, BYTES);
    }

    /// `NoKeyFound`, `NoKeyToRemove`, `TooMuchThingy`.
    #[test]
    fn refusals() {
        let mut p = fresh();
        assert_eq!(p.get_uint(Key::new(b"foo")), None);
        assert!(p.set_uint(Key::new(b"foo"), 42));
        assert_eq!(p.get_uint(Key::new(b"baz")), None);
        assert!(!p.remove_key(Key::new(b"nope")));

        let mut long = SILLY.to_vec();
        long.extend_from_slice(b" ");
        long.extend_from_slice(SILLY);
        assert!(!p.set_string(Key::new(b"silly"), &long));
        assert!(!p.set_buffer(Key::new(b"bytes"), &[0xa5; 100]));
    }

    /// `cborGetSet`.
    #[test]
    fn cbor_get_set() {
        let mut p = fresh();
        assert!(p.set_uint(Key::new(b"foo"), 42));
        let mut buf = [0u8; 50];
        assert_eq!(p.get_cbor(Key::new(b"foo"), &mut buf), Some(50));
        assert!(p.set_cbor(Key::new(b"bar"), &buf));
        assert_eq!(p.num_keys(), 2);
        assert_eq!(p.get_uint(Key::new(b"bar")), Some(42));
        assert_eq!(p.value_size(Key::new(b"bar")), Some(4));

        assert!(p.set_string(Key::new(b"silly"), SILLY));
        assert_eq!(p.get_cbor(Key::new(b"silly"), &mut buf), Some(50));
        assert!(p.set_cbor(Key::new(b"bar"), &buf));
        assert_eq!(p.num_keys(), 3);
        let mut out = [0u8; 100];
        assert_eq!(p.get_string(Key::new(b"bar"), &mut out), Ok(SILLY.len()));
        assert_eq!(p.value_size(Key::new(b"bar")), Some(SILLY.len()));
    }

    /// `BadCborGetSet` and `InvalidKeyCharacters`.
    #[test]
    fn bad_cbor_and_bad_keys() {
        let mut p = fresh();
        let mut buf = [0xffu8; 50];
        assert_eq!(p.get_cbor(Key::new(b"foo"), &mut buf), None);
        assert!(!p.set_cbor(Key::new(b"foo"), &buf));
        assert!(p.set_uint(Key::new(b"foo"), 42));
        assert_eq!(p.get_cbor(Key::new(b"foo"), &mut buf), Some(50));
        assert!(!p.set_cbor(
            Key::new(b"a_super_long_key_string_that_should_not_work"),
            &buf
        ));
        assert_eq!(p.num_keys(), 1);

        assert!(!p.set_cbor(Key::new(b"this will not work"), &buf));
        assert!(!p.set_int(Key::new(b"this-will-not-work"), -1));
        assert!(!p.set_uint(Key::new(b"this!will!not!work"), 1));
        assert!(!p.set_float(Key::new(b"this*will(not)work"), 100.0));
        assert!(!p.set_string(Key::new(b"this_will_not@work"), SILLY));
        assert!(!p.set_buffer(Key::new(b"thiszwillznot^work"), &[0; 25]));
        assert!(p.set_buffer(Key::new(b"this_will_work"), &[0; 25]));
    }

    /// The header and slot layout and the CRC, as the oracle wrote them for
    /// the same two sets. bm_core has no gold image to lift.
    #[test]
    fn the_image_the_oracle_saves() {
        let mut p = fresh();
        assert_eq!(p.seal()[..9], [0x1c, 0xca, 0x40, 0x76, 0, 0, 0, 0, 0]);

        assert!(p.set_uint(Key::new(b"foo"), 42));
        assert!(p.set_string(Key::new(b"silly"), b"hello"));
        let image = p.seal();
        assert_eq!(image.len(), 4709);
        assert_eq!(image[..9], [0x14, 0x34, 0x20, 0x3c, 0, 0, 0, 0, 2]);

        let mut key0 = [0u8; 44];
        key0[..3].copy_from_slice(b"foo");
        key0[32] = 3;
        assert_eq!(image[9..53], key0);
        let mut key1 = [0u8; 44];
        key1[..5].copy_from_slice(b"silly");
        key1[32] = 5;
        key1[40] = ValueType::Str as u8;
        assert_eq!(image[53..97], key1);

        let values = Layout::LP64.value_offset(0);
        assert_eq!(image[values..values + 3], [0x18, 0x2a, 0]);
        assert_eq!(
            image[values + 50..values + 57],
            [0x65, b'h', b'e', b'l', b'l', b'o', 0]
        );
    }

    /// Divergence #48: the C would walk 51 key slots; the port refuses.
    #[test]
    fn an_image_claiming_more_than_50_keys_is_refused() {
        let mut p = fresh();
        assert!(p.set_uint(Key::new(b"foo"), 1));
        let mut image = p.seal().to_vec();
        image[8] = 51;
        let crc = crc32_ieee(&image[4..]);
        image[..4].copy_from_slice(&crc.to_le_bytes());

        let mut q = ConfigPartition::new(Layout::LP64);
        assert!(!q.load_with(|buf| {
            buf.copy_from_slice(&image);
            true
        }));
        assert_eq!(q.num_keys(), 0);
    }

    /// Divergence #45: stored as 31 bytes with `key_len` 32, never found.
    #[test]
    fn a_32_byte_key_cannot_be_read_back() {
        let mut p = fresh();
        let key = Key::new(b"abcdefghijklmnopqrstuvwxyz012345");
        assert!(p.set_uint(key, 1));
        assert!(p.set_uint(key, 2));
        assert_eq!(p.num_keys(), 2);
        assert_eq!(p.get_uint(key), None);
    }

    /// Divergence #46: the head of a refused string replaces the old value's.
    #[test]
    fn a_refused_string_corrupts_the_old_value() {
        let mut p = fresh();
        assert!(p.set_string(Key::new(b"s"), b"hello"));
        assert!(!p.set_string(Key::new(b"s"), &[b'x'; 60]));
        let mut out = [0u8; 64];
        assert_eq!(
            p.get_string(Key::new(b"s"), &mut out),
            Err(CopyError::Refused)
        );
        assert_eq!(p.value_size(Key::new(b"s")), Some(60));
    }
}
