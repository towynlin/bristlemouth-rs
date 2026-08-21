#ifndef __BM_CONFIG_H__
#define __BM_CONFIG_H__

// Integrator-supplied configuration for the bm-wire-sys host build.
//
// bm_core declares these as macros and leaves them to the integrator. The
// values here mirror vendor/bm_core/test/mocks/bm_config.h, which is the
// configuration bm_core's own gtest suite uses on a hosted target.

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

#define bm_app_name "bm_wire_sys"

// Routed through the shim so fuzzing is not I/O bound. Silent unless the
// Rust side calls bm_shim_set_debug(true).
void bm_shim_debug_printf(const char *format, ...);
#define bm_debug(format, ...) bm_shim_debug_printf(format, ##__VA_ARGS__)

// Empty on a hosted build: the real value is `section(".noinit")`, which the
// host toolchain rejects for objects with static storage duration. bm_core
// compiles with -Werror=attributes to catch an integrator who forgets to
// define this at all, so it must be defined, just not to an ELF section.
#define bm_noinit_ram_attribute

#ifndef bm_metrics_enabled
#define bm_metrics_enabled 1
#endif

#endif // __BM_CONFIG_H__
