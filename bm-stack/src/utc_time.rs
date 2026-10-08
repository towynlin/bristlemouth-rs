//! The Spotter's `spotter/utc-time` publication, which C nodes set their RTC
//! from.
//!
//! Not bm_core: the handler is application code in bm_protocol at `62d8b5d`,
//! `src/apps/bm_devkit/bmdk_common/app_main.cpp:238-272`
//! (`handle_bm_subscriptions`), subscribed at line 412 with
//! `bm_sub(APP_PUB_SUB_UTC_TOPIC, ...)`. The bridge, mote_bristlefin,
//! bristleback and rs232_expander apps carry the same branch.
//!
//! | C | Here |
//! |---|---|
//! | `strncmp(APP_PUB_SUB_UTC_TOPIC, topic, topic_len) == 0` | [`decode`]'s topic check, NUL and prefix behaviour included |
//! | `type == APP_PUB_SUB_UTC_TYPE && version == APP_PUB_SUB_UTC_VERSION`, both 1 (`app_pub_sub.h:10-12`) | [`KIND`], [`VERSION`] |
//! | `reinterpret_cast<const bm_common_pub_sub_utc_t *>(data)->utc_us`, a packed `uint64_t` (bm_core `bm_common_messages/bm_common_pub_sub.h:21-23`) | the first 8 data bytes, little-endian |
//! | `dateTimeFromUtc`, then `.ms = usec / 1000` | [`crate::RtcTimeAndDate::from_utc_micros`] |
//! | `rtcSet` | [`crate::Rtc::set`] |
//!
//! Where it differs: the C does not check `data_len`, so data shorter than 8
//! bytes is read past its end. [`decode`] refuses it ([`UtcTimeError::Short`]).
//! The header comment on `utc_us` says nanoseconds; the C converts it as
//! microseconds, and a Spotter sends microseconds.
//!
//! [`UtcTimeSetter`] is the handler for an [`crate::App`]: it takes the
//! publication from [`crate::App::on_event`] and sets the clock in
//! [`crate::App::act`], one loop pass later, since `on_event` has no node.

use crate::node::Event;
use crate::port::{Rtc, RtcTimeAndDate};

/// `APP_PUB_SUB_UTC_TOPIC`.
pub const TOPIC: &[u8] = b"spotter/utc-time";
/// `APP_PUB_SUB_UTC_TYPE`, the pub/sub header's `type`.
pub const KIND: u8 = 1;
/// `APP_PUB_SUB_UTC_VERSION`, the pub/sub header's `version`.
pub const VERSION: u8 = 1;
/// `sizeof(bm_common_pub_sub_utc_t)`.
pub const DATA_LEN: usize = 8;

/// Why [`decode`] did not return a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum UtcTimeError {
    /// The topic check failed. The C prints the topic and data instead.
    Topic,
    /// Kind or version is not 1. The C prints "Unrecognized version".
    Unrecognized {
        /// The publication's `type`.
        kind: u8,
        /// The publication's `version`.
        version: u8,
    },
    /// Fewer than [`DATA_LEN`] data bytes. The C reads past them.
    Short {
        /// The data length.
        len: usize,
    },
}

/// The UTC microseconds a publication delivered to the [`TOPIC`]
/// subscription carries, after the C handler's checks.
///
/// The topic check is `strncmp(TOPIC, topic, topic.len())`: it passes for any
/// topic `TOPIC` begins with, the empty topic included, and for `TOPIC`
/// followed by a NUL and anything. Pub/sub delivers to a `TOPIC` subscription
/// only topics `TOPIC` prefixes (divergence #74), so of those, `TOPIC` itself
/// and `TOPIC` + NUL + anything pass.
///
/// # Errors
///
/// [`UtcTimeError`], checked in the C's order: topic, kind and version, then
/// length.
pub fn decode(topic: &[u8], kind: u8, version: u8, data: &[u8]) -> Result<u64, UtcTimeError> {
    if !strncmp_eq(TOPIC, topic) {
        return Err(UtcTimeError::Topic);
    }
    if kind != KIND || version != VERSION {
        return Err(UtcTimeError::Unrecognized { kind, version });
    }
    let Some(bytes) = data.first_chunk::<DATA_LEN>() else {
        return Err(UtcTimeError::Short { len: data.len() });
    };
    Ok(u64::from_le_bytes(*bytes))
}

/// `strncmp(a, b, b.len()) == 0` for a NUL-terminated `a` stored without its
/// NUL.
fn strncmp_eq(a: &[u8], b: &[u8]) -> bool {
    for (i, &byte) in b.iter().enumerate() {
        let expected = a.get(i).copied().unwrap_or(0);
        if expected != byte {
            return false;
        }
        if byte == 0 {
            return true;
        }
    }
    true
}

/// The C handler, split across an [`crate::App`]'s `on_event` and `act`.
///
/// Subscribe the node to [`TOPIC`] first. Only publications delivered to that
/// subscription are handled, as the C handler is registered for it alone; the
/// same publication delivered to another subscription (`spotter/*`) is not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct UtcTimeSetter {
    pending: Option<u64>,
}

impl UtcTimeSetter {
    /// A setter with nothing pending.
    #[must_use]
    pub fn new() -> Self {
        Self { pending: None }
    }

    /// Call from `on_event`. `None` for anything but a publication delivered
    /// to the [`TOPIC`] subscription; otherwise [`decode`]'s result, and a
    /// time is held for [`Self::apply`], replacing any not yet applied.
    pub fn on_event(&mut self, event: &Event<'_>) -> Option<Result<u64, UtcTimeError>> {
        let Event::Publication {
            subscription,
            topic,
            kind,
            version,
            data,
            ..
        } = event
        else {
            return None;
        };
        if *subscription != TOPIC {
            return None;
        }
        let decoded = decode(topic, *kind, *version, data);
        if let Ok(utc_us) = decoded {
            self.pending = Some(utc_us);
        }
        Some(decoded)
    }

    /// Whether a time is waiting for [`Self::apply`]. An app's `ready` returns
    /// at once while it is.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Call from `act`: set `rtc` to the held time, as `rtcSet` after
    /// `dateTimeFromUtc`. `None` if nothing is held; otherwise the reading
    /// set and whether [`Rtc::set`] accepted it.
    pub fn apply<R: Rtc>(&mut self, rtc: &mut R) -> Option<(RtcTimeAndDate, bool)> {
        let reading = RtcTimeAndDate::from_utc_micros(self.pending.take()?);
        Some((reading, rtc.set(&reading)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data of a Spotter's publication on the E1 bench.
    const BENCH: &[u8] = b"\xb0*M.\xbf\\\x06\x00";

    #[test]
    fn the_bench_publication_decodes_to_its_microseconds() {
        assert_eq!(decode(TOPIC, 1, 1, BENCH), Ok(0x0006_5CBF_2E4D_2AB0));
        assert_eq!(0x0006_5CBF_2E4D_2AB0_u64, 1_790_826_045_582_000);
    }

    #[test]
    fn data_past_eight_bytes_is_ignored() {
        let mut data = BENCH.to_vec();
        data.extend_from_slice(b"tail");
        assert_eq!(decode(TOPIC, 1, 1, &data), decode(TOPIC, 1, 1, BENCH));
    }

    #[test]
    fn short_data_is_refused() {
        assert_eq!(
            decode(TOPIC, 1, 1, &BENCH[..7]),
            Err(UtcTimeError::Short { len: 7 })
        );
    }

    #[test]
    fn kind_and_version_must_both_be_one() {
        assert_eq!(
            decode(TOPIC, 1, 2, BENCH),
            Err(UtcTimeError::Unrecognized {
                kind: 1,
                version: 2
            })
        );
        assert_eq!(
            decode(TOPIC, 0, 1, BENCH),
            Err(UtcTimeError::Unrecognized {
                kind: 0,
                version: 1
            })
        );
    }

    #[test]
    fn the_topic_check_is_strncmp_over_the_topic_length() {
        let ok = |topic: &[u8]| decode(topic, 1, 1, BENCH).is_ok();
        assert!(ok(b"spotter/utc-time"));
        assert!(ok(b"spotter/utc-time\0anything"));
        assert!(ok(b"spotter/utc"), "a prefix of TOPIC");
        assert!(ok(b""));
        assert!(!ok(b"spotter/utc-timer"));
        assert!(!ok(b"spotter/utc-tim3"));
        assert!(!ok(b"spotter/printf"));
    }
}
