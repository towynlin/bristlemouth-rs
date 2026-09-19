//! The packet registry and the sequenced-request machinery, ported from the
//! state around `serialize` and `process_received_message` in `bcmp/packet.c`.
//!
//! [`tx::serialize`][crate::bcmp::tx::serialize] and
//! [`rx::accept`][crate::bcmp::rx::accept] are the wire format. This is the
//! state that decides what they are called with: which message types exist,
//! what sequence number an outgoing message carries, which outstanding
//! requests a received reply may answer, and when an unanswered request is
//! given up on.
//!
//! Written *sans-io*, like [`crate::neighbor`]: no clock, no timers, no
//! transmission. [`Registry::on_tick`] takes the current time and reports what
//! expired; the caller owns the timer that drives it.
//!
//! # Three pieces of C state, in one struct
//!
//! * `PACKET.packet_list`, the type → [`PacketCfg`] registry that
//!   `packet_add`/`packet_remove` maintain;
//! * `message_count`, the outgoing sequence counter — a function-level
//!   `static` inside `serialize`, incremented **only** for a
//!   `sequenced_request` type;
//! * `PACKET.sequence_list`, the outstanding requests, swept by a 150 ms
//!   auto-reload timer.
//!
//! # Callbacks become return values
//!
//! The C stores a `BcmpSequencedRequestCb` per request and invokes it in two
//! places: with the reply's payload when one arrives, and — this is the part
//! that is easy to miss — **with `NULL` when the request times out**. A caller
//! that does not test for the null payload dereferences it.
//!
//! The port has no callback to store. A matched reply comes back as
//! [`Delivery::SequencedReply`] and an expiry as a call to `on_tick`'s
//! `timed_out`, so the two cases cannot be confused for one another. Where the
//! C falls back to `cfg->process` because the stored callback was null, the
//! port's caller does the same by choosing not to handle the entry it was
//! handed.
//!
//! # Time
//!
//! Milliseconds, as everywhere else here. bm_core counts RTOS ticks and
//! converts with `bm_ticks_to_ms`, the identity on every backend in the tree.

use crate::bcmp::header::MessageType;

/// The timeout stamped into every sequenced request, `default_message_timeout_ms`
/// at `packet.c:11`.
///
/// It is **not** how long a request actually survives. See
/// [`MESSAGE_TIMER_EXPIRY_PERIOD_MS`] and divergence #22.
pub const DEFAULT_MESSAGE_TIMEOUT_MS: u32 = 24;

/// How often the C's expiry timer runs, `message_timer_expiry_period_ms` at
/// `packet.c:12`.
///
/// The sweep is the only thing that ever expires a request, so this — not
/// [`DEFAULT_MESSAGE_TIMEOUT_MS`] — sets the granularity. A request survives
/// until the first sweep that lands more than [`DEFAULT_MESSAGE_TIMEOUT_MS`]
/// after it was sent, which is between 25 ms and 174 ms depending on where it
/// fell in the sweep's phase. Divergence #22 has the measurement.
pub const MESSAGE_TIMER_EXPIRY_PERIOD_MS: u32 = 150;

/// How a message type is sequenced, mirroring `BcmpPacketCfg` minus its
/// `process` function pointer.
///
/// The two flags are not exclusive, and the C reads them in a particular
/// order: `serialize` tests `sequenced_reply` first, so a type with both set
/// echoes the caller's number and never allocates a sequence number of its
/// own, while `process_received_message` requires `sequenced_reply &&
/// !sequenced_request` before it will match a reply — so a type with both set
/// can neither create nor consume an outstanding request. Nothing upstream
/// sets both; `bcmp/config.c` is the only module that sets `sequenced_request`
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacketCfg {
    /// This type is a reply: `serialize` stamps the caller's sequence number
    /// into the header rather than allocating one.
    pub sequenced_reply: bool,
    /// This type is a request: `serialize` allocates the next sequence number
    /// and records the request as outstanding.
    pub sequenced_request: bool,
}

impl PacketCfg {
    /// Neither sequenced: the header's sequence number is written as zero.
    pub const UNSEQUENCED: Self = Self {
        sequenced_reply: false,
        sequenced_request: false,
    };
    /// A request, tracked until its reply arrives or it times out.
    pub const REQUEST: Self = Self {
        sequenced_reply: false,
        sequenced_request: true,
    };
    /// A reply, which may answer an outstanding request.
    pub const REPLY: Self = Self {
        sequenced_reply: true,
        sequenced_request: false,
    };
}

/// One outstanding request, mirroring `BcmpRequestElement`.
///
/// The C carries a fifth field, `type`, which is written by
/// `new_sequence_list_item` and **never read**: a reply is matched on its
/// sequence number alone. [`message_type`][Self::message_type] is kept here
/// for the same reason the C keeps it — it is what the caller needs in order
/// to know what it asked — but matching ignores it, exactly as the C does.
/// See divergence #21.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PendingRequest {
    /// The type of the request that was sent.
    pub message_type: MessageType,
    /// The sequence number stamped into its header.
    pub seq_num: u32,
    /// When it was sent.
    pub timestamp_ms: u32,
    /// How long it may go unanswered, always [`DEFAULT_MESSAGE_TIMEOUT_MS`] as
    /// the C calls it.
    pub timeout_ms: u32,
}

impl PendingRequest {
    /// Whether this request has outlived its timeout at `now_ms`.
    ///
    /// The C is `bm_ticks_to_ms(bm_get_tick_count()) - element->timestamp_ms >
    /// element->timeout_ms`: an unsigned 32-bit subtraction, and a *strict*
    /// comparison, so a request is still live at exactly `timeout_ms` and dies
    /// one millisecond later. Note that this is not
    /// [`time_remaining`][crate::util::time_remaining], which the rest of
    /// bm_core uses for the same job: that one casts to `int32_t` and so
    /// treats a clock that has gone backwards as "not yet". Here a `now_ms`
    /// before `timestamp_ms` wraps to a huge difference and the request
    /// expires immediately.
    #[must_use]
    pub const fn expired_at(&self, now_ms: u32) -> bool {
        now_ms.wrapping_sub(self.timestamp_ms) > self.timeout_ms
    }
}

/// Why the registry refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RegistryError {
    /// No [`PacketCfg`] is registered for this message type.
    ///
    /// The C's `serialize` writes **nothing at all** in this case — no header,
    /// no body, no checksum — and returns `BmENODEV`, leaving the caller's
    /// buffer as it found it. A caller must not transmit.
    UnknownType,
    /// The registry is full. bm_core allocates, so it has no such limit.
    Full,
}

impl core::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::UnknownType => "no packet configuration for this message type",
            Self::Full => "registry full",
        })
    }
}

/// What to stamp into an outgoing message's header, and whether a reply to it
/// will be matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outgoing {
    /// The sequence number for
    /// [`tx::serialize`][crate::bcmp::tx::serialize].
    pub seq_num: u32,
    /// Whether an outstanding request was recorded, so that a reply carrying
    /// `seq_num` will come back as [`Delivery::SequencedReply`] and a silent
    /// peer will be reported to `on_tick`'s `timed_out`.
    ///
    /// False for replies and unsequenced messages — and also for a request the
    /// pending table had no room for. The C's equivalent is a failed
    /// `bm_malloc` inside `sequence_list_add_message`, whose result `serialize`
    /// discards: the request goes out on the wire untracked, and its reply is
    /// then handled as an ordinary message. The port does the same rather than
    /// refusing to send.
    pub tracked: bool,
}

/// What a received message should be done with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Nothing is registered for this type. The C returns `BmENODEV` without
    /// dispatching.
    Unregistered,
    /// This reply answers an outstanding request, which has been removed.
    ///
    /// The caller invokes whatever it associated with that request. If it has
    /// nothing — the C's null `cb` — it falls back to handling the message as
    /// [`Delivery::Process`] would, which is what the C does.
    SequencedReply(PendingRequest),
    /// Hand it to the type's own processor.
    Process,
}

/// The type registry and the outstanding-request list.
///
/// `TYPES` is how many message types may be registered and `PENDING` how many
/// requests may be outstanding at once. bm_core keeps both in `bm_malloc`'d
/// linked lists; these are fixed-capacity arrays, because `bm-wire` has no
/// allocator.
#[derive(Debug, Clone)]
pub struct Registry<const TYPES: usize, const PENDING: usize> {
    types: [(MessageType, PacketCfg); TYPES],
    types_len: usize,
    pending: [PendingRequest; PENDING],
    pending_len: usize,
    message_count: u32,
    next_sweep_ms: u32,
}

impl<const TYPES: usize, const PENDING: usize> Default for Registry<TYPES, PENDING> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const TYPES: usize, const PENDING: usize> Registry<TYPES, PENDING> {
    /// An empty registry whose expiry sweep is phased from time zero.
    ///
    /// The C arms its timer in `packet_init`, so the sweeps land at multiples
    /// of [`MESSAGE_TIMER_EXPIRY_PERIOD_MS`] measured from bring-up. A node
    /// that brings the registry up at a non-zero time should use
    /// [`Registry::started_at`] so the phase matches.
    #[must_use]
    pub const fn new() -> Self {
        Self::resuming(0, MESSAGE_TIMER_EXPIRY_PERIOD_MS)
    }

    /// The same, with the expiry sweep phased from `now_ms` — the equivalent
    /// of `packet_init` running at `now_ms` rather than at zero.
    #[must_use]
    pub const fn started_at(now_ms: u32) -> Self {
        Self::resuming(0, now_ms.wrapping_add(MESSAGE_TIMER_EXPIRY_PERIOD_MS))
    }

    /// An empty registry lined up with a `packet.c` that is already running.
    ///
    /// `sequence_count` is where its `message_count` has got to and
    /// `next_sweep_ms` is the millisecond its expiry timer next fires on. Both
    /// are per-*process* state in the C — `message_count` is a function-level
    /// `static` inside `serialize`, and the timer is armed once in
    /// `packet_init` — and nothing resets either. A differential harness
    /// comparing a fresh registry against a long-lived oracle has to start
    /// from where the C is; firmware wants [`Registry::new`] or
    /// [`Registry::started_at`].
    #[must_use]
    pub const fn resuming(sequence_count: u32, next_sweep_ms: u32) -> Self {
        Self {
            types: [(MessageType(0), PacketCfg::UNSEQUENCED); TYPES],
            types_len: 0,
            pending: [PendingRequest {
                message_type: MessageType(0),
                seq_num: 0,
                timestamp_ms: 0,
                timeout_ms: 0,
            }; PENDING],
            pending_len: 0,
            message_count: sequence_count,
            next_sweep_ms,
        }
    }

    /// Register a configuration for `message_type`, as `packet_add` does.
    ///
    /// **Duplicates are allowed**, because `ll_item_add` appends without
    /// looking at the id. Registering a type twice leaves two entries; lookups
    /// and [`Registry::remove`] find the first, so the second is dead until the
    /// first is removed.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Full`] when `TYPES` entries are already registered.
    pub fn add(&mut self, message_type: MessageType, cfg: PacketCfg) -> Result<(), RegistryError> {
        if self.types_len == TYPES {
            return Err(RegistryError::Full);
        }
        self.types[self.types_len] = (message_type, cfg);
        self.types_len += 1;
        Ok(())
    }

    /// Remove the first registration for `message_type`, as `packet_remove`
    /// does. Reports whether there was one.
    pub fn remove(&mut self, message_type: MessageType) -> bool {
        let Some(index) = self.types[..self.types_len]
            .iter()
            .position(|(ty, _)| *ty == message_type)
        else {
            return false;
        };
        self.types.copy_within(index + 1..self.types_len, index);
        self.types_len -= 1;
        true
    }

    /// The configuration registered for `message_type`, if any.
    #[must_use]
    pub fn cfg(&self, message_type: MessageType) -> Option<PacketCfg> {
        self.types[..self.types_len]
            .iter()
            .find(|(ty, _)| *ty == message_type)
            .map(|(_, cfg)| *cfg)
    }

    /// The registered types, in registration order.
    pub fn registrations(&self) -> impl Iterator<Item = (MessageType, PacketCfg)> + '_ {
        self.types[..self.types_len].iter().copied()
    }

    /// The outstanding requests, in the order they were sent.
    pub fn pending(&self) -> impl Iterator<Item = &PendingRequest> + '_ {
        self.pending[..self.pending_len].iter()
    }

    /// How many requests are outstanding.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending_len
    }

    /// The next sequence number a request would be given.
    #[must_use]
    pub fn sequence_count(&self) -> u32 {
        self.message_count
    }

    /// Decide what an outgoing message's header carries, and record it if it
    /// is a request. The registry half of the C's `serialize`.
    ///
    /// `reply_seq_num` is the number being echoed; it is used only when the
    /// type is a [`PacketCfg::sequenced_reply`] and ignored otherwise — the C
    /// takes the same argument and ignores it in the same cases.
    ///
    /// # Errors
    ///
    /// [`RegistryError::UnknownType`] when nothing is registered for
    /// `message_type`, in which case the caller must transmit nothing: the C
    /// leaves its buffer untouched.
    pub fn on_serialize(
        &mut self,
        now_ms: u32,
        message_type: MessageType,
        reply_seq_num: u32,
    ) -> Result<Outgoing, RegistryError> {
        let cfg = self.cfg(message_type).ok_or(RegistryError::UnknownType)?;

        // The C's order: reply first, so a type with both flags set is a reply.
        if cfg.sequenced_reply {
            return Ok(Outgoing {
                seq_num: reply_seq_num,
                tracked: false,
            });
        }
        if !cfg.sequenced_request {
            return Ok(Outgoing {
                seq_num: 0,
                tracked: false,
            });
        }

        let seq_num = self.message_count;
        // The counter moves whether or not the request can be recorded: the C
        // increments it in the header assignment, before it tries to allocate.
        self.message_count = self.message_count.wrapping_add(1);

        let tracked = self.pending_len < PENDING;
        if tracked {
            self.pending[self.pending_len] = PendingRequest {
                message_type,
                seq_num,
                timestamp_ms: now_ms,
                timeout_ms: DEFAULT_MESSAGE_TIMEOUT_MS,
            };
            self.pending_len += 1;
        }
        Ok(Outgoing { seq_num, tracked })
    }

    /// Decide what to do with a received message, consuming the outstanding
    /// request it answers if it answers one. The registry half of
    /// `process_received_message`.
    ///
    /// **The match is on `seq_num` alone.** The C stores the request's type in
    /// its sequence entry and never compares it, so a reply of one type
    /// answers a request of another whenever the numbers line up; the port
    /// reproduces that. See divergence #21.
    pub fn on_received(&mut self, message_type: MessageType, seq_num: u32) -> Delivery {
        let Some(cfg) = self.cfg(message_type) else {
            return Delivery::Unregistered;
        };
        if !(cfg.sequenced_reply && !cfg.sequenced_request) {
            return Delivery::Process;
        }
        let Some(index) = self.pending[..self.pending_len]
            .iter()
            .position(|request| request.seq_num == seq_num)
        else {
            return Delivery::Process;
        };
        let request = self.pending[index];
        self.remove_pending(index);
        Delivery::SequencedReply(request)
    }

    /// Run the expiry sweep if it is due, reporting every request that has
    /// timed out.
    ///
    /// This is `sequence_list_timer_callback`, phase and all. The C arms a
    /// 150 ms auto-reload timer in `packet_init` and sweeps only when it
    /// fires, so a request does not expire at [`DEFAULT_MESSAGE_TIMEOUT_MS`] —
    /// it expires at the first sweep more than that many milliseconds after it
    /// was sent. Reproducing the phase is what makes the port time out the
    /// same requests at the same moments as a C node; see divergence #22.
    ///
    /// `timed_out` is the `cb(NULL)` path: each request is reported once, in
    /// list order, and is gone by the time the next one is reported.
    ///
    /// Call it as often as you like — nothing happens between sweeps — and at
    /// least once per [`MESSAGE_TIMER_EXPIRY_PERIOD_MS`].
    pub fn on_tick(&mut self, now_ms: u32, timed_out: impl FnMut(&PendingRequest)) {
        if (now_ms.wrapping_sub(self.next_sweep_ms) as i32) < 0 {
            return;
        }
        // Catching up on missed sweeps costs nothing: they would all run at
        // this same `now_ms`, and the first one takes everything the rest
        // would have. Advancing the phase by whole periods is what keeps the
        // port firing on the C's schedule rather than on its own.
        let missed = now_ms.wrapping_sub(self.next_sweep_ms) / MESSAGE_TIMER_EXPIRY_PERIOD_MS;
        self.next_sweep_ms = self
            .next_sweep_ms
            .wrapping_add(MESSAGE_TIMER_EXPIRY_PERIOD_MS.wrapping_mul(missed.wrapping_add(1)));
        self.sweep(now_ms, timed_out);
    }

    /// One pass of `ll_traverse(&PACKET.sequence_list, timer_traverse_cb)`.
    fn sweep(&mut self, now_ms: u32, mut timed_out: impl FnMut(&PendingRequest)) {
        let mut index = 0;
        while index < self.pending_len {
            if self.pending[index].expired_at(now_ms) {
                let request = self.pending[index];
                timed_out(&request);
                self.remove_pending(index);
            } else {
                index += 1;
            }
        }
    }

    fn remove_pending(&mut self, index: usize) {
        self.pending.copy_within(index + 1..self.pending_len, index);
        self.pending_len -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: MessageType = MessageType::NEIGHBOR_PROTO_REQUEST;
    const REPLY: MessageType = MessageType::NEIGHBOR_PROTO_REPLY;

    fn registry() -> Registry<8, 8> {
        let mut registry = Registry::new();
        registry
            .add(MessageType::HEARTBEAT, PacketCfg::UNSEQUENCED)
            .unwrap();
        registry.add(REQUEST, PacketCfg::REQUEST).unwrap();
        registry.add(REPLY, PacketCfg::REPLY).unwrap();
        registry
    }

    /// What a sweep reported. An array rather than a `Vec`, because these
    /// tests run with `std` off as well as on.
    #[derive(Default)]
    struct Expired {
        seq_nums: [u32; 8],
        len: usize,
    }

    impl Expired {
        fn as_slice(&self) -> &[u32] {
            &self.seq_nums[..self.len]
        }
    }

    fn expired(registry: &mut Registry<8, 8>, now_ms: u32) -> Expired {
        let mut out = Expired::default();
        registry.on_tick(now_ms, |request| {
            out.seq_nums[out.len] = request.seq_num;
            out.len += 1;
        });
        out
    }

    /// bm_core's own `Packet.sequence_request` asserts the header's sequence
    /// number equals the loop index for every request in a run of them, and
    /// that a second run continues from where the first stopped rather than
    /// restarting. Those are the literal values, not merely agreement with the
    /// C.
    #[test]
    fn the_sequence_counter_starts_at_zero_and_never_restarts() {
        let mut registry = registry();
        for i in 0..64u32 {
            let out = registry.on_serialize(0, REQUEST, 0xDEAD_BEEF).unwrap();
            assert_eq!(out.seq_num, i, "gtest asserts seq_num == i");
            assert!(out.tracked);
            // Keep the table from filling: answer each one immediately.
            assert!(matches!(
                registry.on_received(REPLY, i),
                Delivery::SequencedReply(_)
            ));
        }
        for i in 64..128u32 {
            assert_eq!(registry.on_serialize(0, REQUEST, 0).unwrap().seq_num, i);
            registry.on_received(REPLY, i);
        }
    }

    /// The other half of the same gtest: a reply consumes the request, and a
    /// reply that matches nothing goes to the type's own processor.
    #[test]
    fn a_reply_consumes_the_request_it_answers_and_nothing_else() {
        let mut registry = registry();
        let first = registry.on_serialize(0, REQUEST, 0).unwrap().seq_num;
        let second = registry.on_serialize(0, REQUEST, 0).unwrap().seq_num;
        assert_eq!(registry.pending_len(), 2);

        // Out of order, which the C allows: the list is searched, not popped.
        match registry.on_received(REPLY, second) {
            Delivery::SequencedReply(request) => {
                assert_eq!(request.seq_num, second);
                assert_eq!(request.message_type, REQUEST);
                assert_eq!(request.timeout_ms, DEFAULT_MESSAGE_TIMEOUT_MS);
            }
            other => panic!("expected the second request back, got {other:?}"),
        }
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(registry.on_received(REPLY, second), Delivery::Process);
        assert!(matches!(
            registry.on_received(REPLY, first),
            Delivery::SequencedReply(_)
        ));
        assert_eq!(registry.pending_len(), 0);
    }

    #[test]
    fn a_reply_type_is_stamped_with_the_number_it_is_answering() {
        let mut registry = registry();
        let out = registry.on_serialize(0, REPLY, 0x1234_5678).unwrap();
        assert_eq!(out.seq_num, 0x1234_5678);
        assert!(!out.tracked, "a reply is not tracked");
        assert_eq!(
            registry.sequence_count(),
            0,
            "and it does not move the counter"
        );
    }

    #[test]
    fn an_unsequenced_type_is_stamped_with_zero() {
        let mut registry = registry();
        let out = registry
            .on_serialize(0, MessageType::HEARTBEAT, 0x1234_5678)
            .unwrap();
        assert_eq!(out.seq_num, 0);
        assert!(!out.tracked);
        assert_eq!(registry.sequence_count(), 0);
    }

    #[test]
    fn an_unregistered_type_is_refused_rather_than_stamped() {
        let mut registry = registry();
        assert_eq!(
            registry.on_serialize(0, MessageType::DFU_START, 0),
            Err(RegistryError::UnknownType)
        );
        assert_eq!(
            registry.on_received(MessageType::DFU_START, 0),
            Delivery::Unregistered
        );
    }

    /// `process_received_message` requires `sequenced_reply && !sequenced_request`
    /// before it will match, and `serialize` tests `sequenced_reply` first.
    #[test]
    fn a_type_with_both_flags_neither_creates_nor_consumes_a_request() {
        let mut registry = registry();
        let both = MessageType::CONFIG_VALUE;
        registry
            .add(
                both,
                PacketCfg {
                    sequenced_reply: true,
                    sequenced_request: true,
                },
            )
            .unwrap();

        let out = registry.on_serialize(0, both, 99).unwrap();
        assert_eq!(out.seq_num, 99, "the reply arm wins");
        assert!(!out.tracked);

        let tracked = registry.on_serialize(0, REQUEST, 0).unwrap().seq_num;
        assert_eq!(
            registry.on_received(both, tracked),
            Delivery::Process,
            "and it may not answer a request"
        );
        assert_eq!(registry.pending_len(), 1);
    }

    #[test]
    fn a_request_is_still_live_at_its_timeout_and_dead_one_millisecond_later() {
        let request = PendingRequest {
            message_type: REQUEST,
            seq_num: 0,
            timestamp_ms: 1000,
            timeout_ms: DEFAULT_MESSAGE_TIMEOUT_MS,
        };
        assert!(!request.expired_at(1000 + DEFAULT_MESSAGE_TIMEOUT_MS));
        assert!(request.expired_at(1000 + DEFAULT_MESSAGE_TIMEOUT_MS + 1));
        // The C's subtraction is unsigned, so a clock that goes backwards
        // expires everything rather than waiting.
        assert!(request.expired_at(999));
    }

    /// The sweep, not the timeout, is what expires a request: nothing happens
    /// between the 150 ms firings.
    #[test]
    fn expiry_lands_on_the_sweep_rather_than_on_the_timeout() {
        let mut registry = registry();
        let seq = registry.on_serialize(0, REQUEST, 0).unwrap().seq_num;

        for now in [24, 25, 100, 149] {
            assert!(
                expired(&mut registry, now).as_slice().is_empty(),
                "no sweep is due at {now} ms, so nothing can expire"
            );
        }
        assert_eq!(expired(&mut registry, 150).as_slice(), [seq]);
        assert_eq!(registry.pending_len(), 0);
        assert!(
            expired(&mut registry, 300).as_slice().is_empty(),
            "and only once"
        );
    }

    /// The effective timeout is 150 ms for a request sent at time zero, but it
    /// depends entirely on where the request falls in the sweep's phase.
    #[test]
    fn the_effective_timeout_ranges_from_25_to_174_milliseconds() {
        for sent_at in 0..300u32 {
            let mut registry = registry();
            registry.on_serialize(sent_at, REQUEST, 0).unwrap();

            let mut expired_at = None;
            for now in sent_at..sent_at + 400 {
                registry.on_tick(now, |_| expired_at = Some(now));
                if expired_at.is_some() {
                    break;
                }
            }
            let lifetime = expired_at.expect("everything expires eventually") - sent_at;
            assert!(
                (25..=174).contains(&lifetime),
                "sent at {sent_at} ms, expired after {lifetime} ms"
            );
            // The sweep that takes it is the first multiple of 150 that is
            // more than 24 ms away.
            let sweep = (sent_at + 25).div_ceil(150) * 150;
            assert_eq!(expired_at, Some(sweep));
        }
    }

    #[test]
    fn a_sweep_reports_every_expired_request_in_order_and_keeps_the_rest() {
        let mut registry = registry();
        let old = [
            registry.on_serialize(0, REQUEST, 0).unwrap().seq_num,
            registry.on_serialize(0, REQUEST, 0).unwrap().seq_num,
            registry.on_serialize(0, REQUEST, 0).unwrap().seq_num,
        ];
        // Sent late enough that the 150 ms sweep does not reach it.
        let young = registry.on_serialize(130, REQUEST, 0).unwrap().seq_num;

        assert_eq!(expired(&mut registry, 150).as_slice(), old);
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(registry.pending().next().unwrap().seq_num, young);
        assert_eq!(expired(&mut registry, 300).as_slice(), [young]);
    }

    #[test]
    fn a_missed_sweep_does_not_shift_the_phase() {
        let mut registry = registry();
        // No tick at all until well past several sweeps.
        let seq = registry.on_serialize(0, REQUEST, 0).unwrap().seq_num;
        assert_eq!(expired(&mut registry, 1000).as_slice(), [seq]);

        // The next sweep is still on the C's grid: 1050, not 1150.
        let seq = registry.on_serialize(1000, REQUEST, 0).unwrap().seq_num;
        assert!(expired(&mut registry, 1049).as_slice().is_empty());
        assert_eq!(expired(&mut registry, 1050).as_slice(), [seq]);
    }

    #[test]
    fn a_registry_can_be_phased_from_a_later_start() {
        let mut registry: Registry<8, 8> = Registry::started_at(1000);
        registry.add(REQUEST, PacketCfg::REQUEST).unwrap();
        let seq = registry.on_serialize(1000, REQUEST, 0).unwrap().seq_num;
        assert!(expired(&mut registry, 1149).as_slice().is_empty());
        assert_eq!(expired(&mut registry, 1150).as_slice(), [seq]);
    }

    #[test]
    fn a_full_pending_table_sends_the_request_untracked() {
        let mut registry: Registry<8, 2> = Registry::new();
        registry.add(REQUEST, PacketCfg::REQUEST).unwrap();
        registry.add(REPLY, PacketCfg::REPLY).unwrap();

        let first = registry.on_serialize(0, REQUEST, 0).unwrap();
        let second = registry.on_serialize(0, REQUEST, 0).unwrap();
        let third = registry.on_serialize(0, REQUEST, 0).unwrap();
        assert!(first.tracked && second.tracked);
        assert!(!third.tracked, "no room left");
        assert_eq!(
            third.seq_num, 2,
            "the counter moves anyway, as the C's does"
        );
        assert_eq!(
            registry.on_received(REPLY, third.seq_num),
            Delivery::Process,
            "an untracked request's reply is an ordinary message"
        );
    }

    /// `ll_item_add` appends without looking at the id, and `ll_get_item`
    /// returns the first match, so a duplicate registration is dead weight
    /// until the first one is removed.
    #[test]
    fn a_duplicate_registration_is_shadowed_by_the_first() {
        let mut registry: Registry<4, 4> = Registry::new();
        registry.add(REQUEST, PacketCfg::REQUEST).unwrap();
        registry.add(REQUEST, PacketCfg::UNSEQUENCED).unwrap();
        assert_eq!(registry.cfg(REQUEST), Some(PacketCfg::REQUEST));

        assert!(registry.remove(REQUEST));
        assert_eq!(registry.cfg(REQUEST), Some(PacketCfg::UNSEQUENCED));
        assert!(registry.remove(REQUEST));
        assert_eq!(registry.cfg(REQUEST), None);
        assert!(!registry.remove(REQUEST));
    }

    #[test]
    fn registration_order_survives_a_removal_from_the_middle() {
        let mut registry = registry();
        assert!(registry.remove(REQUEST));
        let mut order = registry.registrations().map(|(ty, _)| ty);
        assert_eq!(order.next(), Some(MessageType::HEARTBEAT));
        assert_eq!(order.next(), Some(REPLY));
        assert_eq!(order.next(), None);
    }

    #[test]
    fn a_full_registry_refuses_rather_than_overwriting() {
        let mut registry: Registry<1, 1> = Registry::new();
        registry.add(REQUEST, PacketCfg::REQUEST).unwrap();
        assert_eq!(
            registry.add(REPLY, PacketCfg::REPLY),
            Err(RegistryError::Full)
        );
        assert_eq!(registry.cfg(REQUEST), Some(PacketCfg::REQUEST));
    }

    #[test]
    fn the_sequence_counter_wraps_where_the_c_does() {
        let mut registry: Registry<4, 4> =
            Registry::resuming(u32::MAX, MESSAGE_TIMER_EXPIRY_PERIOD_MS);
        registry.add(REQUEST, PacketCfg::REQUEST).unwrap();
        registry.add(REPLY, PacketCfg::REPLY).unwrap();
        assert_eq!(
            registry.on_serialize(0, REQUEST, 0).unwrap().seq_num,
            u32::MAX
        );
        assert_eq!(registry.on_serialize(0, REQUEST, 0).unwrap().seq_num, 0);
        assert_eq!(registry.sequence_count(), 1);
    }
}
