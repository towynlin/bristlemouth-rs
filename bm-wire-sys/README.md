# bm-wire-sys

Raw FFI bindings to [bm_core](https://github.com/bristlemouth/bm_core)'s C
implementation, built to be a differential-fuzzing oracle for a pure, safe,
idiomatic Rust port. It compiles the real C — the same code running on
deployed Bristlemouth nodes — so the port can be proven bit-compatible rather
than assumed to be.

Host-only. `build.rs` panics if it is ever pulled into a `target_os = "none"`
build.

```
cargo test                  # build everything and run the smoke tests
./scripts/check_symbols.sh  # confirm nothing but libc is left unresolved
```

## The platform shim

bm_core is a library the integrator completes. `bm_os.h`, `bm_ip.h`,
`bm_configs_generic.h`, `bm_rtc.h`, `bm_dfu_generic.h` and the `bm_config.h`
macros are declared but not defined; firmware supplies FreeRTOS and lwIP,
and `bm_sbc` supplies POSIX threads and raw sockets.

Neither suits a fuzz target, so `csrc/` is a third backend. **Its behaviour
differs from an RTOS in ways that matter**, and the differences are the point:

- **No threads.** `bm_task_create` records the entry point and returns without
  running it; `bm_start_scheduler` is a no-op. Rust drives execution with
  `bm_shim_pump()`.
- **Tasks are escaped with `longjmp`.** bm_core's task bodies are `while (true)`
  loops that block on a queue and never return, so `bm_shim_pump` enters one
  with `setjmp` and the blocking primitives jump back out once the task is
  idle. bm_core calls those primitives as the first statement of each loop
  iteration, so nothing is live across the jump. A task whose loop never
  touches a queue is abandoned after `bm_shim_set_pump_budget` blocking calls.
- **No blocking.** `bm_queue_receive`, `bm_semaphore_take` and
  `bm_stream_buffer_receive` ignore their timeout and report `BmETIMEDOUT`
  immediately rather than sleeping. Queues are real bounded FIFOs, so message
  ordering is preserved; a send to a full queue fails rather than waiting.
- **Virtual clock.** 1 tick == 1 ms, as in `bm_posix.c`, but the counter only
  moves when `bm_shim_advance_ticks` or `bm_delay` says so, firing due timers
  as it goes. Nothing reads the wall clock, so an input replays identically.
- **The PHY is a capture ring.** `bm_shim_network_device()` implements
  `NetworkDeviceTrait` by copying every transmitted frame where
  `bm_shim_tx_pop` can drain it, and `bm_shim_rx_inject` pushes bytes up the
  path the ADIN2111 driver would. That is the wire boundary and the natural
  fuzz entry point.
- **NVM, RTC and DFU flash are RAM.** Config partitions, the clock and the
  update slot are plain buffers, cleared by `bm_shim_generic_reset`.
- **Leak-visible.** Every allocation is plain `malloc`, so ASan and
  LeakSanitizer see the whole graph.

`csrc/bm_stack_shim.c` brings the stack up on the capture device in the order
`middleware/bristlemouth.c` uses.

### State that outlives `bm_shim_reset`

`bm_shim_reset()` tears down everything the shim owns. It cannot touch
bm_core's own file-scope state, and **`bm_l2_deinit` is the only teardown
function bm_core exposes** — `bcmp.c`, `packet.c`, `pubsub.c`, `neighbors.c`,
`topology.c`, `configuration.c`, `resource_discovery.c`, `bm_service.c` and
the DFU modules all keep their statics for the life of the process.

Two consequences:

- Resetting the shim while those modules are up leaves them holding pointers
  to freed queues and timers. Nothing dereferences them as long as no stale
  task is pumped, but do not count on that.
- A fuzz target that brings the stack up must run **one stack per process**
  (`cargo fuzz`'s default fork mode is fine), or restrict itself to the
  modules that can be re-initialised: `packet.c` via `packet_remove`, and
  anything in T0–T2.

This is not theoretical. The wire-path tests originally shared a process with
the rest of the suite and segfaulted intermittently, depending on which thread
the harness happened to reset the shim from. They now live in `tests/stack.rs`,
which cargo runs as its own process — the same one-stack-per-process rule a
fuzz target has to follow.

Adding deinit functions upstream would remove this constraint, and is the
single highest-value change bm_core could make for the port effort.

## Coverage

Sources are grouped in `build.rs` by what they need from the shim, so a link
failure points at a tier instead of a wall of errors.

| Tier | Needs | Modules |
|---|---|---|
| T0 | nothing | `crc16/32`, `util`, `lib_state_machine`, `device`, `l2_policy` |
| T1 | the `bm_os` shim | `aligned_malloc`, `ll`, `q`, `pcap`, `cb_queue`, `timer_callback_handler`, `bcmp/packet` |
| T2 | tinycbor | `configuration`, `cbor_service_helper`, and the C half of `bm_common_messages` |
| T3 | `bm_ip` + a NetworkDevice | `bm_linux`, `l2`, all of `bcmp/`, all of `middleware/`, all of `integrations/` |
| T4 | the DFU flash shim | `dfu_core`, `dfu_client`, `dfu_host`, `bm_mavlink` |

`network/bm_linux.c` serves as the `bm_ip.h` backend rather than a hand-written
stub. Despite the name it opens no sockets and starts no threads: it is a pure
software IPv6 stack that builds and parses Ethernet/IPv6/UDP frames in
malloc'd buffers and hands them to L2. Using it means a Rust port is compared
against bm_core's real framing.

Deliberately excluded:

| Excluded | Why |
|---|---|
| `drivers/adin2111/*` | SPI hardware driver; no wire-format content |
| `network/bm_lwip.c`, `common/bm_freertos.c`, `common/bm_posix.c` | alternative backends for the interfaces `csrc/` implements |
| `middleware/bristlemouth.c` | calls `adin2111_network_device()` directly, so it only works against the real PHY; `csrc/bm_stack_shim.c` replaces it |
| `bm_common_messages/*.cpp` | namespaced C++ with reference parameters; needs hand-written `extern "C"` thunks. `sensor_header_msg` ships as both `.c` and `.cpp`; the `.c` is bound |

## Bindings

One flat `bindings.rs`, allowlisted **by file** rather than by symbol, so
coverage tracks the submodule as bm_core moves upstream — new message types
appear without touching `build.rs`.

Two dozen bm_core headers have no include guard, which makes parsing them in
one translation unit a redefinition error. `build.rs` shadows the header tree
into `OUT_DIR` with guards added, and points bindgen at the copies. A stub that
forwards with `#include_next` is not enough: a quoted include searches the
including file's own directory first, so a sibling header reaches the
unguarded original and bypasses the stub. The C build still compiles against
`vendor/`, so debug info points at real sources. Adding guards upstream would
retire all of this.

`util.h`'s `static inline` helpers (`ip_to_nodeid`, `uint8_to_uint16/32`) are
emitted as callable out-of-line copies via bindgen's `wrap_static_fns`;
`build.rs` compiles the generated `static_fns.c` as a second archive.

Note that a header can declare a function whose `.c` file is excluded — the
`.cpp` message types, for instance. Those come out as `pub fn` that fail at
link time if called.

## Compiler flags worth knowing about

`build.rs` compiles everything at **`-std=c17`**, matching `CMAKE_C_STANDARD 17`
in bm_core's CMakeLists. This is not cosmetic: gcc 15 defaults to `gnu23`,
whose `stddef.h` defines `unreachable()` and collides with tinycbor's. An
oracle should compile bm_core under the standard the firmware is built with.

`build.rs` also defines **`ENABLE_TESTING`**, which is what bm_core's own unit
test build defines. Its only effect here is to give the pure helpers in
`network/bm_linux.c` external linkage: `BM_LINUX_STATIC` expands to nothing, so
`ipv6_pseudo_checksum`, `nodeid_to_ip`, `format_ipv6`, `mac_from_nodeid`,
`multicast_mac_from_ipv6` and `is_multicast` become callable instead of
file-static. They are the oracles for the Rust port's address and checksum
layers — `ipv6_pseudo_checksum` in particular has to agree with lwIP's
`ip6_chksum_pseudo` or a host node and an embedded node cannot validate each
other's BCMP checksums.

bm_core declares those six in no header, so `csrc/bm_shim.h` declares them for
bindgen, in a block marked as test-only exports rather than public API. If
upstream ever makes them static again, that block is what fails to link.

The define is otherwise narrow: only `network/bm_linux.c`, `bcmp/dfu_core.c`,
`bcmp/dfu_client.c` and `bcmp/dfu.h` consult it, and in the DFU files it only
adds test accessors. The ADIN driver also consults it but is not compiled.

`middleware/bm_mavlink.c` compiles as its own unit so that
`-Wno-address-of-packed-member` — which the mavlink headers need, and which
bm_core applies to its own mavlink target for the same reason — is not
blanketed over bm_core's code. The build is warning-free under both gcc and
clang; if you see mavlink warnings, something has escaped that scoping.

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
wherever bm_core already asserts them, so the suites cannot silently diverge.
To run bm_core's own 189 tests:

```
cd path/to/bm_core
cmake --preset unit-tests && cmake --build --preset unit-tests
ctest --preset unit-tests
```
