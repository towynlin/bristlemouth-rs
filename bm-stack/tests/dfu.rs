//! DFU on a node: frames in, frames out, and the slot and no-init RAM behind
//! them. The client's and host's behaviour is compared against the C in
//! `bm-wire-diff`; this is the plumbing.

use bm_stack::dfu::DfuFinished;
use bm_stack::mock::frames;
use bm_stack::{Event, Identity, NoConfig, NoDfu, Node, Outbound, RamDfuSlot, SoftRtc};
use bm_wire::bcmp::dfu::{
    DfuAddress, DfuChunk, DfuMessage, DfuResult, DfuStart, IMG_INFO_FORCE_UPDATE, ImgInfo,
};
use bm_wire::bcmp::dfu_core::{DFU_REBOOT_MAGIC, DfuErr, RebootInfo, State};
use bm_wire::bcmp::{DeviceInfo, MessageType, rx};
use bm_wire::crc::crc16_ccitt;
use bm_wire::util::BmIpAddr;

const NODE_ID: u64 = 0xC0FF_EE00_1234_5678;
const HOST: u64 = 0x0000_0000_55AA_0011;
const GIT_SHA: u32 = 0x1234_5678;
const PORTS: u8 = 2;
const SLOT: usize = 16 * 1024;

struct TestIdentity;

impl Identity for TestIdentity {
    fn node_id(&self) -> u64 {
        NODE_ID
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            git_sha: GIT_SHA,
            ..DeviceInfo::default()
        }
    }
}

struct HostIdentity;

impl Identity for HostIdentity {
    fn node_id(&self) -> u64 {
        HOST
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            git_sha: !GIT_SHA,
            ..DeviceInfo::default()
        }
    }
}

type DfuNode<'a, I = TestIdentity> = Node<
    I,
    SoftRtc,
    4,
    4,
    { bm_stack::node::PING_PAYLOAD_BYTES },
    { bm_stack::node::INFO_REQUESTS_DEFAULT },
    { bm_wire::bcmp::info::CACHED_STRING_BYTES },
    { bm_stack::node::RESOURCES_DEFAULT },
    { bm_wire::bcmp::resource::RESOURCE_NAME_BYTES },
    { bm_stack::node::RESOURCE_REQUESTS_DEFAULT },
    { bm_stack::node::SUBSCRIPTIONS_DEFAULT },
    NoConfig,
    &'a mut RamDfuSlot<SLOT>,
>;

fn node(slot: &mut RamDfuSlot<SLOT>) -> DfuNode<'_> {
    Node::with_dfu(TestIdentity, SoftRtc::new(), NoConfig, slot, PORTS)
}

fn frame_to(dst: BmIpAddr, message: &DfuMessage<'_>) -> Vec<u8> {
    frame_from(HOST, dst, message)
}

fn frame_from(src: u64, dst: BmIpAddr, message: &DfuMessage<'_>) -> Vec<u8> {
    let mut body = vec![0u8; message.encoded_len()];
    message.encode(&mut body).unwrap();
    frames::bcmp(src, dst, message.message_type(), 0, &body)
}

fn from_host(message: &DfuMessage<'_>) -> Vec<u8> {
    frame_to(BmIpAddr::GLOBAL_MULTICAST, message)
}

const TO_US: DfuAddress = DfuAddress {
    src_node_id: HOST,
    dst_node_id: NODE_ID,
};

const TO_HOST: DfuAddress = DfuAddress {
    src_node_id: NODE_ID,
    dst_node_id: HOST,
};

/// Every frame DFU owes at `now_ms`, decoded.
fn drain<I: Identity>(node: &mut DfuNode<'_, I>, now_ms: u32) -> Vec<(BmIpAddr, Vec<u8>)> {
    let mut out = Vec::new();
    while let Some(outbound) = node.next_dfu_transmission(now_ms) {
        out.push(decode(&outbound));
    }
    out
}

fn decode(outbound: &Outbound<'_>) -> (BmIpAddr, Vec<u8>) {
    let mut frame = outbound.frame().to_vec();
    let received = rx::accept(&mut frame).expect("valid");
    (received.dst, received.payload.to_vec())
}

fn bodies(frames: &[(BmIpAddr, Vec<u8>)]) -> Vec<DfuMessage<'_>> {
    frames
        .iter()
        .map(|(dst, body)| {
            assert_eq!(*dst, BmIpAddr::GLOBAL_MULTICAST, "bcmp_tx to ff03::1");
            DfuMessage::decode(body).expect("a DFU body")
        })
        .collect()
}

fn image() -> Vec<u8> {
    (0..5000u32).map(|i| (i * 7) as u8).collect()
}

fn offer(image: &[u8]) -> DfuMessage<'static> {
    DfuMessage::Start(DfuStart {
        addresses: TO_US,
        img_info: ImgInfo {
            image_size: image.len() as u32,
            chunk_size: 1000,
            crc16: crc16_ccitt(0, image),
            major_ver: 2,
            minor_ver: 3,
            filter_key: 0,
            git_sha: !GIT_SHA,
        },
    })
}

fn deliver(node: &mut DfuNode<'_>, now_ms: u32, frame: &[u8]) {
    let mut frame = frame.to_vec();
    let owed = node.on_frame(now_ms, 1, &mut frame);
    assert!(owed.reply.is_none() && owed.forward.is_none());
}

/// Offer, five chunks, reboot request, reboot: every frame the client sends
/// goes out through the node, the image lands in the slot, and the reset is
/// asked for with the reboot info already stored.
#[test]
fn a_node_receives_an_update_into_its_slot() {
    let image = image();
    let mut slot = RamDfuSlot::<SLOT>::new();
    let mut node = node(&mut slot);
    assert!(drain(&mut node, 0).is_empty(), "Init, then Idle");
    assert_eq!(node.dfu().machine().state(), State::Idle);

    deliver(&mut node, 100, &from_host(&offer(&image)));
    let sent = drain(&mut node, 100);
    assert_eq!(
        bodies(&sent),
        [
            DfuMessage::Ack(DfuResult {
                addresses: TO_HOST,
                success: 1,
                err_code: 0
            }),
            DfuMessage::PayloadReq(bm_wire::bcmp::dfu::DfuChunkRequest {
                addresses: TO_HOST,
                seq_num: 0
            }),
        ]
    );
    assert_eq!(
        node.dfu_remaining_ms(110),
        Some(2_000),
        "armed after the delay"
    );

    for (n, chunk) in image.chunks(1000).enumerate() {
        let message = DfuMessage::Payload(DfuChunk {
            addresses: TO_US,
            payload: chunk,
        });
        deliver(&mut node, 200, &from_host(&message));
        let sent = drain(&mut node, 200);
        let expected = if n < 4 {
            MessageType::DFU_PAYLOAD_REQ
        } else {
            MessageType::DFU_REBOOT_REQ
        };
        assert_eq!(bodies(&sent).last().unwrap().message_type(), expected);
    }
    assert_eq!(node.dfu().machine().state(), State::ClientRebootReq);

    deliver(&mut node, 300, &from_host(&DfuMessage::Reboot(TO_US)));
    assert!(drain(&mut node, 300).is_empty());
    assert_eq!(node.dfu().machine().state(), State::ClientActivating);

    assert_eq!(&slot.flash[..5000], &image[..]);
    assert!(slot.flash[5000..].iter().all(|b| *b == 0xFF), "erased");
    assert_eq!(slot.boot.pending_and_reset, 1);
    assert_eq!(
        slot.reboot_info,
        RebootInfo {
            magic: DFU_REBOOT_MAGIC,
            major: 2,
            minor: 3,
            host_node_id: HOST,
            git_sha: !GIT_SHA,
        }
    );
}

/// A node built on a slot whose no-init RAM holds an update resumes it: the
/// boot-complete, then the host's END confirms and clears the reboot info.
#[test]
fn a_rebooted_node_confirms_its_update() {
    let mut slot = RamDfuSlot::<SLOT>::new();
    slot.reboot_info = RebootInfo {
        magic: DFU_REBOOT_MAGIC,
        major: 2,
        minor: 3,
        host_node_id: HOST,
        git_sha: GIT_SHA,
    };
    let mut node = node(&mut slot);
    let sent = drain(&mut node, 0);
    assert_eq!(bodies(&sent), [DfuMessage::BootComplete(TO_HOST)]);
    assert_eq!(node.dfu().machine().state(), State::ClientRebootDone);

    let end = DfuMessage::End(DfuResult {
        addresses: TO_US,
        success: 1,
        err_code: 0,
    });
    deliver(&mut node, 50, &from_host(&end));
    let sent = drain(&mut node, 50);
    assert_eq!(
        bodies(&sent),
        [DfuMessage::End(DfuResult {
            addresses: TO_HOST,
            success: 1,
            err_code: 0
        })]
    );
    assert_eq!(node.dfu().machine().state(), State::Idle);
    assert_eq!(slot.boot.confirmed, 1);
    assert_eq!(slot.reboot_info, RebootInfo::default(), "stored on the way");
}

/// Unanswered chunk requests time out on the node's clock.
#[test]
fn the_chunk_timer_runs_on_the_nodes_clock() {
    let mut slot = RamDfuSlot::<SLOT>::new();
    let mut node = node(&mut slot);
    drain(&mut node, 0);
    deliver(&mut node, 0, &from_host(&offer(&image())));
    drain(&mut node, 0);
    assert!(drain(&mut node, 2_009).is_empty());
    let sent = drain(&mut node, 2_010);
    assert_eq!(
        bodies(&sent),
        [DfuMessage::PayloadReq(
            bm_wire::bcmp::dfu::DfuChunkRequest {
                addresses: TO_HOST,
                seq_num: 0
            }
        )]
    );
}

/// `dfu_copy_and_process_message`: a DFU message for another node is
/// re-flooded if it arrived link-local, and left to L2 otherwise.
#[test]
fn a_dfu_message_for_another_node_is_re_flooded_only_if_link_local() {
    let mut slot = RamDfuSlot::<SLOT>::new();
    let mut node = node(&mut slot);
    let other = DfuMessage::Heartbeat(DfuAddress {
        src_node_id: HOST,
        dst_node_id: 0x7777_BEEF_FEED_2222,
    });
    let mut frame = frame_to(BmIpAddr::LINK_LOCAL_MULTICAST, &other);
    let owed = node.on_frame(0, 1, &mut frame);
    assert!(owed.forward.is_some());
    let mut frame = frame_to(BmIpAddr::GLOBAL_MULTICAST, &other);
    let owed = node.on_frame(0, 1, &mut frame);
    assert!(owed.forward.is_none());
    assert!(owed.relay.is_some(), "L2 relays global multicast itself");
    assert_eq!(
        node.dfu().machine().core().queue().len(),
        1,
        "only InitSuccess"
    );
}

/// A node with no slot refuses an update as a C node whose
/// `flash_area_open` fails, and stays in Error until reboot.
#[test]
fn a_node_without_a_slot_nacks_with_a_flash_error() {
    let mut node: Node<TestIdentity, SoftRtc, 4> = Node::new(TestIdentity, SoftRtc::new(), PORTS);
    while node.next_dfu_transmission(0).is_some() {}
    let mut frame = from_host(&offer(&image()));
    let _ = node.on_frame(0, 1, &mut frame);
    let sent: Vec<_> =
        std::iter::from_fn(|| node.next_dfu_transmission(0).map(|o| decode(&o))).collect();
    assert_eq!(
        bodies(&sent),
        [DfuMessage::Ack(DfuResult {
            addresses: TO_HOST,
            success: 0,
            err_code: DfuErr::FLASH_ACCESS.0
        })]
    );
    assert_eq!(node.dfu().machine().state(), State::Error);
    let _: &NoDfu = node.dfu().slot();
}

/// Every frame `from` owes, handed to `to` as the wire would.
fn pass<A: Identity, B: Identity>(
    from: &mut DfuNode<'_, A>,
    src: u64,
    to: &mut DfuNode<'_, B>,
    now_ms: u32,
) -> usize {
    let frames = drain(from, now_ms);
    for message in bodies(&frames) {
        let mut frame = frame_from(src, BmIpAddr::GLOBAL_MULTICAST, &message);
        let _ = to.on_frame(now_ms, 1, &mut frame);
    }
    frames.len()
}

fn converse(host: &mut DfuNode<'_, HostIdentity>, client: &mut DfuNode<'_>, now_ms: u32) {
    for _ in 0..100 {
        let moved = pass(host, HOST, client, now_ms) + pass(client, NODE_ID, host, now_ms);
        if moved == 0 {
            return;
        }
    }
    panic!("still talking");
}

/// One node hosts an update of another from its slot: the image lands in the
/// client's slot, the client reboots into it and confirms, and the host's
/// application hears success.
#[test]
fn a_node_hosts_an_update_to_another_node() {
    let image = image();
    let mut host_slot = RamDfuSlot::<SLOT>::new();
    host_slot.flash[ImgInfo::LEN..ImgInfo::LEN + image.len()].copy_from_slice(&image);
    let mut client_slot = RamDfuSlot::<SLOT>::new();
    let mut host: DfuNode<'_, HostIdentity> = Node::with_dfu(
        HostIdentity,
        SoftRtc::new(),
        NoConfig,
        &mut host_slot,
        PORTS,
    );
    let info = ImgInfo {
        image_size: image.len() as u32,
        chunk_size: 1000,
        crc16: crc16_ccitt(0, &image),
        major_ver: 2,
        minor_ver: 3,
        filter_key: IMG_INFO_FORCE_UPDATE,
        git_sha: GIT_SHA,
    };
    {
        let mut client = node(&mut client_slot);
        drain(&mut host, 0);
        drain(&mut client, 0);
        assert!(host.dfu_initiate_update(info, NODE_ID, true, 60_000, true));
        converse(&mut host, &mut client, 0);
        assert_eq!(client.dfu().machine().state(), State::ClientActivating);
        assert_eq!(host.dfu().machine().state(), State::HostUpdate);
    }
    assert_eq!(&client_slot.flash[..image.len()], &image[..]);
    assert_eq!(client_slot.boot.pending_and_reset, 1);

    // The reboot: a new node on the same slot and no-init RAM.
    let mut client = node(&mut client_slot);
    converse(&mut host, &mut client, 100);
    assert_eq!(client.dfu().machine().state(), State::Idle);
    assert_eq!(host.dfu().machine().state(), State::Idle);
    assert_eq!(
        host.dfu_mut().take_update_finished(),
        Some(DfuFinished {
            success: true,
            err: DfuErr::NONE,
            node_id: NODE_ID,
        })
    );
    assert_eq!(host.dfu_mut().take_update_finished(), None);
    assert_eq!(client.dfu().slot().boot.confirmed, 1);
}

/// `initiate_update` outside Idle is refused and reported as
/// `BmDfuErrInProgress`, which [`Event::DfuUpdateFinished`] carries.
#[test]
fn a_refused_host_update_is_reported_as_in_progress() {
    let mut slot = RamDfuSlot::<SLOT>::new();
    let mut node = node(&mut slot);
    drain(&mut node, 0);
    let info = ImgInfo {
        image_size: 10,
        chunk_size: 10,
        ..ImgInfo::default()
    };
    assert!(node.dfu_initiate_update(info, HOST, true, 1_000, true));
    drain(&mut node, 0);
    assert_eq!(node.dfu().machine().state(), State::HostReqUpdate);
    assert!(!node.dfu_initiate_update(info, HOST, true, 1_000, true));
    let finished = node.dfu_mut().take_update_finished().expect("reported");
    assert_eq!(finished.err, DfuErr::IN_PROGRESS);
    let event = Event::DfuUpdateFinished(finished);
    assert!(matches!(event, Event::DfuUpdateFinished(f) if !f.success));
}
