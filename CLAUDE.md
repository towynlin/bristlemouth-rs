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

## Pull requests

Open a pull request against `main` when the work on a branch is done and
verified. Do not wait to be asked: this file is the standing request. One PR
per branch; a plan card's PR follows its "Working a card" section. Push
fixes for CI failures and review comments to the same branch.

The description reports, per "Writing style": what changed, each verify
command run, fuzz minutes per target, and what was not verified.

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

Four cargo workspaces: the root, `bm-wire/fuzz`, `bm-phy-adin2111` and
`bm-devkit`. The last two pin embassy to git, and `embassy-time-driver`
carries `links = "embassy-time"`, so a git embassy and a crates.io embassy
cannot share a dependency graph. The root `cargo test` builds neither.

Each crate's `lib.rs` module doc, and each `tests/` file's, says what it
holds; `cargo doc --open` is the per-file index. The `README.md` in
`bm-wire-sys`, `bm-mcuboot-sys`, `bm-image` and `bm-devkit` is that crate's
contract.

| Crate | What it is |
| --- | --- |
| `bm-wire/` | The port: codecs and sans-io state machines. |
| `bm-wire/fuzz/` | `cargo fuzz` targets, ~6 lines each; the work is in `bm-wire-diff`. |
| `bm-stack/` | The node on embassy: `Node`, the `port` traits, `App`, `Services`, DFU, the `channel` feature, the `mock` PHY. |
| `bm-phy-adin2111/` | `bm_stack::Phy` for the ADIN2111 over OPEN Alliance TC6 SPI. |
| `bm-devkit/` | Board support and firmware (`bringup`, `hello_world`) for the dev kit's mote: STM32U575CI, ADIN2111, W25Q64JV. |
| `bm-wire-diff/` | The differential harness: one comparator per surface, shared by fuzz targets and `#[test]`s. |
| `bm-wire-sys/` | The oracle: FFI bindings to the real bm_core C. |
| `bm-mcuboot/` | MCUboot's image header, TLV area and slot trailer. |
| `bm-mcuboot-sys/` | The oracle for slot contents and images: MCUboot v1.9.0's `bootutil` with bm_protocol's configuration, over RAM flash. |
| `bm-mcuboot-diff/` | `bm-mcuboot` against `bm-mcuboot-sys`. |
| `bm-image/` | Builds and reads `.dfu.bin` and `.unified.bin`; library and CLI. |
| `docs/c-divergences.md` | The upstream defect list. |
| `docs/history/` | Closed plans and briefs. **No work there.** `bcmp-port-todo.md`'s shared contract is the record of the porting rules. |

### Rules

Dependencies:

- `bm-wire`: `no_std`, no `alloc`, `forbid(unsafe_code)`, and one dependency:
  `cbor2` at `default-features = false`. **Must never depend on
  `bm-wire-sys`**, in any configuration. The `std` feature is for tests and
  fuzzing only, and must not forward to `cbor2`.
- `bm-stack`: `no_std`, no `alloc` except behind the test-only `mock` feature.
  The only crate that knows about time or I/O. `channel` is off by default,
  so a single-task firmware carries neither `embassy-sync` nor `heapless`.
- `bm-mcuboot`: `no_std`, no `alloc`, no dependencies, `forbid(unsafe_code)`.
  **Must never depend on `bm-mcuboot-sys`.** SHA-256 and ed25519 live in
  `bm-image`.
- `bm-wire-sys`, `bm-mcuboot-sys`, `bm-wire-diff`, `bm-mcuboot-diff` and
  `bm-image` are host-only. `bm-wire` and `bm-stack` must never depend on
  them. `bm-mcuboot-diff` is separate from `bm-wire-diff` so that the fuzz
  workspace does not build MCUboot.

Vendored C:

- `bm-wire-sys/vendor/bm_core/` and `bm-mcuboot-sys/vendor/mcuboot/` are
  submodules. **Never edit them.** Fixes go upstream to `bristlemouth/bm_core`.
  Check `mcuboot` out without `--recursive`: its own submodules are not used.
- `csrc/` in both `-sys` crates is ours: the platform layer, deterministic.
- `bm-wire-sys/build.rs` holds the tiered source lists. `T2_RELEASE` compiles
  `cbor_service_helper.c` and four message codecs with `NDEBUG`, as a release
  build does (divergences #82, #88).
- `bm-wire-sys/scripts/check_symbols.sh --check`, from the workspace root,
  fails on any unresolved symbol outside its libc allowlist. The allowlist
  omits `rand` and `time`, so a non-deterministic reach from `csrc/` trips it.

`bm-stack`:

- `Node::on_frame`, `on_tick` and `on_expiry` are synchronous and take the
  current time; `Node::run` and `run_app` are the only async code. Each entry
  point has a `_with` twin that reports `Event`s.
- The three timers are bm_core's: the 10 s heartbeat, `packet.c`'s 150 ms
  expiry sweep and `bm_service_request.c`'s 500 ms sweep
  (`on_service_expiry`), which must not be put on a grid of the port's own
  (divergence #22).
- `examples/hello_node.rs` uses the public API only and panics on a wrong
  outcome, so CI runs it.

Tests:

- Build peer frames with `bm-stack/src/mock/frames.rs` or
  `bm-wire-diff/src/frames.rs`, not a local builder.
- `bm-wire/fuzz/seeds/<target>/` is committed and replayed by `cargo test`;
  `fuzz/corpus/` is gitignored.
- `bm-wire-diff/testdata/` holds pcaps from C dev kits;
  `tests/capture_hello_pub.rs` documents `hello-pub.pcap`.

Firmware:

- `bm-phy-adin2111`'s `Runner` owns the SPI bus and must be spawned by the
  firmware; until it runs no frame moves. It is on the per-port frame I/O of
  [embassy-rs/embassy#7024](https://github.com/embassy-rs/embassy/pull/7024),
  merged to embassy `main` and awaiting an `embassy-net-adin1110` release.
- `bm-devkit` images link for MCUboot slot 1 and run behind bm_protocol's
  bootloader: `memory.x` starts `FLASH` at `0x0800C200`, and `devkit.x` is
  cortex-m-rt 0.7.7's `link.x`, pinned in `Cargo.toml`.
- `bm-devkit/build.sh [cargo args]` builds and then runs `image.sh` on each
  binary, making `<elf>.dfu.bin` (signed when `BM_IMAGE_KEY` is set) and
  `<elf>.unified.bin` (when `BM_BOOTLOADER` is). `cargo run` goes through
  `runner.sh`, which programs the `.dfu.bin` at `0x0800C000` and attaches.
- The bootloader starts the IWDG and it cannot be stopped; `bm_devkit::start`
  spawns the task that feeds it.
- bm_protocol is not vendored. `bm-devkit/README.md` records its BSP (pins,
  clocks, ADIN2111 sequence, node id, flash layout) with file and line
  references; read it there rather than re-deriving it.

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
cargo run -p bm-stack --example hello_node                 # the public API, end to end
cargo build -p bm-wire --target thumbv7em-none-eabihf      # proves no_std, alloc-free
cargo build -p bm-wire --target thumbv8m.main-none-eabihf  # the dev kit's Cortex-M33
cargo build -p bm-stack --target thumbv8m.main-none-eabihf
cargo build -p bm-mcuboot --target thumbv7em-none-eabihf
cargo build -p bm-mcuboot --target thumbv8m.main-none-eabihf
cd bm-phy-adin2111 && cargo test                           # own workspace, needs network
cd bm-phy-adin2111 && cargo build --target thumbv8m.main-none-eabihf
cd bm-devkit && cargo build && ./build.sh --release       # own workspace, thumb only; ELFs and .dfu.bin
cargo +1.97 check --workspace --all-targets                # the declared MSRV
cargo tree -p bm-wire                                      # only cbor2, serde, serde_core
cargo tree -p bm-mcuboot                                   # no dependencies
./bm-wire-sys/scripts/check_symbols.sh --check             # only libc may be unresolved
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features \
  -p bm-wire -p bm-stack -p bm-wire-diff -p bm-mcuboot-sys \
  -p bm-mcuboot -p bm-mcuboot-diff -p bm-image             # -D warnings, as CI does
cd bm-wire/fuzz && mkdir -p corpus/<target>                # libFuzzer wants it to exist
cd bm-wire/fuzz && cargo fuzz run <target> corpus/<target> seeds/<target>
```

`RUSTDOCFLAGS` matters: a broken intra-doc link — usually a public item linking
to a private one — is a warning by default, so a bare `cargo doc` passes where
CI fails. `bm-wire-sys` is excluded because bindgen re-emits bm_core's own C
comments as doc comments.

There are four lockfiles — `Cargo.lock`, `bm-wire/fuzz/Cargo.lock`,
`bm-phy-adin2111/Cargo.lock` and `bm-devkit/Cargo.lock` — and the other three
workspaces depend on the root crates by path. **Changing any dependency in
`bm-wire`, `bm-stack` or `bm-wire-diff` invalidates all four**, and nothing in
the root workspace says so: CI runs every job with `--locked`, so a stale
lockfile fails the build before it compiles anything. Run every workspace's
verification lines, not just the root's, and commit whichever lockfiles move.

CI runs all of this on every push, plus four things this list leaves out:
`cargo fmt --all --check` twice, since `bm-wire/fuzz` is its own workspace;
`cargo clippy --workspace --all-targets -- -D warnings`; the same clippy for
`thumbv8m.main-none-eabihf`; and `cargo test -p bm-wire` alone, the only run
with the `std` feature off. `.github/workflows/ci.yml` is the whole of it.
Fuzzing is the exception — `cargo test` replays the committed seeds, and
`fuzz.yml` does open-ended runs nightly and on demand.

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
received frame (divergence #11). The `services` target also sets ASan's
`strict_memcmp=0`, because bm_core's own registration order compares
`SUB_LIST` entries past their end (divergence #38). Do not widen either
exemption without a divergence entry saying why.

On a crash: `cargo fuzz tmin <target> <artifact>`, then drop the minimized file
into `bm-wire/fuzz/seeds/<target>/`, where `cargo test` will replay it.

## Setting up a fresh sandbox

A container often ships a stable toolchain older than the 1.97 MSRV, and then
nothing builds: `cargo test` reports "rustc N is not supported by the following
package". That is a stale image, not a repo problem, and the MSRV is not up for
discussion.

`.claude/hooks/session-start.sh` handles all of it and is registered as a
`SessionStart` hook, so on Claude Code on the web it has already run. It checks
out the `bm_core` submodule tree and the `mcuboot` submodule, updates stable,
installs the MSRV toolchain (with rustfmt and clippy) and nightly, adds both
embedded targets to both,
installs `cargo-fuzz`, and warms the four workspaces' dependency caches. It
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
- `stack::drain` returns every frame the oracle built with the source MAC and
  hop limit rewritten to a deployed node's (`stack::normalise`, divergence
  #70), so a comparator compares them against `bm-wire`'s frames unchanged.
- Use `stack::pump_until_quiet`, not a bare `bm_shim_pump`, after injecting: one
  pump runs each task once in creation order, so a received frame reaches L2,
  then BCMP, then L2 again over successive pumps.
- Everything in `bm-wire-sys/csrc/` stays deterministic: no threads, sockets,
  wall clock or randomness. A fuzz input has to replay byte-identically.
- `bm_shim_pump()` escapes task loops with `longjmp`. Anything added to `csrc/`
  that can be called from inside a task body must hold no resource across a
  blocking primitive.
