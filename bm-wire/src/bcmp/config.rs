//! The config messages `0xA0`–`0xA9`, ported from `bcmp/messages.h` and
//! `bcmp/config.c`.
//!
//! Codecs, plus the two pieces of `config.c` that are pure functions:
//! [`decode_value`] (`bcmp_config_decode_value`) and
//! [`encode_status_response`] (the body `bcmp_config_status_response`
//! builds). What a node does with a message — the store edits, the replies,
//! the forwarding — is `bm-stack`'s.
//!
//! # Layouts
//!
//! Every message starts with [`ConfigHeader`], `BmConfigHeader`: 16 bytes,
//! target then source. Three body shapes are shared:
//!
//! | Type | Rust | Body after the header |
//! |---|---|---|
//! | `0xA0` get, `0xA6` delete request | [`ConfigKeyRequest`] | partition, key length, key |
//! | `0xA1` value | [`ConfigValue`] | partition, `u32` data length, data |
//! | `0xA2` set | [`ConfigSet`] | partition, key length, `u32` data length, key, data |
//! | `0xA3` commit, `0xA4` status request, `0xA8` clear request | [`ConfigPartitionRequest`] | partition |
//! | `0xA5` status response | [`ConfigStatusResponse`] | partition, committed, key count, keys |
//! | `0xA7` delete response | [`ConfigDeleteResponse`] | success, partition, key length, key |
//! | `0xA9` clear response | [`ConfigClearResponse`] | success, partition |
//!
//! # Lengths are checked here and not in the C
//!
//! `bcmp_process_config_message` casts the body and reads every length field
//! without consulting `data.size` (divergence #51). The decoders here refuse a
//! body shorter than its own length fields claim. Trailing bytes are ignored,
//! as the C ignores them.
//!
//! # Partitions are a raw byte
//!
//! The partition is decoded as the byte that arrived. `config.c` indexes
//! `CONFIGS` with it unchecked everywhere except `clear_partition`
//! (divergence #50); [`crate::configuration::Partition::from_u8`] is the check.

use crate::BmWireError;
use crate::configuration::{
    ConfigPartition, CopyError, Head, MAX_CONFIG_BUFFER_SIZE_BYTES, MAX_KEY_LEN_BYTES, ValueType,
    copy_string,
};
use crate::le;

/// Longest key `bcmp_config_get` and `bcmp_config_set` will send,
/// `MAX_KEY_LEN_BYTES`. `bcmp_config_del_key` does not check.
pub const MAX_KEY_LEN: usize = MAX_KEY_LEN_BYTES;

/// Longest value a `ConfigSet` is honoured with, `MAX_CONFIG_BUFFER_SIZE_BYTES`.
/// Also the length of every `ConfigValue` sent in answer to a `ConfigGet`,
/// which carries the whole slot (divergence #49).
pub const MAX_VALUE_LEN: usize = MAX_CONFIG_BUFFER_SIZE_BYTES;

/// The ceiling `bcmp_config_status_response` applies to its body,
/// `bcmp_max_payload_size_bytes`.
///
/// Not the ceiling that binds: `bcmp_tx` refuses anything over 1448 bytes
/// first (divergence #8), so a body between 1449 and 1500 bytes passes this
/// test and is then not sent.
pub const STATUS_RESPONSE_MAX_LEN: usize = 1500;

/// `buf[at..at + len]`, or [`BmWireError::Truncated`].
fn slice(buf: &[u8], at: usize, len: usize) -> Result<&[u8], BmWireError> {
    buf.get(at..)
        .and_then(|rest| rest.get(..len))
        .ok_or(BmWireError::Truncated)
}

/// `buf[..len]`, mutably, or [`BmWireError::Truncated`].
fn prefix_mut(buf: &mut [u8], len: usize) -> Result<&mut [u8], BmWireError> {
    buf.get_mut(..len).ok_or(BmWireError::Truncated)
}

/// A length that has to fit a one-byte field.
fn len_u8(len: usize) -> Result<u8, BmWireError> {
    u8::try_from(len).map_err(|_| BmWireError::Invalid)
}

/// A length that has to fit a four-byte field.
fn len_u32(len: usize) -> Result<u32, BmWireError> {
    u32::try_from(len).map_err(|_| BmWireError::Invalid)
}

/// `BmConfigHeader`: who a config message is for, and who sent it.
///
/// Replies go to `source_node_id`, not to the frame's source address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigHeader {
    /// Node the message is addressed to. **Zero is not a broadcast**:
    /// `bcmp_process_config_message` acts only on an exact match and forwards
    /// everything else, zero included.
    pub target_node_id: u64,
    /// Node that sent it.
    pub source_node_id: u64,
}

impl ConfigHeader {
    /// Wire size.
    pub const LEN: usize = 16;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let buf = slice(buf, 0, Self::LEN)?;
        Ok(Self {
            target_node_id: le::u64_at(buf, 0),
            source_node_id: le::u64_at(buf, 8),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = prefix_mut(buf, Self::LEN)?;
        buf[0..8].copy_from_slice(&self.target_node_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.source_node_id.to_le_bytes());
        Ok(())
    }

    /// Whether a node with `our_node_id` acts on the message rather than
    /// forwarding it: `msg_header->target_node_id != node_id()`, negated.
    #[must_use]
    pub fn is_for(&self, our_node_id: u64) -> bool {
        self.target_node_id == our_node_id
    }
}

/// `BmConfigGet` (`0xA0`) and `BmConfigDeleteKeyRequest` (`0xA6`), which have
/// the same layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigKeyRequest<'a> {
    /// Who it is for, and who is asking.
    pub header: ConfigHeader,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
    /// The key, `key_length` bytes, not NUL-terminated.
    pub key: &'a [u8],
}

/// `BmConfigGet` (`0xA0`).
pub type ConfigGet<'a> = ConfigKeyRequest<'a>;
/// `BmConfigDeleteKeyRequest` (`0xA6`).
pub type ConfigDeleteRequest<'a> = ConfigKeyRequest<'a>;

impl<'a> ConfigKeyRequest<'a> {
    /// Bytes before the key.
    pub const HEAD_LEN: usize = ConfigHeader::LEN + 2;

    /// Wire size.
    #[must_use]
    pub fn len(&self) -> usize {
        Self::HEAD_LEN + self.key.len()
    }

    /// Always false: the header alone is 18 bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Decode from the front of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` does not hold the declared key.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let head = slice(buf, 0, Self::HEAD_LEN)?;
        let key = slice(buf, Self::HEAD_LEN, usize::from(head[17]))?;
        Ok(Self {
            header,
            partition: head[16],
            key,
        })
    }

    /// Encode into the front of `buf`, returning [`Self::len`].
    ///
    /// # Errors
    ///
    /// [`BmWireError::Invalid`] for a key longer than 255 bytes, which the
    /// `key_length` byte cannot hold; [`BmWireError::Truncated`] if `buf` is
    /// too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let key_length = len_u8(self.key.len())?;
        let buf = prefix_mut(buf, self.len())?;
        self.header.encode(buf)?;
        buf[16] = self.partition;
        buf[17] = key_length;
        buf[Self::HEAD_LEN..].copy_from_slice(self.key);
        Ok(self.len())
    }
}

/// `BmConfigValue` (`0xA1`): a CBOR value, in answer to a get or a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigValue<'a> {
    /// Who it is for, and who answered.
    pub header: ConfigHeader,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
    /// `data_length` bytes of CBOR. In answer to a get this is the whole
    /// 50-byte slot, stale tail included; in answer to a set it is the value
    /// the set carried.
    pub data: &'a [u8],
}

impl<'a> ConfigValue<'a> {
    /// Bytes before the data.
    pub const HEAD_LEN: usize = ConfigHeader::LEN + 5;

    /// Wire size.
    #[must_use]
    pub fn len(&self) -> usize {
        Self::HEAD_LEN + self.data.len()
    }

    /// Always false: the header alone is 21 bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Decode from the front of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` does not hold the declared data.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let head = slice(buf, 0, Self::HEAD_LEN)?;
        let data_length =
            usize::try_from(le::u32_at(head, 17)).map_err(|_| BmWireError::Truncated)?;
        let data = slice(buf, Self::HEAD_LEN, data_length)?;
        Ok(Self {
            header,
            partition: head[16],
            data,
        })
    }

    /// Encode into the front of `buf`, returning [`Self::len`].
    ///
    /// # Errors
    ///
    /// [`BmWireError::Invalid`] if the data does not fit a `u32` length;
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let data_length = len_u32(self.data.len())?;
        let buf = prefix_mut(buf, self.len())?;
        self.header.encode(buf)?;
        buf[16] = self.partition;
        buf[17..21].copy_from_slice(&data_length.to_le_bytes());
        buf[Self::HEAD_LEN..].copy_from_slice(self.data);
        Ok(self.len())
    }
}

/// `BmConfigSet` (`0xA2`): store a CBOR value under a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigSet<'a> {
    /// Who it is for, and who is asking.
    pub header: ConfigHeader,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
    /// The key, `key_length` bytes, with the data straight after it.
    pub key: &'a [u8],
    /// `data_length` bytes of CBOR.
    pub data: &'a [u8],
}

impl<'a> ConfigSet<'a> {
    /// Bytes before the key.
    pub const HEAD_LEN: usize = ConfigHeader::LEN + 6;

    /// Wire size.
    #[must_use]
    pub fn len(&self) -> usize {
        Self::HEAD_LEN + self.key.len() + self.data.len()
    }

    /// Always false: the header alone is 22 bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Decode from the front of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` does not hold the declared key and
    /// data.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let head = slice(buf, 0, Self::HEAD_LEN)?;
        let key_length = usize::from(head[17]);
        let data_length =
            usize::try_from(le::u32_at(head, 18)).map_err(|_| BmWireError::Truncated)?;
        let key = slice(buf, Self::HEAD_LEN, key_length)?;
        let data = slice(buf, Self::HEAD_LEN + key_length, data_length)?;
        Ok(Self {
            header,
            partition: head[16],
            key,
            data,
        })
    }

    /// Encode into the front of `buf`, returning [`Self::len`].
    ///
    /// # Errors
    ///
    /// [`BmWireError::Invalid`] if a length does not fit its field;
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let key_length = len_u8(self.key.len())?;
        let data_length = len_u32(self.data.len())?;
        let buf = prefix_mut(buf, self.len())?;
        self.header.encode(buf)?;
        buf[16] = self.partition;
        buf[17] = key_length;
        buf[18..22].copy_from_slice(&data_length.to_le_bytes());
        let (key, data) = buf[Self::HEAD_LEN..].split_at_mut(self.key.len());
        key.copy_from_slice(self.key);
        data.copy_from_slice(self.data);
        Ok(self.len())
    }
}

/// `BmConfigCommit` (`0xA3`), `BmConfigStatusRequest` (`0xA4`) and
/// `BmConfigClearRequest` (`0xA8`): a header and a partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigPartitionRequest {
    /// Who it is for, and who is asking.
    pub header: ConfigHeader,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
}

/// `BmConfigCommit` (`0xA3`).
pub type ConfigCommit = ConfigPartitionRequest;
/// `BmConfigStatusRequest` (`0xA4`).
pub type ConfigStatusRequest = ConfigPartitionRequest;
/// `BmConfigClearRequest` (`0xA8`).
pub type ConfigClearRequest = ConfigPartitionRequest;

impl ConfigPartitionRequest {
    /// Wire size.
    pub const LEN: usize = ConfigHeader::LEN + 1;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let buf = slice(buf, 0, Self::LEN)?;
        Ok(Self {
            header,
            partition: buf[16],
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let buf = prefix_mut(buf, Self::LEN)?;
        self.header.encode(buf)?;
        buf[16] = self.partition;
        Ok(Self::LEN)
    }
}

/// `BmConfigStatusResponse` (`0xA5`): the keys a partition holds, and whether
/// it has changes not yet saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigStatusResponse<'a> {
    /// Who it is for, and who answered.
    pub header: ConfigHeader,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
    /// The field is named `committed` and carries `needs_commit`: true means
    /// there **are** unsaved changes, as the struct's comment says.
    pub committed: bool,
    /// `num_keys`.
    pub num_keys: u8,
    /// Everything after `num_keys`: [`Self::keys`] walks it.
    pub key_data: &'a [u8],
}

impl<'a> ConfigStatusResponse<'a> {
    /// Bytes before the keys.
    pub const HEAD_LEN: usize = ConfigHeader::LEN + 3;

    /// Decode the fixed part from the front of `buf`; the rest is
    /// [`Self::key_data`].
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::HEAD_LEN`].
    /// A key list shorter than `num_keys` says is not an error here; see
    /// [`Self::keys`].
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let head = slice(buf, 0, Self::HEAD_LEN)?;
        Ok(Self {
            header,
            partition: head[16],
            committed: head[17] != 0,
            num_keys: head[18],
            key_data: &buf[Self::HEAD_LEN..],
        })
    }

    /// The `num_keys` keys, each a length byte and that many bytes.
    ///
    /// The receive loop at `config.c:745` advances a typed
    /// `BmConfigStatusKeyData *` by `key_length + sizeof(BmConfigStatusKeyData)`.
    /// That looks like a struct-scaled advance walking off the end, but
    /// `sizeof(BmConfigStatusKeyData)` is 1 (a `uint8_t` and a flexible array),
    /// so the advance is exactly `key_length + 1` bytes — one length byte and
    /// the key — and is correct. What the loop does not do is check the entries
    /// against the body length (the #51 class); here an entry that runs past
    /// the body ends the iteration with [`BmWireError::Truncated`].
    pub fn keys(&self) -> StatusKeys<'a> {
        StatusKeys {
            rest: self.key_data,
            remaining: self.num_keys,
        }
    }
}

/// The keys of a [`ConfigStatusResponse`].
#[derive(Debug, Clone)]
pub struct StatusKeys<'a> {
    rest: &'a [u8],
    remaining: u8,
}

impl<'a> Iterator for StatusKeys<'a> {
    type Item = Result<&'a [u8], BmWireError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let entry = self
            .rest
            .split_first()
            .and_then(|(&len, rest)| Some((rest.get(..usize::from(len))?, rest)));
        match entry {
            Some((key, rest)) => {
                self.rest = &rest[key.len()..];
                Some(Ok(key))
            }
            None => {
                self.remaining = 0;
                Some(Err(BmWireError::Truncated))
            }
        }
    }
}

/// The length [`encode_status_response`] would write for `store`, or `None`
/// where `bcmp_config_status_response` sends nothing.
///
/// `msg_size` is `sizeof(BmConfigStatusResponse)` plus one byte and
/// `key_len` bytes per key, where `key_len` is the stored `size_t` rather than
/// the byte it is written into. Over [`STATUS_RESPONSE_MAX_LEN`] the C returns
/// before building anything. An image whose stored lengths overflow the sum
/// has no defined outcome in the C and gets `None`.
#[must_use]
pub fn status_response_len(store: &ConfigPartition) -> Option<usize> {
    store
        .stored_keys()
        .try_fold(ConfigStatusResponse::HEAD_LEN as u64, |size, key| {
            size.checked_add(1)?.checked_add(key.key_len)
        })
        .filter(|&size| size <= STATUS_RESPONSE_MAX_LEN as u64)
        .map(|size| size as usize)
}

/// `bcmp_config_status_response`'s body for `store`, into the front of `buf`.
/// Returns the length, [`status_response_len`].
///
/// Each key is written as `(uint8_t)key_len` followed by `key_len` bytes
/// copied from the key's slot in the image. For a key the store wrote itself
/// `key_len` is at most 32 and the bytes are `key_buf`, NUL and trailing bytes
/// included (divergence #45). A loaded image can claim more, and then the
/// length byte wraps while the copy runs on into the fields after `key_buf`,
/// as the C's `memcpy` does.
///
/// `partition` is the byte to write, which is the request's.
///
/// # Errors
///
/// [`BmWireError::Invalid`] where [`status_response_len`] is `None`;
/// [`BmWireError::Truncated`] if `buf` is too short.
pub fn encode_status_response(
    buf: &mut [u8],
    header: ConfigHeader,
    partition: u8,
    store: &ConfigPartition,
) -> Result<usize, BmWireError> {
    let len = status_response_len(store).ok_or(BmWireError::Invalid)?;
    let buf = prefix_mut(buf, len)?;
    header.encode(buf)?;
    buf[16] = partition;
    buf[17] = u8::from(store.needs_commit());
    buf[18] = store.num_keys();
    let image = store.image();
    let layout = store.layout();
    let mut at = ConfigStatusResponse::HEAD_LEN;
    for (i, key) in store.stored_keys().enumerate() {
        // `status_response_len` bounded the sum, so every length fits.
        let key_len = key.key_len as usize;
        buf[at] = key.key_len as u8;
        at += 1;
        let from = layout.key_offset(i);
        for (j, out) in buf[at..at + key_len].iter_mut().enumerate() {
            *out = image.get(from + j).copied().unwrap_or(0);
        }
        at += key_len;
    }
    Ok(len)
}

/// `BmConfigDeleteKeyResponse` (`0xA7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigDeleteResponse<'a> {
    /// Who it is for, and who answered.
    pub header: ConfigHeader,
    /// Whether `remove_key` found the key.
    pub success: bool,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
    /// The key as the request carried it.
    pub key: &'a [u8],
}

impl<'a> ConfigDeleteResponse<'a> {
    /// Bytes before the key.
    pub const HEAD_LEN: usize = ConfigHeader::LEN + 3;

    /// Wire size.
    #[must_use]
    pub fn len(&self) -> usize {
        Self::HEAD_LEN + self.key.len()
    }

    /// Always false: the header alone is 19 bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Decode from the front of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` does not hold the declared key.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let head = slice(buf, 0, Self::HEAD_LEN)?;
        let key = slice(buf, Self::HEAD_LEN, usize::from(head[18]))?;
        Ok(Self {
            header,
            success: head[16] != 0,
            partition: head[17],
            key,
        })
    }

    /// Encode into the front of `buf`, returning [`Self::len`].
    ///
    /// # Errors
    ///
    /// [`BmWireError::Invalid`] for a key longer than 255 bytes;
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let key_length = len_u8(self.key.len())?;
        let buf = prefix_mut(buf, self.len())?;
        self.header.encode(buf)?;
        buf[16] = u8::from(self.success);
        buf[17] = self.partition;
        buf[18] = key_length;
        buf[Self::HEAD_LEN..].copy_from_slice(self.key);
        Ok(self.len())
    }
}

/// `BmConfigClearResponse` (`0xA9`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigClearResponse {
    /// Who it is for, and who answered.
    pub header: ConfigHeader,
    /// Whether `clear_partition` accepted the partition number.
    pub success: bool,
    /// `BmConfigPartition`, as the byte that arrived.
    pub partition: u8,
}

impl ConfigClearResponse {
    /// Wire size.
    pub const LEN: usize = ConfigHeader::LEN + 2;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let header = ConfigHeader::decode(buf)?;
        let buf = slice(buf, 0, Self::LEN)?;
        Ok(Self {
            header,
            success: buf[16] != 0,
            partition: buf[17],
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let buf = prefix_mut(buf, Self::LEN)?;
        self.header.encode(buf)?;
        buf[16] = u8::from(self.success);
        buf[17] = self.partition;
        Ok(Self::LEN)
    }
}

/// Why [`decode_value`] failed, by the `BmErr` the C returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// `BmEINVAL`: `data` or `buf` is empty.
    Invalid,
    /// `BmEBADMSG`: the value does not parse, is a type
    /// `cbor_type_to_config` refuses, or does not fit `buf`.
    BadMessage,
    /// `BmEBADMSG` from a string or byte string longer than `buf`. tinycbor
    /// has already set `*buf_length` to the full length, which this carries.
    TooSmall(usize),
    /// `BmEIO`: the value is a different [`ValueType`] from the one asked for.
    WrongType,
}

/// `bcmp_config_decode_value`: decode `data`, a `ConfigValue`'s CBOR, as a
/// value of type `ty` into `buf`.
///
/// Returns `*buf_length` as the C leaves it: the length for a string or byte
/// string, and `buf.len()` unchanged for the others. What is written:
///
/// | `ty` | Written | Needs |
/// |---|---|---|
/// | `Uint32` | the value truncated to 32 bits, little-endian | 4 bytes |
/// | `Int32` | the same, of the signed value | 4 bytes |
/// | `Float` | the four bytes of an `fa` float | 4 bytes |
/// | `Str` | the text, then a NUL | the text, see below |
/// | `Bytes` | the bytes, and a NUL if there is room | the bytes |
/// | `Array` | nothing | — |
///
/// A text string exactly as long as `buf` is accepted, and the C then writes
/// its NUL at `buf[buf.len()]`, one past the caller's buffer (divergence #52).
/// Nothing is written there here.
///
/// For `Int32` the C negates `INT64_MIN` on `3b 8000000000000000`
/// (divergence #41); this wraps.
///
/// # Errors
///
/// See [`DecodeError`]. On failure `buf` may already hold the leading chunks
/// of a chunked string, as in the C.
pub fn decode_value(ty: ValueType, data: &[u8], buf: &mut [u8]) -> Result<usize, DecodeError> {
    if data.is_empty() || buf.is_empty() {
        return Err(DecodeError::Invalid);
    }
    let head = Head::parse(data).ok_or(DecodeError::BadMessage)?;
    let found = head.config_type().ok_or(DecodeError::BadMessage)?;
    if found != ty {
        return Err(DecodeError::WrongType);
    }
    let scalar = |buf: &mut [u8], bits: u32| {
        let out = buf.get_mut(..4).ok_or(DecodeError::BadMessage)?;
        out.copy_from_slice(&bits.to_le_bytes());
        Ok(buf.len())
    };
    match ty {
        ValueType::Uint32 | ValueType::Float => scalar(buf, head.arg as u32),
        ValueType::Int32 => {
            let v = head.arg as i64;
            scalar(buf, v.wrapping_neg().wrapping_sub(1) as u32)
        }
        ValueType::Str | ValueType::Bytes => match copy_string(data, buf) {
            Ok(len) => Ok(len),
            Err(CopyError::TooSmall(len)) => Err(DecodeError::TooSmall(len)),
            Err(CopyError::Refused) => Err(DecodeError::BadMessage),
        },
        ValueType::Array => Ok(buf.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::{Key, Layout};

    const HEADER: ConfigHeader = ConfigHeader {
        target_node_id: 0x0102_0304_0506_0708,
        source_node_id: 0x1112_1314_1516_1718,
    };

    #[test]
    fn a_get_is_the_header_a_partition_a_length_and_the_key() {
        let get = ConfigGet {
            header: HEADER,
            partition: 1,
            key: b"foo",
        };
        let mut buf = [0u8; 32];
        assert_eq!(get.encode(&mut buf), Ok(21));
        assert_eq!(&buf[..8], &HEADER.target_node_id.to_le_bytes());
        assert_eq!(&buf[16..21], &[1, 3, b'f', b'o', b'o']);
        assert_eq!(ConfigGet::decode(&buf[..21]), Ok(get));
        assert_eq!(ConfigGet::decode(&buf[..20]), Err(BmWireError::Truncated));
    }

    #[test]
    fn a_set_puts_both_lengths_before_both_payloads() {
        let set = ConfigSet {
            header: HEADER,
            partition: 2,
            key: b"k",
            data: &[0x18, 0x2a],
        };
        let mut buf = [0u8; 32];
        assert_eq!(set.encode(&mut buf), Ok(25));
        assert_eq!(&buf[16..25], &[2, 1, 2, 0, 0, 0, b'k', 0x18, 0x2a]);
        assert_eq!(ConfigSet::decode(&buf[..25]), Ok(set));
        assert_eq!(ConfigSet::decode(&buf[..24]), Err(BmWireError::Truncated));
    }

    #[test]
    fn a_value_claiming_more_data_than_it_has_is_refused() {
        let mut buf = [0u8; ConfigValue::HEAD_LEN + 2];
        buf[17] = 3;
        assert_eq!(ConfigValue::decode(&buf), Err(BmWireError::Truncated));
        buf[17] = 2;
        assert_eq!(ConfigValue::decode(&buf).map(|v| v.data.len()), Ok(2));
    }

    #[test]
    fn status_keys_walk_one_byte_of_length_at_a_time() {
        let mut body = [0u8; 26];
        body[16..].copy_from_slice(&[0, 1, 3, 2, b'a', b'b', 1, b'c', 5, b'd']);
        let status = ConfigStatusResponse::decode(&body).unwrap();
        assert!(status.committed);
        let mut keys = status.keys();
        assert_eq!(keys.next(), Some(Ok(&b"ab"[..])));
        assert_eq!(keys.next(), Some(Ok(&b"c"[..])));
        assert_eq!(keys.next(), Some(Err(BmWireError::Truncated)));
        assert_eq!(keys.next(), None);
    }

    #[test]
    fn a_status_response_copies_key_len_bytes_of_each_key_buf() {
        let mut part = ConfigPartition::new(Layout::LP64);
        assert!(part.set_uint(Key::new(b"foo"), 1));
        // A key with its value straight after it, as `config.c` passes one:
        // `key_buf` keeps the value's bytes, and `key_len` only the key's.
        assert!(part.set_cbor(Key::with_len(b"ab\x61\x7a", 2), &[0x61, 0x7a]));
        assert_eq!(status_response_len(&part), Some(19 + 4 + 3));
        let mut buf = [0u8; 64];
        let len = encode_status_response(&mut buf, HEADER, 7, &part).unwrap();
        assert_eq!(
            &buf[16..len],
            &[7, 1, 2, 3, b'f', b'o', b'o', 2, b'a', b'b']
        );
    }

    #[test]
    fn decode_value_follows_the_c_for_each_type() {
        let mut buf = [0xeeu8; 8];
        assert_eq!(
            decode_value(ValueType::Uint32, &[0x18, 42], &mut buf),
            Ok(8)
        );
        assert_eq!(&buf[..4], &42u32.to_le_bytes());
        assert_eq!(
            decode_value(ValueType::Int32, &[0x18, 42], &mut buf),
            Err(DecodeError::WrongType)
        );
        assert_eq!(decode_value(ValueType::Int32, &[0x20], &mut buf), Ok(8));
        assert_eq!(&buf[..4], &(-1i32).to_le_bytes());
        assert_eq!(
            decode_value(ValueType::Float, &[0xf9, 0x3c, 0x00], &mut buf),
            Err(DecodeError::BadMessage),
            "a half is not a FLOAT (divergence #43)"
        );
        assert_eq!(
            decode_value(ValueType::Uint32, &[0x00], &mut buf[..3]),
            Err(DecodeError::BadMessage)
        );
        assert_eq!(decode_value(ValueType::Str, b"\x63abc", &mut buf), Ok(3));
        assert_eq!(&buf[..4], b"abc\0");
        assert_eq!(
            decode_value(ValueType::Str, b"\x63abc", &mut buf[..2]),
            Err(DecodeError::TooSmall(3))
        );
        assert_eq!(decode_value(ValueType::Array, &[0x80], &mut buf), Ok(8));
        assert_eq!(
            decode_value(ValueType::Array, &[], &mut buf),
            Err(DecodeError::Invalid)
        );
        assert_eq!(
            decode_value(ValueType::Array, &[0x80], &mut []),
            Err(DecodeError::Invalid)
        );
    }
}
