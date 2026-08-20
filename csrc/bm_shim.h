#ifndef __BM_SHIM_H__
#define __BM_SHIM_H__

// Control surface for the deterministic platform shim.
//
// bm_core leaves bm_os.h / bm_ip.h / bm_configs_generic.h / bm_rtc.h /
// bm_dfu_generic.h to the integrator. This crate implements them without
// threads, sockets, or a wall clock, so a fuzz input replays byte-identically.
// This header is the part Rust drives; see README.md for the full contract.

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
