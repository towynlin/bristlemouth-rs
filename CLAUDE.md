# Working in this repo

Read `bm-wire-sys/README.md` first — especially the shim contract and the note
on state that outlives `bm_shim_reset`. Both describe real behavioural
divergence from the firmware, and both are easy to trip over.

Then read `docs/c-divergences.md`. It lists every place bm_core's C does
something surprising that the Rust port deliberately reproduces. If a change
here makes a fuzzer fail, that file is the first place to look.

## The point of this repo

`bm-wire` is a pure Rust port of bm_core, intended to run as firmware on
Bristlemouth dev kits alongside nodes running the C/C++ firmware. Compatibility
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
- `bm-wire-diff/` — the differential harness. Host-only, depends on both other
  crates. One comparator per surface, shared by the fuzz targets and by
  ordinary `#[test]`s.
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
- `docs/c-divergences.md` — the upstream bug list.

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
cargo tree -p bm-wire                            # must show no dependencies
./bm-wire-sys/scripts/check_symbols.sh           # only libc may be unresolved
cd bm-wire && cargo fuzz run <target> corpus/<target> seeds/<target>
```

`cargo fuzz` needs nightly. `build.rs` adds `-fsanitize=address,undefined` to
the C under `CARGO_CFG_FUZZING`, matching cargo-fuzz on the Rust side — so the
fuzzers check the C for undefined behaviour as well as checking the port for
divergence. That is how divergence #6 was found.

One UBSan check is deliberately off: `-fno-sanitize=alignment`. Every BCMP
frame bm_core receives trips it, because `clear_ports_legacy` does a 32-bit
access at frame offset 26 — divergence #11. Leaving it on means aborting on the
first receive rather than finding anything. Do not widen that exemption without
a divergence entry saying why.

When a fuzzer finds a crash: `cargo fuzz tmin <target> <artifact>`, drop the
minimized file into `bm-wire/fuzz/seeds/<target>/`, and it becomes a permanent
regression test the next time `cargo test` runs.

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
- Everything in `bm-wire-sys/csrc/` must stay deterministic: no threads, no
  sockets, no wall clock, no randomness. A fuzz input has to replay
  byte-identically.
- `bm_shim_pump()` escapes task loops with `longjmp`. Anything added to `csrc/`
  that can be called from inside a task body must hold no resource across a
  blocking primitive.
