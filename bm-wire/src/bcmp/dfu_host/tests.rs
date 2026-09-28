//! `dfu_test.cpp`'s host goldens, step for step, plus the timers and the
//! quirks the goldens do not reach.
//!
//! The goldens drive `bm_dfu_test_set_dfu_event_and_run_sm`, which is
//! [`Dfu::run_event`]. Their `bm_dfu_host_get_chunk` fake succeeds without
//! writing; [`Fake`] reads a slot holding an image at
//! [`ImgInfo::LEN`] instead, so the chunks' bytes can be asserted.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;
use crate::bcmp::MessageType;
use crate::bcmp::dfu::DfuResult;
use crate::bcmp::dfu_core::{Dfu, Event, HostStart, RebootInfo};
use crate::configuration::{ConfigStore, Partition};

const SELF: u64 = 0xdead_beef_beef_feed;
const CLIENT: u64 = 0xbeef_beef_daad_baad;
const OTHER: u64 = 0xdead_dead_dead_dead;
const CHUNK_SIZE: u16 = 512;
const IMAGE_SIZE: u32 = 2048;
const TIMEOUT_MS: u32 = 30_000;

struct Fake {
    sent: Vec<Vec<u8>>,
    finished: Vec<(bool, DfuErr, u64)>,
    reads: Vec<(u32, usize)>,
    flash: Vec<u8>,
}

impl Fake {
    fn new() -> Self {
        let mut flash = vec![0; 4096];
        for (i, b) in flash[ImgInfo::LEN..].iter_mut().enumerate() {
            *b = image_byte(i);
        }
        Self {
            sent: Vec::new(),
            finished: Vec::new(),
            reads: Vec::new(),
            flash,
        }
    }

    fn last(&self) -> Option<MessageType> {
        self.sent.last().map(|b| MessageType(u16::from(b[0])))
    }

    fn last_message(&self) -> DfuMessage<'_> {
        DfuMessage::decode(self.sent.last().expect("sent")).expect("decodes")
    }
}

fn image_byte(i: usize) -> u8 {
    (i as u8).wrapping_mul(7) ^ (i >> 8) as u8
}

impl Effects for Fake {
    fn send(&mut self, message: &DfuMessage<'_>) {
        let mut buf = vec![0; message.encoded_len()];
        message.encode(&mut buf).expect("sized");
        self.sent.push(buf);
    }
    fn lpm_peripheral_active(&mut self) {}
    fn lpm_peripheral_inactive(&mut self) {}
    fn update_finished(&mut self, success: bool, err: DfuErr, node_id: u64) {
        self.finished.push((success, err, node_id));
    }
    fn flash_open(&mut self) -> bool {
        unreachable!("the host does not write")
    }
    fn flash_close(&mut self) -> bool {
        unreachable!("the host does not write")
    }
    fn flash_size(&mut self) -> u32 {
        unreachable!("the host does not write")
    }
    fn flash_erase(&mut self, _offset: u32, _len: u32) -> bool {
        unreachable!("the host does not write")
    }
    fn flash_write(&mut self, _offset: u32, _data: &[u8]) -> bool {
        unreachable!("the host does not write")
    }
    fn host_get_chunk(&mut self, offset: u32, buf: &mut [u8]) -> bool {
        self.reads.push((offset, buf.len()));
        let start = offset as usize;
        match self.flash.get(start..start + buf.len()) {
            Some(bytes) => {
                buf.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }
    fn set_confirmed(&mut self) {
        unreachable!("the host does not boot")
    }
    fn set_pending_and_reset(&mut self) {
        unreachable!("the host does not boot")
    }
    fn fail_update_and_reset(&mut self) {
        unreachable!("the host does not boot")
    }
    fn git_sha(&self) -> u32 {
        0xd00d_d00d
    }
    fn config(&mut self) -> Option<&mut ConfigStore> {
        None
    }
    fn commit_config(&mut self, _partition: Partition) -> bool {
        false
    }
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

/// Idle, with `internal` as given: the goldens' `bm_dfu_init`, `InitSuccess`
/// and `bm_dfu_initiate_update(..., true)`, whose queued event they then
/// ignore.
fn host(internal: bool) -> (Dfu<ClientHost>, Fake) {
    let mut dfu = Dfu::new(SELF, RebootInfo::default(), ClientHost::new());
    let mut fx = Fake::new();
    dfu.run_event(Event::bare(EventType::InitSuccess), &mut fx, 0);
    dfu.run_event(Event::NONE, &mut fx, 0);
    assert_eq!(dfu.state(), State::Idle);
    assert!(dfu.initiate_update(&mut fx, info(), CLIENT, false, TIMEOUT_MS, internal));
    while dfu.pop().is_some() {}
    (dfu, fx)
}

fn run(dfu: &mut Dfu<ClientHost>, fx: &mut Fake, event: Event) {
    let now = dfu.core().now();
    dfu.run_event(event, fx, now);
}

fn begin_host(notify: bool) -> Event {
    Event {
        kind: EventType::BeginHost,
        data: EventData::HostStart(HostStart {
            start: DfuStart {
                addresses: DfuAddress {
                    src_node_id: SELF,
                    dst_node_id: CLIENT,
                },
                img_info: info(),
            },
            notify,
            timeout_ms: TIMEOUT_MS,
        }),
    }
}

fn msg(kind: EventType, message: DfuMessage<'_>) -> Event {
    let mut buf = vec![0; message.encoded_len()];
    message.encode(&mut buf).expect("sized");
    Event::message(kind, &buf).expect("fits")
}

fn from_client() -> DfuAddress {
    DfuAddress {
        src_node_id: CLIENT,
        dst_node_id: SELF,
    }
}

fn result(kind: EventType, success: u8, err: DfuErr) -> Event {
    let r = DfuResult {
        addresses: from_client(),
        success,
        err_code: err.0,
    };
    let message = match kind {
        EventType::AckReceived => DfuMessage::Ack(r),
        EventType::UpdateEnd => DfuMessage::End(r),
        EventType::Abort => DfuMessage::Abort(r),
        _ => unreachable!(),
    };
    msg(kind, message)
}

fn chunk_request(seq_num: u16) -> Event {
    msg(
        EventType::ChunkRequest,
        DfuMessage::PayloadReq(crate::bcmp::dfu::DfuChunkRequest {
            addresses: from_client(),
            seq_num,
        }),
    )
}

fn addressed(kind: EventType, message: fn(DfuAddress) -> DfuMessage<'static>) -> Event {
    msg(kind, message(from_client()))
}

/// Idle to `HostUpdate`: `BeginHost`, the NOP, then the client's ACK and
/// its NOP.
fn to_update(dfu: &mut Dfu<ClientHost>, fx: &mut Fake, notify: bool) {
    run(dfu, fx, begin_host(notify));
    assert_eq!(dfu.state(), State::HostReqUpdate);
    run(dfu, fx, result(EventType::AckReceived, 1, DfuErr::NONE));
    assert_eq!(dfu.state(), State::HostUpdate);
    while dfu.pop().is_some() {}
}

fn payload(fx: &Fake) -> Vec<u8> {
    match fx.last_message() {
        DfuMessage::Payload(c) => c.payload.to_vec(),
        other => panic!("not a chunk: {other:?}"),
    }
}

/// `host_golden`: start, ACK, a chunk, the reboot request, the boot
/// complete, the client's END.
#[test]
fn host_golden() {
    let (mut dfu, mut fx) = host(true);

    run(&mut dfu, &mut fx, begin_host(false));
    assert_eq!(dfu.state(), State::HostReqUpdate);
    assert_eq!(fx.last(), Some(MessageType::DFU_START));
    assert_eq!(
        fx.last_message(),
        DfuMessage::Start(DfuStart {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: CLIENT,
            },
            img_info: info(),
        })
    );
    assert_eq!(dfu.core().timer_deadline(Timer::Ack), Some(10_000));

    run(
        &mut dfu,
        &mut fx,
        result(EventType::AckReceived, 1, DfuErr::NONE),
    );
    assert_eq!(dfu.state(), State::HostUpdate);
    assert_eq!(dfu.core().timer_deadline(Timer::Ack), None);
    assert_eq!(dfu.core().timer_deadline(Timer::Update), Some(TIMEOUT_MS));

    run(&mut dfu, &mut fx, chunk_request(0));
    assert_eq!(dfu.state(), State::HostUpdate);
    assert_eq!(fx.last(), Some(MessageType::DFU_PAYLOAD));
    assert_eq!(fx.reads, [(ImgInfo::LEN as u32, usize::from(CHUNK_SIZE))]);
    assert_eq!(payload(&fx), fx.flash[18..18 + 512]);

    run(
        &mut dfu,
        &mut fx,
        addressed(EventType::RebootRequest, DfuMessage::RebootReq),
    );
    assert_eq!(dfu.state(), State::HostUpdate);
    assert_eq!(
        fx.last_message(),
        DfuMessage::Reboot(DfuAddress {
            src_node_id: SELF,
            dst_node_id: CLIENT,
        })
    );

    run(
        &mut dfu,
        &mut fx,
        addressed(EventType::BootComplete, DfuMessage::BootComplete),
    );
    assert_eq!(dfu.state(), State::HostUpdate);
    assert_eq!(
        fx.last_message(),
        DfuMessage::End(DfuResult {
            addresses: DfuAddress {
                src_node_id: SELF,
                dst_node_id: CLIENT,
            },
            success: 1,
            err_code: 0,
        })
    );

    run(
        &mut dfu,
        &mut fx,
        result(EventType::UpdateEnd, 1, DfuErr::NONE),
    );
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(dfu.core().timer_deadline(Timer::Update), None);
    assert!(fx.finished.is_empty(), "the golden passes no callback");
}

/// `host_req_update_fail`: two ACK timeouts, then a bare abort.
#[test]
fn host_req_update_fail() {
    let (mut dfu, mut fx) = host(true);
    run(&mut dfu, &mut fx, begin_host(false));
    assert_eq!(dfu.state(), State::HostReqUpdate);

    run(&mut dfu, &mut fx, Event::bare(EventType::AckTimeout));
    assert_eq!(dfu.state(), State::HostReqUpdate, "retry 1");
    assert_eq!(fx.sent.len(), 2, "the start again");
    assert_eq!(fx.last(), Some(MessageType::DFU_START));
    run(&mut dfu, &mut fx, Event::bare(EventType::AckTimeout));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::TIMEOUT);
    assert_eq!(fx.sent.len(), 2, "no third start");
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Idle);

    run(&mut dfu, &mut fx, begin_host(false));
    assert_eq!(dfu.state(), State::HostReqUpdate);
    assert_eq!(fx.last(), Some(MessageType::DFU_START));
    assert_eq!(dfu.roles().host.ack_retries(), 0, "reset on entry");

    run(&mut dfu, &mut fx, Event::bare(EventType::Abort));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::ABORTED);
    assert_eq!(dfu.core().timer_deadline(Timer::Ack), None);
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Idle);
}

/// `host_update_fail`: a bare abort in `HostUpdate`, then a second update
/// reaches `HostUpdate` again.
#[test]
fn host_update_fail() {
    let (mut dfu, mut fx) = host(true);
    to_update(&mut dfu, &mut fx, false);

    run(&mut dfu, &mut fx, Event::bare(EventType::Abort));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::ABORTED);
    assert_eq!(dfu.core().timer_deadline(Timer::Update), None);
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Idle);

    to_update(&mut dfu, &mut fx, false);
}

/// `host_update_fail_upon_reboot`: after the boot complete, the client's
/// abort carries `BmDfuErrConfirmationAbort`, which becomes the host's
/// error.
#[test]
fn host_update_fail_upon_reboot() {
    let (mut dfu, mut fx) = host(true);
    to_update(&mut dfu, &mut fx, false);
    run(
        &mut dfu,
        &mut fx,
        addressed(EventType::RebootRequest, DfuMessage::RebootReq),
    );
    assert_eq!(fx.last(), Some(MessageType::DFU_REBOOT));
    run(
        &mut dfu,
        &mut fx,
        addressed(EventType::BootComplete, DfuMessage::BootComplete),
    );
    assert_eq!(fx.last(), Some(MessageType::DFU_END));

    run(
        &mut dfu,
        &mut fx,
        result(EventType::Abort, 0, DfuErr::CONFIRMATION_ABORT),
    );
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::CONFIRMATION_ABORT);
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Idle);
}

/// A NACK's `err_code` becomes the host's error; 14 is fatal (divergence
/// #59).
#[test]
fn a_nack_carrying_flash_access_leaves_the_host_in_error() {
    let (mut dfu, mut fx) = host(true);
    run(&mut dfu, &mut fx, begin_host(true));
    run(
        &mut dfu,
        &mut fx,
        result(EventType::AckReceived, 0, DfuErr::FLASH_ACCESS),
    );
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::FLASH_ACCESS);
    assert_eq!(fx.finished, [(false, DfuErr::FLASH_ACCESS, CLIENT)]);
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Error, "fatal");
}

/// The client's END reaches the finish callback with its own `success` and
/// `err_code`, and returns to Idle whether or not it succeeded.
#[test]
fn the_clients_end_is_reported_to_the_callback() {
    let (mut dfu, mut fx) = host(true);
    to_update(&mut dfu, &mut fx, true);
    run(
        &mut dfu,
        &mut fx,
        result(EventType::UpdateEnd, 0, DfuErr::BAD_CRC),
    );
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(fx.finished, [(false, DfuErr::BAD_CRC, CLIENT)]);

    to_update(&mut dfu, &mut fx, true);
    fx.finished.clear();
    // Any nonzero byte is `true` once it is a C `bool`.
    run(
        &mut dfu,
        &mut fx,
        result(EventType::UpdateEnd, 7, DfuErr::NONE),
    );
    assert_eq!(fx.finished, [(true, DfuErr::NONE, CLIENT)]);
}

/// The whole image in order, then empty chunks, whatever `seq_num` asks for
/// (divergence #67).
#[test]
fn chunks_are_served_in_sequence_whatever_is_asked_for() {
    let (mut dfu, mut fx) = host(true);
    to_update(&mut dfu, &mut fx, false);
    let base = ImgInfo::LEN;
    for (i, seq_num) in [0u16, 0, 7, 1].into_iter().enumerate() {
        run(&mut dfu, &mut fx, chunk_request(seq_num));
        let start = base + i * 512;
        assert_eq!(payload(&fx), fx.flash[start..start + 512], "request {i}");
    }
    assert_eq!(dfu.roles().host.bytes_remaining(), 0);
    run(&mut dfu, &mut fx, chunk_request(4));
    assert_eq!(payload(&fx), [0u8; 0], "past the end");
    assert_eq!(fx.reads.last(), Some(&(base as u32 + IMAGE_SIZE, 0)));
    assert_eq!(dfu.state(), State::HostUpdate);
}

/// A failed read of the slot is `BmDfuErrFlashAccess`, which is fatal, and
/// nothing is sent.
#[test]
fn a_failed_read_is_a_fatal_flash_error() {
    let (mut dfu, mut fx) = host(true);
    to_update(&mut dfu, &mut fx, false);
    fx.flash.truncate(100);
    let sent = fx.sent.len();
    run(&mut dfu, &mut fx, chunk_request(0));
    assert_eq!(fx.sent.len(), sent);
    assert_eq!(dfu.core().error(), DfuErr::FLASH_ACCESS);
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Error);
}

/// A non-internal update reads the stream the application feeds, and
/// `HostUpdate`'s exit drops it.
#[test]
fn a_non_internal_update_reads_what_the_application_queued() {
    let (mut dfu, mut fx) = host(false);
    assert!(!dfu.roles_mut().host.queue_data(&[1]), "no buffer yet");
    to_update(&mut dfu, &mut fx, false);
    let host = &mut dfu.roles_mut().host;
    assert!(host.queue_data(&[0xAB; 500]));
    assert!(!host.queue_data(&[0xCD; 13]), "refused whole");
    assert!(host.queue_data(&[0xCD; 12]));
    run(&mut dfu, &mut fx, chunk_request(0));
    let mut expected = vec![0xAB; 500];
    expected.extend_from_slice(&[0xCD; 12]);
    assert_eq!(payload(&fx), expected);
    assert!(fx.reads.is_empty(), "the slot is not read");
    assert_eq!(dfu.roles().host.bytes_remaining(), IMAGE_SIZE - 512);

    run(
        &mut dfu,
        &mut fx,
        result(EventType::UpdateEnd, 1, DfuErr::NONE),
    );
    assert_eq!(dfu.state(), State::Idle);
    assert!(dfu.roles().host.stream().is_none());
}

/// A short read sends a whole chunk, zeros past the bytes read, and counts
/// only those read (divergence #68).
#[test]
fn a_short_read_sends_a_whole_chunk() {
    let (mut dfu, mut fx) = host(false);
    to_update(&mut dfu, &mut fx, false);
    assert!(dfu.roles_mut().host.queue_data(&[0x11; 100]));
    run(&mut dfu, &mut fx, chunk_request(0));
    let mut expected = vec![0x11; 100];
    expected.resize(512, 0);
    assert_eq!(payload(&fx), expected);
    assert_eq!(dfu.roles().host.bytes_remaining(), IMAGE_SIZE - 100);
}

/// An empty stream is the shim's timeout: `BmDfuErrFlashAccess`.
#[test]
fn an_empty_stream_is_a_fatal_flash_error() {
    let (mut dfu, mut fx) = host(false);
    to_update(&mut dfu, &mut fx, false);
    run(&mut dfu, &mut fx, chunk_request(0));
    assert_eq!(dfu.core().error(), DfuErr::FLASH_ACCESS);
}

/// Leaving `HostReqUpdate` other than into `HostUpdate` keeps the stream;
/// the next non-internal update replaces it with an empty one (the C leaks
/// it, divergence #61).
#[test]
fn a_failed_request_keeps_the_stream_until_the_next_update() {
    let (mut dfu, mut fx) = host(false);
    run(&mut dfu, &mut fx, begin_host(false));
    assert!(dfu.roles_mut().host.queue_data(&[1, 2, 3]));
    run(&mut dfu, &mut fx, Event::bare(EventType::Abort));
    run(&mut dfu, &mut fx, Event::NONE);
    assert_eq!(dfu.state(), State::Idle);
    assert_eq!(dfu.roles().host.stream().map(StreamBuffer::len), Some(3));

    run(&mut dfu, &mut fx, begin_host(false));
    assert_eq!(dfu.roles().host.stream().map(StreamBuffer::len), Some(0));
}

/// The update timer aborts `HostUpdate` after `timeoutMs`, counted from the
/// entry, however busy the transfer is.
#[test]
fn the_update_timer_aborts_the_whole_update() {
    let (mut dfu, mut fx) = host(true);
    run(&mut dfu, &mut fx, begin_host(true));
    run(
        &mut dfu,
        &mut fx,
        result(EventType::AckReceived, 1, DfuErr::NONE),
    );
    while dfu.pop().is_some() {}
    dfu.run_event(chunk_request(0), &mut fx, TIMEOUT_MS - 1);
    assert_eq!(dfu.step(&mut fx, TIMEOUT_MS - 1), None);
    assert_eq!(dfu.step(&mut fx, TIMEOUT_MS), Some(EventType::Abort));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(dfu.core().error(), DfuErr::ABORTED);
    assert_eq!(fx.finished, [(false, DfuErr::ABORTED, CLIENT)]);
}

/// Unanswered starts go out ten seconds apart, twice in all.
#[test]
fn the_ack_timer_resends_the_start_once() {
    let (mut dfu, mut fx) = host(true);
    run(&mut dfu, &mut fx, begin_host(false));
    while dfu.pop().is_some() {}
    assert_eq!(dfu.step(&mut fx, 9_999), None);
    assert_eq!(dfu.step(&mut fx, 10_000), Some(EventType::AckTimeout));
    assert_eq!(fx.sent.len(), 2);
    assert_eq!(dfu.step(&mut fx, 20_000), Some(EventType::AckTimeout));
    assert_eq!(dfu.state(), State::Error);
    assert_eq!(fx.sent.len(), 2);
}

/// Only the client may speak to a host.
#[test]
fn a_host_accepts_messages_only_from_its_client() {
    let (mut dfu, mut fx) = host(true);
    run(&mut dfu, &mut fx, begin_host(false));
    let mut body = [0u8; DfuMessage::WITH_TWO_BYTES_LEN];
    DfuMessage::Ack(DfuResult {
        addresses: DfuAddress {
            src_node_id: OTHER,
            dst_node_id: SELF,
        },
        success: 1,
        err_code: 0,
    })
    .encode(&mut body)
    .expect("sized");
    assert_eq!(
        dfu.on_message(&body),
        crate::bcmp::dfu_core::Accepted::WrongPeer
    );
    body[1..9].copy_from_slice(&CLIENT.to_le_bytes());
    assert_eq!(
        dfu.on_message(&body),
        crate::bcmp::dfu_core::Accepted::Queued(EventType::AckReceived)
    );
}

#[test]
fn a_stream_buffer_of_zero_bytes_is_none() {
    assert!(StreamBuffer::new(0).is_none());
    let mut s = StreamBuffer::new(4).expect("some");
    assert!(s.send(&[1, 2, 3]));
    let mut buf = [0u8; 2];
    assert_eq!(s.receive(&mut buf), Some(2));
    assert!(s.send(&[4, 5, 6]), "wraps");
    let mut buf = [0u8; 8];
    assert_eq!(s.receive(&mut buf), Some(4));
    assert_eq!(buf[..4], [3, 4, 5, 6]);
    assert_eq!(s.receive(&mut buf), None);
}

/// A `chunk_size` of zero divides nothing on the host: every chunk is empty
/// and the transfer never advances (divergence #57).
#[test]
fn a_zero_chunk_size_sends_empty_chunks() {
    let (mut dfu, mut fx) = host(true);
    let mut start = begin_host(false);
    if let EventData::HostStart(h) = &mut start.data {
        h.start.img_info.chunk_size = 0;
    }
    run(&mut dfu, &mut fx, start);
    run(
        &mut dfu,
        &mut fx,
        result(EventType::AckReceived, 1, DfuErr::NONE),
    );
    for seq_num in 0..3 {
        run(&mut dfu, &mut fx, chunk_request(seq_num));
        assert_eq!(payload(&fx), [0u8; 0]);
    }
    assert_eq!(dfu.roles().host.bytes_remaining(), IMAGE_SIZE);
}
