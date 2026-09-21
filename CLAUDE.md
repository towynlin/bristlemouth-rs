# Working in this repo

If the toolchain or the `bm_core` submodule is not set up yet, **"Setting up a
fresh sandbox"** below is the whole of it — and is not worth reporting back.

Read `bm-wire-sys/README.md` first — especially the shim contract and the note
on state that outlives `bm_shim_reset`. Both describe real behavioural
divergence from the firmware, and both are easy to trip over.

Then read `docs/c-divergences.md`. It lists every place bm_core's C does
something surprising that the Rust port deliberately reproduces. If a change
here makes a fuzzer fail, that file is the first place to look.

## The point of this repo

`bm-wire` is a pure Rust port of bm_core's wire format and protocol logic;
`bm-stack` is the embassy runtime that turns it into a node. Together they are
intended to run as firmware on Bristlemouth dev kits alongside nodes running
the C/C++ firmware. Compatibility
is *proven*, not assumed: `bm-wire-sys` compiles the real C as an oracle, and
`bm-wire-diff` feeds identical input to both and asserts identical output.

**The C is authoritative.** Where bm_core is surprising or wrong, `bm-wire`
matches it anyway — deployed nodes are on the other end of the wire — and the
finding is written up in `docs/c-divergences.md` for upstream repair.

## Layout

A cargo workspace.

- `bm-wire/` — the port. `no_std`, no `alloc`, `forbid(unsafe_code)`, zero
  dependencies. **Must never depend on `bm-wire-sys`**, in any configuration:
  that is what keeps the host-only oracle out of firmware builds. Its `std`
  feature exists only for tests and fuzzing.
  - `fuzz/` — a `cargo fuzz` crate, its own workspace. Targets are ~6 lines
    each; the real work is in `bm-wire-diff`.
  - `fuzz/seeds/` — committed seed corpora, one directory per target, replayed
    by `cargo test`. `fuzz/corpus/` is gitignored, so durable inputs live here.
- `bm-stack/` — the node. `no_std`, no `alloc` (except behind the test-only
  `mock` feature), and the only crate here that knows about time or I/O. It
  supplies what `bm-wire` deliberately lacks: a clock, a timer, and a PHY.
  - `src/port.rs` — the seams bm_core leaves to the integrator, as traits
    rather than link-time symbols, so a test and the firmware can differ. A
    card that ports an exchange needing a new one adds it here: `Rtc` arrived
    with system time, and configuration storage and the DFU flash slot are
    still to come.
  - `src/node.rs` — `Node::on_frame`, `Node::on_tick` and `Node::on_expiry` are
    synchronous and take the current time; `Node::run` is the only async code.
    All the protocol is in the synchronous half. Each has a `_with` twin that
    reports `Event`s — a reply, a timeout, an unsolicited message — which is
    where a ported exchange's requester half hangs. The two timers are
    bm_core's two: the ten-second heartbeat and `packet.c`'s 150 ms expiry
    sweep, which must not be put on a grid of the port's own (divergence #22).
  - `src/mock.rs` — a scripted PHY that also drives embassy's mock clock, so
    the real `run` loop can be tested with no hardware.
- `bm-phy-adin2111/` — [`bm_stack::Phy`] for the ADIN2111 over OPEN Alliance
  TC6 SPI, on the per-port frame I/O of
  [embassy-rs/embassy#7024](https://github.com/embassy-rs/embassy/pull/7024).
  The port of each frame rides in `PacketMeta::id`. **Its own workspace**, like
  `bm-wire/fuzz`, for two reasons: it pins embassy to a git branch, and
  `embassy-time-driver` carries `links = "embassy-time"`, so a git embassy and
  a crates.io embassy cannot coexist in one dependency graph; and it needs
  **toolchain 1.97**, pinned by its own `rust-toolchain.toml`, because
  `xarxa-driver` uses `cfg_select!`. Keeping both here means the main workspace
  stays on released crates, and `cargo test` at the root needs no network. It
  is not built by the root `cargo test`; verify it explicitly.
  Note that the driver's `Runner` must be spawned by the firmware — it owns the
  SPI bus, and until it runs no frame moves.
- `bm-wire-diff/` — the differential harness. Host-only, depends on the other
  three crates. One comparator per surface, shared by the fuzz targets and by
  ordinary `#[test]`s.
  - `tests/node_frames.rs` — compares whole frames `bm-stack` builds against
    the ones bm_core emits for the same question from the same identity. If
    those agree, a node running this firmware is indistinguishable on the wire.
- `bm-wire-sys/` — raw FFI bindings to the real bm_core C. The oracle.
  - `vendor/bm_core/` — the C submodule. **Never edit it from here.** Changes
    go upstream to `bristlemouth/bm_core`.
  - `csrc/` — the platform layer bm_core leaves to the integrator, implemented
    deterministically. This is ours.
  - `build.rs` — tiered source lists, the generated guarded header tree,
    bindgen.
  - `tests/smoke.rs`, `tests/stack.rs` — one or two tests per tier, proving
    each links and runs.
  - `scripts/check_symbols.sh` — what `libbm_core.a` references but nothing
    defines. Everything left should be libc. Run it from the workspace root.
    `--check` fails on anything outside its libc allowlist, which is what CI
    runs; the allowlist deliberately omits `rand`, `time` and friends, so a
    non-deterministic reach from `csrc/` trips it.
- `docs/c-divergences.md` — the upstream bug list.
- `docs/bcmp-port-todo.md` — what of BCMP is still unported, as
  dependency-ordered task cards sized for one agent each. Read its shared
  contract before starting a card: it records the constraints that are easy
  to get wrong, including why `bm_core`'s gtest suite is not the source of
  gold vectors most of the recipe assumes.
- `docs/embassy-port-tracking-prompt.md` — a self-contained brief for a
  separate agent working in `embassy-rs/embassy`, to make
  `embassy-net-adin1110` report the ingress port and take an egress port per
  frame. `bm-stack`'s `Phy` trait is waiting on it.

## Porting a state machine to bm-wire

Same as above, with two additions.

1. Write it **sans-io**: no clock, no timers, no transmission. Entry points
   take the current time and return what the caller owes the network.
   `bm-wire/src/neighbor.rs` is the pattern.
2. Compare the *notifications*, not just the resulting state. bm_core's
   application-facing callbacks are registerable from Rust
   (`bcmp_neighbor_register_discovery_callback`, ...), and a comparator that
   only diffs the table misses everything the application actually sees --
   divergence #17 is invisible in the table and obvious in the callbacks.

## Porting a function to bm-wire

1. Read the C. Note anything that wraps, truncates, reads out of bounds, or
   contradicts its own doc comment.
2. Write the Rust in `bm-wire`. Match the C's observable behaviour, including
   its quirks. Use explicit little-endian codecs rather than `repr(packed)`
   mirrors — bm_core's own `check_endianness` is a no-op on little-endian
   hosts, so the byte order has to be written down somewhere.
3. Add a comparator in `bm-wire-diff` and a fuzz target that calls it.
4. Where bm_core's gtest suite asserts a value for the same input, assert that
   *literal* value in a `bm-wire` unit test too. C and Rust agreeing only
   proves they agree; the gold vectors prove they are right.
5. If the C is undefined for some inputs, constrain the comparator's input
   domain and say why at the type — never relax the assertion.
6. Add the finding to `docs/c-divergences.md`.

## Adding a bm_core module to the oracle

1. Add the `.c` to the right tier in `bm-wire-sys/build.rs`, and its header to
   `bm-wire-sys/wrapper.h`.
2. `cargo build`, then `./bm-wire-sys/scripts/check_symbols.sh`. A new non-libc
   undefined symbol is an integrator hook needing an implementation in
   `bm-wire-sys/csrc/`.
3. Add a smoke test. If bm_core's gtest suite already asserts a value for the
   same input, use that value rather than inventing one.

## Verifying

```
cargo test                                       # workspace, incl. differential tests
cargo build -p bm-wire --target thumbv7em-none-eabihf   # proves no_std, alloc-free
cargo build -p bm-wire --target thumbv8m.main-none-eabihf  # the dev kit's Cortex-M33
cargo build -p bm-stack --target thumbv8m.main-none-eabihf # the node, same target
cd bm-phy-adin2111 && cargo test                 # own workspace, needs network
cd bm-phy-adin2111 && cargo build --target thumbv8m.main-none-eabihf
cargo +1.97 check --workspace --all-targets      # the declared MSRV
cargo tree -p bm-wire                            # must show no dependencies
./bm-wire-sys/scripts/check_symbols.sh --check   # only libc may be unresolved
cd bm-wire/fuzz && mkdir -p corpus/<target>      # libFuzzer wants it to exist
cd bm-wire/fuzz && cargo fuzz run <target> corpus/<target> seeds/<target>
```

The MSRV is 1.97, declared once in `[workspace.package]` and repeated in
bm-phy-adin2111's own manifest. It is embassy's number, not one of ours:
embassy's README promises only that it compiles on the latest stable, and
`bm-stack` depends on embassy-time, so 1.97 — the channel embassy's
`rust-toolchain.toml` pins, and the oldest stable it actually tests — is the
most this repo can honestly claim. Bump both manifests when embassy bumps.

CI runs all of this on every push; see `.github/workflows/`. Fuzzing is the
exception — `cargo test` replays the committed seeds, and `fuzz.yml` does the
open-ended runs nightly and on demand.

`cargo fuzz` needs nightly, and wants `bm-wire/fuzz` as the working directory
rather than `bm-wire`: cargo-fuzz finds the crate either way, but libFuzzer
resolves the corpus paths against the shell's own directory. `build.rs` adds
`-fsanitize=address,undefined` to the C under `CARGO_CFG_FUZZING`, matching
cargo-fuzz on the Rust side — so the fuzzers check the C for undefined
behaviour as well as checking the port for divergence. That is how divergence
#6 was found.

One UBSan check is deliberately off: `-fno-sanitize=alignment`. Every BCMP
frame bm_core receives trips it, because `clear_ports_legacy` does a 32-bit
access at frame offset 26 — divergence #11. Leaving it on means aborting on the
first receive rather than finding anything. Do not widen that exemption without
a divergence entry saying why.

When a fuzzer finds a crash: `cargo fuzz tmin <target> <artifact>`, drop the
minimized file into `bm-wire/fuzz/seeds/<target>/`, and it becomes a permanent
regression test the next time `cargo test` runs.

## Setting up a fresh sandbox

**A container often ships a stable toolchain older than the 1.97 MSRV, and then
nothing builds at all** — `cargo test` refuses outright with "rustc N is not
supported by the following package". That is the image being stale. It is not a
problem with the repo, and the MSRV is not up for discussion. Install what is
missing and carry on:

```
rustup update stable                                   # if stable < 1.97
rustup toolchain install 1.97                          # for the MSRV check
rustup toolchain install nightly --profile minimal     # for cargo fuzz
rustup target add thumbv7em-none-eabihf thumbv8m.main-none-eabihf
rustup target add --toolchain 1.97 thumbv7em-none-eabihf thumbv8m.main-none-eabihf
rustup component add --toolchain 1.97 rustfmt clippy   # bm-phy-adin2111 pins 1.97
cargo install cargo-fuzz --locked                      # if `cargo fuzz` is missing
git submodule update --init --recursive                # the bm_core oracle
```

**None of this is a finding. Do not report it.** It is setup, it costs a few
minutes, and it says nothing about the code. In particular it is not evidence
that the MSRV is wrong, that CI is broken, or that anything has drifted — only
that the image predates 1.97. Every session that has hit this has reported it,
and every report has been noise.

The one thing here worth raising is the opposite case: a crate that genuinely
fails to **compile** on 1.97 once 1.97 is installed. That would mean the MSRV
claim has become false, and both manifests need bumping together.

## Conventions

- Tests touching the shim take the `SHIM` lock — the C state is process-global.
- Most comparators in `bm-wire-diff` call a pure C function and need no shim
  state at all. `bm-wire-diff/src/bcmp.rs` is the exception, and sets the
  pattern for the ones that follow it: `serialize` and
  `process_received_message` read `packet.c`'s file-scope `PACKET`, so the
  module brings the oracle up **once per process** behind a `OnceLock` and
  serialises every C call behind a `Mutex`.
- **Nothing in `bm-wire-diff` may call `bm_shim_reset`.** `packet_init` hands
  `PACKET` a shim mutex and a shim timer, and `packet.c` has no deinit, so a
  reset frees objects the C still points at. Registering only non-sequenced
  message types keeps the C's sequence list from growing, which is what lets
  the `bcmp` fuzz target run in-process rather than needing fork mode.
- A comparator that brings the **whole stack** up needs a process to itself:
  `bm_shim_stack_init` calls `packet_init` with `bm_linux.c`'s accessors, while
  `bcmp.rs` calls it with its own, and whichever runs second wins. The shared
  bring-up lives in `bm-wire-diff/src/stack.rs`; comparators built on it
  (`l2_egress.rs`, `bcmp_messages.rs`) are driven from their own file under
  `bm-wire-diff/tests/`, which cargo runs as a separate binary, and their seeds
  go in `replay::STACK_TARGETS` rather than `replay::TARGETS`. A test asserts
  every `seeds/` directory is in exactly one of the two, so a new corpus cannot
  silently go unreplayed.
- `stack::pump_until_quiet` rather than `bm_shim_pump` after injecting: a pump
  runs each task once in creation order, so a received frame reaches L2, then
  BCMP, then L2 again over successive pumps, transmitting nothing in between.
- Everything in `bm-wire-sys/csrc/` must stay deterministic: no threads, no
  sockets, no wall clock, no randomness. A fuzz input has to replay
  byte-identically.
- `bm_shim_pump()` escapes task loops with `longjmp`. Anything added to `csrc/`
  that can be called from inside a task body must hold no resource across a
  blocking primitive.
