//! The DFU host, ported sans-io from `bcmp/dfu_host.c`.
//!
//! [`Host`] is `dfu_host_ctx_t` and the two `s_host_*` states. It runs inside
//! [`Dfu`](crate::bcmp::dfu_core::Dfu), which calls it through [`Roles`];
//! reading the image goes through [`Effects::host_get_chunk`], and its timers
//! through [`Core`]. [`ClientHost`] is a node that runs both roles.
//!
//! | C | Here |
//! |---|---|
//! | `s_host_req_update_*`, `s_host_update_*` | [`Host::entry`], [`Host::run`], [`Host::exit`] |
//! | `bm_dfu_host_req_update`, `_send_chunk`, `_send_reboot` | private |
//! | `ack_timer`, `update_timer` | [`Timer::Ack`], [`Timer::Update`] |
//! | `heartbeat_timer` | nothing; see below |
//! | `bm_dfu_host_set_params` | [`Host::set_params`] |
//! | `bm_dfu_host_client_node_valid` | [`Host::client_node_valid`] |
//! | `bm_dfu_host_queue_data`, `data_queue` | [`Host::queue_data`], [`StreamBuffer`] |
//!
//! # Where the image comes from
//!
//! `bm_dfu_internal()` ([`Core::internal`]) chooses. When set, each chunk is
//! read from this node's own slot through [`Effects::host_get_chunk`], from
//! [`ImgInfo::LEN`] onward. When clear, `s_host_req_update_entry` creates a
//! stream buffer of `chunk_size` bytes, the application feeds it with
//! [`Host::queue_data`], and each chunk is read out of it. [`StreamBuffer`]
//! holds it inline, at most [`DFU_MAX_CHUNK_SIZE`] bytes, with
//! `csrc/bm_os_shim.c`'s semantics: a send that does not fit is refused
//! whole, and a read of an empty buffer fails.
//!
//! # Time
//!
//! The C's reads block: up to 5 s for `bm_dfu_host_get_chunk`, up to the
//! update's `timeoutMs` for the stream. `heartbeat_timer` is started before
//! the read and stopped after the chunk is sent, so a `0xD6` goes to the
//! client each second a read blocks. A sans-io read does not block, so the
//! timer could never fire and is not ported: the application must have fed
//! the stream before the client asks for the chunk.
//!
//! # Quirks reproduced
//!
//! * A chunk request's `seq_num` is not read. The host serves the next
//!   `chunk_size` bytes after the last chunk it sent, so a retried request
//!   gets the following chunk, and past the end it sends empty chunks
//!   (divergence #67).
//! * A failed ACK's or an abort's `err_code` becomes this node's error, and
//!   14 or more is fatal (divergence #59).
//! * A read of the stream that finds too few bytes still sends a whole
//!   chunk. The C sends uninitialised heap past what was read; here it is
//!   zeros (divergence #68).
//! * Leaving `HostReqUpdate` other than into `HostUpdate` keeps the stream
//!   buffer; the next non-internal update replaces it. The C leaks it
//!   (divergence #61).
//!
//! # Where the C is undefined
//!
//! * A body shorter than the fields a state reads is read past its end
//!   (divergence #55). Here an ACK, end or abort too short for its
//!   `success` and `err_code` bytes is taken as if they were absent.
//! * A `timeoutMs` of zero is a zero timer period, which FreeRTOS refuses
//!   (divergence #69). Here the update timer is due at once.

use crate::bcmp::dfu::{DFU_MAX_CHUNK_SIZE, DfuAddress, DfuChunk, DfuMessage, DfuStart, ImgInfo};
use crate::bcmp::dfu_client::Client;
use crate::bcmp::dfu_core::{Core, DfuErr, Effects, EventData, EventType, Roles, State, Timer};

/// `bm_dfu_max_ack_retries`: `0xD0`s sent before an unanswered update fails.
pub const MAX_ACK_RETRIES: u8 = 2;

/// Offset of `success` in a `0xD3`, `0xD4` or `0xD5` body.
const SUCCESS_OFFSET: usize = DfuMessage::MIN_LEN;
/// Offset of `err_code` in the same.
const ERR_CODE_OFFSET: usize = DfuMessage::MIN_LEN + 1;

/// `bm_stream_buffer_*` over inline storage, with the shim's semantics.
#[derive(Clone)]
pub struct StreamBuffer {
    storage: [u8; DFU_MAX_CHUNK_SIZE],
    capacity: usize,
    head: usize,
    count: usize,
}

impl core::fmt::Debug for StreamBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StreamBuffer")
            .field("capacity", &self.capacity)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for StreamBuffer {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(
            f,
            "StreamBuffer {{ capacity: {=usize}, count: {=usize}, .. }}",
            self.capacity,
            self.count
        );
    }
}

impl StreamBuffer {
    /// `bm_stream_buffer_create(capacity)`: `None` for zero, as the shim
    /// returns `NULL`. `s_host_req_update_entry` passes a `chunk_size`, which
    /// `initiate_update` has bounded by [`DFU_MAX_CHUNK_SIZE`].
    #[must_use]
    pub fn new(capacity: usize) -> Option<Self> {
        (1..=DFU_MAX_CHUNK_SIZE)
            .contains(&capacity)
            .then_some(Self {
                storage: [0; DFU_MAX_CHUNK_SIZE],
                capacity,
                head: 0,
                count: 0,
            })
    }

    /// Bytes waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Nothing waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// `bm_stream_buffer_send`: append all of `data`, or nothing if it does
    /// not fit.
    pub fn send(&mut self, data: &[u8]) -> bool {
        if self.count + data.len() > self.capacity {
            return false;
        }
        for (i, byte) in data.iter().enumerate() {
            self.storage[(self.head + self.count + i) % self.capacity] = *byte;
        }
        self.count += data.len();
        true
    }

    /// `bm_stream_buffer_receive`: move up to `buf.len()` bytes into `buf`.
    /// `None` if empty, which is the shim's `BmETIMEDOUT`.
    pub fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let n = self.count.min(buf.len());
        for (i, byte) in buf[..n].iter_mut().enumerate() {
            *byte = self.storage[(self.head + i) % self.capacity];
        }
        self.head = (self.head + n) % self.capacity;
        self.count -= n;
        Some(n)
    }
}

/// `dfu_host_ctx_t`, less the queue and timers, which are the core's.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Host {
    img_info: ImgInfo,
    client_node_id: u64,
    bytes_remaining: u32,
    ack_retry_num: u8,
    notify: bool,
    timeout_ms: u32,
    stream: Option<StreamBuffer>,
}

impl Host {
    /// Zeroed, as `host_ctx` is at boot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            img_info: ImgInfo {
                image_size: 0,
                chunk_size: 0,
                crc16: 0,
                major_ver: 0,
                minor_ver: 0,
                filter_key: 0,
                git_sha: 0,
            },
            client_node_id: 0,
            bytes_remaining: 0,
            ack_retry_num: 0,
            notify: false,
            timeout_ms: 0,
            stream: None,
        }
    }

    /// The client of the current or last update.
    #[must_use]
    pub fn client_node_id(&self) -> u64 {
        self.client_node_id
    }

    /// The image of the current or last update.
    #[must_use]
    pub fn img_info(&self) -> ImgInfo {
        self.img_info
    }

    /// Image bytes not yet sent.
    #[must_use]
    pub fn bytes_remaining(&self) -> u32 {
        self.bytes_remaining
    }

    /// Unanswered `0xD0`s since the update began.
    #[must_use]
    pub fn ack_retries(&self) -> u8 {
        self.ack_retry_num
    }

    /// The update's `timeoutMs`.
    #[must_use]
    pub fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }

    /// The stream buffer of a non-internal update, if one is held.
    #[must_use]
    pub fn stream(&self) -> Option<&StreamBuffer> {
        self.stream.as_ref()
    }

    /// The length of the next chunk: `bytes_remaining`, at most
    /// `chunk_size`.
    #[must_use]
    pub fn next_chunk_len(&self) -> u32 {
        self.bytes_remaining
            .min(u32::from(self.img_info.chunk_size))
    }

    /// `bm_dfu_host_set_params`: whether the update has a finish callback,
    /// and its `timeoutMs`.
    pub fn set_params(&mut self, notify: bool, timeout_ms: u32) {
        self.notify = notify;
        self.timeout_ms = timeout_ms;
    }

    /// `bm_dfu_host_client_node_valid`.
    #[must_use]
    pub fn client_node_valid(&self, node_id: u64) -> bool {
        self.client_node_id == node_id
    }

    /// `bm_dfu_host_queue_data`: feed a non-internal update's image. `false`
    /// if no stream buffer is held or `data` does not fit.
    pub fn queue_data(&mut self, data: &[u8]) -> bool {
        self.stream.as_mut().is_some_and(|s| s.send(data))
    }

    /// `on_state_entry` for a host state.
    pub fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        match state {
            State::HostReqUpdate => self.req_update_entry(core, fx),
            // s_host_update_entry, through bm_dfu_host_start_update_timer.
            State::HostUpdate => core.change_period(Timer::Update, self.timeout_ms),
            _ => {}
        }
    }

    /// `run` for a host state.
    pub fn run(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        match state {
            State::HostReqUpdate => self.req_update_run(core, fx),
            State::HostUpdate => self.update_run(core, fx),
            _ => {}
        }
    }

    /// `on_state_exit` for a host state: `s_host_update_exit` deletes the
    /// stream buffer.
    pub fn exit(&mut self, state: State) {
        if state == State::HostUpdate {
            self.stream = None;
        }
    }

    fn to_client(&self, core: &Core) -> DfuAddress {
        DfuAddress {
            src_node_id: core.self_node_id(),
            dst_node_id: self.client_node_id,
        }
    }

    /// `bm_dfu_host_transition_to_error`.
    fn transition_to_error(core: &mut Core, err: DfuErr) {
        core.stop_timer(Timer::Update);
        core.stop_timer(Timer::Ack);
        core.set_error(err);
        core.set_pending_state_change(State::Error);
    }

    /// `bm_dfu_host_req_update`.
    fn req_update(&self, core: &Core, fx: &mut dyn Effects) {
        fx.send(&DfuMessage::Start(DfuStart {
            addresses: self.to_client(core),
            img_info: self.img_info,
        }));
    }

    /// `s_host_req_update_entry`.
    fn req_update_entry(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        // The C reads a `BmDfuEventImgInfo` one byte into whatever buffer the
        // current event carries.
        let (addresses, img_info) = match &core.current_event().data {
            EventData::None => return,
            EventData::HostStart(start) => (start.start.addresses, start.start.img_info),
            EventData::Message { len, bytes } => {
                let body = &bytes[..*len];
                let (Ok(addresses), Some(Ok(info))) = (
                    DfuAddress::of_body(body),
                    body.get(DfuMessage::MIN_LEN..).map(ImgInfo::decode),
                ) else {
                    return;
                };
                (addresses, info)
            }
        };
        self.img_info = img_info;
        self.bytes_remaining = img_info.image_size;
        self.client_node_id = addresses.dst_node_id;
        self.ack_retry_num = 0;
        self.req_update(core, fx);
        if !core.internal() {
            // Replaces any buffer an earlier update left; the C leaks it
            // (divergence #61).
            self.stream = StreamBuffer::new(usize::from(img_info.chunk_size));
        }
        core.start_timer(Timer::Ack);
    }

    /// The `success` and `err_code` bytes of the current event's body.
    fn result(core: &Core) -> Option<(u8, u8)> {
        let body = core.current_event().body()?;
        Some((*body.get(SUCCESS_OFFSET)?, *body.get(ERR_CODE_OFFSET)?))
    }

    /// An abort's error: the body's `err_code`, or `BmDfuErrAborted` for the
    /// update timer's bare event.
    fn abort_error(core: &Core) -> Option<DfuErr> {
        match core.current_event().body() {
            None => Some(DfuErr::ABORTED),
            Some(body) => body.get(ERR_CODE_OFFSET).map(|e| DfuErr(*e)),
        }
    }

    /// `s_host_req_update_run`.
    fn req_update_run(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        match core.current_event().kind {
            EventType::AckReceived => {
                core.stop_timer(Timer::Ack);
                if let Some((success, err_code)) = Self::result(core) {
                    if success != 0 {
                        core.set_pending_state_change(State::HostUpdate);
                    } else {
                        Self::transition_to_error(core, DfuErr(err_code));
                    }
                }
            }
            EventType::AckTimeout => {
                self.ack_retry_num = self.ack_retry_num.wrapping_add(1);
                if self.ack_retry_num >= MAX_ACK_RETRIES {
                    Self::transition_to_error(core, DfuErr::TIMEOUT);
                } else {
                    self.req_update(core, fx);
                    core.start_timer(Timer::Ack);
                }
            }
            EventType::Abort => {
                if let Some(err) = Self::abort_error(core) {
                    Self::transition_to_error(core, err);
                }
            }
            _ => {}
        }
    }

    /// `s_host_update_run`.
    fn update_run(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        let has_body = core.current_event().body().is_some();
        match core.current_event().kind {
            // The heartbeat timer's start and stop around the send are not
            // ported; see the module docs.
            EventType::ChunkRequest if has_body => self.send_chunk(core, fx),
            EventType::RebootRequest => fx.send(&DfuMessage::Reboot(self.to_client(core))),
            EventType::BootComplete if has_body => {
                core.update_end(fx, self.client_node_id, 1, DfuErr::NONE);
            }
            EventType::UpdateEnd => {
                core.stop_timer(Timer::Update);
                if let Some((success, err_code)) = Self::result(core) {
                    if self.notify {
                        fx.update_finished(success != 0, DfuErr(err_code), self.client_node_id);
                    }
                    core.set_pending_state_change(State::Idle);
                }
            }
            EventType::Abort => {
                if let Some(err) = Self::abort_error(core) {
                    Self::transition_to_error(core, err);
                }
            }
            _ => {}
        }
    }

    /// `bm_dfu_host_send_chunk`: the next chunk in sequence, whatever was
    /// asked for (divergence #67).
    fn send_chunk(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        // `next_chunk_len` is at most `chunk_size`, which `initiate_update`
        // bounds by `DFU_MAX_CHUNK_SIZE`; a `0xD0` entry does not, so clamp.
        let len = (self.next_chunk_len() as usize).min(DFU_MAX_CHUNK_SIZE);
        let mut buf = [0u8; DFU_MAX_CHUNK_SIZE];
        let read = if core.internal() {
            let offset = (ImgInfo::LEN as u32)
                .wrapping_add(self.img_info.image_size)
                .wrapping_sub(self.bytes_remaining);
            fx.host_get_chunk(offset, &mut buf[..len]).then_some(len)
        } else {
            // No buffer is the C's untouched `BmENOMEM`.
            self.stream
                .as_mut()
                .and_then(|s| s.receive(&mut buf[..len]))
        };
        let Some(read) = read else {
            Self::transition_to_error(core, DfuErr::FLASH_ACCESS);
            return;
        };
        // A short read still declares and sends `len` bytes (divergence #68).
        fx.send(&DfuMessage::Payload(DfuChunk {
            addresses: self.to_client(core),
            payload: &buf[..len],
        }));
        // `read` is at most `len`, which is at most `bytes_remaining`.
        self.bytes_remaining -= read as u32;
    }
}

/// A node that is both client and host, dispatching on the state as
/// `dfu_states` does.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ClientHost {
    /// `dfu_client.c`.
    pub client: Client,
    /// `dfu_host.c`.
    pub host: Host,
}

impl ClientHost {
    /// Both zeroed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            client: Client::new(),
            host: Host::new(),
        }
    }
}

impl Roles for ClientHost {
    fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        if state.is_client() {
            self.client.entry(state, core, fx);
        } else {
            self.host.entry(state, core, fx);
        }
    }

    fn run(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        if state.is_client() {
            self.client.run(state, core, fx);
        } else {
            self.host.run(state, core, fx);
        }
    }

    fn exit(&mut self, state: State, _core: &mut Core, _fx: &mut dyn Effects) {
        self.host.exit(state);
    }

    fn client_process_update_request(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        self.client.process_update_request(core, fx);
    }

    fn host_set_params(&mut self, notify: bool, timeout_ms: u32) {
        self.host.set_params(notify, timeout_ms);
    }

    fn client_host_node_valid(&self, node_id: u64) -> bool {
        self.client.host_node_valid(node_id)
    }

    fn host_client_node_valid(&self, node_id: u64) -> bool {
        self.host.client_node_valid(node_id)
    }
}

#[cfg(test)]
mod tests;
