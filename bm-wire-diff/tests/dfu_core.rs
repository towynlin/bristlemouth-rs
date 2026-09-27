//! The DFU core state machine, compared against `bcmp/dfu_core.c` in bm_core's
//! live stack.
//!
//! Its own binary because `bm_wire_diff::dfu_core` brings the stack up, whose
//! `bcmp_init` runs `bm_dfu_init`; see `bm_wire_diff::stack`. Each test is a script for
//! [`check`], which compares the two machines after every step, plus
//! assertions on what the C did so that an upstream fix fails here and says
//! which divergence it retired.

use bm_wire::bcmp::dfu_core::{DfuErr, EVENT_QUEUE_LEN, EventType, State};
use bm_wire_diff::dfu_core::{
    CoreState, DfuCoreInput, FrameType, Image, NodeRef, Sender, Step, check, reset,
};
use bm_wire_diff::replay::{STACK_TARGETS, replay_target};

const IMAGE: Image = Image {
    image_size: 2048,
    chunk_size: 512,
    crc16: 0x2fdf,
    major_ver: 1,
    minor_ver: 7,
    filter_key: 0,
    git_sha: 0xdead_d00d,
};

fn initiate(dst: NodeRef, notify: bool, internal: bool) -> Step {
    Step::Initiate {
        image: IMAGE,
        dst,
        notify,
        timeout_ms: 30_000,
        internal,
    }
}

/// `D0 + n` from `src` to this node, with a two-byte tail.
fn message(n: u8, src: NodeRef, tail: &[u8]) -> Step {
    Step::Message {
        frame_type: FrameType::Dfu(n),
        src,
        dst: NodeRef::This,
        tail: tail.to_vec(),
    }
}

fn abort_from(src: NodeRef, err: u8) -> Step {
    message(5, src, &[0, err])
}

#[test]
fn every_committed_seed_still_agrees_with_the_c() {
    let replayed = replay_target("dfu_core");
    assert!(
        replayed > 0,
        "no dfu_core seeds replayed; STACK_TARGETS is {STACK_TARGETS:?}"
    );
    eprintln!("replayed {replayed} dfu_core seeds");
}

/// `process_message_test`: all ten types addressed to this node are queued in
/// Idle, whoever sent them; a full queue refuses the rest.
#[test]
fn every_dfu_type_is_queued_until_the_queue_is_full() {
    let mut steps: Vec<Step> = (0..10)
        .map(|n| message(n, NodeRef::Raw(0xdead_dead_dead_dead), &[0, 0]))
        .collect();
    steps.push(Step::RunAll);
    check(&DfuCoreInput { steps });
}

#[test]
fn messages_for_another_node_or_of_no_dfu_type_are_not_queued() {
    check(&DfuCoreInput {
        steps: vec![
            Step::Message {
                frame_type: FrameType::Dfu(6),
                src: NodeRef::Peer,
                dst: NodeRef::Peer,
                tail: vec![],
            },
            Step::Message {
                frame_type: FrameType::Raw(0xDA),
                src: NodeRef::Peer,
                dst: NodeRef::This,
                tail: vec![],
            },
        ],
    });
}

/// `dfu_api_test`: the four senders' frames, byte for byte.
#[test]
fn the_senders_frames_are_the_cs() {
    check(&DfuCoreInput {
        steps: vec![
            Step::Send(Sender::Ack {
                dst: NodeRef::Peer,
                success: 1,
                err: 0,
            }),
            Step::Send(Sender::ChunkRequest {
                dst: NodeRef::Peer,
                chunk: 0x1234,
            }),
            Step::Send(Sender::End {
                dst: NodeRef::Peer,
                success: 0,
                err: DfuErr::BAD_CRC.0,
            }),
            Step::Send(Sender::Heartbeat { dst: NodeRef::Peer }),
        ],
    });
}

/// `host_golden`'s first step: a `BeginHost` run in Idle enters
/// `HostReqUpdate`, and its entry sends the start.
#[test]
fn begin_host_enters_host_req_update_and_sends_the_start() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, false, true));
    assert_eq!(pair.c.state(), State::Idle, "queued, not run");
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::HostReqUpdate);
    assert_eq!(pair.rust.core().client_node_id(), NodeRef::Peer.id());
}

/// Divergence #58: a finish callback given to one host update is called from
/// every later entry into Error, with that update's client.
#[test]
fn every_later_error_is_reported_to_the_last_hosts_callback() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, true, true));
    pair.apply(&Step::Run);
    pair.apply(&Step::SetError(DfuErr::ABORTED.0));
    pair.apply(&Step::SetPending(CoreState::Error));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);

    // Long after, an unrelated failure: the same callback, the same client.
    pair.apply(&Step::SetError(DfuErr::TIMEOUT.0));
    pair.apply(&Step::SetPending(CoreState::Error));
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::Error);
    // `compare` has already matched the C's callbacks against these.
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);
}

/// Divergence #59: an error of 14 or more leaves the machine in Error, and no
/// update can start until reboot. The host takes the value from a peer's
/// byte; here it is set directly, as `bm_dfu_host_transition_to_error` would.
#[test]
fn a_fatal_error_is_permanent() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, true, true));
    pair.apply(&Step::Run);
    pair.apply(&Step::SetError(200));
    pair.apply(&Step::SetPending(CoreState::Error));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Error);
    pair.apply(&Step::Post(EventType::InitSuccess as u8));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Error);
    pair.apply(&initiate(NodeRef::Peer, true, true));
    assert_eq!(pair.c.state(), State::Error);
}

/// Divergence #60: a second `initiate_update` before the first runs is
/// accepted, overwrites `internal`, and is ignored by `HostReqUpdate`.
#[test]
fn a_second_initiate_is_accepted_and_lost() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, true, false));
    pair.apply(&initiate(NodeRef::PeerAlias, true, true));
    assert!(pair.rust.core().internal(), "the second call's");
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::HostReqUpdate);
    assert_eq!(pair.rust.core().client_node_id(), NodeRef::Peer.id());
}

/// `HostReqUpdate` accepts messages only from its client.
#[test]
fn host_req_update_drops_messages_from_anyone_but_its_client() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, false, true));
    pair.apply(&Step::Run);
    pair.apply(&message(6, NodeRef::PeerAlias, &[]));
    pair.apply(&message(6, NodeRef::Peer, &[]));
    pair.apply(&abort_from(NodeRef::Peer, 8));
    assert_eq!(pair.rust.core().queue().len(), 3, "NOP, heartbeat, abort");
}

/// A pending change whose NOP found the queue full is taken after the next
/// event, whatever it is.
#[test]
fn a_dropped_nop_defers_the_change() {
    let mut pair = reset();
    for _ in 0..EVENT_QUEUE_LEN {
        pair.apply(&message(6, NodeRef::Peer, &[]));
    }
    pair.apply(&Step::SetPending(CoreState::Init));
    assert_eq!(pair.rust.core().queue().len(), EVENT_QUEUE_LEN);
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::Init);
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Init, "Init waits for InitSuccess");
    pair.apply(&Step::Post(EventType::InitSuccess as u8));
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::Idle);
}

/// Divergence #59 in the C host itself: a client's NACK carrying
/// `BmDfuErrFlashAccess` — what a client whose flash fails sends — leaves
/// `dfu_host.c` in Error for good. C only: the host is D4's. The Rust side is
/// left behind, and the next [`reset`] brings both back.
#[test]
fn the_c_host_adopts_a_clients_fatal_nack() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, false, true));
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::HostReqUpdate);

    let mut nack = vec![0xD4];
    nack.extend_from_slice(&NodeRef::Peer.id().to_le_bytes());
    nack.extend_from_slice(&NodeRef::This.id().to_le_bytes());
    nack.extend_from_slice(&[0, DfuErr::FLASH_ACCESS.0]);
    unsafe {
        let buf = bm_wire_sys::bm_malloc(nack.len()).cast::<u8>();
        std::ptr::copy_nonoverlapping(nack.as_ptr(), buf, nack.len());
        bm_wire_sys::bm_dfu_process_message(buf, nack.len());
    }
    for _ in 0..4 * EVENT_QUEUE_LEN {
        pair.c.run(true);
    }
    assert_eq!(pair.c.state(), State::Error);
    assert_eq!(
        unsafe { bm_wire_sys::bm_dfu_get_error() },
        u32::from(DfuErr::FLASH_ACCESS.0)
    );
    let refused = unsafe {
        bm_wire_sys::bm_dfu_initiate_update(Default::default(), NodeRef::Peer.id(), None, 0, true)
    };
    assert!(!refused, "no update can start");
    drop(pair);
    let _ = reset();
}
