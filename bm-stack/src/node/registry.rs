//! `bcmp/packet.c`'s registry from the sending side: registering message types,
//! sending, requesting, and re-sending.

use bm_wire::BmWireError;
use bm_wire::bcmp::registry::{PacketCfg, RegistryError, RegistryView};
use bm_wire::bcmp::{BCMP_HEADER_LEN, MessageType, tx};
use bm_wire::frame::MIN_FRAME_WITH_ADDRESSES;
use bm_wire::util::BmIpAddr;

use crate::config::Configuration;
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};
use crate::service::Services;

use super::{HeldRequests, MESSAGE_TYPES, MTU, Node, Outbound, Stamp};

#[cfg(doc)]
use super::Event;

/// [`tx::build`], handed back as the [`Outbound`] every `build_*` returns.
///
/// A tracked request's frame is also kept in `held`, for re-sending.
pub(super) fn build_outbound<'a, F>(
    tx: &'a mut [u8],
    held: &mut HeldRequests<'_>,
    node_id: u64,
    dst: &BmIpAddr,
    message_type: MessageType,
    stamp: Stamp,
    body: F,
) -> Option<Outbound<'a>>
where
    F: FnOnce(&mut [u8]) -> Result<usize, BmWireError>,
{
    let end = tx::build(tx, node_id, dst, message_type, stamp.seq_num, body).ok()?;
    let frame = tx.get_mut(..end)?;
    if stamp.tracked {
        held.hold(stamp.seq_num, frame);
    }
    Some(Outbound {
        frame,
        mask: stamp.mask,
    })
}

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// Register a message type, as each module's init does with `packet_add`.
    ///
    /// A newly ported exchange registers its types here, with the flags its
    /// C module uses. Until a type is registered the node will
    /// neither send it nor dispatch it.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Full`] once [`MESSAGE_TYPES`] types are registered.
    pub fn register(
        &mut self,
        message_type: MessageType,
        cfg: PacketCfg,
    ) -> Result<(), RegistryError> {
        self.registry.add(message_type, cfg)
    }

    /// Remove the first registration for `message_type`, as `packet_remove`
    /// does, reporting whether there was one.
    ///
    /// A node that unregisters a type it answers stops answering it: the
    /// message is dropped before its body is looked at, the C's `BmENODEV`.
    pub fn unregister(&mut self, message_type: MessageType) -> bool {
        self.registry.remove(message_type)
    }

    /// The packet registry: what is registered, and what is still waiting for
    /// a reply.
    pub fn registry(&self) -> &RegistryView<MESSAGE_TYPES> {
        self.registry
    }

    /// The next request frame a sweep owes the network again, or `None`.
    ///
    /// `timer_traverse_cb`'s `PACKET.cb.send(element->buf)`: the frame the
    /// request was first sent in, byte for byte, to every port. Frames come
    /// out in the order the sweep retried them. A reply or timeout that
    /// arrives before a queued re-send is taken drops it, so take them all
    /// straight after [`Node::on_expiry`] or [`Node::on_tick`].
    pub fn next_retransmission(&mut self) -> Option<Outbound<'_>> {
        let mask = self.all_ports_mask();
        let frame = self.held.next_due()?;
        Some(Outbound { frame, mask })
    }

    /// Serialize a BCMP message and hand it back ready to transmit — `bcmp_tx`.
    ///
    /// `reply_seq_num` is the number being echoed. The registry decides
    /// whether it is used: a [`PacketCfg::sequenced_reply`] type carries it, a
    /// [`PacketCfg::sequenced_request`] type ignores it and takes the next
    /// number from the node's counter, and anything else — which outside
    /// `bcmp/config.c` is every type bm_core has — carries zero. Use
    /// [`Node::request`] when there is nothing to echo.
    ///
    /// Returns `None` and sends nothing when the type is not registered — the
    /// C's `BmENODEV`, where `serialize` leaves the caller's buffer untouched
    /// and `bcmp_tx` never reaches `bm_ip_tx_perform`. Also `None` when the
    /// message does not fit the transmit buffer, checked before the registry is
    /// asked so an oversized request never becomes an outstanding one, as in
    /// the C. The ceiling here is 1447 body bytes; the C's guard admits one
    /// more and then builds a frame a byte over the MTU (divergence #8).
    pub fn send(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        body: &[u8],
        reply_seq_num: u32,
    ) -> Option<Outbound<'_>> {
        self.send_with(
            now_ms,
            dst,
            message_type,
            reply_seq_num,
            body.len(),
            |_, buf| {
                buf.get_mut(..body.len())
                    .ok_or(BmWireError::Truncated)?
                    .copy_from_slice(body);
                Ok(body.len())
            },
        )
    }

    /// [`Node::send`] for a body of `body_len` bytes that `encode` writes
    /// straight into the transmit buffer, with the node's [`Configuration`]
    /// to read from.
    fn send_with(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        reply_seq_num: u32,
        body_len: usize,
        encode: impl FnOnce(&C, &mut [u8]) -> Result<usize, BmWireError>,
    ) -> Option<Outbound<'_>> {
        let end = MIN_FRAME_WITH_ADDRESSES
            .checked_add(BCMP_HEADER_LEN)?
            .checked_add(body_len)?;
        if end > MTU {
            return None;
        }
        let stamp = self.outgoing(now_ms, message_type, reply_seq_num)?;
        let Self {
            identity,
            tx,
            held,
            config,
            ..
        } = self;
        build_outbound(
            &mut tx[..],
            held,
            identity.node_id(),
            dst,
            message_type,
            stamp,
            |buf| encode(config, buf),
        )
    }

    /// [`Node::send_with`] to `FF02::1`, which is where every `bcmp_tx` in
    /// `bcmp/config.c` sends.
    pub(super) fn send_multicast(
        &mut self,
        now_ms: u32,
        message_type: MessageType,
        reply_seq_num: u32,
        body_len: usize,
        encode: impl FnOnce(&C, &mut [u8]) -> Result<usize, BmWireError>,
    ) -> Option<Outbound<'_>> {
        self.send_with(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            message_type,
            reply_seq_num,
            body_len,
            encode,
        )
    }

    /// Send a message that is not answering one — [`Node::send`] with no
    /// sequence number to echo, which is what every one of bm_core's own
    /// request sites passes.
    ///
    /// If `message_type` is registered as a
    /// [`PacketCfg::sequenced_request`], the message carries the node's next
    /// sequence number and is recorded as outstanding: a reply carrying that
    /// number comes back as [`Event::Reply`]. Silence is answered by re-sending
    /// the same frame from each [`Node::on_expiry`] sweep at least
    /// [`DEFAULT_MESSAGE_TIMEOUT_MS`][bm_wire::bcmp::registry::DEFAULT_MESSAGE_TIMEOUT_MS]
    /// after the last send, [`PACKET_RETRY_COUNT`][bm_wire::bcmp::registry::PACKET_RETRY_COUNT]
    /// times, and then by [`Event::Timeout`] from the next.
    pub fn request(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        message_type: MessageType,
        body: &[u8],
    ) -> Option<Outbound<'_>> {
        self.send(now_ms, dst, message_type, body, 0)
    }

    /// The sequence number `message_type` goes out with, and the port mask a
    /// frame this node built is transmitted on.
    ///
    /// `None` for an unregistered type, which is the C's `BmENODEV`: the
    /// registry is asked first, so a message whose type nothing registered is
    /// never built.
    pub(super) fn outgoing(
        &mut self,
        now_ms: u32,
        message_type: MessageType,
        reply_seq_num: u32,
    ) -> Option<Stamp> {
        let outgoing = self
            .registry
            .on_serialize(now_ms, message_type, reply_seq_num)
            .ok()?;
        Some(Stamp {
            seq_num: outgoing.seq_num,
            mask: self.all_ports_mask(),
            tracked: outgoing.tracked,
        })
    }
}
