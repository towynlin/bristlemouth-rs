//! `dfu_test.cpp`'s client goldens, step for step, plus the timer and the
//! quirks the goldens do not reach.
//!
//! The goldens drive `bm_dfu_test_set_dfu_event_and_run_sm`, which is
//! [`Dfu::run_event`]. Their fixture is [`Fake`]: a 4096-byte slot whose
//! operations succeed, `node_id` [`SELF`] and `git_sha` 0xd00dd00d unless a
//! test says otherwise.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;
use crate::bcmp::MessageType;
use crate::bcmp::dfu::{DfuChunk, DfuStart};
use crate::bcmp::dfu_core::{Dfu, Event, EventData};
use crate::configuration::{ConfigStore, Layout};

const SELF: u64 = 0xdead_beef_beef_feed;
const HOST: u64 = 0xbeef_beef_daad_baad;
const OTHER: u64 = 0xdead_dead_dead_dead;
const CHUNK_SIZE: u16 = 512;
const IMAGE_SIZE: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Open,
    Close,
    Erase(u32, u32),
    Write(u32, usize),
    Confirmed,
    PendingAndReset,
    FailAndReset,
    Commit,
}

struct Fake {
    sent: Vec<Vec<u8>>,
    calls: Vec<Call>,
    flash: Vec<u8>,
    fail_write: bool,
    fail_open: bool,
    git_sha: u32,
    config: Option<ConfigStore>,
}

impl Fake {
    fn new() -> Self {
        Self {
            sent: Vec::new(),
            calls: Vec::new(),
            flash: vec![0; 4096],
            fail_write: false,
            fail_open: false,
            git_sha: 0xd00d_d00d,
            config: Some(ConfigStore::new(Layout::LP64)),
        }
    }

    fn types(&self) -> Vec<MessageType> {
        self.sent
            .iter()
            .map(|b| MessageType(u16::from(b[0])))
            .collect()
    }

    fn last(&self) -> Option<MessageType> {
        self.types().last().copied()
    }

    fn last_message(&self) -> DfuMessage<'_> {
        DfuMessage::decode(self.sent.last().expect("sent")).expect("decodes")
    }

    fn count(&self, call: &Call) -> usize {
        self.calls.iter().filter(|c| *c == call).count()
    }
}

impl Effects for Fake {
    fn send(&mut self, message: &DfuMessage<'_>) {
        let mut buf = vec![0; message.encoded_len()];
        message.encode(&mut buf).expect("sized");
        self.sent.push(buf);
    }
    fn lpm_peripheral_active(&mut self) {}
    fn lpm_peripheral_inactive(&mut self) {}
    fn update_finished(&mut self, _success: bool, _err: DfuErr, _node_id: u64) {}
    fn flash_open(&mut self) -> bool {
        self.calls.push(Call::Open);
        !self.fail_open
    }
    fn flash_close(&mut self) -> bool {
        self.calls.push(Call::Close);
        true
    }
    fn flash_size(&mut self) -> u32 {
        self.flash.len() as u32
    }
    fn flash_erase(&mut self, offset: u32, len: u32) -> bool {
        self.calls.push(Call::Erase(offset, len));
        self.flash[offset as usize..(offset + len) as usize].fill(0xff);
        true
    }
    fn flash_write(&mut self, offset: u32, data: &[u8]) -> bool {
        self.calls.push(Call::Write(offset, data.len()));
        if self.fail_write {
            return false;
        }
        self.flash[offset as usize..offset as usize + data.len()].copy_from_slice(data);
        true
    }
    fn host_get_chunk(&mut self, _offset: u32, _buf: &mut [u8]) -> bool {
        unreachable!("a client-only node does not host")
    }
    fn set_confirmed(&mut self) {
        self.calls.push(Call::Confirmed);
    }
    fn set_pending_and_reset(&mut self) {
        self.calls.push(Call::PendingAndReset);
    }
    fn fail_update_and_reset(&mut self) {
        self.calls.push(Call::FailAndReset);
    }
    fn git_sha(&self) -> u32 {
        self.git_sha
    }
    fn config(&mut self) -> Option<&mut ConfigStore> {
        self.config.as_mut()
    }
    fn commit_config(&mut self, _partition: Partition) -> bool {
        self.calls.push(Call::Commit);
        true
    }
}

fn client(info: RebootInfo) -> (Dfu<Client>, Fake) {
    (Dfu::new(SELF, info, Client::new()), Fake::new())
}

fn run(dfu: &mut Dfu<Client>, fx: &mut Fake, event: Event) {
    let now = dfu.core().now();
    dfu.run_event(event, fx, now);
}

fn bare(kind: EventType) -> Event {
    Event::bare(kind)
}

fn msg(kind: EventType, message: DfuMessage<'_>) -> Event {
    let mut buf = vec![0; message.encoded_len()];
    message.encode(&mut buf).expect("sized");
    Event::message(kind, &buf).expect("fits")
}

fn info() -> ImgInfo {
    ImgInfo {
        image_size: IMAGE_SIZE,
        chunk_size: CHUNK_SIZE,
        crc16: 0x2fdf,
        major_ver: 1,
        minor_ver: 7,
        filter_key: 0,
        git_sha: 0xdead_d00d,
    }
}

fn start(img_info: ImgInfo) -> Event {
    msg(
        EventType::ReceivedUpdateRequest,
        DfuMessage::Start(DfuStart {
            addresses: DfuAddress {
                src_node_id: HOST,
                dst_node_id: SELF,
            },
            img_info,
        }),
    )
}

fn chunk(payload: &[u8]) -> Event {
    msg(
        EventType::ImageChunk,
        DfuMessage::Payload(DfuChunk {
            addresses: DfuAddress {
                src_node_id: HOST,
                dst_node_id: SELF,
            },
            payload,
        }),
    )
}

fn from_host(kind: EventType, message: fn(DfuAddress) -> DfuMessage<'static>) -> Event {
    msg(
        kind,
        message(DfuAddress {
            src_node_id: HOST,
            dst_node_id: SELF,
        }),
    )
}

fn end_from_host(success: u8) -> Event {
    msg(
        EventType::UpdateEnd,
        DfuMessage::End(DfuResult {
            addresses: DfuAddress {
                src_node_id: HOST,
                dst_node_id: SELF,
            },
            success,
            err_code: 0,
        }),
    )
}

/// INIT SUCCESS, then the start, as every client golden begins.
fn receiving(img_info: ImgInfo) -> (Dfu<Client>, Fake) {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::Idle);
    run(&mut dfu, &mut fx, start(img_info));
    assert_eq!(dfu.state(), State::ClientReceiving);
    assert_eq!(
        fx.types(),
        [MessageType::DFU_ACK, MessageType::DFU_PAYLOAD_REQ]
    );
    (dfu, fx)
}

/// Four 512-byte chunks of 0xa5, the goldens' image.
fn receive_golden_image(dfu: &mut Dfu<Client>, fx: &mut Fake) {
    let payload = [0xa5; CHUNK_SIZE as usize];
    for n in 1..4 {
        run(dfu, fx, chunk(&payload));
        assert_eq!(dfu.state(), State::ClientReceiving, "{n} chunks");
        assert_eq!(
            fx.last_message(),
            DfuMessage::PayloadReq(crate::bcmp::dfu::DfuChunkRequest {
                addresses: DfuAddress {
                    src_node_id: SELF,
                    dst_node_id: HOST
                },
                seq_num: n,
            })
        );
    }
    run(dfu, fx, chunk(&payload));
    assert_eq!(dfu.state(), State::ClientValidating);
}

/// `client_golden`. Its `crc16` of 0x2fdf is the CRC-16/CCITT of 2048 bytes
/// of 0xa5 — the one image checksum bm_core's tests assert.
#[test]
fn client_golden() {
    let (mut dfu, mut fx) = receiving(info());
    receive_golden_image(&mut dfu, &mut fx);
    assert_eq!(crc16_ccitt(0, &[0xa5; 2048]), 0x2fdf);
    assert_eq!(dfu.roles().running_crc16(), 0x2fdf);
    assert_eq!(&fx.flash[..2048], &[0xa5; 2048][..]);
    assert_eq!(
        fx.calls,
        [
            Call::Open,
            Call::Erase(0, 4096),
            Call::Write(0, 2048),
            Call::Close
        ]
    );

    // Validating
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::ClientRebootReq);
    assert_eq!(fx.last(), Some(MessageType::DFU_REBOOT_REQ));

    // Reboot
    run(
        &mut dfu,
        &mut fx,
        from_host(EventType::Reboot, DfuMessage::Reboot),
    );
    assert_eq!(dfu.state(), State::ClientActivating);
    assert_eq!(fx.count(&Call::PendingAndReset), 1);
    assert_eq!(
        *dfu.core().reboot_info(),
        RebootInfo {
            magic: DFU_REBOOT_MAGIC,
            major: 1,
            minor: 7,
            host_node_id: HOST,
            git_sha: 0xdead_d00d,
        }
    );
}

/// `client_reject_same_sha`: a NACK, and no change of state.
#[test]
fn client_reject_same_sha() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    fx.git_sha = 0xdead_d00d;
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    run(&mut dfu, &mut fx, start(info()));
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(
        fx.last_message(),
        DfuMessage::Ack(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: HOST
            },
            success: 0,
            err_code: DfuErr::SAME_VER.0,
        })
    );
    assert!(fx.calls.is_empty(), "the slot is not touched");
}

/// `client_force_update`.
#[test]
fn client_force_update() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    fx.git_sha = 0xdead_d00d;
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    let forced = ImgInfo {
        filter_key: IMG_INFO_FORCE_UPDATE,
        ..info()
    };
    run(&mut dfu, &mut fx, start(forced));
    assert_eq!(dfu.state(), State::ClientReceiving);
    assert_eq!(
        fx.types(),
        [MessageType::DFU_ACK, MessageType::DFU_PAYLOAD_REQ]
    );
}

fn rebooted(git_sha: u32) -> RebootInfo {
    RebootInfo {
        magic: DFU_REBOOT_MAGIC,
        major: 1,
        minor: 7,
        host_node_id: HOST,
        git_sha,
    }
}

/// `client_golden_image_has_updated`.
#[test]
fn client_golden_image_has_updated() {
    let (mut dfu, mut fx) = client(rebooted(0xdead_d00d));
    fx.git_sha = 0xdead_d00d;
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::ClientRebootDone);
    assert_eq!(fx.last(), Some(MessageType::DFU_BOOT_COMPLETE));

    run(&mut dfu, &mut fx, end_from_host(1));
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(fx.last(), Some(MessageType::DFU_END));
    assert_eq!(fx.count(&Call::Confirmed), 1);
    assert_eq!(*dfu.core().reboot_info(), RebootInfo::default());
}

/// `client_resync_host`: a second start re-acks and asks for chunk zero.
#[test]
fn client_resync_host() {
    let (mut dfu, mut fx) = receiving(info());
    run(&mut dfu, &mut fx, start(info()));
    assert_eq!(dfu.state(), State::ClientReceiving);
    assert_eq!(
        fx.types(),
        [
            MessageType::DFU_ACK,
            MessageType::DFU_PAYLOAD_REQ,
            MessageType::DFU_ACK,
            MessageType::DFU_PAYLOAD_REQ
        ]
    );
}

/// `client_recv_fail`: the fifth chunk timeout aborts.
#[test]
fn client_recv_fail() {
    let (mut dfu, mut fx) = receiving(info());
    for n in 1..5 {
        run(&mut dfu, &mut fx, bare(EventType::ChunkTimeout));
        assert_eq!(dfu.state(), State::ClientReceiving, "retry {n}");
        assert_eq!(fx.last(), Some(MessageType::DFU_PAYLOAD_REQ));
    }
    run(&mut dfu, &mut fx, bare(EventType::ChunkTimeout));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::TIMEOUT);
    assert_eq!(
        fx.last_message(),
        DfuMessage::Abort(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: HOST
            },
            success: 0,
            err_code: DfuErr::ABORTED.0,
        })
    );
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Idle);
}

/// `client_validate_fail`: a bad CRC, then a short image.
#[test]
fn client_validate_fail() {
    let bad_crc = ImgInfo {
        crc16: 0xdead,
        ..info()
    };
    let (mut dfu, mut fx) = receiving(bad_crc);
    receive_golden_image(&mut dfu, &mut fx);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::BAD_CRC);
    assert_eq!(fx.last(), Some(MessageType::DFU_END));
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Idle);

    run(&mut dfu, &mut fx, start(bad_crc));
    assert_eq!(dfu.state(), State::ClientReceiving);
    let payload = [0xa5; CHUNK_SIZE as usize];
    for _ in 0..3 {
        run(&mut dfu, &mut fx, chunk(&payload));
    }
    run(&mut dfu, &mut fx, chunk(&payload[..1])); // 1537
    assert_eq!(dfu.state(), State::ClientValidating);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::MISMATCH_LEN);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Idle);
}

/// `chunks_too_big`: an abort, then Error.
#[test]
fn chunks_too_big() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    let big = ImgInfo {
        chunk_size: CHUNK_SIZE * 100,
        ..info()
    };
    run(&mut dfu, &mut fx, start(big));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::CHUNK_SIZE);
    assert_eq!(fx.types(), [MessageType::DFU_ABORT]);
}

/// `client_reboot_req_fail`.
#[test]
fn client_reboot_req_fail() {
    let (mut dfu, mut fx) = receiving(info());
    receive_golden_image(&mut dfu, &mut fx);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::ClientRebootReq);
    for n in 1..5 {
        run(&mut dfu, &mut fx, bare(EventType::ChunkTimeout));
        assert_eq!(dfu.state(), State::ClientRebootReq, "retry {n}");
        assert_eq!(fx.last(), Some(MessageType::DFU_REBOOT_REQ));
    }
    run(&mut dfu, &mut fx, bare(EventType::ChunkTimeout));
    assert_eq!(dfu.state(), State::Error);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Idle);
}

/// `reboot_done_fail`: the wrong image fails the update at once; the right
/// one fails after five unanswered boot-completes.
#[test]
fn reboot_done_fail() {
    let (mut dfu, mut fx) = client(RebootInfo {
        major: 0,
        ..rebooted(0xbaad_dead)
    });
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::ClientRebootDone);
    assert_eq!(fx.count(&Call::FailAndReset), 1);
    assert_eq!(
        fx.last_message(),
        DfuMessage::End(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: HOST
            },
            success: 0,
            err_code: DfuErr::WRONG_VER.0,
        })
    );
    assert_eq!(*dfu.core().reboot_info(), RebootInfo::default());

    let (mut dfu, mut fx) = client(rebooted(0xdead_d00d));
    fx.git_sha = 0xdead_d00d;
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    assert_eq!(fx.last(), Some(MessageType::DFU_BOOT_COMPLETE));
    for n in 1..5 {
        run(&mut dfu, &mut fx, bare(EventType::ChunkTimeout));
        assert_eq!(dfu.state(), State::ClientRebootDone, "retry {n}");
        assert_eq!(fx.last(), Some(MessageType::DFU_BOOT_COMPLETE));
    }
    run(&mut dfu, &mut fx, bare(EventType::ChunkTimeout));
    assert_eq!(fx.count(&Call::FailAndReset), 1);
    assert_eq!(
        fx.last_message(),
        DfuMessage::Abort(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: HOST
            },
            success: 0,
            err_code: DfuErr::CONFIRMATION_ABORT.0,
        })
    );
}

/// `client_confirm_skip`: `dfu_confirm` reads 0, so the client confirms
/// itself, clears the reboot info, and writes `dfu_confirm` back to 1.
#[test]
fn client_confirm_skip() {
    let (mut dfu, mut fx) = client(rebooted(0xdead_d00d));
    fx.git_sha = 0xdead_d00d;
    let key = Key::new(DFU_CONFIRM_KEY);
    let store = fx.config.as_mut().expect("store");
    assert!(store.partition_mut(Partition::System).set_uint(key, 0));

    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    assert_eq!(dfu.state(), State::ClientRebootDone);
    assert_eq!(*dfu.core().reboot_info(), RebootInfo::default());
    assert_eq!(fx.calls, [Call::Confirmed, Call::Commit]);
    assert!(fx.sent.is_empty());
    let store = fx.config.as_ref().expect("store");
    assert_eq!(store.partition(Partition::System).get_uint(key), Some(1));
}

/// No store, or no key, reads as enabled.
#[test]
fn confirmation_is_on_unless_the_key_says_otherwise() {
    for config in [None, Some(ConfigStore::new(Layout::LP64))] {
        let (mut dfu, mut fx) = client(rebooted(0xdead_d00d));
        fx.git_sha = 0xdead_d00d;
        fx.config = config;
        run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
        assert_eq!(fx.types(), [MessageType::DFU_BOOT_COMPLETE]);
    }
}

/// The chunk timer is armed on entry, after the start's 10 ms delay, and
/// re-armed by a host heartbeat.
#[test]
fn the_chunk_timer_posts_a_timeout_two_seconds_after_the_request() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    dfu.run_event(bare(EventType::InitSuccess), &mut fx, 1000);
    dfu.run_event(start(info()), &mut fx, 1000);
    assert_eq!(dfu.core().now(), 1010, "bm_delay(10)");
    assert_eq!(dfu.core().timer_deadline(Timer::Chunk), Some(3010));
    while dfu.step(&mut fx, 1010).is_some() {}

    dfu.poll(3009);
    assert!(dfu.core().queue().is_empty());
    dfu.run_event(
        from_host(EventType::Heartbeat, DfuMessage::Heartbeat),
        &mut fx,
        3009,
    );
    assert_eq!(dfu.next_deadline(), Some(5009));
    dfu.poll(5009);
    let queued: Vec<_> = dfu.core().queue().iter().map(|e| e.kind).collect();
    assert_eq!(queued, [EventType::ChunkTimeout]);
    assert_eq!(dfu.next_deadline(), None, "one-shot");
    assert_eq!(dfu.step(&mut fx, 5009), Some(EventType::ChunkTimeout));
    assert_eq!(dfu.roles().retries(), 1);
    assert_eq!(fx.last(), Some(MessageType::DFU_PAYLOAD_REQ));
}

/// A timer that comes due inside a delay posts its event before the run
/// that delayed has finished, ahead of that run's own NOP.
#[test]
fn a_timer_due_during_a_delay_is_queued_ahead_of_the_nop() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    dfu.run_event(bare(EventType::InitSuccess), &mut fx, 0);
    while dfu.step(&mut fx, 0).is_some() {}
    dfu.core_mut().start_timer(Timer::Chunk);
    dfu.run_event(start(info()), &mut fx, 1995);
    let queued: Vec<_> = dfu.core().queue().iter().map(|e| e.kind).collect();
    assert_eq!(queued, [EventType::ChunkTimeout, EventType::None]);
    assert_eq!(dfu.core().timer_deadline(Timer::Chunk), Some(4005));
    // Run in ClientReceiving, it counts as a retry.
    dfu.step(&mut fx, 2005);
    assert_eq!(dfu.roles().retries(), 1);
}

/// Divergence #62: a start from the host during a transfer restarts it
/// against the first start's image.
#[test]
fn a_resync_keeps_the_first_images_size_and_crc() {
    let (mut dfu, mut fx) = receiving(info());
    run(&mut dfu, &mut fx, chunk(&[0xa5; 512]));
    let other = ImgInfo {
        image_size: 512,
        chunk_size: 256,
        crc16: 0x1234,
        ..info()
    };
    run(&mut dfu, &mut fx, start(other));
    assert_eq!(dfu.roles().current_chunk(), 0);
    assert_eq!(dfu.roles().num_chunks(), 4, "still the first image's");
    assert_eq!(dfu.roles().image_size(), IMAGE_SIZE);
    assert_eq!(fx.count(&Call::Erase(0, 4096)), 1, "not erased again");
    receive_golden_image(&mut dfu, &mut fx);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::ClientRebootReq);
}

/// Divergence #63: a failed page write sets the error and still asks for
/// the next chunk.
#[test]
fn a_failed_write_still_requests_the_next_chunk() {
    let (mut dfu, mut fx) = receiving(ImgInfo {
        image_size: 4000,
        chunk_size: 1024,
        ..info()
    });
    fx.fail_write = true;
    run(&mut dfu, &mut fx, chunk(&[1; 1024]));
    run(&mut dfu, &mut fx, chunk(&[2; 1024]));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::BM_FRAME);
    assert_eq!(fx.last(), Some(MessageType::DFU_PAYLOAD_REQ));
    assert_eq!(dfu.roles().current_chunk(), 2);
    assert!(
        dfu.core().timer_deadline(Timer::Chunk).is_some(),
        "re-armed after the error stopped it"
    );
}

/// Divergence #63, on the last chunk: the image is finished, the change to
/// Validating replaces the change to Error, and the failure is reported as a
/// length mismatch.
#[test]
fn a_failed_write_on_the_last_chunk_goes_to_validating() {
    let (mut dfu, mut fx) = receiving(ImgInfo {
        image_size: 2048,
        chunk_size: 1024,
        ..info()
    });
    run(&mut dfu, &mut fx, chunk(&[1; 1024]));
    fx.fail_write = true;
    run(&mut dfu, &mut fx, chunk(&[2; 1024]));
    assert_eq!(dfu.state(), State::ClientValidating);
    assert_eq!(fx.count(&Call::Close), 1);
    assert_eq!(
        dfu.core().error(),
        DfuErr::MISMATCH_LEN,
        "BM_FRAME overwritten"
    );
    assert_eq!(
        fx.last_message(),
        DfuMessage::End(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: HOST
            },
            success: 0,
            err_code: DfuErr::MISMATCH_LEN.0,
        })
    );
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Error);
}

/// An image the slot cannot hold is NACKed and the slot left open.
#[test]
fn an_image_as_large_as_the_slot_is_refused() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    run(
        &mut dfu,
        &mut fx,
        start(ImgInfo {
            image_size: 4096,
            ..info()
        }),
    );
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(fx.calls, [Call::Open]);
    assert_eq!(
        fx.last_message(),
        DfuMessage::Ack(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: HOST
            },
            success: 0,
            err_code: DfuErr::TOO_LARGE.0,
        })
    );
}

/// A slot that will not open is a NACK and a fatal error.
#[test]
fn a_slot_that_will_not_open_is_fatal() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    fx.fail_open = true;
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    run(&mut dfu, &mut fx, start(info()));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::FLASH_ACCESS);
    run(&mut dfu, &mut fx, bare(EventType::None));
    assert_eq!(dfu.state(), State::Error, "until reboot");
}

/// Divergence #57: `chunk_size` zero asks for one chunk of a non-empty image.
#[test]
fn chunk_size_zero_is_one_chunk() {
    let (dfu, _) = receiving(ImgInfo {
        chunk_size: 0,
        ..info()
    });
    assert_eq!(dfu.roles().num_chunks(), 1);
    let (dfu, _) = receiving(ImgInfo {
        chunk_size: 0,
        image_size: 0,
        ..info()
    });
    assert_eq!(dfu.roles().num_chunks(), 0);
}

/// A chunk over 1024 bytes is ignored, timer and all; one of any size up to
/// that is taken whatever the offered `chunk_size`.
#[test]
fn chunk_length_is_checked_against_the_maximum_only() {
    let (mut dfu, mut fx) = receiving(info());
    let deadline = dfu.core().timer_deadline(Timer::Chunk);
    let sent = fx.sent.len();
    run(&mut dfu, &mut fx, chunk(&[0; 1025]));
    assert_eq!(fx.sent.len(), sent);
    assert_eq!(dfu.core().timer_deadline(Timer::Chunk), deadline);
    run(&mut dfu, &mut fx, chunk(&[0; 1024]));
    assert_eq!(dfu.roles().current_chunk(), 1);
    assert_eq!(dfu.roles().flash_offset(), 0, "under a page");
}

/// Only the host is heard in a client state.
#[test]
fn a_client_accepts_only_its_host() {
    let (mut dfu, _) = receiving(info());
    let mut body = [0; DfuMessage::MIN_LEN];
    DfuMessage::Heartbeat(DfuAddress {
        src_node_id: OTHER,
        dst_node_id: SELF,
    })
    .encode(&mut body)
    .expect("fits");
    assert_eq!(
        dfu.on_message(&body),
        crate::bcmp::dfu_core::Accepted::WrongPeer
    );
}

/// A bare `ReceivedUpdateRequest` or `ImageChunk` carries no body and does
/// nothing.
#[test]
fn events_without_a_body_are_ignored() {
    let (mut dfu, mut fx) = client(RebootInfo::default());
    run(&mut dfu, &mut fx, bare(EventType::InitSuccess));
    run(&mut dfu, &mut fx, bare(EventType::ReceivedUpdateRequest));
    assert_eq!(dfu.state(), State::Idle);
    assert!(fx.sent.is_empty());
    let (mut dfu, mut fx) = receiving(info());
    run(&mut dfu, &mut fx, bare(EventType::ImageChunk));
    assert_eq!(fx.sent.len(), 2);
    assert!(matches!(dfu.core().current_event().data, EventData::None));
}
