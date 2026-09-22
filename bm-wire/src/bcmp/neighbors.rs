//! `BcmpNeighborTableRequest` and `BcmpNeighborTableReply`, ported from
//! `bcmp/messages.h`, `bcmp/neighbors.c` and `integrations/topology.c`.
//!
//! The reply is an 11-byte head followed by two counted arrays: `port_len`
//! [`PortInfo`] entries, then `neighbor_len` [`NeighborInfo`] entries. Both
//! counts come off the wire, and neither is checked against the size of the
//! message that carried them — see divergence #14. [`NeighborTableReply`]
//! borrows the two arrays out of the frame, so the bounds are checked once, at
//! decode, and the iterators cannot walk past them.
//!
//! # The reply's consumer
//!
//! [`TableRequests`] is `bcmp/neighbors.c`'s requester state —
//! `TARGET_NODE_ID`, `NEIGHBOR_REQUEST_CB` and `NEIGHBOR_TIMER`, which hold one
//! request between them. It is here for the reason [`super::info::InfoRequests`]
//! is: module state rather than wire format, and sans-io, so `bm_stack::Node`
//! owns the transmission and the clock.
//!
//! The two are not the same shape, and the difference is the whole of the
//! requester side:
//!
//! | | `INFO_REQUEST_LIST` (`0x04`) | `bcmp/neighbors.c` (`0x08`) |
//! |---|---|---|
//! | Outstanding requests | unbounded list | one |
//! | Keyed on | low 32 bits of the id (#33) | all 64 bits |
//! | Matched against | the reply's body `node_id` | the reply's body `node_id` |
//! | A broadcast request | answered and consumed | answered by everyone, accepted from nobody (#35) |
//! | Expiry | none (#19) | a 1 s one-shot timer that expires nothing (#36) |

use crate::BmWireError;
use crate::util::time_remaining;

/// `bcmp_table_max_len`: longest reply `bcmp_send_neighbor_table` will build.
///
/// Above it the C returns `BmEINVAL` and answers nothing at all — its `TODO -
/// handle more gracefully` is the whole of the handling. On a two-port node
/// that is 101 neighbours, so no device reaches it.
pub const NEIGHBOR_TABLE_MAX_LEN: usize = 1024;

/// `bcmp_neighbor_timer_timeout_s`, as milliseconds: how long
/// `bcmp_request_neighbor_table` waits before it fires the caller's `timeout`.
///
/// Unlike `packet.c`'s sequenced requests (divergence #22) this is a one-shot
/// timer armed at the moment of the request, so the deadline is the deadline.
/// What it does *not* do is give up on the request — see [`TableRequests`] and
/// divergence #36.
pub const NEIGHBOR_REQUEST_TIMEOUT_MS: u32 = 1000;

/// `BcmpNeighborTableRequest`: ask one node, or every node, for its neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NeighborTableRequest {
    /// Node to answer, or zero for all of them.
    pub target_node_id: u64,
}

impl NeighborTableRequest {
    /// Wire size.
    pub const LEN: usize = 8;

    /// Decode from the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn decode(buf: &[u8]) -> Result<Self, BmWireError> {
        let bytes: [u8; 8] = buf
            .get(..Self::LEN)
            .and_then(|b| b.try_into().ok())
            .ok_or(BmWireError::Truncated)?;
        Ok(Self {
            target_node_id: u64::from_le_bytes(bytes),
        })
    }

    /// Encode into the first [`Self::LEN`] bytes of `buf`.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<(), BmWireError> {
        let buf = buf.get_mut(..Self::LEN).ok_or(BmWireError::Truncated)?;
        buf.copy_from_slice(&self.target_node_id.to_le_bytes());
        Ok(())
    }
}

/// `BcmpPortInfo`: one local port's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PortInfo {
    /// Link state, as the raw byte.
    ///
    /// The C field is a `bool`, so any value other than 0 or 1 is an
    /// out-of-range read there. Keeping the byte means a decoded reply
    /// re-encodes to the same bytes; use [`Self::is_up`] for the meaning.
    pub state: u8,
    /// Port type, mapping to `bm_port_type_e`. `bcmp_send_neighbor_table`
    /// never sets it, so it is always zero on the wire today.
    pub port_type: u8,
}

impl PortInfo {
    /// Wire size.
    pub const LEN: usize = 2;

    /// Whether the link is up.
    #[must_use]
    pub const fn is_up(&self) -> bool {
        self.state != 0
    }
}

/// `BcmpNeighborInfo`: one neighbour, as the replying node sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NeighborInfo {
    /// The neighbour's node id.
    pub node_id: u64,
    /// Which of the replying node's ports the neighbour is on.
    pub port: u8,
    /// Whether the neighbour is currently online, as the raw byte.
    pub online: u8,
}

impl NeighborInfo {
    /// Wire size.
    pub const LEN: usize = 10;

    /// Whether the neighbour is online.
    #[must_use]
    pub const fn is_online(&self) -> bool {
        self.online != 0
    }

    fn decode(buf: &[u8; Self::LEN]) -> Self {
        Self {
            node_id: u64::from_le_bytes(buf[0..8].try_into().expect("8 bytes")),
            port: buf[8],
            online: buf[9],
        }
    }

    fn encode_into(&self, buf: &mut [u8; Self::LEN]) {
        buf[0..8].copy_from_slice(&self.node_id.to_le_bytes());
        buf[8] = self.port;
        buf[9] = self.online;
    }
}

/// `BcmpNeighborTableReply`, borrowed from the frame it arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighborTableReply<'a> {
    /// Node id of the replying node.
    pub node_id: u64,
    ports: &'a [u8],
    neighbors: &'a [u8],
}

impl<'a> NeighborTableReply<'a> {
    /// Size of the fixed part. `sizeof(BcmpNeighborTableReply)`.
    pub const HEADER_LEN: usize = 11;

    /// Decode a reply, borrowing its two arrays from `buf`.
    ///
    /// Trailing bytes past the declared arrays are ignored.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is shorter than the fixed part, or
    /// shorter than the entry counts it declares.
    pub fn decode(buf: &'a [u8]) -> Result<Self, BmWireError> {
        let head = buf.get(..Self::HEADER_LEN).ok_or(BmWireError::Truncated)?;
        let node_id = u64::from_le_bytes(head[0..8].try_into().expect("8 bytes"));
        let port_len = usize::from(head[8]);
        let neighbor_len = usize::from(u16::from_le_bytes([head[9], head[10]]));

        // The check neither bm_core's neighbour code nor topology.c does.
        let ports_bytes = port_len * PortInfo::LEN;
        let neighbors_bytes = neighbor_len * NeighborInfo::LEN;
        let body = buf
            .get(Self::HEADER_LEN..Self::HEADER_LEN + ports_bytes + neighbors_bytes)
            .ok_or(BmWireError::Truncated)?;

        Ok(Self {
            node_id,
            ports: &body[..ports_bytes],
            neighbors: &body[ports_bytes..],
        })
    }

    /// Number of local ports described.
    #[must_use]
    pub fn port_count(&self) -> u8 {
        (self.ports.len() / PortInfo::LEN) as u8
    }

    /// Number of neighbours described.
    #[must_use]
    pub fn neighbor_count(&self) -> u16 {
        (self.neighbors.len() / NeighborInfo::LEN) as u16
    }

    /// The local ports, in order.
    pub fn ports(&self) -> impl Iterator<Item = PortInfo> + '_ {
        self.ports
            .as_chunks::<{ PortInfo::LEN }>()
            .0
            .iter()
            .map(|c| PortInfo {
                state: c[0],
                port_type: c[1],
            })
    }

    /// The neighbours, in order.
    pub fn neighbors(&self) -> impl Iterator<Item = NeighborInfo> + '_ {
        self.neighbors
            .as_chunks::<{ NeighborInfo::LEN }>()
            .0
            .iter()
            .map(NeighborInfo::decode)
    }

    /// Bytes [`Self::encode`] will write.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER_LEN + self.ports.len() + self.neighbors.len()
    }

    /// Re-encode into `buf`, returning how many bytes were written.
    ///
    /// # Errors
    ///
    /// [`BmWireError::Truncated`] if `buf` is too short.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, BmWireError> {
        let end = self.encoded_len();
        let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;
        write_head(buf, self.node_id, self.port_count(), self.neighbor_count());
        let split = Self::HEADER_LEN + self.ports.len();
        buf[Self::HEADER_LEN..split].copy_from_slice(self.ports);
        buf[split..end].copy_from_slice(self.neighbors);
        Ok(end)
    }
}

fn write_head(buf: &mut [u8], node_id: u64, port_len: u8, neighbor_len: u16) {
    buf[0..8].copy_from_slice(&node_id.to_le_bytes());
    buf[8] = port_len;
    buf[9..11].copy_from_slice(&neighbor_len.to_le_bytes());
}

/// Bytes [`encode_neighbor_table_reply`] will write for these lists.
#[must_use]
pub const fn neighbor_table_reply_len(port_count: usize, neighbor_count: usize) -> usize {
    NeighborTableReply::HEADER_LEN + port_count * PortInfo::LEN + neighbor_count * NeighborInfo::LEN
}

/// Build a reply from owned lists, returning how many bytes were written.
///
/// # Errors
///
/// [`BmWireError::Invalid`] if there are more ports or neighbours than the
/// count fields can describe. [`BmWireError::Truncated`] if `buf` is too short.
pub fn encode_neighbor_table_reply(
    buf: &mut [u8],
    node_id: u64,
    ports: &[PortInfo],
    neighbors: &[NeighborInfo],
) -> Result<usize, BmWireError> {
    if ports.len() > usize::from(u8::MAX) || neighbors.len() > usize::from(u16::MAX) {
        return Err(BmWireError::Invalid);
    }
    let end = neighbor_table_reply_len(ports.len(), neighbors.len());
    let buf = buf.get_mut(..end).ok_or(BmWireError::Truncated)?;

    write_head(buf, node_id, ports.len() as u8, neighbors.len() as u16);
    let mut at = NeighborTableReply::HEADER_LEN;
    for port in ports {
        buf[at] = port.state;
        buf[at + 1] = port.port_type;
        at += PortInfo::LEN;
    }
    for neighbor in neighbors {
        let slot: &mut [u8; NeighborInfo::LEN] = (&mut buf[at..at + NeighborInfo::LEN])
            .try_into()
            .expect("slice is NeighborInfo::LEN");
        neighbor.encode_into(slot);
        at += NeighborInfo::LEN;
    }
    Ok(end)
}

// ---------------------------------------------------------------------------
// The requester: `TARGET_NODE_ID`, `NEIGHBOR_REQUEST_CB` and `NEIGHBOR_TIMER`
// ---------------------------------------------------------------------------

/// `bcmp_request_neighbor_table`'s `request` argument, as a choice rather than
/// a pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableRequestKind {
    /// `request == NULL`. A matching reply is still accepted — the timer is
    /// stopped — and then dropped. What `neighbors_test.cpp` passes on its
    /// failure cases; nothing in bm_core passes it in earnest.
    Ignore,
    /// `request != NULL`. A matching reply reaches the caller, **once**: the C
    /// clears `NEIGHBOR_REQUEST_CB` immediately after invoking it.
    /// `integrations/topology.c` is the only caller.
    Report,
}

/// What `bcmp_process_neighbor_table_reply` made of a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableReplyOutcome {
    /// `TARGET_NODE_ID != reply->node_id`. The C returns `BmENOTINTREC` having
    /// done nothing — not even stopped the timer.
    Rejected,
    /// Accepted with no callback armed: the timer is stopped and the reply is
    /// dropped. Either the request was made with [`TableRequestKind::Ignore`],
    /// or a reply already consumed the callback.
    Accepted,
    /// Accepted with a callback armed: the reply is reported, and the callback
    /// is disarmed so the next one is merely [`Self::Accepted`].
    Reported,
}

/// `bcmp/neighbors.c`'s three requester statics, which hold one request between
/// them.
///
/// `bcmp_request_neighbor_table` writes all three; `bcmp_process_neighbor_table_reply`
/// reads the first and clears the second. Reproduced here rather than tidied,
/// because three things about them are observable on the wire and to the
/// application:
///
/// * **`TARGET_NODE_ID` is an exact 64-bit match against the reply's *body*
///   `node_id`,** not against the address it arrived from and not against the
///   type of the request. It starts at zero and is never cleared, so a reply
///   claiming the last target is accepted for the life of the process.
/// * **Zero is a target like any other.** A node whose link-local address is
///   exactly `fe80::` replies with `node_id == 0` and matches, which is the
///   opposite of `bcmp_find_neighbor`'s refusal to match zero (divergence
///   #18). It also matches the zero this starts at, so such a reply is
///   accepted before any request has been made. A *broadcast* request is the
///   mirror image: `target_node_id == 0` asks every node, every node answers
///   with its own id, and none of those ids is zero. See divergence #35.
/// * **The timer expires nothing.** `NEIGHBOR_TIMER` fires the caller's
///   `timeout` and leaves `NEIGHBOR_REQUEST_CB` armed, so a reply arriving
///   afterwards is reported as though it had been on time. See divergence #36.
///
/// [`Self::on_timer`] is the timer; the caller owns the clock that drives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableRequests {
    target_node_id: u64,
    armed: bool,
    /// `NEIGHBOR_TIMER`'s start instant while it is running. The deadline
    /// rather than the instant would not survive a wrapping clock any better:
    /// [`time_remaining`] is what the C compares with.
    started_ms: Option<u32>,
}

impl Default for TableRequests {
    fn default() -> Self {
        Self::new()
    }
}

impl TableRequests {
    /// The state `bcmp/neighbors.c` starts a process in: no callback, no timer,
    /// and a `TARGET_NODE_ID` of zero that a reply claiming zero matches.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            target_node_id: 0,
            armed: false,
            started_ms: None,
        }
    }

    /// `TARGET_NODE_ID`: the node id a reply must claim to be accepted.
    #[must_use]
    pub const fn target_node_id(&self) -> u64 {
        self.target_node_id
    }

    /// Whether `NEIGHBOR_REQUEST_CB` is non-null — a reply is still owed to
    /// somebody.
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// Whether `NEIGHBOR_TIMER` is running.
    #[must_use]
    pub const fn timer_running(&self) -> bool {
        self.started_ms.is_some()
    }

    /// Milliseconds until [`Self::on_timer`] will fire, or `None` when the
    /// timer is not running.
    ///
    /// Zero means it is due now. This is `time_remaining`, bm_core's own
    /// wrap-safe comparison, against a start instant rather than a deadline —
    /// so a clock that has gone backwards reads as "not yet", as it does
    /// everywhere else in bm_core except `packet.c`.
    #[must_use]
    pub fn remaining_ms(&self, now_ms: u32) -> Option<u32> {
        self.started_ms
            .map(|started| time_remaining(started, now_ms, NEIGHBOR_REQUEST_TIMEOUT_MS))
    }

    /// `bcmp_request_neighbor_table`: name the target, re-arm the timer, record
    /// the callback.
    ///
    /// The C does all three **before** `bcmp_tx`, and undoes none of them if
    /// the transmit fails — unlike `bcmp_request_info`, which removes its list
    /// entry. A caller whose request never reached the wire therefore still
    /// gets a timeout a second later, and still accepts a reply. The old timer
    /// is deleted rather than stopped, so a previous request's deadline is
    /// gone whether or not it had been answered.
    pub fn record(&mut self, now_ms: u32, target_node_id: u64, kind: TableRequestKind) {
        self.target_node_id = target_node_id;
        self.armed = matches!(kind, TableRequestKind::Report);
        self.started_ms = Some(now_ms);
    }

    /// `bcmp_process_neighbor_table_reply`: the acceptance test and its
    /// effects.
    ///
    /// `reply_node_id` is [`NeighborTableReply::node_id`] — what the sender
    /// says it is, which the C trusts in preference to the frame's source
    /// address.
    pub fn accept(&mut self, reply_node_id: u64) -> TableReplyOutcome {
        if self.target_node_id != reply_node_id {
            return TableReplyOutcome::Rejected;
        }
        // `bm_timer_stop`, which the C calls whether or not the timer is
        // running and whether or not it was ever created.
        self.started_ms = None;
        if self.armed {
            self.armed = false;
            TableReplyOutcome::Reported
        } else {
            TableReplyOutcome::Accepted
        }
    }

    /// Run `NEIGHBOR_TIMER`, reporting whether it fired.
    ///
    /// One-shot: it fires at most once per [`Self::record`]. The deadline is
    /// held here rather than by the caller, so calling this early, late or
    /// twice changes nothing — only a call at or after the deadline fires it.
    ///
    /// **Nothing else changes.** The callback stays armed and
    /// `TARGET_NODE_ID` keeps its value, so a reply that arrives after this is
    /// still [`TableReplyOutcome::Reported`]. That is divergence #36, not an
    /// omission here.
    pub fn on_timer(&mut self, now_ms: u32) -> bool {
        if self.remaining_ms(now_ms) != Some(0) {
            return false;
        }
        self.started_ms = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_packed_c_structs() {
        assert_eq!(NeighborTableReply::HEADER_LEN, 11);
        assert_eq!(PortInfo::LEN, 2);
        assert_eq!(NeighborInfo::LEN, 10);
    }

    #[test]
    fn a_two_port_reply_round_trips() {
        let ports = [
            PortInfo {
                state: 1,
                port_type: 0,
            },
            PortInfo {
                state: 0,
                port_type: 0,
            },
        ];
        let neighbors = [
            NeighborInfo {
                node_id: 0xDEAD_BEEF_1234_5678,
                port: 1,
                online: 1,
            },
            NeighborInfo {
                node_id: 0x0000_0000_55AA_0011,
                port: 2,
                online: 0,
            },
        ];

        let mut buf = [0u8; 64];
        let len = encode_neighbor_table_reply(&mut buf, 0xC0FF_EE00_1234_5678, &ports, &neighbors)
            .unwrap();
        assert_eq!(len, neighbor_table_reply_len(2, 2));
        assert_eq!(len, 11 + 4 + 20);

        let reply = NeighborTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.node_id, 0xC0FF_EE00_1234_5678);
        assert_eq!(reply.port_count(), 2);
        assert_eq!(reply.neighbor_count(), 2);
        assert!(reply.ports().eq(ports.iter().copied()));
        assert!(reply.neighbors().eq(neighbors.iter().copied()));
        assert!(reply.ports().next().unwrap().is_up());
        assert!(!reply.ports().nth(1).unwrap().is_up());

        let mut again = [0u8; 64];
        assert_eq!(reply.encode(&mut again).unwrap(), len);
        assert_eq!(&again[..len], &buf[..len]);
    }

    #[test]
    fn an_empty_table_is_just_the_header() {
        let mut buf = [0u8; 16];
        let len = encode_neighbor_table_reply(&mut buf, 7, &[], &[]).unwrap();
        assert_eq!(len, NeighborTableReply::HEADER_LEN);
        let reply = NeighborTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.port_count(), 0);
        assert_eq!(reply.neighbor_count(), 0);
        assert_eq!(reply.ports().count(), 0);
        assert_eq!(reply.neighbors().count(), 0);
    }

    /// The counts are attacker-controlled, and the C multiplies them out and
    /// copies without ever looking at how many bytes arrived.
    #[test]
    fn declared_counts_are_checked_against_the_buffer() {
        let mut body = [0u8; NeighborTableReply::HEADER_LEN + 4];
        body[8] = 2; // two ports, exactly the four trailing bytes
        assert!(NeighborTableReply::decode(&body).is_ok());

        body[8] = 3; // six bytes' worth, only four arrived
        assert_eq!(
            NeighborTableReply::decode(&body),
            Err(BmWireError::Truncated)
        );

        // The worst case: both counts saturated on a minimum-size body. The C
        // would compute 255*2 + 65535*10 and memcpy 655 850 bytes out of a
        // frame that carried eleven.
        let mut minimal = [0u8; NeighborTableReply::HEADER_LEN];
        minimal[8] = u8::MAX;
        minimal[9..11].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(
            NeighborTableReply::decode(&minimal),
            Err(BmWireError::Truncated)
        );
    }

    #[test]
    fn trailing_bytes_past_the_arrays_are_ignored() {
        let mut buf = [0u8; 64];
        let len = encode_neighbor_table_reply(
            &mut buf,
            1,
            &[PortInfo {
                state: 1,
                port_type: 0,
            }],
            &[],
        )
        .unwrap();
        buf[len..len + 8].fill(0xA5);
        let reply = NeighborTableReply::decode(&buf[..len + 8]).unwrap();
        assert_eq!(reply.encoded_len(), len);
        assert_eq!(reply.port_count(), 1);
    }

    #[test]
    fn a_port_state_byte_that_is_not_zero_or_one_survives_a_round_trip() {
        // The C reads this field as a `bool`, for which any other value is out
        // of range. Keeping the raw byte means re-encoding is byte-exact.
        let ports = [PortInfo {
            state: 0x42,
            port_type: 0x99,
        }];
        let mut buf = [0u8; 32];
        let len = encode_neighbor_table_reply(&mut buf, 0, &ports, &[]).unwrap();
        let reply = NeighborTableReply::decode(&buf[..len]).unwrap();
        assert_eq!(reply.ports().next().unwrap(), ports[0]);
        assert!(reply.ports().next().unwrap().is_up());
    }

    #[test]
    fn short_buffers_are_rejected_at_every_length() {
        for len in 0..NeighborTableReply::HEADER_LEN {
            assert_eq!(
                NeighborTableReply::decode(&[0u8; NeighborTableReply::HEADER_LEN][..len]),
                Err(BmWireError::Truncated)
            );
        }
        let mut tiny = [0u8; 4];
        assert_eq!(
            encode_neighbor_table_reply(&mut tiny, 0, &[], &[]),
            Err(BmWireError::Truncated)
        );
    }

    /// `bcmp_send_neighbor_table` refuses to answer at all above
    /// `bcmp_table_max_len`. On a two-port node the cliff is between 100 and
    /// 101 neighbours.
    #[test]
    fn the_reply_size_ceiling_falls_where_the_c_puts_it() {
        assert_eq!(NEIGHBOR_TABLE_MAX_LEN, 1024);
        assert_eq!(neighbor_table_reply_len(2, 100), 1015);
        assert_eq!(neighbor_table_reply_len(2, 101), 1025);
        assert!(neighbor_table_reply_len(2, 100) <= NEIGHBOR_TABLE_MAX_LEN);
        assert!(neighbor_table_reply_len(2, 101) > NEIGHBOR_TABLE_MAX_LEN);
    }

    #[test]
    fn a_fresh_requester_is_waiting_for_nothing_from_node_zero() {
        let requests = TableRequests::new();
        assert_eq!(requests.target_node_id(), 0);
        assert!(!requests.is_armed());
        assert!(!requests.timer_running());
        assert_eq!(requests.remaining_ms(0), None);
    }

    /// `TARGET_NODE_ID` starts at zero, so a reply claiming node id zero is
    /// accepted before anything has been asked. Nothing is reported, because
    /// `NEIGHBOR_REQUEST_CB` is null — but the C does reach `bm_timer_stop`.
    #[test]
    fn a_reply_claiming_node_zero_is_accepted_before_any_request() {
        let mut requests = TableRequests::new();
        assert_eq!(requests.accept(0), TableReplyOutcome::Accepted);
        assert_eq!(requests.accept(1), TableReplyOutcome::Rejected);
    }

    #[test]
    fn a_reported_reply_disarms_the_callback_and_stops_the_timer() {
        let mut requests = TableRequests::new();
        requests.record(100, 0xAA, TableRequestKind::Report);
        assert!(requests.is_armed());
        assert_eq!(requests.remaining_ms(100), Some(1000));

        assert_eq!(requests.accept(0xAA), TableReplyOutcome::Reported);
        assert!(!requests.is_armed());
        assert!(!requests.timer_running());

        // Still the target, so still accepted -- but there is nobody left to
        // report it to.
        assert_eq!(requests.accept(0xAA), TableReplyOutcome::Accepted);
    }

    /// The whole of `bcmp_request_neighbor_table(NULL, ...)`: the reply is
    /// accepted, and goes nowhere.
    #[test]
    fn a_request_with_no_callback_accepts_without_reporting() {
        let mut requests = TableRequests::new();
        requests.record(0, 0xAA, TableRequestKind::Ignore);
        assert!(!requests.is_armed());
        assert!(requests.timer_running());
        assert_eq!(requests.accept(0xAA), TableReplyOutcome::Accepted);
    }

    /// A rejected reply leaves the timer alone. The C returns before
    /// `bm_timer_stop`, so somebody else's reply cannot extend or cancel this
    /// node's wait.
    #[test]
    fn a_rejected_reply_leaves_the_timer_running() {
        let mut requests = TableRequests::new();
        requests.record(0, 0xAA, TableRequestKind::Report);
        assert_eq!(requests.accept(0xBB), TableReplyOutcome::Rejected);
        assert!(requests.is_armed());
        assert_eq!(requests.remaining_ms(500), Some(500));
    }

    /// Divergence #36: the timeout fires, and the request is still armed
    /// behind it.
    #[test]
    fn the_timeout_fires_once_and_gives_up_on_nothing() {
        let mut requests = TableRequests::new();
        requests.record(0, 0xAA, TableRequestKind::Report);

        assert!(!requests.on_timer(999), "one millisecond early");
        assert_eq!(requests.remaining_ms(999), Some(1));
        assert!(requests.on_timer(1000), "due at exactly the period");
        assert!(!requests.on_timer(5000), "and one-shot");

        assert!(requests.is_armed(), "the callback outlives its timeout");
        assert_eq!(
            requests.accept(0xAA),
            TableReplyOutcome::Reported,
            "a reply an hour late is reported as an answer"
        );
    }

    /// A second request deletes the first's timer and takes its slot, so the
    /// first can no longer time out and its callback is gone.
    #[test]
    fn a_second_request_replaces_the_first_whole() {
        let mut requests = TableRequests::new();
        requests.record(0, 0xAA, TableRequestKind::Report);
        requests.record(600, 0xBB, TableRequestKind::Ignore);

        assert_eq!(requests.target_node_id(), 0xBB);
        assert!(!requests.is_armed());
        assert_eq!(requests.accept(0xAA), TableReplyOutcome::Rejected);
        assert!(
            !requests.on_timer(1000),
            "the first request's deadline went with its timer"
        );
        assert!(requests.on_timer(1600));
    }

    /// `TARGET_NODE_ID` is compared whole, unlike `INFO_REQUEST_LIST`'s
    /// 32-bit key (divergence #33).
    #[test]
    fn the_target_is_matched_on_all_sixty_four_bits() {
        let mut requests = TableRequests::new();
        requests.record(0, 0xDEAD_BEEF_55AA_0011, TableRequestKind::Report);
        assert_eq!(
            requests.accept(0x0000_0000_55AA_0011),
            TableReplyOutcome::Rejected,
            "the low half agreeing is not enough"
        );
        assert_eq!(
            requests.accept(0xDEAD_BEEF_55AA_0011),
            TableReplyOutcome::Reported
        );
    }

    /// The timer is armed against a wrapping millisecond clock, so a request
    /// made just before the wrap still times out a second later.
    #[test]
    fn the_timeout_survives_the_clock_wrapping() {
        let mut requests = TableRequests::new();
        let started = u32::MAX - 500;
        requests.record(started, 0xAA, TableRequestKind::Report);
        assert_eq!(requests.remaining_ms(started), Some(1000));
        // The deadline wrapped to 499. 701 ms in, and still waiting.
        assert!(!requests.on_timer(200));
        assert_eq!(requests.remaining_ms(200), Some(299));
        assert!(requests.on_timer(499));
    }
}
