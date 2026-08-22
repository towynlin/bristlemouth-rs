//! Differential comparator for [`bm_wire::neighbor`] against `bcmp/neighbors.c`.
//!
//! The first comparator here for a *state machine* rather than a codec. A
//! sequence of heartbeats and clock advances is applied to both bm_core's
//! neighbour table and the port's, and the two tables are compared entry by
//! entry, in order.
//!
//! `bcmp_process_heartbeat` is static, so heartbeats go in the only way a real
//! one would: as a frame, injected at the wire boundary and pumped through L2
//! and BCMP. The table is then read back through `bcmp_get_neighbors`, which is
//! public.
//!
//! # Reading the table runs the liveliness check
//!
//! `bcmp_get_neighbors` calls `bcmp_check_neighbors()` before returning the
//! head pointer, so there is no way to observe the table without also aging it.
//! The comparator therefore calls [`bm_wire::neighbor::NeighborTable::check`]
//! at the same instant before comparing. That is a faithful comparison — it is
//! the only observation the C offers — but it does mean the check is exercised
//! on every step rather than only where a step asks for it.
//!
//! # This target needs fork mode
//!
//! Unlike the other stack-backed comparators, this one accumulates. Every new
//! neighbour makes `bcmp_update_neighbor` call `bcmp_request_info`, which does
//! an unconditional `ll_item_add` onto `INFO_REQUEST_LIST` — no de-duplication,
//! no expiry, and the only removal is on an info *reply*, which never comes
//! here. The list therefore grows by one entry per new neighbour for the life
//! of the process. See divergence #19. Bounded per iteration, unbounded across
//! a fuzz run, so `cargo fuzz run neighbor -- -fork=1`.

use std::sync::{Mutex, OnceLock};

use arbitrary::{Arbitrary, Result, Unstructured};

use bm_wire::bcmp::{BCMP_HEADER_LEN, Heartbeat, MessageType, tx};
use bm_wire::frame::{
    ETHERNET_TYPE_IPV6, ETHERNET_TYPE_OFFSET, IP_PROTO_BCMP, IPV6_DESTINATION_ADDRESS_OFFSET,
    IPV6_NEXT_HEADER_OFFSET, IPV6_PAYLOAD_LENGTH_OFFSET, IPV6_SOURCE_ADDRESS_OFFSET,
    MIN_FRAME_WITH_ADDRESSES,
};
use bm_wire::neighbor::{Neighbor, NeighborTable};
use bm_wire::util::BmIpAddr;

use crate::Domain;
use crate::stack::{NUM_PORTS, drain, inject, oracle};

/// Capacity of the port's table. With one neighbour per port and two ports,
/// nothing here can fill it; the comparator asserts that.
pub const CAPACITY: usize = 8;

/// The node ids a step may claim to come from.
///
/// A small fixed set, for two reasons: it keeps the comparison focused on the
/// table's mechanics rather than on id diversity, and it bounds how fast
/// `INFO_REQUEST_LIST` grows — see the module docs.
///
/// Zero is deliberately included. `bcmp_find_neighbor` refuses to match it, so
/// a node sending from `fe80::` is treated as new on every heartbeat.
pub const NODE_IDS: &[u64] = &[
    0,
    0x0000_0000_55AA_0011,
    0x0000_0000_55AA_0022,
    0xDEAD_BEEF_1234_5678,
];

/// Longest a single step may push the virtual clock.
///
/// The clock fires every due timer as it advances, and BCMP's heartbeat timer
/// reloads every ten seconds, so an unbounded advance is an unbounded loop.
pub const MAX_ADVANCE_MS: u32 = 60_000;

/// One thing that happens to the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A heartbeat arrives.
    Heartbeat {
        /// Index into [`NODE_IDS`].
        node: u8,
        /// Ingress port, 1..=[`NUM_PORTS`].
        port: u8,
        /// The `time_since_boot_us` the sender claims.
        uptime_us: u64,
        /// The `liveliness_lease_dur_s` the sender advertises.
        lease_s: u32,
    },
    /// Time passes.
    Advance {
        /// Milliseconds, capped at [`MAX_ADVANCE_MS`].
        ms: u32,
    },
}

impl Step {
    fn clamp(&mut self) {
        match self {
            Self::Heartbeat { node, port, .. } => {
                *node %= NODE_IDS.len() as u8;
                *port = port.wrapping_sub(1) % NUM_PORTS + 1;
            }
            Self::Advance { ms } => *ms %= MAX_ADVANCE_MS + 1,
        }
    }
}

/// A sequence of steps applied to both tables.
#[derive(Debug, Clone)]
pub struct NeighborInput {
    /// The steps, capped at 32 by [`Domain`].
    pub steps: Vec<Step>,
}

impl<'a> Arbitrary<'a> for NeighborInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        let mut steps = Vec::new();
        while !u.is_empty() && steps.len() < MAX_STEPS {
            steps.push(arbitrary_step(u)?);
        }
        let mut input = Self { steps };
        input.clamp_to_domain();
        Ok(input)
    }

    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        Self::arbitrary(&mut u)
    }
}

/// Most steps a single input may carry.
pub const MAX_STEPS: usize = 32;

fn arbitrary_step(u: &mut Unstructured<'_>) -> Result<Step> {
    let mut step = if u.arbitrary::<bool>()? {
        Step::Heartbeat {
            node: u.arbitrary()?,
            port: u.arbitrary()?,
            uptime_us: u.arbitrary()?,
            lease_s: u.arbitrary()?,
        }
    } else {
        Step::Advance { ms: u.arbitrary()? }
    };
    step.clamp();
    Ok(step)
}

impl Domain for NeighborInput {
    fn clamp_to_domain(&mut self) {
        self.steps.truncate(MAX_STEPS);
        for step in &mut self.steps {
            step.clamp();
        }
    }
}

/// A heartbeat frame from `node_id`, ready to inject.
fn heartbeat_frame(node_id: u64, heartbeat: &Heartbeat) -> Vec<u8> {
    let payload_len = BCMP_HEADER_LEN + Heartbeat::LEN;
    let mut frame = vec![0u8; MIN_FRAME_WITH_ADDRESSES + payload_len];
    frame[ETHERNET_TYPE_OFFSET..ETHERNET_TYPE_OFFSET + 2]
        .copy_from_slice(&ETHERNET_TYPE_IPV6.to_be_bytes());
    frame[IPV6_PAYLOAD_LENGTH_OFFSET..IPV6_PAYLOAD_LENGTH_OFFSET + 2]
        .copy_from_slice(&(payload_len as u16).to_be_bytes());
    frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
    frame[IPV6_SOURCE_ADDRESS_OFFSET..IPV6_SOURCE_ADDRESS_OFFSET + 16]
        .copy_from_slice(&bm_wire::addr::nodeid_to_ip(0xFE80_0000, node_id).0);
    frame[IPV6_DESTINATION_ADDRESS_OFFSET..IPV6_DESTINATION_ADDRESS_OFFSET + 16]
        .copy_from_slice(&BmIpAddr::LINK_LOCAL_MULTICAST.0);

    let mut body = [0u8; Heartbeat::LEN];
    heartbeat.encode(&mut body).expect("12 bytes");
    tx::serialize(&mut frame, MessageType::HEARTBEAT, 0, &body).expect("frame is sized");
    frame
}

/// What bm_core's discovery callback saw, since it was last cleared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Discoveries {
    /// Calls with `discovered = true`: a neighbour appeared, reappeared, or
    /// restarted.
    pub appeared: u32,
    /// Calls with `discovered = false`: a neighbour's lease ran out.
    pub lost: u32,
}

static DISCOVERIES: Mutex<Discoveries> = Mutex::new(Discoveries {
    appeared: 0,
    lost: 0,
});

unsafe extern "C" fn on_discovery(discovered: bool, _neighbor: *mut bm_wire_sys::BcmpNeighbor) {
    let mut seen = DISCOVERIES.lock().unwrap_or_else(|p| p.into_inner());
    if discovered {
        seen.appeared += 1;
    } else {
        seen.lost += 1;
    }
}

/// Register the discovery callback once, so the comparator can compare the
/// notifications as well as the table.
fn register_discovery_callback() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| unsafe {
        bm_wire_sys::bcmp_neighbor_register_discovery_callback(Some(on_discovery));
    });
}

fn take_discoveries() -> Discoveries {
    let mut seen = DISCOVERIES.lock().unwrap_or_else(|p| p.into_inner());
    std::mem::take(&mut *seen)
}

/// The oracle's table, in list order.
///
/// # Safety
///
/// Walks bm_core's `BcmpNeighbor` list, which is only valid while the oracle
/// lock is held and nothing is pumping.
fn oracle_table() -> Vec<Neighbor> {
    let mut out = Vec::new();
    unsafe {
        let mut count = 0u8;
        // Note: this runs bcmp_check_neighbors() as a side effect.
        let mut node = bm_wire_sys::bcmp_get_neighbors(&mut count);
        while !node.is_null() {
            let entry = &*node;
            out.push(Neighbor {
                node_id: entry.node_id,
                port: entry.port,
                last_heartbeat_ms: entry.last_heartbeat_ticks,
                last_time_since_boot_us: entry.last_time_since_boot_us,
                heartbeat_period_s: entry.heartbeat_period_s,
                online: entry.online,
            });
            node = entry.next;
        }
        assert_eq!(
            usize::from(count),
            out.len(),
            "bcmp_get_neighbors' count disagrees with its own list"
        );
    }
    out
}

/// Empty bm_core's neighbour table, so each run starts from a known state.
///
/// `bcmp_remove_neighbor_from_table` unlinks **and frees**, despite
/// `bcmp_free_neighbor` being documented as the half that frees. Calling both,
/// as its header reads, is a double free — see divergence #15.
fn clear_oracle_table() {
    unsafe {
        // Walk the list first and collect the nodes, so a malformed list shows
        // up as a diagnosis rather than as a double free.
        let mut nodes: Vec<*mut bm_wire_sys::BcmpNeighbor> = Vec::new();
        let mut count = 0u8;
        let mut node = bm_wire_sys::bcmp_get_neighbors(&mut count);
        while !node.is_null() {
            assert!(
                !nodes.contains(&node),
                "bm_core's neighbour list contains {node:?} twice"
            );
            assert!(
                nodes.len() < 64,
                "bm_core's neighbour list is longer than anything here should produce"
            );
            nodes.push(node);
            node = (*node).next;
        }

        for node in nodes {
            // No bcmp_free_neighbor here: this already did it.
            assert!(
                bm_wire_sys::bcmp_remove_neighbor_from_table(node),
                "removing {node:?} from the list failed"
            );
        }
    }
}

fn tick_count() -> u32 {
    unsafe { bm_wire_sys::bm_shim_tick_count() }
}

/// Apply the same steps to bm_core's table and the port's, and assert they
/// agree after every one.
///
/// # Panics
///
/// If the tables ever differ, or if the port's table fills — which would mean
/// the comparison had stopped being meaningful.
pub fn check(input: &NeighborInput) {
    let mut input = input.clone();
    input.clamp_to_domain();

    let _guard = oracle();
    register_discovery_callback();
    // Both tables start empty. bm_core has no reset, but it does expose the
    // two halves of a removal, so the table can be emptied one entry at a time.
    clear_oracle_table();
    let mut table = NeighborTable::<CAPACITY>::new();
    drain();
    take_discoveries();

    let mut expected = Discoveries::default();
    for (index, step) in input.steps.iter().enumerate() {
        match *step {
            Step::Heartbeat {
                node,
                port,
                uptime_us,
                lease_s,
            } => {
                let node_id = NODE_IDS[usize::from(node) % NODE_IDS.len()];
                let heartbeat = Heartbeat {
                    time_since_boot_us: uptime_us,
                    liveliness_lease_dur_s: lease_s,
                };
                let now = tick_count();
                inject(port, &heartbeat_frame(node_id, &heartbeat));
                assert_eq!(
                    tick_count(),
                    now,
                    "injecting must not move the virtual clock"
                );
                let outcome = table.on_heartbeat(now, node_id, port, &heartbeat);
                assert!(
                    !outcome.table_full,
                    "step {index}: the port's table filled, so nothing below is meaningful"
                );
                expected.appeared += u32::from(outcome.discovery_callbacks);
            }
            Step::Advance { ms } => {
                unsafe { bm_wire_sys::bm_shim_advance_ticks(ms) };
                crate::stack::pump_until_quiet();
            }
        }
        drain();

        // Reading the C's table ages it; age the port's to match, at the same
        // instant, before comparing.
        let now = tick_count();
        let c = oracle_table();
        table.check(now, |_| expected.lost += 1);
        compare(&c, &table, index, step);

        // The notifications too, not just the resulting table: the doubled
        // announcement for a new neighbour (divergence #17) is invisible in the
        // table but is exactly what an application sees.
        let seen = take_discoveries();
        assert_eq!(
            seen, expected,
            "step {index} ({step:?}): discovery callbacks diverged"
        );
        expected = Discoveries::default();
    }
}

fn compare<const N: usize>(c: &[Neighbor], rs: &NeighborTable<N>, index: usize, step: &Step) {
    let rs: Vec<Neighbor> = rs.neighbors().copied().collect();
    if c == rs.as_slice() {
        return;
    }
    panic!("step {index} ({step:?}): neighbour tables diverged\n  C:    {c:#?}\n  Rust: {rs:#?}");
}
