//! The node side of [`crate::dfu`]: starting an update, and running the DFU
//! task's queue.

use bm_wire::bcmp::dfu::ImgInfo;
use bm_wire::util::BmIpAddr;

use crate::config::Configuration;
use crate::dfu::{HostRequest, NodeDfu};
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};
use crate::service::Services;

use super::{Node, Outbound};

#[cfg(doc)]
use super::Event;

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// The DFU machine, its update slot and no-init RAM.
    pub fn dfu(&self) -> &NodeDfu<D> {
        &self.dfu
    }

    /// The same, mutably.
    pub fn dfu_mut(&mut self) -> &mut NodeDfu<D> {
        &mut self.dfu
    }

    /// Start hosting an update of `dst_node_id` — `bm_dfu_initiate_update`.
    ///
    /// `internal` reads the image from [`DfuSlot::read`]; otherwise the
    /// application feeds it with [`NodeDfu::host_queue_data`], a chunk ahead
    /// of the client's request (see `bm_wire::bcmp::dfu_host`). `notify` asks
    /// for [`Event::DfuUpdateFinished`], or
    /// [`NodeDfu::take_update_finished`] when driving the node by hand.
    ///
    /// Queues the request; [`Node::next_dfu_transmission`] runs it. `false`
    /// for a `chunk_size` over 1024, or if DFU is not idle, which with
    /// `notify` is also reported as finished with `BmDfuErrInProgress`.
    pub fn dfu_initiate_update(
        &mut self,
        info: ImgInfo,
        dst_node_id: u64,
        notify: bool,
        timeout_ms: u32,
        internal: bool,
    ) -> bool {
        let git_sha = self.identity.git_sha();
        let request = HostRequest {
            info,
            dst_node_id,
            notify,
            timeout_ms,
            internal,
        };
        self.dfu.initiate_update(&mut self.config, git_sha, request)
    }

    /// Milliseconds until a DFU timer is due — the client's chunk timer, the
    /// host's ACK or update timer — or `None` when none is running. Zero if
    /// one is overdue.
    #[must_use]
    pub fn dfu_remaining_ms(&self, now_ms: u32) -> Option<u32> {
        let left = self.dfu.next_deadline()?.wrapping_sub(now_ms);
        Some(if (left as i32) < 0 { 0 } else { left })
    }

    /// The next frame DFU owes the network, or `None` once the machine has
    /// nothing left to run.
    ///
    /// This is `bm_dfu_event_thread`: it posts any timer due by `now_ms`,
    /// runs queued events until one sends something, and frames that as
    /// `bcmp_tx` to `ff03::1` does. Call it until it returns `None` after
    /// anything that may have queued an event — a received DFU frame, or a
    /// timer [`Node::dfu_remaining_ms`] said was due — and once at start-up,
    /// when the machine leaves `Init`. A reset the client asks for is made
    /// from here, after the frames sent before it.
    ///
    /// Also `None`, leaving the rest queued, if the body's type has been
    /// unregistered.
    pub fn next_dfu_transmission(&mut self, now_ms: u32) -> Option<Outbound<'_>> {
        let git_sha = self.identity.git_sha();
        let (message_type, len, body) = self.dfu.next_body(now_ms, &mut self.config, git_sha)?;
        self.send(
            now_ms,
            &BmIpAddr::GLOBAL_MULTICAST,
            message_type,
            &body[..len],
            0,
        )
    }
}
