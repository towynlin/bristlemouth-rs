//! The DFU core state machine and client, compared against `bcmp/dfu_core.c`
//! and `bcmp/dfu_client.c` in bm_core's live stack.
//!
//! Its own binary because `bm_wire_diff::dfu_core` brings the stack up, whose
//! `bcmp_init` runs `bm_dfu_init`; see `bm_wire_diff::stack`. Each test is a script for
//! [`check`], which compares the two machines after every step, plus
//! assertions on what the C did so that an upstream fix fails here and says
//! which divergence it retired.

use bm_wire::bcmp::dfu_core::{DfuErr, EVENT_QUEUE_LEN, EventType, State};
use bm_wire_diff::dfu_core::{
    CoreState, DfuCoreInput, FrameType, Image, NodeRef, Pair, Sender, Step, TestImage, check, reset,
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

/// A 5000-byte image in 1000-byte chunks, from [`NodeRef::Peer`].
const OFFERED: TestImage = TestImage {
    len: 5000,
    huge: false,
    chunk_size: 1000,
    seed: 0x5a,
    crc_ok: true,
    own_sha: false,
    force: false,
    major: 2,
    minor: 3,
};

fn offer(pair: &mut Pair, image: TestImage) {
    pair.apply(&Step::Offer {
        src: NodeRef::Peer,
        image,
    });
    pair.apply(&Step::RunAll);
}

fn serve(pair: &mut Pair) {
    pair.apply(&Step::Serve {
        src: NodeRef::Peer,
        short_by: 0,
    });
    pair.apply(&Step::RunAll);
}

/// `client_golden` end to end: offer, five chunks, validation, the reboot
/// request, the host's reboot, activation. The slot's bytes, the reboot info
/// and every frame are compared at each step.
#[test]
fn a_whole_transfer_agrees_with_the_c_client() {
    let mut pair = reset();
    offer(&mut pair, OFFERED);
    assert_eq!(pair.c.state(), State::ClientReceiving);
    for _ in 0..5 {
        serve(&mut pair);
    }
    assert_eq!(pair.c.state(), State::ClientRebootReq);
    pair.apply(&message(8, NodeRef::Peer, &[]));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::ClientActivating);
    assert_eq!(pair.fx.counts.pending_and_reset, 1);
    assert_eq!(pair.fx.counts.writes, 3, "two whole pages and the rest");
}

/// `client_validate_fail`, both halves: a CRC one off, then an image one
/// chunk short of its size.
#[test]
fn a_bad_crc_and_a_short_image_fail_validation() {
    let mut pair = reset();
    offer(
        &mut pair,
        TestImage {
            crc_ok: false,
            ..OFFERED
        },
    );
    for _ in 0..5 {
        serve(&mut pair);
    }
    assert_eq!(pair.c.state(), State::Idle);
    assert_eq!(pair.rust.core().error(), DfuErr::BAD_CRC);

    offer(&mut pair, OFFERED);
    for _ in 0..4 {
        serve(&mut pair);
    }
    pair.apply(&Step::Serve {
        src: NodeRef::Peer,
        short_by: 1,
    });
    pair.apply(&Step::RunAll);
    assert_eq!(pair.rust.core().error(), DfuErr::MISMATCH_LEN);
}

/// `client_reject_same_sha`, `client_force_update`, `chunks_too_big`, and an
/// image the 256 KiB slot cannot hold.
#[test]
fn offers_the_client_refuses() {
    let mut pair = reset();
    let own = TestImage {
        own_sha: true,
        ..OFFERED
    };
    offer(&mut pair, own);
    assert_eq!(pair.c.state(), State::Idle);
    offer(&mut pair, TestImage { force: true, ..own });
    assert_eq!(pair.c.state(), State::ClientReceiving);

    drop(pair);
    let mut pair = reset();
    offer(
        &mut pair,
        TestImage {
            chunk_size: 1025,
            ..OFFERED
        },
    );
    assert_eq!(pair.rust.core().error(), DfuErr::CHUNK_SIZE);

    drop(pair);
    let mut pair = reset();
    offer(
        &mut pair,
        TestImage {
            huge: true,
            ..OFFERED
        },
    );
    assert_eq!(pair.c.state(), State::Idle);
    assert_eq!(pair.fx.counts.opens, 1);
    assert_eq!(pair.fx.counts.closes, 0, "left open");
}

/// `client_recv_fail`, on the clock: five unanswered chunk requests two
/// seconds apart, then the abort.
#[test]
fn the_chunk_timer_gives_up_after_five_timeouts() {
    let mut pair = reset();
    offer(&mut pair, OFFERED);
    for _ in 0..5 {
        pair.apply(&Step::Advance(2_000));
        pair.apply(&Step::RunAll);
    }
    assert_eq!(pair.c.state(), State::Idle);
    assert_eq!(pair.rust.core().error(), DfuErr::TIMEOUT);
}

/// `client_reboot_req_fail`, on the clock.
#[test]
fn the_reboot_request_gives_up_after_five_timeouts() {
    let mut pair = reset();
    offer(&mut pair, OFFERED);
    for _ in 0..5 {
        serve(&mut pair);
    }
    assert_eq!(pair.c.state(), State::ClientRebootReq);
    for _ in 0..5 {
        pair.apply(&Step::Advance(2_000));
        pair.apply(&Step::RunAll);
    }
    assert_eq!(pair.c.state(), State::Idle);
}

/// `client_resync_host`, and divergence #62: a second offer mid-transfer
/// restarts it from chunk zero, still against the first offer's image.
#[test]
fn a_second_offer_restarts_against_the_first_image() {
    let mut pair = reset();
    offer(&mut pair, OFFERED);
    serve(&mut pair);
    serve(&mut pair);
    offer(
        &mut pair,
        TestImage {
            len: 1000,
            ..OFFERED
        },
    );
    assert_eq!(pair.rust.roles().client.current_chunk(), 0);
    assert_eq!(pair.rust.roles().client.num_chunks(), 5, "not 1");
}

/// `bm_dfu_client_flash_area_open`, `_erase` and `_write` failing, and
/// divergence #63's request after a failed write.
#[test]
fn slot_failures_agree_with_the_c() {
    for (open, erase) in [(true, false), (false, true)] {
        let mut pair = reset();
        pair.apply(&Step::Faults {
            open,
            erase,
            write: false,
        });
        offer(&mut pair, OFFERED);
        assert_eq!(pair.c.state(), State::Error, "fatal");
        assert_eq!(pair.rust.core().error(), DfuErr::FLASH_ACCESS);
    }

    let mut pair = reset();
    offer(&mut pair, OFFERED);
    // Two 1000-byte chunks fill no page; the third writes one.
    serve(&mut pair);
    serve(&mut pair);
    pair.apply(&Step::Faults {
        open: false,
        erase: false,
        write: true,
    });
    pair.apply(&Step::Serve {
        src: NodeRef::Peer,
        short_by: 0,
    });
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::Error);
    assert_eq!(pair.rust.core().error(), DfuErr::BM_FRAME);
    assert!(
        pair.rust
            .core()
            .timer_deadline(bm_wire::bcmp::dfu_core::Timer::Chunk)
            .is_some(),
        "re-armed in Error"
    );
}

fn boot_into(pair: &mut Pair, magic: bool, own_sha: bool) {
    pair.apply(&Step::SetRebootInfo {
        magic,
        major: 2,
        minor: 3,
        host: NodeRef::Peer,
        own_sha,
    });
    pair.apply(&Step::SetPending(CoreState::Init));
    pair.apply(&Step::Run);
    pair.apply(&Step::Post(EventType::InitSuccess as u8));
    pair.apply(&Step::Run);
}

/// `client_golden_image_has_updated`: booted into the new image, the client
/// reports and the host's END confirms it.
#[test]
fn a_rebooted_client_confirms_on_the_hosts_end() {
    let mut pair = reset();
    boot_into(&mut pair, true, true);
    assert_eq!(pair.c.state(), State::ClientRebootDone);
    pair.apply(&message(3, NodeRef::Peer, &[1, 0]));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);
    assert_eq!(pair.fx.counts.confirmed, 1);
}

/// `reboot_done_fail`: the wrong image fails at once; the right one after
/// five unanswered boot-completes.
#[test]
fn a_rebooted_client_fails_the_update() {
    let mut pair = reset();
    boot_into(&mut pair, true, false);
    assert_eq!(pair.fx.counts.fail_and_reset, 1);

    drop(pair);
    let mut pair = reset();
    boot_into(&mut pair, true, true);
    for _ in 0..5 {
        pair.apply(&Step::Advance(2_000));
        pair.apply(&Step::RunAll);
    }
    assert_eq!(pair.fx.counts.fail_and_reset, 1);
}

/// `client_confirm_skip`, through the real config store: `dfu_confirm` of
/// zero confirms without the host and commits `dfu_confirm` back to 1.
#[test]
fn dfu_confirm_zero_skips_the_host() {
    let mut pair = reset();
    pair.apply(&Step::SetConfirm(0));
    boot_into(&mut pair, true, true);
    assert_eq!(pair.fx.counts.confirmed, 1);
    assert_eq!(pair.fx.counts.config_resets, 1);
    pair.apply(&Step::SetConfirm(1));
}

/// A chunk timer left running into Idle fires inside the next offer's
/// 10 ms `bm_delay`, and its timeout is then run as a retry.
#[test]
fn a_stale_chunk_timer_fires_inside_the_offers_delay() {
    let mut pair = reset();
    offer(&mut pair, OFFERED);
    pair.apply(&Step::SetPending(CoreState::Idle));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);
    pair.apply(&Step::Advance(1_995));
    offer(&mut pair, OFFERED);
    assert_eq!(pair.rust.roles().client.retries(), 1);
}

/// The host's ACK timer, armed by every `BeginHost`, posts its timeout ten
/// seconds later even after the machine has left `HostReqUpdate`.
#[test]
fn the_host_ack_timer_outlives_its_state() {
    let mut pair = reset();
    pair.apply(&Step::Advance(10_000));
    assert_eq!(
        pair.rust.core().queue().iter().next().map(|e| e.kind),
        Some(EventType::AckTimeout)
    );
    pair.apply(&Step::RunAll);
}
