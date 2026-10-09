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
//! # Input domain
//!
//! The client (`dfu_client.c`) and the host (`dfu_host.c`) are
//! [`ClientHost`]. Two events are **discarded on both sides** at the moment
//! they would run, rather than run, because the C's outcome depends on heap
//! state the port does not have:
//!
//! * a `BeginHost` in `Idle` while `bm_dfu_internal()` is false and the last
//!   non-internal update's stream buffer is still held: the new one would
//!   overwrite and leak it (divergence #61), and LeakSanitizer stops the
//!   fuzzer;
//! * a `ChunkRequest` in `HostUpdate` while `bm_dfu_internal()` is false and
//!   the stream holds some but not all of the next chunk: the C sends the
//!   rest of the chunk from uninitialised heap (divergence #68).
//!
//! [`Step::Post`] never posts a bare `BeginHost`: `s_idle_run` would
//! dereference its NULL buffer. [`Step::SetPending`] is limited to `Init`,
//! `Idle` and `Error`: entering a client or host state on an arbitrary event
//! would read it as a start.
//!
//! Every body sent is first made one the C reads only within
//! ([`in_domain_body`]): at least `frame_type` plus an address, a `0xD0`
//! padded to a whole start, a `0xD2` padded to its declared chunk when that is
//! 1024 bytes or fewer, a `0xD3`, `0xD4` or `0xD5` padded to its `success` and
//! `err_code` (divergence #55). A `0xD0` with `chunk_size` zero has it set to
//! 1, since the C divides by it (divergence #57).
//!
//! A host update's `timeoutMs` of zero is sent as 1: it becomes the update
//! timer's period, which FreeRTOS refuses to be zero and the shim fires at
//! its next tick rather than the port's next poll (divergence #69).
//!
//! # Seams with no oracle
//!
//! The update slot, the boot hooks, `bm_dfu_host_get_chunk` and
//! `bm_config_reset` are integrator seams (`bm_dfu_generic.h`,
//! `bm_configs_generic.h`). `csrc/bm_generic_shim.c` implements them in RAM,
//! counts every call but the host's read, and refuses the operations
//! [`Step::Faults`] names. [`Recorded`] is the same semantics on the Rust side.
//! What is compared is the calls made and the slot's bytes, not the seam. The
//! host reads the same slot the client writes; [`Step::Host`] stores its image
//! there first, through `bm_shim_dfu_load`, which counts nothing.
//!
//! A non-internal update's stream buffer is `csrc/bm_os_shim.c`'s, and
//! [`bm_wire::bcmp::dfu_host::StreamBuffer`] has its semantics.
//!
//! # Time
//!
//! [`Step::Advance`] moves the C's virtual clock, stopping at each deadline
//! the port reports so that two timers due in one step fire in deadline
//! order, as they would under an RTOS; the shim alone would fire them in
//! creation order. Every run is given the C's tick as `now_ms`, read before
//! the C runs, since the C's `bm_delay` moves it.
//!
//! # Process state
//!
//! `bcmp_init` runs `bm_dfu_init` when [`crate::stack`] brings the stack up,
//! once per process, and nothing undoes it, so every script
//! starts with [`reset`], which drives both machines through the same public
//! calls to the same state: `Idle`, error zero, `internal` false, no finish
//! callback and client id zero, queue empty, no timer running, no stream
//! buffer. The finish callback and client id are only reachable by running a
//! `BeginHost` with them, so the reset does. That update then runs to its end
//! — an ACK and an END from client zero — which stops the host's ACK and
//! update timers and deletes any stream buffer a script left behind
//! (`s_host_update_exit`). The client's chunk timer is fired by advancing the
//! C's clock past its period and discarding what it posts.
//!
//! `CLIENT_CTX` cannot be reset and is not read: every way into a client
//! state sets the host id, and every other field is set on entry before it
//! is read. The slot's bytes and `dfu_confirm` are carried over into the
//! Rust side instead, and the shim's call counts are taken relative to the
//! reset.
//!
//! This module brings the stack up and so must not share a process with
//! [`crate::bcmp`] — see [`crate::stack`].

use std::sync::{Mutex, MutexGuard};

use arbitrary::Arbitrary;
use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::dfu::{
    DFU_MAX_CHUNK_SIZE, DfuAddress, DfuMessage, DfuResult, DfuStart, ImgInfo,
};
use bm_wire::bcmp::dfu_client::DFU_CONFIRM_KEY;
use bm_wire::bcmp::dfu_core::{
    Accepted, DFU_REBOOT_MAGIC, Dfu, DfuErr, EVENT_QUEUE_LEN, Effects, Event, EventData, EventType,
    HostStart, MAX_EVENT_BODY_LEN, RebootInfo, State, Timer,
};
use bm_wire::bcmp::dfu_host::ClientHost;
use bm_wire::configuration::{ConfigStore, Key, Layout, Partition};
use bm_wire::crc::crc16_ccitt;
use bm_wire::util::BmIpAddr;
use bm_wire_sys as sys;

use crate::stack::{self, GIT_SHA, NODE_ID};

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

/// An image a [`Step::Offer`] offers and [`Step::Serve`] then sends, with
/// bytes derived from `seed` so a script can carry a whole transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Arbitrary)]
pub struct TestImage {
    /// `image_size`, plus 256 KiB if `huge` — the shim's slot is 256 KiB, so
    /// that is refused as too large.
    pub len: u16,
    /// See `len`.
    pub huge: bool,
    /// `chunk_size`; zero is sent as 1 (divergence #57), over 1024 is refused
    /// by the client.
    pub chunk_size: u16,
    /// Seeds the bytes.
    pub seed: u8,
    /// Whether `crc16` is the bytes' CRC, or one off it.
    pub crc_ok: bool,
    /// Whether `gitSHA` is the node's own, which the client refuses unless
    /// `force`.
    pub own_sha: bool,
    /// `filter_key` is `BM_DFU_IMG_INFO_FORCE_UPDATE`.
    pub force: bool,
    /// `major_ver`.
    pub major: u8,
    /// `minor_ver`.
    pub minor: u8,
}

impl TestImage {
    /// `image_size`.
    #[must_use]
    pub fn image_size(self) -> u32 {
        u32::from(self.len) + if self.huge { 256 * 1024 } else { 0 }
    }

    fn chunk_size(self) -> u16 {
        self.chunk_size.max(1)
    }

    /// Byte `i` of the image.
    fn byte(self, i: u32) -> u8 {
        (i.wrapping_mul(0x9E37_79B1) >> 24) as u8 ^ self.seed
    }

    /// The image's bytes in `range`.
    #[must_use]
    pub fn bytes(self, range: std::ops::Range<u32>) -> Vec<u8> {
        range.map(|i| self.byte(i)).collect()
    }

    /// The `BmDfuImgInfo` that describes it.
    #[must_use]
    pub fn info(self) -> ImgInfo {
        let crc = crc16_ccitt(0, &self.bytes(0..self.image_size()));
        ImgInfo {
            image_size: self.image_size(),
            chunk_size: self.chunk_size(),
            crc16: if self.crc_ok { crc } else { crc ^ 1 },
            major_ver: self.major,
            minor_ver: self.minor,
            filter_key: if self.force {
                bm_wire::bcmp::dfu::IMG_INFO_FORCE_UPDATE
            } else {
                0
            },
            git_sha: if self.own_sha { GIT_SHA } else { !GIT_SHA },
        }
    }
}

/// Make `body` one the C reads only within, for what the port's states read.
/// See the module docs.
#[must_use]
pub fn in_domain_body(mut body: Vec<u8>) -> Vec<u8> {
    let min = |body: &mut Vec<u8>, len: usize| {
        if body.len() < len {
            body.resize(len, 0);
        }
    };
    min(&mut body, DfuMessage::MIN_LEN);
    match body[0] {
        0xD0 => {
            min(&mut body, DfuMessage::START_LEN);
            // `chunk_size`, after the address and `image_size`.
            let at = DfuMessage::MIN_LEN + 4;
            if body[at..at + 2] == [0, 0] {
                body[at] = 1;
            }
        }
        0xD3..=0xD5 => min(&mut body, DfuMessage::WITH_TWO_BYTES_LEN),
        0xD2 => {
            min(&mut body, DfuMessage::WITH_TWO_BYTES_LEN);
            let at = DfuMessage::MIN_LEN;
            let declared = usize::from(u16::from_le_bytes([body[at], body[at + 1]]));
            if declared <= DFU_MAX_CHUNK_SIZE {
                min(&mut body, DfuMessage::WITH_TWO_BYTES_LEN + declared);
            }
        }
        _ => {}
    }
    body
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
        /// `timeoutMs`; zero is sent as 1.
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
    /// A well-formed `0xD0` for `image`, from `src` to this node.
    Offer {
        /// Sender.
        src: NodeRef,
        /// What is offered.
        image: TestImage,
    },
    /// The chunk of the last offered image that the client last asked for,
    /// from `src`, `short_by` bytes short, and cut to what one frame carries.
    /// Nothing if nothing was offered.
    Serve {
        /// Sender.
        src: NodeRef,
        /// Bytes cut from the end.
        short_by: u8,
    },
    /// Advance the clock by this many milliseconds.
    Advance(u16),
    /// Write `client_update_reboot_info`, as a reboot would leave it.
    SetRebootInfo {
        /// `magic` is `DFU_REBOOT_MAGIC`.
        magic: bool,
        /// `major`.
        major: u8,
        /// `minor`.
        minor: u8,
        /// `host_node_id`.
        host: NodeRef,
        /// `gitSHA` is the node's own.
        own_sha: bool,
    },
    /// Refuse these slot operations from now on.
    Faults {
        /// `bm_dfu_client_flash_area_open`.
        open: bool,
        /// `bm_dfu_client_flash_area_erase`.
        erase: bool,
        /// `bm_dfu_client_flash_area_write`.
        write: bool,
    },
    /// `set_config_uint` of `dfu_confirm`.
    SetConfirm(u32),
    /// `bm_dfu_initiate_update` for `image`, stored in the slot first if
    /// `internal`.
    Host {
        /// Destination.
        dst: NodeRef,
        /// What is sent.
        image: TestImage,
        /// Pass a finish callback.
        notify: bool,
        /// `timeoutMs`; zero is sent as 1.
        timeout_ms: u32,
        /// `internal`.
        internal: bool,
    },
    /// `bm_dfu_host_queue_data` with the next `len % 1025` bytes of the last
    /// [`Step::Host`]'s image, or zeros if there was none.
    Feed(u16),
    /// A body of type `0xD0 + n % 10` from the host's current client to this
    /// node, with `success` and `err_code` after the address.
    FromClient {
        /// Type.
        n: u8,
        /// Byte 17.
        success: u8,
        /// Byte 18.
        err_code: u8,
    },
}

impl Step {
    fn body(frame_type: FrameType, src: NodeRef, dst: NodeRef, tail: &[u8]) -> Vec<u8> {
        let mut body = vec![frame_type.byte()];
        body.extend_from_slice(&src.id().to_le_bytes());
        body.extend_from_slice(&dst.id().to_le_bytes());
        let room = MAX_EVENT_BODY_LEN - body.len();
        body.extend_from_slice(&tail[..tail.len().min(room)]);
        in_domain_body(body)
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

/// Whether the event at the head of `dfu`'s queue, run now, stays inside
/// what the comparator can compare. See the module docs.
#[must_use]
pub fn in_domain(dfu: &Dfu<ClientHost>, kind: EventType) -> bool {
    let host = &dfu.roles().host;
    let internal = dfu.core().internal();
    match (dfu.state(), kind) {
        // Divergence #61: the new stream buffer would leak the old.
        (State::Idle, EventType::BeginHost) => internal || host.stream().is_none(),
        // Divergence #68: a short read sends uninitialised heap.
        (State::HostUpdate, EventType::ChunkRequest) if !internal => {
            let have = host.stream().map_or(0, |s| s.len());
            have == 0 || have >= host.next_chunk_len() as usize
        }
        _ => true,
    }
}

/// Calls to the slot and boot hooks, as `BmShimDfuCounts` counts them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// `bm_dfu_client_flash_area_open`.
    pub opens: u32,
    /// `_close`.
    pub closes: u32,
    /// `_erase`.
    pub erases: u32,
    /// `_write`.
    pub writes: u32,
    /// `bm_dfu_client_set_confirmed`.
    pub confirmed: u32,
    /// `bm_dfu_client_set_pending_and_reset`.
    pub pending_and_reset: u32,
    /// `bm_dfu_client_fail_update_and_reset`.
    pub fail_and_reset: u32,
    /// `bm_config_reset`.
    pub config_resets: u32,
}

impl Counts {
    fn from_c(c: sys::BmShimDfuCounts) -> Self {
        Self {
            opens: c.opens,
            closes: c.closes,
            erases: c.erases,
            writes: c.writes,
            confirmed: c.confirmed,
            pending_and_reset: c.pending_and_reset,
            fail_and_reset: c.fail_and_reset,
            config_resets: c.config_resets,
        }
    }

    fn since(self, base: Self) -> Self {
        Self {
            opens: self.opens - base.opens,
            closes: self.closes - base.closes,
            erases: self.erases - base.erases,
            writes: self.writes - base.writes,
            confirmed: self.confirmed - base.confirmed,
            pending_and_reset: self.pending_and_reset - base.pending_and_reset,
            fail_and_reset: self.fail_and_reset - base.fail_and_reset,
            config_resets: self.config_resets - base.config_resets,
        }
    }
}

/// What the Rust machine did outside itself, and the seams it did it
/// through: `csrc/bm_generic_shim.c`'s semantics, in Rust.
pub struct Recorded {
    /// Bodies sent, with their type.
    pub sent: Vec<(MessageType, Vec<u8>)>,
    /// `update_finished` calls.
    pub finished: Vec<(bool, u8, u64)>,
    /// The slot.
    pub flash: Vec<u8>,
    /// Operations refused.
    pub faults: sys::BmShimDfuFaults,
    /// Calls made.
    pub counts: Counts,
    /// `CONFIGS`.
    pub config: ConfigStore,
}

impl Default for Recorded {
    fn default() -> Self {
        Self {
            sent: Vec::new(),
            finished: Vec::new(),
            flash: Vec::new(),
            faults: sys::BmShimDfuFaults::default(),
            counts: Counts::default(),
            config: ConfigStore::new(Layout::LP64),
        }
    }
}

impl std::fmt::Debug for Recorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorded")
            .field("sent", &self.sent)
            .field("finished", &self.finished)
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

impl Recorded {
    fn range(&self, offset: u32, len: usize) -> Option<std::ops::Range<usize>> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len)?;
        (end <= self.flash.len()).then_some(start..end)
    }
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
    fn flash_open(&mut self) -> bool {
        self.counts.opens += 1;
        !self.faults.open
    }
    fn flash_close(&mut self) -> bool {
        self.counts.closes += 1;
        true
    }
    fn flash_size(&mut self) -> u32 {
        u32::try_from(self.flash.len()).expect("256 KiB")
    }
    fn flash_erase(&mut self, offset: u32, len: u32) -> bool {
        self.counts.erases += 1;
        match self.range(offset, len as usize) {
            Some(range) if !self.faults.erase => {
                self.flash[range].fill(0xFF);
                true
            }
            _ => false,
        }
    }
    fn flash_write(&mut self, offset: u32, data: &[u8]) -> bool {
        self.counts.writes += 1;
        match self.range(offset, data.len()) {
            Some(range) if !self.faults.write => {
                self.flash[range].copy_from_slice(data);
                true
            }
            _ => false,
        }
    }
    fn host_get_chunk(&mut self, offset: u32, buf: &mut [u8]) -> bool {
        // Not counted, as the shim does not count it.
        match self.range(offset, buf.len()) {
            Some(range) => {
                buf.copy_from_slice(&self.flash[range]);
                true
            }
            None => false,
        }
    }
    fn set_confirmed(&mut self) {
        self.counts.confirmed += 1;
    }
    fn set_pending_and_reset(&mut self) {
        self.counts.pending_and_reset += 1;
    }
    fn fail_update_and_reset(&mut self) {
        self.counts.fail_and_reset += 1;
    }
    fn git_sha(&self) -> u32 {
        GIT_SHA
    }
    fn config(&mut self) -> Option<&mut ConfigStore> {
        Some(&mut self.config)
    }
    fn commit_config(&mut self, partition: Partition) -> bool {
        // `save_config`'s write always succeeds in the shim; then
        // `bm_config_reset`, which it counts.
        let part = self.config.partition_mut(partition);
        let _ = part.seal();
        part.mark_saved();
        self.counts.config_resets += 1;
        true
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

    fn discard_queue(&self) {
        while let Some(evt) = self.take() {
            Self::free(evt);
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
    /// and return the DFU frames transmitted. Everything else the stack sends
    /// — heartbeats, once time moves — is dropped.
    #[must_use]
    pub fn pump_and_drain(&self) -> Vec<(u8, Vec<u8>)> {
        let raw = self.hide();
        stack::pump_until_quiet();
        let frames = stack::drain()
            .into_iter()
            .filter(|(_, frame)| stack::captured_message_type(frame).is_some_and(is_dfu))
            .collect();
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

    /// The shim's clock.
    #[must_use]
    pub fn now(&self) -> u32 {
        stack::tick_count()
    }

    /// `bm_shim_advance_ticks`.
    pub fn advance(&self, ms: u32) {
        unsafe { sys::bm_shim_advance_ticks(ms) };
    }

    /// The slot and boot-hook calls the shim has counted.
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut c = sys::BmShimDfuCounts::default();
        unsafe { sys::bm_shim_dfu_counts(&raw mut c) };
        Counts::from_c(c)
    }

    /// The shim's slot.
    #[must_use]
    pub fn flash(&self) -> &'static [u8] {
        let mut len = 0u32;
        let ptr = unsafe { sys::bm_shim_dfu_flash(&raw mut len) };
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    }

    /// `get_config_uint(SYSTEM, "dfu_confirm")`.
    #[must_use]
    pub fn confirm(&self) -> Option<u32> {
        let mut value = 0u32;
        unsafe {
            sys::get_config_uint(
                sys::BmConfigPartition_BM_CFG_PARTITION_SYSTEM,
                C_DFU_CONFIRM_KEY.as_ptr().cast(),
                DFU_CONFIRM_KEY.len(),
                &raw mut value,
            )
        }
        .then_some(value)
    }
}

/// [`DFU_CONFIRM_KEY`] NUL-terminated for the C, whose setters
/// `snprintf("%s")` the key whatever length they are given.
const C_DFU_CONFIRM_KEY: &[u8] = b"dfu_confirm\0";

fn is_dfu(t: MessageType) -> bool {
    DFU_TYPES.contains(&t)
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
    /// `host_ctx.client_node_id`, from the port.
    pub host_client_node_id: u64,
    /// `CLIENT_CTX.host_node_id`, from the port, which the comparison of
    /// everything else vouches for.
    pub client_host_node_id: u64,
}

impl Model {
    fn verdict(&self, state: State, queued: usize, body: &[u8]) -> Accepted {
        let address = DfuAddress::of_body(body).expect("in domain");
        if address.dst_node_id != NODE_ID {
            return Accepted::NotForUs;
        }
        let peer = if state.is_host() {
            Some(self.host_client_node_id)
        } else if state.is_client() {
            Some(self.client_host_node_id)
        } else {
            None
        };
        if peer.is_some_and(|peer| address.src_node_id != peer) {
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
    let mut resources = bm_stack::NodeResources::new();
    let mut node = stack::node(&mut resources);
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
    pub rust: Dfu<ClientHost>,
    /// What the port did outside itself since the last [`Pair::compare`],
    /// and its seams.
    pub fx: Recorded,
    /// The comparator's model of the C's `host_ctx`.
    pub model: Model,
    /// The C's counts at the reset.
    base: Counts,
    /// The last [`Step::Offer`]'s image.
    image: Option<TestImage>,
    /// The chunk the client last asked for.
    requested: u16,
    /// The last [`Step::Host`]'s image.
    hosted: Option<TestImage>,
    /// Bytes of it [`Step::Feed`] has queued.
    fed: u32,
    /// The bodies the last [`Pair::compare`] with `pump` compared, both
    /// sides having sent them.
    pub last_sent: Vec<(MessageType, Vec<u8>)>,
}

/// `body` to the C's `bm_dfu_process_message`, as `dfu_copy_and_process_message`
/// hands it over: a heap copy the C frees or queues. `verdict` is the port's;
/// for [`Accepted::UnknownType`] the C leaks the copy (divergence #54), and
/// this frees it instead.
fn c_process(body: &[u8], verdict: Accepted) {
    let buf = unsafe { sys::bm_malloc(body.len()) }.cast::<u8>();
    assert!(!buf.is_null());
    unsafe {
        std::ptr::copy_nonoverlapping(body.as_ptr(), buf, body.len());
        sys::bm_dfu_process_message(buf, body.len());
    }
    if verdict == Accepted::UnknownType {
        unsafe { sys::bm_free(buf.cast()) };
    }
}

/// A well-formed body of `message`.
#[must_use]
pub fn encode(message: &DfuMessage<'_>) -> Vec<u8> {
    let mut body = vec![0u8; message.encoded_len()];
    message.encode(&mut body).expect("sized");
    body
}

/// Bring the oracle up and drive both machines to the same state. See the
/// module docs.
#[must_use]
pub fn reset() -> Pair {
    let guard = oracle();
    let c = Oracle(());
    unsafe { sys::bm_shim_dfu_set_faults(sys::BmShimDfuFaults::default()) };

    // Past the ACK period, a stale chunk or ACK timer has fired. The update
    // timer, and the ACK timer again, are stopped by the update below.
    c.advance(Timer::Ack.period_ms() + 1);
    c.discard_queue();
    let _ = c.pump_and_drain();

    let mut fx = Recorded {
        flash: c.flash().to_vec(),
        ..Recorded::default()
    };
    if let Some(value) = c.confirm() {
        let key = Key::new(DFU_CONFIRM_KEY);
        let sys_part = fx.config.partition_mut(Partition::System);
        assert!(sys_part.set_uint(key, value));
        sys_part.mark_saved();
    }
    let mut rust = Dfu::new(NODE_ID, RebootInfo::default(), ClientHost::new());
    while rust.pop().is_some() {}

    let run_all = |rust: &mut Dfu<ClientHost>, fx: &mut Recorded| {
        let now = c.now();
        while let Some(evt) = c.take() {
            unsafe { sys::bm_dfu_test_set_dfu_event_and_run_sm(evt) };
            Oracle::free(evt);
        }
        while rust.step(fx, now).is_some() {}
    };

    unsafe {
        sys::bm_dfu_set_error(0);
        sys::bm_dfu_set_pending_state_change(State::Idle as u8);
    }
    rust.core_mut().set_pending_state_change(State::Idle);
    run_all(&mut rust, &mut fx);

    // A whole update to client zero, with no callback and a 1 ms timeout.
    // It is `internal`, so the C host allocates no stream buffer (divergence
    // #61), and its exit deletes any a script left.
    let image = sys::BmDfuImgInfo::default();
    assert!(unsafe { sys::bm_dfu_initiate_update(image, 0, None, 1, true) });
    assert!(rust.initiate_update(&mut fx, ImgInfo::default(), 0, false, 1, true));
    run_all(&mut rust, &mut fx);
    assert_eq!(rust.state(), State::HostReqUpdate);
    let from_zero = DfuResult {
        addresses: DfuAddress {
            src_node_id: 0,
            dst_node_id: NODE_ID,
        },
        success: 1,
        err_code: 0,
    };
    for message in [DfuMessage::Ack(from_zero), DfuMessage::End(from_zero)] {
        let body = encode(&message);
        let verdict = rust.on_message(&body);
        assert!(matches!(verdict, Accepted::Queued(_)));
        c_process(&body, verdict);
        run_all(&mut rust, &mut fx);
    }
    assert_eq!(rust.state(), State::Idle);

    // Then a second, discarded unrun, to leave `internal` false.
    assert!(unsafe { sys::bm_dfu_initiate_update(image, 0, None, 0, false) });
    assert!(rust.initiate_update(&mut fx, ImgInfo::default(), 0, false, 0, false));
    c.run(false);
    rust.pop();

    let _ = c.pump_and_drain();
    take_c_finished();
    fx.sent.clear();
    fx.finished.clear();
    let mut pair = Pair {
        _guard: guard,
        base: c.counts(),
        c,
        rust,
        fx,
        model: Model::default(),
        image: None,
        requested: 0,
        hosted: None,
        fed: 0,
        last_sent: Vec::new(),
    };
    pair.compare(true);
    assert_eq!(pair.rust.state(), State::Idle);
    assert!(pair.rust.next_deadline().is_none(), "no timer running");
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
        assert_eq!(
            self.c.counts().since(self.base),
            self.fx.counts,
            "slot and boot-hook calls"
        );
        assert!(self.c.flash() == self.fx.flash.as_slice(), "slot contents");
        let key = Key::new(DFU_CONFIRM_KEY);
        assert_eq!(
            self.c.confirm(),
            self.fx.config.partition(Partition::System).get_uint(key),
            "dfu_confirm"
        );
        let host = &self.rust.roles().host;
        assert!(
            unsafe { sys::bm_dfu_host_client_node_valid(host.client_node_id()) },
            "host_ctx.client_node_id"
        );
        self.model.client_host_node_id = self.rust.roles().client.host_node_id();
        self.model.host_client_node_id = host.client_node_id();
        if pump {
            let sent = std::mem::take(&mut self.fx.sent);
            for (t, body) in &sent {
                if *t == MessageType::DFU_PAYLOAD_REQ
                    && let Ok(DfuMessage::PayloadReq(req)) = DfuMessage::decode(body)
                {
                    self.requested = req.seq_num;
                }
            }
            assert_eq!(self.c.pump_and_drain(), rust_frames(&sent), "frames");
            self.last_sent = sent;
        } else {
            assert!(self.fx.sent.is_empty(), "sent without a pump to compare");
        }
    }

    fn run_one(&mut self) {
        let now = self.c.now();
        let Some(head) = self.rust.core().queue().iter().next().map(|e| e.kind) else {
            self.c.run(true);
            return;
        };
        let run = in_domain(&self.rust, head);
        self.c.run(run);
        if run {
            self.rust.step(&mut self.fx, now);
        } else {
            self.rust.pop();
        }
    }

    /// Hand `body` to both, checking the port's verdict against [`Model`].
    fn deliver(&mut self, body: &[u8]) {
        let before = self.rust.core().queue().len();
        let expected = self.model.verdict(self.rust.state(), before, body);
        let verdict = self.rust.on_message(body);
        assert_eq!(verdict, expected, "the model of bm_dfu_process_message");

        let c_before = self.c.queued().len();
        c_process(body, verdict);
        let grew = self.c.queued().len() > c_before;
        assert_eq!(grew, matches!(verdict, Accepted::Queued(_)), "queued");
    }

    /// Hand a body another node sent to both, as `bm_dfu_process_message`,
    /// then compare.
    ///
    /// # Panics
    ///
    /// On any disagreement.
    pub fn receive(&mut self, body: &[u8]) {
        self.deliver(body);
        self.compare(false);
    }

    /// Move both clocks on by `ms`, stopping at each of the port's deadlines.
    fn advance(&mut self, ms: u32) {
        let target = self.c.now().wrapping_add(ms);
        loop {
            let now = self.c.now();
            let left = target.wrapping_sub(now);
            let step = self
                .rust
                .next_deadline()
                .map(|d| d.wrapping_sub(now))
                .filter(|d| (1..=left).contains(d))
                .unwrap_or(left);
            self.c.advance(step);
            self.rust.poll(self.c.now());
            if step == left {
                return;
            }
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
            } => self.deliver(&Step::body(*frame_type, *src, *dst, tail)),
            Step::Offer { src, image } => {
                self.image = Some(*image);
                let mut body = vec![0u8; DfuMessage::START_LEN];
                DfuMessage::Start(DfuStart {
                    addresses: DfuAddress {
                        src_node_id: src.id(),
                        dst_node_id: NODE_ID,
                    },
                    img_info: image.info(),
                })
                .encode(&mut body)
                .expect("sized");
                self.deliver(&body);
            }
            Step::Serve { src, short_by } => {
                if let Some(image) = self.image {
                    let size = image.image_size();
                    let chunk = u32::from(image.chunk_size());
                    let start = u32::from(self.requested).saturating_mul(chunk).min(size);
                    let end = start.saturating_add(chunk).min(size);
                    let mut payload = image.bytes(start..end);
                    payload.truncate(payload.len().saturating_sub(usize::from(*short_by)));
                    // A `chunk_size` over what one frame carries cannot be
                    // served whole.
                    payload.truncate(MAX_EVENT_BODY_LEN - DfuMessage::WITH_TWO_BYTES_LEN);
                    let message = DfuMessage::Payload(bm_wire::bcmp::dfu::DfuChunk {
                        addresses: DfuAddress {
                            src_node_id: src.id(),
                            dst_node_id: NODE_ID,
                        },
                        payload: &payload,
                    });
                    let mut body = vec![0u8; message.encoded_len()];
                    message.encode(&mut body).expect("sized");
                    self.deliver(&body);
                }
            }
            Step::Initiate {
                image,
                dst,
                notify,
                timeout_ms,
                internal,
            } => {
                // Divergence #69.
                let timeout_ms = (*timeout_ms).max(1);
                let cb: sys::UpdateFinishCb = notify.then_some(record_finish as _);
                let c = unsafe {
                    sys::bm_dfu_initiate_update(image.c(), dst.id(), cb, timeout_ms, *internal)
                };
                let rust = self.rust.initiate_update(
                    &mut self.fx,
                    image.info(),
                    dst.id(),
                    *notify,
                    timeout_ms,
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
            Step::Advance(ms) => {
                self.advance(u32::from(*ms));
                pump = true;
            }
            Step::SetRebootInfo {
                magic,
                major,
                minor,
                host,
                own_sha,
            } => {
                let info = RebootInfo {
                    magic: if *magic { DFU_REBOOT_MAGIC } else { 0 },
                    major: *major,
                    minor: *minor,
                    host_node_id: host.id(),
                    git_sha: if *own_sha { GIT_SHA } else { !GIT_SHA },
                };
                unsafe {
                    sys::client_update_reboot_info = sys::ReboootClientUpdateInfo {
                        magic: info.magic,
                        major: info.major,
                        minor: info.minor,
                        host_node_id: info.host_node_id,
                        gitSHA: info.git_sha,
                    };
                }
                *self.rust.core_mut().reboot_info_mut() = info;
            }
            Step::Faults { open, erase, write } => {
                let faults = sys::BmShimDfuFaults {
                    open: *open,
                    erase: *erase,
                    write: *write,
                };
                unsafe { sys::bm_shim_dfu_set_faults(faults) };
                self.fx.faults = faults;
            }
            Step::Host {
                dst,
                image,
                notify,
                timeout_ms,
                internal,
            } => {
                self.hosted = Some(*image);
                self.fed = 0;
                if *internal {
                    // What fits of the image, where the host reads it.
                    let at = ImgInfo::LEN as u32;
                    let room = u32::try_from(self.fx.flash.len()).expect("256 KiB") - at;
                    let bytes = image.bytes(0..image.image_size().min(room));
                    let len = u32::try_from(bytes.len()).expect("fits");
                    assert!(unsafe { sys::bm_shim_dfu_load(at, bytes.as_ptr(), len) });
                    let at = at as usize;
                    self.fx.flash[at..at + bytes.len()].copy_from_slice(&bytes);
                }
                // Divergence #69.
                let timeout_ms = (*timeout_ms).max(1);
                let cb: sys::UpdateFinishCb = notify.then_some(record_finish as _);
                let info = image.info();
                let c_info = sys::BmDfuImgInfo {
                    image_size: info.image_size,
                    chunk_size: info.chunk_size,
                    crc16: info.crc16,
                    major_ver: info.major_ver,
                    minor_ver: info.minor_ver,
                    filter_key: info.filter_key,
                    gitSHA: info.git_sha,
                };
                let c = unsafe {
                    sys::bm_dfu_initiate_update(c_info, dst.id(), cb, timeout_ms, *internal)
                };
                let rust = self.rust.initiate_update(
                    &mut self.fx,
                    info,
                    dst.id(),
                    *notify,
                    timeout_ms,
                    *internal,
                );
                assert_eq!(c, rust, "bm_dfu_initiate_update's result");
            }
            Step::Feed(len) => {
                let len = u32::from(*len) % (DFU_MAX_CHUNK_SIZE as u32 + 1);
                let mut data = match self.hosted {
                    Some(image) => image.bytes(self.fed..self.fed.saturating_add(len)),
                    None => vec![0; len as usize],
                };
                // `bm_dfu_host_queue_data` refuses a NULL pointer; an empty
                // Vec's is dangling but not NULL.
                data.reserve(1);
                let c = unsafe { sys::bm_dfu_host_queue_data(data.as_mut_ptr(), len) }
                    == sys::BmErr_BmOK;
                let rust = self.rust.roles_mut().host.queue_data(&data);
                assert_eq!(c, rust, "bm_dfu_host_queue_data's result");
                if rust {
                    self.fed = self.fed.saturating_add(len);
                }
            }
            Step::FromClient {
                n,
                success,
                err_code,
            } => {
                let mut tail = vec![*success, *err_code];
                if n % 10 == 2 {
                    // A chunk: `success` and `err_code` are its length.
                    tail.clear();
                }
                let client = NodeRef::Raw(self.rust.roles().host.client_node_id());
                self.deliver(&Step::body(
                    FrameType::Dfu(*n),
                    client,
                    NodeRef::This,
                    &tail,
                ));
            }
            Step::SetConfirm(value) => {
                let c = unsafe {
                    sys::set_config_uint(
                        sys::BmConfigPartition_BM_CFG_PARTITION_SYSTEM,
                        C_DFU_CONFIRM_KEY.as_ptr().cast(),
                        DFU_CONFIRM_KEY.len(),
                        *value,
                    )
                };
                let rust = self
                    .fx
                    .config
                    .partition_mut(Partition::System)
                    .set_uint(Key::new(DFU_CONFIRM_KEY), *value);
                assert_eq!(c, rust, "set_config_uint");
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
