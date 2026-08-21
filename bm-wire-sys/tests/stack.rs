//! The wire path, end to end.
//!
//! Separate from tests/smoke.rs because cargo runs each integration test file
//! as its own process, and the stack can only be brought up once per process:
//! bm_l2_deinit is bm_core's only teardown function, so every other module
//! keeps its file-scope state for good. A bm_shim_reset after this point frees
//! shim queues that those statics still point at, which is a use-after-free
//! waiting for something to pump a stale task. Same reason a fuzz target has
//! to fork per iteration; see README.md.

use bm_wire_sys::*;

const ETHERTYPE_IPV6: u16 = 0x86DD;
const IPV6_NEXT_HEADER_OFFSET: usize = 20;
const IP_PROTO_BCMP: u8 = 0xBC;

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

/// One test, run in phases, because there is only one stack to test with and
/// the harness would otherwise thread these against each other.
#[test]
fn the_stack_comes_up_transmits_and_survives_garbage() {
    let cfg = DeviceCfg {
        node_id: 0xC0FF_EE00_1234_5678,
        device_name: c"shim".as_ptr(),
        version_string: c"0.0.0".as_ptr(),
        ..Default::default()
    };
    unsafe {
        bm_shim_reset();
        assert_eq!(device_init(cfg), BmErr_BmOK);
        assert_eq!(bm_shim_stack_init(), BmErr_BmOK, "stack init");
    }

    transmits_a_bcmp_heartbeat();
    survives_injected_garbage();
}

fn transmits_a_bcmp_heartbeat() {
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
}

fn survives_injected_garbage() {
    // A frame too short to hold an Ethernet header, one that is all zeroes,
    // and one claiming to be BCMP with a message type nothing handles. None
    // may fault, and the stack must still transmit afterwards.
    let cases: [&[u8]; 3] = [&[0x01, 0x02], &[0u8; 64], &{
        let mut frame = [0u8; 128];
        frame[12] = 0x86;
        frame[13] = 0xDD;
        frame[IPV6_NEXT_HEADER_OFFSET] = IP_PROTO_BCMP;
        frame[54] = 0xFF;
        frame[55] = 0xFF;
        frame
    }];

    unsafe {
        for case in cases {
            assert_eq!(
                bm_shim_rx_inject(1, case.as_ptr(), case.len() as u32),
                BmErr_BmOK,
                "injection itself should reach L2"
            );
            bm_shim_pump();
        }

        // Still alive: a heartbeat must still make it out to the device.
        while tx_pop().is_some() {}
        assert_eq!(bcmp_send_heartbeat(10), BmErr_BmOK);
        bm_shim_pump();
    }
    assert!(
        tx_pop().is_some(),
        "the stack stopped transmitting after being fed garbage"
    );
}
