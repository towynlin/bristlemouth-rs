//! Pub/sub publications: the payload of a UDP datagram to [`PORT`].
//!
//! The C is `BmPubSubData` and `BmPubSubHeader` (`middleware/pubsub.h`), the
//! header fill in `bm_pub_wl` and the parse in `bm_handle_msg`
//! (`middleware/pubsub.c`). Topic matching is
//! [`crate::util::bm_wildcard_match`], which is `common/util.c`'s.
//!
//! | Offset | Field | `bm_pub_wl` writes |
//! |---|---|---|
//! | 0 | `type` | 0 |
//! | 1 | `flags` | 0 |
//! | 2 | `topic_len` | the topic's length |
//! | 3 | `ext_header.type` | the caller's `type` |
//! | 4 | `ext_header.version` | the caller's `version` |
//! | 5 | `topic` | `topic_len` bytes, no NUL |
//! | 5 + `topic_len` | data | to the end of the datagram |
//!
//! | C behaviour | Here | Divergence |
//! |---|---|---|
//! | A subscription matches any topic it prefixes, unless it holds a `*` | [`crate::util::bm_wildcard_match`] does the same | #74 |
//! | `bm_handle_msg` computes the data length unchecked; a `topic_len` past the payload wraps it and the callback reads out of bounds | [`decode`] refuses | #75 |
//! | `bm_pub_wl` sizes its buffer in a `uint16_t` that wraps, then copies past it | [`encode`] takes `usize` lengths and a caller buffer | #76 |
//!
//! [`Subscriptions`] is `CTX.subscription_list`, with three possible
//! callbacks per topic: the application, the service layer and the service
//! request layer.

use crate::BmWireError;

/// The UDP port publications are sent from and to, `resource_port`.
pub const PORT: u16 = 4321;

/// `sizeof(BmPubSubData)`.
pub const HEADER_LEN: usize = 5;

/// `BM_TOPIC_MAX_LEN`. `bm_pub_wl` and `bm_sub_wl` refuse a topic this long or
/// longer; a received `topic_len` is a `u8` and is not checked against it.
pub const TOPIC_MAX_LEN: usize = 255;

/// `BM_COMMON_PUB_SUB_VERSION`, the `version` of the messages in
/// `bm_common_messages`.
pub const COMMON_VERSION: u8 = 2;

/// `max_payload_len_udp` (`middleware/middleware.c`): the longest publication
/// `bm_middleware_net_tx` sends. `bm_pub_wl` refuses a longer one with
/// `BmEINVAL`, after delivering it to local subscribers.
pub const MAX_MESSAGE_LEN: usize = 1452;

/// A decoded publication, borrowing the datagram's payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Publication<'a> {
    /// `BmPubSubData::type`. `bm_pub_wl` writes 0; `bm_handle_msg` ignores it.
    pub header_type: u8,
    /// `BmPubSubData::flags`. `bm_pub_wl` writes 0; `bm_handle_msg` ignores it.
    pub flags: u8,
    /// The topic, `topic_len` bytes.
    pub topic: &'a [u8],
    /// `ext_header.type`, the callback's `type`.
    pub kind: u8,
    /// `ext_header.version`, the callback's `version`.
    pub version: u8,
    /// Everything after the topic.
    pub data: &'a [u8],
}

/// Write a publication as `bm_pub_wl` does, returning its length.
///
/// # Errors
///
/// - [`BmWireError::Invalid`] if `topic` is empty or at least
///   [`TOPIC_MAX_LEN`] bytes, where `bm_pub_wl` returns `BmEINVAL` or
///   `BmEMSGSIZE`.
/// - [`BmWireError::Truncated`] if `buf` cannot hold it.
pub fn encode(
    buf: &mut [u8],
    topic: &[u8],
    kind: u8,
    version: u8,
    data: &[u8],
) -> Result<usize, BmWireError> {
    if topic.is_empty() || topic.len() >= TOPIC_MAX_LEN {
        return Err(BmWireError::Invalid);
    }
    let len = HEADER_LEN + topic.len() + data.len();
    let out = buf.get_mut(..len).ok_or(BmWireError::Truncated)?;
    out[..HEADER_LEN].copy_from_slice(&[0, 0, topic.len() as u8, kind, version]);
    let (topic_out, data_out) = out[HEADER_LEN..].split_at_mut(topic.len());
    topic_out.copy_from_slice(topic);
    data_out.copy_from_slice(data);
    Ok(len)
}

/// Read a publication as `bm_handle_msg` does.
///
/// # Errors
///
/// [`BmWireError::Truncated`] if `payload` is shorter than the header, or than
/// the header and `topic_len`. `bm_handle_msg` reads past the payload in both
/// cases (divergence #75).
pub fn decode(payload: &[u8]) -> Result<Publication<'_>, BmWireError> {
    let [header_type, flags, topic_len, kind, version, rest @ ..] = payload else {
        return Err(BmWireError::Truncated);
    };
    if rest.len() < usize::from(*topic_len) {
        return Err(BmWireError::Truncated);
    }
    let (topic, data) = rest.split_at(usize::from(*topic_len));
    Ok(Publication {
        header_type: *header_type,
        flags: *flags,
        topic,
        kind: *kind,
        version: *version,
        data,
    })
}

/// Why [`Subscriptions`] refused, with the `BmErr` `bm_sub_wl` or
/// `bm_unsub_wl` returns for the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionError {
    /// The topic is empty: `BmEINVAL`.
    EmptyTopic,
    /// The topic is [`TOPIC_MAX_LEN`] bytes or longer: `BmEMSGSIZE`.
    TopicTooLong,
    /// `N` topics are held, the topic is longer than `TOPIC`, or the topic
    /// holds [`CALLBACKS`] callbacks: ceilings bm_core does not have. The C's
    /// nearest is `BmENOMEM` from `bm_malloc`.
    Full,
    /// [`Subscriptions::unsubscribe`] of a topic not subscribed. `bm_unsub_wl`
    /// returns `BmEINVAL`: its `err` is never reassigned on that path.
    NotSubscribed,
    /// [`Subscriptions::unsubscribe_as`] of a topic subscribed, but not by
    /// that subscriber: `BmENOENT`.
    NoSuchSubscriber,
}

/// A callback on a topic: what `BmPubSubNode::callback_fn` points at.
///
/// A node has three: its application, `bm_service.c`'s
/// `_service_request_received_cb`, which every service shares, and
/// `bm_service_request.c`'s `_service_request_cb`, which every request
/// shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscriber {
    /// The application, through `Event::Publication` in `bm-stack`.
    Application,
    /// The service layer, through `bm_wire::service::ServiceTable`.
    Service,
    /// The service request layer, on a reply topic, through
    /// `bm_wire::service::Requests`.
    Reply,
}

/// How many callbacks one topic holds, a ceiling bm_core does not have.
/// Divergence #79 is what fills a list past two.
pub const CALLBACKS: usize = 4;

#[derive(Debug, Clone, Copy)]
struct Entry<const TOPIC: usize> {
    topic: [u8; TOPIC],
    len: usize,
    callbacks: [Subscriber; CALLBACKS],
    callbacks_len: usize,
}

impl<const TOPIC: usize> Entry<TOPIC> {
    const EMPTY: Self = Self {
        topic: [0; TOPIC],
        len: 0,
        callbacks: [Subscriber::Application; CALLBACKS],
        callbacks_len: 0,
    };

    fn topic(&self) -> &[u8] {
        &self.topic[..self.len]
    }

    fn callbacks(&self) -> &[Subscriber] {
        &self.callbacks[..self.callbacks_len]
    }
}

/// `middleware/pubsub.c`'s `CTX.subscription_list`: topics, each with its
/// list of callbacks.
///
/// | C | Here |
/// |---|---|
/// | `bm_sub_wl` of a new topic appends it | [`Self::subscribe_as`] appends it |
/// | `bm_sub_wl` of a topic whose **first** callback is the one given: `BmOK`, no change | [`Self::subscribe_as`]: `Ok`, no change |
/// | `bm_sub_wl` of a topic whose first callback is another: appended, even if already listed (divergence #79) | [`Self::subscribe_as`] appends it |
/// | `bm_unsub_wl` removes the first matching callback, and deletes the topic with its last | [`Self::unsubscribe_as`] |
/// | `bm_handle_msg` calls every callback of every matching topic, in list order | [`Self::matching_callbacks`] |
/// | `get_sub(topic, len, true)`: whether any topic matches | [`Self::any_match`] |
///
/// Topics are compared exactly for subscribing, and matched with
/// [`bm_wildcard_match`](crate::util::bm_wildcard_match), publication topic
/// first, for delivery (divergence #74). Order is insertion order; a topic
/// unsubscribed and subscribed again goes to the end.
///
/// `N` is how many topics are held and `TOPIC` the longest; bm_core has
/// neither ceiling, nor [`CALLBACKS`].
#[derive(Debug, Clone)]
pub struct Subscriptions<const N: usize, const TOPIC: usize> {
    entries: [Entry<TOPIC>; N],
    len: usize,
}

impl<const N: usize, const TOPIC: usize> Default for Subscriptions<N, TOPIC> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const TOPIC: usize> Subscriptions<N, TOPIC> {
    /// No subscriptions.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [Entry::EMPTY; N],
            len: 0,
        }
    }

    /// How many topics are subscribed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no topic is subscribed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The subscribed topics, in list order.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> + '_ {
        self.entries[..self.len].iter().map(Entry::topic)
    }

    /// Whether `topic` itself is subscribed: `get_sub(topic, len, false)`.
    #[must_use]
    pub fn contains(&self, topic: &[u8]) -> bool {
        self.position(topic).is_some()
    }

    /// `topic`'s callbacks, in the order `bm_handle_msg` calls them, or
    /// `None` if it is not subscribed.
    #[must_use]
    pub fn callbacks(&self, topic: &[u8]) -> Option<&[Subscriber]> {
        self.position(topic).map(|i| self.entries[i].callbacks())
    }

    fn position(&self, topic: &[u8]) -> Option<usize> {
        self.iter().position(|held| held == topic)
    }

    /// Subscribe the application to `topic`:
    /// [`Self::subscribe_as`]`(topic, Subscriber::Application)`.
    ///
    /// # Errors
    ///
    /// As [`Self::subscribe_as`].
    pub fn subscribe(&mut self, topic: &[u8]) -> Result<(), SubscriptionError> {
        self.subscribe_as(topic, Subscriber::Application)
    }

    /// Subscribe `subscriber` to `topic`, as `bm_sub_wl` up to its
    /// resource-table call.
    ///
    /// # Errors
    ///
    /// [`SubscriptionError::EmptyTopic`], [`SubscriptionError::TopicTooLong`]
    /// or [`SubscriptionError::Full`]; nothing changes.
    pub fn subscribe_as(
        &mut self,
        topic: &[u8],
        subscriber: Subscriber,
    ) -> Result<(), SubscriptionError> {
        check_topic(topic)?;
        if let Some(index) = self.position(topic) {
            let entry = &mut self.entries[index];
            // Divergence #79: only the head is compared.
            if entry.callbacks[0] == subscriber {
                return Ok(());
            }
            if entry.callbacks_len == CALLBACKS {
                return Err(SubscriptionError::Full);
            }
            entry.callbacks[entry.callbacks_len] = subscriber;
            entry.callbacks_len += 1;
            return Ok(());
        }
        if self.len == N || topic.len() > TOPIC {
            return Err(SubscriptionError::Full);
        }
        let entry = &mut self.entries[self.len];
        entry.topic[..topic.len()].copy_from_slice(topic);
        entry.len = topic.len();
        entry.callbacks[0] = subscriber;
        entry.callbacks_len = 1;
        self.len += 1;
        Ok(())
    }

    /// Unsubscribe the application from `topic`:
    /// [`Self::unsubscribe_as`]`(topic, Subscriber::Application)`.
    ///
    /// # Errors
    ///
    /// As [`Self::unsubscribe_as`].
    pub fn unsubscribe(&mut self, topic: &[u8]) -> Result<(), SubscriptionError> {
        self.unsubscribe_as(topic, Subscriber::Application)
    }

    /// Unsubscribe `subscriber` from `topic`, as `bm_unsub_wl`: its first
    /// callback in the list goes, and the topic goes with its last.
    ///
    /// # Errors
    ///
    /// [`SubscriptionError::EmptyTopic`], [`SubscriptionError::TopicTooLong`],
    /// [`SubscriptionError::NotSubscribed`] or
    /// [`SubscriptionError::NoSuchSubscriber`]; nothing changes.
    pub fn unsubscribe_as(
        &mut self,
        topic: &[u8],
        subscriber: Subscriber,
    ) -> Result<(), SubscriptionError> {
        check_topic(topic)?;
        let index = self
            .position(topic)
            .ok_or(SubscriptionError::NotSubscribed)?;
        let entry = &mut self.entries[index];
        let at = entry
            .callbacks()
            .iter()
            .position(|held| *held == subscriber)
            .ok_or(SubscriptionError::NoSuchSubscriber)?;
        entry.callbacks[at..entry.callbacks_len].rotate_left(1);
        entry.callbacks_len -= 1;
        if entry.callbacks_len == 0 {
            self.entries[index..self.len].rotate_left(1);
            self.len -= 1;
        }
        Ok(())
    }

    /// The subscribed topics a publication on `topic` is delivered to, in the
    /// order `bm_handle_msg` visits them.
    pub fn matching<'a>(&'a self, topic: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.matching_callbacks(topic).map(|(held, _)| held)
    }

    /// [`Self::matching`], with each topic's callbacks: `bm_handle_msg` calls
    /// every one, in this order.
    pub fn matching_callbacks<'a>(
        &'a self,
        topic: &'a [u8],
    ) -> impl Iterator<Item = (&'a [u8], &'a [Subscriber])> + 'a {
        self.entries[..self.len]
            .iter()
            .filter(move |entry| crate::util::bm_wildcard_match(topic, entry.topic()))
            .map(|entry| (entry.topic(), entry.callbacks()))
    }

    /// Whether a publication on `topic` reaches any subscription:
    /// `bm_pub_wl`'s test for a local delivery.
    #[must_use]
    pub fn any_match(&self, topic: &[u8]) -> bool {
        self.matching(topic).next().is_some()
    }
}

/// The topic checks `bm_sub_wl`, `bm_unsub_wl` and `bm_pub_wl` make first.
///
/// # Errors
///
/// [`SubscriptionError::EmptyTopic`] or [`SubscriptionError::TopicTooLong`].
pub fn check_topic(topic: &[u8]) -> Result<(), SubscriptionError> {
    if topic.is_empty() {
        Err(SubscriptionError::EmptyTopic)
    } else if topic.len() >= TOPIC_MAX_LEN {
        Err(SubscriptionError::TopicTooLong)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::bm_wildcard_match;

    /// The first `spotter/transmit-data` publication in
    /// `bm-wire-diff/testdata/hello-pub-card-h0.pcap`: the dev kit's
    /// `spotter_tx_data` of the counter 100, as `bm_pub_wl` wrote it.
    const TRANSMIT_DATA: [u8; 31] =
        *b"\x00\x00\x15\x01\x02spotter/transmit-data\x02\x64\x00\x00\x00";

    #[test]
    fn encodes_a_captured_publication() {
        let mut buf = [0u8; 64];
        let len = encode(
            &mut buf,
            b"spotter/transmit-data",
            1,
            COMMON_VERSION,
            &[2, 100, 0, 0, 0],
        )
        .unwrap();
        assert_eq!(&buf[..len], &TRANSMIT_DATA);
    }

    #[test]
    fn decodes_a_captured_publication() {
        assert_eq!(
            decode(&TRANSMIT_DATA).unwrap(),
            Publication {
                header_type: 0,
                flags: 0,
                topic: b"spotter/transmit-data",
                kind: 1,
                version: 2,
                data: &[2, 100, 0, 0, 0],
            }
        );
    }

    #[test]
    fn refuses_what_bm_pub_wl_refuses() {
        let mut buf = [0u8; 512];
        assert_eq!(encode(&mut buf, b"", 0, 0, b""), Err(BmWireError::Invalid));
        let long = [b't'; TOPIC_MAX_LEN];
        assert_eq!(
            encode(&mut buf, &long, 0, 0, b""),
            Err(BmWireError::Invalid)
        );
        assert_eq!(
            encode(&mut buf, &long[..TOPIC_MAX_LEN - 1], 0, 0, b""),
            Ok(HEADER_LEN + TOPIC_MAX_LEN - 1)
        );
    }

    #[test]
    fn refuses_a_short_buffer() {
        let mut buf = [0u8; 8];
        assert_eq!(encode(&mut buf, b"abc", 0, 0, b""), Ok(8));
        assert_eq!(
            encode(&mut buf, b"abc", 0, 0, b"d"),
            Err(BmWireError::Truncated)
        );
    }

    /// Divergence #75: `bm_handle_msg` would report a data length of
    /// `size - 5 - topic_len`, wrapped to 16 bits.
    #[test]
    fn refuses_a_topic_past_the_payload() {
        assert_eq!(decode(b"\0\0\x00\x01"), Err(BmWireError::Truncated));
        assert_eq!(decode(b"\0\0\x02\x01\x02a"), Err(BmWireError::Truncated));
        assert_eq!(
            decode(b"\0\0\x01\x01\x02a").unwrap().data,
            b"",
            "a topic ending at the payload's end"
        );
    }

    /// A received `topic_len` of 0 or 255 is delivered: `bm_handle_msg` checks
    /// neither.
    #[test]
    fn decodes_topic_lengths_bm_pub_wl_refuses() {
        assert_eq!(decode(b"\x07\x08\x00\x01\x02data").unwrap().data, b"data");
        let mut long = [0u8; HEADER_LEN + 255];
        long[2] = 255;
        assert_eq!(decode(&long).unwrap().topic.len(), 255);
    }

    /// `test/src/pubsub_test.cpp`'s subscriptions and the topics it publishes
    /// to them, with the match its `subscribe` test asserts.
    #[test]
    fn pubsub_test_gold_matches() {
        for (pattern, topic, matches) in [
            (&b"example/sub0"[..], &b"example/sub0"[..], true),
            (b"example/sub1/topic", b"example/sub1/topic", true),
            (b"example/**/topic", b"example/sub1/topic", true),
            (b"example/**/topic", b"example/sub2/topic", true),
            (b"example/sub2/topic", b"example/sub1/topic", false),
            (
                b"example/*/difficult/str/*/*test",
                b"example/really/difficult/str/0123456789ABCDEF/here/test",
                true,
            ),
            (
                b"example/*/difficult/str/*/*test",
                b"example/difficult/str/0123456789ABCDEF/test",
                false,
            ),
            (
                b"example/*/difficult/str/*/*test",
                b"example/really/difficult/str/0123456789ABCDEF/",
                false,
            ),
            (
                b"example/*/difficult/str/*/*test",
                b"example/really/difficult/str/test",
                false,
            ),
            (
                b"example/end/str/*",
                b"example/end/str/0123456789ABCDEF",
                true,
            ),
            (b"example/end/str/*", b"example/end/str", false),
            (b"*/begin/str", b"0123456789ABCDEF/begin/str", true),
            (b"*/begin/str", b"begin/str", false),
            (
                b"*fouris/*/wildcards/*crazy*",
                b"wowfouris/acrazy/wildcards/amountcrazy/",
                true,
            ),
            (
                b"*fouris/*/wildcards/*crazy*",
                b"wowfouris/acrazy/wildcards/amount",
                false,
            ),
            (
                b"*fouris/*/wildcards/*crazy*",
                b"wowisacrazy/wildcards/amountcrazy/",
                false,
            ),
        ] {
            assert_eq!(
                bm_wildcard_match(topic, pattern),
                matches,
                "{:?} against {:?}",
                core::str::from_utf8(topic),
                core::str::from_utf8(pattern)
            );
        }
    }

    /// Divergence #74: a pattern with no `*` matches every topic it prefixes,
    /// `?` included.
    #[test]
    fn a_subscription_matches_topics_it_prefixes() {
        assert!(bm_wildcard_match(b"spotter/printf", b"spotter"));
        assert!(bm_wildcard_match(b"spotter/printf", b"spot?er"));
        assert!(bm_wildcard_match(b"anything", b""));
        assert!(!bm_wildcard_match(b"spotter/printf", b"spotter*x"));
        assert!(!bm_wildcard_match(b"spotter", b"spotter/printf"));
    }

    #[test]
    fn subscriptions_keep_list_order_and_refuse_as_bm_sub_wl() {
        let mut subs: Subscriptions<3, 8> = Subscriptions::new();
        assert_eq!(subs.subscribe(b""), Err(SubscriptionError::EmptyTopic));
        assert_eq!(
            subs.subscribe(&[b'a'; TOPIC_MAX_LEN]),
            Err(SubscriptionError::TopicTooLong)
        );
        assert_eq!(subs.subscribe(b"123456789"), Err(SubscriptionError::Full));
        subs.subscribe(b"a").unwrap();
        subs.subscribe(b"b*").unwrap();
        subs.subscribe(b"a").unwrap();
        assert_eq!(subs.len(), 2, "a second subscribe changes nothing");
        subs.subscribe(b"*").unwrap();
        assert_eq!(subs.subscribe(b"c"), Err(SubscriptionError::Full));
        assert_eq!(
            subs.unsubscribe(b"c"),
            Err(SubscriptionError::NotSubscribed)
        );
        subs.unsubscribe(b"a").unwrap();
        subs.subscribe(b"a").unwrap();
        assert!(subs.iter().eq([&b"b*"[..], b"*", b"a"]));
        assert!(
            subs.matching(b"abc").eq([&b"*"[..], b"a"]),
            "prefix match, #74"
        );
        assert!(subs.matching(b"bc").eq([&b"b*"[..], b"*"]));
        assert!(subs.any_match(b"x"));
        subs.unsubscribe(b"*").unwrap();
        assert!(!subs.any_match(b"x"));
    }

    /// Divergence #79, as `a_second_callback_subscribed_twice_is_called_twice`
    /// (`bm-wire-diff/tests/pubsub.rs`) measures it on the oracle.
    #[test]
    fn only_the_first_callback_is_checked_for_a_duplicate() {
        use Subscriber::{Application as A, Service as S};
        let mut subs: Subscriptions<2, 8> = Subscriptions::new();
        subs.subscribe_as(b"t", A).unwrap();
        subs.subscribe_as(b"t", A).unwrap();
        assert_eq!(subs.callbacks(b"t"), Some(&[A][..]));
        subs.subscribe_as(b"t", S).unwrap();
        subs.subscribe_as(b"t", S).unwrap();
        assert_eq!(subs.callbacks(b"t"), Some(&[A, S, S][..]));
        subs.unsubscribe_as(b"t", S).unwrap();
        assert_eq!(subs.callbacks(b"t"), Some(&[A, S][..]));
        subs.unsubscribe_as(b"t", A).unwrap();
        assert_eq!(
            subs.unsubscribe_as(b"t", A),
            Err(SubscriptionError::NoSuchSubscriber)
        );
        subs.subscribe_as(b"t", S).unwrap();
        assert_eq!(subs.callbacks(b"t"), Some(&[S][..]), "S is now the head");
        subs.subscribe_as(b"u", S).unwrap();
        subs.unsubscribe_as(b"t", S).unwrap();
        assert!(
            subs.iter().eq([&b"u"[..]]),
            "the last callback takes the topic"
        );
        assert!(
            subs.matching_callbacks(b"uv").eq([(&b"u"[..], &[S][..])]),
            "prefix match, #74"
        );
    }
}
