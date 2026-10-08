//! The DFU core state machine, client and host, compared against
//! `bcmp/dfu_core.c`, `bcmp/dfu_client.c` and `bcmp/dfu_host.c` in bm_core's
//! live stack.
//!
//! Its own binary because `bm_wire_diff::dfu_core` brings the stack up, whose
//! `bcmp_init` runs `bm_dfu_init`; see `bm_wire_diff::stack`. Each test is a script for
//! [`check`], which compares the two machines after every step, plus
//! assertions on what the C did so that an upstream fix fails here and says
//! which divergence it retired.

use bm_wire::bcmp::dfu::ImgInfo;
use bm_wire::bcmp::dfu_client::Client;
use bm_wire::bcmp::dfu_core::{
    Accepted, Dfu, DfuErr, EVENT_QUEUE_LEN, EventType, RebootInfo, Roles, State,
};
use bm_wire::bcmp::dfu_host::ClientHost;
use bm_wire_diff::dfu_core::{
    CoreState, DfuCoreInput, FrameType, Image, NodeRef, Pair, Recorded, Sender, Step, TestImage,
    check, reset,
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

/// `D0 + n` from the host's current client.
fn from_client(n: u8, success: u8, err_code: u8) -> Step {
    Step::FromClient {
        n,
        success,
        err_code,
    }
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

/// Divergence #59 in the host: a client's NACK carrying
/// `BmDfuErrFlashAccess` — what a client whose flash fails sends — leaves the
/// host in Error for good, and no update can start until reboot.
#[test]
fn a_clients_fatal_nack_stops_the_host() {
    let mut pair = reset();
    pair.apply(&initiate(NodeRef::Peer, true, true));
    pair.apply(&Step::Run);
    assert_eq!(pair.c.state(), State::HostReqUpdate);
    pair.apply(&from_client(4, 0, DfuErr::FLASH_ACCESS.0));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Error);
    assert_eq!(pair.rust.core().error(), DfuErr::FLASH_ACCESS);
    pair.apply(&initiate(NodeRef::Peer, true, true));
    assert_eq!(pair.c.state(), State::Error, "no update can start");
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
    pair.apply(&initiate(NodeRef::Peer, false, true));
    pair.apply(&Step::Run);
    pair.apply(&Step::SetPending(CoreState::Idle));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);
    pair.apply(&Step::Advance(10_000));
    assert_eq!(
        pair.rust.core().queue().iter().next().map(|e| e.kind),
        Some(EventType::AckTimeout)
    );
    pair.apply(&Step::RunAll);
}

/// A 5000-byte image in 1000-byte chunks that a client running
/// [`GIT_SHA`](bm_wire_diff::stack::GIT_SHA) accepts, and after its reboot
/// finds itself running.
const HOSTED: TestImage = TestImage {
    len: 5000,
    huge: false,
    chunk_size: 1000,
    seed: 0xa5,
    crc_ok: true,
    own_sha: true,
    force: true,
    major: 4,
    minor: 5,
};

fn host(pair: &mut Pair, image: TestImage, internal: bool, timeout_ms: u32) {
    pair.apply(&Step::Host {
        dst: NodeRef::Peer,
        image,
        notify: true,
        timeout_ms,
        internal,
    });
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::HostReqUpdate);
}

fn to_host_update(pair: &mut Pair, image: TestImage, internal: bool) {
    host(pair, image, internal, 60_000);
    pair.apply(&from_client(4, 1, 0));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::HostUpdate);
}

fn request_chunk(pair: &mut Pair, seq_num: u16) {
    pair.apply(&message(1, NodeRef::Peer, &seq_num.to_le_bytes()));
    pair.apply(&Step::RunAll);
}

/// `host_golden` end to end: start, ACK, five chunks, the reboot request,
/// the boot complete, the client's END and the finish callback. Every frame
/// and the callback are compared.
#[test]
fn host_golden_agrees_with_the_c() {
    let mut pair = reset();
    to_host_update(&mut pair, HOSTED, true);
    for seq_num in 0..5 {
        request_chunk(&mut pair, seq_num);
    }
    assert_eq!(pair.rust.roles().host.bytes_remaining(), 0);
    pair.apply(&from_client(7, 0, 0));
    pair.apply(&Step::RunAll);
    pair.apply(&from_client(9, 0, 0));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::HostUpdate);
    pair.apply(&from_client(3, 1, 0));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);
}

/// Divergence #67: the chunk sent is the next in sequence, whatever
/// `seq_num` asks for, and past the end it is empty.
#[test]
fn the_host_serves_chunks_in_sequence_whatever_is_asked_for() {
    let mut pair = reset();
    to_host_update(&mut pair, HOSTED, true);
    for seq_num in [0, 0, 3, 1, 1, 9, 0] {
        request_chunk(&mut pair, seq_num);
    }
    let (_, last) = pair.last_sent.last().expect("a chunk");
    assert_eq!(last.len(), 19, "an empty chunk");
    assert_eq!(pair.c.state(), State::HostUpdate);
}

/// A non-internal update serves what the application queued; an empty
/// stream is `BmDfuErrFlashAccess`, which is fatal.
#[test]
fn a_non_internal_update_serves_what_was_fed() {
    let mut pair = reset();
    pair.apply(&Step::Feed(10));
    to_host_update(&mut pair, HOSTED, false);
    pair.apply(&Step::Feed(1000));
    pair.apply(&Step::Feed(1)); // refused: the buffer holds one chunk
    request_chunk(&mut pair, 0);
    pair.apply(&Step::Feed(600));
    pair.apply(&Step::Feed(400));
    request_chunk(&mut pair, 1);
    request_chunk(&mut pair, 2);
    assert_eq!(pair.c.state(), State::Error);
    assert_eq!(pair.rust.core().error(), DfuErr::FLASH_ACCESS);
}

/// Divergence #68 is out of domain: a chunk request that would find the
/// stream part full is discarded on both sides.
#[test]
fn a_part_fed_chunk_is_not_run() {
    let mut pair = reset();
    to_host_update(&mut pair, HOSTED, false);
    pair.apply(&Step::Feed(100));
    request_chunk(&mut pair, 0);
    assert!(pair.last_sent.is_empty());
    assert_eq!(pair.rust.roles().host.stream().map(|s| s.len()), Some(100));
}

/// Divergence #61 is out of domain: a failed non-internal update keeps its
/// stream buffer, and the next non-internal `BeginHost` is discarded on both
/// sides rather than leak it; an internal one runs, and its exit frees it.
#[test]
fn a_leaked_stream_buffer_bars_the_next_non_internal_update() {
    let mut pair = reset();
    host(&mut pair, HOSTED, false, 60_000);
    pair.apply(&from_client(5, 0, DfuErr::ABORTED.0));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle);
    assert!(pair.rust.roles().host.stream().is_some());
    pair.apply(&Step::Host {
        dst: NodeRef::Peer,
        image: HOSTED,
        notify: true,
        timeout_ms: 60_000,
        internal: false,
    });
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::Idle, "discarded");
    to_host_update(&mut pair, HOSTED, true);
    pair.apply(&from_client(3, 1, 0));
    pair.apply(&Step::RunAll);
    assert!(pair.rust.roles().host.stream().is_none());
}

/// `host_req_update_fail`, on the clock: a second start ten seconds after
/// the first, then `BmDfuErrTimeout` ten seconds after that.
#[test]
fn an_unanswered_start_is_sent_twice() {
    let mut pair = reset();
    host(&mut pair, HOSTED, true, 60_000);
    pair.apply(&Step::Advance(10_000));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.last_sent.len(), 1, "the second start");
    pair.apply(&Step::Advance(10_000));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.rust.core().error(), DfuErr::TIMEOUT);
    assert_eq!(pair.c.state(), State::Idle);
}

/// The update timer aborts `HostUpdate` `timeoutMs` after its entry, and the
/// callback hears `BmDfuErrAborted`.
#[test]
fn the_update_timer_aborts_the_update() {
    let mut pair = reset();
    host(&mut pair, HOSTED, true, 5_000);
    pair.apply(&from_client(4, 1, 0));
    pair.apply(&Step::RunAll);
    request_chunk(&mut pair, 0);
    pair.apply(&Step::Advance(4_999));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.c.state(), State::HostUpdate);
    pair.apply(&Step::Advance(1));
    pair.apply(&Step::RunAll);
    assert_eq!(pair.rust.core().error(), DfuErr::ABORTED);
    assert_eq!(pair.c.state(), State::Idle);
}

/// A bm-wire node that is not the oracle, and its seams.
struct Peer<R> {
    dfu: Dfu<R>,
    fx: Recorded,
}

impl<R: Roles> Peer<R> {
    fn new(roles: R, reboot_info: RebootInfo) -> Self {
        let mut peer = Self {
            dfu: Dfu::new(NodeRef::Peer.id(), reboot_info, roles),
            fx: Recorded {
                flash: vec![0; 256 * 1024],
                ..Recorded::default()
            },
        };
        peer.run(0);
        peer
    }

    fn run(&mut self, now_ms: u32) {
        while self.dfu.step(&mut self.fx, now_ms).is_some() {}
    }
}

/// One exchange between the oracle's pair and `peer`: everything the pair
/// sent goes to the peer, the peer runs, and everything it sent goes to the
/// pair, which runs. `false` once nothing moves.
fn exchange<R: Roles>(pair: &mut Pair, peer: &mut Peer<R>) -> bool {
    let mut moved = false;
    for (_, body) in std::mem::take(&mut pair.last_sent) {
        assert!(matches!(peer.dfu.on_message(&body), Accepted::Queued(_)));
        moved = true;
    }
    peer.run(pair.c.now());
    for (_, body) in std::mem::take(&mut peer.fx.sent) {
        pair.receive(&body);
        moved = true;
    }
    pair.apply(&Step::RunAll);
    moved || !pair.last_sent.is_empty()
}

fn exchange_until_quiet<R: Roles>(pair: &mut Pair, peer: &mut Peer<R>) {
    for _ in 0..100 {
        if !exchange(pair, peer) {
            return;
        }
    }
    panic!("still talking after 100 exchanges");
}

/// A whole update, one way round: a bm-wire host updates the C client, which
/// runs beside a bm-wire client given the same frames. Every frame the client
/// side sends is compared at every step; the transfer completes, the client
/// reboots into the image and confirms it, and the host hears success.
#[test]
fn a_rust_host_updates_the_c_client() {
    let mut pair = reset();
    let mut host = Peer::new(ClientHost::new(), RebootInfo::default());
    let image = HOSTED;
    let len = image.image_size() as usize;
    host.fx.flash[ImgInfo::LEN..ImgInfo::LEN + len].copy_from_slice(&image.bytes(0..len as u32));
    assert!(host.dfu.initiate_update(
        &mut host.fx,
        image.info(),
        NodeRef::This.id(),
        true,
        60_000,
        true
    ));
    host.run(pair.c.now());
    exchange_until_quiet(&mut pair, &mut host);
    assert_eq!(pair.c.state(), State::ClientActivating);
    assert_eq!(host.dfu.state(), State::HostUpdate);
    assert_eq!(pair.fx.counts.pending_and_reset, 1);
    assert_eq!(&pair.c.flash()[..len], image.bytes(0..len as u32));

    // The reboot: the C keeps `client_update_reboot_info` in no-init RAM.
    pair.apply(&Step::SetPending(CoreState::Init));
    pair.apply(&Step::Post(EventType::InitSuccess as u8));
    exchange_until_quiet(&mut pair, &mut host);
    assert_eq!(pair.c.state(), State::Idle);
    assert_eq!(pair.fx.counts.confirmed, 1);
    assert_eq!(host.dfu.state(), State::Idle);
    assert_eq!(host.fx.finished, [(true, 0, NodeRef::This.id())]);
}

/// A whole update, the other way round: the C host updates a bm-wire
/// client, beside a bm-wire host given the same frames. Every frame the host
/// side sends is compared at every step, and the finish callback too.
#[test]
fn the_c_host_updates_a_rust_client() {
    let mut pair = reset();
    let mut client = Peer::new(Client::new(), RebootInfo::default());
    pair.apply(&Step::Host {
        dst: NodeRef::Peer,
        image: HOSTED,
        notify: true,
        timeout_ms: 60_000,
        internal: true,
    });
    pair.apply(&Step::RunAll);
    exchange_until_quiet(&mut pair, &mut client);
    assert_eq!(client.dfu.state(), State::ClientActivating);
    assert_eq!(pair.c.state(), State::HostUpdate);
    let len = HOSTED.image_size();
    assert_eq!(client.fx.flash[..len as usize], HOSTED.bytes(0..len));

    // The reboot, with what the client left in no-init RAM.
    let reboot_info = *client.dfu.core().reboot_info();
    let mut rebooted = Peer::new(Client::new(), reboot_info);
    rebooted.fx.flash = std::mem::take(&mut client.fx.flash);
    pair.last_sent.clear();
    exchange_until_quiet(&mut pair, &mut rebooted);
    assert_eq!(rebooted.dfu.state(), State::Idle);
    assert_eq!(rebooted.fx.counts.confirmed, 1);
    assert_eq!(pair.c.state(), State::Idle);
}
