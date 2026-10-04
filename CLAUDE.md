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

A cargo workspace.

- `bm-wire/` — the port. `no_std`, no `alloc`, `forbid(unsafe_code)`, and one
  dependency: `cbor2`, at `default-features = false`, for the config chain's
  CBOR values. **Must never depend on `bm-wire-sys`**, in any configuration:
  that keeps the host-only oracle out of firmware builds. The `std` feature is
  for tests and fuzzing only, and must not forward to `cbor2`.
  - `src/cbor/parser.rs` — tinycbor's parser, ported: `Value` is
    `CborValue`. For decoders whose outcomes are tinycbor's error codes and
    item counting rather than CBOR's.
  - `src/service/` — the services' bodies: `sys_info`, `config_map`,
    `power_info`, `metrics`, each encode and decode, and each built-in's
    handler; `power_info.rs` also holds `power_info_service.c`'s callback
    queue (`Callbacks`); `table.rs` is
    `bm_service.c`'s list and request walk (`ServiceTable`), the request and
    reply headers, and echo's handler; `request.rs` is
    `bm_service_request.c`'s list, id counter and 500 ms sweep (`Requests`).
  - `fuzz/` — a `cargo fuzz` crate, its own workspace. Targets are ~6 lines
    each; the work is in `bm-wire-diff`.
  - `fuzz/seeds/` — committed seed corpora, one directory per target, replayed
    by `cargo test`. `fuzz/corpus/` is gitignored.
- `bm-stack/` — the node. `no_std`, no `alloc` (except behind the test-only
  `mock` feature), and the only crate that knows about time or I/O.
  - `src/port.rs` — the seams bm_core leaves to the integrator, as traits
    rather than link-time symbols, including the DFU update slot and the
    no-init RAM that carries an update across a reset.
  - `src/dfu.rs` — DFU on a node, client and host: the machine, its outbox,
    the resets it asks for and the finish callbacks it reports.
  - `src/config.rs` — `config_init` and `save_config`, the two functions of
    `bcmp/configuration.c` that touch storage; the store itself is
    `bm_wire::configuration`.
  - `src/node.rs` — `Node::on_frame`, `on_tick` and `on_expiry` are
    synchronous and take the current time; `Node::run` is the only async code.
    Each has a `_with` twin that reports `Event`s. The three timers are
    bm_core's: the 10 s heartbeat, `packet.c`'s 150 ms expiry sweep and
    `bm_service_request.c`'s 500 ms sweep (`on_service_expiry`), which must
    not be put on a grid of the port's own (divergence #22). DFU runs
    from `next_dfu_transmission`, as bm_core's runs on its own task.
    Pub/sub (`subscribe`, `unsubscribe`, `publish`, `Event::Publication`)
    holds UDP port 4321; the subscription table is `bm_wire::pubsub::Subscriptions`.
    `spotter_log` and `spotter_tx_data` wrap `publish`; their bodies are
    `bm_wire::spotter`.
  - `src/service.rs` — `Services`, the application's service handlers, a
    `Node`'s `S`. `Node::register_service`, `register_echo_service` and
    `unregister_service` list them; `on_frame` answers a request in
    `Owed::reply`. `Node::service_request` asks another node's service;
    the answer is `Event::ServiceReply` or `Event::ServiceTimeout`, or for
    `Node::power_info_request`, `Event::PowerInfoReply` (divergence #96).
    A node lists the metrics service at construction unless
    `Services::METRICS` is false, as `bristlemouth_init` does.
  - `src/utc_time.rs` — the Spotter's `spotter/utc-time`, which C nodes set
    their RTC from (bm_protocol app code, not bm_core): `decode` and
    `UtcTimeSetter`, for an `App`.
  - `src/app.rs` — `App`, application code `Node::run_app` runs in the
    node's loop: a cancel-safe `ready` arm, then `act` with `&mut Node`.
  - `src/channel.rs` — behind the `channel` feature: `Channels`, an
    `embassy-sync` `NodeHandle` for an application in a task of its own:
    owned `Command`s in, owned `Notification`s out, run by `ChannelApp`, an
    `App`. A service request command reports
    `Notification::ServiceRequested` with its id, then the reply, timeout or
    power_info notification. Off by default, so a single-task firmware
    carries neither `embassy-sync` nor `heapless`.
  - `src/mock.rs` — a scripted PHY that also drives embassy's mock clock.
    `src/mock/frames.rs` builds the peer frames a script feeds it; use it
    rather than a local builder.
  - `examples/hello_node.rs` — a node on the mock PHY through the public API
    only: a scripted neighbour, an `App` that pings it, publishes, logs to
    the Spotter and asks for its sys_info. Panics on a wrong
    outcome, so CI runs it.
- `bm-phy-adin2111/` — `bm_stack::Phy` for the ADIN2111 over OPEN Alliance TC6
  SPI, on the per-port frame I/O of
  [embassy-rs/embassy#7024](https://github.com/embassy-rs/embassy/pull/7024),
  merged to embassy `main` and awaiting an `embassy-net-adin1110` release.
  Each frame's port rides in `PacketMeta::id`. **Its own workspace**, because
  it pins embassy to git and `embassy-time-driver` carries
  `links = "embassy-time"`, so a git embassy and a crates.io embassy cannot
  share a dependency graph. Not built by the root `cargo test`. Its `Runner`
  must be spawned by the firmware — it owns the SPI bus, and until it runs no
  frame moves.
- `bm-devkit/` — board support for the dev kit's mote (STM32U575CI,
  ADIN2111 on SPI3, W25Q64JV NOR flash on SPI2): `start` powers and brings up
  the ADIN2111, sets up the flash and starts the RTC, `node` builds a
  `Devkit` node with the chip's node id, the binary's name as `app_name`, the
  RTC and its config partitions in flash; `src/bin/bringup.rs`
  runs one and logs the config keys it loaded; `src/bin/hello_world.rs` is
  the hello-world app: subscribes to `spotter/*`, sends `hello world` with
  `spotter_log` every 10 s, sets the RTC from `spotter/utc-time`, and
  lists echo, sys_info and config_map after metrics, as a C dev kit does.
  **Its own workspace**, for
  bm-phy-adin2111's reason, with `Cargo.lock` on the same embassy commit;
  `.cargo/config.toml` sets the thumb target and a `probe-rs run` runner.
  `README.md` is the record of bm_protocol's BSP (pins, clocks, ADIN2111
  sequence, node id, config flash layout), with file and line references —
  bm_protocol is not vendored, so read it there rather than re-deriving it.
  - `src/w25.rs` — the flash driver, bm_protocol's `spiflash::W25`, over
    `embedded-hal` traits only.
  - `src/rtc.rs` — `DevkitRtc`, `bm_stack::Rtc` over the STM32 RTC on LSE,
    as bm_protocol's `stm32_rtc.c`.
  - `src/storage.rs` — `FlashConfigStorage`, `bm_stack::ConfigStorage` at
    bm_protocol's partition offsets.
- `bm-wire-diff/` — the differential harness. Host-only. One comparator per
  surface, shared by the fuzz targets and by ordinary `#[test]`s.
  - `src/frames.rs` — BCMP, UDP and publication frames a peer sends, for
    comparators to inject; `bm_wire::bcmp::tx::build` into a `Vec`. Use it rather than a local
    builder.
  - `tests/node_frames.rs` — compares whole frames `bm-stack` builds against
    the ones bm_core emits for the same question from the same identity.
  - `src/node_udp.rs`, `tests/node_udp.rs` — UDP through `bm_stack::Node`
    against the oracle's whole stack: sends, relays, and delivery to bound
    ports and to `bm_middleware_rx`; `bm_pub_wl` against `Node::publish`.
  - `src/spotter.rs`, `tests/spotter.rs` — `spotter_log` and
    `spotter_tx_data` against `Node::spotter_log` and `Node::spotter_tx_data`.
  - `src/pubsub.rs`, `tests/pubsub.rs` — `bm_sub_wl`, `bm_unsub_wl`,
    `bm_pub_wl` and `bm_handle_msg` against `Node`'s pub/sub, with one Rust
    node mirroring the oracle's subscription and resource lists for the life
    of the process.
  - `src/services.rs`, `tests/services.rs` — `bm_service.c`, echo,
    sys_info, config_map, power_info, metrics and `bm_service_request.c`
    against `Node`'s services and requests, with one Rust node mirroring the
    oracle's service list, request list, subscriptions and resources for the
    life of the process, and a config store and metrics components both
    sides empty at each input.
  - `tests/service_request_failures.rs` — `bm_service_request`'s failure
    paths on the oracle alone (divergence #91), which the Rust node's
    ceilings refuse earlier.
  - `src/service_codecs.rs` — the service bodies against
    `bm_common_messages`, in-process; what it skips is listed at the top.
  - `src/metrics_codec.rs` — the metrics body against `metrics_reply_msg.c`,
    in-process; destinations are compared after every decode, failed or not.
  - `testdata/` — pcaps from C dev kits. `hello-pub-card-h0.pcap` is card
    H0's; `tests/capture_h0.rs` documents it and asserts the header fields
    where deployed nodes differ from `bm_linux.c` (divergence #70).
    `src/pcap.rs` reads them.
- `bm-wire-sys/` — raw FFI bindings to the real bm_core C. The oracle.
  - `vendor/bm_core/` — the C submodule. **Never edit it from here.** Fixes go
    upstream to `bristlemouth/bm_core`.
  - `csrc/` — the platform layer bm_core leaves to the integrator, implemented
    deterministically. This is ours.
  - `build.rs` — tiered source lists, the generated guarded header tree,
    bindgen. `T2_RELEASE` compiles `cbor_service_helper.c` and four message
    codecs with `NDEBUG`, as a release build does (divergences #82, #88).
  - `scripts/check_symbols.sh` — what `libbm_core.a` and
    `libbm_core_release.a` reference but nothing defines; everything left
    should be libc. Run from the workspace root. `--check` fails on anything
    outside its libc allowlist, which deliberately omits `rand` and `time`, so
    a non-deterministic reach from `csrc/` trips it.
- `bm-mcuboot-sys/` — MCUboot v1.9.0's `bootutil` with bm_protocol's
  configuration, over RAM flash. The oracle for slot contents and images.
  Host-only; `bm-wire` and `bm-stack` must never depend on it. `README.md`
  is its contract.
  - `vendor/mcuboot/` — the C submodule. **Never edit it.** Check it out
    without `--recursive`: its own submodules are not used.
  - `csrc/` — `mcuboot_config.h` and the port headers, citing the bm_protocol
    lines they mirror; `bm_mcuboot.c`, the flash map over RAM and the entry
    points. Deterministic, as `bm-wire-sys/csrc/`.
  - `build.rs` — compiles everything twice: `libbm_mcuboot.a` with no
    signature type and `libbm_mcuboot_ed25519.a` with `MCUBOOT_SIGN_ED25519`,
    every symbol of the second prefixed `ed25519_`.
  - `src/lib.rs` — `lock(Build)` returns an `Oracle`: `reset`, `read`,
    `write`, `set_pending`, `set_confirmed`, `swap_type`, `boot_go`.
  - `testdata/` — `test_ed25519_key.pem`, a test-only private key whose
    public half the signing build trusts, and an image `imgtool` signed
    with it.
- `docs/c-divergences.md` — the upstream defect list.
- `docs/hello-world-todo.md` — the plan for a Rust hello-world app on a dev
  kit (UDP, pub/sub, `spotter_log`, board support). **Complete and closed; no
  work there.** Kept as documentation of what was built and why.
- `docs/services-todo.md` — the plan for `bm_service*.c` and the built-in
  services (echo, sys_info, config_map, power_info, metrics). **Complete and
  closed; no work there.** Kept as documentation of what was built and why.
- `docs/mcuboot-todo.md` — the plan for running `bm-devkit` firmware under
  the C nodes' MCUboot bootloader and updating it over Bristlemouth DFU.
  **Open; work comes from here.**
- `docs/bcmp-port-todo.md` — the BCMP port. Complete and closed to new
  cards; its shared contract is the record of the porting rules.
- `docs/embassy-port-tracking-prompt.md` — the brief that produced
  embassy#7024, which made `embassy-net-adin1110` report the ingress port and
  take an egress port per frame. Merged; kept as the record of the design.

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
cd bm-phy-adin2111 && cargo test                           # own workspace, needs network
cd bm-phy-adin2111 && cargo build --target thumbv8m.main-none-eabihf
cd bm-devkit && cargo build && cargo build --release      # own workspace, thumb only
cargo +1.97 check --workspace --all-targets                # the declared MSRV
cargo tree -p bm-wire                                      # only cbor2, serde, serde_core
./bm-wire-sys/scripts/check_symbols.sh --check             # only libc may be unresolved
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features \
  -p bm-wire -p bm-stack -p bm-wire-diff -p bm-mcuboot-sys # -D warnings, as CI does
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
