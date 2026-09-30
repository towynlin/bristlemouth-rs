//! A [`NodeHandle`] over `embassy-sync` channels, for applications that run as
//! tasks of their own.
//!
//! [`Channels`] owns two queues. The application's task holds a
//! [`NodeHandle`] and sends [`Command`]s; the node's task runs a
//! [`ChannelApp`] through [`Node::run_app`], which receives them, and turns
//! [`Event`]s into owned [`Notification`]s going the other way.
//!
//! Both halves are ordinary [`App`] plumbing: the channel costs a `Command`
//! variant per app-facing call and a `Notification` variant per event, each
//! owning a copy of what the borrowed form points into.
//!
//! [`Node::run_app`]: crate::Node::run_app

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use heapless::Vec;

use bm_wire::bcmp::resource::RESOURCE_NAME_BYTES;
use bm_wire::util::BmIpAddr;

use crate::app::App;
use crate::config::Configuration;
use crate::node::{Event, Node, Outbound, PING_PAYLOAD_BYTES};
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};

/// Longest topic a [`Command`] or [`Notification`] carries: the default
/// `RESOURCE_NAME`, which bounds a [`Node`] subscription's topic.
pub const TOPIC_BYTES: usize = RESOURCE_NAME_BYTES;

/// Most data a [`Command::Publish`] or [`Notification::Publication`] carries.
/// An application publishing more uses [`App`] and [`Node::publish_with`]
/// directly. A received publication with more is counted by
/// [`ChannelApp::dropped`].
pub const DATA_BYTES: usize = 256;

/// Something an application asks the node to do.
///
/// Nothing reports whether a command succeeded; an application that needs to
/// know uses [`App`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(
    clippy::large_enum_variant,
    reason = "no allocator to box into; a queue slot is sized for the largest variant"
)]
pub enum Command {
    /// [`Node::ping`] to `FF02::1`.
    Ping {
        /// Node to answer, or zero for every node.
        target_node_id: u64,
        /// Echoed back in the reply.
        payload: Vec<u8, PING_PAYLOAD_BYTES>,
    },
    /// [`Node::publish_with`]; local deliveries come back as
    /// [`Notification::Publication`].
    Publish {
        /// The topic.
        topic: Vec<u8, TOPIC_BYTES>,
        /// `ext_header.type`.
        kind: u8,
        /// `ext_header.version`.
        version: u8,
        /// The data.
        data: Vec<u8, DATA_BYTES>,
    },
    /// [`Node::subscribe`].
    Subscribe {
        /// The topic, which may hold `*` and `?`.
        topic: Vec<u8, TOPIC_BYTES>,
    },
    /// [`Node::unsubscribe`].
    Unsubscribe {
        /// The topic.
        topic: Vec<u8, TOPIC_BYTES>,
    },
}

/// An [`Event`], owned, for sending to another task.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[allow(
    clippy::large_enum_variant,
    reason = "no allocator to box into; a queue slot is sized for the largest variant"
)]
pub enum Notification {
    /// [`Event::EchoReply`].
    EchoReply {
        /// Node id the reply came from.
        source: u64,
        /// The echoed sequence number.
        seq_num: u16,
        /// The echoed payload.
        payload: Vec<u8, PING_PAYLOAD_BYTES>,
        /// Milliseconds since the ping.
        round_trip_ms: u32,
    },
    /// [`Event::Publication`].
    Publication {
        /// Node id the publication came from.
        source: u64,
        /// The subscription that matched.
        subscription: Vec<u8, TOPIC_BYTES>,
        /// The publication's topic.
        topic: Vec<u8, { bm_wire::pubsub::TOPIC_MAX_LEN }>,
        /// `ext_header.type`.
        kind: u8,
        /// `ext_header.version`.
        version: u8,
        /// The data.
        data: Vec<u8, DATA_BYTES>,
    },
}

impl Notification {
    /// The owned form of `event`, or `None` for an event with no
    /// [`Notification`] yet or a publication with more than [`DATA_BYTES`].
    #[must_use]
    pub fn from_event(event: &Event<'_>) -> Option<Self> {
        match *event {
            Event::EchoReply {
                source,
                reply,
                round_trip_ms,
            } => Some(Self::EchoReply {
                source,
                seq_num: reply.seq_num,
                // A reply is only reported when it matches the node's stored
                // payload, which is at most `PING_PAYLOAD_BYTES` long.
                payload: Vec::from_slice(reply.payload).ok()?,
                round_trip_ms,
            }),
            Event::Publication {
                source,
                subscription,
                topic,
                kind,
                version,
                data,
            } => Some(Self::Publication {
                source,
                subscription: Vec::from_slice(subscription).ok()?,
                topic: Vec::from_slice(topic).ok()?,
                kind,
                version,
                data: Vec::from_slice(data).ok()?,
            }),
            _ => None,
        }
    }
}

/// The two queues between an application task and the node's task.
///
/// `M` is the mutex the channels lock with: `CriticalSectionRawMutex` when the
/// tasks run on different executors or priorities, `NoopRawMutex` when they
/// share one thread. `DEPTH` is the capacity of each queue.
pub struct Channels<M: RawMutex, const DEPTH: usize> {
    commands: Channel<M, Command, DEPTH>,
    notifications: Channel<M, Notification, DEPTH>,
}

impl<M: RawMutex, const DEPTH: usize> Channels<M, DEPTH> {
    /// Empty queues. `const`, so they can live in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            commands: Channel::new(),
            notifications: Channel::new(),
        }
    }

    /// The application's end.
    #[must_use]
    pub fn handle(&self) -> NodeHandle<'_, M, DEPTH> {
        NodeHandle {
            commands: self.commands.sender(),
            notifications: self.notifications.receiver(),
        }
    }

    /// The node's end, for [`Node::run_app`](crate::Node::run_app).
    #[must_use]
    pub fn app(&self) -> ChannelApp<'_, M, DEPTH> {
        ChannelApp {
            commands: self.commands.receiver(),
            notifications: self.notifications.sender(),
            next: None,
            dropped: 0,
        }
    }
}

impl<M: RawMutex, const DEPTH: usize> Default for Channels<M, DEPTH> {
    fn default() -> Self {
        Self::new()
    }
}

/// What an application task holds. `Copy`, so several tasks can share one.
pub struct NodeHandle<'a, M: RawMutex, const DEPTH: usize> {
    commands: Sender<'a, M, Command, DEPTH>,
    notifications: Receiver<'a, M, Notification, DEPTH>,
}

impl<M: RawMutex, const DEPTH: usize> Clone for NodeHandle<'_, M, DEPTH> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: RawMutex, const DEPTH: usize> Copy for NodeHandle<'_, M, DEPTH> {}

impl<M: RawMutex, const DEPTH: usize> NodeHandle<'_, M, DEPTH> {
    /// Queue a command, waiting while the queue is full.
    pub async fn send(&self, command: Command) {
        self.commands.send(command).await;
    }

    /// Ping `target_node_id`, or every node if zero.
    ///
    /// Returns `false` without queueing anything when `payload` is longer
    /// than [`PING_PAYLOAD_BYTES`].
    pub async fn ping(&self, target_node_id: u64, payload: &[u8]) -> bool {
        let Ok(payload) = Vec::from_slice(payload) else {
            return false;
        };
        self.send(Command::Ping {
            target_node_id,
            payload,
        })
        .await;
        true
    }

    /// Publish `data` on `topic`.
    ///
    /// Returns `false` without queueing anything when `topic` is longer than
    /// [`TOPIC_BYTES`] or `data` than [`DATA_BYTES`].
    pub async fn publish(&self, topic: &[u8], kind: u8, version: u8, data: &[u8]) -> bool {
        let (Ok(topic), Ok(data)) = (Vec::from_slice(topic), Vec::from_slice(data)) else {
            return false;
        };
        self.send(Command::Publish {
            topic,
            kind,
            version,
            data,
        })
        .await;
        true
    }

    /// Subscribe to `topic`.
    ///
    /// Returns `false` without queueing anything when `topic` is longer than
    /// [`TOPIC_BYTES`].
    pub async fn subscribe(&self, topic: &[u8]) -> bool {
        let Ok(topic) = Vec::from_slice(topic) else {
            return false;
        };
        self.send(Command::Subscribe { topic }).await;
        true
    }

    /// Unsubscribe from `topic`.
    ///
    /// Returns `false` without queueing anything when `topic` is longer than
    /// [`TOPIC_BYTES`].
    pub async fn unsubscribe(&self, topic: &[u8]) -> bool {
        let Ok(topic) = Vec::from_slice(topic) else {
            return false;
        };
        self.send(Command::Unsubscribe { topic }).await;
        true
    }

    /// The next notification, waiting for one.
    pub async fn notification(&self) -> Notification {
        self.notifications.receive().await
    }
}

/// The [`App`] that runs a [`Channels`]' node end.
pub struct ChannelApp<'a, M: RawMutex, const DEPTH: usize> {
    commands: Receiver<'a, M, Command, DEPTH>,
    notifications: Sender<'a, M, Notification, DEPTH>,
    /// Received by `ready`, carried out by `act`.
    next: Option<Command>,
    dropped: u32,
}

impl<M: RawMutex, const DEPTH: usize> ChannelApp<'_, M, DEPTH> {
    /// Notifications discarded because the queue was full, or publications
    /// with more than [`DATA_BYTES`]. The node never waits on the application,
    /// so a task that stops reading loses them.
    #[must_use]
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

/// Queue `event` as a [`Notification`], counting it in `dropped` if it is a
/// publication too large to own or the queue is full.
fn notify<M: RawMutex, const DEPTH: usize>(
    notifications: &Sender<'_, M, Notification, DEPTH>,
    dropped: &mut u32,
    event: &Event<'_>,
) {
    let sent = match Notification::from_event(event) {
        Some(notification) => notifications.try_send(notification).is_ok(),
        None => !matches!(event, Event::Publication { .. }),
    };
    if !sent {
        *dropped = dropped.wrapping_add(1);
    }
}

impl<
    M: RawMutex,
    const DEPTH: usize,
    I: Identity,
    R: Rtc,
    const NEIGHBORS: usize,
    const PENDING: usize,
    const PING_PAYLOAD: usize,
    const INFO_REQUESTS: usize,
    const INFO_STRINGS: usize,
    const RESOURCES: usize,
    const RESOURCE_NAME: usize,
    const RESOURCE_REQUESTS: usize,
    const SUBSCRIPTIONS: usize,
    C: Configuration,
    D: DfuSlot + NoInitRam,
>
    App<
        Node<
            I,
            R,
            NEIGHBORS,
            PENDING,
            PING_PAYLOAD,
            INFO_REQUESTS,
            INFO_STRINGS,
            RESOURCES,
            RESOURCE_NAME,
            RESOURCE_REQUESTS,
            SUBSCRIPTIONS,
            C,
            D,
        >,
    > for ChannelApp<'_, M, DEPTH>
{
    async fn ready(&mut self) {
        // Cancel-safe: `receive` takes nothing from the queue until it
        // resolves, and the result is stored in the same poll.
        if self.next.is_none() {
            self.next = Some(self.commands.receive().await);
        }
    }

    fn act<'n>(
        &mut self,
        node: &'n mut Node<
            I,
            R,
            NEIGHBORS,
            PENDING,
            PING_PAYLOAD,
            INFO_REQUESTS,
            INFO_STRINGS,
            RESOURCES,
            RESOURCE_NAME,
            RESOURCE_REQUESTS,
            SUBSCRIPTIONS,
            C,
            D,
        >,
        now_ms: u32,
    ) -> Option<Outbound<'n>> {
        match self.next.take()? {
            Command::Ping {
                target_node_id,
                payload,
            } => node.ping(
                now_ms,
                &BmIpAddr::LINK_LOCAL_MULTICAST,
                target_node_id,
                &payload,
            ),
            Command::Publish {
                topic,
                kind,
                version,
                data,
            } => {
                let (notifications, dropped) = (&self.notifications, &mut self.dropped);
                node.publish_with(&topic, kind, version, &data, |event| {
                    notify(notifications, dropped, &event);
                })
                .ok()
            }
            Command::Subscribe { topic } => {
                let _ = node.subscribe(&topic);
                None
            }
            Command::Unsubscribe { topic } => {
                let _ = node.unsubscribe(&topic);
                None
            }
        }
    }

    fn on_event(&mut self, event: Event<'_>) {
        notify(&self.notifications, &mut self.dropped, &event);
    }
}
