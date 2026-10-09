//! Pub/sub: `middleware/pubsub.c`'s subscribe, unsubscribe and publish,
//! Spotter's two topics, and delivering a received publication.

use bm_wire::bcmp::resource::{RESOURCE_NAME_BYTES, ResourceAddError, ResourceType};
use bm_wire::l2;
use bm_wire::pubsub::{self, Subscriber, SubscriptionError, SubscriptionsView};
use bm_wire::service as service_wire;
use bm_wire::spotter::{self, EncodeError, NetworkType};
use bm_wire::udp;
use bm_wire::util::BmIpAddr;

use crate::config::Configuration;
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};
use crate::service::{SERVICE_NAME_BYTES, Services};

use super::{Event, Node, Outbound, PublishError, SpotterError, SubscribeError};

#[cfg(doc)]
use super::NodeResources;
#[cfg(doc)]
use bm_wire::pubsub::Subscriptions;

/// The body buffers are sized for the largest body, so
/// [`EncodeError::Truncated`] does not occur.
fn spotter_error(e: EncodeError) -> SpotterError {
    match e {
        EncodeError::NoData => SpotterError::NoData,
        EncodeError::MessageSize | EncodeError::Truncated => SpotterError::MessageSize,
    }
}

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// Subscribe to `topic` — `bm_sub_wl`.
    ///
    /// Publications matching it arrive as [`Event::Publication`]. `topic` may
    /// hold `*` and `?`, and matches every topic it prefixes (divergence #74).
    /// Subscribing to a topic already subscribed changes nothing, unless the
    /// service layer subscribed it first: then the application is listed
    /// again, and hears each publication once more (divergence #79). Either
    /// way `topic` is then advertised as a `SUB` resource, which a topic
    /// already covered does not change (divergence #38).
    ///
    /// # Errors
    ///
    /// [`SubscribeError::Refused`] with nothing changed, for an empty topic,
    /// one of [`pubsub::TOPIC_MAX_LEN`] bytes or more, one longer than
    /// [`RESOURCE_NAME_BYTES`], [`NodeResources`]' `SUBSCRIPTIONS` already held, or
    /// [`pubsub::CALLBACKS`] on the topic.
    /// [`SubscribeError::NotAdvertised`] when subscribed but the resource
    /// table is full.
    pub fn subscribe(&mut self, topic: &[u8]) -> Result<(), SubscribeError> {
        self.subscribe_as(topic, Subscriber::Application)
    }

    pub(super) fn subscribe_as(
        &mut self,
        topic: &[u8],
        subscriber: Subscriber,
    ) -> Result<(), SubscribeError> {
        self.subscriptions
            .subscribe_as(topic, subscriber)
            .map_err(SubscribeError::Refused)?;
        match self.resources.add(topic, ResourceType::Subscriber) {
            Ok(()) | Err(ResourceAddError::AlreadyPresent) => Ok(()),
            Err(ResourceAddError::Full) => Err(SubscribeError::NotAdvertised),
        }
    }

    /// Unsubscribe from `topic` — `bm_unsub_wl`. The `SUB` resource stays: the
    /// C's resource lists have no remove.
    ///
    /// # Errors
    ///
    /// As [`Subscriptions::unsubscribe`]; nothing changes.
    /// [`SubscriptionError::NoSuchSubscriber`] for a topic only a service
    /// subscribed.
    pub fn unsubscribe(&mut self, topic: &[u8]) -> Result<(), SubscriptionError> {
        self.subscriptions.unsubscribe(topic)
    }

    /// The topics subscribed, in the order publications reach them.
    pub fn subscriptions(&self) -> &SubscriptionsView<RESOURCE_NAME_BYTES> {
        self.subscriptions
    }

    /// [`Node::publish_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::publish_with`].
    pub fn publish(
        &mut self,
        topic: &[u8],
        kind: u8,
        version: u8,
        data: &[u8],
    ) -> Result<Outbound<'_>, PublishError> {
        self.publish_with(topic, kind, version, data, |_| {})
    }

    /// Publish `data` on `topic` — `bm_pub_wl`.
    ///
    /// In the C's order:
    ///
    /// 1. each application callback on this node's subscriptions matching
    ///    `topic` is reported to `events` as [`Event::Publication`] from this
    ///    node's own id, and a service request callback may report
    ///    [`Event::ServiceReply`]. A service callback is not called: the C
    ///    queues the publication to its own middleware task, which would;
    /// 2. the publication is built as [`pubsub::encode`] into a datagram to
    ///    `FF03::1` from and to [`pubsub::PORT`], as [`Node::send_udp`] builds
    ///    one;
    /// 3. `topic` is advertised as a `PUB` resource, ignoring a refusal as
    ///    `bm_pub_wl` does.
    ///
    /// `kind` and `version` are `ext_header.type` and `.version`; bm_core's
    /// own messages use 1 and [`pubsub::COMMON_VERSION`].
    ///
    /// # Errors
    ///
    /// See [`PublishError`]: only [`PublishError::MessageTooLong`] comes after
    /// the local deliveries.
    pub fn publish_with(
        &mut self,
        topic: &[u8],
        kind: u8,
        version: u8,
        data: &[u8],
        mut events: impl FnMut(Event<'_>),
    ) -> Result<Outbound<'_>, PublishError> {
        pubsub::check_topic(topic).map_err(|e| match e {
            SubscriptionError::EmptyTopic => PublishError::EmptyTopic,
            _ => PublishError::TopicTooLong,
        })?;
        let node_id = self.identity.node_id();
        Self::deliver_locally(
            self.subscriptions,
            &mut self.service_requests,
            &mut self.power_info,
            node_id,
            topic,
            kind,
            version,
            data,
            &mut events,
        );
        if pubsub::HEADER_LEN + topic.len() + data.len() > pubsub::MAX_MESSAGE_LEN {
            return Err(PublishError::MessageTooLong);
        }
        let dst = BmIpAddr::GLOBAL_MULTICAST;
        let src = udp::source_address(node_id, &dst);
        // Cannot fail: MAX_MESSAGE_LEN is what fits in MTU after the headers.
        let end = udp::build_with(
            &mut self.tx[..],
            &src,
            &dst,
            pubsub::PORT,
            pubsub::PORT,
            |buf| pubsub::encode(buf, topic, kind, version, data),
        )
        .map_err(|_| PublishError::MessageTooLong)?;
        let _ = self.resources.add(topic, ResourceType::Publisher);
        let frame = &mut self.tx[..end];
        let mask = l2::take_requested_egress_port(frame, self.port_count)
            .map_err(|_| PublishError::MessageTooLong)?;
        Ok(Outbound { frame, mask })
    }

    /// [`Node::spotter_log_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::spotter_log_with`].
    pub fn spotter_log(
        &mut self,
        target_node_id: u64,
        file_name: Option<&[u8]>,
        print_time: u8,
        text: &[u8],
    ) -> Result<Outbound<'_>, SpotterError> {
        self.spotter_log_with(target_node_id, file_name, print_time, text, |_| {})
    }

    /// Publish a line for the Spotter — `spotter_log`, taking the formatted
    /// text: format into a buffer with [`core::fmt::Write`] first.
    ///
    /// [`spotter::encode_log`] of the arguments, published as
    /// [`Node::publish_with`] does to [`spotter::log_topic`] of `file_name`
    /// with [`spotter::KIND`] and [`pubsub::COMMON_VERSION`].
    /// `spotter_log_console` is `file_name` `None` and `print_time`
    /// [`spotter::USE_TIMESTAMP`]. `target_node_id` 0 is every Spotter.
    ///
    /// The body is built in a [`spotter::MAX_LOG_LEN`]-byte buffer on the
    /// stack for the length of the call.
    ///
    /// # Errors
    ///
    /// See [`SpotterError`]: only [`SpotterError::NotSent`] comes after the
    /// local deliveries.
    pub fn spotter_log_with(
        &mut self,
        target_node_id: u64,
        file_name: Option<&[u8]>,
        print_time: u8,
        text: &[u8],
        events: impl FnMut(Event<'_>),
    ) -> Result<Outbound<'_>, SpotterError> {
        let mut body = [0u8; spotter::MAX_LOG_LEN];
        let len = spotter::encode_log(&mut body, target_node_id, file_name, print_time, text)
            .map_err(spotter_error)?;
        self.publish_spotter(spotter::log_topic(file_name), &body[..len], events)
    }

    /// [`Node::spotter_tx_data_with`], discarding local deliveries.
    ///
    /// # Errors
    ///
    /// As [`Node::spotter_tx_data_with`].
    pub fn spotter_tx_data(
        &mut self,
        data: &[u8],
        network: NetworkType,
    ) -> Result<Outbound<'_>, SpotterError> {
        self.spotter_tx_data_with(data, network, |_| {})
    }

    /// Ask the Spotter to send `data` over satellite or cellular —
    /// `spotter_tx_data`.
    ///
    /// [`spotter::encode_tx_data`], published as [`Node::publish_with`] does
    /// to [`spotter::TRANSMIT_DATA_TOPIC`] with [`spotter::KIND`] and
    /// [`pubsub::COMMON_VERSION`]. The body is built in a
    /// [`spotter::MAX_TX_LEN`]-byte buffer on the stack.
    ///
    /// # Errors
    ///
    /// [`SpotterError::MessageSize`] if `data` is longer than
    /// [`NetworkType::max_len`]. Anything shorter fits a publication.
    pub fn spotter_tx_data_with(
        &mut self,
        data: &[u8],
        network: NetworkType,
        events: impl FnMut(Event<'_>),
    ) -> Result<Outbound<'_>, SpotterError> {
        let mut body = [0u8; spotter::MAX_TX_LEN];
        let len = spotter::encode_tx_data(&mut body, network, data).map_err(spotter_error)?;
        self.publish_spotter(spotter::TRANSMIT_DATA_TOPIC, &body[..len], events)
    }

    fn publish_spotter(
        &mut self,
        topic: &[u8],
        body: &[u8],
        events: impl FnMut(Event<'_>),
    ) -> Result<Outbound<'_>, SpotterError> {
        // Only `MessageTooLong` is reachable: the topics are valid.
        self.publish_with(topic, spotter::KIND, pubsub::COMMON_VERSION, body, events)
            .map_err(|_| SpotterError::NotSent)
    }

    /// `bm_handle_msg`: every callback on every matching subscription, in
    /// list order. An application callback is an [`Event::Publication`]; the
    /// service callback is `_service_request_received_cb`, which may build a
    /// reply into the transmit buffer; the reply callback is
    /// `_service_request_cb`, which may report an [`Event::ServiceReply`].
    ///
    /// The service callback runs once per publication, however often it is
    /// listed on matching subscriptions. Each of the C's calls walks the same
    /// list with the same topic, so each reaches the same handler and
    /// publishes the same reply; a C requester takes the first and drops the
    /// rest (divergence #89). The reply callback runs once too: the C's first
    /// call removes the request the reply answers, and the rest find none.
    pub(super) fn deliver_publication(
        &mut self,
        now_ms: u32,
        source: u64,
        publication: &pubsub::Publication<'_>,
        events: &mut impl FnMut(Event<'_>),
    ) -> Option<Outbound<'_>> {
        let node_id = self.identity.node_id();
        let mut served = false;
        let mut replied = false;
        let mut reply_end = None;
        for (subscription, callbacks) in self.subscriptions.matching_callbacks(publication.topic) {
            for callback in callbacks {
                match callback {
                    Subscriber::Application => events(Event::Publication {
                        source,
                        subscription,
                        topic: publication.topic,
                        kind: publication.kind,
                        version: publication.version,
                        data: publication.data,
                    }),
                    Subscriber::Service if !served => {
                        served = true;
                        reply_end = Self::serve(
                            &self.service_table,
                            &mut self.services,
                            &self.identity,
                            &self.config,
                            self.tx,
                            now_ms,
                            node_id,
                            source,
                            publication,
                        );
                    }
                    Subscriber::Service => {}
                    Subscriber::Reply if !replied => {
                        replied = true;
                        Self::answer_request(
                            &mut self.service_requests,
                            &mut self.power_info,
                            node_id,
                            publication.data,
                            events,
                        );
                    }
                    Subscriber::Reply => {}
                }
            }
        }
        let end = reply_end?;

        // `bm_pub_wl` of the reply: local delivery, then the resource.
        let payload = &self.tx[udp::PAYLOAD_OFFSET..end];
        let reply = pubsub::decode(payload).ok()?;
        Self::deliver_locally(
            self.subscriptions,
            &mut self.service_requests,
            &mut self.power_info,
            node_id,
            reply.topic,
            reply.kind,
            reply.version,
            reply.data,
            events,
        );
        let mut topic = [0u8; SERVICE_NAME_BYTES + service_wire::REPLY_SUFFIX.len()];
        topic[..reply.topic.len()].copy_from_slice(reply.topic);
        let topic = &topic[..reply.topic.len()];
        let _ = self.resources.add(topic, ResourceType::Publisher);
        let frame = &mut self.tx[..end];
        let mask = l2::take_requested_egress_port(frame, self.port_count).ok()?;
        Some(Outbound { frame, mask })
    }
}
