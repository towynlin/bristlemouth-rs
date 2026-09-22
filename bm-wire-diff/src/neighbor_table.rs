//! Differential comparator for neighbour-table reply consumption —
//! `bm_stack`'s requester against `bcmp_request_neighbor_table` and
//! `bcmp_process_neighbor_table_reply` in `bcmp/neighbors.c`.
//!
//! A sequence of requests, replies and clock advances is applied to bm_core's
//! stack and to a [`bm_stack::Node`], and after every step three things are
//! compared:
//!
//! | What | C side | Rust side |
//! |---|---|---|
//! | The request it provokes | the `0x08` frames the oracle transmits | [`bm_stack::Outbound`] |
//! | The reply callback | `bcmp_request_neighbor_table`'s `request(reply)` | [`bm_stack::Event::NeighborTable`] |
//! | The timeout callback | its `timeout(timer)` | [`bm_stack::Event::NeighborTableTimeout`] |
//!
//! Those three are the whole of what bm_core exposes: `TARGET_NODE_ID`,
//! `NEIGHBOR_REQUEST_CB` and `NEIGHBOR_TIMER` are file-scope statics with no
//! accessor. [`Model`] is this comparator's belief about what they hold, and
//! it is asserted against both sides on every step rather than being trusted —
//! the discipline card M2 arrived at.
//!
//! # The two divergences this found
//!
//! * **#35.** A broadcast request (`target_node_id == 0`) is answered by every
//!   node and accepted from none, because the acceptance test is an exact
//!   match against the replier's own id. Zero is still a *target* like any
//!   other, so a node calling itself zero is heard — and `TARGET_NODE_ID`
//!   starts at zero, so such a reply is accepted before anything is asked.
//! * **#36.** The 1 s timer fires the caller's `timeout` and disarms nothing,
//!   so a reply arriving afterwards is reported as an answer.
//!
//! # No heartbeats, and so no fork mode
//!
//! `bcmp_update_neighbor` is reachable only from `bcmp_process_heartbeat`, and
//! a heartbeat is the only thing that grows `INFO_REQUEST_LIST` (divergence
//! #19). Keeping heartbeats out of the input domain therefore keeps this
//! target free of the one thing that accumulates across a run, and it costs
//! nothing: the requester never consults the neighbour table.
//!
//! What *does* outlive a seed is `bcmp/neighbors.c`'s own three statics, which
//! have no deinit. [`reset_requester`] puts them back by the only route there
//! is — a request naming node zero, answered — so every seed starts where the
//! process started.
//!
//! # The timer is an integrator seam
//!
//! `bm_timer_create`/`start`/`stop` are declared in `bm_os.h` and defined
//! nowhere in bm_core; the only implementation here is ours, in
//! `bm-wire-sys/csrc/bm_os_shim.c`, which fires a timer once
//! `(int32_t)(tick - due) >= 0`. That is
//! [`time_remaining`] restated, which is what
//! [`bm_wire::bcmp::neighbors::TableRequests`] compares with, so the two agree
//! by construction rather than by comparison — as `bm_rtc_get` does for card
//! M2. Everything downstream *is* compared: whether the timeout ran, and
//! whether a reply after it is still accepted.

use std::sync::Mutex;

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_stack::node::NEIGHBOR_REQUEST_TIMEOUT_MS;
use bm_stack::{Event, Node, SoftRtc};
use bm_wire::bcmp::neighbors::{
    NeighborInfo, NeighborTableRequest, PortInfo, TableRequestKind, encode_neighbor_table_reply,
    neighbor_table_reply_len,
};
use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, rx, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::l2;
use bm_wire::util::{BmIpAddr, time_remaining};

use crate::Domain;
use crate::stack::{
    NUM_PORTS, OracleIdentity, drain, inject, oracle, pump_until_quiet, tick_count,
};

/// The node ids a step may name.
///
/// Index 3 shares its low 32 bits with index 1, which `INFO_REQUEST_LIST`
/// would treat as one entry (divergence #33). `TARGET_NODE_ID` is a plain
/// `uint64_t` and compares whole, so the two are distinct here — the
/// comparator pins that difference rather than assuming it.
///
/// Zero is index 0, and it is two things at once: the target of a broadcast
/// request that nothing can answer, and the id a reply may claim in order to
/// match the `TARGET_NODE_ID` a process starts with. See divergence #35.
pub const NODE_IDS: &[u64] = &[
    0,
    0x0000_0000_55AA_0011,
    0x0000_0000_55AA_0022,
    0xDEAD_BEEF_55AA_0011,
];

/// Most port or neighbour entries a reply may declare.
///
/// The card asks for a two-node table, and the arrays are what divergence
/// #14's unchecked lengths are read out of — so 0, 1 and 2 of each, with the
/// declarations always matching what arrived.
pub const MAX_ENTRIES: u8 = 2;

/// Longest a single step may push the virtual clock.
///
/// The clock fires every due timer as it advances and BCMP's heartbeat timer
/// reloads every ten seconds, so an unbounded advance is an unbounded loop.
pub const MAX_ADVANCE_MS: u32 = 60_000;

/// Most steps a single input may carry.
pub const MAX_STEPS: usize = 24;

/// The port's node. Four neighbours, as everywhere else here; the requester
/// does not use the table.
type TableNode = Node<OracleIdentity, SoftRtc, 4>;

/// One thing that happens to the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The node asks someone for their neighbour table —
    /// `bcmp_request_neighbor_table`.
    Request {
        /// Index into [`NODE_IDS`].
        node: u8,
        /// Whether the request carries a reply callback. `false` is
        /// `request == NULL`.
        report: bool,
    },
    /// A neighbour-table reply arrives.
    Reply {
        /// Index into [`NODE_IDS`] for the frame's source address.
        from: u8,
        /// Index into [`NODE_IDS`] for the `node_id` in the *body*, which is
        /// what the acceptance test reads.
        claims: u8,
        /// Ingress port, 1..=[`NUM_PORTS`].
        port: u8,
        /// Port entries the reply carries, 0..=[`MAX_ENTRIES`].
        ports: u8,
        /// Neighbour entries the reply carries, 0..=[`MAX_ENTRIES`].
        neighbors: u8,
        /// Varies the entries, so a second reply is distinguishable from the
        /// first.
        revision: u8,
    },
    /// Time passes, which is the only thing that fires the timer.
    Advance {
        /// Milliseconds, capped at [`MAX_ADVANCE_MS`].
        ms: u32,
    },
}

impl Step {
    fn clamp(&mut self) {
        match self {
            Self::Request { node, .. } => *node %= NODE_IDS.len() as u8,
            Self::Reply {
                from,
                claims,
                port,
                ports,
                neighbors,
                ..
            } => {
                *from %= NODE_IDS.len() as u8;
                *claims %= NODE_IDS.len() as u8;
                *port = port.wrapping_sub(1) % NUM_PORTS + 1;
                *ports %= MAX_ENTRIES + 1;
                *neighbors %= MAX_ENTRIES + 1;
            }
            Self::Advance { ms } => *ms %= MAX_ADVANCE_MS + 1,
        }
    }
}

/// A sequence of steps applied to both nodes.
#[derive(Debug, Clone)]
pub struct NeighborTableInput {
    /// The steps, capped at [`MAX_STEPS`] by [`Domain`].
    pub steps: Vec<Step>,
}

impl<'a> Arbitrary<'a> for NeighborTableInput {
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
    let mut step = match u.int_in_range(0..=2u8)? {
        0 => Step::Request {
            node: u.arbitrary()?,
            report: u.arbitrary()?,
        },
        1 => Step::Reply {
            from: u.arbitrary()?,
            claims: u.arbitrary()?,
            port: u.arbitrary()?,
            ports: u.arbitrary()?,
            neighbors: u.arbitrary()?,
            revision: u.arbitrary()?,
        },
        _ => Step::Advance { ms: u.arbitrary()? },
    };
    step.clamp();
    Ok(step)
}

impl Domain for NeighborTableInput {
    fn clamp_to_domain(&mut self) {
        self.steps.truncate(MAX_STEPS);
        for step in &mut self.steps {
            step.clamp();
        }
    }
}

// ---------------------------------------------------------------------------
// The frames
// ---------------------------------------------------------------------------

/// The port entries a reply carries at `revision`.
fn port_entries(count: u8, revision: u8) -> Vec<PortInfo> {
    (0..count)
        .map(|index| PortInfo {
            // 0 or 1: the C field is a `bool`, so anything else would be an
            // out-of-range read on its side of the comparison.
            state: (revision.wrapping_add(index)) & 1,
            // `bcmp_send_neighbor_table` never sets this, so a reply that did
            // would be comparing against a field nothing writes.
            port_type: 0,
        })
        .collect()
}

/// The neighbour entries a reply carries at `revision`.
fn neighbor_entries(count: u8, revision: u8) -> Vec<NeighborInfo> {
    (0..count)
        .map(|index| NeighborInfo {
            node_id: NODE_IDS[usize::from(index.wrapping_add(revision)) % NODE_IDS.len()],
            port: index % NUM_PORTS + 1,
            online: (revision.wrapping_add(index)) & 1,
        })
        .collect()
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

/// A `0x09` frame from `from`, claiming `claims` and describing the entries a
/// step asked for.
///
/// Every declared length matches what the frame carries: divergence #14 means
/// a lying one has the C reading past the frame, which is not behaviour to
/// compare against.
fn reply_frame(from: u64, claims: u64, ports: u8, neighbors: u8, revision: u8) -> Vec<u8> {
    let ports = port_entries(ports, revision);
    let neighbors = neighbor_entries(neighbors, revision);
    let mut body = vec![0u8; neighbor_table_reply_len(ports.len(), neighbors.len())];
    encode_neighbor_table_reply(&mut body, claims, &ports, &neighbors)
        .expect("sized from neighbor_table_reply_len");
    peer_frame(from, MessageType::NEIGHBOR_TABLE_REPLY, &body)
}

// ---------------------------------------------------------------------------
// What the C's two callbacks were handed
// ---------------------------------------------------------------------------

/// One reply, as either side of the comparison saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reported {
    /// `reply->node_id`, which is what was matched.
    pub node_id: u64,
    /// `(state, type)` per port entry.
    pub ports: Vec<(u8, u8)>,
    /// `(node_id, port, online)` per neighbour entry.
    pub neighbors: Vec<(u64, u8, u8)>,
}

static REPORTED: Mutex<Vec<Reported>> = Mutex::new(Vec::new());
static TIMEOUTS: Mutex<usize> = Mutex::new(0);

/// `bcmp_request_neighbor_table`'s `request`, handed the
/// `BcmpNeighborTableReply` still inside the received frame.
///
/// # Safety
///
/// `reply` is bm_core's `BcmpNeighborTableReply *`. The declared entry counts
/// are trusted here exactly as `topology.c`'s `neighbor_request_cb` trusts
/// them; the comparator only ever injects replies whose declarations match what
/// arrived, which is the domain limit divergence #14 describes.
unsafe extern "C" fn on_reply(
    reply: *mut bm_wire_sys::BcmpNeighborTableReply,
) -> bm_wire_sys::BmErr {
    let (node_id, port_len, neighbor_len) = unsafe {
        (
            (*reply).node_id,
            usize::from((*reply).port_len),
            usize::from((*reply).neighbor_len),
        )
    };
    let ports = unsafe {
        core::slice::from_raw_parts((*reply).port_list.as_ptr(), port_len)
            .iter()
            .map(|port| (u8::from(port.state), port.type_))
            .collect()
    };
    // The neighbour array starts where the port array ends, which is what
    // `assemble_neighbor_info_list` is handed on the way out.
    let neighbors = unsafe {
        let after_ports = (*reply).port_list.as_ptr().add(port_len);
        core::slice::from_raw_parts(
            after_ports.cast::<bm_wire_sys::BcmpNeighborInfo>(),
            neighbor_len,
        )
        .iter()
        .map(|n| (n.node_id, n.port, n.online))
        .collect()
    };
    REPORTED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Reported {
            node_id,
            ports,
            neighbors,
        });
    bm_wire_sys::BmErr_BmOK
}

/// `bcmp_request_neighbor_table`'s `timeout`, which the C hands
/// `bm_timer_create` and never calls itself.
///
/// # Safety
///
/// Called from `fire_due_timers` with the timer that fired, which this ignores
/// — as `topology_timer_handler` does.
unsafe extern "C" fn on_timeout(_timer: bm_wire_sys::BmTimer) {
    *TIMEOUTS.lock().unwrap_or_else(|p| p.into_inner()) += 1;
}

fn take_reported() -> Vec<Reported> {
    std::mem::take(&mut *REPORTED.lock().unwrap_or_else(|p| p.into_inner()))
}

fn take_timeouts() -> usize {
    std::mem::take(&mut *TIMEOUTS.lock().unwrap_or_else(|p| p.into_inner()))
}

// ---------------------------------------------------------------------------
// The comparator's belief about the C's three statics
// ---------------------------------------------------------------------------

/// `TARGET_NODE_ID`, `NEIGHBOR_REQUEST_CB` and `NEIGHBOR_TIMER`, as this
/// comparator believes them to be.
///
/// Deliberately a second implementation rather than a call into
/// [`bm_wire::bcmp::neighbors::TableRequests`]: it is what says *why* each
/// side did what it did, and a wrong belief here is a test failure rather than
/// a silent agreement. Both of card M2's divergences came out of exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Model {
    /// `TARGET_NODE_ID`, which starts at zero and is never cleared.
    pub target_node_id: u64,
    /// Whether `NEIGHBOR_REQUEST_CB` is non-null.
    pub armed: bool,
    /// When `NEIGHBOR_TIMER` was started, while it is running.
    pub started_ms: Option<u32>,
}

impl Model {
    /// `bcmp_request_neighbor_table`, all three writes.
    fn record(&mut self, now_ms: u32, target_node_id: u64, report: bool) {
        self.target_node_id = target_node_id;
        self.armed = report;
        self.started_ms = Some(now_ms);
    }

    /// `bcmp_process_neighbor_table_reply`. Reports whether the callback ran.
    fn accept(&mut self, claimed: u64) -> bool {
        if self.target_node_id != claimed {
            return false;
        }
        self.started_ms = None;
        let reported = self.armed;
        self.armed = false;
        reported
    }

    /// The clock reaching `now_ms`. Reports whether the timeout ran.
    fn advance(&mut self, now_ms: u32) -> bool {
        let Some(started) = self.started_ms else {
            return false;
        };
        if time_remaining(started, now_ms, NEIGHBOR_REQUEST_TIMEOUT_MS) != 0 {
            return false;
        }
        self.started_ms = None;
        true
    }
}

// ---------------------------------------------------------------------------
// The comparison
// ---------------------------------------------------------------------------

/// A frame the oracle transmitted, and the port it went out on.
type Captured = (u8, Vec<u8>);

/// The `0x08` requests in a batch of captured frames, one entry per request
/// rather than per port copy.
///
/// # Panics
///
/// If the copies of one request disagree about who it is addressed to, or if
/// the oracle emitted a number of copies that is not a whole number of
/// requests.
fn captured_table_requests(captured: &[Captured]) -> Vec<(u64, Vec<Captured>)> {
    let copies: Vec<Captured> = captured
        .iter()
        .filter(|(_, frame)| {
            // The type comes out of the BCMP header, not out of `rx::accept`'s
            // verdict: a frame the C stamped need not validate (divergence
            // #12).
            let mut copy = frame.clone();
            rx::accept(&mut copy)
                .map(|r| r.header.message_type == MessageType::NEIGHBOR_TABLE_REQUEST)
                .unwrap_or(false)
        })
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
                    let mut copy = frame.clone();
                    let received = rx::accept(&mut copy).expect("filtered on it parsing");
                    NeighborTableRequest::decode(received.payload)
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
            "step {index}: the neighbour-table request diverged on port {port} at byte {at}\n  \
             C:        {c_frame:02x?}\n  bm-stack: {stamped:02x?}"
        );
    }
}

/// Put `bcmp/neighbors.c`'s three statics back where the process found them.
///
/// There is no deinit and no accessor, so the only route is the front door: a
/// request naming node zero, answered by a reply claiming node zero. That
/// leaves `TARGET_NODE_ID` at zero, `NEIGHBOR_REQUEST_CB` null and
/// `NEIGHBOR_TIMER` created but stopped — which is [`Model::default`] and a
/// fresh [`bm_wire::bcmp::neighbors::TableRequests`], since a stopped timer and
/// no timer are the same thing to everything that can observe either.
///
/// Unconditional, because the previous seed's state is not knowable after a
/// panic: the request overwrites whatever was there before the reply clears it.
///
/// # Panics
///
/// If the oracle refuses the request, or if it does not answer its own reply.
pub fn reset_requester() {
    // Whatever a panicking seed left in the two recorders, before the reset
    // adds to it -- otherwise the assertion below reports the wrong thing.
    take_reported();
    take_timeouts();
    unsafe {
        assert_eq!(
            bm_wire_sys::bcmp_request_neighbor_table(
                0,
                (&raw const bm_wire_sys::multicast_ll_addr).cast(),
                Some(on_reply),
                Some(on_timeout),
            ),
            bm_wire_sys::BmErr_BmOK,
            "the oracle refused to ask"
        );
    }
    pump_until_quiet();
    inject(1, &reply_frame(NODE_IDS[1], 0, 0, 0, 0));
    drain();
    assert_eq!(
        take_reported().len(),
        1,
        "a reply claiming zero must answer the request naming zero"
    );
    take_timeouts();
}

/// Apply the same steps to bm_core's stack and to a [`bm_stack::Node`], and
/// assert the two agree after every one.
///
/// # Panics
///
/// If the request frames, the reply callbacks or the timeouts ever differ, or
/// if either side departs from [`Model`].
pub fn check(input: &NeighborTableInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let _guard = oracle();
    reset_requester();
    let mut node = crate::stack::node();
    let mut model = Model::default();
    drain();

    for (index, step) in input.steps.iter().enumerate() {
        let now = tick_count();
        let mut ours: Option<Vec<u8>> = None;
        let mut reported = Vec::new();
        let mut timeouts = 0usize;
        let mut expect_reported = false;
        let mut expect_timeout = false;

        match *step {
            Step::Request {
                node: which,
                report,
            } => {
                let target = NODE_IDS[usize::from(which)];
                let request_cb = if report {
                    Some(on_reply as unsafe extern "C" fn(_) -> _)
                } else {
                    None
                };
                unsafe {
                    assert_eq!(
                        bm_wire_sys::bcmp_request_neighbor_table(
                            target,
                            (&raw const bm_wire_sys::multicast_ll_addr).cast(),
                            request_cb,
                            Some(on_timeout),
                        ),
                        bm_wire_sys::BmErr_BmOK,
                        "step {index}: the oracle refused to ask"
                    );
                }
                pump_until_quiet();
                let kind = if report {
                    TableRequestKind::Report
                } else {
                    TableRequestKind::Ignore
                };
                ours = node
                    .request_neighbor_table(now, &BmIpAddr::LINK_LOCAL_MULTICAST, target, kind)
                    .map(|outbound| outbound.frame().to_vec());
                model.record(now, target, report);
            }
            Step::Reply {
                from,
                claims,
                port,
                ports,
                neighbors,
                revision,
            } => {
                let claimed = NODE_IDS[usize::from(claims)];
                let frame = reply_frame(
                    NODE_IDS[usize::from(from)],
                    claimed,
                    ports,
                    neighbors,
                    revision,
                );
                inject(port, &frame);
                expect_reported = model.accept(claimed);

                let mut ours_frame = frame.clone();
                let owed = node.on_frame_with(now, port, &mut ours_frame, |event| match event {
                    Event::NeighborTable { reply, .. } => reported.push(Reported {
                        node_id: reply.node_id,
                        ports: reply.ports().map(|p| (p.state, p.port_type)).collect(),
                        neighbors: reply
                            .neighbors()
                            .map(|n| (n.node_id, n.port, n.online))
                            .collect(),
                    }),
                    Event::NeighborTableTimeout { .. } => timeouts += 1,
                    _ => {}
                });
                ours = owed.reply.map(|outbound| outbound.frame().to_vec());
            }
            Step::Advance { ms } => {
                unsafe { bm_wire_sys::bm_shim_advance_ticks(ms) };
                pump_until_quiet();
                let after = tick_count();
                expect_timeout = model.advance(after);
                // The heartbeat the advance also fired is not modelled: the
                // requester does not read the neighbour table, and the `0x08`
                // filter below ignores everything else on the wire.
                node.on_neighbor_request_timer(after, |event| match event {
                    Event::NeighborTableTimeout { .. } => timeouts += 1,
                    Event::NeighborTable { .. } => unreachable!("a timer carries no reply"),
                    _ => {}
                });
            }
        }

        // 1. The request each side put on the wire, byte for byte per port.
        let captured = drain();
        let c_requests = captured_table_requests(&captured);
        for (target, copies) in &c_requests {
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

        // 2. The reply callback, and 3. the timeout callback.
        assert_eq!(
            take_reported(),
            reported,
            "step {index} ({step:?}): the reply callbacks diverged"
        );
        assert_eq!(
            take_timeouts(),
            timeouts,
            "step {index} ({step:?}): the timeout callbacks diverged"
        );

        // And what the comparator believed was going on, against both sides.
        assert_eq!(
            reported.len(),
            usize::from(expect_reported),
            "step {index} ({step:?}): {model:?} expected reported={expect_reported}"
        );
        assert_eq!(
            timeouts,
            usize::from(expect_timeout),
            "step {index} ({step:?}): {model:?} expected timeout={expect_timeout}"
        );
        assert_state(index, step, &model, &node);
    }
}

/// Assert the port's requester state is what [`Model`] says the C's is.
///
/// The C has no accessor for any of the three, so this is the port being held
/// to the belief rather than to the C directly. The callback comparisons above
/// are what hold the belief to the C.
fn assert_state(index: usize, step: &Step, model: &Model, node: &TableNode) {
    let ours = node.table_requests();
    assert_eq!(
        (
            ours.target_node_id(),
            ours.is_armed(),
            ours.remaining_ms(tick_count())
        ),
        (
            model.target_node_id,
            model.armed,
            model.started_ms.map(|at| time_remaining(
                at,
                tick_count(),
                NEIGHBOR_REQUEST_TIMEOUT_MS
            ))
        ),
        "step {index} ({step:?}): the port's requester state left {model:?}"
    );
}
