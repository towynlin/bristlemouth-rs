# Working in this repo

Read `README.md` first — especially the shim contract and the note on state
that outlives `bm_shim_reset`. Both describe real behavioural divergence from
the firmware, and both are easy to trip over.

## Layout

- `vendor/bm_core/` — the C submodule. **Never edit it from here.** Changes go
  upstream to `bristlemouth/bm_core`.
- `csrc/` — the platform layer bm_core leaves to the integrator, implemented
  deterministically. This is ours.
- `build.rs` — tiered source lists, the generated guarded header tree, bindgen.
- `tests/smoke.rs` — one or two tests per tier, proving each links and runs.
- `scripts/check_symbols.sh` — what `libbm_core.a` references but nothing
  defines. Everything left should be libc.

## Adding a bm_core module

1. Add the `.c` to the right tier in `build.rs`, and its header to `wrapper.h`.
2. `cargo build`, then `./scripts/check_symbols.sh`. A new non-libc undefined
   symbol is an integrator hook needing an implementation in `csrc/`.
3. Add a smoke test. If bm_core's gtest suite already asserts a value for the
   same input, use that value rather than inventing one.

## Conventions

- Tests touching the shim take the `SHIM` lock — the C state is process-global.
- Everything in `csrc/` must stay deterministic: no threads, no sockets, no
  wall clock, no randomness. A fuzz input has to replay byte-identically.
- `bm_shim_pump()` escapes task loops with `longjmp`. Anything added to `csrc/`
  that can be called from inside a task body must hold no resource across a
  blocking primitive.
