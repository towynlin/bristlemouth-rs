# Working in this repo

Read `bm-wire-sys/README.md` first — especially the shim contract and the note
on state that outlives `bm_shim_reset`. Both describe real behavioural
divergence from the firmware, and both are easy to trip over.

## Layout

A cargo workspace. The eventual goal is differential fuzzing: `bm-wire` is
proven bit-compatible with the C in `bm-wire-sys` rather than assumed to be.

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
    defines. Everything left should be libc. Run it from the workspace root;
    it looks for the archive under the shared `target/`.
- `bm-wire/` — the pure, safe, idiomatic Rust port. Still a stub.
  - `fuzz/` — a `cargo fuzz` crate, its own workspace (excluded from the outer
    one). `fuzz_target_1` is still the generated stub.

## Adding a bm_core module

1. Add the `.c` to the right tier in `bm-wire-sys/build.rs`, and its header to
   `bm-wire-sys/wrapper.h`.
2. `cargo build`, then `./bm-wire-sys/scripts/check_symbols.sh`. A new non-libc
   undefined symbol is an integrator hook needing an implementation in
   `bm-wire-sys/csrc/`.
3. Add a smoke test. If bm_core's gtest suite already asserts a value for the
   same input, use that value rather than inventing one.

## Fuzzing

`cargo fuzz` needs a nightly toolchain. Targets live in
`bm-wire/fuzz/fuzz_targets/`; from `bm-wire/`, `cargo fuzz list` and
`cargo fuzz run <target>`. Because `fuzz/` is its own workspace, adding a
dependency there means editing `bm-wire/fuzz/Cargo.toml`, not the root
manifest.

## Conventions

- Tests touching the shim take the `SHIM` lock — the C state is process-global.
- Everything in `bm-wire-sys/csrc/` must stay deterministic: no threads, no
  sockets, no wall clock, no randomness. A fuzz input has to replay
  byte-identically.
- `bm_shim_pump()` escapes task loops with `longjmp`. Anything added to `csrc/`
  that can be called from inside a task body must hold no resource across a
  blocking primitive.
