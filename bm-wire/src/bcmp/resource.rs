//! `BcmpResourceTableRequest` and `BcmpResourceTableReply`, ported from
//! `bcmp/messages.h` and `bcmp/resource_discovery.c`.
//!
//! The reply is a 12-byte head — `node_id`, `num_pubs`, `num_subs` — followed
//! by `num_pubs + num_subs` records of `{ uint16 len; char name[len] }`,
//! **publishers first**. Every other variable-length BCMP body declares its
//! lengths in the head; this one interleaves them with the data, so the only
//! way to find the nth record is to walk the first n. The C walks it without
//! ever consulting `BcmpProcessData.size`, exactly as it parses device-info
//! and neighbour-table replies — divergence #14.
//!
//! # `target_node_id == 0` is not a broadcast here
//!
//! `bcmp_process_resource_discovery_request` rejects anything but an exact
//! match against `node_id()`, where `bcmp/info.c`, `bcmp/ping.c` and
//! `bcmp/neighbors.c` all treat zero as "any node". So a `0x0A` naming zero is
//! answered by nobody. See divergence #37 and [`ResourceTableRequest::is_for`].
//!
//! # The two consumers
//!
//! [`ResourceTable`] is `PUB_LIST` and `SUB_LIST`, and [`ResourceRequests`] is
//! `RESOURCE_REQUEST_LIST`. Both are module state rather than wire format, and
//! are here for the reason [`super::info::InfoRequests`] is: sans-io, with
//! `bm_stack::Node` owning the transmission and the clock.
//!
//! This module's correlation is its own, and is not `bcmp/info.c`'s:
//!
//! | | `INFO_REQUEST_LIST` (`0x04`) | `RESOURCE_REQUEST_LIST` (`0x0A`) |
//! |---|---|---|
//! | Outstanding requests | unbounded list | unbounded list |
//! | Keyed on | low 32 bits of the id (#33) | low 32 bits of the id (#33) |
//! | Matched against | the reply's body `node_id` | the reply's body `node_id`, which must **also** equal the source address |
//! | A broadcast request | answered by everyone | answered by nobody (#37) |
//! | Expiry | none (#19) | none (#19) |

use crate::BmWireError;

/// `ResourceType`: which of the two lists a resource belongs to.
///
/// The C enum is `{ PUB, SUB }`, and every function that takes one selects
/// `&SUB_LIST` for `SUB` and `&PUB_LIST` for *anything else* — so a value
/// outside the enum reads as [`Self::Publisher`]. Nothing here can produce
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    /// `PUB`: a topic this node publishes to.
    Publisher,
    /// `SUB`: a topic this node subscribes to.
    Subscriber,
}

/// `BcmpResourceTableRequest`: ask one node for its resource table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceTableRequest {
    /// Node that is to answer.
    ///
    /// The field's doc comment in `bcmp/messages.h` says "Zeroed = all
    /// nodes". The handler does not implement that; see [`Self::is_for`].
    pub target_node_id: u64,
}

impl ResourceTableRequest {
    /// Wire size.
    pub const LEN: usize = 8;

    /// Whether `bcmp_process_resource_discovery_request` would answer this on
    /// a node whose id is `node_id`.
    ///
    /// An exact match and nothing else — `if (req->target_node_id !=
    /// node_id()) break;`. A request naming zero is answered by no node
    /// (divergence #37), and a node whose link-local address is exactly
    /// `fe80::` answers only requests naming zero, since [`u64`] zero is its
    /// id.
    #[must_use]
    pub const fn is_for(&self, node_id: u64) -> bool {
        self.target_node_id == node_id
    }

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

/// `BcmpResource`: one length-prefixed resource name.
///
/// The name is not NUL-terminated on the wire and bm_core does not validate it
/// as UTF-8 — `bcmp_resource_discovery_print_resources` prints it with
/// `%.*s`. Exposed as bytes for that reason; use [`core::str::from_utf8`] if a
/// caller needs text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resource<'a> {
    /// The name, exactly `resource_len` bytes of it.
    pub name: &'a [u8],
}

impl<'a> Resource<'a> {
    /// Size of the length prefix. `sizeof(BcmpResource)`.
    pub const HEADER_LEN: usize = 2;

    /// Longest name the prefix can describe.
    pub const MAX_NAME_LEN: usize = u16::MAX as usize;

    /// Bytes this record occupies on the wire.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.name.len()
    }

    /// Decode one record from the front of `buf`, returning it and the bytes
    /// it consumed.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the prefix, or
    /// shorter than the length the prefix declares.
    pub fn decode(buf: &'a [u8]) -> Result<(Self, usize), BmWireError> {
        let head = buf.get(..Self::HEADER_LEN).ok_or(BmWireError::Truncated)?;
        let len = usize::from(u16::from_le_bytes([head[0], head[1]]));
        let name = buf
            .get(Self::HEADER_LEN..Self::HEADER_LEN + len)
            .ok_or(BmWireError::Truncated)?;
        Ok((Self { name }, Self::HEADER_LEN + len))
    }
}

/// `BcmpResourceTableReply`, borrowed from the frame it arrived in.
///
/// The record region is validated once, at decode, so [`Self::publishers`] and
/// [`Self::subscribers`] cannot walk past the frame. bm_core validates nothing
/// — `bcmp_process_resource_discovery_reply` walks `num_pubs` records and then
/// `num_subs` more, advancing by each record's own declared length, and the
/// only bound on any of it is where `bm_malloc` happened to put the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceTableReply<'a> {
    /// Node id of the replying node, which `bcmp_process_resource_discovery_reply`
    /// requires to equal the source address the frame arrived from.
    pub node_id: u64,
    num_pubs: u16,
    num_subs: u16,
    /// The publisher records, exactly.
    publishers: &'a [u8],
    /// The subscriber records, exactly.
    subscribers: &'a [u8],
}

impl<'a> ResourceTableReply<'a> {
    /// Size of the fixed part. `sizeof(BcmpResourceTableReply)`.
    pub const HEADER_LEN: usize = 12;

    /// Decode a reply, borrowing its records from `buf`.
    ///
    /// Trailing bytes past the declared records are ignored, as they are by
    /// the C.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// if the records it declares do not fit in what followed.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let head = buf.get(..Self::HEADER_LEN).ok_or(BmWireError::Truncated)?;
        let node_id = u64::from_le_bytes(head[0..8].try_into().expect("8 bytes"));
        let num_pubs = u16::from_le_bytes([head[8], head[9]]);
        let num_subs = u16::from_le_bytes([head[10], head[11]]);

        let records = &buf[Self::HEADER_LEN..];
        // The walk the C does, with the bound the C does not have.
        let pub_bytes = measure(records, num_pubs)?;
        let sub_bytes = measure(&records[pub_bytes..], num_subs)?;
        Ok(Self {
            node_id,
            num_pubs,
            num_subs,
            publishers: &records[..pub_bytes],
            subscribers: &records[pub_bytes..pub_bytes + sub_bytes],
        })
    }

    /// `num_pubs`.
    #[must_use]
    pub const fn publisher_count(&self) -> u16 {
        self.num_pubs
    }

    /// `num_subs`.
    #[must_use]
    pub const fn subscriber_count(&self) -> u16 {
        self.num_subs
    }

    /// The publisher records, in order.
    pub fn publishers(&self) -> impl Iterator<Item = Resource<'a>> + '_ {
        Records {
            rest: self.publishers,
        }
    }

    /// The subscriber records, in order.
    pub fn subscribers(&self) -> impl Iterator<Item = Resource<'a>> + '_ {
        Records {
            rest: self.subscribers,
        }
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.publishers.len() + self.subscribers.len()
    }

    /// Re-encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let end = self.encoded_len();
        let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;
        write_head(buf, self.node_id, self.num_pubs, self.num_subs);
        let split = Self::HEADER_LEN + self.publishers.len();
        buf[Self::HEADER_LEN..split].copy_from_slice(self.publishers);
        buf[split..end].copy_from_slice(self.subscribers);
        Ok(end)
    }
}

/// How many bytes `count` records occupy at the front of `buf`.
fn measure(buf: &[u8], count: u16) -> Result<usize, BmWireError> {
    let mut at = 0;
    for _ in 0..count {
        let (_, len) = Resource::decode(buf.get(at..).ok_or(BmWireError::Truncated)?)?;
        at += len;
    }
    Ok(at)
}

/// Walks a region of records that [`ResourceTableReply::decode`] has already
/// bounds-checked.
struct Records<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Records<'a> {
    type Item = Resource<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let (resource, len) = Resource::decode(self.rest).expect("decode bounded the region");
        self.rest = &self.rest[len..];
        Some(resource)
    }
}

fn write_head(buf: &mut [u8], node_id: u64, num_pubs: u16, num_subs: u16) {
    buf[0..8].copy_from_slice(&node_id.to_le_bytes());
    buf[8..10].copy_from_slice(&num_pubs.to_le_bytes());
    buf[10..12].copy_from_slice(&num_subs.to_le_bytes());
}

/// Build a reply from two lists of names, returning how many bytes were
/// written.
///
/// The counts are written after the records, so each list is walked once and
/// nothing has to be counted up front.
///
/// # Errors
///
/// [`BmWireError::Invalid`] if either list is longer than its `u16` count, or
/// if a name is longer than [`Resource::MAX_NAME_LEN`].
/// [`BmWireError::Truncated`] if `buf` is too short.
pub fn encode_resource_table_reply<'p, 's>(
    buf: &mut [u8],
    node_id: u64,
    publishers: impl Iterator<Item = &'p [u8]>,
    subscribers: impl Iterator<Item = &'s [u8]>,
) -> Result<usize, BmWireError> {
    if buf.len() < ResourceTableReply::HEADER_LEN {
        return Err(BmWireError::Truncated);
    }
    let mut at = ResourceTableReply::HEADER_LEN;
    let num_pubs = write_records(buf, &mut at, publishers)?;
    let num_subs = write_records(buf, &mut at, subscribers)?;
    write_head(buf, node_id, num_pubs, num_subs);
    Ok(at)
}

fn write_records<'n>(
    buf: &mut [u8],
    at: &mut usize,
    names: impl Iterator<Item = &'n [u8]>,
) -> Result<u16, BmWireError> {
    let mut count: u16 = 0;
    for name in names {
        if name.len() > Resource::MAX_NAME_LEN {
            return Err(BmWireError::Invalid);
        }
        count = count.checked_add(1).ok_or(BmWireError::Invalid)?;
        let end = *at + Resource::HEADER_LEN + name.len();
        let slot = buf.get_mut(*at..end).ok_or(BmWireError::Truncated)?;
        slot[0..2].copy_from_slice(&(name.len() as u16).to_le_bytes());
        slot[Resource::HEADER_LEN..].copy_from_slice(name);
        *at = end;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// `PUB_LIST` and `SUB_LIST`
// ---------------------------------------------------------------------------

/// Why [`ResourceTable::add`] added nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceAddError {
    /// [`ResourceTable::find`] matched, the C's `BmEAGAIN`.
    ///
    /// The match is a prefix match, so this reports names that are not equal
    /// to anything in the list — see [`ResourceTable::find`].
    AlreadyPresent,
    /// The name does not fit `NAME` bytes, or the table already holds `N`
    /// resources.
    ///
    /// Ceilings bm_core does not have: it `bm_malloc`s a node and a
    /// `sizeof(BcmpResource) + resource_len` buffer per resource, and reports
    /// only the node's failure. See [`ResourceTable`].
    Full,
}

/// A name whose length the C would read past — [`ResourceTable::find`]'s
/// undefined case.
///
/// `bcmp_resource_discovery_find_resource_priv` compares `resource_len` bytes
/// of the *needle* against each stored entry, whichever length that entry was
/// allocated with. A needle longer than an entry it reaches therefore reads
/// past that entry's allocation. Divergence #38.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindReadsOutOfBounds {
    /// Index within its list of the entry the C would over-read.
    pub entry: usize,
    /// That entry's length, which is less than the needle's.
    pub entry_len: usize,
}

/// `PUB_LIST` and `SUB_LIST`: the resources this node publishes to and
/// subscribes to.
///
/// bm_core keeps two `bm_malloc`'d singly-linked lists, each with its own
/// semaphore and a `uint16_t num_resources`. This is one fixed-capacity
/// structure with the properties of those lists that are observable:
///
/// * **Append-only.** There is no remove, no clear and no deinit — the only
///   way back to an empty list is `bcmp_resource_discovery_init`, which drops
///   the chain on the floor and creates fresh semaphores. A resource a node
///   ever advertises, it advertises for as long as it runs.
/// * **Insertion order is wire order.** `bcmp_resource_populate_msg_data`
///   walks from `start`, so the reply lists resources oldest-first within each
///   half.
/// * **The de-duplication is a prefix match.** See [`Self::find`].
///
/// `N` is how many resources both lists hold between them and `NAME` how many
/// bytes of each name are kept. Both are ceilings bm_core does not have; past
/// either, [`Self::add`] reports [`ResourceAddError::Full`] rather than
/// truncating, because a truncated name is a different name on the wire.
#[derive(Debug, Clone)]
pub struct ResourceTable<const N: usize, const NAME: usize = RESOURCE_NAME_BYTES> {
    entries: [Entry<NAME>; N],
    len: usize,
}

/// Default longest resource name [`ResourceTable`] keeps.
///
/// bm_core has no limit but `bcmp_tx`'s, which is 1448 bytes for the whole
/// reply. Bristlemouth topic names are `middleware/pubsub.c` topic strings —
/// tens of bytes — so this is generous, and a node that needs longer raises
/// the parameter.
pub const RESOURCE_NAME_BYTES: usize = 64;

#[derive(Debug, Clone, Copy)]
struct Entry<const NAME: usize> {
    kind: ResourceType,
    len: usize,
    name: [u8; NAME],
}

impl<const NAME: usize> Entry<NAME> {
    const EMPTY: Self = Self {
        kind: ResourceType::Publisher,
        len: 0,
        name: [0; NAME],
    };

    fn name(&self) -> &[u8] {
        &self.name[..self.len]
    }
}

impl<const N: usize, const NAME: usize> Default for ResourceTable<N, NAME> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const NAME: usize> ResourceTable<N, NAME> {
    /// Two empty lists.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [Entry::EMPTY; N],
            len: 0,
        }
    }

    /// How many resources both lists hold between them.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether both lists are empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Most resources the two lists hold between them.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Longest name a resource may have.
    #[must_use]
    pub const fn name_capacity(&self) -> usize {
        NAME
    }

    /// `bcmp_resource_discovery_get_num_resources`: how many resources one
    /// list holds.
    #[must_use]
    pub fn count(&self, kind: ResourceType) -> u16 {
        // `num_resources` is a `uint16_t` the C only ever increments, so a
        // list of more than 65 535 would wrap there. `N` is the ceiling here.
        self.iter(kind).count() as u16
    }

    /// One list's names, oldest first — the order they go out in.
    pub fn iter(&self, kind: ResourceType) -> impl Iterator<Item = &[u8]> + '_ {
        self.entries[..self.len]
            .iter()
            .filter(move |entry| entry.kind == kind)
            .map(Entry::name)
    }

    /// `bcmp_resource_discovery_find_resource`, quirk included.
    ///
    /// **This is a prefix match, not an equality test.** The C is
    ///
    /// ```c
    /// if (memcmp(resource, cur->resource->resource, resource_len) == 0)
    /// ```
    ///
    /// which compares the *needle's* length and ignores `cur->resource_len`
    /// entirely. So a search for `bm/x` matches a stored `bm/xyz`, a search
    /// for the empty name matches whatever is at the head of the list, and
    /// `bcmp_resource_discovery_add_resource` refuses to add a name that is
    /// merely a prefix of one already there. Divergence #38.
    ///
    /// The other half of that quirk is undefined and is *not* reproduced: a
    /// needle longer than an entry the walk reaches has the C reading past
    /// that entry's allocation. Here such an entry simply does not match and
    /// the walk continues. [`Self::find_over_reads`] reports whether the C
    /// would have gone out of bounds, which is how a differential comparator
    /// stays out of the undefined half.
    #[must_use]
    pub fn find(&self, name: &[u8], kind: ResourceType) -> bool {
        self.iter(kind)
            .any(|stored| stored.len() >= name.len() && &stored[..name.len()] == name)
    }

    /// Whether `bcmp_resource_discovery_find_resource_priv` would read out of
    /// bounds looking for `name`, and where.
    ///
    /// The walk stops at the first match, so only the entries before it are
    /// read. `None` means every `memcmp` the C performs stays inside its own
    /// allocation and [`Self::find`] is a faithful comparison.
    #[must_use]
    pub fn find_over_reads(&self, name: &[u8], kind: ResourceType) -> Option<FindReadsOutOfBounds> {
        for (entry, stored) in self.iter(kind).enumerate() {
            if stored.len() < name.len() {
                return Some(FindReadsOutOfBounds {
                    entry,
                    entry_len: stored.len(),
                });
            }
            if &stored[..name.len()] == name {
                return None;
            }
        }
        None
    }

    /// `bcmp_resource_discovery_add_resource`.
    ///
    /// Refuses a name [`Self::find`] matches, which is the C's `BmEAGAIN` —
    /// and so refuses names that are prefixes of ones already stored.
    ///
    /// # Errors
    ///
    /// [`ResourceAddError::AlreadyPresent`] if the list already covers `name`,
    /// [`ResourceAddError::Full`] if there is no room for it.
    pub fn add(&mut self, name: &[u8], kind: ResourceType) -> Result<(), ResourceAddError> {
        if self.find(name, kind) {
            return Err(ResourceAddError::AlreadyPresent);
        }
        if self.len == N || name.len() > NAME {
            return Err(ResourceAddError::Full);
        }
        let entry = &mut self.entries[self.len];
        entry.kind = kind;
        entry.len = name.len();
        entry.name[..name.len()].copy_from_slice(name);
        self.len += 1;
        Ok(())
    }

    /// Bytes [`Self::encode_reply`] will write —
    /// `bcmp_resource_compute_list_size` over both lists, plus the head.
    #[must_use]
    pub fn reply_len(&self) -> usize {
        ResourceTableReply::HEADER_LEN
            + self.entries[..self.len]
                .iter()
                .map(|entry| Resource::HEADER_LEN + entry.len)
                .sum::<usize>()
    }

    /// Build the `0x0B` body this node would answer with —
    /// `bcmp_resource_discovery_get_local_resources`, and the body
    /// `bcmp_process_resource_discovery_request` transmits.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than
    /// [`Self::reply_len`].
    pub fn encode_reply(&self, buf: &mut [u8], node_id: u64) -> Result<usize, BmWireError> {
        encode_resource_table_reply(
            buf,
            node_id,
            self.iter(ResourceType::Publisher),
            self.iter(ResourceType::Subscriber),
        )
    }
}

// ---------------------------------------------------------------------------
// `RESOURCE_REQUEST_LIST`
// ---------------------------------------------------------------------------

/// `bcmp_resource_discovery_send_request`'s `fp` argument, as a choice rather
/// than a pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceRequestKind {
    /// `fp == NULL`. The reply is matched, consumed and printed with
    /// `bm_debug`; nothing reaches the application.
    Ignore,
    /// `fp != NULL`. The reply reaches the caller.
    Report,
}

/// What `bcmp_process_resource_discovery_reply` made of a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceReplyOutcome {
    /// `repl->node_id != ip_to_nodeid(data.src)`. The C returns `BmOK` having
    /// done nothing — it does not even look at the request list.
    ///
    /// The one correlation in BCMP that compares the body's claim against the
    /// address it arrived from. `bcmp/info.c` and `bcmp/neighbors.c` both
    /// trust the body alone.
    Mismatched,
    /// The claim agreed with the source, but nothing was outstanding for it:
    /// `ll_get_item` returned `BmENODEV` and the following `ll_remove` was a
    /// no-op.
    Unsolicited,
    /// A request was outstanding and is now consumed, with no callback to run.
    Accepted,
    /// A request was outstanding, is now consumed, and the reply is reported.
    Reported,
}

/// `RESOURCE_REQUEST_LIST`: which nodes have been asked for their resource
/// table and have not answered.
///
/// bm_core keeps a `bm_malloc`'d `LL`. This is a fixed-capacity array with the
/// three properties of that list that are observable, which are
/// [`super::info::InfoRequests`]' three:
///
/// * **No de-duplication.** `ll_item_add` appends unconditionally.
/// * **No expiry.** The only removal is a reply, so a node asked and never
///   heard from keeps its entry for the life of the process. `N` is a ceiling
///   bm_core does not have. See divergence #19.
/// * **Thirty-two bit keys.** `LLItem::id` is a `uint32_t`, so the list is
///   keyed on the low half of a 64-bit node id. See divergence #33.
#[derive(Debug, Clone)]
pub struct ResourceRequests<const N: usize> {
    entries: [(u32, ResourceRequestKind); N],
    len: usize,
}

impl<const N: usize> Default for ResourceRequests<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ResourceRequests<N> {
    /// An empty list.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [(0, ResourceRequestKind::Ignore); N],
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
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is outstanding.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Most entries the list can hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// The outstanding requests, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = (u32, ResourceRequestKind)> + '_ {
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
    /// as `BmENOMEM` **after** the request has gone out.
    pub fn record(&mut self, target_node_id: u64, kind: ResourceRequestKind) -> bool {
        if self.len == N {
            return false;
        }
        self.entries[self.len] = (Self::key(target_node_id), kind);
        self.len += 1;
        true
    }

    /// `bcmp_process_resource_discovery_reply`: the acceptance test and its
    /// effects.
    ///
    /// `claimed_node_id` is [`ResourceTableReply::node_id`] and
    /// `source_node_id` is `ip_to_nodeid(data.src)`. They must be equal for
    /// the request list to be consulted at all; then `ll_get_item` and
    /// `ll_remove` both take the first entry whose low 32 bits match.
    pub fn accept(&mut self, claimed_node_id: u64, source_node_id: u64) -> ResourceReplyOutcome {
        if claimed_node_id != source_node_id {
            return ResourceReplyOutcome::Mismatched;
        }
        let key = Self::key(source_node_id);
        let Some(index) = self.entries[..self.len].iter().position(|(k, _)| *k == key) else {
            return ResourceReplyOutcome::Unsolicited;
        };
        let kind = self.entries[index].1;
        self.entries.copy_within(index + 1..self.len, index);
        self.len -= 1;
        match kind {
            ResourceRequestKind::Report => ResourceReplyOutcome::Reported,
            ResourceRequestKind::Ignore => ResourceReplyOutcome::Accepted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUB: ResourceType = ResourceType::Publisher;
    const SUB: ResourceType = ResourceType::Subscriber;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(ResourceTableRequest::LEN, 8);
        assert_eq!(Resource::HEADER_LEN, 2);
        assert_eq!(ResourceTableReply::HEADER_LEN, 12);
    }

    /// Divergence #37: an exact match and nothing else, where every other
    /// request type in BCMP takes zero as a broadcast.
    #[test]
    fn a_request_naming_zero_is_for_nobody_but_node_zero() {
        let broadcast = ResourceTableRequest { target_node_id: 0 };
        assert!(!broadcast.is_for(0xC0FF_EE00_1234_5678));
        assert!(
            broadcast.is_for(0),
            "a node at exactly fe80:: has id zero, and answers only this"
        );

        let directed = ResourceTableRequest {
            target_node_id: 0xC0FF_EE00_1234_5678,
        };
        assert!(directed.is_for(0xC0FF_EE00_1234_5678));
        assert!(!directed.is_for(0));
        assert!(
            !directed.is_for(0x0000_0000_1234_5678),
            "matched whole, unlike the list keys"
        );
    }

    #[test]
    fn a_request_round_trips() {
        let request = ResourceTableRequest {
            target_node_id: 0xDEAD_BEEF_1234_5678,
        };
        let mut buf = [0u8; ResourceTableRequest::LEN];
        request.encode(&mut buf).unwrap();
        assert_eq!(buf, 0xDEAD_BEEF_1234_5678u64.to_le_bytes());
        assert_eq!(ResourceTableRequest::decode(&buf).unwrap(), request);
        assert_eq!(
            ResourceTableRequest::decode(&buf[..7]),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn a_reply_with_both_lists_round_trips() {
        let publishers: [&[u8]; 2] = [b"spotter/utc-time", b"sensor/temp"];
        let subscribers: [&[u8]; 1] = [b"button"];

        let mut buf = [0u8; 128];
        let len = encode_resource_table_reply(
            &mut buf,
            0xC0FF_EE00_1234_5678,
            publishers.iter().copied(),
            subscribers.iter().copied(),
        )
        .unwrap();
        // Head, then 2 + len per record.
        assert_eq!(len, 12 + (2 + 16) + (2 + 11) + (2 + 6));

        let reply = ResourceTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.node_id, 0xC0FF_EE00_1234_5678);
        assert_eq!(reply.publisher_count(), 2);
        assert_eq!(reply.subscriber_count(), 1);
        assert!(
            reply
                .publishers()
                .map(|r| r.name)
                .eq(publishers.iter().copied())
        );
        assert!(
            reply
                .subscribers()
                .map(|r| r.name)
                .eq(subscribers.iter().copied())
        );
        assert_eq!(reply.encoded_len(), len);

        let mut again = [0u8; 128];
        assert_eq!(reply.encode(&mut again).unwrap(), len);
        assert_eq!(&again[..len], &buf[..len]);
    }

    /// Publishers come first, and the only thing separating the two halves is
    /// `num_pubs` — so the same bytes read differently if the counts change.
    #[test]
    fn the_split_between_the_halves_is_the_count_alone() {
        let names: [&[u8]; 3] = [b"a", b"bb", b"ccc"];
        let mut buf = [0u8; 64];
        let len =
            encode_resource_table_reply(&mut buf, 1, names.iter().copied(), core::iter::empty())
                .unwrap();
        assert_eq!(&buf[8..12], &[3, 0, 0, 0]);

        // Move one record across the boundary without touching the records.
        buf[8..10].copy_from_slice(&2u16.to_le_bytes());
        buf[10..12].copy_from_slice(&1u16.to_le_bytes());
        let reply = ResourceTableReply::decode(&buf[..len]).unwrap();
        assert!(reply.publishers().map(|r| r.name).eq([&b"a"[..], b"bb"]));
        assert!(reply.subscribers().map(|r| r.name).eq([&b"ccc"[..]]));
    }

    #[test]
    fn an_empty_table_is_just_the_head() {
        let mut buf = [0u8; 16];
        let len =
            encode_resource_table_reply(&mut buf, 7, core::iter::empty(), core::iter::empty())
                .unwrap();
        assert_eq!(len, ResourceTableReply::HEADER_LEN);
        let reply = ResourceTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.publisher_count(), 0);
        assert_eq!(reply.subscriber_count(), 0);
        assert_eq!(reply.publishers().count(), 0);
        assert_eq!(reply.subscribers().count(), 0);
    }

    /// A zero-length name is a record of two bytes, and the C stores one
    /// happily — `bm_malloc(sizeof(BcmpResource) + 0)`.
    #[test]
    fn a_zero_length_name_is_a_record_of_its_own() {
        let names: [&[u8]; 2] = [b"", b"x"];
        let mut buf = [0u8; 32];
        let len =
            encode_resource_table_reply(&mut buf, 0, names.iter().copied(), core::iter::empty())
                .unwrap();
        assert_eq!(len, 12 + 2 + 3);
        let reply = ResourceTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.publisher_count(), 2);
        assert!(reply.publishers().map(|r| r.name).eq([&b""[..], b"x"]));
    }

    /// Both counts and every record length are attacker-chosen, and the C
    /// multiplies none of it out against the size of the message that arrived.
    #[test]
    fn declared_records_are_checked_against_the_buffer() {
        let mut body = [0u8; ResourceTableReply::HEADER_LEN + 4];
        body[8..10].copy_from_slice(&1u16.to_le_bytes());
        body[12..14].copy_from_slice(&2u16.to_le_bytes()); // a two-byte name
        assert!(ResourceTableReply::decode(&body).is_ok());

        body[12..14].copy_from_slice(&3u16.to_le_bytes()); // three, and two arrived
        assert_eq!(
            ResourceTableReply::decode(&body),
            Err(BmWireError::Truncated)
        );

        // A second record that is not there at all.
        body[12..14].copy_from_slice(&2u16.to_le_bytes());
        body[10..12].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            ResourceTableReply::decode(&body),
            Err(BmWireError::Truncated)
        );

        // The worst case: both counts saturated on a head-only body. The C
        // would walk 131 070 records out of a message that carried twelve
        // bytes.
        let mut minimal = [0u8; ResourceTableReply::HEADER_LEN];
        minimal[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        minimal[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            ResourceTableReply::decode(&minimal),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn trailing_bytes_past_the_records_are_ignored() {
        let mut buf = [0u8; 64];
        let len = encode_resource_table_reply(
            &mut buf,
            1,
            [&b"topic"[..]].iter().copied(),
            core::iter::empty(),
        )
        .unwrap();
        buf[len..len + 8].fill(0xA5);
        let reply = ResourceTableReply::decode(&buf[..len + 8]).unwrap();
        assert_eq!(reply.encoded_len(), len);
        assert_eq!(reply.publisher_count(), 1);
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..ResourceTableReply::HEADER_LEN {
            assert_eq!(
                ResourceTableReply::decode(&[0u8; ResourceTableReply::HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
        }
        let mut tiny = [0u8; 4];
        assert_eq!(
            encode_resource_table_reply(
                &mut tiny,
                0,
                core::iter::empty::<&[u8]>(),
                core::iter::empty::<&[u8]>()
            ),
            Err(BmWireError::Truncated)
        );
        let mut head_only = [0u8; ResourceTableReply::HEADER_LEN];
        assert_eq!(
            encode_resource_table_reply(
                &mut head_only,
                0,
                [&b"x"[..]].iter().copied(),
                core::iter::empty()
            ),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn a_table_keeps_the_two_lists_in_insertion_order() {
        let mut table = ResourceTable::<8, 16>::new();
        table.add(b"pub/one", PUB).unwrap();
        table.add(b"sub/one", SUB).unwrap();
        table.add(b"pub/two", PUB).unwrap();

        assert_eq!(table.len(), 3);
        assert_eq!(table.count(PUB), 2);
        assert_eq!(table.count(SUB), 1);
        assert!(table.iter(PUB).eq([&b"pub/one"[..], b"pub/two"]));
        assert!(table.iter(SUB).eq([&b"sub/one"[..]]));
    }

    /// The two lists are independent, so the same name can be in both.
    #[test]
    fn the_same_name_can_be_published_and_subscribed() {
        let mut table = ResourceTable::<8, 16>::new();
        table.add(b"both", PUB).unwrap();
        table.add(b"both", SUB).unwrap();
        assert_eq!(table.count(PUB), 1);
        assert_eq!(table.count(SUB), 1);
        assert_eq!(
            table.add(b"both", PUB),
            Err(ResourceAddError::AlreadyPresent)
        );
    }

    /// Divergence #38, the defined half: `memcmp` runs for the needle's
    /// length, so a shorter needle matches a longer entry.
    #[test]
    fn find_matches_a_prefix_of_a_stored_name() {
        let mut table = ResourceTable::<8, 16>::new();
        table.add(b"sensor/temp", PUB).unwrap();

        assert!(table.find(b"sensor/temp", PUB));
        assert!(table.find(b"sensor", PUB), "a prefix matches");
        assert!(table.find(b"s", PUB));
        assert!(table.find(b"", PUB), "and so does nothing at all");
        assert!(!table.find(b"sensor/humid", PUB));
        assert!(!table.find(b"sensor", SUB), "the other list is untouched");

        // Which means `add` refuses a name that is not in the list.
        assert_eq!(
            table.add(b"sensor", PUB),
            Err(ResourceAddError::AlreadyPresent)
        );
        assert_eq!(table.count(PUB), 1);
    }

    /// An empty name at the head of a list makes every later `find` over-read,
    /// because `memcmp` is given the needle's length and the head is two
    /// bytes long.
    #[test]
    fn find_reports_where_the_c_would_read_out_of_bounds() {
        let mut table = ResourceTable::<8, 16>::new();
        table.add(b"ab", PUB).unwrap();

        assert_eq!(table.find_over_reads(b"ab", PUB), None);
        assert_eq!(table.find_over_reads(b"a", PUB), None);
        assert_eq!(
            table.find_over_reads(b"abc", PUB),
            Some(FindReadsOutOfBounds {
                entry: 0,
                entry_len: 2
            })
        );
        assert!(
            !table.find(b"abc", PUB),
            "the port declines to guess at the undefined half"
        );

        // A match earlier in the list stops the walk before the short entry.
        table.add(b"zz", PUB).unwrap();
        assert_eq!(
            table.find_over_reads(b"zzz", PUB),
            Some(FindReadsOutOfBounds {
                entry: 0,
                entry_len: 2
            }),
            "the walk reaches `ab` first"
        );
        let mut ordered = ResourceTable::<8, 16>::new();
        ordered.add(b"long-enough", PUB).unwrap();
        ordered.add(b"ab", PUB).unwrap();
        assert_eq!(
            ordered.find_over_reads(b"long", PUB),
            None,
            "`long-enough` matches, so `ab` is never read"
        );
    }

    #[test]
    fn the_ceilings_report_rather_than_truncate() {
        let mut table = ResourceTable::<2, 4>::new();
        assert_eq!(table.capacity(), 2);
        assert_eq!(table.name_capacity(), 4);
        assert_eq!(table.add(b"12345", PUB), Err(ResourceAddError::Full));
        table.add(b"1234", PUB).unwrap();
        table.add(b"1234", SUB).unwrap();
        assert_eq!(table.add(b"x", PUB), Err(ResourceAddError::Full));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn a_table_encodes_the_reply_it_would_answer_with() {
        let mut table = ResourceTable::<8, 32>::new();
        table.add(b"pub/a", PUB).unwrap();
        table.add(b"sub/bb", SUB).unwrap();
        table.add(b"pub/ccc", PUB).unwrap();

        let mut buf = [0u8; 64];
        let len = table.encode_reply(&mut buf, 0xAABB).unwrap();
        assert_eq!(len, table.reply_len());
        assert_eq!(len, 12 + (2 + 5) + (2 + 7) + (2 + 6));

        let reply = ResourceTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.node_id, 0xAABB);
        assert!(
            reply
                .publishers()
                .map(|r| r.name)
                .eq([&b"pub/a"[..], b"pub/ccc"])
        );
        assert!(reply.subscribers().map(|r| r.name).eq([&b"sub/bb"[..]]));

        let mut tiny = [0u8; 16];
        assert_eq!(
            table.encode_reply(&mut tiny, 0),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn a_fresh_request_list_is_empty() {
        let list = ResourceRequests::<4>::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert_eq!(list.capacity(), 4);
        assert!(!list.contains(0));
    }

    /// The reply must claim the node it came from. Nothing else in BCMP
    /// compares the two.
    #[test]
    fn a_reply_whose_claim_disagrees_with_its_source_is_ignored() {
        let mut list = ResourceRequests::<4>::new();
        assert!(list.record(0xAA, ResourceRequestKind::Report));
        assert_eq!(
            list.accept(0xAA, 0xBB),
            ResourceReplyOutcome::Mismatched,
            "the request list is not even consulted"
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Reported);
        assert!(list.is_empty());
    }

    #[test]
    fn a_reply_nothing_asked_for_is_unsolicited() {
        let mut list = ResourceRequests::<4>::new();
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Unsolicited);
        assert!(list.record(0xBB, ResourceRequestKind::Ignore));
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Unsolicited);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn a_request_without_a_callback_is_accepted_and_dropped() {
        let mut list = ResourceRequests::<4>::new();
        assert!(list.record(0xAA, ResourceRequestKind::Ignore));
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Accepted);
        assert!(list.is_empty());
    }

    /// Divergence #19's shape: asking twice leaves two entries, and it takes
    /// two replies to clear them. The first entry answers first.
    #[test]
    fn asking_twice_needs_answering_twice() {
        let mut list = ResourceRequests::<4>::new();
        assert!(list.record(0xAA, ResourceRequestKind::Report));
        assert!(list.record(0xAA, ResourceRequestKind::Ignore));
        assert_eq!(list.len(), 2);
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Reported);
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Accepted);
        assert_eq!(list.accept(0xAA, 0xAA), ResourceReplyOutcome::Unsolicited);
    }

    /// Divergence #33: `LLItem::id` holds half a node id, so two nodes sharing
    /// their low 32 bits share an entry.
    #[test]
    fn the_request_list_is_keyed_on_half_an_id() {
        assert_eq!(
            ResourceRequests::<4>::key(0xDEAD_BEEF_1234_5678),
            0x1234_5678
        );
        let mut list = ResourceRequests::<4>::new();
        assert!(list.record(0xDEAD_BEEF_55AA_0011, ResourceRequestKind::Report));
        assert!(list.contains(0x0000_0000_55AA_0011));
        assert_eq!(
            list.accept(0x0000_0000_55AA_0011, 0x0000_0000_55AA_0011),
            ResourceReplyOutcome::Reported,
            "another node's reply answers the request"
        );
    }

    #[test]
    fn a_full_request_list_records_nothing() {
        let mut list = ResourceRequests::<2>::new();
        assert!(list.record(1, ResourceRequestKind::Ignore));
        assert!(list.record(2, ResourceRequestKind::Ignore));
        assert!(!list.record(3, ResourceRequestKind::Ignore));
        assert_eq!(list.len(), 2);
        assert_eq!(list.accept(3, 3), ResourceReplyOutcome::Unsolicited);
    }
}
