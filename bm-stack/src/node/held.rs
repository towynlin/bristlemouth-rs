//! The node's frame bookkeeping: the bytes a relay restores, the one ping it
//! remembers, and the frames its outstanding requests were sent in.

use bm_wire::bcmp::BCMP_HEADER_OFFSET;
use bm_wire::frame::{IPV6_INGRESS_EGRESS_PORTS_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET};

use super::MTU;

/// The bytes `bm_wire::bcmp::rx::accept` rewrites, saved so a frame can still be
/// relayed after it has been parsed.
///
/// bm_core copies the frame for forwarding *before* it submits it, so the
/// forwarded copy carries the legacy port bytes and the checksum exactly as
/// they arrived — none of `process_received_message`'s clears. Reproducing that
/// without a second MTU-sized buffer means putting the five bytes back.
#[derive(Debug, Clone, Copy)]
pub(super) struct Snapshot {
    ports: u8,
    legacy: [u8; 2],
    checksum: [u8; 2],
}

/// Offset of the first byte `clear_ports_legacy` zeroes, from
/// `bm_wire::bcmp::rx`.
const LEGACY_PORT_CLEAR_OFFSET: usize = IPV6_SOURCE_ADDRESS_OFFSET + 4;

impl Snapshot {
    /// `None` for a frame too short to hold all five bytes — which is also too
    /// short for `accept` to have rewritten any of them. `accept` refuses
    /// anything whose IPv6 payload length is under 13, and that needs 67 bytes,
    /// so a frame this cannot snapshot is a frame it has nothing to restore.
    pub(super) fn take(frame: &[u8]) -> Option<Self> {
        let checksum_offset = BCMP_HEADER_OFFSET + bm_wire::bcmp::CHECKSUM_FIELD_OFFSET;
        Some(Self {
            ports: *frame.get(IPV6_INGRESS_EGRESS_PORTS_OFFSET)?,
            legacy: [
                *frame.get(LEGACY_PORT_CLEAR_OFFSET)?,
                *frame.get(LEGACY_PORT_CLEAR_OFFSET + 1)?,
            ],
            checksum: [
                *frame.get(checksum_offset)?,
                *frame.get(checksum_offset + 1)?,
            ],
        })
    }

    pub(super) fn restore(self, frame: &mut [u8]) {
        let checksum_offset = BCMP_HEADER_OFFSET + bm_wire::bcmp::CHECKSUM_FIELD_OFFSET;
        frame[IPV6_INGRESS_EGRESS_PORTS_OFFSET] = self.ports;
        frame[LEGACY_PORT_CLEAR_OFFSET] = self.legacy[0];
        frame[LEGACY_PORT_CLEAR_OFFSET + 1] = self.legacy[1];
        frame[checksum_offset] = self.checksum[0];
        frame[checksum_offset + 1] = self.checksum[1];
    }
}

/// `bcmp/ping.c`'s four file-scope statics, which track exactly one
/// outstanding ping.
///
/// A second [`Node::ping`] overwrites the first's expectations, as
/// `bcmp_send_ping_request` does by freeing and reallocating
/// `EXPECTED_PAYLOAD`. Nothing else ever clears them, so the last ping's
/// payload keeps answering for as long as the node runs.
#[derive(Debug)]
pub(super) struct PingState<'r> {
    /// `BCMP_SEQ`. A `uint32_t` counter whose low sixteen bits are what
    /// reaches the wire, and a sequence space entirely separate from
    /// `packet.c`'s `message_count`.
    pub(super) seq: u32,
    /// `PING_REQUEST_TIMEOUT`, which despite the name times nothing out: it is
    /// stamped after every request and read only to report a round-trip. See
    /// divergence #32.
    pub(super) sent_at_ms: u32,
    /// `EXPECTED_PAYLOAD_LEN`, and `None` for the `EXPECTED_PAYLOAD == NULL`
    /// the C starts in and returns to on a payload-free request. The C's two
    /// statics are only ever set and cleared together, so one field holds both.
    expected_len: Option<u16>,
    /// `EXPECTED_PAYLOAD`'s bytes, as much of them as is worth keeping.
    pub(super) expected: &'r mut [u8],
}

impl<'r> PingState<'r> {
    pub(super) fn new(expected: &'r mut [u8]) -> Self {
        Self {
            seq: 0,
            sent_at_ms: 0,
            expected_len: None,
            expected,
        }
    }

    /// What `bcmp_process_ping_reply` compares against, or `None` for the C's
    /// null pointer.
    pub(super) fn expected_payload(&self) -> Option<&[u8]> {
        self.expected_len
            .map(|len| &self.expected[..usize::from(len)])
    }

    /// `bcmp_send_ping_request`'s clear-then-copy: the old expectation goes
    /// whether or not a new one replaces it, and an empty payload leaves the
    /// pointer null.
    pub(super) fn remember(&mut self, payload: &[u8]) {
        self.expected_len = None;
        if !payload.is_empty() {
            self.expected[..payload.len()].copy_from_slice(payload);
            self.expected_len = Some(payload.len() as u16);
        }
    }
}

/// One tracked request's frame, as it was first built.
#[derive(Debug)]
pub(super) struct HeldRequest {
    seq_num: u32,
    /// Zero for an empty slot: no frame is empty.
    pub(super) len: usize,
    frame: [u8; MTU],
}

impl HeldRequest {
    pub(super) const EMPTY: Self = Self {
        seq_num: 0,
        len: 0,
        frame: [0; MTU],
    };
}

/// The frames of tracked requests, kept for re-sending.
///
/// `serialize` records each sequenced request's buffer in its
/// `BcmpRequestElement` and takes a reference to it, and `timer_traverse_cb`
/// hands that same buffer back to the IP layer on each retry. The C's buffer
/// comes back out of L2 unchanged apart from destination byte 13, which
/// `bm_l2_link_output` reads as an application egress port and clears; each
/// port's egress stamp and checksum patch is reverted after it is sent. For a
/// destination without an egress port — every address bm_core's own request
/// sites use — a retry is the original frame, and this keeps the original
/// frame.
///
/// One slot per entry the registry can hold, so a tracked request always has
/// somewhere to go.
#[derive(Debug)]
pub(super) struct HeldRequests<'r> {
    slots: &'r mut [HeldRequest],
    /// Sequence numbers owed a re-send, in the order the sweep retried them.
    /// As long as `slots`.
    due: &'r mut [u32],
    due_len: usize,
}

impl<'r> HeldRequests<'r> {
    pub(super) fn new(slots: &'r mut [HeldRequest], due: &'r mut [u32]) -> Self {
        Self {
            slots,
            due,
            due_len: 0,
        }
    }

    fn slot(&self, seq_num: u32) -> Option<usize> {
        self.slots
            .iter()
            .position(|held| held.len != 0 && held.seq_num == seq_num)
    }

    /// Keep `frame` as the request numbered `seq_num`.
    pub(super) fn hold(&mut self, seq_num: u32, frame: &[u8]) {
        if let Some(held) = self.slots.iter_mut().find(|held| held.len == 0) {
            held.seq_num = seq_num;
            held.len = frame.len();
            held.frame[..frame.len()].copy_from_slice(frame);
        }
    }

    /// The request is answered or timed out: `sequence_list_remove_message`
    /// and `timer_traverse_cb` both drop their reference to the buffer.
    pub(super) fn release(&mut self, seq_num: u32) {
        if let Some(index) = self.slot(seq_num) {
            self.slots[index].len = 0;
        }
        self.undue(seq_num);
    }

    fn undue(&mut self, seq_num: u32) {
        if let Some(index) = self.due[..self.due_len].iter().position(|s| *s == seq_num) {
            self.due.copy_within(index + 1..self.due_len, index);
            self.due_len -= 1;
        }
    }

    /// The sweep retried this request. A request already owed a re-send is
    /// not owed two: that only happens when the caller skips
    /// [`Node::next_retransmission`] between sweeps.
    pub(super) fn mark_due(&mut self, seq_num: u32) {
        if self.slot(seq_num).is_some()
            && !self.due[..self.due_len].contains(&seq_num)
            && self.due_len < self.due.len()
        {
            self.due[self.due_len] = seq_num;
            self.due_len += 1;
        }
    }

    /// The next frame owed a re-send, taken off the queue.
    pub(super) fn next_due(&mut self) -> Option<&mut [u8]> {
        if self.due_len == 0 {
            return None;
        }
        let seq_num = self.due[0];
        self.undue(seq_num);
        let index = self.slot(seq_num)?;
        let held = &mut self.slots[index];
        Some(&mut held.frame[..held.len])
    }
}

/// What `serialize` decided for an outgoing message.
#[derive(Debug, Clone, Copy)]
pub(super) struct Stamp {
    pub(super) seq_num: u32,
    pub(super) mask: u16,
    /// Recorded as an outstanding request, so its frame is kept for retries.
    pub(super) tracked: bool,
}
