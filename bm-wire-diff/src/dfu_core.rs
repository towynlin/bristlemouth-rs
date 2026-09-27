//! Differential comparator for [`bm_wire::bcmp::dfu_core`], against
//! `bcmp/dfu_core.c` running in bm_core's live stack.
//!
//! # What is compared
//!
//! A script of [`Step`]s is applied to both machines. After every step
//! [`check`] asserts that the two agree on:
//!
//! | Observable | C side |
//! |---|---|
//! | state | `get_current_state_enum(bm_dfu_test_get_sm_ctx())` |
//! | error, `internal` | `bm_dfu_get_error`, `bm_dfu_internal` |
//! | the queue, event by event, bodies included | drained from `bm_dfu_get_event_queue()` and put back |
//! | reboot info | `client_update_reboot_info` |
//! | finish callbacks | an `UpdateFinishCb` that records its arguments |
//! | frames sent | the capture ring, against the same bodies sent through a [`bm_stack::Node`] |
//!
//! and that [`Dfu::on_message`]'s verdict is the one [`Model`] predicts from
//! `bm_dfu_process_message`'s checks.
//!
//! Messages go to `bm_dfu_process_message` directly, not through the wire:
//! `dfu_copy_and_process_message`'s forwarding half is `bm-stack`'s.
//!
//! The C's DFU task is never allowed to run an event. [`Oracle::run`] does
//! what one iteration of `bm_dfu_event_thread` does, through
//! `bm_dfu_test_set_dfu_event_and_run_sm`, and the queue is emptied around
//! every pump so the task finds nothing to take.
//!
//! # Input domain: the core, and one step of the host
//!
//! The client and host are cards D3 and D4. Until they land, an event that
//! would reach their logic is **discarded on both sides** at the moment it
//! would run, rather than run:
//!
//! * a `ReceivedUpdateRequest` in `Idle` (`bm_dfu_client_process_update_request`);
//! * an `AckReceived`, `AckTimeout` or `Abort` in `HostReqUpdate`;
//! * a `BeginHost` in `Idle` while `bm_dfu_internal()` is false:
//!   `s_host_req_update_entry` then creates a stream buffer that only
//!   `HostUpdate`'s exit frees, so leaving `HostReqUpdate` any other way leaks
//!   it (divergence #61) and LeakSanitizer stops the fuzzer.
//!
//! What remains is `Init`, `Idle`, `Error` and entry into `HostReqUpdate`,
//! whose C (`s_host_req_update_entry`) sends the `0xD0` it was given and
//! records the client — [`ReqUpdateOnly`] does exactly that, and nothing else.
//! Every other event `s_host_req_update_run` ignores. That is enough to reach
//! the finish callback, the fatal error and the stale `BeginHost` of
//! divergences #58–#60 in the C.
//!
//! [`Step::Post`] never posts a bare `BeginHost`: `s_idle_run` would
//! dereference its NULL buffer. Bodies are at least `frame_type` plus an
//! address, which is all `bm_dfu_process_message` reads (divergence #55).
//! [`Step::SetPending`] is limited to `Init`, `Idle` and `Error`: entering
//! `HostReqUpdate` on a message event would read the message as a start.
//!
//! # Process state
//!
//! `bcmp_init` runs `bm_dfu_init` when [`crate::stack`] brings the stack up,
//! once per process, and nothing undoes it, so every script
//! starts with [`reset`], which drives both machines through the same public
//! calls to the same state: `Idle`, error zero, `internal` false, no finish
//! callback and client id zero, queue empty. The last two are only reachable
//! by running a `BeginHost` with them, so the reset does.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`].

use std::sync::{Mutex, MutexGuard};

use arbitrary::Arbitrary;
use bm_wire::bcmp::dfu::{DfuAddress, DfuMessage, DfuStart, ImgInfo};
use bm_wire::bcmp::dfu_core::{
    Accepted, Core, Dfu, DfuErr, EVENT_QUEUE_LEN, Effects, Event, EventData, EventType, HostStart,
    MAX_EVENT_BODY_LEN, RebootInfo, Roles, State,
};
use bm_wire::bcmp::{MessageType, PacketCfg};
use bm_wire::util::BmIpAddr;
use bm_wire_sys as sys;

use crate::stack::{self, NODE_ID};

/// A peer's node id.
pub const PEER: u64 = 0xbeef_beef_daad_baad;
/// A second peer, sharing [`PEER`]'s low 32 bits.
pub const PEER_ALIAS: u64 = 0x0000_0001_daad_baad;

/// A node id a step can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub enum NodeRef {
    /// [`NODE_ID`].
    This,
    /// [`PEER`].
    Peer,
    /// [`PEER_ALIAS`].
    PeerAlias,
    /// Zero.
    Zero,
    /// Anything.
    Raw(u64),
}

impl NodeRef {
    /// The id.
    #[must_use]
    pub fn id(self) -> u64 {
        match self {
            Self::This => NODE_ID,
            Self::Peer => PEER,
            Self::PeerAlias => PEER_ALIAS,
            Self::Zero => 0,
            Self::Raw(id) => id,
        }
    }
}

/// The first byte of a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub enum FrameType {
    /// `0xD0 + n % 10`.
    Dfu(u8),
    /// Any byte.
    Raw(u8),
}

impl FrameType {
    fn byte(self) -> u8 {
        match self {
            Self::Dfu(n) => 0xD0 + n % 10,
            Self::Raw(b) => b,
        }
    }
}

/// A state [`Step::SetPending`] may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub enum CoreState {
    /// [`State::Init`].
    Init,
    /// [`State::Idle`].
    Idle,
    /// [`State::Error`].
    Error,
}

impl CoreState {
    fn state(self) -> State {
        match self {
            Self::Init => State::Init,
            Self::Idle => State::Idle,
            Self::Error => State::Error,
        }
    }
}

/// One of the four senders `dfu_core.c` exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub enum Sender {
    /// `bm_dfu_send_ack`.
    Ack {
        /// Destination.
        dst: NodeRef,
        /// `success`.
        success: u8,
        /// `err_code`.
        err: u8,
    },
    /// `bm_dfu_req_next_chunk`.
    ChunkRequest {
        /// Destination.
        dst: NodeRef,
        /// `chunk_num`.
        chunk: u16,
    },
    /// `bm_dfu_update_end`.
    End {
        /// Destination.
        dst: NodeRef,
        /// `success`.
        success: u8,
        /// `err_code`.
        err: u8,
    },
    /// `bm_dfu_send_heartbeat`.
    Heartbeat {
        /// Destination.
        dst: NodeRef,
    },
}

/// The image a [`Step::Initiate`] offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub struct Image {
    /// `image_size`.
    pub image_size: u32,
    /// `chunk_size`; over 1024 is refused.
    pub chunk_size: u16,
    /// `crc16`.
    pub crc16: u16,
    /// `major_ver`.
    pub major_ver: u8,
    /// `minor_ver`.
    pub minor_ver: u8,
    /// `filter_key`.
    pub filter_key: u32,
    /// `gitSHA`.
    pub git_sha: u32,
}

impl Image {
    fn info(self) -> ImgInfo {
        ImgInfo {
            image_size: self.image_size,
            chunk_size: self.chunk_size,
            crc16: self.crc16,
            major_ver: self.major_ver,
            minor_ver: self.minor_ver,
            filter_key: self.filter_key,
            git_sha: self.git_sha,
        }
    }

    fn c(self) -> sys::BmDfuImgInfo {
        sys::BmDfuImgInfo {
            image_size: self.image_size,
            chunk_size: self.chunk_size,
            crc16: self.crc16,
            major_ver: self.major_ver,
            minor_ver: self.minor_ver,
            filter_key: self.filter_key,
            gitSHA: self.git_sha,
        }
    }
}

/// One thing done to both machines.
#[derive(Debug, Clone, PartialEq, Eq, Arbitrary)]
pub enum Step {
    /// `bm_dfu_process_message` with `frame_type`, the address, then `tail`.
    Message {
        /// First byte.
        frame_type: FrameType,
        /// `src_node_id`.
        src: NodeRef,
        /// `dst_node_id`.
        dst: NodeRef,
        /// The rest, cut to fit [`MAX_EVENT_BODY_LEN`].
        tail: Vec<u8>,
    },
    /// `bm_dfu_initiate_update`.
    Initiate {
        /// The image.
        image: Image,
        /// Destination.
        dst: NodeRef,
        /// Pass a finish callback.
        notify: bool,
        /// `timeoutMs`.
        timeout_ms: u32,
        /// `internal`.
        internal: bool,
    },
    /// A timer's `bm_queue_send` of a bare event; `n % 15`, never
    /// `BeginHost`.
    Post(u8),
    /// `bm_dfu_set_error`.
    SetError(u8),
    /// `bm_dfu_set_pending_state_change`.
    SetPending(CoreState),
    /// One iteration of `bm_dfu_event_thread`.
    Run,
    /// Run until the queue is empty.
    RunAll,
    /// One of the senders.
    Send(Sender),
}

impl Step {
    fn body(frame_type: FrameType, src: NodeRef, dst: NodeRef, tail: &[u8]) -> Vec<u8> {
        let mut body = vec![frame_type.byte()];
        body.extend_from_slice(&src.id().to_le_bytes());
        body.extend_from_slice(&dst.id().to_le_bytes());
        let room = MAX_EVENT_BODY_LEN - body.len();
        body.extend_from_slice(&tail[..tail.len().min(room)]);
        body
    }
}

/// A script.
#[derive(Debug, Clone, PartialEq, Eq, Arbitrary)]
pub struct DfuCoreInput {
    /// Applied in order.
    pub steps: Vec<Step>,
}

const EVENT_TYPES: [EventType; 15] = [
    EventType::None,
    EventType::InitSuccess,
    EventType::ReceivedUpdateRequest,
    EventType::ChunkRequest,
    EventType::ImageChunk,
    EventType::UpdateEnd,
    EventType::AckReceived,
    EventType::AckTimeout,
    EventType::ChunkTimeout,
    EventType::Heartbeat,
    EventType::Abort,
    EventType::BeginHost,
    EventType::RebootRequest,
    EventType::Reboot,
    EventType::BootComplete,
];

const DFU_TYPES: [MessageType; 10] = [
    MessageType::DFU_START,
    MessageType::DFU_PAYLOAD_REQ,
    MessageType::DFU_PAYLOAD,
    MessageType::DFU_END,
    MessageType::DFU_ACK,
    MessageType::DFU_ABORT,
    MessageType::DFU_HEARTBEAT,
    MessageType::DFU_REBOOT_REQ,
    MessageType::DFU_REBOOT,
    MessageType::DFU_BOOT_COMPLETE,
];

/// Whether an event run in `state`, with `internal` as `bm_dfu_internal`
/// reports it, stays inside what is ported. See the module docs.
#[must_use]
pub fn in_domain(state: State, kind: EventType, internal: bool) -> bool {
    match state {
        State::Init | State::Error => true,
        State::Idle => match kind {
            EventType::ReceivedUpdateRequest => false,
            // A non-internal host leaks its stream buffer on leaving
            // HostReqUpdate (divergence #61).
            EventType::BeginHost => internal,
            _ => true,
        },
        State::HostReqUpdate => !matches!(
            kind,
            EventType::AckReceived | EventType::AckTimeout | EventType::Abort
        ),
        _ => false,
    }
}

/// The roles, as far as the domain reaches: `s_host_req_update_entry`'s send
/// and the client id it records, and `bm_dfu_host_client_node_valid`.
#[derive(Debug, Default)]
pub struct ReqUpdateOnly {
    client_node_id: u64,
}

impl Roles for ReqUpdateOnly {
    fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects, _now_ms: u32) {
        assert_eq!(state, State::HostReqUpdate, "out of domain");
        // `s_host_req_update_entry` returns early without a buffer.
        if let EventData::HostStart(start) = core.current_event().data {
            self.client_node_id = start.start.addresses.dst_node_id;
            fx.send(&DfuMessage::Start(DfuStart {
                addresses: DfuAddress {
                    src_node_id: core.self_node_id(),
                    dst_node_id: self.client_node_id,
                },
                img_info: start.start.img_info,
            }));
        }
    }

    fn run(&mut self, state: State, core: &mut Core, _fx: &mut dyn Effects, _now_ms: u32) {
        assert!(
            in_domain(state, core.current_event().kind, core.internal()),
            "out of domain"
        );
    }

    fn exit(&mut self, state: State, _core: &mut Core, _fx: &mut dyn Effects, _now_ms: u32) {
        assert_eq!(state, State::HostReqUpdate, "out of domain");
    }

    fn client_process_update_request(
        &mut self,
        _core: &mut Core,
        _fx: &mut dyn Effects,
        _now_ms: u32,
    ) {
        unreachable!("out of domain");
    }

    fn host_set_params(&mut self, _notify: bool, _timeout_ms: u32) {}

    fn client_host_node_valid(&self, _node_id: u64) -> bool {
        unreachable!("no client state is in domain");
    }

    fn host_client_node_valid(&self, node_id: u64) -> bool {
        self.client_node_id == node_id
    }
}

/// What the Rust machine did outside itself.
#[derive(Debug, Default)]
pub struct Recorded {
    /// Bodies sent, with their type.
    pub sent: Vec<(MessageType, Vec<u8>)>,
    /// `update_finished` calls.
    pub finished: Vec<(bool, u8, u64)>,
}

impl Effects for Recorded {
    fn send(&mut self, message: &DfuMessage<'_>) {
        let mut buf = vec![0u8; message.encoded_len()];
        message.encode(&mut buf).expect("sized");
        self.sent.push((message.message_type(), buf));
    }
    fn lpm_peripheral_active(&mut self) {}
    fn lpm_peripheral_inactive(&mut self) {}
    fn update_finished(&mut self, success: bool, err: DfuErr, node_id: u64) {
        self.finished.push((success, err.0, node_id));
    }
}

static C_FINISHED: Mutex<Vec<(bool, u8, u64)>> = Mutex::new(Vec::new());

unsafe extern "C" fn record_finish(success: bool, error: sys::BmDfuErr, node_id: u64) {
    let error = u8::try_from(error).expect("every BmDfuErr the C passes came from a byte");
    C_FINISHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((success, error, node_id));
}

fn take_c_finished() -> Vec<(bool, u8, u64)> {
    std::mem::take(&mut *C_FINISHED.lock().unwrap_or_else(|p| p.into_inner()))
}

/// [`stack::oracle`]. `bcmp_init` runs `bm_dfu_init`, so DFU is already up,
/// and the stack's first pump has run its `InitSuccess`.
pub fn oracle() -> MutexGuard<'static, ()> {
    stack::oracle()
}

/// The C machine, through its public and test API.
pub struct Oracle(());

impl Oracle {
    fn queue() -> sys::BmQueue {
        unsafe { sys::bm_dfu_get_event_queue() }
    }

    /// `get_current_state_enum`.
    #[must_use]
    pub fn state(&self) -> State {
        let raw = unsafe { sys::get_current_state_enum(sys::bm_dfu_test_get_sm_ctx()) };
        State::from_u8(raw).expect("a DFU state")
    }

    fn take(&self) -> Option<sys::BmDfuEvent> {
        let mut evt = sys::BmDfuEvent::default();
        let err = unsafe { sys::bm_queue_receive(Self::queue(), (&raw mut evt).cast(), 0) };
        (err == sys::BmErr_BmOK).then_some(evt)
    }

    fn put(&self, evt: &sys::BmDfuEvent) -> bool {
        unsafe { sys::bm_queue_send(Self::queue(), (&raw const *evt).cast(), 0) == sys::BmErr_BmOK }
    }

    fn free(evt: sys::BmDfuEvent) {
        if !evt.buf.is_null() {
            unsafe { sys::bm_free(evt.buf.cast()) };
        }
    }

    /// Everything queued, oldest first, left in place.
    #[must_use]
    pub fn queued(&self) -> Vec<Event> {
        let raw = self.hide();
        let events = raw.iter().map(to_event).collect();
        self.restore(raw);
        events
    }

    fn hide(&self) -> Vec<sys::BmDfuEvent> {
        std::iter::from_fn(|| self.take()).collect()
    }

    fn restore(&self, raw: Vec<sys::BmDfuEvent>) {
        for evt in raw {
            assert!(self.put(&evt), "the queue had room for these a moment ago");
        }
    }

    /// Pump the stack with the DFU queue emptied, so its task runs nothing,
    /// and return what was transmitted.
    #[must_use]
    pub fn pump_and_drain(&self) -> Vec<(u8, Vec<u8>)> {
        let raw = self.hide();
        stack::pump_until_quiet();
        let frames = stack::drain();
        self.restore(raw);
        frames
    }

    /// One iteration of `bm_dfu_event_thread`, or of discarding the event if
    /// `run` is false.
    pub fn run(&self, run: bool) {
        if let Some(evt) = self.take() {
            if run {
                unsafe { sys::bm_dfu_test_set_dfu_event_and_run_sm(evt) };
            }
            Self::free(evt);
        }
    }
}

/// A C queue entry as the Rust [`Event`] it should equal.
fn to_event(evt: &sys::BmDfuEvent) -> Event {
    let kind = *EVENT_TYPES
        .get(usize::from(evt.type_))
        .expect("a DFU event type");
    if evt.buf.is_null() {
        assert_eq!(evt.len, 0, "no buffer, no length");
        return Event::bare(kind);
    }
    let bytes = unsafe { std::slice::from_raw_parts(evt.buf, evt.len) };
    if kind == EventType::BeginHost {
        let start = unsafe { std::ptr::read_unaligned(evt.buf.cast::<sys::DfuHostStartEvent>()) };
        assert_eq!(evt.len, size_of::<sys::DfuHostStartEvent>());
        let Ok(DfuMessage::Start(decoded)) = DfuMessage::decode(&bytes[..DfuMessage::START_LEN])
        else {
            panic!("a BeginHost carries a start: {bytes:02x?}");
        };
        let finish_cb = start.finish_cb;
        let timeout_ms = start.timeoutMs;
        return Event {
            kind,
            data: EventData::HostStart(HostStart {
                start: decoded,
                notify: finish_cb.is_some(),
                timeout_ms,
            }),
        };
    }
    Event::message(kind, bytes).expect("fits")
}

/// What `bm_dfu_process_message` should decide, from the comparator's own
/// view of the C.
#[derive(Debug, Default)]
pub struct Model {
    /// `host_ctx.client_node_id`: set on every entry into `HostReqUpdate`
    /// with a start.
    pub host_client_node_id: u64,
}

impl Model {
    fn verdict(&self, state: State, queued: usize, body: &[u8]) -> Accepted {
        let address = DfuAddress::of_body(body).expect("in domain");
        if address.dst_node_id != NODE_ID {
            return Accepted::NotForUs;
        }
        if state.is_host() && address.src_node_id != self.host_client_node_id {
            return Accepted::WrongPeer;
        }
        let Some(kind) = EventType::for_frame_type(body[0]) else {
            return Accepted::UnknownType;
        };
        if queued == EVENT_QUEUE_LEN {
            return Accepted::QueueFull;
        }
        Accepted::Queued(kind)
    }
}

/// The frames a `bm-stack` node puts on the wire for `sent`, in order.
fn rust_frames(sent: &[(MessageType, Vec<u8>)]) -> Vec<(u8, Vec<u8>)> {
    let mut node = stack::node();
    for t in DFU_TYPES {
        // Node::new registers 22 types; MESSAGE_TYPES is 32.
        node.register(t, PacketCfg::UNSEQUENCED).expect("room");
    }
    let mut frames = Vec::new();
    for (message_type, body) in sent {
        let outbound = node
            .send(0, &BmIpAddr::GLOBAL_MULTICAST, *message_type, body, 0)
            .expect("registered, and fits");
        frames.extend(stack::capture(outbound));
    }
    frames
}

/// Both machines, for the length of one script.
pub struct Pair {
    /// Held for the whole script.
    _guard: MutexGuard<'static, ()>,
    /// The C.
    pub c: Oracle,
    /// The port.
    pub rust: Dfu<ReqUpdateOnly>,
    /// What the port did outside itself since the last [`Pair::compare`].
    pub fx: Recorded,
    /// The comparator's model of the C's `host_ctx`.
    pub model: Model,
}

/// Bring the oracle up and drive both machines to the same state. See the
/// module docs.
#[must_use]
pub fn reset() -> Pair {
    let guard = oracle();
    let c = Oracle(());
    let mut rust = Dfu::new(NODE_ID, RebootInfo::default(), ReqUpdateOnly::default());
    let mut fx = Recorded::default();

    while let Some(evt) = c.take() {
        Oracle::free(evt);
    }
    while rust.pop().is_some() {}

    unsafe {
        sys::bm_dfu_set_error(0);
        sys::bm_dfu_set_pending_state_change(State::Idle as u8);
    }
    c.run(true);
    rust.core_mut().set_pending_state_change(State::Idle);
    rust.step(&mut fx, 0);

    // A BeginHost with no callback and client zero, then back to Idle. It is
    // `internal`, so the C host allocates no stream buffer (divergence #61).
    let image = sys::BmDfuImgInfo::default();
    assert!(unsafe { sys::bm_dfu_initiate_update(image, 0, None, 0, true) });
    assert!(rust.initiate_update(&mut fx, ImgInfo::default(), 0, false, 0, true));
    c.run(true);
    rust.step(&mut fx, 0);
    unsafe { sys::bm_dfu_set_pending_state_change(State::Idle as u8) };
    rust.core_mut().set_pending_state_change(State::Idle);
    while let Some(evt) = c.take() {
        unsafe { sys::bm_dfu_test_set_dfu_event_and_run_sm(evt) };
        Oracle::free(evt);
    }
    while rust.step(&mut fx, 0).is_some() {}

    // Then a second, discarded unrun, to leave `internal` false.
    assert!(unsafe { sys::bm_dfu_initiate_update(image, 0, None, 0, false) });
    assert!(rust.initiate_update(&mut fx, ImgInfo::default(), 0, false, 0, false));
    c.run(false);
    rust.pop();

    let _ = c.pump_and_drain();
    take_c_finished();
    let mut pair = Pair {
        _guard: guard,
        c,
        rust,
        fx: Recorded::default(),
        model: Model::default(),
    };
    pair.compare(true);
    assert_eq!(pair.rust.state(), State::Idle);
    pair
}

impl Pair {
    /// Assert both machines agree; `pump` also compares the frames sent
    /// since the last call.
    ///
    /// # Panics
    ///
    /// On any disagreement.
    pub fn compare(&mut self, pump: bool) {
        assert_eq!(self.c.state(), self.rust.state(), "state");
        let core = self.rust.core();
        assert_eq!(
            unsafe { sys::bm_dfu_get_error() },
            u32::from(core.error().0),
            "error"
        );
        assert_eq!(
            unsafe { sys::bm_dfu_internal() },
            core.internal(),
            "internal"
        );
        let info = unsafe { std::ptr::read_unaligned(&raw const sys::client_update_reboot_info) };
        let (magic, host_node_id, git_sha) = (info.magic, info.host_node_id, info.gitSHA);
        assert_eq!(
            RebootInfo {
                magic,
                major: info.major,
                minor: info.minor,
                host_node_id,
                git_sha,
            },
            *core.reboot_info(),
            "reboot info"
        );
        let rust_queue: Vec<Event> = core.queue().iter().copied().collect();
        assert_eq!(self.c.queued(), rust_queue, "queue");
        assert_eq!(
            take_c_finished(),
            std::mem::take(&mut self.fx.finished),
            "finish callbacks"
        );
        if pump {
            let sent = std::mem::take(&mut self.fx.sent);
            assert_eq!(self.c.pump_and_drain(), rust_frames(&sent), "frames");
        } else {
            assert!(self.fx.sent.is_empty(), "sent without a pump to compare");
        }
    }

    fn run_one(&mut self) {
        let Some(head) = self.rust.core().queue().iter().next().map(|e| e.kind) else {
            self.c.run(true);
            return;
        };
        let state = self.rust.state();
        let run = in_domain(state, head, self.rust.core().internal());
        self.c.run(run);
        if run {
            if state == State::Idle
                && head == EventType::BeginHost
                && let Some(EventData::HostStart(start)) =
                    self.rust.core().queue().iter().next().map(|e| e.data)
            {
                self.model.host_client_node_id = start.start.addresses.dst_node_id;
            }
            self.rust.step(&mut self.fx, 0);
        } else {
            self.rust.pop();
        }
    }

    /// Apply one step to both and compare.
    ///
    /// # Panics
    ///
    /// On any disagreement.
    pub fn apply(&mut self, step: &Step) {
        let mut pump = false;
        match step {
            Step::Message {
                frame_type,
                src,
                dst,
                tail,
            } => {
                let body = Step::body(*frame_type, *src, *dst, tail);
                let before = self.rust.core().queue().len();
                let expected = self.model.verdict(self.rust.state(), before, &body);
                let verdict = self.rust.on_message(&body);
                assert_eq!(verdict, expected, "the model of bm_dfu_process_message");

                let c_before = self.c.queued().len();
                let buf = unsafe { sys::bm_malloc(body.len()) }.cast::<u8>();
                assert!(!buf.is_null());
                unsafe {
                    std::ptr::copy_nonoverlapping(body.as_ptr(), buf, body.len());
                    sys::bm_dfu_process_message(buf, body.len());
                }
                let grew = self.c.queued().len() > c_before;
                assert_eq!(grew, matches!(verdict, Accepted::Queued(_)), "queued");
                if !grew && verdict == Accepted::UnknownType {
                    // Divergence #54: the C drops it without freeing.
                    unsafe { sys::bm_free(buf.cast()) };
                }
            }
            Step::Initiate {
                image,
                dst,
                notify,
                timeout_ms,
                internal,
            } => {
                let cb: sys::UpdateFinishCb = notify.then_some(record_finish as _);
                let c = unsafe {
                    sys::bm_dfu_initiate_update(image.c(), dst.id(), cb, *timeout_ms, *internal)
                };
                let rust = self.rust.initiate_update(
                    &mut self.fx,
                    image.info(),
                    dst.id(),
                    *notify,
                    *timeout_ms,
                    *internal,
                );
                assert_eq!(c, rust, "bm_dfu_initiate_update's result");
            }
            Step::Post(n) => {
                let kind = EVENT_TYPES[usize::from(*n) % EVENT_TYPES.len()];
                if kind != EventType::BeginHost {
                    let evt = sys::BmDfuEvent {
                        type_: kind as u8,
                        buf: std::ptr::null_mut(),
                        len: 0,
                    };
                    assert_eq!(
                        self.c.put(&evt),
                        self.rust.core_mut().post(Event::bare(kind)),
                        "post"
                    );
                }
            }
            Step::SetError(e) => {
                unsafe { sys::bm_dfu_set_error(sys::BmDfuErr::from(*e)) };
                self.rust.core_mut().set_error(DfuErr(*e));
            }
            Step::SetPending(state) => {
                let state = state.state();
                unsafe { sys::bm_dfu_set_pending_state_change(state as u8) };
                self.rust.core_mut().set_pending_state_change(state);
            }
            Step::Run => {
                self.run_one();
                pump = true;
            }
            Step::RunAll => {
                for _ in 0..4 * EVENT_QUEUE_LEN {
                    if self.rust.core().queue().is_empty() {
                        break;
                    }
                    self.run_one();
                }
                pump = true;
            }
            Step::Send(sender) => {
                let core = self.rust.core();
                let fx = &mut self.fx;
                unsafe {
                    match *sender {
                        Sender::Ack { dst, success, err } => {
                            sys::bm_dfu_send_ack(dst.id(), success, err.into());
                            core.send_ack(fx, dst.id(), success, DfuErr(err));
                        }
                        Sender::ChunkRequest { dst, chunk } => {
                            sys::bm_dfu_req_next_chunk(dst.id(), chunk);
                            core.req_next_chunk(fx, dst.id(), chunk);
                        }
                        Sender::End { dst, success, err } => {
                            sys::bm_dfu_update_end(dst.id(), success, err.into());
                            core.update_end(fx, dst.id(), success, DfuErr(err));
                        }
                        Sender::Heartbeat { dst } => {
                            sys::bm_dfu_send_heartbeat(dst.id());
                            core.send_heartbeat(fx, dst.id());
                        }
                    }
                }
                pump = true;
            }
        }
        self.compare(pump);
    }
}

/// Reset both machines, apply every step, comparing after each.
///
/// # Panics
///
/// On any disagreement.
pub fn check(input: &DfuCoreInput) {
    let mut pair = reset();
    for step in &input.steps {
        pair.apply(step);
    }
}
