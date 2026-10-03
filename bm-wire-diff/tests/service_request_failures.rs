//! `bm_service_request`'s failure paths, measured on the oracle alone
//! (divergence #91). The Rust node's ceilings refuse these names and sizes
//! before an id is taken, so `tests/services.rs` cannot mirror them.
//!
//! Its own binary for the reason `bm_wire_diff::stack` gives, and because
//! each failure here takes an id the services comparator's mirror would not.

use std::sync::Mutex;

use bm_wire_diff::pubsub::oracle_subscriptions;
use bm_wire_diff::stack::{
    drain, oracle, pump_until_quiet, start_timer_callback_handler, tick_count,
};

/// `(ack, id, service)` per call.
static ANSWERS: Mutex<Vec<(bool, u32, Vec<u8>)>> = Mutex::new(Vec::new());

unsafe extern "C" fn reply_cb(
    ack: bool,
    msg_id: u32,
    service_strlen: usize,
    service: *const core::ffi::c_char,
    _reply_len: usize,
    _reply_data: *mut u8,
) -> bool {
    let service = unsafe { std::slice::from_raw_parts(service.cast::<u8>(), service_strlen) };
    ANSWERS
        .lock()
        .unwrap()
        .push((ack, msg_id, service.to_vec()));
    true
}

fn request(service: &[u8], data: &[u8], timeout_s: u32) -> bool {
    unsafe {
        bm_wire_sys::bm_service_request(
            service.len(),
            service.as_ptr().cast(),
            data.len(),
            data.as_ptr(),
            Some(reply_cb),
            timeout_s,
        )
    }
}

/// Advance to the next 500 ms sweep and run it; return what it reported.
fn sweep() -> Vec<(bool, u32, Vec<u8>)> {
    let wait = 500 - tick_count() % 500;
    unsafe { bm_wire_sys::bm_shim_advance_ticks(wait) };
    pump_until_quiet();
    drain();
    std::mem::take(&mut *ANSWERS.lock().unwrap())
}

/// One test, so the ids are in a known order.
#[test]
fn failed_requests_stay_listed_and_time_out() {
    let _guard = oracle();
    start_timer_callback_handler();
    assert_eq!(tick_count(), 0, "the sweep is phased from bring-up");

    // Too large: refused before an id is taken.
    assert!(!request(b"big", &[0; 1025], 0));
    assert!(drain().is_empty());

    // A reply topic of 255 bytes: bm_sub_wl refuses it after the request is
    // listed as id 0. Nothing is sent, nothing subscribed.
    let long = [b'n'; 251];
    let subscriptions = oracle_subscriptions();
    assert!(!request(&long, b"", 0));
    pump_until_quiet();
    assert!(drain().is_empty(), "nothing sent");
    assert_eq!(oracle_subscriptions(), subscriptions, "nothing subscribed");

    // A failed send: bm_pub_wl's allocation is refused after the request is
    // listed as id 1 and `svc/rep` subscribed.
    unsafe { bm_wire_sys::bm_shim_heap_watch_begin(512) };
    let sent = request(b"svc", &[0; 600], 0);
    let watch = unsafe { bm_wire_sys::bm_shim_heap_watch_end() };
    assert!(!sent);
    assert!(watch.refused > 0, "{watch:?}");
    pump_until_quiet();
    assert!(drain().is_empty(), "nothing sent");
    assert!(oracle_subscriptions().contains(&b"svc/rep".to_vec()));

    // Both time out at the first sweep, as requests that were sent do.
    assert!(request(b"svc", b"", 0), "id 2");
    assert_eq!(
        sweep(),
        [
            (false, 0, long.to_vec()),
            (false, 1, b"svc".to_vec()),
            (false, 2, b"svc".to_vec()),
        ]
    );
}
