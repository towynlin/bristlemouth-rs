//! `middleware/bm_service_request.c`: the requests a node has made and is
//! waiting on.
//!
//! | C | Here |
//! |---|---|
//! | `CTX.service_request_list` | [`Requests`] |
//! | `CTX.request_count` | [`Requests::next_id`] |
//! | `_create_node` and `_request_list_add_request` | [`Requests::add`] |
//! | `_service_request_cb` | [`Requests::on_reply`] |
//! | `_service_request_timer_expiry_cb`, on its 500 ms timer | [`Requests::on_tick`] |
//!
//! | C behaviour | Here | Divergence |
//! |---|---|---|
//! | A request whose reply topic is not subscribed, or that is not sent, stays listed and times out | [`Requests::add`] lists it; the caller does not remove it | #91 |
//! | `timeout_s * 1000` wraps in 32 bits, and a timeout past `i32::MAX` ms expires at the next sweep | [`Requests::add`] and the sweep do the same | #91 |
//! | `_service_request_cb` reads the 16-byte header without a length check | [`ReplyOutcome::Short`] | #92 |
//! | `_service_request_cb` passes `data_size` unchecked; the callback reads past the publication | [`ReplyOutcome::Answered`] carries what arrived | #92 |
//! | `_service_request_cb` matches the id and target, not the topic | [`Requests::on_reply`] does the same | #92 |

use super::ReplyHeader;
use crate::util::time_remaining;

/// `ExpiryTimerPeriodMs`: how often the sweep runs.
pub const EXPIRY_PERIOD_MS: u32 = 500;

/// A request waiting on its reply: a `BmServiceRequestNode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<const NAME: usize> {
    service: [u8; NAME],
    len: usize,
    id: u32,
    start_ms: u32,
    timeout_ms: u32,
}

impl<const NAME: usize> Request<NAME> {
    const EMPTY: Self = Self {
        service: [0; NAME],
        len: 0,
        id: 0,
        start_ms: 0,
        timeout_ms: 0,
    };

    /// `id`, which the reply echoes.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// The service asked, without `/req`.
    #[must_use]
    pub fn service(&self) -> &[u8] {
        &self.service[..self.len]
    }

    /// `request_start_ms`.
    #[must_use]
    pub const fn start_ms(&self) -> u32 {
        self.start_ms
    }

    /// `timeout_ms`: the caller's seconds times 1000, wrapped.
    #[must_use]
    pub const fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }

    /// Whether the sweep at `now_ms` expires it: `time_remaining_ms` is 0.
    #[must_use]
    pub fn expired_at(&self, now_ms: u32) -> bool {
        time_remaining(self.start_ms, now_ms, self.timeout_ms) == 0
    }
}

/// Why [`Requests::add`] refused: `N` requests are waiting, or the name is
/// longer than `NAME`. Ceilings bm_core does not have; its nearest is a
/// `bm_malloc` failure in `_create_node`, which returns before an id is
/// taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestsFull;

/// What `_service_request_cb` makes of one publication on a reply topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyOutcome<'a, const NAME: usize> {
    /// The body is shorter than [`ReplyHeader::LEN`]. The C reads the header
    /// past the publication (divergence #92). Nothing changes.
    Short,
    /// `target_node_id` is another node's. Nothing changes.
    OtherTarget,
    /// No waiting request has the reply's `id`. Nothing changes.
    Unknown,
    /// The request with the reply's `id` is answered and no longer listed:
    /// `reply_cb(true, ...)`.
    Answered {
        /// The request, as it was listed. Its service need not be the
        /// reply's topic (divergence #92).
        request: Request<NAME>,
        /// The reply's data: `data_size` bytes, or what arrived if fewer.
        /// The C passes `data_size` and the callback reads past the
        /// publication (divergence #92).
        data: &'a [u8],
    },
}

/// `CTX.service_request_list`, `CTX.request_count` and the phase of the
/// expiry timer.
///
/// `N` requests of services named in up to `NAME` bytes; bm_core has neither
/// ceiling.
#[derive(Debug, Clone)]
pub struct Requests<const N: usize, const NAME: usize> {
    list: [Request<NAME>; N],
    len: usize,
    request_count: u32,
    next_sweep_ms: u32,
}

impl<const N: usize, const NAME: usize> Default for Requests<N, NAME> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const NAME: usize> Requests<N, NAME> {
    /// No requests, the first id 0, and the sweep phased from time zero.
    #[must_use]
    pub const fn new() -> Self {
        Self::resuming(0, EXPIRY_PERIOD_MS)
    }

    /// The same, with the sweep phased from `now_ms`:
    /// `bm_service_request_init` running then.
    #[must_use]
    pub const fn started_at(now_ms: u32) -> Self {
        Self::resuming(0, now_ms.wrapping_add(EXPIRY_PERIOD_MS))
    }

    /// No requests, lined up with a `bm_service_request.c` already running:
    /// `request_count` is the next id it hands out and `next_sweep_ms` the
    /// millisecond its timer next fires on. For a differential harness;
    /// firmware wants [`Self::new`] or [`Self::started_at`].
    #[must_use]
    pub const fn resuming(request_count: u32, next_sweep_ms: u32) -> Self {
        Self {
            list: [Request::EMPTY; N],
            len: 0,
            request_count,
            next_sweep_ms,
        }
    }

    /// How many requests are waiting.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether none is.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The id the next request takes: `CTX.request_count`.
    #[must_use]
    pub const fn next_id(&self) -> u32 {
        self.request_count
    }

    /// The millisecond the sweep next runs on.
    #[must_use]
    pub const fn next_sweep_ms(&self) -> u32 {
        self.next_sweep_ms
    }

    /// The waiting requests, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Request<NAME>> + '_ {
        self.list[..self.len].iter()
    }

    /// `_create_node` and `_request_list_add_request`: list a request to
    /// `service` made at `now_ms`, taking the next id. Returns the id.
    ///
    /// The timeout is `timeout_s * 1000`, wrapped to 32 bits, as the C
    /// computes it (divergence #91).
    ///
    /// # Errors
    ///
    /// [`RequestsFull`]; nothing changes and no id is taken.
    pub fn add(
        &mut self,
        service: &[u8],
        timeout_s: u32,
        now_ms: u32,
    ) -> Result<u32, RequestsFull> {
        if self.len == N || service.len() > NAME {
            return Err(RequestsFull);
        }
        let id = self.request_count;
        self.request_count = self.request_count.wrapping_add(1);
        let mut held = [0u8; NAME];
        held[..service.len()].copy_from_slice(service);
        self.list[self.len] = Request {
            service: held,
            len: service.len(),
            id,
            start_ms: now_ms,
            timeout_ms: timeout_s.wrapping_mul(1000),
        };
        self.len += 1;
        Ok(id)
    }

    /// `_service_request_cb`, for a publication carrying `body` reaching a
    /// node whose id is `node_id`.
    #[must_use]
    pub fn on_reply<'a>(&mut self, node_id: u64, body: &'a [u8]) -> ReplyOutcome<'a, NAME> {
        let Ok(header) = ReplyHeader::decode(body) else {
            return ReplyOutcome::Short;
        };
        if header.target_node_id != node_id {
            return ReplyOutcome::OtherTarget;
        }
        let Some(request) = self.take(header.id) else {
            return ReplyOutcome::Unknown;
        };
        let data = &body[ReplyHeader::LEN..];
        let size = usize::try_from(header.data_size).unwrap_or(usize::MAX);
        ReplyOutcome::Answered {
            request,
            data: &data[..size.min(data.len())],
        }
    }

    /// Remove and return the first request with `id`:
    /// `_service_request_list_get_node_by_id` and `_request_list_remove_request`.
    fn take(&mut self, id: u32) -> Option<Request<NAME>> {
        let index = self.iter().position(|r| r.id == id)?;
        Some(self.remove(index))
    }

    fn remove(&mut self, index: usize) -> Request<NAME> {
        let request = self.list[index];
        self.list[index..self.len].rotate_left(1);
        self.len -= 1;
        request
    }

    /// Run the expiry sweep if it is due, reporting and removing each expired
    /// request in list order: `reply_cb(false, ...)`.
    ///
    /// The C's timer fires every [`EXPIRY_PERIOD_MS`] from
    /// `bm_service_request_init`, so a request expires at the first sweep at
    /// least its timeout after it was made. Call this at least once per
    /// period; between sweeps it does nothing, and sweeps missed are run once.
    pub fn on_tick(&mut self, now_ms: u32, mut expired: impl FnMut(&Request<NAME>)) {
        if (now_ms.wrapping_sub(self.next_sweep_ms) as i32) < 0 {
            return;
        }
        let missed = now_ms.wrapping_sub(self.next_sweep_ms) / EXPIRY_PERIOD_MS;
        self.next_sweep_ms = self
            .next_sweep_ms
            .wrapping_add(EXPIRY_PERIOD_MS.wrapping_mul(missed.wrapping_add(1)));
        // The C restarts from the head after each removal; the requests
        // before it were unexpired at the same `now_ms`, so the order is list
        // order.
        let mut index = 0;
        while index < self.len {
            let request = &self.list[index];
            if request.expired_at(now_ms) {
                expired(request);
                self.remove(index);
            } else {
                index += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(target: u64, id: u32, data_size: u32, data: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        ReplyHeader {
            target_node_id: target,
            id,
            data_size,
        }
        .encode(&mut out)
        .unwrap();
        out[16..16 + data.len()].copy_from_slice(data);
        out
    }

    #[test]
    fn ids_count_up_and_a_refusal_takes_none() {
        let mut requests: Requests<2, 4> = Requests::new();
        assert_eq!(requests.add(b"a", 1, 0), Ok(0));
        assert_eq!(requests.add(b"abcde", 1, 0), Err(RequestsFull));
        assert_eq!(requests.add(b"b", 1, 0), Ok(1));
        assert_eq!(requests.add(b"c", 1, 0), Err(RequestsFull));
        assert_eq!(requests.next_id(), 2);
        let mut wrapping: Requests<1, 4> = Requests::resuming(u32::MAX, 0);
        assert_eq!(wrapping.add(b"a", 1, 0), Ok(u32::MAX));
        assert_eq!(wrapping.next_id(), 0);
    }

    #[test]
    fn a_reply_answers_by_id_and_target_only() {
        let mut requests: Requests<4, 8> = Requests::new();
        requests.add(b"one", 1, 0).unwrap();
        requests.add(b"two", 1, 0).unwrap();
        let body = reply(7, 1, 2, b"hi");
        assert_eq!(requests.on_reply(8, &body), ReplyOutcome::OtherTarget);
        assert_eq!(
            requests.on_reply(7, &reply(7, 5, 0, b"")),
            ReplyOutcome::Unknown
        );
        let ReplyOutcome::Answered { request, data } = requests.on_reply(7, &body) else {
            panic!("answered");
        };
        assert_eq!(
            (request.id(), request.service(), data),
            (1, &b"two"[..], &b"hi"[..])
        );
        assert_eq!(requests.on_reply(7, &body), ReplyOutcome::Unknown, "taken");
        assert_eq!(requests.on_reply(7, &body[..15]), ReplyOutcome::Short);
        assert_eq!(requests.len(), 1);
    }

    /// Divergence #92: `data_size` past the publication.
    #[test]
    fn data_is_what_arrived() {
        let mut requests: Requests<4, 8> = Requests::new();
        requests.add(b"a", 1, 0).unwrap();
        let body = reply(7, 0, 1000, b"abc");
        let ReplyOutcome::Answered { data, .. } = requests.on_reply(7, &body[..19]) else {
            panic!("answered");
        };
        assert_eq!(data, b"abc");
    }

    #[test]
    fn requests_expire_on_the_sweeps_grid() {
        let mut requests: Requests<4, 8> = Requests::started_at(10);
        requests.add(b"a", 1, 400).unwrap();
        requests.add(b"b", 0, 400).unwrap();
        requests.add(b"c", 1, 600).unwrap();
        let mut expired = [(0, 0); 3];
        let mut n = 0;
        for now in [509, 510, 1009, 1510, 2010] {
            requests.on_tick(now, |r| {
                expired[n] = (now, r.id());
                n += 1;
            });
        }
        assert_eq!(expired, [(510, 1), (1510, 0), (2010, 2)]);
        assert!(requests.is_empty());
    }

    #[test]
    fn missed_sweeps_keep_the_phase() {
        let mut requests: Requests<4, 8> = Requests::new();
        requests.on_tick(1700, |_| {});
        assert_eq!(requests.next_sweep_ms(), 2000);
    }

    /// Divergence #91: the timeout wraps, and past `i32::MAX` ms
    /// `time_remaining` reads as overdue.
    #[test]
    fn long_timeouts_wrap() {
        let mut requests: Requests<4, 8> = Requests::new();
        requests.add(b"a", 4_294_968, 0).unwrap();
        assert_eq!(requests.iter().next().unwrap().timeout_ms(), 704);
        requests.add(b"b", 2_147_485, 0).unwrap();
        requests.add(b"c", 2_147_483, 0).unwrap();
        let mut expired = [0; 2];
        let mut n = 0;
        for now in [500, 1000] {
            requests.on_tick(now, |r| {
                expired[n] = r.id();
                n += 1;
            });
        }
        assert_eq!(expired, [1, 0]);
        assert_eq!(requests.len(), 1);
    }
}
