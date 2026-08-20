#ifndef __BM_SHIM_H__
#define __BM_SHIM_H__

// Control surface for the deterministic platform shim.
//
// bm_core leaves bm_os.h / bm_ip.h / bm_configs_generic.h / bm_rtc.h /
// bm_dfu_generic.h to the integrator. This crate implements them without
// threads, sockets, or a wall clock, so a fuzz input replays byte-identically.
// This header is the part Rust drives; see README.md for the full contract.

#include "network_device.h"
#include "util.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Tear down every object the shim owns and reset the virtual clock to zero.
//
// This does NOT reset bm_core's own file-scope state. Call the relevant
// module deinit functions first (bm_l2_deinit, bcmp_deinit, ...); modules
// without one are listed in README.md as a per-iteration hazard.
void bm_shim_reset(void);

// Clears only the RAM standing in for NVM, the RTC, and the DFU flash slot.
// bm_shim_reset calls this; it is exposed so a test can wipe storage without
// tearing down queues and timers.
void bm_shim_generic_reset(void);

// bm_debug output. Silent by default so fuzzing is not I/O bound.
void bm_shim_set_debug(bool on);
void bm_shim_debug_printf(const char *format, ...);

// --- bringing the stack up ---

// Initialise L2, the IP layer, BCMP, topology, services, pubsub and
// middleware against bm_shim_network_device(), in the order
// middleware/bristlemouth.c uses. Call device_init first: the IP layer
// derives this node's addresses from node_id().
//
// Only bm_l2_deinit exists upstream, so the modules this brings up cannot be
// torn down; see README.md on running one stack per process.
BmErr bm_shim_stack_init(void);

// --- the wire boundary ---

// A NetworkDevice whose PHY is a capture buffer. Pass it to bm_l2_init (or
// bristlemouth_init); everything bm_core transmits then lands in the ring
// bm_shim_tx_pop drains, and bm_shim_rx_inject pushes bytes back up the path
// the ADIN2111 driver would.
NetworkDevice bm_shim_network_device(void);

// Deliver a frame to L2 as if it arrived on `port` (1-based). The frame is
// copied first: L2 rewrites the ingress/egress bytes in place.
BmErr bm_shim_rx_inject(uint8_t port, const uint8_t *data, uint32_t len);

// Drive the link-change callback L2 registered, so a test can bring a port up
// without waiting for renegotiation.
void bm_shim_link_change(uint8_t port, bool up);

// Frames captured but not yet drained, and frames the ring had no room for.
// A non-zero drop count means a test is missing transmissions.
uint32_t bm_shim_tx_count(void);
uint32_t bm_shim_tx_dropped(void);

// Copy the oldest captured frame into `buf` and remove it. Returns the frame's
// full length -- which may exceed `buf_len`, meaning the copy was truncated --
// or -1 when nothing is captured. `port` receives the egress port if non-NULL.
int32_t bm_shim_tx_pop(uint8_t *buf, uint32_t buf_len, uint8_t *port);

// Drop captured frames and forget the registered callbacks. bm_shim_reset
// calls this.
void bm_shim_network_device_reset(void);

// --- virtual clock (1 tick == 1 ms, matching bm_posix.c) ---

uint32_t bm_shim_tick_count(void);

// Advance the clock, firing every timer that comes due along the way.
// Auto-reload timers may fire more than once in a single call.
void bm_shim_advance_ticks(uint32_t ticks);

// --- task scheduling ---

// Run each registered task until it goes idle.
//
// bm_core's task bodies are `while (true)` loops that block on a queue and
// never return, so the shim escapes them with longjmp: a task that reaches an
// empty queue has nothing left to do, and control returns here. A task is
// entered from the top on every pump, so any setup preceding its loop reruns.
//
// Returns the number of tasks run.
uint32_t bm_shim_pump(void);

// Backstop against a task whose loop never touches a queue: the pump abandons
// a task after this many blocking calls. Raise it if a legitimate burst of
// queued work is being cut short.
void bm_shim_set_pump_budget(uint32_t calls);

// Number of tasks bm_core has registered via bm_task_create.
uint32_t bm_shim_task_count(void);

#ifdef __cplusplus
}
#endif

#endif // __BM_SHIM_H__
