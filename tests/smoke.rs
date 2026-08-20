//! Smoke tests: one per source tier.
//!
//! The point is not coverage — bm_core has its own gtest suite for that — but
//! to prove each tier links and runs. A module that compiles but pulls an
//! unresolved symbol from an excluded backend fails here rather than later
//! inside a fuzz target.
//!
//! Expected values are lifted from vendor/bm_core/test/src/*_test.cpp wherever
//! bm_core already asserts them, so the two suites cannot silently diverge.

use bm_wire_sys::*;

// --- T0: pure functions, no shim required ---

#[test]
fn crc_matches_reference_vectors() {
    let check = b"123456789";
    unsafe {
        // Seed 0x0000, no output XOR => CRC-16/KERMIT, per crc.h's doc comment.
        assert_eq!(crc16_ccitt(0, check.as_ptr(), check.len()), 0x2189);
        assert_eq!(crc32_ieee(check.as_ptr(), check.len()), 0xCBF4_3926);
        // Updating across a split must equal the one-shot value.
        let split = crc32_ieee_update(0, check[..4].as_ptr(), 4);
        assert_eq!(
            crc32_ieee_update(split, check[4..].as_ptr(), 5),
            0xCBF4_3926
        );
    }
}

#[test]
fn utc_date_time_round_trips() {
    // Values from vendor/bm_core/test/src/utc_from_date_time_test.cpp.
    unsafe {
        assert_eq!(utc_from_date_time(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(utc_from_date_time(2020, 2, 4, 9, 2, 3), 1_580_806_923);

        let mut dt = UtcDateTime::default();
        date_time_from_utc(1_580_806_923 * 1_000_000, &mut dt);
        assert_eq!(
            (dt.year, dt.month, dt.day, dt.hour, dt.min, dt.sec, dt.usec),
            (2020, 2, 4, 9, 2, 3, 0)
        );
    }
}

#[test]
fn wildcard_match_handles_stars_and_question_marks() {
    // Values from vendor/bm_core/test/src/util_test.cpp.
    let m = |s: &str, p: &str| unsafe {
        bm_wildcard_match(
            s.as_ptr().cast(),
            s.len() as u16,
            p.as_ptr().cast(),
            p.len() as u16,
        )
    };
    assert!(m("aaaa", "a*a"));
    assert!(m("aaabxc_file.txt", "*a*b?c*.txt"));
    assert!(!m("alpha_betaXc123.txt", "*a*b?c*.txt"));
    assert!(m("alpha_betaXc123.txt", "*a*b*c*.txt"));
    assert!(m("report-1925-diary", "report-????-*y"));
    assert!(!m("report-2023-Xbad", "report-????-*y"));
}

#[test]
fn time_remaining_wraps_like_a_tick_counter() {
    unsafe {
        assert_eq!(time_remaining(0, 40, 100), 60);
        assert_eq!(time_remaining(0, 100, 100), 0);
        // Elapsed past the timeout saturates at zero rather than underflowing.
        assert_eq!(time_remaining(0, 250, 100), 0);
    }
}

#[test]
fn static_inline_helpers_are_callable() {
    // These are `static inline` in util.h and only exist because build.rs asks
    // bindgen to emit out-of-line copies. ip_to_nodeid is endianness code, so
    // it is exactly the kind of thing a Rust port can get subtly wrong.
    let mut addr = BmIpAddr::default();
    addr.addr[8..].copy_from_slice(&0xDEAD_BEEF_1234_5678u64.to_be_bytes());
    unsafe {
        assert_eq!(ip_to_nodeid(&addr), 0xDEAD_BEEF_1234_5678);

        let mut bytes = [0x12u8, 0x34, 0x56, 0x78];
        assert_eq!(uint8_to_uint16(bytes.as_mut_ptr()), 0x1234);
        assert_eq!(uint8_to_uint32(bytes.as_mut_ptr()), 0x1234_5678);
    }
}

#[test]
fn multicast_classification_reads_the_address_prefix() {
    // Bristlemouth uses FF03::1 for global and FF02::x for link-local; see
    // multicast_global_addr / multicast_ll_addr in vendor/bm_core/common/util.c.
    let mut addr = BmIpAddr::default();
    addr.addr[0] = 0xFF;
    addr.addr[1] = 0x03;
    addr.addr[15] = 0x01;
    unsafe {
        assert!(is_global_multicast(&addr));
        assert!(!is_link_local_multicast(&addr));
    }

    addr.addr[1] = 0x02;
    unsafe {
        assert!(is_link_local_multicast(&addr));
        assert!(is_link_local_neighbor_multicast(&addr));
        assert!(!is_global_multicast(&addr));
    }

    // FF02::2 is link-local multicast but not the neighbor address.
    addr.addr[15] = 0x02;
    unsafe {
        assert!(is_link_local_multicast(&addr));
        assert!(!is_link_local_neighbor_multicast(&addr));
    }
}

#[test]
fn device_reports_back_what_it_was_initialised_with() {
    let cfg = DeviceCfg {
        node_id: 0xDEAD_BEEF_1234_5678,
        vendor_id: 0x1234,
        product_id: 0x5678,
        hw_ver: 3,
        ver_major: 1,
        ver_minor: 2,
        ver_patch: 4,
        ..Default::default()
    };
    unsafe {
        assert_eq!(device_init(cfg), BmErr_BmOK);
        assert_eq!(node_id(), 0xDEAD_BEEF_1234_5678);
        assert_eq!(vendor_id(), 0x1234);
        assert_eq!(hardware_revision(), 3);

        let (mut maj, mut min, mut patch) = (0u8, 0u8, 0u8);
        assert_eq!(firmware_version(&mut maj, &mut min, &mut patch), BmErr_BmOK);
        assert_eq!((maj, min, patch), (1, 2, 4));
    }
}

// --- T1: needs the bm_os shim ---
//
// Everything below touches process-global C state (the shim's own tables, and
// bm_core's file-scope statics), so it runs under one lock rather than in
// parallel across the test harness's threads.

use std::sync::{Mutex, MutexGuard};

static SHIM: Mutex<()> = Mutex::new(());

/// Take the shim lock and hand back a clean shim.
fn shim() -> MutexGuard<'static, ()> {
    let guard = SHIM.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe { bm_shim_reset() };
    guard
}

#[test]
fn q_is_a_byte_fifo() {
    let _guard = shim();
    unsafe {
        let q = q_create(64);
        assert!(!q.is_null());

        let first = *b"hello";
        let second = *b"bye";
        assert_eq!(q_enqueue(q, first.as_ptr().cast(), 5), BmErr_BmOK);
        assert_eq!(q_enqueue(q, second.as_ptr().cast(), 3), BmErr_BmOK);

        let mut out = [0u8; 5];
        assert_eq!(q_dequeue(q, out.as_mut_ptr().cast(), 5), BmErr_BmOK);
        assert_eq!(&out, b"hello");

        let mut out = [0u8; 3];
        assert_eq!(q_dequeue(q, out.as_mut_ptr().cast(), 3), BmErr_BmOK);
        assert_eq!(&out, b"bye");

        assert_eq!(q_size(q), 0);
        assert_eq!(q_delete(q), BmErr_BmOK);
    }
}

#[test]
fn ll_stores_and_removes_by_id() {
    let _guard = shim();
    let mut list = LL::default();
    let mut payload: u32 = 0xABCD_1234;
    unsafe {
        let item = ll_create_item(
            std::ptr::null_mut(),
            (&raw mut payload).cast(),
            size_of::<u32>() as u32,
            7,
        );
        assert!(!item.is_null());
        assert_eq!(ll_item_add(&mut list, item), BmErr_BmOK);

        let mut fetched: *mut std::ffi::c_void = std::ptr::null_mut();
        assert_eq!(ll_get_item(&mut list, 7, &mut fetched), BmErr_BmOK);
        assert_eq!(*fetched.cast::<u32>(), 0xABCD_1234);

        // An id that was never added must not be reported as present.
        assert_ne!(ll_get_item(&mut list, 8, &mut fetched), BmErr_BmOK);

        assert_eq!(ll_remove(&mut list, 7), BmErr_BmOK);
        assert_ne!(ll_get_item(&mut list, 7, &mut fetched), BmErr_BmOK);
    }
}

/// The payload shape `packet.c` is told to expect via its accessor callbacks.
/// Mirrors PacketTestData in vendor/bm_core/test/src/packet_test.cpp.
#[repr(C)]
struct TestPayload {
    buf: *mut u8,
    src: [u8; 16],
    dst: [u8; 16],
}

unsafe extern "C" fn get_data(payload: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    unsafe { (*payload.cast::<TestPayload>()).buf.cast() }
}
unsafe extern "C" fn get_src(payload: *mut std::ffi::c_void) -> *mut BmIpAddr {
    unsafe { (&raw mut (*payload.cast::<TestPayload>()).src).cast() }
}
unsafe extern "C" fn get_dst(payload: *mut std::ffi::c_void) -> *mut BmIpAddr {
    unsafe { (&raw mut (*payload.cast::<TestPayload>()).dst).cast() }
}
unsafe extern "C" fn zero_checksum(_payload: *mut std::ffi::c_void, _size: u32) -> u16 {
    0
}

static HEARTBEATS_SEEN: Mutex<Vec<BcmpHeartbeat>> = Mutex::new(Vec::new());

unsafe extern "C" fn on_heartbeat(data: BcmpProcessData) -> BmErr {
    let hb = unsafe { std::ptr::read_unaligned(data.payload.cast::<BcmpHeartbeat>()) };
    HEARTBEATS_SEEN.lock().unwrap().push(hb);
    BmErr_BmOK
}

#[test]
fn packet_serializes_and_dispatches_a_heartbeat() {
    let _guard = shim();
    let mut buf = vec![0u8; 512];
    let mut payload = TestPayload {
        buf: buf.as_mut_ptr(),
        src: [0x11; 16],
        dst: [0x22; 16],
    };
    let hb = BcmpHeartbeat {
        time_since_boot_us: 0x0123_4567_89AB_CDEF,
        liveliness_lease_dur_s: 60,
    };

    unsafe {
        assert_eq!(
            packet_init(Some(get_src), Some(get_dst), Some(get_data), Some(zero_checksum)),
            BmErr_BmOK
        );
        let mut cfg = BcmpPacketCfg {
            sequenced_reply: false,
            sequenced_request: false,
            process: Some(on_heartbeat),
        };
        assert_eq!(
            packet_add(&mut cfg, BcmpMessageType_BcmpHeartbeatMessage),
            BmErr_BmOK
        );

        // Serialize writes a BcmpHeader followed by the message body.
        assert_eq!(
            serialize(
                (&raw mut payload).cast(),
                (&raw const hb as *mut BcmpHeartbeat).cast(),
                size_of::<BcmpHeartbeat>() as u32,
                BcmpMessageType_BcmpHeartbeatMessage,
                0,
                None,
            ),
            BmErr_BmOK
        );
        let header = std::ptr::read_unaligned(buf.as_ptr().cast::<BcmpHeader>());
        // BcmpHeader is __attribute__((packed)); copy the field out first.
        let message_type = header.type_;
        assert_eq!(message_type, BcmpMessageType_BcmpHeartbeatMessage as u16);

        // Feeding the same bytes back must reach the registered handler.
        HEARTBEATS_SEEN.lock().unwrap().clear();
        assert_eq!(
            process_received_message(
                (&raw mut payload).cast(),
                (size_of::<BcmpHeader>() + size_of::<BcmpHeartbeat>()) as u32,
            ),
            BmErr_BmOK
        );
        assert_eq!(
            HEARTBEATS_SEEN.lock().unwrap().as_slice(),
            &[hb],
            "the heartbeat must survive the serialize/process round trip"
        );

        assert_eq!(
            packet_remove(BcmpMessageType_BcmpHeartbeatMessage),
            BmErr_BmOK
        );
    }
}

static TIMER_FIRES: Mutex<u32> = Mutex::new(0);

unsafe extern "C" fn count_fire(_timer: BmTimer) {
    *TIMER_FIRES.lock().unwrap() += 1;
}

#[test]
fn virtual_clock_fires_timers_without_sleeping() {
    let _guard = shim();
    *TIMER_FIRES.lock().unwrap() = 0;
    unsafe {
        let one_shot = bm_timer_create(
            c"one_shot".as_ptr(),
            100,
            false,
            std::ptr::null_mut(),
            Some(count_fire),
        );
        let repeating = bm_timer_create(
            c"repeating".as_ptr(),
            50,
            true,
            std::ptr::null_mut(),
            Some(count_fire),
        );
        assert_eq!(bm_timer_start(one_shot, 0), BmErr_BmOK);
        assert_eq!(bm_timer_start(repeating, 0), BmErr_BmOK);

        bm_shim_advance_ticks(49);
        assert_eq!(*TIMER_FIRES.lock().unwrap(), 0);
        assert_eq!(bm_shim_tick_count(), 49);

        bm_shim_advance_ticks(1); // t=50: the repeating timer only
        assert_eq!(*TIMER_FIRES.lock().unwrap(), 1);

        bm_shim_advance_ticks(50); // t=100: repeating again, plus the one-shot
        assert_eq!(*TIMER_FIRES.lock().unwrap(), 3);

        // The one-shot must not come back.
        bm_shim_advance_ticks(50); // t=150: repeating only
        assert_eq!(*TIMER_FIRES.lock().unwrap(), 4);
        assert_ne!(bm_timer_is_timer_active(one_shot), BmErr_BmOK);
    }
}

static CALLBACKS_RUN: Mutex<u32> = Mutex::new(0);

unsafe extern "C" fn count_callback(_arg: *mut std::ffi::c_void) {
    *CALLBACKS_RUN.lock().unwrap() += 1;
}

#[test]
fn pump_drains_a_task_that_never_returns() {
    let _guard = shim();
    *CALLBACKS_RUN.lock().unwrap() = 0;
    unsafe {
        // timer_callback_handler registers a `while (true)` task that blocks on
        // a queue. Pumping it must run the queued work and then come back --
        // this is the whole reason the shim escapes task loops with longjmp.
        assert_eq!(timer_callback_handler_init(), BmErr_BmOK);
        assert_eq!(bm_shim_task_count(), 1);

        assert!(timer_callback_handler_send_cb(
            Some(count_callback),
            std::ptr::null_mut(),
            10
        ));
        assert!(timer_callback_handler_send_cb(
            Some(count_callback),
            std::ptr::null_mut(),
            10
        ));

        assert_eq!(bm_shim_pump(), 1);
        assert_eq!(*CALLBACKS_RUN.lock().unwrap(), 2);

        // Pumping an idle task is a no-op rather than a hang.
        assert_eq!(bm_shim_pump(), 1);
        assert_eq!(*CALLBACKS_RUN.lock().unwrap(), 2);
    }
}

// --- T2: needs tinycbor ---

#[test]
fn config_round_trips_through_the_ram_partition() {
    let _guard = shim();
    unsafe {
        config_init();

        let key = c"sample_rate";
        let key_len = key.count_bytes();
        assert!(set_config_uint(
            BmConfigPartition_BM_CFG_PARTITION_USER,
            key.as_ptr(),
            key_len,
            48_000
        ));

        let mut value = 0u32;
        assert!(get_config_uint(
            BmConfigPartition_BM_CFG_PARTITION_USER,
            key.as_ptr(),
            key_len,
            &mut value
        ));
        assert_eq!(value, 48_000);

        // A string key alongside it, to prove the key table indexes correctly.
        let name = c"node_name";
        let text = c"dev_kit";
        assert!(set_config_string(
            BmConfigPartition_BM_CFG_PARTITION_USER,
            name.as_ptr(),
            name.count_bytes(),
            text.as_ptr(),
            text.count_bytes()
        ));

        let mut out = [0u8; 32];
        let mut out_len = out.len();
        assert!(get_config_string(
            BmConfigPartition_BM_CFG_PARTITION_USER,
            name.as_ptr(),
            name.count_bytes(),
            out.as_mut_ptr().cast(),
            &mut out_len
        ));
        assert_eq!(&out[..out_len], b"dev_kit");

        assert!(remove_key(
            BmConfigPartition_BM_CFG_PARTITION_USER,
            key.as_ptr(),
            key_len
        ));
        assert!(!get_config_uint(
            BmConfigPartition_BM_CFG_PARTITION_USER,
            key.as_ptr(),
            key_len,
            &mut value
        ));
    }
}

#[test]
fn sys_info_reply_survives_a_cbor_round_trip() {
    let _guard = shim();
    let app_name = c"bm_wire_sys";
    let mut sent = SysInfoReplyData {
        node_id: 0x0123_4567_89AB_CDEF,
        git_sha: 0xDEAD_BEEF,
        sys_config_crc: 0x1234_5678,
        app_name_strlen: app_name.count_bytes() as u32,
        app_name: app_name.as_ptr().cast_mut(),
    };

    let mut buf = [0u8; 256];
    let mut encoded_len = 0usize;
    unsafe {
        assert_eq!(
            sys_info_reply_encode(
                &mut sent,
                buf.as_mut_ptr(),
                buf.len(),
                &mut encoded_len
            ),
            CborError_CborNoError
        );
        assert!(encoded_len > 0 && encoded_len <= buf.len());

        let mut received = SysInfoReplyData::default();
        assert_eq!(
            sys_info_reply_decode(&mut received, buf.as_ptr(), encoded_len),
            CborError_CborNoError
        );
        assert_eq!(received.node_id, sent.node_id);
        assert_eq!(received.git_sha, sent.git_sha);
        assert_eq!(received.sys_config_crc, sent.sys_config_crc);
        assert_eq!(received.app_name_strlen, sent.app_name_strlen);

        // The decoder allocates the string via bm_malloc.
        let decoded_name = std::slice::from_raw_parts(
            received.app_name.cast::<u8>(),
            received.app_name_strlen as usize,
        );
        assert_eq!(decoded_name, app_name.to_bytes());
        bm_free(received.app_name.cast());

        // Truncated input must be rejected rather than read past the end.
        let mut truncated = SysInfoReplyData::default();
        assert_ne!(
            sys_info_reply_decode(&mut truncated, buf.as_ptr(), encoded_len / 2),
            CborError_CborNoError
        );
    }
}

// --- T3: the wire path ---

const ETHERTYPE_IPV6: u16 = 0x86DD;
const IPV6_NEXT_HEADER_OFFSET: usize = 20;
const IP_PROTO_BCMP: u8 = 0xBC;

/// Bring the whole stack up on the capture device, with the given node id.
///
/// bm_core exposes no deinit but bm_l2_deinit, so the modules brought up here
/// keep their file-scope state for the life of the process; see README.md.
/// Only one test may call this.
fn init_stack(node_id: u64) {
    let cfg = DeviceCfg {
        node_id,
        device_name: c"shim".as_ptr(),
        version_string: c"0.0.0".as_ptr(),
        ..Default::default()
    };
    unsafe {
        assert_eq!(device_init(cfg), BmErr_BmOK);
        assert_eq!(bm_shim_stack_init(), BmErr_BmOK, "stack init");
    }
}

/// Drain one captured frame, or None.
fn tx_pop() -> Option<(u8, Vec<u8>)> {
    let mut buf = vec![0u8; 2048];
    let mut port = 0u8;
    let len = unsafe { bm_shim_tx_pop(buf.as_mut_ptr(), buf.len() as u32, &mut port) };
    if len < 0 {
        return None;
    }
    let len = len as usize;
    assert!(len <= buf.len(), "captured frame truncated by the test buffer");
    buf.truncate(len);
    Some((port, buf))
}

#[test]
fn stack_brings_up_and_transmits_a_bcmp_heartbeat() {
    let _guard = shim();
    init_stack(0xC0FF_EE00_1234_5678);

    unsafe {
        // The link must come up before BCMP will say anything.
        bm_shim_link_change(1, true);
        bm_shim_pump();

        assert_eq!(bcmp_send_heartbeat(10), BmErr_BmOK);
        bm_shim_pump();
        assert_eq!(bm_shim_tx_dropped(), 0, "capture ring overflowed");
    }

    let (_port, frame) = tx_pop().expect("the heartbeat should have reached the device");

    // The device sees a full Ethernet frame carrying IPv6 with BCMP inside.
    assert!(frame.len() > IPV6_NEXT_HEADER_OFFSET);
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    assert_eq!(ethertype, ETHERTYPE_IPV6, "frame is not IPv6 over Ethernet");
    assert_eq!(
        frame[IPV6_NEXT_HEADER_OFFSET], IP_PROTO_BCMP,
        "IPv6 next header is not BCMP"
    );

    // L2 is the one module with a deinit, so at least its queue and task do
    // not outlive this test holding pointers the next bm_shim_reset frees.
    unsafe { bm_l2_deinit() };
}

#[test]
fn rx_inject_of_garbage_is_survivable() {
    let _guard_ordering = SHIM.lock().unwrap_or_else(|p| p.into_inner());
    // Deliberately no bm_shim_reset here: this test rides on whatever stack
    // state exists, which is the same position a fuzz iteration is in.

    // A frame too short to hold an Ethernet header, one that is all zeroes,
    // and one claiming to be BCMP with a nonsense body. None may crash, and
    // afterwards the stack must still be usable.
    let cases: [&[u8]; 3] = [&[0x01, 0x02], &[0u8; 64], &{
        let mut frame = [0u8; 128];
        frame[12] = 0x86;
        frame[13] = 0xDD;
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[54] = 0xFF; // a message type nothing handles
        frame[55] = 0xFF;
        frame
    }];

    unsafe {
        for case in cases {
            // BmENODEV just means L2 was never initialised, which is fine:
            // the point is that none of these faults.
            let _ = bm_shim_rx_inject(1, case.as_ptr(), case.len() as u32);
            bm_shim_pump();
        }
    }
}
