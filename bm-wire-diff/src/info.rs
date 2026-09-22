//! Differential comparator for device-info reply consumption — `bm_stack`'s
//! info cache against `bcmp_process_info_reply` in `bcmp/info.c`.
//!
//! A sequence of heartbeats, device-info replies, explicit requests and clock
//! advances is applied to bm_core's stack and to a [`bm_stack::Node`], and
//! after every step three things are compared:
//!
//! | What | C side | Rust side |
//! |---|---|---|
//! | The cache | each `BcmpNeighbor`'s `info`, `version_str`, `device_name` | [`bm_stack::Node::device_info`] |
//! | The callback | `bcmp_request_info`'s `cb(info)` | [`bm_stack::Event::DeviceInfo`] |
//! | The request it provokes | the `0x04` frames the oracle transmits | [`bm_stack::Owed::reply`] |
//!
//! The third is what makes this more than a cache test: `bcmp_update_neighbor`
//! and `bcmp_process_heartbeat` ask about *different* node ids, and which one
//! a restart names depends on what is in the cache — divergence #34.
//!
//! # Every string is NUL-free, and every length honest
//!
//! bm_core stores each string as a `char *` with no length beside it, so
//! `strlen` is the only way to read one back and a byte past an interior NUL
//! is unobservable. [`STRINGS`] therefore carries no NUL bytes.
//!
//! The declared lengths always match the bytes that arrived, because
//! `populate_neighbor_info` copies per the declaration without consulting
//! `BcmpProcessData.size` — divergence #14. A malformed reply would have the C
//! reading past the frame, which is not behaviour to compare against;
//! [`bm_wire::bcmp::info::DeviceInfoReply::decode`] refuses those and its own
//! unit tests cover them.
//!
//! # This target needs fork mode
//!
//! `INFO_REQUEST_LIST` never expires an entry (divergence #19), so a request
//! that goes unanswered outlives the seed that made it and would then satisfy
//! a later seed's reply. [`check`] answers every request it provoked before
//! returning, which keeps the list empty between seeds — but the list is still
//! process-global state that a crash leaves behind, so
//! `cargo fuzz run info -- -fork=1`.

use std::ffi::CStr;
use std::sync::Mutex;

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_stack::node::PING_PAYLOAD_BYTES;
use bm_stack::{Event, Node, SoftRtc};
use bm_wire::bcmp::info::{DeviceInfoReply, DeviceInfoRequest};
use bm_wire::bcmp::{
    BCMP_HEADER_LEN, BCMP_HEADER_OFFSET, DeviceInfo, Heartbeat, InfoRequestKind, MessageType, tx,
};
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
    Captured, NUM_PORTS, OracleIdentity, captured_message_type, clear_neighbor_table, drain,
    inject, oracle, pump_until_quiet, tick_count,
};

/// Where a BCMP body starts in a frame, past the BCMP header.
const BCMP_BODY_OFFSET: usize = BCMP_HEADER_OFFSET + BCMP_HEADER_LEN;

/// The node ids a step may name.
///
/// Index 3 shares its low 32 bits with index 1. `LLItem::id` is a `uint32_t`
/// and node ids are 64-bit, so those two are the same entry as far as
/// `INFO_REQUEST_LIST` is concerned — divergence #33.
///
/// Zero is included for divergence #18: `bcmp_find_neighbor` never matches it,
/// so a reply claiming node id zero is matched against the request list and
/// then cached nowhere.
pub const NODE_IDS: &[u64] = &[
    0,
    0x0000_0000_55AA_0011,
    0x0000_0000_55AA_0022,
    0xDEAD_BEEF_55AA_0011,
];

/// The `(version string, device name)` pairs a reply may carry.
///
/// No NUL bytes, for the reason the module docs give. The empty pairs matter:
/// `populate_neighbor_info` guards each string with `if (len)`, so a reply
/// declaring zero leaves the previous string in place rather than clearing it.
pub const STRINGS: &[(&[u8], &[u8])] = &[
    (b"", b""),
    (b"0.1.0", b"bm_sbc"),
    (b"1.2.3-rc1", b""),
    (b"", b"dev-kit"),
    (b"a-much-longer-version-string-than-usual", b"n"),
];

/// Longest a single step may push the virtual clock.
///
/// The clock fires every due timer as it advances and BCMP's heartbeat timer
/// reloads every ten seconds, so an unbounded advance is an unbounded loop.
pub const MAX_ADVANCE_MS: u32 = 60_000;

/// Most steps a single input may carry.
pub const MAX_STEPS: usize = 24;

/// Neighbour-table capacity of the port's node, and so of its info cache.
///
/// The capture device has two ports and bm_core keeps one neighbour per port,
/// so nothing here can fill either; [`check`] asserts that.
pub const NEIGHBORS: usize = 4;

/// How many unanswered device-info requests the port's node remembers.
///
/// bm_core's `INFO_REQUEST_LIST` is unbounded and never expires an entry
/// (divergence #19), so a ceiling is a difference in kind and not something to
/// compare across. A step adds at most one entry and both lists start empty,
/// so one more than [`MAX_STEPS`] puts the port's ceiling out of reach of any
/// seed — `bm_stack::node::INFO_REQUESTS_DEFAULT` would not. [`check`] asserts
/// the list never fills, and the spare is what keeps that assertion about
/// *reaching* the ceiling rather than about a seed of exactly [`MAX_STEPS`]
/// requests landing on it.
pub const REQUEST_CAPACITY: usize = MAX_STEPS + 1;

/// The port's node, sized as above.
type InfoNode = Node<OracleIdentity, SoftRtc, NEIGHBORS, 4, PING_PAYLOAD_BYTES, REQUEST_CAPACITY>;

/// A node with the oracle's identity, port count and link state, and a request
/// list no seed can fill.
fn info_node() -> InfoNode {
    let mut node = Node::new(OracleIdentity, SoftRtc::new(), NUM_PORTS);
    for port in 1..=NUM_PORTS {
        node.set_link_up(port, true);
    }
    node
}

/// One thing that happens to the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A heartbeat arrives, which is how a neighbour comes to exist and how a
    /// restart is announced.
    Heartbeat {
        /// Index into [`NODE_IDS`], the sender.
        node: u8,
        /// Ingress port, 1..=[`NUM_PORTS`].
        port: u8,
        /// The `time_since_boot_us` the sender claims. A lower value than last
        /// time is a restart.
        uptime_us: u64,
    },
    /// A device-info reply arrives.
    Reply {
        /// Index into [`NODE_IDS`] for the frame's source address.
        from: u8,
        /// Index into [`NODE_IDS`] for the `node_id` in the *body*, which is
        /// what everything downstream is keyed on.
        claims: u8,
        /// Ingress port, 1..=[`NUM_PORTS`].
        port: u8,
        /// Index into [`STRINGS`].
        strings: u8,
        /// Varies the fixed part, so a second reply from the same node is
        /// distinguishable from the first.
        revision: u8,
    },
    /// The node asks someone to describe itself — `bcmp_request_info`.
    Request {
        /// Index into [`NODE_IDS`].
        node: u8,
        /// Whether the request carries a callback. `false` is `cb == NULL`,
        /// which is what bm_core's own call sites pass.
        report: bool,
    },
    /// Time passes.
    Advance {
        /// Milliseconds, capped at [`MAX_ADVANCE_MS`].
        ms: u32,
    },
}

impl Step {
    fn clamp(&mut self) {
        match self {
            Self::Heartbeat { node, port, .. } => {
                *node %= NODE_IDS.len() as u8;
                *port = port.wrapping_sub(1) % NUM_PORTS + 1;
            }
            Self::Reply {
                from,
                claims,
                port,
                strings,
                ..
            } => {
                *from %= NODE_IDS.len() as u8;
                *claims %= NODE_IDS.len() as u8;
                *port = port.wrapping_sub(1) % NUM_PORTS + 1;
                *strings %= STRINGS.len() as u8;
            }
            Self::Request { node, .. } => *node %= NODE_IDS.len() as u8,
            Self::Advance { ms } => *ms %= MAX_ADVANCE_MS + 1,
        }
    }
}

/// A sequence of steps applied to both nodes.
#[derive(Debug, Clone)]
pub struct InfoInput {
    /// The steps, capped at [`MAX_STEPS`] by [`Domain`].
    pub steps: Vec<Step>,
}

impl<'a> Arbitrary<'a> for InfoInput {
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
    let mut step = match u.int_in_range(0..=3u8)? {
        0 => Step::Heartbeat {
            node: u.arbitrary()?,
            port: u.arbitrary()?,
            uptime_us: u.arbitrary()?,
        },
        1 => Step::Reply {
            from: u.arbitrary()?,
            claims: u.arbitrary()?,
            port: u.arbitrary()?,
            strings: u.arbitrary()?,
            revision: u.arbitrary()?,
        },
        2 => Step::Request {
            node: u.arbitrary()?,
            report: u.arbitrary()?,
        },
        _ => Step::Advance { ms: u.arbitrary()? },
    };
    step.clamp();
    Ok(step)
}

impl Domain for InfoInput {
    fn clamp_to_domain(&mut self) {
        self.steps.truncate(MAX_STEPS);
        for step in &mut self.steps {
            step.clamp();
        }
    }
}

/// The fixed part a reply carries for `node_id` at `revision`.
///
/// Every field varies, so a comparison that only copied some of them fails.
#[must_use]
pub fn device_info(node_id: u64, revision: u8) -> DeviceInfo {
    let wide = u32::from(revision);
    DeviceInfo {
        node_id,
        vendor_id: 0xBE00 | u16::from(revision),
        product_id: 0x0040_u16.wrapping_add(u16::from(revision)),
        serial_num: [revision; 16],
        git_sha: 0x1234_0000 | wide,
        ver_major: revision,
        ver_minor: revision.wrapping_add(1),
        ver_rev: revision.wrapping_add(2),
        ver_hw: revision.wrapping_add(3),
    }
}

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

/// A heartbeat frame, which is what makes a neighbour exist.
fn heartbeat_frame(node_id: u64, uptime_us: u64) -> Vec<u8> {
    let heartbeat = Heartbeat {
        time_since_boot_us: uptime_us,
        liveliness_lease_dur_s: bm_wire::neighbor::HEARTBEAT_PERIOD_S,
    };
    let mut body = [0u8; Heartbeat::LEN];
    heartbeat.encode(&mut body).expect("12 bytes");
    peer_frame(node_id, MessageType::HEARTBEAT, &body)
}

/// A device-info reply frame describing `claims`.
fn reply_frame(from: u64, claims: u64, strings: usize, revision: u8) -> Vec<u8> {
    let (version, name) = STRINGS[strings % STRINGS.len()];
    let reply = DeviceInfoReply {
        info: device_info(claims, revision),
        version_string: version,
        device_name: name,
    };
    let mut body = vec![0u8; reply.encoded_len()];
    reply.encode(&mut body).expect("sized from encoded_len");
    peer_frame(from, MessageType::DEVICE_INFO_REPLY, &body)
}

/// One node's cached device information, on either side of the comparison.
///
/// A node the C has no information for reads back as the zeroed `info` and two
/// null pointers its `BcmpNeighbor` was allocated with, which is exactly what
/// [`Self::default`] is — so "nothing known" compares equal on both sides
/// without either having to special-case it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cached {
    /// The fixed part.
    pub info: DeviceInfo,
    /// The version string, empty for a null pointer.
    pub version_string: Vec<u8>,
    /// The device name, empty for a null pointer.
    pub device_name: Vec<u8>,
}

/// What `bcmp_request_info`'s callback was handed.
static REPORTED: Mutex<Vec<Cached>> = Mutex::new(Vec::new());

/// `bcmp_request_info`'s `cb`, which is handed the `BcmpDeviceInfoReply` still
/// inside the received frame.
///
/// # Safety
///
/// `arg` is bm_core's `BcmpDeviceInfoReply *`. The declared string lengths are
/// trusted here exactly as `populate_neighbor_info` trusts them; the comparator
/// only ever injects replies whose declarations match what arrived, which is
/// the domain limit divergence #14 describes.
unsafe extern "C" fn on_info(arg: *mut core::ffi::c_void) {
    let reply = arg.cast::<bm_wire_sys::BcmpDeviceInfoReply>();
    let (info, version_len, name_len) = unsafe {
        (
            (*reply).info,
            usize::from((*reply).ver_str_len),
            usize::from((*reply).dev_name_len),
        )
    };
    let strings = unsafe {
        core::slice::from_raw_parts(
            (*reply).strings.as_ptr().cast::<u8>(),
            version_len + name_len,
        )
    };
    REPORTED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Cached {
            info: from_c_info(&info),
            version_string: strings[..version_len].to_vec(),
            device_name: strings[version_len..].to_vec(),
        });
}

fn take_reported() -> Vec<Cached> {
    std::mem::take(&mut *REPORTED.lock().unwrap_or_else(|p| p.into_inner()))
}

fn from_c_info(info: &bm_wire_sys::BcmpDeviceInfo) -> DeviceInfo {
    DeviceInfo {
        node_id: info.node_id,
        vendor_id: info.vendor_id,
        product_id: info.product_id,
        serial_num: info.serial_num,
        git_sha: info.git_sha,
        ver_major: info.ver_major,
        ver_minor: info.ver_minor,
        ver_rev: info.ver_rev,
        ver_hw: info.ver_hw,
    }
}

/// What the oracle's neighbour table knows about each node, in list order.
///
/// # Safety
///
/// Walks bm_core's `BcmpNeighbor` list and reads two `char *` out of each
/// entry as NUL-terminated strings, which is the only way bm_core offers: it
/// keeps no length beside either pointer. Valid only while the oracle lock is
/// held and nothing is pumping.
fn oracle_cache() -> Vec<(u64, Cached)> {
    let mut out = Vec::new();
    unsafe {
        let mut count = 0u8;
        let mut node = bm_wire_sys::bcmp_get_neighbors(&mut count);
        while !node.is_null() {
            let entry = &*node;
            out.push((
                entry.node_id,
                Cached {
                    info: from_c_info(&entry.info),
                    version_string: c_string(entry.version_str),
                    device_name: c_string(entry.device_name),
                },
            ));
            node = entry.next;
        }
    }
    out
}

/// A `char *` bm_core allocated, or an empty vector for a null pointer.
///
/// # Safety
///
/// `ptr` is NUL-terminated or null.
unsafe fn c_string(ptr: *mut core::ffi::c_char) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec()
}

/// The oracle's `INFO_REQUEST_LIST`, as the comparator tracks it.
///
/// bm_core exposes neither the list nor its length, so this is kept from the
/// C's *own* transmissions rather than from what the port decided: a `0x04`
/// frame on the wire means `ll_item_add` ran and `bcmp_tx` succeeded, which is
/// exactly one entry. Entries are the low 32 bits of the target, since that is
/// what `LLItem::id` can hold — divergence #33.
///
/// [`LinkModel`] rides alongside, in lockstep, because this list reaches
/// divergence #20 in ordinary use: three outstanding requests, a reply
/// answering the middle one, a reply answering the last one, and
/// `INFO_REQUEST_LIST`'s tail points at freed memory. All three replies are
/// attacker-chosen — the key comes out of the reply body. [`check`] declines
/// the *append* that would then write through the freed pointer, which is the
/// only undefined operation: `ll_remove` and `ll_get_item` both walk from the
/// head.
#[derive(Debug, Default)]
struct OracleRequests {
    keys: Vec<u32>,
    links: LinkModel,
}

impl OracleRequests {
    /// Whether an `ll_item_add` would now be undefined, so no step may provoke
    /// a request until the list has emptied.
    fn add_is_undefined(&self) -> bool {
        self.links.add_is_undefined()
    }

    fn added(&mut self, target_node_id: u64) {
        self.links.add();
        self.keys.push(target_node_id as u32);
        debug_assert_eq!(self.keys.len(), self.links.len());
    }

    /// `ll_remove` takes the first entry with the key, or none.
    fn answered(&mut self, claimed_node_id: u64) {
        let key = claimed_node_id as u32;
        if let Some(index) = self.keys.iter().position(|k| *k == key) {
            self.keys.remove(index);
            self.links.remove(index);
        }
    }
}

/// The `0x04` requests in a batch of captured frames, one entry per request
/// rather than per port copy.
///
/// # Panics
///
/// If the copies of one request disagree about who it is addressed to, or if
/// the oracle emitted a number of copies that is not a whole number of
/// requests.
fn captured_info_requests(captured: &[Captured]) -> Vec<(u64, Vec<Captured>)> {
    let copies: Vec<Captured> = captured
        .iter()
        // The type comes out of the BCMP header rather than out of
        // `rx::accept`'s verdict: a frame the oracle stamped an egress port
        // into need not validate, because divergence #12 drops the end-around
        // carry. Filtering on `accept` would skip exactly those.
        .filter(|(_, frame)| captured_message_type(frame) == Some(MessageType::DEVICE_INFO_REQUEST))
        .cloned()
        .collect();
    assert_eq!(
        copies.len() % usize::from(NUM_PORTS),
        0,
        "a link-local request goes out once per port, got {} copies",
        copies.len()
    );

    copies
        .chunks(usize::from(NUM_PORTS))
        .map(|chunk| {
            let targets: Vec<u64> = chunk
                .iter()
                .map(|(_, frame)| {
                    DeviceInfoRequest::decode(&frame[BCMP_BODY_OFFSET..])
                        .expect("the oracle builds an eight-byte body")
                        .target_node_id
                })
                .collect();
            assert!(
                targets.windows(2).all(|w| w[0] == w[1]),
                "the port copies of one request name different nodes: {targets:x?}"
            );
            (targets[0], chunk.to_vec())
        })
        .collect()
}

/// Compare the whole request frame, per port, after stamping ours the way L2
/// would.
fn compare_request_frames(index: usize, c_copies: &[Captured], ours: &[u8]) {
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
            "step {index}: the device-info request diverged on port {port} at byte {at}\n  \
             C:        {c_frame:02x?}\n  bm-stack: {stamped:02x?}"
        );
    }
}

/// Apply the same steps to bm_core's stack and to a [`bm_stack::Node`], and
/// assert the two agree after every one.
///
/// # Panics
///
/// If the caches, the callbacks or the requests ever differ.
pub fn check(input: &InfoInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let _guard = oracle();
    // Both sides start with nothing. Clearing the table frees the strings the
    // last seed left on it, which is the only reset the oracle has.
    clear_neighbor_table();
    let mut node = info_node();
    drain();
    take_reported();

    let mut requests = OracleRequests::default();

    for (index, step) in input.steps.iter().enumerate() {
        let now = tick_count();
        let mut ours: Option<Vec<u8>> = None;
        let mut reported = Vec::new();

        // Past this point `ll_item_add` would write through a freed pointer,
        // so neither node is given anything that could ask for information.
        // Only a reply, which removes, is still defined. See `OracleRequests`
        // and divergence #20.
        let appends = matches!(step, Step::Heartbeat { .. } | Step::Request { .. });
        if appends && requests.add_is_undefined() {
            compare_caches(index, step, &node);
            continue;
        }

        match *step {
            Step::Heartbeat {
                node: which,
                port,
                uptime_us,
            } => {
                let frame = heartbeat_frame(NODE_IDS[usize::from(which)], uptime_us);
                inject(port, &frame);
                ours = our_reply(&mut node, now, port, &frame, &mut reported);
            }
            Step::Reply {
                from,
                claims,
                port,
                strings,
                revision,
            } => {
                let claimed = NODE_IDS[usize::from(claims)];
                let frame = reply_frame(
                    NODE_IDS[usize::from(from)],
                    claimed,
                    usize::from(strings),
                    revision,
                );
                inject(port, &frame);
                requests.answered(claimed);
                ours = our_reply(&mut node, now, port, &frame, &mut reported);
            }
            Step::Request {
                node: which,
                report,
            } => {
                let target = NODE_IDS[usize::from(which)];
                let cb = if report {
                    Some(on_info as unsafe extern "C" fn(*mut core::ffi::c_void))
                } else {
                    None
                };
                unsafe {
                    assert_eq!(
                        bm_wire_sys::bcmp_request_info(
                            target,
                            (&raw const bm_wire_sys::multicast_ll_addr).cast(),
                            cb,
                        ),
                        bm_wire_sys::BmErr_BmOK,
                        "step {index}: the oracle refused to ask"
                    );
                }
                pump_until_quiet();
                let kind = if report {
                    InfoRequestKind::Report
                } else {
                    InfoRequestKind::Cache
                };
                ours = node
                    .request_device_info(now, target, kind)
                    .map(|outbound| outbound.frame().to_vec());
            }
            Step::Advance { ms } => {
                unsafe { bm_wire_sys::bm_shim_advance_ticks(ms) };
                pump_until_quiet();
            }
        }

        let captured = drain();
        let c_requests = captured_info_requests(&captured);
        for (target, copies) in &c_requests {
            requests.added(*target);
            let our_frame = ours.as_deref().unwrap_or_else(|| {
                panic!(
                    "step {index} ({step:?}): the C asked node {target:016x} and we asked nobody"
                )
            });
            compare_request_frames(index, copies, our_frame);
        }
        assert_eq!(
            c_requests.len(),
            usize::from(ours.is_some()),
            "step {index} ({step:?}): the two nodes disagree on how many requests to send"
        );

        assert_eq!(
            take_reported(),
            reported,
            "step {index} ({step:?}): the info callbacks diverged"
        );
        compare_caches(index, step, &node);
    }

    answer_everything_outstanding(&mut requests);
}

/// Run the same frame through our node, returning the request it wants sent
/// and recording every [`Event::DeviceInfo`] on the way.
fn our_reply(
    node: &mut InfoNode,
    now: u32,
    port: u8,
    frame: &[u8],
    reported: &mut Vec<Cached>,
) -> Option<Vec<u8>> {
    let mut frame = frame.to_vec();
    let owed = node.on_frame_with(now, port, &mut frame, |event| {
        if let Event::DeviceInfo { reply, .. } = event {
            reported.push(Cached {
                info: reply.info,
                version_string: reply.version_string.to_vec(),
                device_name: reply.device_name.to_vec(),
            });
        }
    });
    owed.reply.map(|outbound| outbound.frame().to_vec())
}

/// Compare what each side knows about every node the oracle has a neighbour
/// entry for, and assert we know nothing about anything else.
fn compare_caches(index: usize, step: &Step, node: &InfoNode) {
    // The port has ceilings bm_core does not. Both are sized out of reach; a
    // full one would mean the comparison had stopped being meaningful.
    assert!(
        node.info_requests().len() < node.info_requests().capacity(),
        "step {index} ({step:?}): the port's request list filled, \
         and bm_core's cannot"
    );
    assert!(
        node.device_info_cache().len() < node.device_info_cache().capacity(),
        "step {index} ({step:?}): the port's info cache filled, \
         and bm_core keeps one entry per neighbour"
    );

    let c = oracle_cache();
    for (node_id, theirs) in &c {
        let ours = node
            .device_info(*node_id)
            .map_or_else(Cached::default, |c| Cached {
                info: c.info,
                version_string: c.version_string.to_vec(),
                device_name: c.device_name.to_vec(),
            });
        assert_eq!(
            &ours, theirs,
            "step {index} ({step:?}): what the two nodes know about {node_id:016x} diverged"
        );
    }

    for node_id in node.device_info_cache().node_ids() {
        assert!(
            c.iter().any(|(id, _)| *id == node_id),
            "step {index} ({step:?}): we cached {node_id:016x}, \
             which the oracle has no neighbour entry for"
        );
    }
}

/// Answer every request the oracle still has outstanding, so
/// `INFO_REQUEST_LIST` is empty again when the next seed starts.
///
/// Runs after the last comparison, so what it caches on the way is never
/// compared; the next [`check`] clears the neighbour table anyway.
fn answer_everything_outstanding(requests: &mut OracleRequests) {
    // A reply claiming the key itself matches whatever the key was recorded
    // from: the list only ever sees the low 32 bits. Walking the list in order
    // means every removal is a head removal, which is always defined however
    // the seed left the `previous` pointers.
    while let Some(key) = requests.keys.first().copied() {
        inject(1, &reply_frame(u64::from(key), u64::from(key), 0, 0));
        requests.answered(u64::from(key));
    }
    drain();
    assert!(
        !requests.add_is_undefined(),
        "an empty INFO_REQUEST_LIST cannot leave a dangling tail"
    );
}
