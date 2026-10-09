//! BCMP requests and replies: ping, system time, device information, the
//! neighbour table and resource discovery.

use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::info::{
    CachedInfo, DeviceInfoReply, DeviceInfoRequest, InfoCacheView, InfoRequestKind,
    InfoRequestsView,
};
use bm_wire::bcmp::neighbors::{
    NEIGHBOR_TABLE_MAX_LEN, NeighborTableRequest, PortInfo, TableRequestKind, TableRequests,
    encode_neighbor_table_reply_from, neighbor_table_reply_len,
};
use bm_wire::bcmp::ping::{EchoReply, EchoRequest};
use bm_wire::bcmp::resource::{
    ResourceAddError, ResourceRequestKind, ResourceRequestsView, ResourceTableRequest,
    ResourceTableView, ResourceType,
};
use bm_wire::bcmp::time::{SystemTimeHeader, SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
use bm_wire::util::BmIpAddr;

use crate::config::Configuration;
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc, RtcTimeAndDate};
use crate::service::Services;

use super::{Event, Node, Outbound, build_outbound};

#[cfg(doc)]
use super::NodeResources;
#[cfg(doc)]
use bm_wire::bcmp::resource::RESOURCE_NAME_BYTES;

/// Most ports a neighbour-table reply will describe.
///
/// The port field in the reply is a `u8`, and no Bristlemouth device has more
/// than a handful; this is only the size of a stack array.
const MAX_REPORTED_PORTS: usize = 16;

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// The `switch` in `bcmp_time_process_time_message`, for a message this
    /// node is not forwarding.
    ///
    /// Three arms, and they do not agree with each other about what
    /// `target_node_id == 0` means: see [`bm_wire::bcmp::time`] and divergence
    /// #27. All three answers go to `FF02::1` rather than back to the address
    /// the message arrived from, because `bcmp_time_send_response` always
    /// passes `multicast_ll_addr`.
    pub(super) fn process_system_time<'s>(
        &'s mut self,
        now_ms: u32,
        message_type: MessageType,
        payload: &[u8],
    ) -> Option<Outbound<'s>> {
        let our_node_id = self.identity.node_id();
        match message_type {
            MessageType::SYSTEM_TIME_REQUEST => {
                let request = SystemTimeRequest::decode(payload).ok()?;
                if !request.is_for(our_node_id) {
                    // A broadcast request reached the switch and dies here.
                    return None;
                }
                // `bm_rtc_get` failing means no answer at all, not an empty one.
                let utc_time_us = self.rtc.get()?.to_utc_micros();
                self.build_system_time_response(now_ms, request.header.source_node_id, utc_time_us)
            }
            MessageType::SYSTEM_TIME_RESPONSE => {
                // `bm_debug` and nothing else. The application already has it
                // as `Event::Message`, which is more than a C node offers.
                None
            }
            MessageType::SYSTEM_TIME_SET => {
                // The C reads `utc_time_us` past the header without checking
                // the size, as above.
                let set = SystemTimeSet::decode(payload).ok()?;
                // The C's `0x12` arm makes no test of its own -- this is the
                // outer one restated, so the function is right when read
                // alone. That absence is the divergence: zero got past the
                // outer test by being a broadcast, and nothing here takes it
                // back, so a broadcast set is honoured where a broadcast
                // request is not.
                if !set.is_for(our_node_id) {
                    return None;
                }
                if !self
                    .rtc
                    .set(&RtcTimeAndDate::from_utc_micros(set.utc_time_us))
                {
                    return None;
                }
                // The response echoes the microseconds that were *asked for*,
                // not what the RTC kept -- which differ, because the RTC has
                // only millisecond resolution.
                self.build_system_time_response(now_ms, set.header.source_node_id, set.utc_time_us)
            }
            _ => None,
        }
    }

    /// Ping `target_node_id`, or every node if it is zero — the whole of
    /// `bcmp_send_ping_request`.
    ///
    /// The request carries this node's id truncated to sixteen bits as its
    /// `id`, and `ping.c`'s own counter — not `packet.c`'s — as its `seq_num`,
    /// also truncated. `payload` is copied into the node's single expectation
    /// slot, replacing whatever the last ping left there; a reply matching it
    /// arrives as [`Event::EchoReply`].
    ///
    /// bm_core sends to `multicast_ll_addr` from every call site it has, but
    /// takes the address as an argument, so this does too.
    ///
    /// Returns `None` without sending, and **without disturbing the ping
    /// state**, when `payload` is longer than [`NodeResources`]' `PING_PAYLOAD` or than the `u16`
    /// length field: there would be nowhere to remember it. That is the one
    /// thing here with no C counterpart — bm_core `bm_malloc`s the copy and
    /// dereferences the result unchecked.
    ///
    /// Also `None`, but *after* the counter has advanced and the payload has
    /// been stored, if the request does not fit the transmit buffer or
    /// [`MessageType::ECHO_REQUEST`] is unregistered. That is the C's order,
    /// where `BCMP_SEQ++` and the copy both happen before `bcmp_tx` is called.
    pub fn ping(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        target_node_id: u64,
        payload: &[u8],
    ) -> Option<Outbound<'_>> {
        if payload.len() > self.ping.expected.len() || payload.len() > EchoRequest::MAX_PAYLOAD_LEN
        {
            return None;
        }

        // `echo_req->seq_num = BCMP_SEQ++`: the frame carries the value from
        // before the increment, sixteen bits of a thirty-two bit counter.
        let seq_num = self.ping.seq as u16;
        self.ping.seq = self.ping.seq.wrapping_add(1);
        // An empty slice stands in for both of the C's ways of asking for a
        // payload-free ping, a null pointer and a zero length -- it folds them
        // together at the top of `bcmp_send_ping_request` itself.
        self.ping.remember(payload);
        // The C stamps this after `bcmp_tx` returns rather than before. No
        // clock moves in between, so the order is not observable.
        self.ping.sent_at_ms = now_ms;

        let request = EchoRequest {
            target_node_id,
            id: self.identity.node_id() as u16,
            seq_num,
            payload,
        };
        let stamp = self.outgoing(now_ms, MessageType::ECHO_REQUEST, 0)?;
        let Self {
            identity, tx, held, ..
        } = self;
        build_outbound(
            &mut tx[..],
            held,
            identity.node_id(),
            dst,
            MessageType::ECHO_REQUEST,
            stamp,
            |body| request.encode(body),
        )
    }

    /// `BCMP_SEQ`: the number the *next* ping will carry, before truncation.
    #[must_use]
    pub fn ping_sequence(&self) -> u32 {
        self.ping.seq
    }

    /// `EXPECTED_PAYLOAD` and `EXPECTED_PAYLOAD_LEN`, which an echo reply is
    /// compared against. `None` is the C's null pointer.
    ///
    /// Nothing clears this: bm_core keeps the last ping's payload for the life
    /// of the process, so a reply carrying it is accepted however long
    /// afterwards it arrives.
    #[must_use]
    pub fn expected_ping_payload(&self) -> Option<&[u8]> {
        self.ping.expected_payload()
    }

    /// Ask `target_node_id` what time it is — `bcmp_time_get_time`.
    ///
    /// Goes to `FF02::1`, so every node on the link sees it and exactly one
    /// answers. Passing zero asks **nobody**: the broadcast reaches every
    /// node's switch and every node drops it on the exact-match test. That is
    /// divergence #27, reproduced rather than corrected — a C node on the
    /// other end would ignore it too.
    pub fn request_system_time(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; SystemTimeRequest::LEN];
        SystemTimeRequest {
            header: SystemTimeHeader {
                target_node_id,
                source_node_id: self.identity.node_id(),
            },
        }
        .encode(&mut body)
        .ok()?;
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::SYSTEM_TIME_REQUEST,
            &body,
        )
    }

    /// Tell `target_node_id` what time it is — `bcmp_time_set_time`.
    ///
    /// Zero here *is* a broadcast, unlike [`Node::request_system_time`]: every
    /// node on the link adopts `utc_time_us` and every one of them answers.
    pub fn set_system_time(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        utc_time_us: u64,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; SystemTimeSet::LEN];
        SystemTimeSet {
            header: SystemTimeHeader {
                target_node_id,
                source_node_id: self.identity.node_id(),
            },
            utc_time_us,
        }
        .encode(&mut body)
        .ok()?;
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::SYSTEM_TIME_SET,
            &body,
        )
    }

    /// Ask `target_node_id` to describe itself, or every node if it is zero —
    /// `bcmp_request_info`.
    ///
    /// Goes to `FF02::1`, which is the address both of bm_core's own call
    /// sites pass. The request is recorded in [`Node::info_requests`] first
    /// and the record is dropped again if nothing could be sent, which is the
    /// C's `ll_item_add` before `bcmp_tx` and its `ll_remove` after a failure.
    ///
    /// `kind` is the C's `cb` argument. Both of bm_core's own call sites pass
    /// `NULL`, which is [`InfoRequestKind::Cache`].
    ///
    /// Returns `None` without sending when
    /// [`MessageType::DEVICE_INFO_REQUEST`] is unregistered. A request the
    /// list had no room for is still sent, and its reply then arrives as
    /// unsolicited traffic — the same shape as an untracked sequenced request.
    pub fn request_device_info(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        kind: InfoRequestKind,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; DeviceInfoRequest::LEN];
        DeviceInfoRequest { target_node_id }
            .encode(&mut body)
            .ok()?;
        let recorded = self.info_requests.record(target_node_id, kind);
        // `bcmp_tx` fails for an unregistered type and nothing else here: the
        // body is eight bytes, so the size guard cannot refuse it. Asking the
        // registry before the transmit borrow begins puts the C's `ll_remove`
        // ahead of the failure it answers, which is the same end state.
        if self
            .registry
            .cfg(MessageType::DEVICE_INFO_REQUEST)
            .is_none()
        {
            if recorded {
                // `ll_remove` takes the first entry with the key, which is not
                // necessarily the one just added -- see divergence #19.
                self.info_requests.take(target_node_id);
            }
            return None;
        }
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::DEVICE_INFO_REQUEST,
            &body,
        )
    }

    /// Ask `target_node_id` for its neighbour table —
    /// `bcmp_request_neighbor_table`.
    ///
    /// `dst` is the C's `addr`, and which one it is matters: bm_core's only
    /// caller, `integrations/topology.c`, passes `multicast_global_addr`, so
    /// the request is relayed across the whole network rather than only to
    /// this node's neighbours.
    ///
    /// `kind` is the C's `request` argument, and a
    /// [`TableRequestKind::Report`] reply arrives as
    /// [`Event::NeighborTable`]. Passing zero for `target_node_id` asks every
    /// node and accepts no answer — see divergence #35.
    ///
    /// **The request is recorded whether or not it is sent**, which is the C's
    /// order: `TARGET_NODE_ID`, the timer and the callback are all written
    /// before `bcmp_tx`, and none of them is undone when it fails.
    /// [`Node::request_device_info`] is the other way round. So a `None` here
    /// still arms the timeout, and still makes the node accept a reply — the
    /// C reports the error to its caller and leaves the same state behind.
    ///
    /// Returns `None` without sending when
    /// [`MessageType::NEIGHBOR_TABLE_REQUEST`] is unregistered.
    pub fn request_neighbor_table(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        target_node_id: u64,
        kind: TableRequestKind,
    ) -> Option<Outbound<'_>> {
        self.table_requests.record(now_ms, target_node_id, kind);
        let mut body = [0u8; NeighborTableRequest::LEN];
        NeighborTableRequest { target_node_id }
            .encode(&mut body)
            .ok()?;
        self.request(now_ms, dst, MessageType::NEIGHBOR_TABLE_REQUEST, &body)
    }

    /// `bcmp/neighbors.c`'s requester state: which node was asked for its
    /// neighbour table, whether an answer is still owed to the application,
    /// and how long the timer has left.
    #[must_use]
    pub fn table_requests(&self) -> &TableRequests {
        &self.table_requests
    }

    /// Run `NEIGHBOR_TIMER`, reporting [`Event::NeighborTableTimeout`] if it is
    /// due.
    ///
    /// Schedule it from [`Node::neighbor_request_remaining_ms`], or call it
    /// often enough that a second is not much overshot. Like
    /// [`Node::on_expiry`] the
    /// deadline lives in the state, so calling this early, late or twice
    /// changes nothing — and unlike the expiry sweep it fires at the deadline
    /// rather than on a grid, because the C's timer is a one-shot armed by the
    /// request. [`Node::on_tick`] and [`Node::on_expiry`] also run it, so a
    /// node driven only by those still times out, up to their period late.
    pub fn on_neighbor_request_timer(&mut self, now_ms: u32, mut events: impl FnMut(Event<'_>)) {
        if self.table_requests.on_timer(now_ms) {
            events(Event::NeighborTableTimeout {
                target_node_id: self.table_requests.target_node_id(),
            });
        }
    }

    /// Milliseconds until [`Node::on_neighbor_request_timer`] has something to
    /// report, or `None` when no neighbour-table request is being timed.
    #[must_use]
    pub fn neighbor_request_remaining_ms(&self, now_ms: u32) -> Option<u32> {
        self.table_requests.remaining_ms(now_ms)
    }

    /// Advertise a resource — `bcmp_resource_discovery_add_resource`.
    ///
    /// [`Node::subscribe`] and [`Node::publish`] call this as `bm_sub_wl` and
    /// `bm_pub_wl` do; anything else advertised is the firmware's. The two
    /// lists are what a `0x0A` is answered with.
    ///
    /// # Errors
    ///
    /// [`ResourceAddError::AlreadyPresent`] if the list already covers `name`
    /// — which, the C's de-duplication being a prefix match, includes names
    /// that are not in it (divergence #38). [`ResourceAddError::Full`] once
    /// [`NodeResources`]' `RESOURCES` resources are held or for a name longer
    /// than [`RESOURCE_NAME_BYTES`], neither of which bm_core has.
    pub fn add_resource(
        &mut self,
        name: &[u8],
        kind: ResourceType,
    ) -> Result<(), ResourceAddError> {
        self.resources.add(name, kind)
    }

    /// `PUB_LIST` and `SUB_LIST`: the resources this node advertises.
    pub fn resources(&self) -> &ResourceTableView {
        self.resources
    }

    /// `RESOURCE_REQUEST_LIST`: which nodes have been asked for their resource
    /// table and have not answered.
    pub fn resource_requests(&self) -> &ResourceRequestsView {
        self.resource_requests
    }

    /// Ask `target_node_id` for its resource table —
    /// `bcmp_resource_discovery_send_request`.
    ///
    /// Goes to `FF02::1`, the address the C hard-codes. Passing zero asks
    /// nobody: the responder wants an exact match, so the request reaches
    /// every node and is answered by none (divergence #37).
    ///
    /// `kind` is the C's `fp` argument, and a [`ResourceRequestKind::Report`]
    /// reply arrives as [`Event::ResourceTable`].
    ///
    /// **The request is recorded only if it was sent**, which is the C's
    /// order: `bcmp_tx` runs first and `ll_item_add` only on its success.
    /// [`Node::request_device_info`] records first and undoes it;
    /// [`Node::request_neighbor_table`] records and keeps it. All three
    /// modules differ, so none of them is a pattern for the next.
    ///
    /// Returns `None` without sending when
    /// [`MessageType::RESOURCE_TABLE_REQUEST`] is unregistered. A request the
    /// list had no room for is still sent, and its reply then arrives as
    /// unsolicited traffic — the C reaches the same place when `ll_item_add`
    /// fails, and reports `BmENOMEM` for a request already on the wire.
    pub fn request_resource_table(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        kind: ResourceRequestKind,
    ) -> Option<Outbound<'_>> {
        let mut body = [0u8; ResourceTableRequest::LEN];
        ResourceTableRequest { target_node_id }
            .encode(&mut body)
            .ok()?;
        // An unregistered type is the only failure `bcmp_tx` can report here:
        // the body is eight bytes, so the size guard cannot refuse it. Asking
        // the registry before the transmit borrow begins keeps the C's order,
        // where nothing is recorded for a request that did not go out.
        self.registry.cfg(MessageType::RESOURCE_TABLE_REQUEST)?;
        self.resource_requests.record(target_node_id, kind);
        self.request(
            now_ms,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::RESOURCE_TABLE_REQUEST,
            &body,
        )
    }

    /// What is known about `node_id`, or `None` if nothing is.
    ///
    /// Populated by a [`InfoRequestKind::Cache`] reply from a node that is
    /// already in the neighbour table, and forgotten when that neighbour is
    /// evicted. An *offline* neighbour keeps its entry, as it keeps its table
    /// row.
    #[must_use]
    pub fn device_info(&self, node_id: u64) -> Option<CachedInfo<'_>> {
        self.info.get(node_id)
    }

    /// Everything known about every node — what `populate_neighbor_info`
    /// writes onto bm_core's neighbour table entries.
    pub fn device_info_cache(&self) -> &InfoCacheView {
        self.info
    }

    /// `INFO_REQUEST_LIST`: which nodes have been asked to describe themselves
    /// and have not answered.
    pub fn info_requests(&self) -> &InfoRequestsView {
        self.info_requests
    }

    pub(super) fn build_device_info_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        reply_seq_num: u32,
    ) -> Option<Outbound<'_>> {
        let stamp = self.outgoing(now_ms, MessageType::DEVICE_INFO_REPLY, reply_seq_num)?;
        let Self {
            identity, tx, held, ..
        } = self;
        let node_id = identity.node_id();
        let mut info = identity.device_info();
        info.node_id = node_id;
        info.git_sha = identity.git_sha();
        // Each length field is a single byte. bm_core assigns `strlen()` to a
        // `uint8_t`, which wraps -- a 300-byte name becomes 44 bytes of it.
        // Truncating is the same size ceiling without the surprise.
        let version = identity.version_string();
        let name = identity.device_name();
        let reply = DeviceInfoReply {
            info,
            version_string: &version[..version.len().min(DeviceInfoReply::MAX_STRING_LEN)],
            device_name: &name[..name.len().min(DeviceInfoReply::MAX_STRING_LEN)],
        };
        build_outbound(
            &mut tx[..],
            held,
            node_id,
            dst,
            MessageType::DEVICE_INFO_REPLY,
            stamp,
            |body| reply.encode(body),
        )
    }

    pub(super) fn build_echo_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        reply: &EchoReply<'_>,
    ) -> Option<Outbound<'_>> {
        // `bcmp_send_ping_reply` hands `bcmp_tx` the request's *body*
        // `seq_num` to echo into the header -- and `serialize` throws it away,
        // because ping is registered neither `sequenced_reply` nor
        // `sequenced_request`, so the header gets a zero. Passing it anyway
        // keeps the call the same shape as the C's; divergence #31 is what
        // becomes of it.
        let stamp = self.outgoing(now_ms, MessageType::ECHO_REPLY, u32::from(reply.seq_num))?;
        let Self {
            identity, tx, held, ..
        } = self;
        build_outbound(
            &mut tx[..],
            held,
            identity.node_id(),
            dst,
            MessageType::ECHO_REPLY,
            stamp,
            |body| reply.encode(body),
        )
    }

    /// `bcmp_time_send_response`: a `0x11` to `FF02::1`, naming `target_node_id`
    /// in the body and this node as its source.
    fn build_system_time_response(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        utc_time_us: u64,
    ) -> Option<Outbound<'_>> {
        let stamp = self.outgoing(now_ms, MessageType::SYSTEM_TIME_RESPONSE, 0)?;
        let Self {
            identity, tx, held, ..
        } = self;
        let node_id = identity.node_id();
        let response = SystemTimeResponse {
            header: SystemTimeHeader {
                target_node_id,
                source_node_id: node_id,
            },
            utc_time_us,
        };
        build_outbound(
            &mut tx[..],
            held,
            node_id,
            &BmIpAddr::LINK_LOCAL_MULTICAST,
            MessageType::SYSTEM_TIME_RESPONSE,
            stamp,
            |body| {
                response.encode(body)?;
                Ok(SystemTimeResponse::LEN)
            },
        )
    }

    /// `bcmp_process_resource_discovery_request`'s answer: the whole of both
    /// lists, publishers first.
    ///
    /// bm_core sizes a `bm_malloc` with `bcmp_resource_compute_list_size` and
    /// then fills it under the lists' locks a second time, so a concurrent
    /// `bcmp_resource_discovery_add_resource` overflows it — divergence #39.
    /// Here the table cannot change while the reply is being built.
    ///
    /// The `seq_num` handed to `bcmp_tx` is a literal zero rather than the
    /// request's, which for an unsequenced type is what `serialize` would have
    /// written anyway (the other side of divergence #31).
    pub(super) fn build_resource_table_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
    ) -> Option<Outbound<'_>> {
        let stamp = self.outgoing(now_ms, MessageType::RESOURCE_TABLE_REPLY, 0)?;
        let Self {
            identity,
            resources,
            held,
            tx,
            ..
        } = self;
        let node_id = identity.node_id();
        build_outbound(
            &mut tx[..],
            held,
            node_id,
            dst,
            MessageType::RESOURCE_TABLE_REPLY,
            stamp,
            |body| resources.encode_reply(body, node_id),
        )
    }

    pub(super) fn build_neighbor_table_reply(
        &mut self,
        now_ms: u32,
        dst: &BmIpAddr,
        reply_seq_num: u32,
    ) -> Option<Outbound<'_>> {
        // `bcmp_send_neighbor_table`'s first act: a table that would not fit
        // `bcmp_table_max_len` is answered with nothing at all. Checked before
        // the registry is asked, as the C checks it before `bcmp_tx`. Out of
        // reach at any plausible neighbour count -- 101 neighbours on a two-port
        // node -- so nothing differential covers it.
        if neighbor_table_reply_len(usize::from(self.port_count), self.neighbors.len())
            > NEIGHBOR_TABLE_MAX_LEN
        {
            return None;
        }
        let stamp = self.outgoing(now_ms, MessageType::NEIGHBOR_TABLE_REPLY, reply_seq_num)?;
        let Self {
            identity,
            neighbors,
            port_count,
            link_mask,
            held,
            tx,
            ..
        } = self;
        let node_id = identity.node_id();

        // bm_core reports every port with the link state it has and a type it
        // never fills in.
        let mut ports = [PortInfo::default(); MAX_REPORTED_PORTS];
        let ports = &mut ports[..usize::from(*port_count).min(MAX_REPORTED_PORTS)];
        for (index, port) in ports.iter_mut().enumerate() {
            port.state = u8::from(*link_mask & (1 << index) != 0);
        }

        let table = neighbors
            .neighbors()
            .map(|neighbor| bm_wire::bcmp::NeighborInfo {
                node_id: neighbor.node_id,
                port: neighbor.port,
                online: u8::from(neighbor.online),
            });

        build_outbound(
            &mut tx[..],
            held,
            node_id,
            dst,
            MessageType::NEIGHBOR_TABLE_REPLY,
            stamp,
            |body| encode_neighbor_table_reply_from(body, node_id, ports, table),
        )
    }
}
