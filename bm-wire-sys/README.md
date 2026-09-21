# bm-wire-sys

Raw FFI bindings to [bm_core](https://github.com/bristlemouth/bm_core)'s C,
built as a differential-fuzzing oracle for the Rust port. It compiles the same
code running on deployed Bristlemouth nodes, so the port can be proven
bit-compatible.

Host-only. `build.rs` panics if it is pulled into a `target_os = "none"` build.

```
cargo test                  # build everything and run the smoke tests
./scripts/check_symbols.sh  # confirm nothing but libc is left unresolved
```

## The platform shim

bm_core is a library the integrator completes. `bm_os.h`, `bm_ip.h`,
`bm_configs_generic.h`, `bm_rtc.h`, `bm_dfu_generic.h` and the `bm_config.h`
macros are declared but not defined; firmware supplies FreeRTOS and lwIP, and
`bm_sbc` supplies POSIX threads and raw sockets. `csrc/` is a third backend,
and its behaviour differs from an RTOS deliberately:

- **No threads.** `bm_task_create` records the entry point and returns without
  running it; `bm_start_scheduler` is a no-op. Rust drives execution with
  `bm_shim_pump()`.
- **Tasks are escaped with `longjmp`.** bm_core's task bodies are `while (true)`
  loops that block on a queue and never return, so `bm_shim_pump` enters one
  with `setjmp` and the blocking primitives jump back out once the task is idle.
  bm_core calls those primitives as the first statement of each iteration, so
  nothing is live across the jump. A task whose loop never touches a queue is
  abandoned after `bm_shim_set_pump_budget` blocking calls.
- **No blocking.** `bm_queue_receive`, `bm_semaphore_take` and
  `bm_stream_buffer_receive` ignore their timeout and return `BmETIMEDOUT`
  immediately. Queues are real bounded FIFOs, so ordering is preserved; a send
  to a full queue fails rather than waiting.
- **Virtual clock.** 1 tick == 1 ms, as in `bm_posix.c`, but the counter moves
  only when `bm_shim_advance_ticks` or `bm_delay` says so, firing due timers as
  it goes. Nothing reads the wall clock, so an input replays identically.
- **The PHY is a capture ring.** `bm_shim_network_device()` implements
  `NetworkDeviceTrait` by copying every transmitted frame where
  `bm_shim_tx_pop` can drain it; `bm_shim_rx_inject` pushes bytes up the path
  the ADIN2111 driver would. That is the wire boundary and the fuzz entry point.
- **NVM, RTC and DFU flash are RAM.** Plain buffers, cleared by
  `bm_shim_generic_reset`.
- **Leak-visible.** Every allocation is plain `malloc`, so ASan and
  LeakSanitizer see the whole graph.

`csrc/bm_stack_shim.c` brings the stack up on the capture device in the order
`middleware/bristlemouth.c` uses.

### State that outlives `bm_shim_reset`

`bm_shim_reset()` tears down everything the shim owns. It cannot touch
bm_core's file-scope state, and **`bm_l2_deinit` is the only teardown function
bm_core exposes** — `bcmp.c`, `packet.c`, `pubsub.c`, `neighbors.c`,
`topology.c`, `configuration.c`, `resource_discovery.c`, `bm_service.c` and the
DFU modules keep their statics for the life of the process.

So:

- Resetting the shim while those modules are up leaves them holding pointers to
  freed queues and timers.
- A fuzz target that brings the stack up must run **one stack per process**
  (`cargo fuzz`'s default fork mode is fine), or restrict itself to what can be
  re-initialised: `packet.c` via `packet_remove`, and anything in T0–T2.

The wire-path tests originally shared a process with the rest of the suite and
segfaulted intermittently; they now live in `tests/stack.rs`, which cargo runs
as its own process.

Adding deinit functions upstream would remove this constraint.

## Coverage

Sources are grouped in `build.rs` by what they need from the shim, so a link
failure points at a tier instead of a wall of errors.

| Tier | Needs | Modules |
|---|---|---|
| T0 | nothing | `crc16/32`, `util`, `lib_state_machine`, `device`, `l2_policy` |
| T1 | the `bm_os` shim | `aligned_malloc`, `ll`, `q`, `pcap`, `cb_queue`, `timer_callback_handler`, `bcmp/packet` |
| T2 | tinycbor | `configuration`, `cbor_service_helper`, and the C half of `bm_common_messages` |
| T3 | `bm_ip` + a NetworkDevice | `bm_linux`, `l2`, all of `bcmp/`, `middleware/`, `integrations/` |
| T4 | the DFU flash shim | `dfu_core`, `dfu_client`, `dfu_host`, `bm_mavlink` |

`network/bm_linux.c` serves as the `bm_ip.h` backend rather than a hand-written
stub. Despite the name it opens no sockets and starts no threads: it is a pure
software IPv6 stack that builds and parses Ethernet/IPv6/UDP frames in malloc'd
buffers and hands them to L2, so the port is compared against bm_core's real
framing.

Deliberately excluded:

| Excluded | Why |
|---|---|
| `drivers/adin2111/*` | SPI hardware driver; no wire-format content |
| `network/bm_lwip.c`, `common/bm_freertos.c`, `common/bm_posix.c` | alternative backends for the interfaces `csrc/` implements |
| `middleware/bristlemouth.c` | calls `adin2111_network_device()` directly; `csrc/bm_stack_shim.c` replaces it |
| `bm_common_messages/*.cpp` | namespaced C++ with reference parameters; needs `extern "C"` thunks. `sensor_header_msg` ships as both `.c` and `.cpp`; the `.c` is bound |

## Bindings

One flat `bindings.rs`, allowlisted **by file** rather than by symbol, so
coverage tracks the submodule as bm_core moves upstream.

Two dozen bm_core headers have no include guard, which makes parsing them in
one translation unit a redefinition error. `build.rs` shadows the header tree
into `OUT_DIR` with guards added and points bindgen at the copies. A stub
forwarding with `#include_next` is not enough: a quoted include searches the
including file's own directory first, so a sibling header reaches the unguarded
original. The C build still compiles against `vendor/`, so debug info points at
real sources. Adding guards upstream would retire all of this.

`util.h`'s `static inline` helpers (`ip_to_nodeid`, `uint8_to_uint16/32`) are
emitted as callable out-of-line copies via bindgen's `wrap_static_fns`;
`build.rs` compiles the generated `static_fns.c` as a second archive.

A header can declare a function whose `.c` file is excluded — the `.cpp`
message types, for instance. Those come out as `pub fn` that fail at link time
if called.

## Compiler flags

`build.rs` compiles everything at **`-std=c17`**, matching `CMAKE_C_STANDARD 17`
in bm_core's CMakeLists. Not cosmetic: gcc 15 defaults to `gnu23`, whose
`stddef.h` defines `unreachable()` and collides with tinycbor's.

`build.rs` also defines **`ENABLE_TESTING`**, which bm_core's own unit test
build defines. Its only effect here is to give the pure helpers in
`network/bm_linux.c` external linkage: `BM_LINUX_STATIC` expands to nothing, so
`ipv6_pseudo_checksum`, `nodeid_to_ip`, `format_ipv6`, `mac_from_nodeid`,
`multicast_mac_from_ipv6` and `is_multicast` become callable. They are the
oracles for the port's address and checksum layers — `ipv6_pseudo_checksum` in
particular has to agree with lwIP's `ip6_chksum_pseudo` or a host node and an
embedded node cannot validate each other's BCMP checksums.

bm_core declares those six in no header, so `csrc/bm_shim.h` declares them for
bindgen, in a block marked as test-only exports. If upstream makes them static
again, that block is what fails to link.

The define is otherwise narrow: only `network/bm_linux.c`, `bcmp/dfu_core.c`,
`bcmp/dfu_client.c` and `bcmp/dfu.h` consult it, and in the DFU files it only
adds test accessors.

`middleware/bm_mavlink.c` compiles as its own unit so that
`-Wno-address-of-packed-member`, which the mavlink headers need, is not
blanketed over bm_core's code. The build is warning-free under gcc and clang.

### After bumping the submodule

```
git submodule update --init --recursive   # bm_core pulls in tinycbor and mavlink
touch build.rs && cargo test
./scripts/check_symbols.sh
```

A new integrator hook shows up as an unresolved non-libc symbol; a new source
file needs adding to the right tier in `build.rs`.

## Cross-checking against bm_core

`tests/smoke.rs` lifts expected values from `vendor/bm_core/test/src/*_test.cpp`
wherever bm_core already asserts them. To run bm_core's own 189 tests:

```
cd path/to/bm_core
cmake --preset unit-tests && cmake --build --preset unit-tests
ctest --preset unit-tests
```
