//! Differential comparator for [`bm_wire::bcmp::registry`] against the state
//! around `serialize` and `process_received_message` in `bcmp/packet.c`.
//!
//! [`crate::bcmp`] compares the *bytes* those two functions produce. This one
//! compares the *decisions* they make: which sequence number an outgoing
//! message gets, whether a received reply is matched to an outstanding
//! request, which callback is invoked with what, and when an unanswered
//! request is given up on. A script of sends, receives and clock advances is
//! applied to both implementations and the resulting stream of callbacks is
//! compared after every step.
//!
//! # This one needs a process to itself
//!
//! Not because it brings the stack up — it does not — but because it owns
//! `packet.c`'s file-scope `PACKET`. It calls `packet_init` with its own
//! accessors and registers its own types, exactly as [`crate::bcmp`] does with
//! different ones, and whichever runs second loses: `packet_init` overwrites
//! the accessors, and `packet_add` appends to a registry whose first match
//! wins. So this module is driven from `bm-wire-diff/tests/registry.rs`, which
//! cargo runs as its own binary, and its seeds live in
//! [`crate::replay::STACK_TARGETS`]. **Nothing here may have a `#[cfg(test)]`
//! test of its own**, because those run in the library test binary alongside
//! [`crate::bcmp`].
//!
//! `bm_shim_reset` must not be called here either, for the reason
//! [`crate::bcmp`] gives: `packet_init` hands `PACKET` a shim mutex and a shim
//! timer that `packet.c` has no way to let go of.
//!
//! # What accumulates, and what does not
//!
//! Three things in the C outlive a single run, and all three are handled
//! rather than ignored:
//!
//! * the **sequence list** would grow, so [`check`] ends every run by
//!   advancing the clock past the expiry sweep and asserting the list is
//!   empty. That is what lets this target run in-process instead of needing
//!   `-fork=1`.
//! * `message_count` never resets, so the comparator mirrors it in
//!   `RESUME` and starts each fresh [`Registry`] from where the C is.
//! * the **expiry timer's phase** never resets either: it was armed at tick 0
//!   and fires every [`MESSAGE_TIMER_EXPIRY_PERIOD_MS`] for the life of the
//!   process, which after the tick counter wraps is no longer a multiple of
//!   150. `RESUME` mirrors that too, by transcribing `fire_due_timers` from
//!   `csrc/bm_os_shim.c` rather than by asking the code under test.
//!
//! # The domain is constrained, and this is why
//!
//! `ll_remove` in `common/ll.c` leaves `LL::tail` pointing at freed memory
//! when a node is unlinked from the middle of the list and the new tail is
//! then unlinked too; the next `ll_item_add` writes through it. That is
//! divergence #20, and it is reachable from here — three outstanding requests,
//! a reply to the middle one, a reply to the last one, then another request.
//! `LinkModel` tracks the C's `previous` pointers so [`check`] can decline
//! to make that fourth call. **This is a domain restriction, never a relaxed
//! assertion**: every step the comparator does perform is compared in full.

use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::header::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET};
use bm_wire::bcmp::registry::{
    Delivery, MESSAGE_TIMER_EXPIRY_PERIOD_MS, PacketCfg, Registry, RegistryError,
};
use bm_wire::bcmp::{BcmpHeader, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

use crate::Domain;

/// Message types this comparator registers, with the configuration each gets.
///
/// The `sequenced_request`/`sequenced_reply` assignment for the config types
/// is bm_core's own, from `bcmp_config_init` at `config.c:787` — `config.c` is
/// the only module upstream that sets `sequenced_request` at all, so it is the
/// only real-world example of the machinery under test. The neighbour-proto
/// pair is the one `test/src/packet_test.cpp` drives, and the last entry sets
/// both flags, which nothing upstream does but the C's branches allow.
pub const REGISTERED: &[(MessageType, PacketCfg)] = &[
    (MessageType::CONFIG_GET, PacketCfg::REQUEST),
    (MessageType::CONFIG_SET, PacketCfg::REQUEST),
    (MessageType::CONFIG_STATUS_REQUEST, PacketCfg::REQUEST),
    (MessageType::NEIGHBOR_PROTO_REQUEST, PacketCfg::REQUEST),
    (MessageType::CONFIG_VALUE, PacketCfg::REPLY),
    (MessageType::CONFIG_STATUS_RESPONSE, PacketCfg::REPLY),
    (MessageType::NEIGHBOR_PROTO_REPLY, PacketCfg::REPLY),
    (MessageType::CONFIG_COMMIT, PacketCfg::UNSEQUENCED),
    (MessageType::HEARTBEAT, PacketCfg::UNSEQUENCED),
    (
        MessageType::NET_ASSERT_QUIET,
        PacketCfg {
            sequenced_reply: true,
            sequenced_request: true,
        },
    ),
];

/// A type nothing registers, so the comparator can exercise the paths where
/// the C finds no configuration: `serialize` writes nothing at all, and
/// `process_received_message` validates the frame and dispatches nothing.
pub const UNREGISTERED: MessageType = MessageType(0x4242);

/// Capacity of the port's type registry. One spare, so a run can never be
/// comparing a full registry against the C's unbounded one by accident.
pub const TYPE_CAPACITY: usize = REGISTERED.len() + 1;

/// Capacity of the port's outstanding-request table.
///
/// One per step, which is the most a single script can create, so the port's
/// table never fills where the C's list would not have.
pub const PENDING_CAPACITY: usize = MAX_STEPS;

/// Most steps a single input may carry.
pub const MAX_STEPS: usize = 32;

/// Longest a single [`Step::Advance`] may push the virtual clock.
///
/// Around seven expiry sweeps, which is enough to exercise the shim timer's
/// catch-up without making the tick counter race towards its wrap.
pub const MAX_ADVANCE_MS: u32 = 1_000;

/// Longest body a step may carry. Small: this comparator is about dispatch,
/// not about payload size, which [`crate::bcmp`] covers.
pub const MAX_BODY: usize = 32;

// ---------------------------------------------------------------------------
// The oracle
// ---------------------------------------------------------------------------

// The accessors `packet_init` needs, in `network/bm_linux.c`'s shape: the
// handle is a pointer to an Ethernet + IPv6 frame. Same layout as
// `crate::bcmp` uses, and for the same reason — it is what bm_core's own IP
// backend hands `packet.c`.

unsafe extern "C" fn get_src_ip(payload: *mut c_void) -> *mut bm_wire_sys::BmIpAddr {
    unsafe { payload.cast::<u8>().add(IPV6_SOURCE_ADDRESS_OFFSET).cast() }
}

unsafe extern "C" fn get_dst_ip(payload: *mut c_void) -> *mut bm_wire_sys::BmIpAddr {
    unsafe {
        payload
            .cast::<u8>()
            .add(IPV6_DESTINATION_ADDRESS_OFFSET)
            .cast()
    }
}

unsafe extern "C" fn get_data(payload: *mut c_void) -> *mut c_void {
    unsafe { payload.cast::<u8>().add(BCMP_HEADER_OFFSET).cast() }
}

unsafe extern "C" fn get_checksum(payload: *mut c_void, size: u32) -> u16 {
    unsafe {
        bm_wire_sys::ipv6_pseudo_checksum(
            get_src_ip(payload),
            get_dst_ip(payload),
            IP_PROTO_BCMP,
            size,
            get_data(payload),
        )
    }
}

/// Something the C called back about, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `cfg->process` ran: the message was not a reply to anything
    /// outstanding, or it was and the request carried no callback.
    Process {
        /// The type in the received header.
        message_type: MessageType,
        /// The sequence number in the received header.
        seq_num: u32,
        /// The body, of the length the caller said it was.
        payload: Vec<u8>,
    },
    /// A request's `BcmpSequencedRequestCb` ran with a reply's payload.
    Reply {
        /// Which [`Step::Send`] issued the request, by its index in the script.
        slot: u8,
        /// The reply's body.
        payload: Vec<u8>,
    },
    /// A request's `BcmpSequencedRequestCb` ran with `NULL`: it timed out.
    TimedOut {
        /// Which [`Step::Send`] issued the request.
        slot: u8,
    },
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// How many bytes the next sequenced callback should copy out of the payload
/// it is handed. The C's `BcmpSequencedRequestCb` takes a bare `uint8_t *`
/// with no length, so the length has to come from the comparator.
static PAYLOAD_LEN: Mutex<usize> = Mutex::new(0);

fn push_event(event: Event) {
    EVENTS.lock().unwrap_or_else(|p| p.into_inner()).push(event);
}

fn take_events() -> Vec<Event> {
    std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|p| p.into_inner()))
}

unsafe extern "C" fn record_process(data: bm_wire_sys::BcmpProcessData) -> bm_wire_sys::BmErr {
    let (message_type, seq_num, payload) = unsafe {
        let header = std::slice::from_raw_parts(data.header.cast::<u8>(), BCMP_HEADER_LEN);
        let header = BcmpHeader::decode(header).expect("13 bytes is a header");
        (
            header.message_type,
            header.seq_num,
            std::slice::from_raw_parts(data.payload, data.size as usize).to_vec(),
        )
    };
    push_event(Event::Process {
        message_type,
        seq_num,
        payload,
    });
    bm_wire_sys::BmErr_BmOK
}

/// A `BcmpSequencedRequestCb` that knows which request it belongs to.
///
/// The C's signature carries no context, so the only way to tell one
/// request's callback from another's is to hand each request a different
/// function. One per possible [`Step::Send`] in a script.
unsafe fn record_sequenced(slot: u8, payload: *mut u8) -> bm_wire_sys::BmErr {
    if payload.is_null() {
        // `timer_traverse_cb` calls `element->cb(NULL)`: this is the timeout.
        push_event(Event::TimedOut { slot });
    } else {
        let len = *PAYLOAD_LEN.lock().unwrap_or_else(|p| p.into_inner());
        let payload = unsafe { std::slice::from_raw_parts(payload, len) }.to_vec();
        push_event(Event::Reply { slot, payload });
    }
    bm_wire_sys::BmErr_BmOK
}

macro_rules! sequenced_callbacks {
    ($($name:ident => $slot:literal),* $(,)?) => {
        $(
            unsafe extern "C" fn $name(payload: *mut u8) -> bm_wire_sys::BmErr {
                unsafe { record_sequenced($slot, payload) }
            }
        )*
        /// One callback per script slot; see [`record_sequenced`].
        static CALLBACKS: &[bm_wire_sys::BcmpSequencedRequestCb] = &[$(Some($name)),*];
    };
}

sequenced_callbacks!(
    cb00 => 0, cb01 => 1, cb02 => 2, cb03 => 3, cb04 => 4, cb05 => 5, cb06 => 6, cb07 => 7,
    cb08 => 8, cb09 => 9, cb10 => 10, cb11 => 11, cb12 => 12, cb13 => 13, cb14 => 14, cb15 => 15,
    cb16 => 16, cb17 => 17, cb18 => 18, cb19 => 19, cb20 => 20, cb21 => 21, cb22 => 22, cb23 => 23,
    cb24 => 24, cb25 => 25, cb26 => 26, cb27 => 27, cb28 => 28, cb29 => 29, cb30 => 30, cb31 => 31,
);

/// The C state that outlives a run, mirrored independently of the code under
/// test so that seeding a fresh [`Registry`] from it is not circular.
#[derive(Debug, Clone, Copy)]
struct Resume {
    /// `serialize`'s `message_count` static.
    sequence_count: u32,
    /// The tick `PACKET.timer` next fires on.
    next_sweep_ms: u32,
}

static RESUME: Mutex<Resume> = Mutex::new(Resume {
    sequence_count: 0,
    // `packet_init` runs at tick 0 and `bm_timer_start` arms the timer at
    // `CTX.tick + period`.
    next_sweep_ms: MESSAGE_TIMER_EXPIRY_PERIOD_MS,
});

impl Resume {
    /// Advance the timer to where `fire_due_timers` would leave it after the
    /// clock reached `now_ms`. Transcribed from `csrc/bm_os_shim.c`.
    fn advance_timer(&mut self, now_ms: u32) {
        while (now_ms.wrapping_sub(self.next_sweep_ms) as i32) >= 0 {
            self.next_sweep_ms = self
                .next_sweep_ms
                .wrapping_add(MESSAGE_TIMER_EXPIRY_PERIOD_MS);
        }
    }
}

static ORACLE: OnceLock<Mutex<()>> = OnceLock::new();

/// Bring `packet.c` up once, with this module's registrations, then serialise
/// every use of it.
///
/// # Panics
///
/// If `packet_init` or any `packet_add` fails.
fn oracle() -> MutexGuard<'static, ()> {
    let lock = ORACLE.get_or_init(|| {
        unsafe {
            assert_eq!(
                bm_wire_sys::packet_init(
                    Some(get_src_ip),
                    Some(get_dst_ip),
                    Some(get_data),
                    Some(get_checksum),
                ),
                bm_wire_sys::BmErr_BmOK,
                "packet_init"
            );
            assert_eq!(
                bm_wire_sys::bm_shim_tick_count(),
                0,
                "the expiry timer's phase is read from a clock that starts here"
            );
            for (ty, cfg) in REGISTERED {
                let mut c_cfg = bm_wire_sys::BcmpPacketCfg {
                    sequenced_reply: cfg.sequenced_reply,
                    sequenced_request: cfg.sequenced_request,
                    process: Some(record_process),
                };
                assert_eq!(
                    bm_wire_sys::packet_add(&mut c_cfg, u32::from(ty.0)),
                    bm_wire_sys::BmErr_BmOK,
                    "packet_add({ty:?})"
                );
            }
        }
        Mutex::new(())
    });
    lock.lock().unwrap_or_else(|p| p.into_inner())
}

fn tick_count() -> u32 {
    unsafe { bm_wire_sys::bm_shim_tick_count() }
}

// ---------------------------------------------------------------------------
// The C's linked-list shape, modelled just far enough to stay out of #20
// ---------------------------------------------------------------------------

/// A model of `PACKET.sequence_list`'s link structure.
///
/// Only the `previous` pointers matter, and only for the tail: `ll_remove`
/// reads `current->previous` when it unlinks a node that is the tail but not
/// the head, and does not fix up the `previous` of a node whose predecessor it
/// just freed. So the tail's `previous` can dangle, and `ll_item_add`
/// dereferences `LL::tail` on the very next append. See divergence #20.
#[derive(Debug, Default)]
struct LinkModel {
    /// `(node id, id of the node that was the tail when this one was added)`,
    /// in list order.
    nodes: Vec<(u64, Option<u64>)>,
    next_id: u64,
    /// `LL::tail` points at a freed node.
    tail_dangling: bool,
}

impl LinkModel {
    /// Whether an `ll_item_add` right now would write through a dangling
    /// `LL::tail`. The list emptying through the head branch clears the
    /// hazard: `ll_item_add` looks at `LL::head`, and takes the branch that
    /// rebuilds both pointers when it is null.
    fn add_is_undefined(&self) -> bool {
        self.tail_dangling && !self.nodes.is_empty()
    }

    fn add(&mut self) {
        assert!(
            !self.add_is_undefined(),
            "the comparator must not append through a dangling tail"
        );
        let previous = self.nodes.last().map(|(id, _)| *id);
        self.nodes.push((self.next_id, previous));
        self.next_id += 1;
    }

    fn remove(&mut self, index: usize) {
        let is_tail = index + 1 == self.nodes.len();
        if is_tail && self.nodes.len() >= 2 {
            let previous = self.nodes[index].1;
            let alive = previous.is_some_and(|id| self.nodes.iter().any(|(n, _)| *n == id));
            if !alive {
                self.tail_dangling = true;
            }
        }
        self.nodes.remove(index);
        if self.nodes.is_empty() {
            self.tail_dangling = false;
        }
    }
}

// ---------------------------------------------------------------------------
// The input
// ---------------------------------------------------------------------------

/// Which sequence number a received message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqChoice {
    /// The number of an outstanding request, so the reply actually matches.
    /// Reduced modulo the number outstanding; falls back to `Raw(0)` when
    /// nothing is outstanding.
    Outstanding(u8),
    /// Whatever the fuzzer picked, which almost never matches.
    Raw(u32),
}

/// One thing that happens to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A message is serialised for transmission.
    Send {
        /// Index into [`REGISTERED`]; one past the end selects [`UNREGISTERED`].
        type_index: u8,
        /// The number a reply type echoes; ignored for anything else.
        reply_seq_num: u32,
        /// Whether the request is given a `BcmpSequencedRequestCb`. The C
        /// tolerates a null one and falls back to `cfg->process` on the reply.
        with_callback: bool,
        /// Body length, capped at [`MAX_BODY`].
        body_len: u8,
    },
    /// A message arrives.
    Receive {
        /// Index into [`REGISTERED`]; one past the end selects [`UNREGISTERED`].
        type_index: u8,
        /// The sequence number in its header.
        seq: SeqChoice,
        /// Body length, capped at [`MAX_BODY`].
        body_len: u8,
    },
    /// Time passes, and the expiry sweep runs if it comes due.
    Advance {
        /// Milliseconds, capped at [`MAX_ADVANCE_MS`].
        ms: u32,
    },
    /// Time passes until the next expiry sweep is exactly `ms` away.
    ///
    /// The sweep's phase is process-global — the timer was armed once, at tick
    /// zero, and nothing resets it — so a script that wants to straddle the
    /// timeout boundary cannot get there by advancing a fixed amount: where a
    /// plain [`Step::Advance`] lands depends on every run that came before.
    /// This lands on the phase instead. Reaching the requested offset may
    /// cross one sweep on the way, which is an ordinary advance and compared
    /// as one.
    AlignBeforeSweep {
        /// How far the next sweep should be, 1..=[`MESSAGE_TIMER_EXPIRY_PERIOD_MS`].
        ms: u32,
    },
}

impl Step {
    fn clamp(&mut self) {
        let limit = REGISTERED.len() as u8 + 1;
        match self {
            Self::Send {
                type_index,
                body_len,
                ..
            }
            | Self::Receive {
                type_index,
                body_len,
                ..
            } => {
                *type_index %= limit;
                *body_len %= MAX_BODY as u8 + 1;
            }
            Self::Advance { ms } => *ms %= MAX_ADVANCE_MS + 1,
            Self::AlignBeforeSweep { ms } => {
                // Folded only when out of range, so a hand-written script gets
                // the offset it asked for rather than one next to it.
                if *ms == 0 || *ms > MESSAGE_TIMER_EXPIRY_PERIOD_MS {
                    *ms = *ms % MESSAGE_TIMER_EXPIRY_PERIOD_MS + 1;
                }
            }
        }
    }
}

/// A sequence of steps applied to both registries.
#[derive(Debug, Clone)]
pub struct RegistryInput {
    /// The steps, capped at [`MAX_STEPS`] by [`Domain`].
    pub steps: Vec<Step>,
}

impl<'a> Arbitrary<'a> for RegistryInput {
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

/// One step, decoded from the front of `u` as fixed-width little-endian
/// fields.
///
/// Deliberately plain: `u8` for the tag rather than `int_in_range`, so a seed
/// file can be written by hand — `bm-wire/fuzz/seeds/registry/` is all
/// hand-written scripts, and their bytes have to be predictable.
fn arbitrary_step(u: &mut Unstructured<'_>) -> Result<Step> {
    let tag: u8 = u.arbitrary()?;
    let mut step = match tag % 4 {
        0 => Step::Send {
            type_index: u.arbitrary()?,
            reply_seq_num: u.arbitrary()?,
            with_callback: u.arbitrary()?,
            body_len: u.arbitrary()?,
        },
        1 => Step::Receive {
            type_index: u.arbitrary()?,
            seq: if u.arbitrary()? {
                SeqChoice::Outstanding(u.arbitrary()?)
            } else {
                SeqChoice::Raw(u.arbitrary()?)
            },
            body_len: u.arbitrary()?,
        },
        2 => Step::Advance { ms: u.arbitrary()? },
        _ => Step::AlignBeforeSweep { ms: u.arbitrary()? },
    };
    step.clamp();
    Ok(step)
}

impl Domain for RegistryInput {
    fn clamp_to_domain(&mut self) {
        self.steps.truncate(MAX_STEPS);
        for step in &mut self.steps {
            step.clamp();
        }
    }
}

/// The type a step selects, and the configuration it was registered with.
fn message_type(type_index: u8) -> (MessageType, Option<PacketCfg>) {
    match REGISTERED.get(usize::from(type_index)) {
        Some((ty, cfg)) => (*ty, Some(*cfg)),
        None => (UNREGISTERED, None),
    }
}

/// A body whose bytes identify the step that produced it, so a payload that
/// reaches the wrong callback is visible in the failure message.
fn body_for(step_index: usize, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (step_index as u8).wrapping_mul(31).wrapping_add(i as u8))
        .collect()
}

/// A frame with the IPv6 header filled in and room for `body_len` BCMP bytes.
fn blank_frame(body_len: usize) -> Vec<u8> {
    let payload_len = BCMP_HEADER_LEN + body_len;
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
    frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
    frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
        .copy_from_slice(&(payload_len as u16).to_be_bytes());
    frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
    frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
        .copy_from_slice(&bm_wire::addr::nodeid_to_ip(0xFE80_0000, 0x55AA_0011).0);
    frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
        .copy_from_slice(&BmIpAddr::LINK_LOCAL_MULTICAST.0);
    frame
}

// ---------------------------------------------------------------------------
// The comparator
// ---------------------------------------------------------------------------

/// What the comparator remembers about a request it sent, in the order the
/// C's `sequence_list` holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    /// Which of [`CALLBACKS`] the request was given, if any. Fixed at send
    /// time, so it survives entries being removed from in front of it.
    slot: u8,
    /// The sequence number the C stamped into the request.
    seq_num: u32,
    /// Whether it was given a callback at all.
    with_callback: bool,
}

/// Apply the same script to `packet.c` and to [`Registry`], and assert they
/// make the same decisions at every step.
///
/// # Panics
///
/// If the two disagree about a sequence number, about which callback a message
/// reaches, or about when a request times out — or if the C is left holding
/// outstanding requests once the run has drained.
#[allow(clippy::too_many_lines)]
pub fn check(input: &RegistryInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let _guard = oracle();
    let mut resume = *RESUME.lock().unwrap_or_else(|p| p.into_inner());
    let mut registry = Registry::<TYPE_CAPACITY, PENDING_CAPACITY>::resuming(
        resume.sequence_count,
        resume.next_sweep_ms,
    );
    for (ty, cfg) in REGISTERED {
        registry.add(*ty, *cfg).expect("TYPE_CAPACITY is sized");
    }
    let mut links = LinkModel::default();
    // Whatever a previous run left behind, which should be nothing.
    assert!(
        take_events().is_empty(),
        "a previous run left callbacks pending"
    );

    // The requests the C is holding, in its list order.
    let mut slots: Vec<Slot> = Vec::new();
    let mut next_slot = 0usize;

    for (index, step) in input.steps.iter().enumerate() {
        let expected: Vec<Event> = match *step {
            Step::Send {
                type_index,
                reply_seq_num,
                with_callback,
                body_len,
            } => {
                let (ty, cfg) = message_type(type_index);
                let makes_a_request =
                    cfg.is_some_and(|cfg| cfg.sequenced_request && !cfg.sequenced_reply);
                if makes_a_request && links.add_is_undefined() {
                    // Divergence #20: the next `ll_item_add` would write
                    // through a freed pointer. Skip the step on both sides
                    // rather than comparing against undefined behaviour.
                    continue;
                }

                let body = body_for(index, usize::from(body_len));
                let mut frame_c = blank_frame(body.len());
                let mut frame_rs = frame_c.clone();
                let mut body_c = body.clone();

                let callback = if with_callback && makes_a_request {
                    assert!(
                        next_slot < CALLBACKS.len(),
                        "a script cannot send more requests than there are callbacks"
                    );
                    CALLBACKS[next_slot]
                } else {
                    None
                };
                let err = unsafe {
                    bm_wire_sys::serialize(
                        frame_c.as_mut_ptr().cast(),
                        body_c.as_mut_ptr().cast(),
                        body_c.len() as u32,
                        u32::from(ty.0),
                        reply_seq_num,
                        callback,
                    )
                };

                match registry.on_serialize(tick_count(), ty, reply_seq_num) {
                    Ok(outgoing) => {
                        assert_eq!(
                            err,
                            bm_wire_sys::BmErr_BmOK,
                            "step {index} ({step:?}): the port serialised {ty:?} and the C refused"
                        );
                        tx::serialize(&mut frame_rs, ty, outgoing.seq_num, &body)
                            .expect("blank_frame is sized for the body");
                        assert_eq!(
                            outgoing.tracked,
                            makes_a_request,
                            "step {index} ({step:?}): the port {} where the C {}",
                            if outgoing.tracked {
                                "tracked the request"
                            } else {
                                "tracked nothing"
                            },
                            if makes_a_request {
                                "appended to its sequence list"
                            } else {
                                "did not"
                            },
                        );
                        if outgoing.tracked {
                            assert_eq!(
                                outgoing.seq_num, resume.sequence_count,
                                "step {index}: the outgoing sequence number left the C's counter"
                            );
                            resume.sequence_count = resume.sequence_count.wrapping_add(1);
                            links.add();
                            slots.push(Slot {
                                slot: next_slot as u8,
                                seq_num: outgoing.seq_num,
                                with_callback,
                            });
                            next_slot += 1;
                        }
                    }
                    Err(RegistryError::UnknownType) => {
                        assert_eq!(
                            err,
                            bm_wire_sys::BmErr_BmENODEV,
                            "step {index} ({step:?}): the port refused {ty:?} and the C did not"
                        );
                    }
                    Err(other) => panic!("step {index} ({step:?}): the port failed with {other}"),
                }
                assert_frames_eq(&frame_c, &frame_rs, index, step);
                Vec::new()
            }

            Step::Receive {
                type_index,
                seq,
                body_len,
            } => {
                let (ty, _) = message_type(type_index);
                let seq_num = match seq {
                    SeqChoice::Raw(seq_num) => seq_num,
                    SeqChoice::Outstanding(which) => match registry.pending_len() {
                        0 => 0,
                        len => {
                            registry
                                .pending()
                                .nth(usize::from(which) % len)
                                .expect("in range")
                                .seq_num
                        }
                    },
                };
                let body = body_for(index, usize::from(body_len));
                let mut frame = blank_frame(body.len());
                tx::serialize(&mut frame, ty, seq_num, &body).expect("blank_frame is sized");

                // What the port says should happen, decided before the C runs.
                let expected = match registry.on_received(ty, seq_num) {
                    Delivery::Unregistered => Vec::new(),
                    Delivery::Process => vec![Event::Process {
                        message_type: ty,
                        seq_num,
                        payload: body.clone(),
                    }],
                    Delivery::SequencedReply(request) => {
                        let position = slots
                            .iter()
                            .position(|held| held.seq_num == request.seq_num)
                            .unwrap_or_else(|| {
                                panic!("step {index}: the port matched a request nothing sent")
                            });
                        let held = slots.remove(position);
                        links.remove(position);
                        if held.with_callback {
                            vec![Event::Reply {
                                slot: held.slot,
                                payload: body.clone(),
                            }]
                        } else {
                            // A null `cb` falls through to `cfg->process`,
                            // with the entry already removed.
                            vec![Event::Process {
                                message_type: ty,
                                seq_num,
                                payload: body.clone(),
                            }]
                        }
                    }
                };

                *PAYLOAD_LEN.lock().unwrap_or_else(|p| p.into_inner()) = body.len();
                unsafe {
                    bm_wire_sys::process_received_message(
                        frame.as_mut_ptr().cast(),
                        body.len() as u32,
                    )
                };
                expected
            }

            Step::Advance { ms } => {
                unsafe { bm_wire_sys::bm_shim_advance_ticks(ms) };
                let now = tick_count();
                resume.advance_timer(now);
                sweep(&mut registry, &mut slots, &mut links, now, index)
            }

            Step::AlignBeforeSweep { ms } => {
                // `advance_timer` leaves the next sweep strictly ahead of the
                // clock and at most one period away, so this distance is in
                // 1..=150 and the arithmetic below cannot underflow.
                let distance = resume.next_sweep_ms.wrapping_sub(tick_count());
                let advance = if ms <= distance {
                    distance - ms
                } else {
                    distance + MESSAGE_TIMER_EXPIRY_PERIOD_MS - ms
                };
                unsafe { bm_wire_sys::bm_shim_advance_ticks(advance) };
                let now = tick_count();
                resume.advance_timer(now);
                assert_eq!(
                    resume.next_sweep_ms.wrapping_sub(now),
                    ms,
                    "step {index}: aligning to {ms} ms before the sweep missed"
                );
                sweep(&mut registry, &mut slots, &mut links, now, index)
            }
        };

        let seen = take_events();
        assert_eq!(
            seen, expected,
            "step {index} ({step:?}): the C's callbacks diverged from the port's decisions\n  \
             outstanding: {slots:?}"
        );
    }

    drain(&mut registry, &mut slots, &mut links, &mut resume);
    *RESUME.lock().unwrap_or_else(|p| p.into_inner()) = resume;
}

/// Run the port's expiry sweep at `now_ms` and translate what it reports into
/// the callbacks the C should have made.
///
/// `timer_traverse_cb` invokes the callback only when the request has one, so
/// a request sent without one expires silently — the port reports it either
/// way, because it has no callback to test.
fn sweep(
    registry: &mut Registry<TYPE_CAPACITY, PENDING_CAPACITY>,
    slots: &mut Vec<Slot>,
    links: &mut LinkModel,
    now_ms: u32,
    index: usize,
) -> Vec<Event> {
    let mut expired = Vec::new();
    registry.on_tick(now_ms, |request| expired.push(request.seq_num));

    let mut events = Vec::new();
    for seq_num in expired {
        let position = slots
            .iter()
            .position(|held| held.seq_num == seq_num)
            .unwrap_or_else(|| panic!("step {index}: a request nothing sent expired"));
        let held = slots.remove(position);
        links.remove(position);
        if held.with_callback {
            events.push(Event::TimedOut { slot: held.slot });
        }
    }
    events
}

/// Run the clock forward until nothing is outstanding on either side.
///
/// This is what keeps the target in-process: the C's `sequence_list` is the
/// one thing here that would otherwise grow without bound across a fuzz run.
fn drain(
    registry: &mut Registry<TYPE_CAPACITY, PENDING_CAPACITY>,
    slots: &mut Vec<Slot>,
    links: &mut LinkModel,
    resume: &mut Resume,
) {
    // Two full sweep periods clear anything, wherever it fell in the phase.
    unsafe { bm_wire_sys::bm_shim_advance_ticks(2 * MESSAGE_TIMER_EXPIRY_PERIOD_MS) };
    let now = tick_count();
    resume.advance_timer(now);
    let expected = sweep(registry, slots, links, now, usize::MAX);

    let seen = take_events();
    assert_eq!(
        seen, expected,
        "draining: the C's timeouts diverged from the port's"
    );
    assert_eq!(
        registry.pending_len(),
        0,
        "draining: the port is still holding requests"
    );
    assert!(slots.is_empty(), "draining: unaccounted requests {slots:?}");

    // And nothing is left to fire: if the C still held entries, the next sweep
    // would report them.
    unsafe { bm_wire_sys::bm_shim_advance_ticks(2 * MESSAGE_TIMER_EXPIRY_PERIOD_MS) };
    resume.advance_timer(tick_count());
    let stragglers = take_events();
    assert!(
        stragglers.is_empty(),
        "draining: the C's sequence list still held {stragglers:?}"
    );
}

fn assert_frames_eq(c: &[u8], rs: &[u8], index: usize, step: &Step) {
    if c == rs {
        return;
    }
    let at = c
        .iter()
        .zip(rs)
        .position(|(a, b)| a != b)
        .unwrap_or(c.len().min(rs.len()));
    panic!(
        "step {index} ({step:?}): serialized frames diverged at byte {at}: C {:#04x?}, Rust {:#04x?}\n  C:    {c:02x?}\n  Rust: {rs:02x?}",
        c.get(at),
        rs.get(at),
    );
}
