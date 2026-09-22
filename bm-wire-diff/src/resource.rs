//! Differential comparator for resource discovery — `bm_stack`'s `0x0A`/`0x0B`
//! against `bcmp/resource_discovery.c`.
//!
//! A sequence of adds, finds, requests and arriving frames is applied to
//! bm_core's stack and to a [`bm_stack::Node`], and after every step four
//! things are compared:
//!
//! | What | C side | Rust side |
//! |---|---|---|
//! | The two lists | `bcmp_resource_discovery_get_num_resources` and `_find_resource` | [`bm_wire::bcmp::resource::ResourceTable`] |
//! | The reply a `0x0A` provokes | the `0x0B` frames the oracle transmits | [`bm_stack::Owed::reply`] |
//! | The request `send_request` provokes | the `0x0A` frames the oracle transmits | [`bm_stack::Node::request_resource_table`] |
//! | The reply callback | `bcmp_resource_discovery_send_request`'s `fp(repl)` | [`bm_stack::Event::ResourceTable`] |
//!
//! # The divergence this pins
//!
//! **#37.** `bcmp_process_resource_discovery_request` breaks unless
//! `target_node_id` is an exact match, so a `0x0A` naming zero is answered by
//! nobody — where the same request to `bcmp/info.c`, `bcmp/ping.c` or
//! `bcmp/neighbors.c` is answered by everybody. [`TARGETS`] carries zero for
//! that, and the `0x0B` comparison below is what proves it: both nodes stay
//! silent.
//!
//! # `PUB_LIST` and `SUB_LIST` are forever, and are not empty to begin with
//!
//! There is no remove, no clear and no deinit — `bcmp_resource_discovery_init`
//! is the only thing that empties them, and it drops the whole chain on the
//! floor and creates two fresh semaphores. Nor do they start empty:
//! `bm_shim_stack_init` brings `metrics_service_init` up, which subscribes to
//! `<node id>/metrics/req` through `bm_sub`. So the oracle's two lists are
//! process-global, **monotone**, and seeded by somebody else, which decides
//! four things about this comparator:
//!
//! * The names a step may use come from a fixed pool, [`NAMES`]. A long run
//!   converges on both lists holding every addable one, after which every
//!   `Add` is refused by both sides — a converging comparison, but never a
//!   vacuous one: the `0x0B` reply the full table produces is still compared
//!   byte for byte on every [`Step::Incoming`].
//! * [`Model`] is process-global too, and [`check`] rebuilds the port's table
//!   from it rather than starting empty.
//! * The growth phase is covered by the seeds a fresh process replays, and by
//!   the unit tests in `bm_wire::bcmp::resource` and `bm-stack`'s
//!   `tests/node.rs`, which own their state outright.
//! * [`Model`] is *read out of the oracle* the first time it is needed, by
//!   `bcmp_resource_discovery_get_local_resources`, rather than starting
//!   empty. That accessor is also compared against the port's encoder on every
//!   step: it is the same two functions the `0x0B` reply is built from.
//!
//! # Divergence #38 is the domain limit
//!
//! `bcmp_resource_discovery_find_resource_priv` compares the **needle's**
//! length against each stored entry, ignoring `cur->resource_len`, so a needle
//! longer than an entry the walk reaches reads past that entry's allocation.
//! Two rules keep every `memcmp` the C performs inside its own allocation:
//!
//! 1. every needle is at most [`RESOURCE_LEN`] bytes, and
//! 2. an `Add` of a *shorter* name is performed only when [`Model`] says it
//!    will be refused — so nothing below [`RESOURCE_LEN`] is ever stored, and
//!    rule 1 cannot be violated later. The entry `metrics_service_init` leaves
//!    behind is 28 bytes, comfortably above the line.
//!
//! The lists being monotone is why rule 2 has to look ahead: one short entry
//! would make every full-length needle undefined for the rest of the process.
//! [`check`] asserts the invariant rather than trusting it.
//!
//! The *defined* half of #38 is compared in full, and is the interesting half:
//! a needle that is a strict prefix of a stored name matches, so
//! `bcmp_resource_discovery_add_resource` refuses names that are not in the
//! list at all.
//!
//! # This target needs fork mode
//!
//! `RESOURCE_REQUEST_LIST` never expires an entry, as `INFO_REQUEST_LIST` does
//! not (divergence #19). [`check`] answers everything outstanding at the
//! *start* of each seed rather than the end, so a seed that panics does not
//! poison the next one — but the two lists above a panic leaves behind are
//! still process state, so `cargo fuzz run resource -- -fork=1`.

use std::sync::{Mutex, MutexGuard, OnceLock};

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_stack::node::{INFO_REQUESTS_DEFAULT, PING_PAYLOAD_BYTES};
use bm_stack::{Event, Node, SoftRtc};
use bm_wire::bcmp::info::CACHED_STRING_BYTES;
use bm_wire::bcmp::resource::{
    ResourceAddError, ResourceRequestKind, ResourceTableRequest, ResourceType,
    encode_resource_table_reply,
};
use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::l2;
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::ll::LinkModel;
use crate::stack::{
    Captured, NUM_PORTS, OracleIdentity, captured_message_type, drain, inject, oracle,
    pump_until_quiet, tick_count,
};

/// The node ids a `0x0A` may name, and [`Step::Request`] may ask for.
///
/// Index 1 is the oracle's own, the only id a `0x0A` can name and be answered.
/// Index 0 is zero, which divergence #37 makes a dead letter rather than a
/// broadcast. Index 3 shares its low 32 bits with index 2, so the two are one
/// entry as far as `RESOURCE_REQUEST_LIST` is concerned — divergence #33 — and
/// two different targets to the responder, which compares whole.
pub const TARGETS: &[u64] = &[
    0,
    crate::stack::NODE_ID,
    0x0000_0000_55AA_0011,
    0xDEAD_BEEF_55AA_0011,
];

/// The node ids an injected frame may come from, or claim in its body.
///
/// Indices 1 and 3 are [`TARGETS`] 2 and 3, so a peer's reply can answer a
/// request; index 3 shares its low 32 bits with index 1, which is what
/// separates a 32-bit list key from a 64-bit comparison.
///
/// **None of them is the oracle's own id.** A frame L2 relays keeps the
/// sender's source address, so a peer calling itself by the oracle's id would
/// be indistinguishable from something the oracle built, and [`is_ours`] is
/// how the two are told apart. A harness limitation, not a divergence: nothing
/// downstream of `bcmp_process_resource_discovery_reply` reads the source for
/// anything but the comparison against `repl->node_id`, which [`Step::Reply`]
/// varies directly.
pub const PEER_IDS: &[u64] = &[
    0,
    0x0000_0000_55AA_0011,
    0x0000_0000_55AA_0022,
    0xDEAD_BEEF_55AA_0011,
];

/// The id a [`Step::Incoming`] request appears to come from. The responder
/// never reads it: `bcmp_tx(data.dst, ...)` answers the *destination*, which
/// is `FF02::1` either way.
const REQUESTER_ID: u64 = PEER_IDS[2];

/// Longest needle a step may use, and the shortest name a list may come to
/// hold.
///
/// Every entry in either list is at least this long — the four storable
/// [`NAMES`] are exactly this, and the one `metrics_service_init` leaves
/// behind is longer — which is what keeps `find_resource_priv`'s `memcmp`
/// inside its own allocation. See the module docs.
pub const RESOURCE_LEN: usize = 12;

/// The resource names a step may name.
///
/// The first four are [`RESOURCE_LEN`] bytes and may be stored. The rest are
/// shorter and are only ever *searched* for, or added in the knowledge that
/// the add will be refused:
///
/// | Name | Length | What it is |
/// |---|---|---|
/// | `spotter/time` | 12 | storable |
/// | `sensor/temp1` | 12 | storable |
/// | `sensor/temp2` | 12 | storable |
/// | `nothing-like` | 12 | storable, and a prefix of nothing |
/// | `spotter` | 7 | a strict prefix of `spotter/time` |
/// | `sensor/` | 7 | a strict prefix of two of them |
/// | *(empty)* | 0 | matches whatever is at the head of a list |
pub const NAMES: &[&[u8]] = &[
    b"spotter/time",
    b"sensor/temp1",
    b"sensor/temp2",
    b"nothing-like",
    b"spotter",
    b"sensor/",
    b"",
];

/// Most steps a single input may carry.
pub const MAX_STEPS: usize = 24;

/// How many resources the port's node holds across both lists.
///
/// Four of [`NAMES`] are storable and there are two lists, so eight is every
/// state the pool can reach; `metrics_service_init`'s subscription makes nine,
/// and ten leaves the ceiling out of reach. [`check`] asserts it stays there.
/// bm_core `bm_malloc`s each resource and has no ceiling at all.
pub const RESOURCES: usize = 10;

/// Longest name the port's table keeps.
///
/// `<node id>/metrics/req` is 28 bytes, so [`RESOURCE_LEN`] is not the
/// ceiling: this is sized for what the stack's own subscriptions bring.
pub const RESOURCE_NAME: usize = 48;

/// How many unanswered resource-table requests the port's node remembers.
///
/// `RESOURCE_REQUEST_LIST` is unbounded and never expires an entry, so a
/// ceiling is a difference in kind. A step adds at most one entry and
/// [`check`] empties the list before each seed, so one more than [`MAX_STEPS`]
/// puts the port's ceiling out of reach of any seed.
pub const REQUEST_CAPACITY: usize = MAX_STEPS + 1;

/// The port's node, sized as above.
type ResourceNode = Node<
    OracleIdentity,
    SoftRtc,
    4,
    4,
    PING_PAYLOAD_BYTES,
    INFO_REQUESTS_DEFAULT,
    CACHED_STRING_BYTES,
    RESOURCES,
    RESOURCE_NAME,
    REQUEST_CAPACITY,
>;

/// The record lists a `0x0B` arriving from a peer may carry, as index pairs
/// into [`NAMES`].
///
/// Only the storable names, so a reply reads the way a real node's would. What
/// is being compared is the walk, so the shapes vary in both counts.
const REPLY_SHAPES: &[(&[usize], &[usize])] = &[
    (&[], &[]),
    (&[0], &[]),
    (&[], &[1]),
    (&[0, 1], &[2]),
    (&[3], &[0, 1, 2]),
];

/// One thing that happens to the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A resource is advertised — `bcmp_resource_discovery_add_resource`.
    Add {
        /// Index into [`NAMES`].
        name: u8,
        /// `SUB` rather than `PUB`.
        subscriber: bool,
    },
    /// A resource is looked up — `bcmp_resource_discovery_find_resource`.
    Find {
        /// Index into [`NAMES`].
        name: u8,
        /// `SUB` rather than `PUB`.
        subscriber: bool,
    },
    /// The node asks someone for their resource table —
    /// `bcmp_resource_discovery_send_request`.
    Request {
        /// Index into [`TARGETS`].
        node: u8,
        /// Whether the request carries a callback. `false` is `fp == NULL`.
        report: bool,
    },
    /// A `0x0A` arrives, which only an exact match on the target is answered.
    Incoming {
        /// Index into [`TARGETS`] for the `target_node_id` in the body.
        target: u8,
        /// Ingress port, 1..=[`NUM_PORTS`].
        port: u8,
    },
    /// A `0x0B` arrives.
    Reply {
        /// Index into [`PEER_IDS`] for the frame's source address.
        from: u8,
        /// Index into [`PEER_IDS`] for the `node_id` in the *body*, which must
        /// equal the source for the reply to be looked at.
        claims: u8,
        /// Ingress port, 1..=[`NUM_PORTS`].
        port: u8,
        /// Index into [`REPLY_SHAPES`].
        shape: u8,
    },
}

impl Step {
    fn clamp(&mut self) {
        match self {
            Self::Add { name, .. } | Self::Find { name, .. } => *name %= NAMES.len() as u8,
            Self::Request { node, .. } => *node %= TARGETS.len() as u8,
            Self::Incoming { target, port } => {
                *target %= TARGETS.len() as u8;
                *port = port.wrapping_sub(1) % NUM_PORTS + 1;
            }
            Self::Reply {
                from,
                claims,
                port,
                shape,
            } => {
                *from %= PEER_IDS.len() as u8;
                *claims %= PEER_IDS.len() as u8;
                *port = port.wrapping_sub(1) % NUM_PORTS + 1;
                *shape %= REPLY_SHAPES.len() as u8;
            }
        }
    }
}

/// A sequence of steps applied to both nodes.
#[derive(Debug, Clone)]
pub struct ResourceInput {
    /// The steps, capped at [`MAX_STEPS`] by [`Domain`].
    pub steps: Vec<Step>,
}

impl<'a> Arbitrary<'a> for ResourceInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut steps = Vec::new();
        while !u.is_empty() && steps.len() < MAX_STEPS {
            steps.push(arbitrary_step(u)?);
        }
        let mut input = Self { steps };
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        Self::arbitrary(&mut u)
    }
}

fn arbitrary_step(u: &mut Unstructured<'_>) -> Result<Step> {
    let mut step = match u.int_in_range(0..=4u8)? {
        0 => Step::Add {
            name: u.arbitrary()?,
            subscriber: u.arbitrary()?,
        },
        1 => Step::Find {
            name: u.arbitrary()?,
            subscriber: u.arbitrary()?,
        },
        2 => Step::Request {
            node: u.arbitrary()?,
            report: u.arbitrary()?,
        },
        3 => Step::Incoming {
            target: u.arbitrary()?,
            port: u.arbitrary()?,
        },
        _ => Step::Reply {
            from: u.arbitrary()?,
            claims: u.arbitrary()?,
            port: u.arbitrary()?,
            shape: u.arbitrary()?,
        },
    };
    step.clamp();
    Ok(step)
}

impl Domain for ResourceInput {
    fn clamp_to_domain(&mut self) {
        self.steps.truncate(MAX_STEPS);
        for step in &mut self.steps {
            step.clamp();
        }
    }
}

// ---------------------------------------------------------------------------
// What the C's callback was handed
// ---------------------------------------------------------------------------

/// One reply, as either side of the comparison saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reported {
    /// `repl->node_id`, which had to equal the source address to get here.
    pub node_id: u64,
    /// The publisher names, in order.
    pub publishers: Vec<Vec<u8>>,
    /// The subscriber names, in order.
    pub subscribers: Vec<Vec<u8>>,
}

static REPORTED: Mutex<Vec<Reported>> = Mutex::new(Vec::new());

/// `bcmp_resource_discovery_send_request`'s `fp`, handed the
/// `BcmpResourceTableReply` still inside the received frame.
///
/// # Safety
///
/// `arg` is bm_core's `BcmpResourceTableReply *`. The declared record lengths
/// are trusted here exactly as the C's own `bm_debug` walk trusts them; this
/// comparator only ever injects replies whose records fit the body they
/// arrived in, which is the domain limit divergence #14 describes.
unsafe extern "C" fn on_reply(arg: *mut core::ffi::c_void) {
    let reply = arg.cast::<bm_wire_sys::BcmpResourceTableReply>();
    let (node_id, num_pubs, num_subs) =
        unsafe { ((*reply).node_id, (*reply).num_pubs, (*reply).num_subs) };
    let mut at = unsafe { (*reply).resource_list.as_ptr() };

    // The walk `bcmp_process_resource_discovery_reply` does: each record's own
    // declared length advances the cursor to the next one.
    let mut read = |count: u16| -> Vec<Vec<u8>> {
        (0..count)
            .map(|_| unsafe {
                let record = at.cast::<bm_wire_sys::BcmpResource>();
                let len = usize::from((*record).resource_len);
                let name =
                    core::slice::from_raw_parts((*record).resource.as_ptr().cast::<u8>(), len)
                        .to_vec();
                at = at.add(core::mem::size_of::<bm_wire_sys::BcmpResource>() + len);
                name
            })
            .collect()
    };
    let publishers = read(num_pubs);
    let subscribers = read(num_subs);

    REPORTED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Reported {
            node_id,
            publishers,
            subscribers,
        });
}

fn take_reported() -> Vec<Reported> {
    std::mem::take(&mut *REPORTED.lock().unwrap_or_else(|p| p.into_inner()))
}

// ---------------------------------------------------------------------------
// The comparator's belief about the module's three statics
// ---------------------------------------------------------------------------

/// `PUB_LIST`, `SUB_LIST` and `RESOURCE_REQUEST_LIST`, as this comparator
/// believes them to be.
///
/// Deliberately a second implementation rather than a call into
/// [`bm_wire::bcmp::resource`]: it is what says *why* each side did what it
/// did, and a wrong belief here is a test failure rather than a silent
/// agreement.
///
/// Process-global, because the C's three are: none of them has a deinit. The
/// request list is emptied at the start of every seed; the other two are not
/// emptied at all.
#[derive(Debug, Default)]
pub struct Model {
    /// `PUB_LIST`, oldest first.
    pub publishers: Vec<Vec<u8>>,
    /// `SUB_LIST`, oldest first.
    pub subscribers: Vec<Vec<u8>>,
    /// `RESOURCE_REQUEST_LIST`: `(key, carries a callback)` in list order,
    /// keyed on the low 32 bits of the target (divergence #33).
    requests: Vec<(u32, bool)>,
    /// The link structure behind it, so an `ll_item_add` through a dangling
    /// tail is declined rather than performed. See [`LinkModel`] and
    /// divergence #20.
    links: LinkModel,
}

impl Model {
    fn list(&self, kind: ResourceType) -> &Vec<Vec<u8>> {
        match kind {
            ResourceType::Publisher => &self.publishers,
            ResourceType::Subscriber => &self.subscribers,
        }
    }

    fn list_mut(&mut self, kind: ResourceType) -> &mut Vec<Vec<u8>> {
        match kind {
            ResourceType::Publisher => &mut self.publishers,
            ResourceType::Subscriber => &mut self.subscribers,
        }
    }

    /// `bcmp_resource_discovery_find_resource_priv`: `memcmp` for the
    /// *needle's* length, first match wins.
    fn find(&self, name: &[u8], kind: ResourceType) -> bool {
        self.list(kind)
            .iter()
            .any(|stored| stored.len() >= name.len() && &stored[..name.len()] == name)
    }

    /// Whether that walk would read past an entry's allocation.
    fn over_reads(&self, name: &[u8], kind: ResourceType) -> bool {
        for stored in self.list(kind) {
            if stored.len() < name.len() {
                return true;
            }
            if &stored[..name.len()] == name {
                return false;
            }
        }
        false
    }

    /// `bcmp_resource_discovery_add_resource`. Reports whether it appended.
    fn add(&mut self, name: &[u8], kind: ResourceType) -> bool {
        if self.find(name, kind) {
            return false;
        }
        self.list_mut(kind).push(name.to_vec());
        true
    }

    /// Whether the C can run this step with every `memcmp` inside its own
    /// allocation, and without leaving a list that could not.
    ///
    /// The second clause is what the lists being monotone forces: one entry
    /// shorter than [`RESOURCE_LEN`] would make every full-length needle
    /// undefined for the rest of the process, so an `Add` of a short name is
    /// performed only when it is going to be refused.
    fn step_is_defined(&self, step: &Step) -> bool {
        match *step {
            Step::Add { name, subscriber } => {
                let (name, kind) = (NAMES[usize::from(name)], kind_of(subscriber));
                !self.over_reads(name, kind)
                    && (name.len() >= RESOURCE_LEN || self.find(name, kind))
            }
            Step::Find { name, subscriber } => {
                !self.over_reads(NAMES[usize::from(name)], kind_of(subscriber))
            }
            // Nothing else touches either list.
            Step::Request { .. } | Step::Incoming { .. } | Step::Reply { .. } => true,
        }
    }

    /// `bcmp_resource_discovery_send_request`'s `ll_item_add`.
    fn request_recorded(&mut self, target_node_id: u64, report: bool) {
        self.links.add();
        self.requests.push((target_node_id as u32, report));
        debug_assert_eq!(self.requests.len(), self.links.len());
    }

    /// Whether an `ll_item_add` right now would write through a dangling
    /// `LL::tail`, so no step may provoke a request until the list has
    /// emptied.
    fn request_add_is_undefined(&self) -> bool {
        self.links.add_is_undefined()
    }

    /// `ll_get_item` then `ll_remove`, both taking the first entry with the
    /// key. Reports whether a callback ran.
    fn reply_accepted(&mut self, claimed: u64, source: u64) -> bool {
        if claimed != source {
            return false;
        }
        let key = source as u32;
        let Some(index) = self.requests.iter().position(|(k, _)| *k == key) else {
            return false;
        };
        let (_, report) = self.requests.remove(index);
        self.links.remove(index);
        report
    }

    /// Whether every stored name is long enough that no needle this
    /// comparator uses can read past it.
    fn names_are_long_enough(&self) -> bool {
        self.publishers
            .iter()
            .chain(&self.subscribers)
            .all(|name| name.len() >= RESOURCE_LEN)
    }

    /// The reply body this table would produce for `node_id`.
    fn reply_body(&self, node_id: u64) -> Vec<u8> {
        let mut body = vec![0u8; 1024];
        let len = encode_resource_table_reply(
            &mut body,
            node_id,
            self.publishers.iter().map(Vec::as_slice),
            self.subscribers.iter().map(Vec::as_slice),
        )
        .expect("the pool cannot fill a kilobyte");
        body.truncate(len);
        body
    }
}

fn kind_of(subscriber: bool) -> ResourceType {
    if subscriber {
        ResourceType::Subscriber
    } else {
        ResourceType::Publisher
    }
}

fn c_kind(kind: ResourceType) -> bm_wire_sys::ResourceType {
    match kind {
        ResourceType::Publisher => bm_wire_sys::ResourceType_PUB,
        ResourceType::Subscriber => bm_wire_sys::ResourceType_SUB,
    }
}

static MODEL: OnceLock<Mutex<Model>> = OnceLock::new();

/// The comparator's belief about the oracle's three statics, which outlive
/// every seed exactly as they do.
///
/// The two resource lists are read out of the oracle the first time this is
/// called, because they are not empty by then: `bm_shim_stack_init` brings
/// `metrics_service_init` up, which subscribes to `<node id>/metrics/req`.
/// `RESOURCE_REQUEST_LIST` is empty at that point — nothing has asked
/// anybody — and [`check`] empties it again before every seed.
///
/// # Panics
///
/// If called before [`oracle`], which is what brings the C up.
fn model() -> MutexGuard<'static, Model> {
    MODEL
        .get_or_init(|| {
            let (publishers, subscribers, _) = oracle_local_resources();
            Mutex::new(Model {
                publishers,
                subscribers,
                ..Model::default()
            })
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// `bcmp_resource_discovery_get_local_resources`, as two lists of names and
/// the bytes the C built them into.
///
/// Those bytes are the `0x0B` body: the function is
/// `bcmp_resource_compute_list_size` and `bcmp_resource_populate_msg_data`,
/// which is what `bcmp_process_resource_discovery_request` answers with, minus
/// the transmission.
///
/// # Panics
///
/// If the C cannot allocate the reply, which it only fails to do out of
/// memory.
fn oracle_local_resources() -> (Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<u8>) {
    // SAFETY: the buffer was built by `populate_msg_data` from the C's own
    // lists, so each record's declared length is the length it copied and the
    // walk below stays inside the allocation. The caller owns it.
    unsafe {
        let reply = bm_wire_sys::bcmp_resource_discovery_get_local_resources();
        assert!(!reply.is_null(), "the oracle could not build its own table");
        let (num_pubs, num_subs) = ((*reply).num_pubs, (*reply).num_subs);
        let start = reply.cast::<u8>();
        let mut at = (*reply).resource_list.as_ptr();

        let mut read = |count: u16| -> Vec<Vec<u8>> {
            (0..count)
                .map(|_| {
                    let record = at.cast::<bm_wire_sys::BcmpResource>();
                    let len = usize::from((*record).resource_len);
                    let name =
                        core::slice::from_raw_parts((*record).resource.as_ptr().cast::<u8>(), len)
                            .to_vec();
                    at = at.add(core::mem::size_of::<bm_wire_sys::BcmpResource>() + len);
                    name
                })
                .collect()
        };
        let publishers = read(num_pubs);
        let subscribers = read(num_subs);

        let len = at.offset_from(start) as usize;
        let bytes = core::slice::from_raw_parts(start, len).to_vec();
        bm_wire_sys::bm_free(reply.cast());
        (publishers, subscribers, bytes)
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// A BCMP frame from `node_id` to `FF02::1`, ready to inject.
fn peer_frame(node_id: u64, message_type: MessageType, body: &[u8]) -> Vec<u8> {
    let payload_len = BCMP_HEADER_LEN + body.len();
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
    frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
    frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
        .copy_from_slice(&(payload_len as u16).to_be_bytes());
    frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
    frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
        .copy_from_slice(&bm_wire::addr::nodeid_to_ip(0xFE80_0000, node_id).0);
    frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
        .copy_from_slice(&BmIpAddr::LINK_LOCAL_MULTICAST.0);
    tx::serialize(&mut frame, message_type, 0, body).expect("frame is sized");
    frame
}

/// A `0x0A` naming `target_node_id`.
fn request_frame(from: u64, target_node_id: u64) -> Vec<u8> {
    let mut body = [0u8; ResourceTableRequest::LEN];
    ResourceTableRequest { target_node_id }
        .encode(&mut body)
        .expect("eight bytes");
    peer_frame(from, MessageType::RESOURCE_TABLE_REQUEST, &body)
}

/// A `0x0B` from `from`, claiming `claims` and carrying [`REPLY_SHAPES`]
/// entry `shape`.
///
/// Every record length matches the bytes that follow it: a lying one has the C
/// walking past the frame, which is not behaviour to compare against.
fn reply_frame(from: u64, claims: u64, shape: usize) -> Vec<u8> {
    let (publishers, subscribers) = REPLY_SHAPES[shape];
    let mut body = vec![0u8; 256];
    let len = encode_resource_table_reply(
        &mut body,
        claims,
        publishers.iter().map(|i| NAMES[*i]),
        subscribers.iter().map(|i| NAMES[*i]),
    )
    .expect("the shapes are small");
    body.truncate(len);
    peer_frame(from, MessageType::RESOURCE_TABLE_REPLY, &body)
}

/// What [`REPLY_SHAPES`] entry `shape` says, as a [`Reported`].
fn expected_report(claims: u64, shape: usize) -> Reported {
    let (publishers, subscribers) = REPLY_SHAPES[shape];
    Reported {
        node_id: claims,
        publishers: publishers.iter().map(|i| NAMES[*i].to_vec()).collect(),
        subscribers: subscribers.iter().map(|i| NAMES[*i].to_vec()).collect(),
    }
}

/// The resource-discovery frames in a batch of captured ones that the oracle
/// *built*, as opposed to relayed.
///
/// `0x0A` appears on the wire twice for one injection — once as L2's relay of
/// what arrived, once as the copy the oracle would have built itself — and
/// only the second is this comparator's business. [`is_ours`] is the
/// separation, and it is why [`PEER_IDS`] excludes the oracle's own id.
fn built_by_the_oracle(frames: &[Captured]) -> Vec<Captured> {
    frames
        .iter()
        .filter(|(_, frame)| {
            matches!(
                captured_message_type(frame),
                Some(MessageType::RESOURCE_TABLE_REQUEST | MessageType::RESOURCE_TABLE_REPLY)
            ) && is_ours(frame)
        })
        .cloned()
        .collect()
}

/// Compare one whole frame, per port, after stamping ours the way L2 would.
fn compare_frames(what: &str, c_copies: &[Captured], ours: &[u8]) {
    let mut ours = ours.to_vec();
    for (port, c_frame) in c_copies {
        let stamped = l2::stamp_egress_port(&mut ours, *port).expect("stampable");
        if c_frame.as_slice() == &*stamped {
            continue;
        }
        let at = c_frame
            .iter()
            .zip(stamped.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(c_frame.len().min(stamped.len()));
        panic!(
            "{what}: the resource discovery frame diverged on port {port} at byte {at}\n  \
             C:        {c_frame:02x?}\n  bm-stack: {stamped:02x?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------

/// Build a node whose two lists are what [`Model`] says the oracle's are.
///
/// The lists are process-global and monotone, so a node that started empty
/// would answer a `0x0A` with a shorter table than the C after the first seed.
fn resource_node(model: &Model) -> ResourceNode {
    let mut node = Node::new(OracleIdentity, SoftRtc::new(), NUM_PORTS);
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    for (kind, names) in [
        (ResourceType::Publisher, &model.publishers),
        (ResourceType::Subscriber, &model.subscribers),
    ] {
        for name in names {
            node.add_resource(name, kind)
                .expect("the model only holds what the pool can produce");
        }
    }
    node
}

/// Answer every request the oracle has outstanding, so
/// `RESOURCE_REQUEST_LIST` is empty when the seed starts.
///
/// Runs at the start rather than the end, so a seed that panics does not
/// poison the next one. Walking the list in order means every removal is a
/// head removal, which is always defined however the last seed left the
/// `previous` pointers.
///
/// A reply claiming the key itself matches whatever the key was recorded from:
/// the list only ever sees the low 32 bits. It also has to *come* from that
/// id, since this is the one exchange that compares the body's claim against
/// the source address.
fn drain_requests(model: &mut Model) {
    while let Some((key, _)) = model.requests.first().copied() {
        let node_id = u64::from(key);
        inject(1, &reply_frame(node_id, node_id, 0));
        model.reply_accepted(node_id, node_id);
    }
    drain();
    take_reported();
    assert!(
        !model.request_add_is_undefined(),
        "an empty RESOURCE_REQUEST_LIST cannot leave a dangling tail"
    );
}

/// Apply the same steps to bm_core's stack and to a [`bm_stack::Node`], and
/// assert the two agree after every one.
///
/// # Panics
///
/// If the lists, the frames or the callbacks ever differ, or if either side
/// departs from [`Model`].
pub fn check(input: &ResourceInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let _guard = oracle();
    let mut model = model();
    drain_requests(&mut model);
    assert!(
        model.names_are_long_enough(),
        "the domain limit rests on every stored name being at least {RESOURCE_LEN} \
         bytes, and the lists hold {:?} / {:?}",
        model.publishers,
        model.subscribers
    );

    let mut node = resource_node(&model);
    drain();
    take_reported();
    assert_lists("before the first step", &model, &node);

    for (index, step) in input.steps.iter().enumerate() {
        let now = tick_count();
        let mut ours: Option<Vec<u8>> = None;
        let mut reported = Vec::new();
        let mut expect_reported = None;

        // Past this point `ll_item_add` would write through a freed pointer,
        // so neither node is asked to make a request. Only a reply, which
        // removes, is still defined. See divergence #20.
        if matches!(step, Step::Request { .. }) && model.request_add_is_undefined() {
            continue;
        }
        // And divergence #38's undefined half.
        if !model.step_is_defined(step) {
            continue;
        }

        match *step {
            Step::Add { name, subscriber } => {
                let (name, kind) = (NAMES[usize::from(name)], kind_of(subscriber));
                let err = unsafe {
                    bm_wire_sys::bcmp_resource_discovery_add_resource(
                        name.as_ptr().cast(),
                        name.len() as u16,
                        c_kind(kind),
                        bm_wire_sys::default_resource_add_timeout_ms,
                    )
                };
                let added = model.add(name, kind);
                assert_eq!(
                    err,
                    if added {
                        bm_wire_sys::BmErr_BmOK
                    } else {
                        bm_wire_sys::BmErr_BmEAGAIN
                    },
                    "step {index} ({step:?}): the oracle's add reported {err}, \
                     and the model expected added={added}"
                );
                assert_eq!(
                    node.add_resource(name, kind),
                    if added {
                        Ok(())
                    } else {
                        Err(ResourceAddError::AlreadyPresent)
                    },
                    "step {index} ({step:?}): the port's add diverged"
                );
            }
            Step::Find { name, subscriber } => {
                let (name, kind) = (NAMES[usize::from(name)], kind_of(subscriber));
                let mut found = false;
                let err = unsafe {
                    bm_wire_sys::bcmp_resource_discovery_find_resource(
                        name.as_ptr().cast(),
                        name.len() as u16,
                        &mut found,
                        c_kind(kind),
                        bm_wire_sys::default_resource_add_timeout_ms,
                    )
                };
                assert_eq!(
                    err,
                    bm_wire_sys::BmErr_BmOK,
                    "step {index} ({step:?}): the oracle refused to look"
                );
                assert_eq!(
                    (node.resources().find(name, kind), model.find(name, kind)),
                    (found, found),
                    "step {index} ({step:?}): the port and the model disagree with \
                     the oracle's found={found}"
                );
            }
            Step::Request {
                node: which,
                report,
            } => {
                let target = TARGETS[usize::from(which)];
                let cb = if report {
                    Some(on_reply as unsafe extern "C" fn(*mut core::ffi::c_void))
                } else {
                    None
                };
                unsafe {
                    assert_eq!(
                        bm_wire_sys::bcmp_resource_discovery_send_request(target, cb),
                        bm_wire_sys::BmErr_BmOK,
                        "step {index}: the oracle refused to ask"
                    );
                }
                pump_until_quiet();
                let kind = if report {
                    ResourceRequestKind::Report
                } else {
                    ResourceRequestKind::Ignore
                };
                ours = node
                    .request_resource_table(now, target, kind)
                    .map(|outbound| outbound.frame().to_vec());
                model.request_recorded(target, report);
            }
            Step::Incoming { target, port } => {
                let frame = request_frame(REQUESTER_ID, TARGETS[usize::from(target)]);
                inject(port, &frame);
                ours = our_reply(&mut node, now, port, &frame, &mut reported);
            }
            Step::Reply {
                from,
                claims,
                port,
                shape,
            } => {
                let (from, claims) = (PEER_IDS[usize::from(from)], PEER_IDS[usize::from(claims)]);
                let frame = reply_frame(from, claims, usize::from(shape));
                inject(port, &frame);
                if model.reply_accepted(claims, from) {
                    expect_reported = Some(expected_report(claims, usize::from(shape)));
                }
                ours = our_reply(&mut node, now, port, &frame, &mut reported);
            }
        }

        // 1. The `0x0B` an incoming request provoked, or 2. the `0x0A` a
        //    request provoked, byte for byte per port. Nothing here advances
        //    the clock, so a heartbeat cannot appear alongside.
        let built = built_by_the_oracle(&drain());
        assert_eq!(
            built.len(),
            usize::from(ours.is_some()) * usize::from(NUM_PORTS),
            "step {index} ({step:?}): the oracle built {} resource frames of its own \
             and we built {}",
            built.len(),
            usize::from(ours.is_some())
        );
        if let Some(our_frame) = &ours {
            compare_frames(&format!("step {index} ({step:?})"), &built, our_frame);
        }

        // 3. The reply callback.
        assert_eq!(
            take_reported(),
            reported,
            "step {index} ({step:?}): the reply callbacks diverged"
        );
        assert_eq!(
            reported,
            expect_reported.into_iter().collect::<Vec<_>>(),
            "step {index} ({step:?}): the model expected something else"
        );

        // And the two lists, against both sides.
        assert_lists(&format!("step {index}"), &model, &node);
    }
}

/// Whether a captured frame was built by the oracle rather than relayed
/// through it — the node id in its source address is the oracle's own.
///
/// The node id rather than the whole address, because L2 stamps the egress
/// port into the source address's third byte on the way out
/// ([`bm_wire::frame::IPV6_INGRESS_EGRESS_PORTS_OFFSET`]) and the node id is
/// the low eight bytes, which it does not touch.
#[must_use]
pub fn is_ours(frame: &[u8]) -> bool {
    frame
        .get(IPV6_SOURCE_ADDRESS_OFFSET + 8..IPV6_SOURCE_ADDRESS_OFFSET + 16)
        .and_then(|id| <[u8; 8]>::try_from(id).ok())
        .is_some_and(|id| u64::from_be_bytes(id) == crate::stack::NODE_ID)
}

/// Run the same frame through our node, returning the reply it wants sent and
/// recording every [`Event::ResourceTable`] on the way.
fn our_reply(
    node: &mut ResourceNode,
    now: u32,
    port: u8,
    frame: &[u8],
    reported: &mut Vec<Reported>,
) -> Option<Vec<u8>> {
    let mut frame = frame.to_vec();
    let owed = node.on_frame_with(now, port, &mut frame, |event| {
        if let Event::ResourceTable { reply, .. } = event {
            reported.push(Reported {
                node_id: reply.node_id,
                publishers: reply.publishers().map(|r| r.name.to_vec()).collect(),
                subscribers: reply.subscribers().map(|r| r.name.to_vec()).collect(),
            });
        }
    });
    owed.reply.map(|outbound| outbound.frame().to_vec())
}

/// Assert both lists are what [`Model`] says, on the port's side and through
/// the oracle's two accessors.
fn assert_lists(what: &str, model: &Model, node: &ResourceNode) {
    // The port has ceilings bm_core does not. Both are sized out of reach; a
    // full one would mean the comparison had stopped being meaningful.
    assert!(
        node.resources().len() < node.resources().capacity(),
        "{what}: the port's resource table filled, and bm_core's cannot"
    );
    assert!(
        node.resource_requests().len() < node.resource_requests().capacity(),
        "{what}: the port's request list filled, and bm_core's cannot"
    );

    for kind in [ResourceType::Publisher, ResourceType::Subscriber] {
        let expected = model.list(kind);
        let mut count = 0u16;
        let err = unsafe {
            bm_wire_sys::bcmp_resource_discovery_get_num_resources(
                &mut count,
                c_kind(kind),
                bm_wire_sys::default_resource_add_timeout_ms,
            )
        };
        assert_eq!(err, bm_wire_sys::BmErr_BmOK, "{what}: oracle count");
        assert_eq!(
            usize::from(count),
            expected.len(),
            "{what}: the oracle's {kind:?} list holds {count}, the model {:?}",
            expected
        );
        assert_eq!(
            node.resources().count(kind),
            count,
            "{what}: the port's {kind:?} count diverged"
        );
        assert!(
            node.resources()
                .iter(kind)
                .eq(expected.iter().map(Vec::as_slice)),
            "{what}: the port's {kind:?} list is not {expected:?}"
        );
    }

    // And the body itself, three ways: what the C's own
    // `compute_list_size`/`populate_msg_data` pair builds, what the port's
    // encoder builds, and what the model says they should both be.
    let (_, _, theirs) = oracle_local_resources();
    let mut ours = vec![0u8; 1024];
    let len = node
        .resources()
        .encode_reply(&mut ours, crate::stack::NODE_ID)
        .expect("the pool cannot fill a kilobyte");
    ours.truncate(len);
    assert_eq!(
        ours, theirs,
        "{what}: the port's local resource table is not the oracle's"
    );
    assert_eq!(
        ours,
        model.reply_body(crate::stack::NODE_ID),
        "{what}: the port's reply body is not what the model would build"
    );
}
