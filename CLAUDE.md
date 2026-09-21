# Working in this repo

Read `bm-wire-sys/README.md` (the shim contract, and state that outlives
`bm_shim_reset`) and `docs/c-divergences.md` (every place the Rust port
deliberately reproduces a bm_core quirk) before changing anything. If a fuzzer
fails, `c-divergences.md` is the first place to look.

If the toolchain or the `bm_core` submodule looks wrong, see "Setting up a
fresh sandbox" below.

## Writing style

Docs, comments and commit messages here are **brief and factual**. State what
the code does and why, once. Do not editorialise, dramatise, or repeat a point
in a second phrasing. Drop narrative framing ("the interesting half", "found
the hard way", "this is not theoretical"), value judgements about upstream, and
sentences that only restate the preceding one. Prefer a table or a list to a
paragraph. Cite file and symbol names rather than describing them.

Apply the same rule to reports: say what changed, what was verified, and what
was not.

## The point of this repo

`bm-wire` is a Rust port of bm_core's wire format and protocol logic;
`bm-stack` is the embassy runtime that turns it into a node. Together they run
as firmware on Bristlemouth dev kits alongside nodes running the C/C++
firmware.

Compatibility is proven, not assumed: `bm-wire-sys` compiles the real C as an
oracle, and `bm-wire-diff` feeds identical input to both and asserts identical
output.

**The C is authoritative.** Where bm_core is wrong, `bm-wire` matches it anyway
— deployed nodes are on the other end of the wire — and the finding goes in
`docs/c-divergences.md` for upstream repair.

## Layout

A cargo workspace.

- `bm-wire/` — the port. `no_std`, no `alloc`, `forbid(unsafe_code)`, zero
  dependencies. **Must never depend on `bm-wire-sys`**, in any configuration:
  that keeps the host-only oracle out of firmware builds. The `std` feature is
  for tests and fuzzing only.
  - `fuzz/` — a `cargo fuzz` crate, its own workspace. Targets are ~6 lines
    each; the work is in `bm-wire-diff`.
  - `fuzz/seeds/` — committed seed corpora, one directory per target, replayed
    by `cargo test`. `fuzz/corpus/` is gitignored.
- `bm-stack/` — the node. `no_std`, no `alloc` (except behind the test-only
  `mock` feature), and the only crate that knows about time or I/O.
  - `src/port.rs` — the seams bm_core leaves to the integrator, as traits
    rather than link-time symbols. Config storage and the DFU flash slot are
    still to come.
  - `src/node.rs` — `Node::on_frame`, `on_tick` and `on_expiry` are
    synchronous and take the current time; `Node::run` is the only async code.
    Each has a `_with` twin that reports `Event`s. The two timers are
    bm_core's: the 10 s heartbeat and `packet.c`'s 150 ms expiry sweep, which
    must not be put on a grid of the port's own (divergence #22).
  - `src/mock.rs` — a scripted PHY that also drives embassy's mock clock.
- `bm-phy-adin2111/` — `bm_stack::Phy` for the ADIN2111 over OPEN Alliance TC6
  SPI, on the per-port frame I/O of
  [embassy-rs/embassy#7024](https://github.com/embassy-rs/embassy/pull/7024).
  Each frame's port rides in `PacketMeta::id`. **Its own workspace**, because
  it pins embassy to a git branch and `embassy-time-driver` carries
  `links = "embassy-time"`, so a git embassy and a crates.io embassy cannot
  share a dependency graph. Not built by the root `cargo test`. Its `Runner`
  must be spawned by the firmware — it owns the SPI bus, and until it runs no
  frame moves.
- `bm-wire-diff/` — the differential harness. Host-only. One comparator per
  surface, shared by the fuzz targets and by ordinary `#[test]`s.
  - `tests/node_frames.rs` — compares whole frames `bm-stack` builds against
    the ones bm_core emits for the same question from the same identity.
- `bm-wire-sys/` — raw FFI bindings to the real bm_core C. The oracle.
  - `vendor/bm_core/` — the C submodule. **Never edit it from here.** Fixes go
    upstream to `bristlemouth/bm_core`.
  - `csrc/` — the platform layer bm_core leaves to the integrator, implemented
    deterministically. This is ours.
  - `build.rs` — tiered source lists, the generated guarded header tree,
    bindgen.
  - `scripts/check_symbols.sh` — what `libbm_core.a` references but nothing
    defines; everything left should be libc. Run from the workspace root.
    `--check` fails on anything outside its libc allowlist, which deliberately
    omits `rand` and `time`, so a non-deterministic reach from `csrc/` trips it.
- `docs/c-divergences.md` — the upstream defect list.
- `docs/bcmp-port-todo.md` — what of BCMP is unported, as dependency-ordered
  task cards. Read its shared contract before starting a card.
- `docs/embassy-port-tracking-prompt.md` — a brief for a separate agent working
  in `embassy-rs/embassy`, to make `embassy-net-adin1110` report the ingress
  port and take an egress port per frame. `bm-stack`'s `Phy` waits on it.

## Porting a function to bm-wire

1. Read the C. Note anything that wraps, truncates, reads out of bounds, or
   contradicts its own doc comment.
2. Write the Rust in `bm-wire`, matching the C's observable behaviour including
   its quirks. Use explicit little-endian codecs, not `repr(packed)` mirrors —
   bm_core's `check_endianness` is a no-op on little-endian hosts, so byte
   order has to be written down somewhere.
3. Add a comparator in `bm-wire-diff` and a fuzz target that calls it.
4. Where bm_core's gtest suite asserts a value for the same input, assert that
   literal in a `bm-wire` unit test too. C and Rust agreeing only proves they
   agree; gold vectors prove they are right.
5. If the C is undefined for some inputs, constrain the comparator's input
   domain and say why at the type. Never relax the assertion.
6. Add the finding to `docs/c-divergences.md`.

## Porting a state machine to bm-wire

As above, plus:

1. Write it **sans-io**: no clock, no timers, no transmission. Entry points
   take the current time and return what the caller owes the network.
   `bm-wire/src/neighbor.rs` is the pattern.
2. Compare the *notifications*, not just the resulting state. bm_core's
   application callbacks are registerable from Rust
   (`bcmp_neighbor_register_discovery_callback`, ...); a comparator that only
   diffs the table misses divergence #17.

## Adding a bm_core module to the oracle

1. Add the `.c` to the right tier in `bm-wire-sys/build.rs`, and its header to
   `bm-wire-sys/wrapper.h`.
2. `cargo build`, then `./bm-wire-sys/scripts/check_symbols.sh`. A new non-libc
   undefined symbol is an integrator hook needing an implementation in
   `bm-wire-sys/csrc/`.
3. Add a smoke test, using bm_core's own asserted value where one exists.

## Verifying

```
cargo test                                                 # workspace, incl. differential tests
cargo build -p bm-wire --target thumbv7em-none-eabihf      # proves no_std, alloc-free
cargo build -p bm-wire --target thumbv8m.main-none-eabihf  # the dev kit's Cortex-M33
cargo build -p bm-stack --target thumbv8m.main-none-eabihf
cd bm-phy-adin2111 && cargo test                           # own workspace, needs network
cd bm-phy-adin2111 && cargo build --target thumbv8m.main-none-eabihf
cargo +1.97 check --workspace --all-targets                # the declared MSRV
cargo tree -p bm-wire                                      # must show no dependencies
./bm-wire-sys/scripts/check_symbols.sh --check             # only libc may be unresolved
cd bm-wire/fuzz && mkdir -p corpus/<target>                # libFuzzer wants it to exist
cd bm-wire/fuzz && cargo fuzz run <target> corpus/<target> seeds/<target>
```

CI runs all of this on every push; see `.github/workflows/`. Fuzzing is the
exception — `cargo test` replays the committed seeds, and `fuzz.yml` does
open-ended runs nightly and on demand.

MSRV is 1.97, declared in `[workspace.package]` and repeated in
bm-phy-adin2111's manifest. It is embassy's number: embassy promises only that
it compiles on the latest stable, `bm-stack` depends on embassy-time, and 1.97
is the channel embassy's `rust-toolchain.toml` pins. Bump both manifests when
embassy bumps.

### Fuzzing notes

`cargo fuzz` needs nightly, and wants `bm-wire/fuzz` as the working directory:
libFuzzer resolves corpus paths against the shell's directory.

`build.rs` adds `-fsanitize=address,undefined` to the C under
`CARGO_CFG_FUZZING`, so the fuzzers check the C for undefined behaviour as well
as the port for divergence. That found divergence #6. One check is off —
`-fno-sanitize=alignment`, because `clear_ports_legacy` trips it on every
received frame (divergence #11). Do not widen that exemption without a
divergence entry saying why.

On a crash: `cargo fuzz tmin <target> <artifact>`, then drop the minimized file
into `bm-wire/fuzz/seeds/<target>/`, where `cargo test` will replay it.

## Setting up a fresh sandbox

A container often ships a stable toolchain older than the 1.97 MSRV, and then
nothing builds: `cargo test` reports "rustc N is not supported by the following
package". That is a stale image, not a repo problem, and the MSRV is not up for
discussion.

`.claude/hooks/session-start.sh` handles all of it and is registered as a
`SessionStart` hook, so on Claude Code on the web it has already run. It checks
out the `bm_core` submodule tree, updates stable, installs the MSRV toolchain
(with rustfmt and clippy) and nightly, adds both embedded targets to both,
installs `cargo-fuzz`, and warms the three workspaces' dependency caches. It
reads the MSRV from `Cargo.toml`, is idempotent, and no-ops outside a remote
container. Run it by hand with:

```
CLAUDE_CODE_REMOTE=true ./.claude/hooks/session-start.sh
```

**None of this is a finding. Do not report it.** Two things here are worth
raising, and the hook fails loudly on both:

- the MSRV names a toolchain that cannot be installed, or stable cannot be
  brought up to it;
- a crate that fails to compile on the MSRV once it is installed, which means
  both manifests need bumping together.

## Conventions

- Tests touching the shim take the `SHIM` lock — the C state is process-global.
- Most comparators call a pure C function and need no shim state.
  `bm-wire-diff/src/bcmp.rs` is the exception and the pattern for the rest:
  `serialize` and `process_received_message` read `packet.c`'s file-scope
  `PACKET`, so the module brings the oracle up once per process behind a
  `OnceLock` and serialises every C call behind a `Mutex`.
- **Nothing in `bm-wire-diff` may call `bm_shim_reset`.** `packet_init` hands
  `PACKET` a shim mutex and a shim timer, and `packet.c` has no deinit, so a
  reset frees objects the C still points at. Registering only non-sequenced
  message types keeps the C's sequence list from growing, which lets the `bcmp`
  fuzz target run in-process rather than in fork mode.
- A comparator that brings the **whole stack** up needs a process to itself:
  `bm_shim_stack_init` calls `packet_init` with `bm_linux.c`'s accessors while
  `bcmp.rs` calls it with its own, and whichever runs second wins. Build on
  `bm-wire-diff/src/stack.rs`, drive it from its own file under
  `bm-wire-diff/tests/`, and register its seeds in `replay::STACK_TARGETS`
  rather than `replay::TARGETS`. A test asserts every `seeds/` directory is in
  exactly one list.
- Use `stack::pump_until_quiet`, not a bare `bm_shim_pump`, after injecting: one
  pump runs each task once in creation order, so a received frame reaches L2,
  then BCMP, then L2 again over successive pumps.
- Everything in `bm-wire-sys/csrc/` stays deterministic: no threads, sockets,
  wall clock or randomness. A fuzz input has to replay byte-identically.
- `bm_shim_pump()` escapes task loops with `longjmp`. Anything added to `csrc/`
  that can be called from inside a task body must hold no resource across a
  blocking primitive.
