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
//! Four things in the C outlive a single run, and all four are handled
//! rather than ignored:
//!
//! * the **sequence list** would grow, so [`check`] ends every run by
//!   sweeping until every request has been retried and timed out, and
//!   asserting the list is empty. That is what lets this target run in-process
//!   instead of needing `-fork=1`.
//! * each outstanding request holds a **reference to its frame**, which
//!   `timer_traverse_cb` re-sends on a retry. The comparator owns the frames,
//!   counts the C's references to them, and asserts every one has been let go
//!   by the end of the run.
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
//! [`crate::ll::LinkModel`] tracks the C's `previous` pointers so [`check`]
//! can decline to make that fourth call. **This is a domain restriction, never
//! a relaxed assertion**: every step the comparator does perform is compared
//! in full.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::header::{BCMP_HEADER_LEN, BCMP_HEADER_OFFSET};
use bm_wire::bcmp::registry::{
    Delivery, Expiry, MESSAGE_TIMER_EXPIRY_PERIOD_MS, PACKET_RETRY_COUNT, PacketCfg, Registry,
    RegistryError,
};
use bm_wire::bcmp::{BcmpHeader, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::ll::LinkModel;

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

/// The frames handed to `serialize`, keyed by address, with how many
/// references each has.
///
/// The comparator holds one reference while it serialises a frame. `serialize`
/// takes another through `increment` when it records a request, and
/// `sequence_list_remove_message` and `timer_traverse_cb` give it back through
/// `decrement`. A frame is freed at zero, so a re-send of a frame the C had let
/// go would be caught by [`resend`] rather than read freed memory.
static FRAMES: Mutex<Option<HashMap<usize, Held>>> = Mutex::new(None);

struct Held {
    frame: Box<[u8]>,
    refs: u32,
}

fn frames() -> MutexGuard<'static, Option<HashMap<usize, Held>>> {
    FRAMES.lock().unwrap_or_else(|p| p.into_inner())
}

/// Hand a frame to the C's side of the comparator, holding one reference.
fn adopt(frame: Vec<u8>) -> *mut u8 {
    let mut frame = frame.into_boxed_slice();
    let ptr = frame.as_mut_ptr();
    frames()
        .get_or_insert_with(HashMap::new)
        .insert(ptr as usize, Held { frame, refs: 1 });
    ptr
}

/// A copy of a frame the C may still hold.
fn frame_at(ptr: *mut u8) -> Vec<u8> {
    frames()
        .as_ref()
        .and_then(|frames| frames.get(&(ptr as usize)))
        .map(|held| held.frame.to_vec())
        .expect("a frame the comparator handed out")
}

/// Drop a reference, freeing the frame at zero.
fn release(ptr: *mut u8) {
    let mut frames = frames();
    let frames = frames.get_or_insert_with(HashMap::new);
    let held = frames
        .get_mut(&(ptr as usize))
        .expect("the C released a frame it was never given");
    held.refs -= 1;
    if held.refs == 0 {
        frames.remove(&(ptr as usize));
    }
}

unsafe extern "C" fn increment(payload: *mut c_void) {
    let mut frames = frames();
    frames
        .get_or_insert_with(HashMap::new)
        .get_mut(&(payload as usize))
        .expect("the C held a frame it was never given")
        .refs += 1;
}

unsafe extern "C" fn decrement(payload: *mut c_void) {
    release(payload.cast());
}

/// `PACKET.cb.send(element->buf)`: a retry. Recorded by the sequence number in
/// the frame's own header, which is what makes it the retry of one request
/// rather than another.
unsafe extern "C" fn resend(payload: *mut c_void) -> bm_wire_sys::BmErr {
    let frame = {
        let frames = frames();
        let held = frames
            .as_ref()
            .and_then(|frames| frames.get(&(payload as usize)))
            .expect("the C re-sent a frame it had already released");
        held.frame.to_vec()
    };
    let header = BcmpHeader::decode(&frame[BCMP_HEADER_OFFSET..]).expect("a serialised frame");
    push_event(Event::Resent {
        seq_num: header.seq_num,
    });
    bm_wire_sys::BmErr_BmOK
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
    /// A request's callback ran with nothing: it timed out.
    TimedOut {
        /// Which [`Step::Send`] issued the request.
        slot: u8,
    },
    /// `cfg->process` ran with zeroed data: a request sent without a callback
    /// timed out. The zeroed data says nothing about which one; the port's
    /// side knows.
    ProcessTimedOut,
    /// A request was re-sent from its held frame.
    Resent {
        /// The sequence number in the re-sent frame's header.
        seq_num: u32,
    },
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// How many bytes the next `payload` callback should copy out of the payload
/// it is handed. That form takes a bare `uint8_t *` with no length, so the
/// length has to come from the comparator.
static PAYLOAD_LEN: Mutex<usize> = Mutex::new(0);

fn push_event(event: Event) {
    EVENTS.lock().unwrap_or_else(|p| p.into_inner()).push(event);
}

fn take_events() -> Vec<Event> {
    std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|p| p.into_inner()))
}

/// `packet_timeout_occurred`, which is `static inline` in `packet.h`.
fn timed_out(data: &bm_wire_sys::BcmpProcessData) -> bool {
    data.header.is_null() && data.payload.is_null() && data.src.is_null() && data.dst.is_null()
}

unsafe extern "C" fn record_process(data: bm_wire_sys::BcmpProcessData) -> bm_wire_sys::BmErr {
    if timed_out(&data) {
        push_event(Event::ProcessTimedOut);
        return bm_wire_sys::BmErr_BmETIMEDOUT;
    }
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

/// A `payload` callback that knows which request it belongs to.
///
/// The C's signature carries no context, so the only way to tell one
/// request's callback from another's is to hand each request a different
/// function. One per possible [`Step::Send`] in a script.
unsafe fn record_payload(slot: u8, payload: *mut u8) -> bm_wire_sys::BmErr {
    if payload.is_null() {
        // `invoke_cb(element->cb, (BcmpProcessData){0})` hands a `payload`
        // callback the zeroed payload pointer: this is the timeout.
        push_event(Event::TimedOut { slot });
    } else {
        let len = *PAYLOAD_LEN.lock().unwrap_or_else(|p| p.into_inner());
        let payload = unsafe { std::slice::from_raw_parts(payload, len) }.to_vec();
        push_event(Event::Reply { slot, payload });
    }
    bm_wire_sys::BmErr_BmOK
}

/// A `full` callback that knows which request it belongs to. It sees the
/// length, so it needs no help from [`PAYLOAD_LEN`].
unsafe fn record_full(slot: u8, data: bm_wire_sys::BcmpProcessData) -> bm_wire_sys::BmErr {
    if timed_out(&data) {
        push_event(Event::TimedOut { slot });
    } else {
        let payload = unsafe { std::slice::from_raw_parts(data.payload, data.size as usize) };
        push_event(Event::Reply {
            slot,
            payload: payload.to_vec(),
        });
    }
    bm_wire_sys::BmErr_BmOK
}

macro_rules! sequenced_callbacks {
    ($($payload:ident, $full:ident => $slot:literal),* $(,)?) => {
        $(
            unsafe extern "C" fn $payload(payload: *mut u8) -> bm_wire_sys::BmErr {
                unsafe { record_payload($slot, payload) }
            }
            unsafe extern "C" fn $full(data: bm_wire_sys::BcmpProcessData) -> bm_wire_sys::BmErr {
                unsafe { record_full($slot, data) }
            }
        )*
        /// One callback per script slot. Even slots get the legacy `payload`
        /// form, which `bcmp/config.c` uses, and odd slots the `full` form
        /// `packet.h` recommends, so both of `invoke_cb`'s paths are compared.
        static CALLBACKS: &[bm_wire_sys::BcmpSequencedRequestCb] = &[$(
            if $slot % 2 == 0 {
                bm_wire_sys::BcmpSequencedRequestCb { full: None, payload: Some($payload) }
            } else {
                bm_wire_sys::BcmpSequencedRequestCb { full: Some($full), payload: None }
            }
        ),*];
    };
}

sequenced_callbacks!(
    cb00, full00 => 0, cb01, full01 => 1, cb02, full02 => 2, cb03, full03 => 3,
    cb04, full04 => 4, cb05, full05 => 5, cb06, full06 => 6, cb07, full07 => 7,
    cb08, full08 => 8, cb09, full09 => 9, cb10, full10 => 10, cb11, full11 => 11,
    cb12, full12 => 12, cb13, full13 => 13, cb14, full14 => 14, cb15, full15 => 15,
    cb16, full16 => 16, cb17, full17 => 17, cb18, full18 => 18, cb19, full19 => 19,
    cb20, full20 => 20, cb21, full21 => 21, cb22, full22 => 22, cb23, full23 => 23,
    cb24, full24 => 24, cb25, full25 => 25, cb26, full26 => 26, cb27, full27 => 27,
    cb28, full28 => 28, cb29, full29 => 29, cb30, full30 => 30, cb31, full31 => 31,
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
                bm_wire_sys::packet_init(bm_wire_sys::BcmpPacketCb {
                    src_ip: Some(get_src_ip),
                    dst_ip: Some(get_dst_ip),
                    data: Some(get_data),
                    checksum: Some(get_checksum),
                    increment: Some(increment),
                    decrement: Some(decrement),
                    send: Some(resend),
                }),
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
        /// Whether the request is given a callback. Without one, `serialize`
        /// stores `cfg->process` in its place, which then sees the reply, or
        /// zeroed data on a timeout.
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
                // The C may keep this one for re-sending, so the comparator
                // owns it and counts the C's references.
                let frame_c = adopt(blank_frame(body.len()));
                let mut frame_rs = blank_frame(body.len());
                let mut body_c = body.clone();

                let callback = if with_callback && makes_a_request {
                    assert!(
                        next_slot < CALLBACKS.len(),
                        "a script cannot send more requests than there are callbacks"
                    );
                    CALLBACKS[next_slot]
                } else {
                    bm_wire_sys::BcmpSequencedRequestCb::default()
                };
                let err = unsafe {
                    bm_wire_sys::serialize(
                        frame_c.cast(),
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
                assert_frames_eq(&frame_at(frame_c), &frame_rs, index, step);
                // `bcmp_tx`'s `bm_ip_tx_cleanup`: the sender's reference goes.
                release(frame_c);
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
/// what the C should have done, in the C's list order.
///
/// A retry is `PACKET.cb.send`. A timeout goes to the request's callback, or
/// — for a request sent without one — to `cfg->process` with zeroed data,
/// because `serialize` stored `cfg->process` as its `full` callback.
fn sweep(
    registry: &mut Registry<TYPE_CAPACITY, PENDING_CAPACITY>,
    slots: &mut Vec<Slot>,
    links: &mut LinkModel,
    now_ms: u32,
    index: usize,
) -> Vec<Event> {
    let mut expired = Vec::new();
    registry.on_tick(now_ms, |expiry| expired.push(expiry));

    let mut events = Vec::new();
    for expiry in expired {
        match expiry {
            Expiry::Retry(request) => events.push(Event::Resent {
                seq_num: request.seq_num,
            }),
            Expiry::TimedOut(request) => {
                let position = slots
                    .iter()
                    .position(|held| held.seq_num == request.seq_num)
                    .unwrap_or_else(|| panic!("step {index}: a request nothing sent expired"));
                let held = slots.remove(position);
                links.remove(position);
                events.push(if held.with_callback {
                    Event::TimedOut { slot: held.slot }
                } else {
                    Event::ProcessTimedOut
                });
            }
        }
    }
    events
}

/// Run the clock forward, one sweep at a time, until nothing is outstanding on
/// either side.
///
/// This is what keeps the target in-process: the C's `sequence_list` is the
/// one thing here that would otherwise grow without bound across a fuzz run.
/// One sweep at a time, because the shim fires every sweep a long advance
/// skipped at the same tick, and a request retried on the first of those is
/// not expired again on the rest: each retry needs a sweep of its own.
fn drain(
    registry: &mut Registry<TYPE_CAPACITY, PENDING_CAPACITY>,
    slots: &mut Vec<Slot>,
    links: &mut LinkModel,
    resume: &mut Resume,
) {
    // The first sweep a request can expire on, then one per retry.
    for _ in 0..=u32::from(PACKET_RETRY_COUNT) + 1 {
        unsafe { bm_wire_sys::bm_shim_advance_ticks(MESSAGE_TIMER_EXPIRY_PERIOD_MS) };
        let now = tick_count();
        resume.advance_timer(now);
        let expected = sweep(registry, slots, links, now, usize::MAX);

        let seen = take_events();
        assert_eq!(
            seen, expected,
            "draining: the C's retries and timeouts diverged from the port's"
        );
    }
    assert_eq!(
        registry.pending_len(),
        0,
        "draining: the port is still holding requests"
    );
    assert!(slots.is_empty(), "draining: unaccounted requests {slots:?}");

    // And nothing is left to fire: if the C still held entries, a later sweep
    // would report them.
    for _ in 0..=u32::from(PACKET_RETRY_COUNT) {
        unsafe { bm_wire_sys::bm_shim_advance_ticks(MESSAGE_TIMER_EXPIRY_PERIOD_MS) };
        resume.advance_timer(tick_count());
    }
    let stragglers = take_events();
    assert!(
        stragglers.is_empty(),
        "draining: the C's sequence list still held {stragglers:?}"
    );
    let held = frames().as_ref().map_or(0, HashMap::len);
    assert_eq!(held, 0, "draining: the C still holds {held} frames");
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
