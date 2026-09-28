//! DFU on a node: [`bm_wire::bcmp::dfu_core::Dfu`] over the client of
//! [`bm_wire::bcmp::dfu_client`], with the node's seams behind it.
//!
//! bm_core runs DFU on its own task. Here [`NodeDfu`] holds the machine, the
//! bodies it has sent but the node has not yet framed, and a reset it has
//! asked for; [`crate::Node::next_dfu_transmission`] steps it and hands the
//! frames out one at a time.
//!
//! Two things are ordered differently from the C, neither visible on the
//! wire:
//!
//! * `bm_delay` does not wait. The machine's clock moves on by the delay
//!   (see [`bm_wire::bcmp::dfu_core`]), so its timers fire when the C's
//!   would; the frames it covers go out as soon as the node is next polled.
//! * [`DfuSlot::set_pending_and_reset`] and
//!   [`DfuSlot::fail_update_and_reset`] are called once every frame sent
//!   before them is out, and after the reboot info has been stored. The C
//!   calls them in place, after a `bm_delay` meant to let those frames go.

use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::dfu::DfuMessage;
use bm_wire::bcmp::dfu_client::Client;
use bm_wire::bcmp::dfu_core::{Accepted, Dfu, DfuErr, Effects, RebootInfo};
use bm_wire::configuration::{ConfigStore, Partition};

use crate::config::Configuration;
use crate::port::{DfuSlot, NoInitRam};

/// Bodies the machine may send in one step and the node has yet to frame.
/// The client sends at most two per step.
pub const OUTBOX_LEN: usize = 4;

/// The longest body the client or the core sends, a `0xD0`.
const OUTBOX_BODY: usize = DfuMessage::START_LEN;

/// A reset the client asked for, made once its frames are out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reset {
    /// [`DfuSlot::set_pending_and_reset`].
    Pending,
    /// [`DfuSlot::fail_update_and_reset`].
    Fail,
}

/// Sent bodies, oldest first. A send that finds it full is dropped, as a
/// failed `bcmp_tx` is only logged.
#[derive(Debug, Clone)]
struct Outbox {
    bodies: [(MessageType, usize, [u8; OUTBOX_BODY]); OUTBOX_LEN],
    head: usize,
    len: usize,
}

impl Outbox {
    const fn new() -> Self {
        Self {
            bodies: [(MessageType(0), 0, [0; OUTBOX_BODY]); OUTBOX_LEN],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, message: &DfuMessage<'_>) {
        if self.len == OUTBOX_LEN {
            return;
        }
        let slot = &mut self.bodies[(self.head + self.len) % OUTBOX_LEN];
        let Ok(n) = message.encode(&mut slot.2) else {
            return;
        };
        slot.0 = message.message_type();
        slot.1 = n;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<(MessageType, usize, [u8; OUTBOX_BODY])> {
        if self.len == 0 {
            return None;
        }
        let body = self.bodies[self.head];
        self.head = (self.head + 1) % OUTBOX_LEN;
        self.len -= 1;
        Some(body)
    }
}

/// The node's DFU state: `dfu_ctx`, `CLIENT_CTX` and what is waiting to go
/// out.
pub struct NodeDfu<D> {
    machine: Dfu<Client>,
    slot: D,
    outbox: Outbox,
    reset: Option<Reset>,
    stored: RebootInfo,
}

impl<D> core::fmt::Debug for NodeDfu<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NodeDfu")
            .field("state", &self.machine.state())
            .field("reset", &self.reset)
            .finish_non_exhaustive()
    }
}

impl<D: DfuSlot + NoInitRam> NodeDfu<D> {
    /// `bm_dfu_init`, with the reboot info `slot` kept across the last reset.
    pub fn new(self_node_id: u64, mut slot: D) -> Self {
        let stored = slot.load();
        Self {
            machine: Dfu::new(self_node_id, stored, Client::new()),
            slot,
            outbox: Outbox::new(),
            reset: None,
            stored,
        }
    }

    /// The state machine.
    pub fn machine(&self) -> &Dfu<Client> {
        &self.machine
    }

    /// The slot and no-init RAM.
    pub fn slot(&self) -> &D {
        &self.slot
    }

    /// The same, mutably.
    pub fn slot_mut(&mut self) -> &mut D {
        &mut self.slot
    }

    /// `bm_dfu_process_message`.
    pub fn on_message(&mut self, body: &[u8]) -> Accepted {
        self.machine.on_message(body)
    }

    /// When a timer is next due, if one is running.
    pub fn next_deadline(&self) -> Option<u32> {
        self.machine.next_deadline()
    }

    /// The next body to frame, stepping the machine until one is sent or it
    /// has nothing left to run. A reset the client asked for is made here,
    /// after the frames sent before it.
    pub(crate) fn next_body<C: Configuration>(
        &mut self,
        now_ms: u32,
        config: &mut C,
        git_sha: u32,
    ) -> Option<(MessageType, usize, [u8; OUTBOX_BODY])> {
        loop {
            if let Some(body) = self.outbox.pop() {
                return Some(body);
            }
            if let Some(reset) = self.reset.take() {
                self.persist();
                match reset {
                    Reset::Pending => self.slot.set_pending_and_reset(),
                    Reset::Fail => self.slot.fail_update_and_reset(),
                }
                continue;
            }
            let mut fx = Fx {
                slot: &mut self.slot,
                outbox: &mut self.outbox,
                reset: &mut self.reset,
                config: &mut *config,
                git_sha,
            };
            let ran = self.machine.step(&mut fx, now_ms).is_some();
            self.persist();
            if !ran && self.outbox.len == 0 && self.reset.is_none() {
                return None;
            }
        }
    }

    /// Hand a changed reboot info to no-init RAM.
    fn persist(&mut self) {
        let info = *self.machine.core().reboot_info();
        if info != self.stored {
            self.slot.store(&info);
            self.stored = info;
        }
    }
}

/// The machine's [`Effects`], over the node's seams.
struct Fx<'a, C, D> {
    slot: &'a mut D,
    outbox: &'a mut Outbox,
    reset: &'a mut Option<Reset>,
    config: &'a mut C,
    git_sha: u32,
}

impl<C: Configuration, D: DfuSlot> Effects for Fx<'_, C, D> {
    fn send(&mut self, message: &DfuMessage<'_>) {
        self.outbox.push(message);
    }
    fn lpm_peripheral_active(&mut self) {}
    fn lpm_peripheral_inactive(&mut self) {}
    /// Only a host update registers a callback, and this node does not host.
    fn update_finished(&mut self, _success: bool, _err: DfuErr, _node_id: u64) {}
    fn flash_open(&mut self) -> bool {
        self.slot.open()
    }
    fn flash_close(&mut self) -> bool {
        self.slot.close()
    }
    fn flash_size(&mut self) -> u32 {
        self.slot.size()
    }
    fn flash_erase(&mut self, offset: u32, len: u32) -> bool {
        self.slot.erase(offset, len)
    }
    fn flash_write(&mut self, offset: u32, data: &[u8]) -> bool {
        self.slot.write(offset, data)
    }
    fn set_confirmed(&mut self) {
        self.slot.set_confirmed();
    }
    fn set_pending_and_reset(&mut self) {
        *self.reset = Some(Reset::Pending);
    }
    fn fail_update_and_reset(&mut self) {
        *self.reset = Some(Reset::Fail);
    }
    fn git_sha(&self) -> u32 {
        self.git_sha
    }
    fn config(&mut self) -> Option<&mut ConfigStore> {
        self.config.store_mut()
    }
    fn commit_config(&mut self, partition: Partition) -> bool {
        self.config.commit(partition)
    }
}
