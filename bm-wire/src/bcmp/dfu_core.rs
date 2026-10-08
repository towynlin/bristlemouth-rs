//! The DFU core state machine, ported sans-io from `bcmp/dfu_core.c`.
//!
//! bm_core runs DFU as one hierarchical state machine (`lib_state_machine.c`)
//! on its own task, fed by a five-deep event queue. Received messages, host
//! start requests, timer expiries and the machine's own "run me again" requests
//! all go through that queue, and the task pops one event at a time and runs
//! the current state with it.
//!
//! [`Dfu`] is that machine with the task taken out. It owns the queue, because
//! the queue's depth and ordering are observable: an event is checked against
//! the state it was *queued* in, not the one it is run in, and a pending state
//! change is taken after whatever is already queued ahead of its NOP. The
//! caller decides when to run it: [`Dfu::step`] is one iteration of
//! `bm_dfu_event_thread`.
//!
//! # What is here and what is not
//!
//! | C | Here |
//! |---|---|
//! | `dfu_ctx`, `dfu_event_queue` | [`Core`] |
//! | `lib_sm_run` over `dfu_states` | [`Dfu::step`], [`Dfu::run_event`] |
//! | `s_init_run`, `s_idle_*`, `s_error_*` | private to [`Dfu`] |
//! | the `s_client_*` and `s_host_*` states | [`Roles`], implemented by `dfu_client.c` and `dfu_host.c`'s ports |
//! | `bm_dfu_process_message` | [`Dfu::on_message`] |
//! | `bm_dfu_initiate_update` | [`Dfu::initiate_update`] |
//! | `bm_dfu_send_ack`, `bm_dfu_req_next_chunk`, `bm_dfu_update_end`, `bm_dfu_send_heartbeat` | [`Core::send_ack`] and siblings |
//! | `bm_dfu_set_pending_state_change`, `bm_dfu_set_error`, `bm_dfu_get_current_event` | [`Core`] methods |
//! | `client_update_reboot_info` | [`RebootInfo`], held in [`Core`] |
//! | `bcmp_tx`, the finish callback, `bm_dfu_core_lpm_peripheral_*` | [`Effects`] |
//! | the client's and host's `BmTimer`s, `bm_delay` | [`Timer`], [`Core::start_timer`], [`Core::change_period`], [`Core::delay`], [`Dfu::poll`] |
//! | `bm_dfu_generic.h`, `bm_dfu_host_get_chunk`, `git_sha`, the config store | [`Effects`] |
//!
//! # Time
//!
//! The C's timers run on the RTOS timer task and post an event when they fire;
//! `bm_delay` blocks the DFU task while they do. [`Core`] keeps the timers as
//! deadlines against a clock the caller advances: [`Dfu::poll`] and
//! [`Dfu::step`] move it to `now_ms` and post every timer that has come due,
//! and [`Core::delay`] moves it on by the delay mid-run, posting any that
//! come due inside it. Nothing waits. Timers come due in deadline order, ties
//! in [`Timer`] order, which is the order `bm_dfu_init` creates them in.
//!
//! `dfu_copy_and_process_message`'s other half — forwarding a message for
//! another node when it arrived link-local — is `bm-stack`'s, as it is for
//! config and time. [`DfuAddress::of_body`] is the read it decides on.
//!
//! # Quirks reproduced
//!
//! * The source check in [`Dfu::on_message`] uses the state at queue time
//!   (the C's `bm_dfu_process_message` runs on the BCMP task).
//! * A NOP that does not fit the queue is dropped; the pending change is then
//!   taken after the next event, which the old state runs first.
//! * A pending change to the current state neither exits nor re-enters it.
//! * The error state reports to the finish callback of the last *host* update,
//!   whichever role failed (divergence #58).
//! * An error value of [`DfuErr::FLASH_ACCESS`] or above leaves the machine in
//!   [`State::Error`] until reboot. The host takes that value from a received
//!   `err_code` byte, so a peer can choose it (divergence #59).
//! * `initiate_update` checks the state when it is called, not when its event
//!   is run, and sets [`Core::internal`] on every call it queues (divergence
//!   #60).

use crate::bcmp::dfu::{
    DFU_MAX_CHUNK_SIZE, DfuAddress, DfuChunkRequest, DfuMessage, DfuResult, DfuStart, ImgInfo,
};
use crate::bcmp::header::BCMP_HEADER_LEN;
use crate::configuration::{ConfigStore, Partition};
use crate::frame::MIN_FRAME_WITH_ADDRESSES;
use crate::le;
use crate::{BmWireError, bcmp::MessageType};

/// Depth of `dfu_event_queue`, from `bm_queue_create(5, ...)` in `bm_dfu_init`.
pub const EVENT_QUEUE_LEN: usize = 5;

/// `DFU_REBOOT_MAGIC`: [`RebootInfo::magic`] when a client rebooted into a
/// new image and has yet to confirm it.
pub const DFU_REBOOT_MAGIC: u32 = 0xBADC_0FFE;

/// Longest BCMP body a received frame can carry, and so the longest body an
/// [`Event`] holds: a 1514-byte Ethernet frame less its Ethernet, IPv6 and
/// BCMP headers. `dfu_copy_and_process_message` copies the whole body.
pub const MAX_EVENT_BODY_LEN: usize = 1514 - MIN_FRAME_WITH_ADDRESSES - BCMP_HEADER_LEN;

/// `enum BmDfuHfsmStates`, with its C values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum State {
    /// `BmDfuStateInit`: waiting for [`EventType::InitSuccess`].
    Init = 0,
    /// `BmDfuStateIdle`.
    Idle = 1,
    /// `BmDfuStateError`.
    Error = 2,
    /// `BmDfuStateClientReceiving`.
    ClientReceiving = 3,
    /// `BmDfuStateClientValidating`.
    ClientValidating = 4,
    /// `BmDfuStateClientRebootReq`.
    ClientRebootReq = 5,
    /// `BmDfuStateClientRebootDone`.
    ClientRebootDone = 6,
    /// `BmDfuStateClientActivating`.
    ClientActivating = 7,
    /// `BmDfuStateHostReqUpdate`.
    HostReqUpdate = 8,
    /// `BmDfuStateHostUpdate`.
    HostUpdate = 9,
}

impl State {
    /// All ten, in C order.
    pub const ALL: [Self; 10] = [
        Self::Init,
        Self::Idle,
        Self::Error,
        Self::ClientReceiving,
        Self::ClientValidating,
        Self::ClientRebootReq,
        Self::ClientRebootDone,
        Self::ClientActivating,
        Self::HostReqUpdate,
        Self::HostUpdate,
    ];

    /// The state with C value `value`, if there is one.
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }

    /// A `BmDfuStateClient*` state: messages are accepted only from the host.
    #[must_use]
    pub fn is_client(self) -> bool {
        matches!(
            self,
            Self::ClientReceiving
                | Self::ClientValidating
                | Self::ClientRebootReq
                | Self::ClientRebootDone
                | Self::ClientActivating
        )
    }

    /// A `BmDfuStateHost*` state: messages are accepted only from the client.
    #[must_use]
    pub fn is_host(self) -> bool {
        matches!(self, Self::HostReqUpdate | Self::HostUpdate)
    }
}

/// `enum BmDfuEvtType`, with its C values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum EventType {
    /// `DfuEventNone`: the NOP a pending state change queues.
    None = 0,
    /// `DfuEventInitSuccess`: queued by `bm_dfu_init`.
    InitSuccess = 1,
    /// `DfuEventReceivedUpdateRequest`: a `0xD0`.
    ReceivedUpdateRequest = 2,
    /// `DfuEventChunkRequest`: a `0xD1`.
    ChunkRequest = 3,
    /// `DfuEventImageChunk`: a `0xD2`.
    ImageChunk = 4,
    /// `DfuEventUpdateEnd`: a `0xD3`.
    UpdateEnd = 5,
    /// `DfuEventAckReceived`: a `0xD4`.
    AckReceived = 6,
    /// `DfuEventAckTimeout`: the host's ACK timer.
    AckTimeout = 7,
    /// `DfuEventChunkTimeout`: the client's chunk timer.
    ChunkTimeout = 8,
    /// `DfuEventHeartbeat`: a `0xD6`.
    Heartbeat = 9,
    /// `DfuEventAbort`: a `0xD5`, or the host's update timer.
    Abort = 10,
    /// `DfuEventBeginHost`: queued by `bm_dfu_initiate_update`.
    BeginHost = 11,
    /// `DfuEventRebootRequest`: a `0xD7`.
    RebootRequest = 12,
    /// `DfuEventReboot`: a `0xD8`.
    Reboot = 13,
    /// `DfuEventBootComplete`: a `0xD9`.
    BootComplete = 14,
}

impl EventType {
    /// The event `bm_dfu_process_message` queues for a body whose first byte
    /// is `frame_type`, or `None` for a byte its `switch` has no case for.
    #[must_use]
    pub fn for_frame_type(frame_type: u8) -> Option<Self> {
        Some(match MessageType(u16::from(frame_type)) {
            MessageType::DFU_START => Self::ReceivedUpdateRequest,
            MessageType::DFU_PAYLOAD => Self::ImageChunk,
            MessageType::DFU_END => Self::UpdateEnd,
            MessageType::DFU_ACK => Self::AckReceived,
            MessageType::DFU_ABORT => Self::Abort,
            MessageType::DFU_HEARTBEAT => Self::Heartbeat,
            MessageType::DFU_PAYLOAD_REQ => Self::ChunkRequest,
            MessageType::DFU_REBOOT_REQ => Self::RebootRequest,
            MessageType::DFU_REBOOT => Self::Reboot,
            MessageType::DFU_BOOT_COMPLETE => Self::BootComplete,
            _ => return None,
        })
    }
}

/// `BmDfuErr`, as its value.
///
/// A newtype rather than an enum because the host stores a received
/// `err_code` byte here unchecked, so every `u8` occurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct DfuErr(pub u8);

impl DfuErr {
    /// `BmDfuErrNone`.
    pub const NONE: Self = Self(0);
    /// `BmDfuErrTooLarge`.
    pub const TOO_LARGE: Self = Self(1);
    /// `BmDfuErrSameVer`.
    pub const SAME_VER: Self = Self(2);
    /// `BmDfuErrMismatchLen`.
    pub const MISMATCH_LEN: Self = Self(3);
    /// `BmDfuErrBadCrc`.
    pub const BAD_CRC: Self = Self(4);
    /// `BmDfuErrImgChunkAccess`.
    pub const IMG_CHUNK_ACCESS: Self = Self(5);
    /// `BmDfuErrTimeout`.
    pub const TIMEOUT: Self = Self(6);
    /// `BmDfuErrBmFrame`.
    pub const BM_FRAME: Self = Self(7);
    /// `BmDfuErrAborted`.
    pub const ABORTED: Self = Self(8);
    /// `BmDfuErrWrongVer`.
    pub const WRONG_VER: Self = Self(9);
    /// `BmDfuErrInProgress`.
    pub const IN_PROGRESS: Self = Self(10);
    /// `BmDfuErrChunkSize`.
    pub const CHUNK_SIZE: Self = Self(11);
    /// `BmDfuErrUnkownNodeId`.
    pub const UNKNOWN_NODE_ID: Self = Self(12);
    /// `BmDfuErrConfirmationAbort`.
    pub const CONFIRMATION_ABORT: Self = Self(13);
    /// `BmDfuErrFlashAccess`, the first "fatal" value.
    pub const FLASH_ACCESS: Self = Self(14);

    /// `s_error_entry`'s test: at or past [`Self::FLASH_ACCESS`], the machine
    /// stays in [`State::Error`].
    #[must_use]
    pub fn is_fatal(self) -> bool {
        self.0 >= Self::FLASH_ACCESS.0
    }
}

/// The DFU timers that post events, in the order `bm_dfu_client_init` and
/// `bm_dfu_host_init` create them. All are one-shot.
///
/// `host_ctx.heartbeat_timer` is not here. `s_host_update_run` starts it
/// before reading a chunk and stops it after sending one, so it fires only
/// while that read blocks for a second or more, and
/// [`Effects::host_get_chunk`] does not block. See `bcmp::dfu_host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Timer {
    /// `CLIENT_CTX.chunk_timer`: [`EventType::ChunkTimeout`] after
    /// `bm_dfu_client_chunk_timeout_ms`.
    Chunk,
    /// `host_ctx.ack_timer`: [`EventType::AckTimeout`] after
    /// `bm_dfu_host_ack_timeout_ms`.
    Ack,
    /// `host_ctx.update_timer`: [`EventType::Abort`] after the update's
    /// `timeoutMs`, which [`Core::change_period`] sets.
    Update,
}

impl Timer {
    /// All, in creation order.
    pub const ALL: [Self; 3] = [Self::Chunk, Self::Ack, Self::Update];

    /// The period it is created with.
    #[must_use]
    pub const fn period_ms(self) -> u32 {
        match self {
            // `bm_dfu_client_chunk_timeout_ms`, dfu_client.h.
            Self::Chunk => 2_000,
            // `bm_dfu_host_ack_timeout_ms`, dfu_host.h.
            Self::Ack => 10_000,
            // `bm_dfu_update_default_timeout_ms`, dfu_host.h.
            Self::Update => 300_000,
        }
    }

    /// The event its handler posts.
    #[must_use]
    pub const fn event(self) -> EventType {
        match self {
            Self::Chunk => EventType::ChunkTimeout,
            Self::Ack => EventType::AckTimeout,
            Self::Update => EventType::Abort,
        }
    }
}

/// `ReboootClientUpdateInfo`: what a client leaves in no-init RAM for the
/// image it reboots into.
///
/// `s_init_run` reads [`Self::magic`] to choose between [`State::Idle`] and
/// [`State::ClientRebootDone`], and `s_idle_entry` zeroes the whole struct.
/// The C struct is packed; [`Self::encode`] is its layout, which a C image and
/// a Rust image on the same part must share for an update from one to the
/// other to complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct RebootInfo {
    /// [`DFU_REBOOT_MAGIC`], or anything else.
    pub magic: u32,
    /// Major version of the image being booted into.
    pub major: u8,
    /// Minor version of the image being booted into.
    pub minor: u8,
    /// The host that sent it.
    pub host_node_id: u64,
    /// `gitSHA` of the image being booted into.
    pub git_sha: u32,
}

impl RebootInfo {
    /// `sizeof(ReboootClientUpdateInfo)`.
    pub const LEN: usize = 18;

    /// Decode the packed layout.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let b: &[u8; Self::LEN] = le::prefix(buf)?;
        Ok(Self {
            magic: le::u32_at(b, 0),
            major: b[4],
            minor: b[5],
            host_node_id: le::u64_at(b, 6),
            git_sha: le::u32_at(b, 14),
        })
    }

    /// Encode the packed layout.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than [`Self::LEN`].
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let b = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        b[0..4].copy_from_slice(&self.magic.to_le_bytes());
        b[4] = self.major;
        b[5] = self.minor;
        b[6..14].copy_from_slice(&self.host_node_id.to_le_bytes());
        b[14..18].copy_from_slice(&self.git_sha.to_le_bytes());
        Ok(())
    }
}

/// `DfuHostStartEvent`: what `bm_dfu_initiate_update` queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct HostStart {
    /// The `0xD0` body the host will send, addressed from this node.
    pub start: DfuStart,
    /// Whether a finish callback was given. The C keeps the pointer; here the
    /// call is [`Effects::update_finished`].
    pub notify: bool,
    /// `timeoutMs`, handed to the host as its update timeout.
    pub timeout_ms: u32,
}

/// What an event carries: `BmDfuEvent::buf`.
#[derive(Clone, Copy)]
#[allow(
    clippy::large_enum_variant,
    reason = "no alloc: the body is held inline where the C mallocs it"
)]
pub enum EventData {
    /// `buf == NULL`: timers, NOPs and `InitSuccess`.
    None,
    /// A received DFU body, whole, as `dfu_copy_and_process_message` copied it.
    Message {
        /// Bytes in use.
        len: usize,
        /// The body, then zeros.
        bytes: [u8; MAX_EVENT_BODY_LEN],
    },
    /// A [`EventType::BeginHost`]'s start request.
    HostStart(HostStart),
}

impl core::fmt::Debug for EventData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Message { len, bytes } => {
                f.debug_tuple("Message").field(&&bytes[..*len]).finish()
            }
            Self::HostStart(h) => f.debug_tuple("HostStart").field(h).finish(),
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for EventData {
    fn format(&self, f: defmt::Formatter<'_>) {
        match self {
            Self::None => defmt::write!(f, "None"),
            Self::Message { len, bytes } => defmt::write!(f, "Message({=[u8]})", &bytes[..*len]),
            Self::HostStart(h) => defmt::write!(f, "HostStart({})", h),
        }
    }
}

impl PartialEq for EventData {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Message { .. }, Self::Message { .. }) => self.body() == other.body(),
            (Self::HostStart(a), Self::HostStart(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for EventData {}

impl EventData {
    /// The received body, if this carries one.
    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        match self {
            Self::Message { len, bytes } => Some(&bytes[..*len]),
            _ => None,
        }
    }
}

/// `BmDfuEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Event {
    /// `type`.
    pub kind: EventType,
    /// `buf` and `len`.
    pub data: EventData,
}

impl Event {
    /// `{DfuEventNone, NULL, 0}`.
    pub const NONE: Self = Self::bare(EventType::None);

    /// An event with no buffer, as the timers and `bm_dfu_init` queue them.
    #[must_use]
    pub const fn bare(kind: EventType) -> Self {
        Self {
            kind,
            data: EventData::None,
        }
    }

    /// An event carrying a copy of `body`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Invalid`] if `body` is longer than
    /// [`MAX_EVENT_BODY_LEN`].
    pub fn message(kind: EventType, body: &[u8]) -> Result<Self, BmWireError> {
        let mut bytes = [0u8; MAX_EVENT_BODY_LEN];
        bytes
            .get_mut(..body.len())
            .ok_or(BmWireError::Invalid)?
            .copy_from_slice(body);
        Ok(Self {
            kind,
            data: EventData::Message {
                len: body.len(),
                bytes,
            },
        })
    }

    /// The received body, if this event carries one.
    #[must_use]
    pub fn body(&self) -> Option<&[u8]> {
        self.data.body()
    }

    /// The received body decoded, if this event carries one that decodes.
    #[must_use]
    pub fn message_body(&self) -> Option<DfuMessage<'_>> {
        DfuMessage::decode(self.body()?).ok()
    }
}

/// What the machine does outside itself. The C calls these directly.
pub trait Effects {
    /// `bcmp_tx(&multicast_global_addr, frame_type, body, len, 0, NULL)`:
    /// every DFU message goes to `ff03::1`, unsequenced, and a failure to send
    /// is only logged.
    fn send(&mut self, message: &DfuMessage<'_>);
    /// `bm_dfu_core_lpm_peripheral_active`, on leaving [`State::Idle`].
    fn lpm_peripheral_active(&mut self);
    /// `bm_dfu_core_lpm_peripheral_inactive`, on entering [`State::Idle`] or
    /// [`State::Error`].
    fn lpm_peripheral_inactive(&mut self);
    /// The `UpdateFinishCb` given to `bm_dfu_initiate_update`, called only if
    /// one was given.
    fn update_finished(&mut self, success: bool, err: DfuErr, node_id: u64);

    /// `bm_dfu_client_flash_area_open`: open the update slot. The C keeps the
    /// handle it returns; here the implementation keeps it.
    fn flash_open(&mut self) -> bool;
    /// `bm_dfu_client_flash_area_close`. The client ignores the result.
    fn flash_close(&mut self) -> bool;
    /// `bm_dfu_client_flash_area_get_size`.
    fn flash_size(&mut self) -> u32;
    /// `bm_dfu_client_flash_area_erase`.
    fn flash_erase(&mut self, offset: u32, len: u32) -> bool;
    /// `bm_dfu_client_flash_area_write`.
    fn flash_write(&mut self, offset: u32, data: &[u8]) -> bool;
    /// `bm_dfu_host_get_chunk(offset, buf, len, timeout)`: read `buf.len()`
    /// bytes of the image this node hosts from `offset` of its own slot. The
    /// image starts at [`ImgInfo::LEN`] (`DFU_IMG_START_OFFSET_BYTES`).
    fn host_get_chunk(&mut self, offset: u32, buf: &mut [u8]) -> bool;
    /// `bm_dfu_client_set_confirmed`: mark the running image good.
    fn set_confirmed(&mut self);
    /// `bm_dfu_client_set_pending_and_reset`: mark the received image to be
    /// tried on the next boot, and reset. Does not return on hardware.
    fn set_pending_and_reset(&mut self);
    /// `bm_dfu_client_fail_update_and_reset`: revert to the previous image,
    /// and reset. Does not return on hardware.
    fn fail_update_and_reset(&mut self);
    /// `git_sha()`: the running image's SHA, which a `0xD0` is compared with.
    fn git_sha(&self) -> u32;
    /// `CONFIGS`, where the client keeps `dfu_confirm`, or `None` for a node
    /// without one. Every `get_config_*` then fails and every `set_config_*`
    /// refuses.
    fn config(&mut self) -> Option<&mut ConfigStore>;
    /// `save_config(partition, true)`: seal, write, then reset. Does not
    /// return on hardware.
    fn commit_config(&mut self, partition: Partition) -> bool;
}

/// The client and host states, which `dfu_client.c` and `dfu_host.c` supply.
///
/// [`Dfu`] calls these for the seven `BmDfuStateClient*` and
/// `BmDfuStateHost*` states, and for the two hand-offs out of
/// [`State::Idle`]. Each gets the [`Core`] to read the current event, send,
/// queue events and request state changes through, as the C files call back
/// into `dfu_core.c`.
///
/// Timers and `bm_delay` are the [`Core`]'s, since a delay in one role fires
/// the other's timers too.
pub trait Roles {
    /// `on_state_entry` for a client or host state.
    fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects);
    /// `run` for a client or host state.
    fn run(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects);
    /// `on_state_exit` for a client or host state. Only
    /// [`State::HostUpdate`] has one in the C.
    fn exit(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects);
    /// `bm_dfu_client_process_update_request`: a `0xD0` run in
    /// [`State::Idle`].
    fn client_process_update_request(&mut self, core: &mut Core, fx: &mut dyn Effects);
    /// `bm_dfu_host_set_params`: a [`EventType::BeginHost`] run in
    /// [`State::Idle`], before the change to [`State::HostReqUpdate`].
    fn host_set_params(&mut self, notify: bool, timeout_ms: u32);
    /// `bm_dfu_client_host_node_valid`: accept a message from `node_id` in a
    /// client state.
    fn client_host_node_valid(&self, node_id: u64) -> bool;
    /// `bm_dfu_host_client_node_valid`: accept a message from `node_id` in a
    /// host state.
    fn host_client_node_valid(&self, node_id: u64) -> bool;
}

/// What [`Dfu::on_message`] did with a body, in the order
/// `bm_dfu_process_message` decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Accepted {
    /// Queued as this event.
    Queued(EventType),
    /// `dst_node_id` is not this node.
    NotForUs,
    /// The source is not the peer the current client or host state accepts.
    WrongPeer,
    /// The first byte is not a DFU type. The C leaks the copy (divergence
    /// #54).
    UnknownType,
    /// The queue was full.
    QueueFull,
    /// Shorter than `frame_type` and an address. The C reads past the end
    /// (divergence #55).
    Truncated,
    /// Longer than [`MAX_EVENT_BODY_LEN`], which no received frame can be.
    TooLong,
}

/// `dfu_event_queue`: a bounded FIFO whose sends fail when it is full.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct EventQueue {
    slots: [Event; EVENT_QUEUE_LEN],
    head: usize,
    len: usize,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl EventQueue {
    /// Empty.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Event::NONE; EVENT_QUEUE_LEN],
            head: 0,
            len: 0,
        }
    }

    /// `bm_queue_send(q, &event, 0)`: `false` if full.
    pub fn push(&mut self, event: Event) -> bool {
        if self.len == EVENT_QUEUE_LEN {
            return false;
        }
        self.slots[(self.head + self.len) % EVENT_QUEUE_LEN] = event;
        self.len += 1;
        true
    }

    /// `bm_queue_receive`: the oldest event.
    pub fn pop(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }
        let event = self.slots[self.head];
        self.slots[self.head] = Event::NONE;
        self.head = (self.head + 1) % EVENT_QUEUE_LEN;
        self.len -= 1;
        Some(event)
    }

    /// Events waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Nothing waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        (0..self.len).map(|i| &self.slots[(self.head + i) % EVENT_QUEUE_LEN])
    }
}

/// `dfu_core_ctx_t` and the queue: everything of the core's that the roles
/// may touch.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Core {
    state: State,
    current: Event,
    pending: Option<State>,
    error: DfuErr,
    self_node_id: u64,
    client_node_id: u64,
    notify: bool,
    internal: bool,
    queue: EventQueue,
    reboot_info: RebootInfo,
    /// The time last passed in, plus any delays since. `None` until then.
    clock: Option<u32>,
    /// Deadline per [`Timer`], `None` when stopped.
    timers: [Option<u32>; Timer::ALL.len()],
    /// Period per [`Timer`].
    periods: [u32; Timer::ALL.len()],
}

impl Core {
    /// Current state: `get_current_state_enum`.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The event being run: `bm_dfu_get_current_event`.
    #[must_use]
    pub fn current_event(&self) -> &Event {
        &self.current
    }

    /// The state a pending change will move to after the current run, if
    /// one is pending.
    #[must_use]
    pub fn pending_state_change(&self) -> Option<State> {
        self.pending
    }

    /// `bm_dfu_set_pending_state_change`: move to `state` once the current
    /// run returns, and queue a NOP so there is a run to return from. The NOP
    /// is dropped if the queue is full; the change stays pending.
    pub fn set_pending_state_change(&mut self, state: State) {
        self.pending = Some(state);
        let _ = self.queue.push(Event::NONE);
    }

    /// `bm_dfu_get_error`. Never cleared: it is the last error set.
    #[must_use]
    pub fn error(&self) -> DfuErr {
        self.error
    }

    /// `bm_dfu_set_error`.
    pub fn set_error(&mut self, error: DfuErr) {
        self.error = error;
    }

    /// `dfu_ctx.self_node_id`.
    #[must_use]
    pub fn self_node_id(&self) -> u64 {
        self.self_node_id
    }

    /// `dfu_ctx.client_node_id`: the destination of the last
    /// [`EventType::BeginHost`] run. What [`Effects::update_finished`] is
    /// told from [`State::Error`].
    #[must_use]
    pub fn client_node_id(&self) -> u64 {
        self.client_node_id
    }

    /// Whether the last [`EventType::BeginHost`] run carried a finish
    /// callback. Only the next one replaces it (divergence #58).
    #[must_use]
    pub fn notify(&self) -> bool {
        self.notify
    }

    /// `bm_dfu_internal`: the `internal` flag of the last
    /// [`Dfu::initiate_update`] that queued an event.
    #[must_use]
    pub fn internal(&self) -> bool {
        self.internal
    }

    /// The event queue.
    #[must_use]
    pub fn queue(&self) -> &EventQueue {
        &self.queue
    }

    /// `bm_queue_send(bm_dfu_get_event_queue(), &event, 0)`, as the client
    /// and host timers do. `false` if the queue is full.
    pub fn post(&mut self, event: Event) -> bool {
        self.queue.push(event)
    }

    /// `client_update_reboot_info`. The integrator keeps it in no-init RAM.
    #[must_use]
    pub fn reboot_info(&self) -> &RebootInfo {
        &self.reboot_info
    }

    /// `client_update_reboot_info`, for the client to fill in before it
    /// reboots.
    pub fn reboot_info_mut(&mut self) -> &mut RebootInfo {
        &mut self.reboot_info
    }

    /// The machine's clock: the last `now_ms` it was given, plus any
    /// [`Self::delay`] since. Zero before the first.
    #[must_use]
    pub fn now(&self) -> u32 {
        self.clock.unwrap_or(0)
    }

    /// `bm_timer_start`: (re)arm `timer` for its period from [`Self::now`].
    pub fn start_timer(&mut self, timer: Timer) {
        self.timers[timer as usize] = Some(self.now().wrapping_add(self.periods[timer as usize]));
    }

    /// `bm_timer_change_period`: set `timer`'s period and (re)arm it, as
    /// FreeRTOS's `xTimerChangePeriod` does. The period is kept for later
    /// starts.
    pub fn change_period(&mut self, timer: Timer, period_ms: u32) {
        self.periods[timer as usize] = period_ms;
        self.start_timer(timer);
    }

    /// `timer`'s current period.
    #[must_use]
    pub fn period(&self, timer: Timer) -> u32 {
        self.periods[timer as usize]
    }

    /// `bm_timer_stop`.
    pub fn stop_timer(&mut self, timer: Timer) {
        self.timers[timer as usize] = None;
    }

    /// When `timer` fires, if it is running.
    #[must_use]
    pub fn timer_deadline(&self, timer: Timer) -> Option<u32> {
        self.timers[timer as usize]
    }

    /// `bm_delay(ms)`: the clock moves on, and any timer that comes due
    /// meanwhile posts its event.
    pub fn delay(&mut self, ms: u32) {
        let now = self.now().wrapping_add(ms);
        self.clock = Some(now);
        self.fire_due();
    }

    /// Move the clock to `now_ms` unless it is already past it, then fire
    /// what is due.
    fn advance_to(&mut self, now_ms: u32) {
        let ahead = self
            .clock
            .is_some_and(|clock| (now_ms.wrapping_sub(clock) as i32) < 0);
        if !ahead {
            self.clock = Some(now_ms);
        }
        self.fire_due();
    }

    /// Post each due timer's event, earliest deadline first, ties in
    /// [`Timer`] order. A post that finds the queue full is lost, as the C's
    /// handlers only log it.
    fn fire_due(&mut self) {
        let now = self.now();
        while let Some(timer) = Timer::ALL
            .into_iter()
            .filter(|t| self.timers[*t as usize].is_some_and(|d| (now.wrapping_sub(d) as i32) >= 0))
            .min_by_key(|t| self.timers[*t as usize].map(|d| d.wrapping_sub(now) as i32))
        {
            self.timers[timer as usize] = None;
            let _ = self.queue.push(Event::bare(timer.event()));
        }
    }

    /// The earliest running timer's deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u32> {
        let now = self.now();
        self.timers
            .iter()
            .flatten()
            .min_by_key(|d| d.wrapping_sub(now) as i32)
            .copied()
    }

    fn address_to(&self, dst_node_id: u64) -> DfuAddress {
        DfuAddress {
            src_node_id: self.self_node_id,
            dst_node_id,
        }
    }

    /// `bm_dfu_send_ack`.
    pub fn send_ack(&self, fx: &mut dyn Effects, dst_node_id: u64, success: u8, err: DfuErr) {
        fx.send(&DfuMessage::Ack(DfuResult {
            addresses: self.address_to(dst_node_id),
            success,
            err_code: err.0,
        }));
    }

    /// `bm_dfu_req_next_chunk`.
    pub fn req_next_chunk(&self, fx: &mut dyn Effects, dst_node_id: u64, chunk_num: u16) {
        fx.send(&DfuMessage::PayloadReq(DfuChunkRequest {
            addresses: self.address_to(dst_node_id),
            seq_num: chunk_num,
        }));
    }

    /// `bm_dfu_update_end`.
    pub fn update_end(&self, fx: &mut dyn Effects, dst_node_id: u64, success: u8, err: DfuErr) {
        fx.send(&DfuMessage::End(DfuResult {
            addresses: self.address_to(dst_node_id),
            success,
            err_code: err.0,
        }));
    }

    /// `bm_dfu_send_heartbeat`.
    pub fn send_heartbeat(&self, fx: &mut dyn Effects, dst_node_id: u64) {
        fx.send(&DfuMessage::Heartbeat(self.address_to(dst_node_id)));
    }
}

/// The DFU state machine: `dfu_core.c` over a [`Roles`] implementation.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Dfu<R> {
    core: Core,
    roles: R,
}

impl<R: Roles> Dfu<R> {
    /// `bm_dfu_init`: in [`State::Init`], with [`EventType::InitSuccess`]
    /// queued and nothing run.
    ///
    /// `reboot_info` is what no-init RAM held at boot.
    #[must_use]
    pub fn new(self_node_id: u64, reboot_info: RebootInfo, roles: R) -> Self {
        let mut queue = EventQueue::new();
        queue.push(Event::bare(EventType::InitSuccess));
        Self {
            core: Core {
                state: State::Init,
                current: Event::NONE,
                pending: None,
                error: DfuErr::NONE,
                self_node_id,
                client_node_id: 0,
                notify: false,
                internal: false,
                queue,
                reboot_info,
                clock: None,
                timers: [None; Timer::ALL.len()],
                periods: [
                    Timer::Chunk.period_ms(),
                    Timer::Ack.period_ms(),
                    Timer::Update.period_ms(),
                ],
            },
            roles,
        }
    }

    /// The core's state.
    #[must_use]
    pub fn core(&self) -> &Core {
        &self.core
    }

    /// The core's state, for a caller standing in for a role — the C's
    /// public `bm_dfu_set_error` and `bm_dfu_set_pending_state_change`.
    pub fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    /// The roles.
    #[must_use]
    pub fn roles(&self) -> &R {
        &self.roles
    }

    /// The roles, mutably.
    pub fn roles_mut(&mut self) -> &mut R {
        &mut self.roles
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> State {
        self.core.state
    }

    /// `bm_dfu_process_message`: check a received body against this node and
    /// the current state, and queue the event its first byte names.
    ///
    /// Call it for a body whose [`DfuAddress::dst_node_id`] is this node;
    /// `dfu_copy_and_process_message` forwards the rest.
    pub fn on_message(&mut self, body: &[u8]) -> Accepted {
        let Ok(address) = DfuAddress::of_body(body) else {
            return Accepted::Truncated;
        };
        if address.dst_node_id != self.core.self_node_id {
            return Accepted::NotForUs;
        }
        let state = self.core.state;
        let valid = if state.is_client() {
            self.roles.client_host_node_valid(address.src_node_id)
        } else if state.is_host() {
            self.roles.host_client_node_valid(address.src_node_id)
        } else {
            true
        };
        if !valid {
            return Accepted::WrongPeer;
        }
        let Some(kind) = EventType::for_frame_type(body[0]) else {
            return Accepted::UnknownType;
        };
        let Ok(event) = Event::message(kind, body) else {
            return Accepted::TooLong;
        };
        if self.core.queue.push(event) {
            Accepted::Queued(kind)
        } else {
            Accepted::QueueFull
        }
    }

    /// `bm_dfu_initiate_update`: queue a request to update `dst_node_id` as
    /// its host.
    ///
    /// Refuses a chunk size over [`DFU_MAX_CHUNK_SIZE`] silently, and anything
    /// outside [`State::Idle`] or past a full queue with
    /// [`Effects::update_finished`]`(false, IN_PROGRESS, dst_node_id)` if
    /// `notify` is set. The state is checked now, not when the event runs.
    pub fn initiate_update(
        &mut self,
        fx: &mut dyn Effects,
        info: ImgInfo,
        dst_node_id: u64,
        notify: bool,
        timeout_ms: u32,
        internal: bool,
    ) -> bool {
        if usize::from(info.chunk_size) > DFU_MAX_CHUNK_SIZE {
            return false;
        }
        let refuse = |fx: &mut dyn Effects| {
            if notify {
                fx.update_finished(false, DfuErr::IN_PROGRESS, dst_node_id);
            }
            false
        };
        if self.core.state != State::Idle {
            return refuse(fx);
        }
        let start = HostStart {
            start: DfuStart {
                addresses: self.core.address_to(dst_node_id),
                img_info: info,
            },
            notify,
            timeout_ms,
        };
        let event = Event {
            kind: EventType::BeginHost,
            data: EventData::HostStart(start),
        };
        if !self.core.queue.push(event) {
            return refuse(fx);
        }
        self.core.internal = internal;
        true
    }

    /// Move the clock to `now_ms` and post the event of every timer that has
    /// come due — what the RTOS timer task does while the DFU task waits.
    pub fn poll(&mut self, now_ms: u32) {
        self.core.advance_to(now_ms);
    }

    /// The earliest running timer's deadline: when [`Self::poll`] next has
    /// something to do.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u32> {
        self.core.next_deadline()
    }

    /// One iteration of `bm_dfu_event_thread`: [`Self::poll`], then take the
    /// oldest event and run the machine with it. Returns the event's type, or
    /// `None` if the queue was empty and nothing ran.
    pub fn step(&mut self, fx: &mut dyn Effects, now_ms: u32) -> Option<EventType> {
        self.poll(now_ms);
        let event = self.pop()?;
        let kind = event.kind;
        self.run_event(event, fx, now_ms);
        Some(kind)
    }

    /// Take the oldest event without running it. [`Self::step`] is this
    /// followed by [`Self::run_event`].
    pub fn pop(&mut self) -> Option<Event> {
        self.core.queue.pop()
    }

    /// Run the machine once with `event`, bypassing the queue:
    /// `bm_dfu_test_set_dfu_event_and_run_sm`, which bm_core's `dfu_test.cpp`
    /// drives.
    ///
    /// The clock moves to `now_ms` first, which may post timer events behind
    /// the queue.
    pub fn run_event(&mut self, event: Event, fx: &mut dyn Effects, now_ms: u32) {
        self.poll(now_ms);
        self.core.current = event;
        self.lib_sm_run(fx);
        self.core.current = Event::NONE;
    }

    /// `lib_sm_run` with `bm_dfu_check_transitions`: run the state, then take
    /// a pending change. A change to the state already current is taken
    /// without exit or entry, as the C compares state pointers.
    fn lib_sm_run(&mut self, fx: &mut dyn Effects) {
        let state = self.core.state;
        self.run_state(state, fx);
        let Some(next) = self.core.pending.take() else {
            return;
        };
        if next == state {
            return;
        }
        self.exit_state(state, fx);
        self.core.state = next;
        self.enter_state(next, fx);
    }

    fn run_state(&mut self, state: State, fx: &mut dyn Effects) {
        let core = &mut self.core;
        match state {
            // s_init_run
            State::Init => {
                if core.current.kind == EventType::InitSuccess {
                    let next = if core.reboot_info.magic == DFU_REBOOT_MAGIC {
                        State::ClientRebootDone
                    } else {
                        State::Idle
                    };
                    core.set_pending_state_change(next);
                }
            }
            // s_idle_run
            State::Idle => match core.current.kind {
                EventType::ReceivedUpdateRequest => {
                    self.roles.client_process_update_request(core, fx);
                }
                EventType::BeginHost => {
                    // Only `initiate_update` queues a `BeginHost`, always with
                    // its start request; the C would read through a NULL `buf`.
                    if let EventData::HostStart(start) = core.current.data {
                        core.notify = start.notify;
                        core.client_node_id = start.start.addresses.dst_node_id;
                        self.roles.host_set_params(start.notify, start.timeout_ms);
                        core.set_pending_state_change(State::HostReqUpdate);
                    }
                }
                _ => {}
            },
            // s_error_run
            State::Error => {}
            _ => self.roles.run(state, core, fx),
        }
    }

    fn enter_state(&mut self, state: State, fx: &mut dyn Effects) {
        let core = &mut self.core;
        match state {
            State::Init => {}
            // s_idle_entry
            State::Idle => {
                fx.lpm_peripheral_inactive();
                core.reboot_info = RebootInfo::default();
            }
            // s_error_entry
            State::Error => {
                fx.lpm_peripheral_inactive();
                if core.notify {
                    fx.update_finished(false, core.error, core.client_node_id);
                }
                if !core.error.is_fatal() {
                    core.set_pending_state_change(State::Idle);
                }
            }
            _ => self.roles.entry(state, core, fx),
        }
    }

    fn exit_state(&mut self, state: State, fx: &mut dyn Effects) {
        match state {
            State::Init | State::Error => {}
            // s_idle_exit
            State::Idle => fx.lpm_peripheral_active(),
            _ => self.roles.exit(state, &mut self.core, fx),
        }
    }
}

#[cfg(test)]
mod tests;
