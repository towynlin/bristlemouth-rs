//! The neighbour table, ported from `bcmp/neighbors.c` and
//! `bcmp_process_heartbeat` in `bcmp/heartbeat.c`.
//!
//! This is the first module here that is not a codec. It is written *sans-io*:
//! it owns no clock, no timers and no transmission, and every entry point
//! takes the current time as a parameter and returns what the caller should do
//! about it. That keeps it testable without an executor and keeps `bm-wire`
//! free of dependencies; the timer that drives [`NeighborTable::check`] and the
//! transmission that answers [`HeartbeatOutcome::request_info`] belong to the
//! runtime above.
//!
//! # Time
//!
//! Everything here is in milliseconds. bm_core counts in RTOS ticks and
//! converts with `bm_ms_to_ticks`, which is the identity on every backend in
//! the tree; a runtime whose tick is not a millisecond has to convert before
//! calling in.
//!
//! # Capacity
//!
//! bm_core keeps a `bm_malloc`'d linked list. This is a fixed-capacity array,
//! because `bm-wire` has no allocator. That is not a limitation in practice:
//! [`NeighborTable::on_heartbeat`] evicts whatever was on the ingress port
//! before adding, exactly as the C does, so the table never holds more than one
//! entry per port.

use crate::bcmp::Heartbeat;
use crate::util::time_remaining;

/// How often bm_core emits a heartbeat, and the lease duration it advertises.
/// `bcmp_heartbeat_s` in `bcmp/bcmp.c`.
pub const HEARTBEAT_PERIOD_S: u32 = 10;

/// One entry in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Neighbor {
    /// The neighbour's node id.
    pub node_id: u64,
    /// The port it was last heard on.
    pub port: u8,
    /// When its last heartbeat arrived.
    pub last_heartbeat_ms: u32,
    /// The `time_since_boot_us` of that heartbeat. A lower value in a later
    /// heartbeat means the neighbour restarted.
    pub last_time_since_boot_us: u64,
    /// The lease duration it advertised, in seconds.
    pub heartbeat_period_s: u32,
    /// Whether its lease is still good.
    pub online: bool,
}

impl Neighbor {
    /// How long the neighbour may be silent before it is considered offline.
    ///
    /// Two advertised periods, as `neighbor_check` computes it — including the
    /// overflow: the C evaluates `2 * heartbeat_period_s * 1000` in 32-bit
    /// unsigned arithmetic, so a neighbour advertising a large lease wraps to a
    /// short one, or to none at all. See divergence #16.
    #[must_use]
    pub const fn lease_ms(&self) -> u32 {
        self.heartbeat_period_s.wrapping_mul(2).wrapping_mul(1000)
    }
}

/// What a heartbeat did to the table, and what the caller now owes the network.
///
/// Flat rather than a list of events because the C's branches are flat, and
/// because a fixed struct needs no allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeartbeatOutcome {
    /// A new entry was created for this neighbour.
    pub added: bool,
    /// The node id evicted from the ingress port to make room.
    ///
    /// bm_core allows one neighbour per port and drops the previous occupant
    /// without ceremony, freeing it as it goes.
    pub evicted: Option<u64>,
    /// How many times bm_core would invoke the discovery callback with
    /// `discovered = true`.
    ///
    /// **Two for a brand-new neighbour**, which is a quirk rather than a
    /// design: `bcmp_update_neighbor` fires it once on insert, and
    /// `bcmp_process_heartbeat` then sees the fresh entry's `online` still
    /// false and fires it again. See divergence #17.
    pub discovery_callbacks: u8,
    /// The neighbour's uptime went backwards, so it restarted.
    pub reset: bool,
    /// The caller should send it a device-info request.
    pub request_info: bool,
    /// The table was full, so nothing was recorded.
    ///
    /// bm_core cannot report this: it allocates, so it only fails on a genuine
    /// out-of-memory. With eviction by port, a table of at least one entry per
    /// port never reaches this.
    pub table_full: bool,
}

/// A fixed-capacity neighbour table.
///
/// `N` should be at least the number of ports the device has.
#[derive(Debug, Clone)]
pub struct NeighborTable<const N: usize> {
    entries: [Neighbor; N],
    len: usize,
}

impl<const N: usize> Default for NeighborTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> NeighborTable<N> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [Neighbor {
                node_id: 0,
                port: 0,
                last_heartbeat_ms: 0,
                last_time_since_boot_us: 0,
                heartbeat_period_s: 0,
                online: false,
            }; N],
            len: 0,
        }
    }

    /// How many neighbours are recorded, online or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The neighbours, in the order bm_core's linked list holds them: insertion
    /// order, with removals closing the gap.
    pub fn neighbors(&self) -> impl Iterator<Item = &Neighbor> + '_ {
        self.entries[..self.len].iter()
    }

    /// Look a neighbour up by node id.
    ///
    /// **A `node_id` of zero never matches.** `bcmp_find_neighbor` guards its
    /// comparison with `if (node_id && ...)`, so a node whose address carries a
    /// zero id — `fe80::`, say — is treated as unknown on every heartbeat. See
    /// divergence #18.
    #[must_use]
    pub fn find(&self, node_id: u64) -> Option<&Neighbor> {
        if node_id == 0 {
            return None;
        }
        self.entries[..self.len]
            .iter()
            .find(|n| n.node_id == node_id)
    }

    fn position(&self, node_id: u64) -> Option<usize> {
        if node_id == 0 {
            return None;
        }
        self.entries[..self.len]
            .iter()
            .position(|n| n.node_id == node_id)
    }

    fn remove(&mut self, index: usize) {
        self.entries.copy_within(index + 1..self.len, index);
        self.len -= 1;
    }

    /// Record a heartbeat and report what it changed.
    ///
    /// `node_id` is the sender's, decoded from the frame's source address, and
    /// `ingress_port` is the port it arrived on.
    pub fn on_heartbeat(
        &mut self,
        now_ms: u32,
        node_id: u64,
        ingress_port: u8,
        heartbeat: &Heartbeat,
    ) -> HeartbeatOutcome {
        let mut outcome = HeartbeatOutcome::default();

        let index = match self.position(node_id) {
            Some(index) => index,
            None => {
                // New neighbour. The port takes one occupant, so whoever was
                // there is dropped first.
                if let Some(occupant) = self.entries[..self.len]
                    .iter()
                    .position(|n| n.port == ingress_port)
                {
                    outcome.evicted = Some(self.entries[occupant].node_id);
                    self.remove(occupant);
                }
                if self.len == N {
                    outcome.table_full = true;
                    return outcome;
                }
                self.entries[self.len] = Neighbor {
                    node_id,
                    port: ingress_port,
                    ..Neighbor::default()
                };
                self.len += 1;
                outcome.added = true;
                // bcmp_update_neighbor fires discovery and asks for info the
                // moment the entry exists.
                outcome.discovery_callbacks += 1;
                outcome.request_info = true;
                self.len - 1
            }
        };

        let neighbor = &mut self.entries[index];
        outcome.reset = heartbeat.time_since_boot_us < neighbor.last_time_since_boot_us;

        // The second discovery callback for a new neighbour: its `online` is
        // still false here, because bcmp_add_neighbor zeroes the entry.
        if !neighbor.online || outcome.reset {
            outcome.discovery_callbacks += 1;
        }
        if outcome.reset {
            outcome.request_info = true;
        }

        neighbor.last_time_since_boot_us = heartbeat.time_since_boot_us;
        neighbor.heartbeat_period_s = heartbeat.liveliness_lease_dur_s;
        neighbor.last_heartbeat_ms = now_ms;
        neighbor.online = true;

        outcome
    }

    /// Take every online neighbour whose lease has run out offline.
    ///
    /// `went_offline` is called for each, which is where bm_core invokes the
    /// discovery callback with `discovered = false`. Entries are **not**
    /// removed: an offline neighbour stays in the table until something takes
    /// its port, which is what the C does too.
    pub fn check(&mut self, now_ms: u32, mut went_offline: impl FnMut(&Neighbor)) {
        for neighbor in &mut self.entries[..self.len] {
            if neighbor.online
                && time_remaining(neighbor.last_heartbeat_ms, now_ms, neighbor.lease_ms()) == 0
            {
                neighbor.online = false;
                went_offline(neighbor);
            }
        }
    }
}

/// The heartbeat this node should send.
///
/// `bcmp_send_heartbeat` builds exactly this, from a millisecond uptime scaled
/// to microseconds — so the bottom three digits are always zero on the wire,
/// whatever resolution the caller has.
#[must_use]
pub fn heartbeat_for(uptime_ms: u32, lease_duration_s: u32) -> Heartbeat {
    Heartbeat {
        time_since_boot_us: u64::from(uptime_ms) * 1000,
        liveliness_lease_dur_s: lease_duration_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hb(time_since_boot_us: u64) -> Heartbeat {
        Heartbeat {
            time_since_boot_us,
            liveliness_lease_dur_s: HEARTBEAT_PERIOD_S,
        }
    }

    #[test]
    fn a_new_neighbour_is_added_and_announced_twice() {
        let mut table = NeighborTable::<4>::new();
        let outcome = table.on_heartbeat(1000, 0xAA, 1, &hb(500_000));

        assert!(outcome.added);
        assert_eq!(outcome.evicted, None);
        assert!(outcome.request_info);
        assert!(!outcome.reset);
        assert_eq!(
            outcome.discovery_callbacks, 2,
            "bcmp_update_neighbor fires once, then bcmp_process_heartbeat again"
        );

        let neighbor = table.find(0xAA).unwrap();
        assert!(neighbor.online);
        assert_eq!(neighbor.port, 1);
        assert_eq!(neighbor.last_heartbeat_ms, 1000);
        assert_eq!(neighbor.last_time_since_boot_us, 500_000);
    }

    #[test]
    fn a_steady_neighbour_is_announced_once_and_then_not_at_all() {
        let mut table = NeighborTable::<4>::new();
        table.on_heartbeat(0, 0xAA, 1, &hb(0));
        for tick in 1..5u32 {
            let outcome =
                table.on_heartbeat(tick * 10_000, 0xAA, 1, &hb(u64::from(tick) * 10_000_000));
            assert!(!outcome.added);
            assert!(!outcome.reset);
            assert!(!outcome.request_info);
            assert_eq!(outcome.discovery_callbacks, 0);
        }
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn a_neighbour_that_restarts_is_announced_and_asked_for_info() {
        let mut table = NeighborTable::<4>::new();
        table.on_heartbeat(0, 0xAA, 1, &hb(9_000_000));
        let outcome = table.on_heartbeat(10_000, 0xAA, 1, &hb(1_000));

        assert!(outcome.reset, "uptime went backwards");
        assert!(outcome.request_info);
        assert_eq!(outcome.discovery_callbacks, 1);
        assert_eq!(table.len(), 1, "a restart is not a new neighbour");
    }

    #[test]
    fn a_port_holds_one_neighbour_and_the_previous_one_is_dropped() {
        let mut table = NeighborTable::<4>::new();
        table.on_heartbeat(0, 0xAA, 1, &hb(0));
        table.on_heartbeat(0, 0xBB, 2, &hb(0));
        assert_eq!(table.len(), 2);

        let outcome = table.on_heartbeat(1000, 0xCC, 1, &hb(0));
        assert_eq!(outcome.evicted, Some(0xAA));
        assert!(outcome.added);
        assert_eq!(table.len(), 2);
        assert!(table.find(0xAA).is_none());
        assert!(table.find(0xBB).is_some());
        assert!(table.find(0xCC).is_some());

        // Insertion order, with the gap closed: BB was second and is now first.
        let order: [u64; 2] = [
            table.neighbors().next().unwrap().node_id,
            table.neighbors().nth(1).unwrap().node_id,
        ];
        assert_eq!(order, [0xBB, 0xCC]);
    }

    #[test]
    fn a_zero_node_id_is_never_recognised() {
        let mut table = NeighborTable::<4>::new();
        let first = table.on_heartbeat(0, 0, 1, &hb(0));
        assert!(first.added);

        // Same node, same port, and bm_core still calls it new -- so it evicts
        // itself and asks for its info all over again.
        let second = table.on_heartbeat(1000, 0, 1, &hb(1_000_000));
        assert!(second.added);
        assert_eq!(second.evicted, Some(0));
        assert!(second.request_info);
        assert_eq!(second.discovery_callbacks, 2);
        assert_eq!(
            table.len(),
            1,
            "it replaced itself rather than accumulating"
        );
        assert!(table.find(0).is_none(), "and it can never be looked up");
    }

    #[test]
    fn a_neighbour_goes_offline_after_two_lease_periods() {
        let mut table = NeighborTable::<4>::new();
        table.on_heartbeat(0, 0xAA, 1, &hb(0));
        let lease = table.find(0xAA).unwrap().lease_ms();
        assert_eq!(lease, 2 * HEARTBEAT_PERIOD_S * 1000);

        let mut lost = 0;
        table.check(lease - 1, |_| lost += 1);
        assert_eq!(lost, 0, "still inside the lease");
        assert!(table.find(0xAA).unwrap().online);

        table.check(lease, |n| {
            lost += 1;
            assert_eq!(n.node_id, 0xAA);
        });
        assert_eq!(lost, 1);
        assert!(!table.find(0xAA).unwrap().online);
        assert_eq!(table.len(), 1, "an offline neighbour stays in the table");

        // And it is only reported once.
        table.check(lease * 4, |_| lost += 1);
        assert_eq!(lost, 1);
    }

    #[test]
    fn a_neighbour_that_comes_back_is_announced_again() {
        let mut table = NeighborTable::<4>::new();
        table.on_heartbeat(0, 0xAA, 1, &hb(0));
        let lease = table.find(0xAA).unwrap().lease_ms();
        table.check(lease, |_| {});

        let outcome = table.on_heartbeat(lease + 1000, 0xAA, 1, &hb(1_000_000));
        assert!(!outcome.added, "it was never removed");
        assert_eq!(outcome.discovery_callbacks, 1, "it was offline");
        assert!(!outcome.request_info, "coming back is not a restart");
        assert!(table.find(0xAA).unwrap().online);
    }

    /// The C computes the lease as `2 * period_s * 1000` in 32-bit unsigned
    /// arithmetic, which wraps well before the field does.
    #[test]
    fn an_advertised_lease_can_wrap_to_nothing() {
        let mut table = NeighborTable::<4>::new();
        let forever = Heartbeat {
            time_since_boot_us: 0,
            // 2 * 2147483648 * 1000 mod 2^32 == 0
            liveliness_lease_dur_s: 2_147_483_648,
        };
        table.on_heartbeat(1000, 0xAA, 1, &forever);
        assert_eq!(
            table.find(0xAA).unwrap().lease_ms(),
            0,
            "a lease of 68 years wraps to none at all"
        );

        let mut lost = 0;
        table.check(1000, |_| lost += 1);
        assert_eq!(lost, 1, "and the neighbour is immediately offline");
    }

    #[test]
    fn a_full_table_reports_rather_than_overwriting() {
        let mut table = NeighborTable::<2>::new();
        table.on_heartbeat(0, 0xAA, 1, &hb(0));
        table.on_heartbeat(0, 0xBB, 2, &hb(0));
        let outcome = table.on_heartbeat(0, 0xCC, 3, &hb(0));
        assert!(outcome.table_full);
        assert!(!outcome.added);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn the_heartbeat_we_send_matches_the_c() {
        // bcmp_send_heartbeat: bm_ticks_to_ms(bm_get_tick_count()) * 1000.
        let heartbeat = heartbeat_for(1_234_567, HEARTBEAT_PERIOD_S);
        assert_eq!(heartbeat.time_since_boot_us, 1_234_567_000);
        assert_eq!(heartbeat.liveliness_lease_dur_s, 10);
    }
}
