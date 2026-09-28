//! Application code running beside the node, inside [`Node::run_app`].
//!
//! [`Node::run_app`]: crate::Node::run_app

use crate::node::{Event, Outbound};

/// Application code that [`Node::run_app`] runs in the node's own loop.
///
/// The loop waits on [`App::ready`] beside the PHY and the node's timers. When
/// it resolves, the loop calls [`App::act`] with the node, and transmits the
/// frame it returns. Every [`Event`] goes to [`App::on_event`].
///
/// `N` is the concrete [`Node`] type, so `act` can call any of its methods:
/// [`Node::ping`], [`Node::request_device_info`], [`Node::send`], and so on.
///
/// An application that wants to run as a task of its own can make `ready`
/// receive from a channel that task sends to; nothing here needs to know.
///
/// [`Node`]: crate::Node
/// [`Node::run_app`]: crate::Node::run_app
/// [`Node::ping`]: crate::Node::ping
/// [`Node::request_device_info`]: crate::Node::request_device_info
/// [`Node::send`]: crate::Node::send
#[allow(async_fn_in_trait)]
pub trait App<N> {
    /// Resolve when the application has something to do.
    ///
    /// **Must be cancel-safe.** The loop drops this future whenever a frame or
    /// a node timer wins the `select`, and calls it again on the next pass.
    /// Keep deadlines in `self` (an [`embassy_time::Ticker`], a
    /// [`embassy_time::Timer::at`] on a stored instant, a channel receive),
    /// not in the future's locals.
    async fn ready(&mut self);

    /// Act on the node, `now_ms` after [`Node::run_app`] started, on the
    /// clock the node's own entry points were given.
    ///
    /// Returns at most one frame, because the node has one transmit buffer.
    /// An application with more to send leaves `ready` resolving immediately
    /// until it has sent it all.
    ///
    /// [`Node::run_app`]: crate::Node::run_app
    fn act<'n>(&mut self, node: &'n mut N, now_ms: u32) -> Option<Outbound<'n>>;

    /// An [`Event`] the node reported. Called from within the node's receive
    /// and timer handling, so it cannot reach the node.
    fn on_event(&mut self, event: Event<'_>) {
        let _ = event;
    }
}

/// An [`App`] that never acts and hands every event to a closure — what
/// [`Node::run_with`] runs.
///
/// [`Node::run_with`]: crate::Node::run_with
pub(crate) struct Observer<F>(pub(crate) F);

impl<N, F: FnMut(Event<'_>)> App<N> for Observer<F> {
    async fn ready(&mut self) {
        core::future::pending().await
    }

    fn act<'n>(&mut self, _node: &'n mut N, _now_ms: u32) -> Option<Outbound<'n>> {
        None
    }

    fn on_event(&mut self, event: Event<'_>) {
        (self.0)(event);
    }
}
