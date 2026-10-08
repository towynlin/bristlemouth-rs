//! `dfu_test.cpp`'s state sequences, as far as the core decides them.
//!
//! bm_core's goldens drive `bm_dfu_test_set_dfu_event_and_run_sm` through the
//! real client and host. Those are `dfu_client`'s and `dfu_host`'s tests; here
//! [`StandIn`] makes the calls into the core that `dfu_client.c` and
//! `dfu_host.c` make at the same step,
//! so every state enum asserted below is the golden's, and each test names the
//! golden it follows. What the roles decide for themselves (the SHA check, the
//! chunk count, the retry limits) is not tested here.

extern crate std;

use std::vec::Vec;

use super::*;

/// `node_id_fake.return_val` in `dfu_test.cpp`.
const SELF: u64 = 0xdead_beef_beef_feed;
/// The peer every golden uses.
const PEER: u64 = 0xbeef_beef_daad_baad;
/// `process_message_test`'s sender.
const OTHER: u64 = 0xdead_dead_dead_dead;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fx {
    Sent(MessageType),
    SentBytes(Vec<u8>),
    Active,
    Inactive,
    Finished(bool, DfuErr, u64),
}

#[derive(Default)]
struct Recorder(Vec<Fx>);

impl Effects for Recorder {
    fn send(&mut self, message: &DfuMessage<'_>) {
        self.0.push(Fx::Sent(message.message_type()));
        let mut buf = [0u8; 64];
        let n = message.encode(&mut buf).expect("fits");
        self.0.push(Fx::SentBytes(buf[..n].to_vec()));
    }
    fn lpm_peripheral_active(&mut self) {
        self.0.push(Fx::Active);
    }
    fn lpm_peripheral_inactive(&mut self) {
        self.0.push(Fx::Inactive);
    }
    fn update_finished(&mut self, success: bool, err: DfuErr, node_id: u64) {
        self.0.push(Fx::Finished(success, err, node_id));
    }
    fn flash_open(&mut self) -> bool {
        unreachable!("the stand-in roles touch no flash")
    }
    fn flash_close(&mut self) -> bool {
        unreachable!("the stand-in roles touch no flash")
    }
    fn flash_size(&mut self) -> u32 {
        unreachable!("the stand-in roles touch no flash")
    }
    fn flash_erase(&mut self, _offset: u32, _len: u32) -> bool {
        unreachable!("the stand-in roles touch no flash")
    }
    fn flash_write(&mut self, _offset: u32, _data: &[u8]) -> bool {
        unreachable!("the stand-in roles touch no flash")
    }
    fn host_get_chunk(&mut self, _offset: u32, _buf: &mut [u8]) -> bool {
        unreachable!("the stand-in roles touch no flash")
    }
    fn set_confirmed(&mut self) {
        unreachable!("the stand-in roles do not boot")
    }
    fn set_pending_and_reset(&mut self) {
        unreachable!("the stand-in roles do not boot")
    }
    fn fail_update_and_reset(&mut self) {
        unreachable!("the stand-in roles do not boot")
    }
    fn git_sha(&self) -> u32 {
        0
    }
    fn config(&mut self) -> Option<&mut crate::configuration::ConfigStore> {
        None
    }
    fn commit_config(&mut self, _partition: crate::configuration::Partition) -> bool {
        false
    }
}

impl Recorder {
    fn sent(&self) -> Vec<MessageType> {
        self.0
            .iter()
            .filter_map(|f| match f {
                Fx::Sent(t) => Some(*t),
                _ => None,
            })
            .collect()
    }

    fn lpm(&self) -> Vec<Fx> {
        self.0
            .iter()
            .filter(|f| matches!(f, Fx::Active | Fx::Inactive))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Entry(State),
    Run(State, EventType),
    Exit(State),
    UpdateRequest,
    HostParams(bool, u32),
}

/// Roles that record their calls and do what the C roles do to the core at
/// the steps the goldens take.
#[derive(Default)]
struct StandIn {
    calls: Vec<Call>,
    host_node_id: u64,
    client_node_id: u64,
}

impl Roles for StandIn {
    fn entry(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        self.calls.push(Call::Entry(state));
        match state {
            // s_client_receiving_entry
            State::ClientReceiving => core.req_next_chunk(fx, self.host_node_id, 0),
            // s_host_req_update_entry, via bm_dfu_host_req_update
            State::HostReqUpdate => {
                if let EventData::HostStart(start) = core.current_event().data {
                    self.client_node_id = start.start.addresses.dst_node_id;
                    fx.send(&DfuMessage::Start(start.start));
                }
            }
            // s_client_update_done_entry, when the SHA matches
            State::ClientRebootDone => {
                self.host_node_id = core.reboot_info().host_node_id;
                fx.send(&DfuMessage::BootComplete(DfuAddress {
                    src_node_id: core.self_node_id(),
                    dst_node_id: self.host_node_id,
                }));
            }
            _ => {}
        }
    }

    fn run(&mut self, state: State, core: &mut Core, fx: &mut dyn Effects) {
        let kind = core.current_event().kind;
        self.calls.push(Call::Run(state, kind));
        match (state, kind) {
            // s_client_receiving_run: a repeated start re-acks and stays.
            (State::ClientReceiving, EventType::ReceivedUpdateRequest) => {
                core.send_ack(fx, self.host_node_id, 1, DfuErr::NONE);
                core.req_next_chunk(fx, self.host_node_id, 0);
            }
            // s_host_req_update_run: an ACK with success set.
            (State::HostReqUpdate, EventType::AckReceived) => {
                core.set_pending_state_change(State::HostUpdate);
            }
            // bm_dfu_host_transition_to_error, from s_host_*_run on an abort.
            (State::HostReqUpdate | State::HostUpdate, EventType::Abort) => {
                let err = match core.current_event().message_body() {
                    Some(DfuMessage::Abort(r)) => DfuErr(r.err_code),
                    _ => DfuErr::ABORTED,
                };
                core.set_error(err);
                core.set_pending_state_change(State::Error);
            }
            // s_client_update_done_run: the host's END.
            (State::ClientRebootDone, EventType::UpdateEnd) => {
                core.update_end(fx, self.host_node_id, 1, DfuErr::NONE);
                core.set_pending_state_change(State::Idle);
            }
            _ => {}
        }
    }

    fn exit(&mut self, state: State, _core: &mut Core, _fx: &mut dyn Effects) {
        self.calls.push(Call::Exit(state));
    }

    fn client_process_update_request(&mut self, core: &mut Core, fx: &mut dyn Effects) {
        self.calls.push(Call::UpdateRequest);
        // The accepting branch of bm_dfu_client_process_update_request.
        if let Some(DfuMessage::Start(start)) = core.current_event().message_body() {
            self.host_node_id = start.addresses.src_node_id;
            core.send_ack(fx, self.host_node_id, 1, DfuErr::NONE);
            core.set_pending_state_change(State::ClientReceiving);
        }
    }

    fn host_set_params(&mut self, notify: bool, timeout_ms: u32) {
        self.calls.push(Call::HostParams(notify, timeout_ms));
    }

    fn client_host_node_valid(&self, node_id: u64) -> bool {
        self.host_node_id == node_id
    }

    fn host_client_node_valid(&self, node_id: u64) -> bool {
        self.client_node_id == node_id
    }
}

fn dfu() -> Dfu<StandIn> {
    Dfu::new(SELF, RebootInfo::default(), StandIn::default())
}

/// `bm_dfu_test_set_dfu_event_and_run_sm`.
fn run(dfu: &mut Dfu<StandIn>, fx: &mut Recorder, event: Event) {
    dfu.run_event(event, fx, 0);
}

fn body(message: DfuMessage<'_>) -> Vec<u8> {
    let mut buf = [0u8; 128];
    let n = message.encode(&mut buf).expect("fits");
    buf[..n].to_vec()
}

fn message_event(kind: EventType, message: DfuMessage<'_>) -> Event {
    Event::message(kind, &body(message)).expect("fits")
}

/// `client_golden`'s start request.
fn start_from_peer() -> DfuMessage<'static> {
    DfuMessage::Start(DfuStart {
        addresses: DfuAddress {
            src_node_id: PEER,
            dst_node_id: SELF,
        },
        img_info: ImgInfo {
            image_size: 2048,
            chunk_size: 512,
            crc16: 0x2fdf,
            major_ver: 1,
            minor_ver: 7,
            filter_key: 0,
            git_sha: 0xdead_d00d,
        },
    })
}

fn result(src: u64, dst: u64, success: u8, err_code: u8) -> DfuResult {
    DfuResult {
        addresses: DfuAddress {
            src_node_id: src,
            dst_node_id: dst,
        },
        success,
        err_code,
    }
}

/// `host_golden`'s image.
fn host_info() -> ImgInfo {
    ImgInfo {
        image_size: 2048,
        chunk_size: 512,
        crc16: 0x2fdf,
        major_ver: 1,
        minor_ver: 7,
        filter_key: 0,
        git_sha: 0xdead_d00d,
    }
}

fn to_idle(dfu: &mut Dfu<StandIn>, fx: &mut Recorder) {
    run(dfu, fx, Event::bare(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::Idle);
}

#[test]
fn the_enums_have_the_c_values() {
    assert_eq!(State::HostUpdate as u8, 9);
    assert_eq!(EventType::BootComplete as u8, 14);
    assert_eq!(DfuErr::FLASH_ACCESS.0, 14);
    for (i, s) in State::ALL.iter().enumerate() {
        assert_eq!(State::from_u8(i as u8), Some(*s));
    }
    assert_eq!(State::from_u8(10), None);
}

/// `init_test`: `bm_dfu_init` queues one `InitSuccess` and runs nothing.
#[test]
fn init_queues_init_success_and_runs_nothing() {
    let dfu = dfu();
    assert_eq!(dfu.state(), State::Init);
    let queued: Vec<_> = dfu.core().queue().iter().map(|e| e.kind).collect();
    assert_eq!(queued, [EventType::InitSuccess]);
    assert!(dfu.roles().calls.is_empty());
}

/// `dfu_api_test`'s first step, taken from the queue as the task would.
#[test]
fn init_success_moves_to_idle_and_clears_the_reboot_info() {
    let info = RebootInfo {
        magic: 0x1234_5678,
        major: 1,
        minor: 2,
        host_node_id: PEER,
        git_sha: 3,
    };
    let mut dfu = Dfu::new(SELF, info, StandIn::default());
    let mut fx = Recorder::default();
    assert_eq!(dfu.step(&mut fx, 0), Some(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(*dfu.core().reboot_info(), RebootInfo::default());
    assert_eq!(fx.0, [Fx::Inactive]);
    // The pending change's NOP is still queued; it runs Idle with nothing.
    assert_eq!(dfu.step(&mut fx, 0), Some(EventType::None));
    assert_eq!(dfu.step(&mut fx, 0), None);
    assert_eq!(dfu.state(), State::Idle);
}

/// `client_golden_image_has_updated` and `reboot_done_fail`'s first step: the
/// magic sends a booted client to `ClientRebootDone`, bypassing Idle, so the
/// reboot info survives for the client to read.
#[test]
fn the_reboot_magic_resumes_a_client_update() {
    let info = RebootInfo {
        magic: DFU_REBOOT_MAGIC,
        major: 1,
        minor: 7,
        host_node_id: PEER,
        git_sha: 0xdead_d00d,
    };
    let mut dfu = Dfu::new(SELF, info, StandIn::default());
    let mut fx = Recorder::default();
    run(&mut dfu, &mut fx, Event::bare(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::ClientRebootDone);
    assert_eq!(*dfu.core().reboot_info(), info);
    assert_eq!(fx.sent(), [MessageType::DFU_BOOT_COMPLETE]);
    assert!(fx.lpm().is_empty(), "Idle was never entered");

    // REBOOT_DONE: the host's END confirms, and the client goes idle.
    let end = DfuMessage::End(result(PEER, SELF, 1, 0));
    run(&mut dfu, &mut fx, message_event(EventType::UpdateEnd, end));
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(fx.sent().last(), Some(&MessageType::DFU_END));
    assert_eq!(*dfu.core().reboot_info(), RebootInfo::default());
}

/// Init ignores everything but `InitSuccess`.
#[test]
fn init_ignores_other_events() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    for kind in [EventType::None, EventType::BeginHost, EventType::Abort] {
        run(&mut dfu, &mut fx, Event::bare(kind));
        assert_eq!(dfu.state(), State::Init);
    }
    assert!(fx.0.is_empty());
}

/// `process_message_test`: every DFU type addressed to this node is queued as
/// its event in Idle, whoever sent it.
#[test]
fn every_dfu_type_is_queued_as_its_event() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}

    let addr = DfuAddress {
        src_node_id: OTHER,
        dst_node_id: SELF,
    };
    let r = result(OTHER, SELF, 0, 0);
    let cases = [
        (
            DfuMessage::Start(DfuStart {
                addresses: addr,
                img_info: ImgInfo::default(),
            }),
            EventType::ReceivedUpdateRequest,
        ),
        (
            DfuMessage::PayloadReq(DfuChunkRequest {
                addresses: addr,
                seq_num: 0,
            }),
            EventType::ChunkRequest,
        ),
        (
            DfuMessage::Payload(crate::bcmp::dfu::DfuChunk {
                addresses: addr,
                payload: &[],
            }),
            EventType::ImageChunk,
        ),
        (DfuMessage::End(r), EventType::UpdateEnd),
        (DfuMessage::Ack(r), EventType::AckReceived),
        (DfuMessage::Abort(r), EventType::Abort),
        (DfuMessage::Heartbeat(addr), EventType::Heartbeat),
        (DfuMessage::RebootReq(addr), EventType::RebootRequest),
        (DfuMessage::Reboot(addr), EventType::Reboot),
        (DfuMessage::BootComplete(addr), EventType::BootComplete),
    ];
    for (message, kind) in cases {
        let b = body(message);
        assert_eq!(dfu.on_message(&b), Accepted::Queued(kind));
        let queued = dfu.pop().expect("queued");
        assert_eq!(queued.kind, kind);
        assert_eq!(queued.body(), Some(&b[..]));
    }
}

#[test]
fn a_message_for_another_node_or_of_no_dfu_type_is_not_queued() {
    let mut dfu = dfu();
    let mut other = body(DfuMessage::Heartbeat(DfuAddress {
        src_node_id: PEER,
        dst_node_id: SELF + 1,
    }));
    assert_eq!(dfu.on_message(&other), Accepted::NotForUs);
    other[9..17].copy_from_slice(&SELF.to_le_bytes());
    other[0] = 0xDA;
    assert_eq!(dfu.on_message(&other), Accepted::UnknownType);
    assert_eq!(dfu.on_message(&other[..16]), Accepted::Truncated);
    assert_eq!(dfu.core().queue().len(), 1, "only InitSuccess");
}

/// The queue holds five; the sixth is refused.
#[test]
fn the_queue_holds_five() {
    let mut dfu = dfu();
    let hb = body(DfuMessage::Heartbeat(DfuAddress {
        src_node_id: PEER,
        dst_node_id: SELF,
    }));
    for _ in 0..4 {
        assert_eq!(dfu.on_message(&hb), Accepted::Queued(EventType::Heartbeat));
    }
    assert_eq!(dfu.on_message(&hb), Accepted::QueueFull);
    assert_eq!(dfu.core().queue().len(), EVENT_QUEUE_LEN);
}

/// `dfu_api_test`: the four senders, addressed from this node.
#[test]
fn the_senders_build_the_c_bodies() {
    let dfu = dfu();
    let mut fx = Recorder::default();
    let core = dfu.core();
    core.send_ack(&mut fx, PEER, 1, DfuErr::NONE);
    core.req_next_chunk(&mut fx, PEER, 0);
    core.update_end(&mut fx, PEER, 1, DfuErr::NONE);
    core.send_heartbeat(&mut fx, PEER);
    assert_eq!(
        fx.sent(),
        [
            MessageType::DFU_ACK,
            MessageType::DFU_PAYLOAD_REQ,
            MessageType::DFU_END,
            MessageType::DFU_HEARTBEAT
        ]
    );
    let ack =
        fx.0.iter()
            .find_map(|f| match f {
                Fx::SentBytes(b) => Some(b.clone()),
                _ => None,
            })
            .expect("sent");
    let mut expected = std::vec![0xD4];
    expected.extend_from_slice(&SELF.to_le_bytes());
    expected.extend_from_slice(&PEER.to_le_bytes());
    expected.extend_from_slice(&[1, 0]);
    assert_eq!(ack, expected, "source first, then destination");
}

/// `dfu_api_test`'s last assertion and `host_golden`'s first steps.
#[test]
fn begin_host_hands_the_start_to_the_host() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}

    assert!(dfu.initiate_update(&mut fx, host_info(), PEER, false, 30_000, true));
    assert!(dfu.core().internal());
    assert_eq!(dfu.step(&mut fx, 0), Some(EventType::BeginHost));
    assert_eq!(dfu.state(), State::HostReqUpdate);
    assert_eq!(fx.sent(), [MessageType::DFU_START]);
    assert_eq!(fx.lpm(), [Fx::Inactive, Fx::Active]);
    assert_eq!(dfu.core().client_node_id(), PEER);
    assert_eq!(
        dfu.roles().calls,
        [
            Call::HostParams(false, 30_000),
            Call::Entry(State::HostReqUpdate)
        ]
    );

    // HOST UPDATE: the client's ACK.
    let ack = DfuMessage::Ack(result(PEER, SELF, 1, 0));
    assert_eq!(
        dfu.on_message(&body(ack)),
        Accepted::Queued(EventType::AckReceived)
    );
    while dfu.step(&mut fx, 0).is_some() {}
    assert_eq!(dfu.state(), State::HostUpdate);
}

/// `client_golden`'s first two steps, and `client_resync_host`: a second
/// start in `ClientReceiving` is a change to the same state, which neither
/// exits nor re-enters it.
#[test]
fn a_change_to_the_current_state_neither_exits_nor_enters() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    run(
        &mut dfu,
        &mut fx,
        message_event(EventType::ReceivedUpdateRequest, start_from_peer()),
    );
    assert_eq!(dfu.state(), State::ClientReceiving);
    assert_eq!(
        fx.sent(),
        [MessageType::DFU_ACK, MessageType::DFU_PAYLOAD_REQ]
    );

    dfu.roles_mut().calls.clear();
    dfu.core_mut()
        .set_pending_state_change(State::ClientReceiving);
    run(
        &mut dfu,
        &mut fx,
        message_event(EventType::ReceivedUpdateRequest, start_from_peer()),
    );
    assert_eq!(dfu.state(), State::ClientReceiving);
    assert_eq!(
        dfu.roles().calls,
        [Call::Run(
            State::ClientReceiving,
            EventType::ReceivedUpdateRequest
        )]
    );
    assert_eq!(dfu.core().pending_state_change(), None);
}

/// A client accepts messages only from its host; Idle accepts anyone.
#[test]
fn a_client_state_drops_messages_from_anyone_but_its_host() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    run(
        &mut dfu,
        &mut fx,
        message_event(EventType::ReceivedUpdateRequest, start_from_peer()),
    );
    let from = |src| {
        body(DfuMessage::Heartbeat(DfuAddress {
            src_node_id: src,
            dst_node_id: SELF,
        }))
    };
    assert_eq!(dfu.on_message(&from(OTHER)), Accepted::WrongPeer);
    assert_eq!(
        dfu.on_message(&from(PEER)),
        Accepted::Queued(EventType::Heartbeat)
    );
}

/// `host_req_update_fail` and `host_update_fail`: an abort takes the host to
/// Error, and the NOP that Error's entry queues takes it to Idle.
#[test]
fn a_recoverable_error_returns_to_idle_on_the_next_run() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}
    assert!(dfu.initiate_update(&mut fx, host_info(), PEER, true, 1000, false));
    while dfu.step(&mut fx, 0).is_some() {}
    assert_eq!(dfu.state(), State::HostReqUpdate);

    let abort = DfuMessage::Abort(result(PEER, SELF, 0, DfuErr::ABORTED.0));
    run(&mut dfu, &mut fx, message_event(EventType::Abort, abort));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(
        fx.0.last(),
        Some(&Fx::Finished(false, DfuErr::ABORTED, PEER))
    );
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Idle);
}

/// `s_error_entry` leaves a fatal error in Error, and the host takes the
/// error from the client's byte (divergence #59).
#[test]
fn a_fatal_error_code_from_the_peer_leaves_the_host_in_error() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}
    assert!(dfu.initiate_update(&mut fx, host_info(), PEER, false, 1000, false));
    while dfu.step(&mut fx, 0).is_some() {}

    let abort = DfuMessage::Abort(result(PEER, SELF, 0, DfuErr::FLASH_ACCESS.0));
    assert_eq!(
        dfu.on_message(&body(abort)),
        Accepted::Queued(EventType::Abort)
    );
    while dfu.step(&mut fx, 0).is_some() {}
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::FLASH_ACCESS);

    // Nothing moves it, and no new update can start.
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Error);
    assert!(!dfu.initiate_update(&mut fx, host_info(), PEER, true, 1000, false));
    assert_eq!(
        fx.0.last(),
        Some(&Fx::Finished(false, DfuErr::IN_PROGRESS, PEER))
    );
}

/// Divergence #58: the error state calls the last host update's callback,
/// even for a client failure long after that update ended.
#[test]
fn a_client_error_is_reported_to_the_last_hosts_callback() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}
    assert!(dfu.initiate_update(&mut fx, host_info(), PEER, true, 1000, false));
    while dfu.step(&mut fx, 0).is_some() {}
    let abort = DfuMessage::Abort(result(PEER, SELF, 0, DfuErr::ABORTED.0));
    run(&mut dfu, &mut fx, message_event(EventType::Abort, abort));
    while dfu.step(&mut fx, 0).is_some() {}
    assert_eq!(dfu.state(), State::Idle);

    // Now a client, updated by a different host, times out.
    fx.0.clear();
    let mut start = start_from_peer();
    if let DfuMessage::Start(s) = &mut start {
        s.addresses.src_node_id = OTHER;
    }
    run(
        &mut dfu,
        &mut fx,
        message_event(EventType::ReceivedUpdateRequest, start),
    );
    assert_eq!(dfu.state(), State::ClientReceiving);
    dfu.core_mut().set_error(DfuErr::TIMEOUT);
    dfu.core_mut().set_pending_state_change(State::Error);
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Error);
    assert!(
        fx.0.contains(&Fx::Finished(false, DfuErr::TIMEOUT, PEER)),
        "reported against the old client, not the new host: {:?}",
        fx.0
    );
}

/// A pending change whose NOP found the queue full is taken after the next
/// event, which the old state runs first.
#[test]
fn a_dropped_nop_defers_the_change_to_the_next_event() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}
    run(
        &mut dfu,
        &mut fx,
        message_event(EventType::ReceivedUpdateRequest, start_from_peer()),
    );
    while dfu.step(&mut fx, 0).is_some() {}
    let hb = body(DfuMessage::Heartbeat(DfuAddress {
        src_node_id: PEER,
        dst_node_id: SELF,
    }));
    for _ in 0..EVENT_QUEUE_LEN {
        assert_eq!(dfu.on_message(&hb), Accepted::Queued(EventType::Heartbeat));
    }
    dfu.core_mut().set_error(DfuErr::TIMEOUT);
    dfu.core_mut().set_pending_state_change(State::Error);
    assert_eq!(dfu.core().queue().len(), EVENT_QUEUE_LEN, "NOP dropped");

    dfu.roles_mut().calls.clear();
    assert_eq!(dfu.step(&mut fx, 0), Some(EventType::Heartbeat));
    assert_eq!(
        dfu.roles().calls,
        [
            Call::Run(State::ClientReceiving, EventType::Heartbeat),
            Call::Exit(State::ClientReceiving)
        ]
    );
    assert_eq!(dfu.state(), State::Error);
}

/// Divergence #60: two calls before the first runs both pass the Idle check;
/// the second's event reaches the host state, which ignores it.
#[test]
fn a_second_initiate_before_the_first_runs_is_accepted_and_lost() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    while dfu.step(&mut fx, 0).is_some() {}
    assert!(dfu.initiate_update(&mut fx, host_info(), PEER, true, 1000, true));
    assert!(dfu.initiate_update(&mut fx, host_info(), OTHER, true, 1000, false));
    assert!(!dfu.core().internal(), "the second call's flag");
    while dfu.step(&mut fx, 0).is_some() {}
    assert_eq!(dfu.state(), State::HostReqUpdate);
    assert_eq!(dfu.core().client_node_id(), PEER);
    assert!(
        dfu.roles()
            .calls
            .contains(&Call::Run(State::HostReqUpdate, EventType::BeginHost))
    );
    assert!(!fx.0.iter().any(|f| matches!(f, Fx::Finished(..))));
}

#[test]
fn initiate_refuses_an_oversized_chunk_silently() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    to_idle(&mut dfu, &mut fx);
    let mut info = host_info();
    info.chunk_size = DFU_MAX_CHUNK_SIZE as u16 + 1;
    assert!(!dfu.initiate_update(&mut fx, info, PEER, true, 1000, true));
    assert!(fx.0.iter().all(|f| !matches!(f, Fx::Finished(..))));
}

#[test]
fn initiate_outside_idle_reports_in_progress() {
    let mut dfu = dfu();
    let mut fx = Recorder::default();
    assert!(!dfu.initiate_update(&mut fx, host_info(), PEER, true, 1000, true));
    assert_eq!(fx.0, [Fx::Finished(false, DfuErr::IN_PROGRESS, PEER)]);
    assert!(!dfu.initiate_update(&mut fx, host_info(), PEER, false, 1000, true));
    assert_eq!(fx.0.len(), 1, "no callback, no call");
}

#[test]
fn reboot_info_is_the_packed_layout() {
    let info = RebootInfo {
        magic: DFU_REBOOT_MAGIC,
        major: 1,
        minor: 7,
        host_node_id: PEER,
        git_sha: 0xdead_d00d,
    };
    let mut buf = [0u8; RebootInfo::LEN];
    info.encode(&mut buf).expect("fits");
    assert_eq!(
        buf,
        [
            0xFE, 0x0F, 0xDC, 0xBA, 1, 7, 0xad, 0xba, 0xad, 0xda, 0xef, 0xbe, 0xef, 0xbe, 0x0d,
            0xd0, 0xad, 0xde
        ]
    );
    assert_eq!(RebootInfo::decode(&buf), Ok(info));
    assert_eq!(RebootInfo::decode(&buf[..17]), Err(BmWireError::Truncated));
}

/// Timers post in deadline order, ties in creation order, once each.
#[test]
fn timers_fire_in_deadline_order_then_creation_order() {
    let mut dfu = dfu();
    dfu.pop();
    dfu.poll(100);
    dfu.core_mut().start_timer(Timer::Ack); // 10_100
    dfu.poll(8_100);
    dfu.core_mut().start_timer(Timer::Chunk); // 10_100
    assert_eq!(dfu.next_deadline(), Some(10_100));
    dfu.poll(20_000);
    let queued: Vec<_> = dfu.core().queue().iter().map(|e| e.kind).collect();
    assert_eq!(queued, [EventType::ChunkTimeout, EventType::AckTimeout]);
    assert_eq!(dfu.next_deadline(), None);

    dfu.core_mut().start_timer(Timer::Ack); // 30_000
    dfu.core_mut().delay(1_000);
    dfu.core_mut().start_timer(Timer::Chunk); // 23_000
    dfu.poll(40_000);
    let queued: Vec<_> = dfu.core().queue().iter().skip(2).map(|e| e.kind).collect();
    assert_eq!(
        queued,
        [EventType::ChunkTimeout, EventType::AckTimeout],
        "the later-started, earlier-due timer first"
    );
}

/// The clock only moves forward, across the wrap, and a delay moves it
/// past what the caller last said.
#[test]
fn the_clock_does_not_go_back() {
    let mut dfu = dfu();
    dfu.poll(u32::MAX - 5);
    dfu.core_mut().delay(10);
    assert_eq!(dfu.core().now(), 4);
    dfu.poll(u32::MAX);
    assert_eq!(dfu.core().now(), 4, "behind the delay");
    dfu.core_mut().start_timer(Timer::Chunk);
    assert_eq!(dfu.next_deadline(), Some(2_004));
    dfu.poll(2_003);
    assert_eq!(dfu.core().queue().len(), 1, "only InitSuccess");
    dfu.poll(2_004);
    assert_eq!(dfu.core().queue().len(), 2);
}
