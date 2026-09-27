//! The DFU client, ported sans-io from `bcmp/dfu_client.c`.
//!
//! [`Client`] is `DfuClientCtx` and the five `s_client_*` states. It runs
//! inside [`Dfu`](crate::bcmp::dfu_core::Dfu), which calls it through
//! [`Roles`]; everything it does outside itself — flash, the boot hooks, the
//! config store, `git_sha` — goes through [`Effects`], and its timer and
//! delays through [`Core`].
//!
//! | C | Here |
//! |---|---|
//! | `bm_dfu_client_process_update_request` | [`Client::process_update_request`] |
//! | `s_client_*_entry`, `s_client_*_run` | [`Client::entry`], [`Client::run`] |
//! | `bm_dfu_process_payload`, `bm_dfu_process_end` | private |
//! | `chunk_timer`, `chunk_timer_handler` | [`Timer::Chunk`] |
//! | `bm_dfu_client_host_node_valid` | [`Client::host_node_valid`] |
//! | `bm_dfu_client_confirm_is_enabled`, `_confirm_enable` | the `dfu_confirm` key through [`Effects::config`] |
//!
//! # Quirks reproduced
//!
//! * A `0xD0` in `ClientReceiving` restarts the transfer from chunk zero
//!   against the image of the *first* `0xD0`: size, chunk count and CRC are
//!   not re-read, and the slot is not re-erased (divergence #62).
//! * A failed flash write sets the error and still requests the next chunk and
//!   re-arms the timer; on the last chunk it also finishes the image, and the
//!   change to `ClientValidating` replaces the change to `Error`
//!   (divergence #63).
//! * A refused image (`BmDfuErrTooLarge`) leaves the slot open
//!   (divergence #64).
//! * The host's `0xD3` confirms an update whatever its `success` byte says
//!   (divergence #65).
//! * `num_chunks` and `current_chunk` are `uint16_t`, so an image of 65 536
//!   chunks or more wraps the count (divergence #66).
//! * A chunk's `payload_length` is checked against
//!   [`DFU_MAX_CHUNK_SIZE`], not against the offered `chunk_size`.
//!
//! # Where the C is undefined
//!
//! * `chunk_size` zero divides by zero (divergence #57). Here it gives what a
//!   Cortex-M `UDIV` gives with `DIV_0_TRP` clear: quotient zero, so one
//!   chunk for a non-empty image and none for an empty one.
//! * A body shorter than the fields a state reads is read past its end
//!   (divergence #55). Here a short `0xD0` is ignored, and a short chunk is
//!   ignored as an oversized one is.

use crate::bcmp::dfu::{
    DFU_MAX_CHUNK_SIZE, DfuAddress, DfuMessage, DfuResult, IMG_INFO_FORCE_UPDATE, ImgInfo,
};
use crate::bcmp::dfu_core::{
    Core, DFU_REBOOT_MAGIC, DfuErr, Effects, EventType, RebootInfo, Roles, State, Timer,
};
use crate::configuration::{Key, Partition};
use crate::crc::crc16_ccitt;

/// `bm_img_page_length`: bytes gathered before each flash write.
pub const IMG_PAGE_LEN: usize = 2048;

/// `bm_dfu_max_chunk_retries`: chunk, reboot-request and boot-complete
/// timeouts tolerated before giving up.
pub const MAX_CHUNK_RETRIES: u8 = 5;

/// `dfu_confirm_config_key`, in the system partition. Anything but 1 makes a
/// rebooted client confirm its new image without asking the host.
pub const DFU_CONFIRM_KEY: &[u8] = b"dfu_confirm";

/// Offset of a chunk's `payload_length` in a `0xD2` body.
const PAYLOAD_LEN_OFFSET: usize = DfuMessage::MIN_LEN;
/// Offset of a chunk's first byte.
const PAYLOAD_OFFSET: usize = DfuMessage::WITH_TWO_BYTES_LEN;

/// `DfuClientCtx`, less the queue, timer and flash handle, which are the
/// core's and the integrator's.
#[derive(Clone)]
pub struct Client {
    image_size: u32,
    num_chunks: u16,
    crc16: u16,
    running_crc16: u16,
    page_byte_counter: u16,
    flash_offset: u32,
    page_buf: [u8; IMG_PAGE_LEN],
    chunk_retry_num: u8,
    current_chunk: u16,
    host_node_id: u64,
}

impl core::fmt::Debug for Client {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Client")
            .field("image_size", &self.image_size)
            .field("num_chunks", &self.num_chunks)
            .field("crc16", &self.crc16)
            .field("running_crc16", &self.running_crc16)
            .field("page_byte_counter", &self.page_byte_counter)
            .field("flash_offset", &self.flash_offset)
            .field("chunk_retry_num", &self.chunk_retry_num)
            .field("current_chunk", &self.current_chunk)
            .field("host_node_id", &self.host_node_id)
            .finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// Zeroed, as `CLIENT_CTX` is at boot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            image_size: 0,
            num_chunks: 0,
            crc16: 0,
            running_crc16: 0,
            page_byte_counter: 0,
            flash_offset: 0,
            page_buf: [0; IMG_PAGE_LEN],
            chunk_retry_num: 0,
            current_chunk: 0,
            host_node_id: 0,
        }
    }

    /// The host of the current or last update.
    #[must_use]
    pub fn host_node_id(&self) -> u64 {
        self.host_node_id
    }

    /// The image size the last accepted `0xD0` offered.
    #[must_use]
    pub fn image_size(&self) -> u32 {
        self.image_size
    }

    /// Chunks the client will ask for.
    #[must_use]
    pub fn num_chunks(&self) -> u16 {
        self.num_chunks
    }

    /// The chunk being asked for.
    #[must_use]
    pub fn current_chunk(&self) -> u16 {
        self.current_chunk
    }

    /// Timeouts since the last chunk or state entry.
    #[must_use]
    pub fn retries(&self) -> u8 {
        self.chunk_retry_num
    }

    /// `crc16_ccitt` over every chunk received so far.
    #[must_use]
    pub fn running_crc16(&self) -> u16 {
        self.running_crc16
    }

    /// Bytes written to the slot so far.
    #[must_use]
    pub fn flash_offset(&self) -> u32 {
        self.flash_offset
    }

    /// `bm_dfu_client_host_node_valid`.
    #[must_use]
    pub fn host_node_valid(&self, node_id: u64) -> bool {
        self.host_node_id == node_id
    }

    /// `bm_dfu_client_process_update_request`: a `0xD0` in `Idle`. Accepts,
    /// NACKs or aborts.
    pub fn process_update_request(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        let Some(body) = core.current_event().body() else {
            return;
        };
        // The C reads the address and image info whatever the type byte.
        let (Ok(addresses), Some(Ok(info))) = (
            DfuAddress::of_body(body),
            body.get(DfuMessage::MIN_LEN..).map(ImgInfo::decode),
        ) else {
            return;
        };
        self.host_node_id = addresses.src_node_id;

        if info.git_sha == fx.git_sha() && info.filter_key != IMG_INFO_FORCE_UPDATE {
            core.send_ack(fx, self.host_node_id, 0, DfuErr::SAME_VER);
            return;
        }
        if usize::from(info.chunk_size) > DFU_MAX_CHUNK_SIZE {
            self.abort(core, fx, DfuErr::ABORTED);
            self.transition_to_error(core, DfuErr::CHUNK_SIZE);
            return;
        }
        self.image_size = info.image_size;
        let chunk_size = u32::from(info.chunk_size);
        // Divergence #57: Cortex-M `UDIV` by zero, quotient 0.
        let whole = info.image_size.checked_div(chunk_size).unwrap_or(0);
        let rem = info
            .image_size
            .checked_rem(chunk_size)
            .unwrap_or(info.image_size);
        // `uint16_t num_chunks`.
        self.num_chunks = if rem != 0 { whole + 1 } else { whole } as u16;
        self.crc16 = info.crc16;

        if !fx.flash_open() {
            core.send_ack(fx, self.host_node_id, 0, DfuErr::FLASH_ACCESS);
            self.transition_to_error(core, DfuErr::FLASH_ACCESS);
            return;
        }
        if fx.flash_size() <= info.image_size {
            // The slot stays open.
            core.send_ack(fx, self.host_node_id, 0, DfuErr::TOO_LARGE);
            return;
        }
        let size = fx.flash_size();
        if !fx.flash_erase(0, size) {
            core.send_ack(fx, self.host_node_id, 0, DfuErr::FLASH_ACCESS);
            self.transition_to_error(core, DfuErr::FLASH_ACCESS);
            return;
        }
        core.send_ack(fx, self.host_node_id, 1, DfuErr::NONE);
        *core.reboot_info_mut() = RebootInfo {
            magic: DFU_REBOOT_MAGIC,
            major: info.major_ver,
            minor: info.minor_ver,
            host_node_id: self.host_node_id,
            git_sha: info.git_sha,
        };
        core.delay(10);
        core.set_pending_state_change(State::ClientReceiving);
    }

    /// `on_state_entry` for a client state. Other states have none here.
    pub fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        match state {
            State::ClientReceiving => self.receiving_entry(core, fx),
            State::ClientValidating => self.validating_entry(core, fx),
            State::ClientRebootReq => self.reboot_req_entry(core, fx),
            State::ClientRebootDone => self.update_done_entry(core, fx),
            State::ClientActivating => {
                // s_client_activating_entry: "so DFU_END can get out" first.
                core.delay(10);
                fx.set_pending_and_reset();
            }
            _ => {}
        }
    }

    /// `run` for a client state. `ClientValidating` and `ClientActivating`
    /// do nothing.
    pub fn run(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        match state {
            State::ClientReceiving => self.receiving_run(core, fx),
            State::ClientRebootReq => self.reboot_req_run(core, fx),
            State::ClientRebootDone => self.update_done_run(core, fx),
            _ => {}
        }
    }

    /// `bm_dfu_client_abort`.
    fn abort(&self, core: &Core, fx: &mut dyn Effects, err: DfuErr) {
        fx.send(&DfuMessage::Abort(DfuResult {
            addresses: self.to_host(core, self.host_node_id),
            success: 0,
            err_code: err.0,
        }));
    }

    fn to_host(&self, core: &Core, host_node_id: u64) -> DfuAddress {
        DfuAddress {
            src_node_id: core.self_node_id(),
            dst_node_id: host_node_id,
        }
    }

    /// `bm_dfu_client_transition_to_error`.
    fn transition_to_error(&self, core: &mut Core, err: DfuErr) {
        core.stop_timer(Timer::Chunk);
        core.set_error(err);
        core.set_pending_state_change(State::Error);
    }

    /// `bm_dfu_client_fail_update_and_reboot`.
    fn fail_update_and_reboot(core: &mut Core, fx: &mut dyn Effects) {
        *core.reboot_info_mut() = RebootInfo::default();
        core.delay(100);
        fx.fail_update_and_reset();
    }

    fn restart_transfer(&mut self) {
        self.current_chunk = 0;
        self.chunk_retry_num = 0;
        self.page_byte_counter = 0;
        self.flash_offset = 0;
        self.running_crc16 = 0;
    }

    /// `s_client_receiving_entry`.
    fn receiving_entry(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        self.restart_transfer();
        core.req_next_chunk(fx, self.host_node_id, self.current_chunk);
        core.start_timer(Timer::Chunk);
    }

    /// `s_client_receiving_run`.
    fn receiving_run(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        match core.current_event().kind {
            EventType::ImageChunk => {
                let Some(body) = core.current_event().body() else {
                    return;
                };
                let Some(len) = body
                    .get(PAYLOAD_LEN_OFFSET..PAYLOAD_OFFSET)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                else {
                    return;
                };
                if usize::from(len) > DFU_MAX_CHUNK_SIZE {
                    // Ignored; the timer keeps running.
                    return;
                }
                let Some(received) = body.get(PAYLOAD_OFFSET..PAYLOAD_OFFSET + usize::from(len))
                else {
                    return;
                };
                // Copied out of the event so the core can be borrowed again.
                let mut buf = [0u8; DFU_MAX_CHUNK_SIZE];
                let chunk = &mut buf[..received.len()];
                chunk.copy_from_slice(received);

                core.stop_timer(Timer::Chunk);
                self.running_crc16 = crc16_ccitt(self.running_crc16, chunk);
                if !self.process_payload(fx, chunk) {
                    // Divergence #63: and carry on as if it had worked.
                    self.transition_to_error(core, DfuErr::BM_FRAME);
                }
                self.current_chunk = self.current_chunk.wrapping_add(1);
                self.chunk_retry_num = 0;
                if self.current_chunk < self.num_chunks {
                    core.req_next_chunk(fx, self.host_node_id, self.current_chunk);
                    core.start_timer(Timer::Chunk);
                } else if self.process_end(fx) {
                    core.set_pending_state_change(State::ClientValidating);
                } else {
                    self.transition_to_error(core, DfuErr::BM_FRAME);
                }
            }
            EventType::ChunkTimeout => {
                self.chunk_retry_num = self.chunk_retry_num.wrapping_add(1);
                if self.chunk_retry_num >= MAX_CHUNK_RETRIES {
                    self.abort(core, fx, DfuErr::ABORTED);
                    self.transition_to_error(core, DfuErr::TIMEOUT);
                } else {
                    core.req_next_chunk(fx, self.host_node_id, self.current_chunk);
                    core.start_timer(Timer::Chunk);
                }
            }
            EventType::ReceivedUpdateRequest => {
                // "The host dropped our previous ack": divergence #62.
                core.stop_timer(Timer::Chunk);
                core.send_ack(fx, self.host_node_id, 1, DfuErr::NONE);
                self.restart_transfer();
                core.delay(100);
                core.req_next_chunk(fx, self.host_node_id, self.current_chunk);
                core.start_timer(Timer::Chunk);
            }
            EventType::Heartbeat => core.start_timer(Timer::Chunk),
            _ => {}
        }
    }

    /// `bm_dfu_process_payload`: gather `chunk` into the page buffer, writing
    /// the page when it fills. `false` on an empty chunk or a failed write,
    /// which loses the bytes that would have started the next page.
    fn process_payload(&mut self, fx: &mut dyn Effects, chunk: &[u8]) -> bool {
        if chunk.is_empty() {
            return false;
        }
        let counter = usize::from(self.page_byte_counter);
        if counter + chunk.len() < IMG_PAGE_LEN {
            self.page_buf[counter..counter + chunk.len()].copy_from_slice(chunk);
            // `len + counter` is under a page, so this fits.
            self.page_byte_counter += chunk.len() as u16;
            return true;
        }
        let (head, tail) = chunk.split_at(IMG_PAGE_LEN - counter);
        self.page_buf[counter..].copy_from_slice(head);
        self.page_byte_counter = 0;
        if !fx.flash_write(self.flash_offset, &self.page_buf) {
            return false;
        }
        self.flash_offset = self.flash_offset.wrapping_add(IMG_PAGE_LEN as u32);
        self.page_buf[..tail.len()].copy_from_slice(tail);
        // A chunk is at most 1024 bytes, so the tail is too.
        self.page_byte_counter = tail.len() as u16;
        true
    }

    /// `bm_dfu_process_end`: write the partial page, then close the slot
    /// whatever happened.
    fn process_end(&mut self, fx: &mut dyn Effects) -> bool {
        let mut ok = true;
        if self.page_byte_counter != 0 {
            let len = usize::from(self.page_byte_counter);
            if fx.flash_write(self.flash_offset, &self.page_buf[..len]) {
                self.flash_offset = self
                    .flash_offset
                    .wrapping_add(u32::from(self.page_byte_counter));
            } else {
                ok = false;
            }
        }
        let _ = fx.flash_close();
        ok
    }

    /// `s_client_validating_entry`.
    fn validating_entry(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        if self.image_size != self.flash_offset {
            core.update_end(fx, self.host_node_id, 0, DfuErr::MISMATCH_LEN);
            self.transition_to_error(core, DfuErr::MISMATCH_LEN);
        } else if self.crc16 == self.running_crc16 {
            core.set_pending_state_change(State::ClientRebootReq);
        } else {
            core.update_end(fx, self.host_node_id, 0, DfuErr::BAD_CRC);
            self.transition_to_error(core, DfuErr::BAD_CRC);
        }
    }

    /// `s_client_reboot_req_entry`.
    fn reboot_req_entry(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        self.chunk_retry_num = 0;
        fx.send(&DfuMessage::RebootReq(
            self.to_host(core, self.host_node_id),
        ));
        core.start_timer(Timer::Chunk);
    }

    /// `s_client_reboot_req_run`.
    fn reboot_req_run(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        match core.current_event().kind {
            EventType::Reboot => {
                if core.current_event().body().is_some() {
                    core.stop_timer(Timer::Chunk);
                    core.set_pending_state_change(State::ClientActivating);
                }
            }
            EventType::ChunkTimeout => {
                self.chunk_retry_num = self.chunk_retry_num.wrapping_add(1);
                if self.chunk_retry_num >= MAX_CHUNK_RETRIES {
                    self.abort(core, fx, DfuErr::ABORTED);
                    self.transition_to_error(core, DfuErr::TIMEOUT);
                } else {
                    fx.send(&DfuMessage::RebootReq(
                        self.to_host(core, self.host_node_id),
                    ));
                    core.start_timer(Timer::Chunk);
                }
            }
            _ => {}
        }
    }

    /// `bm_dfu_client_confirm_is_enabled`: `dfu_confirm` is 1, or unreadable.
    fn confirm_is_enabled(fx: &mut dyn Effects) -> bool {
        fx.config()
            .and_then(|store| {
                store
                    .partition(Partition::System)
                    .get_uint(Key::new(DFU_CONFIRM_KEY))
            })
            .unwrap_or(1)
            == 1
    }

    /// `bm_dfu_client_confirm_enable`: set `dfu_confirm`, then commit, which
    /// resets.
    fn confirm_enable(fx: &mut dyn Effects, enable: bool) {
        if let Some(store) = fx.config() {
            let _ = store
                .partition_mut(Partition::System)
                .set_uint(Key::new(DFU_CONFIRM_KEY), u32::from(enable));
        }
        let _ = fx.commit_config(Partition::System);
    }

    /// `s_client_update_done_entry`: rebooted into the new image.
    fn update_done_entry(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        let info = *core.reboot_info();
        self.host_node_id = info.host_node_id;
        self.chunk_retry_num = 0;
        if fx.git_sha() != info.git_sha {
            core.update_end(fx, info.host_node_id, 0, DfuErr::WRONG_VER);
            Self::fail_update_and_reboot(core, fx);
        } else if Self::confirm_is_enabled(fx) {
            fx.send(&DfuMessage::BootComplete(
                self.to_host(core, info.host_node_id),
            ));
            core.start_timer(Timer::Chunk);
        } else {
            // Confirm without the host, and re-arm confirmation for next time.
            *core.reboot_info_mut() = RebootInfo::default();
            fx.set_confirmed();
            Self::confirm_enable(fx, true);
        }
    }

    /// `s_client_update_done_run`.
    fn update_done_run(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        match core.current_event().kind {
            EventType::UpdateEnd => {
                if core.current_event().body().is_some() {
                    core.stop_timer(Timer::Chunk);
                    fx.set_confirmed();
                    let host = core.reboot_info().host_node_id;
                    core.update_end(fx, host, 1, DfuErr::NONE);
                    core.set_pending_state_change(State::Idle);
                }
            }
            EventType::ChunkTimeout => {
                self.chunk_retry_num = self.chunk_retry_num.wrapping_add(1);
                if self.chunk_retry_num >= MAX_CHUNK_RETRIES {
                    self.abort(core, fx, DfuErr::CONFIRMATION_ABORT);
                    Self::fail_update_and_reboot(core, fx);
                } else {
                    let host = core.reboot_info().host_node_id;
                    fx.send(&DfuMessage::BootComplete(self.to_host(core, host)));
                    core.start_timer(Timer::Chunk);
                }
            }
            _ => {}
        }
    }
}

/// A node that is a client only. The host states are reached only through
/// [`Dfu::initiate_update`](crate::bcmp::dfu_core::Dfu::initiate_update),
/// which such a node does not call; if entered, they do nothing and accept
/// nothing.
impl Roles for Client {
    fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        Client::entry(self, state, core, fx);
    }

    fn run(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        Client::run(self, state, core, fx);
    }

    fn exit(&mut self, _state: State, _core: &mut Core, _fx: &mut dyn Effects) {}

    fn client_process_update_request(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        self.process_update_request(core, fx);
    }

    fn host_set_params(&mut self, _notify: bool, _timeout_ms: u32) {}

    fn client_host_node_valid(&self, node_id: u64) -> bool {
        self.host_node_valid(node_id)
    }

    fn host_client_node_valid(&self, _node_id: u64) -> bool {
        false
    }
}

#[cfg(test)]
mod tests;
