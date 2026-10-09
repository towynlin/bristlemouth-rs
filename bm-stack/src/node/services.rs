//! Services: `middleware/bm_service.c` and `bm_service_request.c`, and the
//! built-in echo, sys_info, config_map, power_info and metrics services.

use bm_wire::BmWireError;
use bm_wire::bcmp::resource::{RESOURCE_NAME_BYTES, ResourceType};
use bm_wire::configuration::{MapError, Partition};
use bm_wire::l2;
use bm_wire::pubsub::{self, Subscriber, SubscriptionError, SubscriptionsView};
use bm_wire::service::config_map::{self, ConfigMapRequest};
use bm_wire::service::metrics;
use bm_wire::service::power_info::{self, Ended};
use bm_wire::service::sys_info::{self, SysInfoReply};
use bm_wire::service::{
    self as service_wire, Lookup, ReplyHeader, ReplyOutcome, RequestHeader, ServiceTable,
};
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::config::Configuration;
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};
use crate::service::{
    PowerInfoCallbacks, RegisterError, SERVICE_NAME_BYTES, SERVICES, ServiceHandler,
    ServiceRequestError, ServiceRequests, Services, UnregisterError,
};

use super::{Event, MTU, Node, Outbound};

#[cfg(doc)]
use super::Owed;
#[cfg(doc)]
use crate::config::NoConfig;

/// `services_cbor_encoded_as_crc32(BM_CFG_PARTITION_SYSTEM)`, for a sys_info
/// reply. With no store, an empty partition's: the CRC of `a0`.
fn sys_config_crc(config: &impl Configuration) -> u32 {
    config.store().map_or_else(
        || bm_wire::crc::crc32_ieee(&[0xa0]),
        |store| store.partition(Partition::System).cbor_map_crc32(),
    )
}

/// The `BmPowerInfoReplyCb` call `power_info_reply_cb` makes, if any.
fn report_power_info(ended: Ended, events: &mut impl FnMut(Event<'_>)) {
    if let Ended::Called { callback, reply } = ended {
        events(Event::PowerInfoReply {
            id: callback,
            reply,
        });
    }
}

/// `services_cbor_as_map(partition)`, for a config_map reply. With no store,
/// an empty partition's: `a0`.
fn partition_map(
    config: &impl Configuration,
    partition: Partition,
    out: &mut [u8],
) -> Result<usize, MapError> {
    match config.store() {
        Some(store) => store.partition(partition).cbor_map(out),
        None => match out.first_mut() {
            Some(byte) => {
                *byte = 0xa0;
                Ok(1)
            }
            None => Err(MapError::TooSmall(1)),
        },
    }
}

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// `_service_request_received_cb`, up to the `bm_pub_wl` of its reply:
    /// find the service, call its handler, and build the reply frame into
    /// `tx`. Returns the frame's length, or `None` for no reply.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn serve(
        table: &ServiceTable<ServiceHandler, SERVICES, SERVICE_NAME_BYTES>,
        services: &mut S,
        identity: &I,
        config: &C,
        tx: &mut [u8; MTU],
        now_ms: u32,
        node_id: u64,
        source: u64,
        publication: &pubsub::Publication<'_>,
    ) -> Option<usize> {
        let Lookup::Call {
            name,
            handler,
            header,
            data,
            ..
        } = table.lookup(publication.topic, publication.data)
        else {
            return None;
        };
        let dst = BmIpAddr::GLOBAL_MULTICAST;
        let src = udp::source_address(node_id, &dst);
        let topic_len = name.len() + service_wire::REPLY_SUFFIX.len();
        let head = pubsub::HEADER_LEN + topic_len;
        udp::build_with(tx, &src, &dst, pubsub::PORT, pubsub::PORT, |buf| {
            let body = buf
                .get_mut(head..head + service_wire::MAX_DATA_SIZE)
                .ok_or(BmWireError::Truncated)?;
            body.fill(0);
            let (reply_header, reply) = body.split_at_mut(ReplyHeader::LEN);
            let len = match handler {
                ServiceHandler::Echo => service_wire::echo(data, reply),
                ServiceHandler::SysInfo => {
                    let info = SysInfoReply::new(
                        node_id,
                        identity.git_sha(),
                        sys_config_crc(config),
                        identity.app_name(),
                    );
                    sys_info::handle(data, &info, reply)
                }
                ServiceHandler::ConfigMap => config_map::handle(
                    data,
                    node_id,
                    |partition, out| partition_map(config, partition, out),
                    reply,
                ),
                ServiceHandler::PowerInfo => {
                    power_info::handle(data, || services.power_info(), reply)
                }
                // `bm_ticks_to_ms(bm_get_tick_count())`: the C's uptime is
                // its tick count, which is what `now_ms` counts from.
                ServiceHandler::Metrics => services
                    .metrics(|components| metrics::handle(node_id, now_ms, components, reply)),
                ServiceHandler::Application => services.handle(name, data, reply),
            }
            .filter(|len| *len <= reply.len())
            .ok_or(BmWireError::Invalid)?;
            ReplyHeader {
                target_node_id: source,
                id: header.id,
                data_size: len as u32,
            }
            .encode(reply_header)?;
            // `bm_pub_wl`'s header, type 0 and `BM_COMMON_PUB_SUB_VERSION`.
            buf[..pubsub::HEADER_LEN].copy_from_slice(&[
                0,
                0,
                topic_len as u8,
                0,
                pubsub::COMMON_VERSION,
            ]);
            service_wire::topic(
                &mut buf[pubsub::HEADER_LEN..head],
                name,
                service_wire::REPLY_SUFFIX,
            )?;
            Ok(head + ReplyHeader::LEN + len)
        })
        .ok()
    }

    /// A local delivery of a publication this node made: each application
    /// callback on a matching subscription, from this node's own id, and
    /// the reply callback once. Service callbacks are not called.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn deliver_locally(
        subscriptions: &SubscriptionsView<RESOURCE_NAME_BYTES>,
        requests: &mut ServiceRequests,
        power_info: &mut PowerInfoCallbacks,
        node_id: u64,
        topic: &[u8],
        kind: u8,
        version: u8,
        data: &[u8],
        events: &mut impl FnMut(Event<'_>),
    ) {
        let mut replied = false;
        for (subscription, callbacks) in subscriptions.matching_callbacks(topic) {
            for callback in callbacks {
                match callback {
                    Subscriber::Application => events(Event::Publication {
                        source: node_id,
                        subscription,
                        topic,
                        kind,
                        version,
                        data,
                    }),
                    Subscriber::Reply if !replied => {
                        replied = true;
                        Self::answer_request(requests, power_info, node_id, data, events);
                    }
                    Subscriber::Reply | Subscriber::Service => {}
                }
            }
        }
    }

    /// `_service_request_cb`: report the request `body` answers, if any.
    pub(super) fn answer_request(
        requests: &mut ServiceRequests,
        power_info: &mut PowerInfoCallbacks,
        node_id: u64,
        body: &[u8],
        events: &mut impl FnMut(Event<'_>),
    ) {
        if let ReplyOutcome::Answered { request, data } = requests.on_reply(node_id, body) {
            match power_info.on_end(request.id(), Some(data)) {
                Ended::Other => events(Event::ServiceReply {
                    id: request.id(),
                    service: request.service(),
                    data,
                }),
                ended => report_power_info(ended, events),
            }
        }
    }

    /// List the service `name`, answered by [`Services::handle`] —
    /// `bm_service_register`.
    ///
    /// Appends `name` to the list, then subscribes the service layer to
    /// `<name>/req` as [`Node::subscribe`] does, `SUB` resource included. A
    /// name already listed is listed again (divergence #89). Requests then
    /// arrive through [`Node::on_frame`], and the reply comes back in
    /// [`Owed::reply`].
    ///
    /// # Errors
    ///
    /// [`RegisterError::Full`] with nothing changed;
    /// [`RegisterError::Subscribe`] with the service listed.
    pub fn register_service(&mut self, name: &[u8]) -> Result<(), RegisterError> {
        self.list_service(name, ServiceHandler::Application)
    }

    /// List the echo service, `<node id>/echo` — `echo_service_init`. A
    /// request's data comes back as its reply.
    ///
    /// # Errors
    ///
    /// As [`Node::register_service`].
    pub fn register_echo_service(&mut self) -> Result<(), RegisterError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 21 bytes.
        let len = service_wire::service_name(&mut name, self.identity.node_id(), b"/echo")
            .map_err(|_| RegisterError::Full)?;
        self.list_service(&name[..len], ServiceHandler::Echo)
    }

    /// List the sys_info service, `<node id>/sys_info` —
    /// `sys_info_service_init`. An empty request is answered with a
    /// [`SysInfoReply`] of this node's id, [`Identity::git_sha`],
    /// [`Identity::app_name`] and `sys_config_crc`: the CRC-32 of the system
    /// partition as a CBOR map,
    /// [`bm_wire::configuration::ConfigPartition::cbor_map_crc32`]. A node
    /// with [`NoConfig`] sends an empty partition's.
    ///
    /// # Errors
    ///
    /// As [`Node::register_service`].
    pub fn register_sys_info_service(&mut self) -> Result<(), RegisterError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 25 bytes.
        let len = service_wire::service_name(&mut name, self.identity.node_id(), sys_info::SUFFIX)
            .map_err(|_| RegisterError::Full)?;
        self.list_service(&name[..len], ServiceHandler::SysInfo)
    }

    /// [`Node::sys_info_request_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn sys_info_request(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        timeout_s: u32,
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.sys_info_request_with(now_ms, target_node_id, timeout_s, |_| {})
    }

    /// Ask `target_node_id` for its sys_info — `sys_info_service_request`:
    /// [`Node::service_request_with`] to `<target>/sys_info` with no data.
    ///
    /// The answer is [`Event::ServiceReply`] with that service;
    /// [`bm_wire::service::sys_info::DecodedSysInfoReply::decode_into`] reads
    /// its data.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn sys_info_request_with(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        timeout_s: u32,
        events: impl FnMut(Event<'_>),
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 25 bytes.
        let len = service_wire::service_name(&mut name, target_node_id, sys_info::SUFFIX)
            .map_err(|_| ServiceRequestError::Full)?;
        self.service_request_with(now_ms, &name[..len], &[], timeout_s, events)
    }

    /// List the config_map service, `<node id>/config_map` —
    /// `config_cbor_map_service_init`. A request names a partition; the
    /// reply carries it as a CBOR map,
    /// [`bm_wire::configuration::ConfigPartition::cbor_map`].
    /// [`bm_wire::service::config_map::handle`] lists the outcomes. A node
    /// with [`NoConfig`] sends an empty map for each partition.
    ///
    /// # Errors
    ///
    /// As [`Node::register_service`].
    pub fn register_config_map_service(&mut self) -> Result<(), RegisterError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 27 bytes.
        let len =
            service_wire::service_name(&mut name, self.identity.node_id(), config_map::SUFFIX)
                .map_err(|_| RegisterError::Full)?;
        self.list_service(&name[..len], ServiceHandler::ConfigMap)
    }

    /// [`Node::config_map_request_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn config_map_request(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition_id: u32,
        timeout_s: u32,
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.config_map_request_with(now_ms, target_node_id, partition_id, timeout_s, |_| {})
    }

    /// Ask `target_node_id` for a configuration partition as a CBOR map —
    /// `config_cbor_map_service_request`: [`Node::service_request_with`] to
    /// `<target>/config_map` with a [`ConfigMapRequest`].
    /// `config_map::PARTITION_ID_*` are the ids a node answers.
    ///
    /// The answer is [`Event::ServiceReply`] with that service;
    /// [`bm_wire::service::config_map::DecodedConfigMapReply::decode_into`]
    /// reads its data.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn config_map_request_with(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition_id: u32,
        timeout_s: u32,
        events: impl FnMut(Event<'_>),
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 27 bytes.
        let len = service_wire::service_name(&mut name, target_node_id, config_map::SUFFIX)
            .map_err(|_| ServiceRequestError::Full)?;
        // Cannot fail: at most 19 bytes.
        let mut data = [0u8; 32];
        let data_len = ConfigMapRequest { partition_id }
            .encode(&mut data)
            .map_err(|_| ServiceRequestError::TooLarge)?;
        self.service_request_with(now_ms, &name[..len], &data[..data_len], timeout_s, events)
    }

    /// List the power_info service, `bus_power_controller/timing` —
    /// `power_info_service_init`. An empty request is answered with
    /// [`Services::power_info`]; see [`bm_wire::service::power_info::handle`].
    ///
    /// The name carries no node id, so every node listing it answers a
    /// request. The C refuses a NULL callback; here a node whose
    /// [`Services::power_info`] returns `None` lists the service and sends no
    /// reply.
    ///
    /// # Errors
    ///
    /// As [`Node::register_service`].
    pub fn register_power_info_service(&mut self) -> Result<(), RegisterError> {
        self.list_service(power_info::SERVICE, ServiceHandler::PowerInfo)
    }

    /// [`Node::power_info_request_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn power_info_request(
        &mut self,
        now_ms: u32,
        timeout_s: u32,
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.power_info_request_with(now_ms, timeout_s, |_| {})
    }

    /// Ask the bus for its power timing — `power_info_service_request`:
    /// queue a callback for the request, then [`Node::service_request_with`]
    /// to `bus_power_controller/timing` with no data. Every node listing the
    /// service answers; the first reply ends the request.
    ///
    /// The answer is [`Event::PowerInfoReply`], not
    /// [`Event::ServiceReply`]; an expiry reports nothing. Answers pair with
    /// requests in the order the requests were made, not by id (divergence
    /// #96).
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`]. [`ServiceRequestError::Full`]
    /// queues no callback: the C's equivalent is `queue_cb_enqueue` failing.
    pub fn power_info_request_with(
        &mut self,
        now_ms: u32,
        timeout_s: u32,
        events: impl FnMut(Event<'_>),
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.request_with(now_ms, power_info::SERVICE, &[], timeout_s, true, events)
    }

    /// List the metrics service, `<node id>/metrics` — `metrics_service_init`,
    /// which [`Node::new`] calls if [`Services::METRICS`].
    pub(super) fn register_metrics_service(&mut self) -> Result<(), RegisterError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 24 bytes.
        let len = service_wire::service_name(&mut name, self.identity.node_id(), metrics::SUFFIX)
            .map_err(|_| RegisterError::Full)?;
        self.list_service(&name[..len], ServiceHandler::Metrics)
    }

    /// [`Node::metrics_request_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn metrics_request(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        timeout_s: u32,
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.metrics_request_with(now_ms, target_node_id, timeout_s, |_| {})
    }

    /// Ask `target_node_id` for its metrics — `metrics_service_request`:
    /// [`Node::service_request_with`] to `<target>/metrics` with no data.
    ///
    /// The answer is [`Event::ServiceReply`] with that service;
    /// [`bm_wire::service::metrics::decode`] reads its data.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn metrics_request_with(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        timeout_s: u32,
        events: impl FnMut(Event<'_>),
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        let mut name = [0u8; SERVICE_NAME_BYTES];
        // Cannot fail: 24 bytes.
        let len = service_wire::service_name(&mut name, target_node_id, metrics::SUFFIX)
            .map_err(|_| ServiceRequestError::Full)?;
        self.service_request_with(now_ms, &name[..len], &[], timeout_s, events)
    }

    fn list_service(&mut self, name: &[u8], handler: ServiceHandler) -> Result<(), RegisterError> {
        self.service_table
            .add(name, handler)
            .map_err(|_| RegisterError::Full)?;
        let mut topic = [0u8; SERVICE_NAME_BYTES + service_wire::REQUEST_SUFFIX.len()];
        // Cannot fail: the table holds no longer name.
        let len = service_wire::topic(&mut topic, name, service_wire::REQUEST_SUFFIX)
            .map_err(|_| RegisterError::Full)?;
        self.subscribe_as(&topic[..len], Subscriber::Service)
            .map_err(RegisterError::Subscribe)
    }

    /// Unlist a service — `bm_service_unregister`.
    ///
    /// Unsubscribes the service layer from `<name>/req`, then removes the
    /// first listed service whose name starts with `name`, which need not be
    /// `name` itself (divergence #89).
    ///
    /// # Errors
    ///
    /// [`UnregisterError::Unsubscribe`] with nothing changed;
    /// [`UnregisterError::NotListed`] when unsubscribed but nothing was
    /// removed.
    pub fn unregister_service(&mut self, name: &[u8]) -> Result<(), UnregisterError> {
        let mut topic = [0u8; SERVICE_NAME_BYTES + service_wire::REQUEST_SUFFIX.len()];
        let topic = match service_wire::topic(&mut topic, name, service_wire::REQUEST_SUFFIX) {
            Ok(len) => &topic[..len],
            // Longer than any topic a service subscribes.
            Err(_) => {
                let len = name.len() + service_wire::REQUEST_SUFFIX.len();
                return Err(UnregisterError::Unsubscribe(
                    if len >= pubsub::TOPIC_MAX_LEN {
                        SubscriptionError::TopicTooLong
                    } else {
                        SubscriptionError::NotSubscribed
                    },
                ));
            }
        };
        self.subscriptions
            .unsubscribe_as(topic, Subscriber::Service)
            .map_err(UnregisterError::Unsubscribe)?;
        if self.service_table.remove(name) {
            Ok(())
        } else {
            Err(UnregisterError::NotListed)
        }
    }

    /// [`Node::service_request_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::service_request_with`].
    pub fn service_request(
        &mut self,
        now_ms: u32,
        service: &[u8],
        data: &[u8],
        timeout_s: u32,
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.service_request_with(now_ms, service, data, timeout_s, |_| {})
    }

    /// Ask `service` — `bm_service_request`. Returns the request's id and
    /// the frame to send.
    ///
    /// In the C's order:
    ///
    /// 1. the request is listed in [`Node::service_requests`] with the next
    ///    id, made at `now_ms`, timing out `timeout_s` seconds later, wrapped
    ///    to 32 bits of milliseconds (divergence #91);
    /// 2. the service request layer subscribes `<service>/rep`, as
    ///    [`Node::subscribe`] does, `SUB` resource included;
    /// 3. `<service>/req` is published as [`Node::publish_with`] publishes,
    ///    carrying a [`RequestHeader`] and `data`, with type 0 and
    ///    [`pubsub::COMMON_VERSION`]. A local service is not called (see the
    ///    module docs).
    ///
    /// The reply arrives as [`Event::ServiceReply`] from
    /// [`Node::on_frame_with`]; silence as [`Event::ServiceTimeout`] from
    /// [`Node::on_service_expiry`].
    ///
    /// # Errors
    ///
    /// See [`ServiceRequestError`]. After
    /// [`ServiceRequestError::NotSubscribed`] and
    /// [`ServiceRequestError::NotSent`] the request stays listed and times
    /// out, as in the C (divergence #91).
    pub fn service_request_with(
        &mut self,
        now_ms: u32,
        service: &[u8],
        data: &[u8],
        timeout_s: u32,
        events: impl FnMut(Event<'_>),
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        self.request_with(now_ms, service, data, timeout_s, false, events)
    }

    /// [`Node::service_request_with`], queueing a power_info callback for the
    /// request first if `power_info`.
    fn request_with(
        &mut self,
        now_ms: u32,
        service: &[u8],
        data: &[u8],
        timeout_s: u32,
        power_info: bool,
        mut events: impl FnMut(Event<'_>),
    ) -> Result<(u32, Outbound<'_>), ServiceRequestError> {
        if data.len() > service_wire::MAX_DATA_SIZE {
            return Err(ServiceRequestError::TooLarge);
        }
        let id = self
            .service_requests
            .add(service, timeout_s, now_ms)
            .map_err(|_| ServiceRequestError::Full)?;
        if power_info {
            // Cannot fail: one callback per waiting request, and `add` found
            // a request's room.
            let _ = self.power_info.push(id);
        }
        let mut topic = [0u8; SERVICE_NAME_BYTES + service_wire::REPLY_SUFFIX.len()];
        // Cannot fail: `add` holds no longer name.
        let len = service_wire::topic(&mut topic, service, service_wire::REPLY_SUFFIX)
            .map_err(|_| ServiceRequestError::NotSent { id })?;
        self.subscribe_as(&topic[..len], Subscriber::Reply)
            .map_err(|error| ServiceRequestError::NotSubscribed { id, error })?;
        let len = service_wire::topic(&mut topic, service, service_wire::REQUEST_SUFFIX)
            .map_err(|_| ServiceRequestError::NotSent { id })?;
        let topic = &topic[..len];

        let node_id = self.identity.node_id();
        let dst = BmIpAddr::GLOBAL_MULTICAST;
        let src = udp::source_address(node_id, &dst);
        let header = RequestHeader {
            id,
            data_size: data.len() as u32,
        };
        // At most 52 bytes of topic and 1032 of body: within
        // `MAX_MESSAGE_LEN`, so the C's `bm_middleware_net_tx` sends it too.
        let end = udp::build_with(
            &mut self.tx[..],
            &src,
            &dst,
            pubsub::PORT,
            pubsub::PORT,
            |buf| {
                let head = pubsub::encode(buf, topic, 0, pubsub::COMMON_VERSION, &[])?;
                let body = buf
                    .get_mut(head..head + RequestHeader::LEN + data.len())
                    .ok_or(BmWireError::Truncated)?;
                header.encode(body)?;
                body[RequestHeader::LEN..].copy_from_slice(data);
                Ok(head + body.len())
            },
        )
        .map_err(|_| ServiceRequestError::NotSent { id })?;

        let payload = &self.tx[udp::PAYLOAD_OFFSET..end];
        let request = pubsub::decode(payload).map_err(|_| ServiceRequestError::NotSent { id })?;
        Self::deliver_locally(
            self.subscriptions,
            &mut self.service_requests,
            &mut self.power_info,
            node_id,
            request.topic,
            request.kind,
            request.version,
            request.data,
            &mut events,
        );
        let _ = self.resources.add(topic, ResourceType::Publisher);
        let frame = &mut self.tx[..end];
        let mask = l2::take_requested_egress_port(frame, self.port_count)
            .map_err(|_| ServiceRequestError::NotSent { id })?;
        Ok((id, Outbound { frame, mask }))
    }

    /// Run `bm_service_request.c`'s expiry sweep, reporting each request that
    /// expired as [`Event::ServiceTimeout`].
    ///
    /// Call it at least every [`service_wire::EXPIRY_PERIOD_MS`]. The sweep
    /// runs on that grid from construction, as the C's timer runs from
    /// `bm_service_request_init`, and a request expires at the first sweep at
    /// least its timeout after it was made. Calling early, late or twice
    /// changes nothing.
    pub fn on_service_expiry(&mut self, now_ms: u32, mut events: impl FnMut(Event<'_>)) {
        let power_info = &mut self.power_info;
        self.service_requests.on_tick(now_ms, |request| {
            match power_info.on_end(request.id(), None) {
                Ended::Other => events(Event::ServiceTimeout {
                    id: request.id(),
                    service: request.service(),
                }),
                ended => report_power_info(ended, &mut events),
            }
        });
    }

    /// `CTX.service_request_list`: the requests waiting on a reply.
    pub fn service_requests(&self) -> &ServiceRequests {
        &self.service_requests
    }

    /// `power_info_service.c`'s callback queue, and which waiting requests
    /// [`Node::power_info_request`] made.
    pub fn power_info_callbacks(&self) -> &PowerInfoCallbacks {
        &self.power_info
    }

    /// `BM_SERVICE_CONTEXT.service_list`: the services listed, in the order a
    /// request is matched against them.
    pub fn service_table(&self) -> &ServiceTable<ServiceHandler, SERVICES, SERVICE_NAME_BYTES> {
        &self.service_table
    }

    /// The application's service handlers.
    pub fn services(&self) -> &S {
        &self.services
    }

    /// The application's service handlers, mutably.
    pub fn services_mut(&mut self) -> &mut S {
        &mut self.services
    }
}
