//! `BcmpDeviceInfoRequest` and `BcmpDeviceInfoReply`, ported from
//! `bcmp/messages.h` and `bcmp/info.c`.
//!
//! The reply is the first message with a variable-length body: a fixed
//! 38-byte head followed by two strings whose lengths are declared inside it,
//! concatenated with no separator and no NUL terminator.
//!
//! # Lengths are checked here and nowhere in the C
//!
//! `populate_neighbor_info` copies `ver_str_len` and `dev_name_len` bytes out
//! of the received frame without ever comparing them to how many bytes
//! actually arrived. [`DeviceInfoReply::decode`] validates both against the
//! buffer instead. See divergence #14 — this is a domain limit, not a
//! behaviour the port reproduces: there is no defined C behaviour to match.
//!
//! # The reply's consumer
//!
//! [`InfoRequests`] is `INFO_REQUEST_LIST` and [`InfoCache`] is the
//! `BcmpDeviceInfo`, `version_str` and `device_name` that
//! `populate_neighbor_info` writes onto a `BcmpNeighbor`. Both are sans-io:
//! `bm_stack::Node` transmits the request, gates the cache on its neighbour
//! table and forgets an entry when the neighbour holding it is evicted.
//!
//! `INFO_EXPECT_NODE_ID` has no counterpart. `bcmp_expect_info_from_node_id`
//! has no caller in bm_core outside its own test, and the branch it arms
//! builds a temporary neighbour, passes it to `bcmp_print_neighbor_info` and
//! frees it — it retains nothing and transmits nothing, so there is nothing
//! for a port to reproduce.

use crate::BmWireError;

/// `BcmpDeviceInfoRequest`: ask one node, or every node, to describe itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceInfoRequest {
    /// Node to answer, or zero for all of them.
    pub target_node_id: u64,
}

impl DeviceInfoRequest {
    /// Wire size.
    pub const LEN: usize = 8;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let bytes: [u8; 8] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        Ok(Self {
            target_node_id: u64::from_le_bytes(bytes),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf.copy_from_slice(&self.target_node_id.to_le_bytes());
        Ok(())
    }
}

/// `BcmpDeviceInfo`: the fixed part of a node's self-description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    /// Node id of the node being described.
    pub node_id: u64,
    /// Vendor of the hardware module.
    pub vendor_id: u16,
    /// Product identifier within the vendor.
    pub product_id: u16,
    /// Factory-flashed serial number.
    pub serial_num: [u8; 16],
    /// Last four bytes of the firmware's git SHA.
    pub git_sha: u32,
    /// Firmware major version.
    pub ver_major: u8,
    /// Firmware minor version.
    pub ver_minor: u8,
    /// Firmware revision.
    pub ver_rev: u8,
    /// Hardware revision, or zero for don't-care.
    pub ver_hw: u8,
}

impl DeviceInfo {
    /// Wire size.
    pub const LEN: usize = 36;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let buf: &[u8; Self::LEN] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        let mut serial_num = [0u8; 16];
        serial_num.copy_from_slice(&buf[12..28]);
        Ok(Self {
            node_id: u64::from_le_bytes(buf[0..8].try_into().expect("8 bytes")),
            vendor_id: u16::from_le_bytes([buf[8], buf[9]]),
            product_id: u16::from_le_bytes([buf[10], buf[11]]),
            serial_num,
            git_sha: u32::from_le_bytes(buf[28..32].try_into().expect("4 bytes")),
            ver_major: buf[32],
            ver_minor: buf[33],
            ver_rev: buf[34],
            ver_hw: buf[35],
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf[0..8].copy_from_slice(&self.node_id.to_le_bytes());
        buf[8..10].copy_from_slice(&self.vendor_id.to_le_bytes());
        buf[10..12].copy_from_slice(&self.product_id.to_le_bytes());
        buf[12..28].copy_from_slice(&self.serial_num);
        buf[28..32].copy_from_slice(&self.git_sha.to_le_bytes());
        buf[32] = self.ver_major;
        buf[33] = self.ver_minor;
        buf[34] = self.ver_rev;
        buf[35] = self.ver_hw;
        Ok(())
    }
}

/// `BcmpDeviceInfoReply`: [`DeviceInfo`] plus two length-prefixed strings.
///
/// The strings are borrowed from the frame and are **not** NUL-terminated on
/// the wire, nor validated as UTF-8 by bm_core. They are exposed as bytes for
/// that reason; use [`core::str::from_utf8`] if a caller needs text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfoReply<'a> {
    /// The fixed part.
    pub info: DeviceInfo,
    /// Firmware version string, at most 255 bytes.
    pub version_string: &'a [u8],
    /// Device name, at most 255 bytes.
    pub device_name: &'a [u8],
}

impl<'a> DeviceInfoReply<'a> {
    /// Size of the fixed part, before the strings. `sizeof(BcmpDeviceInfoReply)`.
    pub const HEADER_LEN: usize = DeviceInfo::LEN + 2;

    /// Longest string either field can carry, since each length is a `u8`.
    pub const MAX_STRING_LEN: usize = u8::MAX as usize;

    /// Decode a reply, borrowing its strings from `buf`.
    ///
    /// Trailing bytes past the declared strings are ignored, as they are by
    /// the C.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// shorter than the string lengths it declares.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let head = buf.get(..Self::HEADER_LEN).ok_or(BmWireError::Truncated)?;
        let info = DeviceInfo::decode(head)?;
        let version_len = usize::from(head[DeviceInfo::LEN]);
        let name_len = usize::from(head[DeviceInfo::LEN + 1]);

        // The check bm_core does not do.
        let strings = buf
            .get(Self::HEADER_LEN..Self::HEADER_LEN + version_len + name_len)
            .ok_or(BmWireError::Truncated)?;

        Ok(Self {
            info,
            version_string: &strings[..version_len],
            device_name: &strings[version_len..],
        })
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.version_string.len() + self.device_name.len()
    }

    /// Encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    /// [`BmWireError::Invalid`] if either string is longer than
    /// [`Self::MAX_STRING_LEN`], since the length fields could not describe it.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        if self.version_string.len() > Self::MAX_STRING_LEN
            || self.device_name.len() > Self::MAX_STRING_LEN
        {
            return Err(BmWireError::Invalid);
        }
        let end = self.encoded_len();
        let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;

        self.info.encode(buf)?;
        buf[DeviceInfo::LEN] = self.version_string.len() as u8;
        buf[DeviceInfo::LEN + 1] = self.device_name.len() as u8;
        let split = Self::HEADER_LEN + self.version_string.len();
        buf[Self::HEADER_LEN..split].copy_from_slice(self.version_string);
        buf[split..end].copy_from_slice(self.device_name);
        Ok(end)
    }
}

// ---------------------------------------------------------------------------
// `INFO_REQUEST_LIST` and the information it collects
// ---------------------------------------------------------------------------

/// Longest string [`InfoCache`] keeps per field by default, which is the
/// longest either field can describe on the wire.
pub const CACHED_STRING_BYTES: usize = DeviceInfoReply::MAX_STRING_LEN;

/// `bcmp_request_info`'s `cb` argument, as a choice rather than a pointer.
///
/// `bcmp_process_info_reply` takes one branch or the other, never both: a
/// request made with a callback never updates the cache, and one made without
/// never reaches the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoRequestKind {
    /// `cb == NULL`, which is what both of bm_core's own call sites pass
    /// (`bcmp_update_neighbor` and `bcmp_process_heartbeat`'s restart path).
    /// The reply updates [`InfoCache`], and only if the sender is already in
    /// the neighbour table.
    Cache,
    /// `cb != NULL`. The reply goes to the caller and no cache is touched.
    Report,
}

/// `INFO_REQUEST_LIST`: which nodes have been asked to describe themselves and
/// have not answered.
///
/// bm_core keeps a `bm_malloc`'d `LL`. This is a fixed-capacity array with the
/// three properties of that list that are observable:
///
/// * **No de-duplication.** `ll_item_add` appends unconditionally, so asking
///   the same node twice leaves two entries and it takes two replies to clear
///   them. See divergence #19.
/// * **No expiry.** The only removal is a reply, so a node that is asked and
///   never answers keeps its entry for the life of the process. `N` is
///   therefore a ceiling bm_core does not have: [`Self::record`] reports a
///   full list rather than growing.
/// * **Thirty-two bit keys.** `LLItem::id` is a `uint32_t` while node ids are
///   64-bit, so the list is keyed on the low half of one. See divergence #33.
#[derive(Debug, Clone)]
pub struct InfoRequests<const N: usize> {
    entries: [(u32, InfoRequestKind); N],
    len: usize,
}

impl<const N: usize> Default for InfoRequests<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> InfoRequests<N> {
    /// An empty list.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [(0, InfoRequestKind::Cache); N],
            len: 0,
        }
    }

    /// The key `ll_create_item` is given: the low 32 bits of the node id.
    #[must_use]
    pub const fn key(node_id: u64) -> u32 {
        node_id as u32
    }

    /// How many requests are outstanding, duplicates counted separately.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is outstanding.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Most entries the list can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// The outstanding requests, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = (u32, InfoRequestKind)> + '_ {
        self.entries[..self.len].iter().copied()
    }

    /// Whether anything is outstanding for `node_id`, by the truncated key.
    #[must_use]
    pub fn contains(&self, node_id: u64) -> bool {
        let key = Self::key(node_id);
        self.entries[..self.len].iter().any(|(k, _)| *k == key)
    }

    /// Record a request — `ll_create_item` and `ll_item_add`.
    ///
    /// Appends without looking for an existing entry, as the C does. Returns
    /// `false`, and records nothing, once `N` entries are outstanding; the C
    /// reaches the same place only on a `bm_malloc` failure, which it reports
    /// as `BmENOMEM` and does not transmit for either.
    pub fn record(&mut self, target_node_id: u64, kind: InfoRequestKind) -> bool {
        if self.len == N {
            return false;
        }
        self.entries[self.len] = (Self::key(target_node_id), kind);
        self.len += 1;
        true
    }

    /// Consume the first request outstanding for `node_id` — `ll_get_item`
    /// followed by `ll_remove`, which both match on the first entry with the
    /// key.
    ///
    /// `None` when nothing was asked, which is what makes an unsolicited
    /// device-info reply do nothing at all.
    pub fn take(&mut self, node_id: u64) -> Option<InfoRequestKind> {
        let key = Self::key(node_id);
        let index = self.entries[..self.len]
            .iter()
            .position(|(k, _)| *k == key)?;
        let kind = self.entries[index].1;
        self.entries.copy_within(index + 1..self.len, index);
        self.len -= 1;
        Some(kind)
    }
}

/// One node's device information, borrowed out of [`InfoCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedInfo<'a> {
    /// The fixed part, as the reply carried it.
    pub info: DeviceInfo,
    /// The version string most recently reported, empty if none ever was.
    pub version_string: &'a [u8],
    /// The device name most recently reported, empty if none ever was.
    pub device_name: &'a [u8],
}

/// What `populate_neighbor_info` leaves on a `BcmpNeighbor`: the fixed part of
/// each node's self-description and its two strings.
///
/// bm_core hangs this off the neighbour table entry and frees it with the
/// entry; here it is a table of its own, keyed by node id, and `bm_stack`
/// forgets an entry when the neighbour holding it is evicted.
///
/// `STRING` is how many bytes of each string are kept. bm_core `bm_malloc`s
/// exactly what arrived, so the default, [`CACHED_STRING_BYTES`], is the
/// longest a `u8` length can describe and nothing is ever truncated. A smaller
/// value is a deliberate divergence for a node that cannot spare the memory:
/// the excess is dropped, and what is kept is still the prefix that arrived.
#[derive(Debug, Clone)]
pub struct InfoCache<const N: usize, const STRING: usize = CACHED_STRING_BYTES> {
    entries: [CacheEntry<STRING>; N],
    len: usize,
}

#[derive(Debug, Clone, Copy)]
struct CacheEntry<const STRING: usize> {
    node_id: u64,
    info: DeviceInfo,
    version_len: usize,
    version: [u8; STRING],
    name_len: usize,
    name: [u8; STRING],
}

impl<const STRING: usize> CacheEntry<STRING> {
    const EMPTY: Self = Self {
        node_id: 0,
        info: DeviceInfo {
            node_id: 0,
            vendor_id: 0,
            product_id: 0,
            serial_num: [0; 16],
            git_sha: 0,
            ver_major: 0,
            ver_minor: 0,
            ver_rev: 0,
            ver_hw: 0,
        },
        version_len: 0,
        version: [0; STRING],
        name_len: 0,
        name: [0; STRING],
    };

    fn view(&self) -> CachedInfo<'_> {
        CachedInfo {
            info: self.info,
            version_string: &self.version[..self.version_len],
            device_name: &self.name[..self.name_len],
        }
    }
}

impl<const N: usize, const STRING: usize> Default for InfoCache<N, STRING> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const STRING: usize> InfoCache<N, STRING> {
    /// An empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [CacheEntry::EMPTY; N],
            len: 0,
        }
    }

    /// How many nodes are described.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is described.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Most nodes the cache can describe.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// The node ids described, in insertion order.
    pub fn node_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries[..self.len].iter().map(|entry| entry.node_id)
    }

    /// What is known about `node_id`.
    #[must_use]
    pub fn get(&self, node_id: u64) -> Option<CachedInfo<'_>> {
        self.entries[..self.len]
            .iter()
            .find(|entry| entry.node_id == node_id)
            .map(CacheEntry::view)
    }

    /// Every entry, in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = CachedInfo<'_>> + '_ {
        self.entries[..self.len].iter().map(CacheEntry::view)
    }

    /// Record a reply against the node it names — `populate_neighbor_info`.
    ///
    /// The fixed part is replaced whole, every time. **A string is replaced
    /// only when the reply declares a non-zero length for it**: the C guards
    /// each `bm_free`/`bm_malloc`/`memcpy` with `if (dev_info->ver_str_len)`,
    /// so a reply carrying no strings updates the numbers and leaves whatever
    /// the last reply said the node was called.
    ///
    /// Returns `false`, changing nothing, when the node is new and the cache
    /// is full. bm_core has no such ceiling: its storage is the neighbour
    /// table entry, which already exists by the time this runs.
    pub fn store(&mut self, reply: &DeviceInfoReply<'_>) -> bool {
        let node_id = reply.info.node_id;
        let index = match self.entries[..self.len]
            .iter()
            .position(|entry| entry.node_id == node_id)
        {
            Some(index) => index,
            None => {
                if self.len == N {
                    return false;
                }
                self.entries[self.len] = CacheEntry::EMPTY;
                self.entries[self.len].node_id = node_id;
                self.len += 1;
                self.len - 1
            }
        };

        let entry = &mut self.entries[index];
        entry.info = reply.info;
        if !reply.version_string.is_empty() {
            entry.version_len = copy_truncating(&mut entry.version, reply.version_string);
        }
        if !reply.device_name.is_empty() {
            entry.name_len = copy_truncating(&mut entry.name, reply.device_name);
        }
        true
    }

    /// Forget `node_id` — `bcmp_free_neighbor`, which frees both strings along
    /// with the entry holding them.
    ///
    /// Reports whether there was anything to forget.
    pub fn forget(&mut self, node_id: u64) -> bool {
        let Some(index) = self.entries[..self.len]
            .iter()
            .position(|entry| entry.node_id == node_id)
        else {
            return false;
        };
        self.entries.copy_within(index + 1..self.len, index);
        self.len -= 1;
        true
    }
}

/// Copy as much of `src` as fits, reporting how much that was.
fn copy_truncating(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());
    dst[..len].copy_from_slice(&src[..len]);
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(DeviceInfo::LEN, 36);
        assert_eq!(DeviceInfoReply::HEADER_LEN, 38);
        assert_eq!(DeviceInfoRequest::LEN, 8);
    }

    /// `ipv6_pseudo_checksum_real_packet3` in
    /// `bm_core/test/src/bm_linux_test.cpp` is a device-info reply captured
    /// off a real link. Decoding it checks the layout against the wire rather
    /// than against our own encoder.
    #[test]
    fn a_captured_reply_decodes() {
        // The 49 body bytes, after the 13-byte BCMP header.
        let body = [
            0x11, 0x00, 0xAA, 0x55, 0x00, 0x00, 0x00, 0x00, // node_id
            0x01, 0x00, // vendor_id
            0x01, 0x00, // product_id
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, // serial_num
            0x00, 0x00, 0x00, 0x00, // git_sha
            0x00, 0x01, 0x00, 0x01, // ver_major, ver_minor, ver_rev, ver_hw
            0x05, 0x06, // ver_str_len, dev_name_len
            0x30, 0x2E, 0x31, 0x2E, 0x30, // "0.1.0"
            0x62, 0x6D, 0x5F, 0x73, 0x62, 0x63, // "bm_sbc"
        ];
        assert_eq!(body.len(), DeviceInfoReply::HEADER_LEN + 5 + 6);

        let reply = DeviceInfoReply::decode(&body).unwrap();
        assert_eq!(reply.info.node_id, 0x0000_0000_55AA_0011);
        assert_eq!(reply.info.vendor_id, 1);
        assert_eq!(reply.info.product_id, 1);
        assert_eq!(reply.info.serial_num, [0u8; 16]);
        assert_eq!(reply.info.git_sha, 0);
        assert_eq!(
            (
                reply.info.ver_major,
                reply.info.ver_minor,
                reply.info.ver_rev,
                reply.info.ver_hw
            ),
            (0, 1, 0, 1)
        );
        assert_eq!(reply.version_string, b"0.1.0");
        assert_eq!(reply.device_name, b"bm_sbc");

        // The node id matches the address the frame came from, fe80::55aa:11.
        let src = crate::util::BmIpAddr([
            0xFE, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0xAA,
            0x00, 0x11,
        ]);
        assert_eq!(src.to_node_id(), reply.info.node_id);

        let mut out = [0u8; 64];
        assert_eq!(reply.encode(&mut out).unwrap(), body.len());
        assert_eq!(&out[..body.len()], &body);
    }

    #[test]
    fn declared_string_lengths_are_checked_against_the_buffer() {
        let mut body = [0u8; DeviceInfoReply::HEADER_LEN + 4];
        body[DeviceInfo::LEN] = 3; // version
        body[DeviceInfo::LEN + 1] = 1; // name -- 4 bytes, exactly fits
        assert!(DeviceInfoReply::decode(&body).is_ok());

        // One byte more than arrived. bm_core would read past the frame here.
        body[DeviceInfo::LEN + 1] = 2;
        assert_eq!(
            DeviceInfoReply::decode(&body),
            Err(BmWireError::Truncated),
            "a length longer than the buffer must be refused, not trusted"
        );

        // The worst case: both lengths saturated on a minimum-size body.
        let mut minimal = [0u8; DeviceInfoReply::HEADER_LEN];
        minimal[DeviceInfo::LEN] = 255;
        minimal[DeviceInfo::LEN + 1] = 255;
        assert_eq!(
            DeviceInfoReply::decode(&minimal),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn empty_strings_round_trip() {
        let reply = DeviceInfoReply {
            info: DeviceInfo::default(),
            version_string: b"",
            device_name: b"",
        };
        let mut buf = [0u8; DeviceInfoReply::HEADER_LEN];
        assert_eq!(reply.encode(&mut buf).unwrap(), DeviceInfoReply::HEADER_LEN);
        assert_eq!(DeviceInfoReply::decode(&buf).unwrap(), reply);
    }

    #[test]
    fn strings_too_long_for_a_u8_length_are_refused() {
        let long = [b'x'; 256];
        let reply = DeviceInfoReply {
            info: DeviceInfo::default(),
            version_string: &long,
            device_name: b"",
        };
        let mut buf = [0u8; 512];
        assert_eq!(reply.encode(&mut buf), Err(BmWireError::Invalid));
    }

    #[test]
    fn trailing_bytes_past_the_strings_are_ignored() {
        let mut body = [0xAAu8; DeviceInfoReply::HEADER_LEN + 16];
        body[..DeviceInfo::LEN].fill(0);
        body[DeviceInfo::LEN] = 2;
        body[DeviceInfo::LEN + 1] = 2;
        let reply = DeviceInfoReply::decode(&body).unwrap();
        assert_eq!(reply.version_string, &[0xAA, 0xAA]);
        assert_eq!(reply.device_name, &[0xAA, 0xAA]);
        assert_eq!(reply.encoded_len(), DeviceInfoReply::HEADER_LEN + 4);
    }

    fn reply<'a>(node_id: u64, version: &'a [u8], name: &'a [u8]) -> DeviceInfoReply<'a> {
        DeviceInfoReply {
            info: DeviceInfo {
                node_id,
                ..DeviceInfo::default()
            },
            version_string: version,
            device_name: name,
        }
    }

    #[test]
    fn a_request_list_is_keyed_on_the_low_half_of_the_node_id() {
        // `LLItem::id` is a uint32_t; `bcmp_request_info` hands it a uint64_t.
        assert_eq!(
            InfoRequests::<4>::key(0xDEAD_BEEF_1234_5678),
            0x1234_5678,
            "the top half never reaches the list"
        );

        let mut list = InfoRequests::<4>::new();
        assert!(list.record(0x0000_0001_0000_0009, InfoRequestKind::Cache));
        assert!(
            list.contains(0xFFFF_FFFF_0000_0009),
            "a node sharing the low 32 bits looks like the one that was asked"
        );
        assert_eq!(
            list.take(0xFFFF_FFFF_0000_0009),
            Some(InfoRequestKind::Cache)
        );
        assert!(list.is_empty());
    }

    #[test]
    fn a_request_list_does_not_de_duplicate_and_never_expires() {
        let mut list = InfoRequests::<4>::new();
        for _ in 0..3 {
            assert!(list.record(0xAA, InfoRequestKind::Cache));
        }
        assert_eq!(list.len(), 3, "ll_item_add appends unconditionally");

        // One reply clears one entry, so the other two wait forever.
        assert_eq!(list.take(0xAA), Some(InfoRequestKind::Cache));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn a_request_list_consumes_its_entries_in_order() {
        let mut list = InfoRequests::<4>::new();
        assert!(list.record(0xAA, InfoRequestKind::Report));
        assert!(list.record(0xBB, InfoRequestKind::Cache));
        assert!(list.record(0xAA, InfoRequestKind::Cache));

        // ll_get_item and ll_remove both stop at the first match.
        assert_eq!(list.take(0xAA), Some(InfoRequestKind::Report));
        assert!(
            list.iter().eq([
                (0xBBu32, InfoRequestKind::Cache),
                (0xAA, InfoRequestKind::Cache)
            ]),
            "the rest keep their order"
        );
        assert_eq!(list.take(0xCC), None, "nothing was asked of that node");
    }

    #[test]
    fn a_full_request_list_records_nothing() {
        let mut list = InfoRequests::<2>::new();
        assert!(list.record(1, InfoRequestKind::Cache));
        assert!(list.record(2, InfoRequestKind::Cache));
        assert!(!list.record(3, InfoRequestKind::Cache));
        assert_eq!(list.len(), 2);
        assert!(!list.contains(3));
    }

    #[test]
    fn a_cached_reply_reads_back_whole() {
        let mut cache = InfoCache::<4>::new();
        let mut reply = reply(0xAA, b"1.2.3", b"bm_sbc");
        reply.info.vendor_id = 0xBEEF;
        reply.info.ver_hw = 7;
        assert!(cache.store(&reply));

        let cached = cache.get(0xAA).unwrap();
        assert_eq!(cached.info, reply.info);
        assert_eq!(cached.version_string, b"1.2.3");
        assert_eq!(cached.device_name, b"bm_sbc");
        assert_eq!(cache.len(), 1);
        assert!(cache.node_ids().eq([0xAAu64]));
        assert!(cache.get(0xBB).is_none());
    }

    /// `populate_neighbor_info` guards each string with `if (len)`, so a reply
    /// carrying none updates the numbers and leaves the old strings in place.
    #[test]
    fn an_empty_string_keeps_the_last_one_rather_than_clearing_it() {
        let mut cache = InfoCache::<4>::new();
        assert!(cache.store(&reply(0xAA, b"1.2.3", b"bm_sbc")));

        let mut second = reply(0xAA, b"", b"");
        second.info.git_sha = 0x1234_5678;
        assert!(cache.store(&second));

        let cached = cache.get(0xAA).unwrap();
        assert_eq!(
            cached.info.git_sha, 0x1234_5678,
            "the fixed part is replaced"
        );
        assert_eq!(cached.version_string, b"1.2.3", "the string is not");
        assert_eq!(cached.device_name, b"bm_sbc");
        assert_eq!(cache.len(), 1, "and it is the same entry");
    }

    #[test]
    fn a_non_empty_string_replaces_the_last_one() {
        let mut cache = InfoCache::<4>::new();
        assert!(cache.store(&reply(0xAA, b"1.2.3", b"bm_sbc")));
        assert!(cache.store(&reply(0xAA, b"9", b"")));

        let cached = cache.get(0xAA).unwrap();
        assert_eq!(cached.version_string, b"9");
        assert_eq!(cached.device_name, b"bm_sbc");
    }

    #[test]
    fn forgetting_an_entry_closes_the_gap() {
        let mut cache = InfoCache::<4>::new();
        assert!(cache.store(&reply(0xAA, b"a", b"a")));
        assert!(cache.store(&reply(0xBB, b"b", b"b")));
        assert!(cache.store(&reply(0xCC, b"c", b"c")));

        assert!(cache.forget(0xBB));
        assert!(!cache.forget(0xBB), "and only once");
        assert!(cache.node_ids().eq([0xAAu64, 0xCC]));
        assert!(cache.get(0xBB).is_none());
    }

    #[test]
    fn a_full_cache_refuses_a_new_node_but_still_updates_a_known_one() {
        let mut cache = InfoCache::<2>::new();
        assert!(cache.store(&reply(0xAA, b"a", b"a")));
        assert!(cache.store(&reply(0xBB, b"b", b"b")));
        assert!(!cache.store(&reply(0xCC, b"c", b"c")));
        assert!(cache.get(0xCC).is_none());
        assert!(cache.store(&reply(0xAA, b"a2", b"a2")));
        assert_eq!(cache.get(0xAA).unwrap().version_string, b"a2");
    }

    /// The default keeps everything a `u8` length can describe. A smaller
    /// `STRING` is the deliberate divergence the type documents.
    #[test]
    fn the_default_string_capacity_never_truncates() {
        assert_eq!(CACHED_STRING_BYTES, DeviceInfoReply::MAX_STRING_LEN);
        let longest = [b'x'; CACHED_STRING_BYTES];
        let mut cache = InfoCache::<1>::new();
        assert!(cache.store(&reply(0xAA, &longest, &longest)));
        let cached = cache.get(0xAA).unwrap();
        assert_eq!(cached.version_string, &longest[..]);
        assert_eq!(cached.device_name, &longest[..]);

        let mut small = InfoCache::<1, 2>::new();
        assert!(small.store(&reply(0xAA, b"abcdef", b"ghijkl")));
        let cached = small.get(0xAA).unwrap();
        assert_eq!(cached.version_string, b"ab", "the prefix that arrived");
        assert_eq!(cached.device_name, b"gh");
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..DeviceInfoReply::HEADER_LEN {
            assert_eq!(
                DeviceInfoReply::decode(&[0u8; DeviceInfoReply::HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
        }
        for len in 0..DeviceInfoRequest::LEN {
            assert_eq!(
                DeviceInfoRequest::decode(&[0u8; 8][..len]),
                Err(BmWireError::Truncated)
            );
        }
    }
}
