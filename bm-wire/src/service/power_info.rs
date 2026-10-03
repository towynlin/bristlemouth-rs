//! `power_info`: its reply body, `bm_common_messages/power_info_reply_msg.c`,
//! and the halves of `middleware/power_info_service.c` that hold no I/O.
//!
//! The request is empty. The reply is a map of three uint entries,
//! `total_on_s`, `remaining_on_s` and `upcoming_off_s`, each 32 bits kept.
//! Unlike the other services' decoders this one goes through
//! `bm_messages_helper.c` and checks each value is an unsigned integer.
//!
//! | C | Here |
//! |---|---|
//! | `power_info_request_cb` | [`handle`] |
//! | `service_queue`, `queue_cb_enqueue` | [`Callbacks`], [`Callbacks::push`] |
//! | `power_info_reply_cb`, `queue_cb_dequeue` | [`Callbacks::on_end`] |

use super::{encode_map, enter_map, leave_map, skip_key};
use crate::cbor::parser::{CborError, Value};

/// `power_info_reply_msg_num_fields`.
pub const NUM_FIELDS: usize = 3;

/// `power_info_service`: the service is `bus_power_controller/timing`. It
/// carries no node id, so every node listing it answers every request.
pub const SERVICE: &[u8] = b"bus_power_controller/timing";

/// `power_info_request_cb`: write the reply into `out` and return its length,
/// or `None` for no reply.
///
/// `stats` is the `BmPowerInfoStatsCb` `power_info_service_init` stored,
/// called only for an empty request. A request carrying data, `stats`
/// returning `None` (the C's handler with no callback), or a reply that does
/// not fit `out` gets no reply.
#[must_use]
pub fn handle(
    request: &[u8],
    stats: impl FnOnce() -> Option<PowerInfoReply>,
    out: &mut [u8],
) -> Option<usize> {
    if !request.is_empty() {
        return None;
    }
    stats()?.encode(out).ok()
}

/// `service_queue`, the `BmCbQueue` of reply callbacks
/// `power_info_service_request` queues, and the waiting requests whose
/// `reply_cb` is `power_info_reply_cb`.
///
/// A callback is named by the id of the request that queued it. Each
/// request that ends, answered or expired, dequeues the **oldest** callback,
/// not its own (divergence #96): a reply that overtakes an earlier request's
/// is reported to the earlier request's callback, and the overtaken request
/// then takes the later one's.
///
/// `N` requests; the C's queue is unbounded.
#[derive(Debug, Clone)]
pub struct Callbacks<const N: usize> {
    /// The callbacks, oldest first.
    queue: [u32; N],
    /// The requests waiting, in the order they were made.
    waiting: [u32; N],
    len: usize,
}

/// [`Callbacks::push`] refused: `N` are queued. Nothing changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbacksFull;

/// What [`Callbacks::on_end`] made of a request ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Not a request [`Callbacks::push`] listed: its own `reply_cb` runs.
    Other,
    /// The oldest callback was dequeued without being called: the request
    /// expired, or its reply did not decode.
    Dropped {
        /// The callback's request id.
        callback: u32,
    },
    /// The oldest callback was dequeued and called with the decoded reply.
    Called {
        /// The callback's request id. It need not be the request answered.
        callback: u32,
        /// The reply.
        reply: PowerInfoReply,
    },
}

impl<const N: usize> Default for Callbacks<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Callbacks<N> {
    /// None queued.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: [0; N],
            waiting: [0; N],
            len: 0,
        }
    }

    /// The callbacks queued, oldest first, each named by the id of the
    /// request that queued it.
    pub fn queued(&self) -> impl Iterator<Item = u32> + '_ {
        self.queue[..self.len].iter().copied()
    }

    /// Whether `id` is a waiting request [`Callbacks::push`] listed.
    #[must_use]
    pub fn is_waiting(&self, id: u32) -> bool {
        self.waiting[..self.len].contains(&id)
    }

    /// `queue_cb_enqueue`, for the request `id` that
    /// `power_info_service_request` then makes.
    ///
    /// # Errors
    ///
    /// [`CallbacksFull`]: the C's equivalent is the enqueue's `bm_malloc`
    /// failing, which sends no request.
    pub fn push(&mut self, id: u32) -> Result<(), CallbacksFull> {
        if self.len == N {
            return Err(CallbacksFull);
        }
        self.queue[self.len] = id;
        self.waiting[self.len] = id;
        self.len += 1;
        Ok(())
    }

    /// `power_info_reply_cb` for the request `id` ending: `reply` is the
    /// reply's data, or `None` for an expiry (`ack` false).
    ///
    /// The oldest callback is dequeued, and called if the reply decodes.
    pub fn on_end(&mut self, id: u32, reply: Option<&[u8]>) -> Ended {
        let Some(index) = self.waiting[..self.len].iter().position(|w| *w == id) else {
            return Ended::Other;
        };
        self.waiting[index..self.len].rotate_left(1);
        let callback = self.queue[0];
        self.queue[..self.len].rotate_left(1);
        self.len -= 1;
        let mut d = PowerInfoReply::default();
        match reply.map(|data| d.decode_into(data)) {
            Some(Ok(())) => Ended::Called { callback, reply: d },
            _ => Ended::Dropped { callback },
        }
    }
}

/// `PowerInfoReplyData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PowerInfoReply {
    /// `total_on_s`.
    pub total_on_s: u32,
    /// `remaining_on_s`.
    pub remaining_on_s: u32,
    /// `upcoming_off_s`.
    pub upcoming_off_s: u32,
}

/// `decode_key_value_uint32`: the value is written only once the key, the
/// value's type and both advances have succeeded.
fn uint32(value: &mut Value<'_>, out: &mut u32) -> Result<(), CborError> {
    skip_key(value)?;
    if !value.is_unsigned_integer() {
        return Err(CborError::IllegalType);
    }
    let v = value.extract();
    value.advance()?;
    *out = v as u32;
    Ok(())
}

impl PowerInfoReply {
    /// `power_info_reply_encode` into `out`, returning the encoded length.
    ///
    /// # Errors
    ///
    /// [`CborError::OutOfMemory`] if it does not fit.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, CborError> {
        encode_map(out, NUM_FIELDS, |w| {
            w.uint("total_on_s", self.total_on_s.into());
            w.uint("remaining_on_s", self.remaining_on_s.into());
            w.uint("upcoming_off_s", self.upcoming_off_s.into());
        })
    }

    /// `power_info_reply_decode`.
    ///
    /// Fields are written as they are read, so a failure leaves the ones
    /// before it changed. `power_info_reply_cb` hands the result to the
    /// requester's callback only if the decode succeeded
    /// ([`Callbacks::on_end`]).
    ///
    /// # Errors
    ///
    /// tinycbor's error where the C returns one.
    pub fn decode_into(&mut self, buf: &[u8]) -> Result<(), CborError> {
        let (mut map, mut value) = enter_map(buf, NUM_FIELDS)?;
        uint32(&mut value, &mut self.total_on_s)?;
        uint32(&mut value, &mut self.remaining_on_s)?;
        uint32(&mut value, &mut self.upcoming_off_s)?;
        leave_map(&mut map, &value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bm_common_messages/test/power_info_ut.cpp`, `PowerInfoReply`. The
    /// test round-trips through a 1024-byte buffer; the bytes are the
    /// oracle's.
    #[test]
    fn power_info_ut() {
        let encode = PowerInfoReply {
            total_on_s: u32::MAX,
            remaining_on_s: 100_000,
            upcoming_off_s: 3_333_333,
        };
        let mut buf = [0u8; 1024];
        let len = encode.encode(&mut buf).unwrap();
        assert_eq!(
            &buf[..len],
            b"\xa3\x6atotal_on_s\x1a\xff\xff\xff\xff\
              \x6eremaining_on_s\x1a\x00\x01\x86\xa0\
              \x6eupcoming_off_s\x1a\x00\x32\xdc\xd5"
        );
        let mut decode = PowerInfoReply::default();
        assert_eq!(decode.decode_into(&buf), Ok(()));
        assert_eq!(decode, encode);
    }

    fn encoded(total_on_s: u32) -> ([u8; 64], usize) {
        let mut buf = [0u8; 64];
        let len = PowerInfoReply {
            total_on_s,
            remaining_on_s: 2,
            upcoming_off_s: 3,
        }
        .encode(&mut buf)
        .unwrap();
        (buf, len)
    }

    #[test]
    fn the_handler_answers_an_empty_request() {
        let mut out = [0u8; super::super::REPLY_DATA_LEN];
        let n = handle(b"", || Some(PowerInfoReply::default()), &mut out).unwrap();
        let mut d = PowerInfoReply::default();
        d.decode_into(&out[..n]).unwrap();
        assert_eq!(d, PowerInfoReply::default());

        assert_eq!(handle(b"", || None, &mut out), None, "no callback");
        assert_eq!(
            handle(b"x", || unreachable!("not called"), &mut out),
            None,
            "a request with data"
        );
        assert_eq!(
            handle(b"", || Some(PowerInfoReply::default()), &mut out[..8]),
            None,
            "past the buffer"
        );
    }

    #[test]
    fn requests_ending_in_order_get_their_own_callbacks() {
        let mut q: Callbacks<4> = Callbacks::new();
        q.push(5).unwrap();
        q.push(9).unwrap();
        let (buf, len) = encoded(1);
        assert_eq!(q.on_end(7, Some(&buf[..len])), Ended::Other);
        assert!(matches!(
            q.on_end(5, Some(&buf[..len])),
            Ended::Called { callback: 5, reply } if reply.total_on_s == 1
        ));
        assert_eq!(q.on_end(9, None), Ended::Dropped { callback: 9 });
        assert_eq!(q.queued().count(), 0);
        assert_eq!(q.on_end(9, None), Ended::Other, "already ended");
    }

    /// Divergence #96: the queue is FIFO, so ends out of order swap the
    /// callbacks.
    #[test]
    fn a_reply_overtaking_an_earlier_request_takes_its_callback() {
        let mut q: Callbacks<4> = Callbacks::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        let (buf, len) = encoded(22);
        assert!(matches!(
            q.on_end(2, Some(&buf[..len])),
            Ended::Called { callback: 1, reply } if reply.total_on_s == 22
        ));
        assert!(q.is_waiting(1) && !q.is_waiting(2));
        assert!(q.queued().eq([2]));
        let (buf, len) = encoded(11);
        assert!(matches!(
            q.on_end(1, Some(&buf[..len])),
            Ended::Called { callback: 2, reply } if reply.total_on_s == 11
        ));
    }

    /// An expiry, or a reply that does not decode, still uses up the oldest
    /// callback.
    #[test]
    fn an_end_without_a_decoded_reply_drops_the_oldest_callback() {
        let mut q: Callbacks<2> = Callbacks::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        assert_eq!(q.push(3), Err(CallbacksFull));
        assert_eq!(q.on_end(2, Some(b"\xa0")), Ended::Dropped { callback: 1 });
        assert_eq!(q.on_end(1, None), Ended::Dropped { callback: 2 });
        assert_eq!(q.queued().count(), 0);
    }

    #[test]
    fn a_failure_keeps_the_fields_read_before_it() {
        let mut buf = [0u8; 64];
        let len = PowerInfoReply {
            total_on_s: 1,
            remaining_on_s: 2,
            upcoming_off_s: 3,
        }
        .encode(&mut buf)
        .unwrap();
        // The last value, 0x03, made negative.
        buf[len - 1] = 0x23;
        let mut d = PowerInfoReply::default();
        assert_eq!(d.decode_into(&buf[..len]), Err(CborError::IllegalType));
        assert_eq!(
            (d.total_on_s, d.remaining_on_s, d.upcoming_off_s),
            (1, 2, 0)
        );
    }
}
