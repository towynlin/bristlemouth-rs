//! The async half: [`Node::run`] and its variants, and [`transmit`] and
//! [`deliver`], which put what the synchronous entry points return on the PHY.

use bm_wire::bcmp::forward;
use bm_wire::l2::{self, TxKind};
use bm_wire::neighbor::HEARTBEAT_PERIOD_S;
use bm_wire::service as service_wire;

use crate::app::{App, Observer};
use crate::config::Configuration;
use crate::port::{DfuSlot, Egress, Identity, NoInitRam, Phy, Rtc};
use crate::service::Services;

use super::{EXPIRY_PERIOD_MS, Event, MTU, Node, Outbound, Owed, Reflood, port_mask};

/// Transmit one frame, stamping the egress port into each copy that needs it.
///
/// This is `bm_l2_process_tx_evt` together with `send_global_multicast_packet`:
///
/// * global multicast goes out unstamped — to every port at once when the mask
///   covers every port, otherwise once per port in the mask;
/// * link-local multicast goes out once per port in the mask, with that port
///   stamped into the source address and the checksum patched to match;
/// * anything else is dropped, as bm_core drops it.
///
/// The mask is [`Outbound::mask`], which is every port for a frame the node
/// built and the routing policy's egress mask for a relay.
///
/// # Errors
///
/// Whatever the PHY returns. A failure on one port abandons the rest.
pub async fn transmit<P: Phy>(
    phy: &mut P,
    outbound: Outbound<'_>,
    port_count: u8,
) -> Result<(), P::Error> {
    let Outbound { frame, mask } = outbound;
    let all_ports = l2::all_ports_mask(port_count);
    let ports = || (1..=port_count).filter(move |port| mask & port_mask(*port) != 0);

    match l2::tx_kind(frame) {
        TxKind::GlobalMulticast if mask == all_ports => phy.send(frame, Egress::AllPorts).await,
        TxKind::GlobalMulticast => {
            for port in ports() {
                phy.send(frame, Egress::Port(port)).await?;
            }
            Ok(())
        }
        TxKind::LinkLocalMulticast => {
            for port in ports() {
                // The stamp is undone when it goes out of scope, so the next
                // port starts from a clean frame.
                let Ok(stamped) = l2::stamp_egress_port(frame, port) else {
                    return Ok(());
                };
                phy.send(&stamped, Egress::Port(port)).await?;
            }
            Ok(())
        }
        TxKind::Dropped => Ok(()),
    }
}

/// Transmit everything a received frame owed, in bm_core's order: the relayed
/// copy first, then the node's own reply.
///
/// [`Owed::forward`] is **not** covered, because a re-flood needs the node's
/// transmit buffer once per port and this has already given it away. Read the
/// field out before calling this and hand it to [`Node::reflood`] afterwards,
/// which is what [`Node::run`] does.
///
/// # Errors
///
/// Whatever the PHY returns. A failure abandons whatever is left.
pub async fn deliver<P: Phy>(
    phy: &mut P,
    owed: Owed<'_, '_>,
    port_count: u8,
) -> Result<(), P::Error> {
    if let Some(relay) = owed.relay {
        transmit(phy, relay, port_count).await?;
    }
    if let Some(reply) = owed.reply {
        transmit(phy, reply, port_count).await?;
    }
    Ok(())
}

impl<I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'_, I, R, C, D, S>
{
    /// Run the node until the PHY fails, discarding every [`Event`].
    ///
    /// See [`Node::run_with`], which is the same loop with somewhere for the
    /// replies and timeouts to go.
    ///
    /// # Errors
    ///
    /// The first error the PHY reports, from either direction.
    pub async fn run<P: Phy>(&mut self, phy: &mut P) -> P::Error {
        self.run_with(phy, |_| {}).await
    }

    /// Re-flood a received message out every port but the one it arrived on —
    /// the loop `bcmp_ll_forward` runs internally.
    ///
    /// `frame` is the frame the [`Reflood`] came from, which the caller gets
    /// back once the [`Owed`] it was carried in has been delivered. Each copy
    /// is built into the node's one transmit buffer and put on the wire before
    /// the next is built, because there is only one of it — the same
    /// constraint the C has, allocating one forward buffer per port in turn.
    ///
    /// A copy that does not fit the transmit buffer is skipped rather than
    /// abandoning the rest, which is what the C does too: its per-port loop
    /// records the error and carries on.
    ///
    /// # Errors
    ///
    /// Whatever the PHY returns. A failure abandons the remaining ports.
    pub async fn reflood<P: Phy>(
        &mut self,
        phy: &mut P,
        reflood: Reflood,
        frame: &[u8],
    ) -> Result<(), P::Error> {
        let port_count = self.port_count;
        for egress_port in forward::egress_ports(port_count, reflood.ingress_port) {
            let Some(outbound) = self.forward_link_local(egress_port, reflood.bcmp(frame)) else {
                continue;
            };
            transmit(phy, outbound, port_count).await?;
        }
        Ok(())
    }

    /// Put everything DFU owes on the wire — the frames
    /// [`Node::next_dfu_transmission`] hands out.
    ///
    /// # Errors
    ///
    /// Whatever the PHY returns. A failure abandons the frame it was sending.
    pub async fn transmit_dfu<P: Phy>(&mut self, phy: &mut P, now_ms: u32) -> Result<(), P::Error> {
        let port_count = self.port_count;
        while let Some(outbound) = self.next_dfu_transmission(now_ms) {
            transmit(phy, outbound, port_count).await?;
        }
        Ok(())
    }

    /// Put every queued re-send on the wire — the frames
    /// [`Node::next_retransmission`] hands out.
    ///
    /// # Errors
    ///
    /// Whatever the PHY returns. A failure abandons the rest, which stay
    /// queued.
    pub async fn retransmit<P: Phy>(&mut self, phy: &mut P) -> Result<(), P::Error> {
        let port_count = self.port_count;
        while let Some(outbound) = self.next_retransmission() {
            transmit(phy, outbound, port_count).await?;
        }
        Ok(())
    }

    /// Run the node until the PHY fails, reporting every [`Event`].
    ///
    /// [`Node::run_app`] with an application that never acts.
    ///
    /// # Errors
    ///
    /// The first error the PHY reports, from either direction.
    pub async fn run_with<P: Phy>(
        &mut self,
        phy: &mut P,
        events: impl FnMut(Event<'_>),
    ) -> P::Error {
        self.run_app(phy, &mut Observer(events)).await
    }

    /// Run the node and `app` until the PHY fails.
    ///
    /// Waits on whichever comes first — a frame, the heartbeat tick, either
    /// expiry sweep, a one-shot node timer, or [`App::ready`] — handles it,
    /// and transmits anything owed. The three periodic timers are bm_core's:
    /// `bcmp_heartbeat_s`, which also ages the neighbour table in that order,
    /// `packet.c`'s [`EXPIRY_PERIOD_MS`] sweep and `bm_service_request.c`'s
    /// [`service_wire::EXPIRY_PERIOD_MS`] sweep. Keeping them apart is what
    /// lets a request time out on the C's grid while heartbeats stay ten
    /// seconds apart.
    ///
    /// The application arm is polled last, so a frame or node timer that is
    /// due at the same time is handled first. Every [`Event`] goes to
    /// [`App::on_event`].
    ///
    /// Returns rather than panicking when the PHY errors, so the caller can
    /// decide whether that is fatal. It has no other exit.
    ///
    /// # Errors
    ///
    /// The first error the PHY reports, from either direction.
    pub async fn run_app<P: Phy, A: App<Self>>(&mut self, phy: &mut P, app: &mut A) -> P::Error {
        use embassy_futures::select::{Either6, select6};
        use embassy_time::{Duration, Instant, Ticker, Timer};

        let started = Instant::now();
        let mut ticker = Ticker::every(Duration::from_secs(u64::from(HEARTBEAT_PERIOD_S)));
        let mut expiry = Ticker::every(Duration::from_millis(u64::from(EXPIRY_PERIOD_MS)));
        let mut service_expiry = Ticker::every(Duration::from_millis(u64::from(
            service_wire::EXPIRY_PERIOD_MS,
        )));
        let mut rx = [0u8; MTU];
        let port_count = self.port_count;

        // `bm_dfu_init` queued `InitSuccess`; the DFU task runs it first.
        if let Err(error) = self.transmit_dfu(phy, 0).await {
            return error;
        }

        loop {
            // Cheap: the driver keeps this as an array it updates when it
            // services a PHY interrupt, so this is a read, not a transfer.
            for port in 1..=port_count {
                let up = phy.link_up(port);
                self.set_link_up(port, up);
            }

            let uptime_ms = |()| -> u32 {
                // Wraps at 49.7 days, which is what bm_core's tick counter
                // does too; `time_remaining` is written to survive it.
                started.elapsed().as_millis() as u32
            };

            // `NEIGHBOR_TIMER` and the DFU chunk timer: one-shots armed by
            // what they time rather than tickers, so they share an arm that
            // waits exactly as long as the sooner has left. Nothing running
            // means nothing to wait for, and the other arms are the
            // only way out.
            let now = uptime_ms(());
            let neighbor_wait = match (
                self.neighbor_request_remaining_ms(now),
                self.dfu_remaining_ms(now),
            ) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let neighbor_timer = async move {
                match neighbor_wait {
                    Some(ms) => Timer::after(Duration::from_millis(u64::from(ms))).await,
                    None => core::future::pending().await,
                }
            };

            match select6(
                phy.receive(&mut rx),
                ticker.next(),
                expiry.next(),
                service_expiry.next(),
                neighbor_timer,
                app.ready(),
            )
            .await
            {
                Either6::First(Ok((port, len))) => {
                    let now = uptime_ms(());
                    let owed =
                        self.on_frame_with(now, port, &mut rx[..len], |event| app.on_event(event));
                    // Copied out before `owed` is consumed: the re-flood needs
                    // the frame back, and `deliver` is holding it.
                    let forward = owed.forward;
                    if let Err(error) = deliver(phy, owed, port_count).await {
                        return error;
                    }
                    if let Some(reflood) = forward
                        && let Err(error) = self.reflood(phy, reflood, &rx[..len]).await
                    {
                        return error;
                    }
                }
                Either6::First(Err(error)) => return error,
                Either6::Second(()) => {
                    let now = uptime_ms(());
                    if let Some(outbound) = self.on_tick_with(now, |event| app.on_event(event))
                        && let Err(error) = transmit(phy, outbound, port_count).await
                    {
                        return error;
                    }
                    if let Err(error) = self.retransmit(phy).await {
                        return error;
                    }
                }
                Either6::Third(()) => {
                    let now = uptime_ms(());
                    self.on_expiry(now, |event| app.on_event(event));
                    if let Err(error) = self.retransmit(phy).await {
                        return error;
                    }
                }
                Either6::Fourth(()) => {
                    let now = uptime_ms(());
                    self.on_service_expiry(now, |event| app.on_event(event));
                }
                Either6::Fifth(()) => {
                    let now = uptime_ms(());
                    self.on_neighbor_request_timer(now, |event| app.on_event(event));
                }
                Either6::Sixth(()) => {
                    let now = uptime_ms(());
                    if let Some(outbound) = app.act(self, now)
                        && let Err(error) = transmit(phy, outbound, port_count).await
                    {
                        return error;
                    }
                }
            }
            // Whatever woke the loop may have queued a DFU event or brought a
            // DFU timer due.
            if let Err(error) = self.transmit_dfu(phy, uptime_ms(())).await {
                return error;
            }
            while let Some(finished) = self.dfu.take_update_finished() {
                app.on_event(Event::DfuUpdateFinished(finished));
            }
        }
    }
}
