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
}
