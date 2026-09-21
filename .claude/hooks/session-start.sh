#!/bin/bash
#
# Everything a fresh container needs before `cargo test` will even start.
#
# This is CLAUDE.md's "Setting up a fresh sandbox" section, executed instead of
# read. The repo's MSRV (1.97, embassy's number) is routinely newer than the
# stable toolchain a container image ships with, and until that is fixed
# *nothing* builds -- cargo refuses the whole workspace with "rustc N is not
# supported by the following package". The `bm_core` submodule has the same
# shape: without it `bm-wire-sys` does not compile and every differential test
# fails for the wrong reason.
#
# Idempotent, so re-running it on resume or clear costs nothing.

set -euo pipefail

# Local checkouts are the developer's own business; only fix up the containers
# that come up empty.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}"

# A SessionStart hook's stdout can be fed back to the agent as context, and
# rustup's "unchanged - rustc ..." summaries are not context anybody wants.
# Send everything below to stderr and leave stdout empty. (This is also why the
# script is synchronous: async mode needs stdout for its JSON handshake.)
exec 1>&2
log() { printf '[session-start] %s\n' "$*"; }

# --- the C oracle ----------------------------------------------------------
# bm-wire-sys/vendor/bm_core, plus its own nested submodules, which tier T2 and
# above of bm-wire-sys/build.rs need.
log 'checking out bm_core and its nested submodules'
git submodule update --init --recursive

# --- toolchains ------------------------------------------------------------
# Read the MSRV rather than hardcoding it, so this does not rot the next time
# embassy bumps and someone edits the manifest.
MSRV="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([0-9][0-9.]*\)".*/\1/p' Cargo.toml | head -n1)"
if [ -z "$MSRV" ]; then
  log 'could not read rust-version from Cargo.toml; has [workspace.package] moved?'
  exit 1
fi
log "MSRV is $MSRV"

# Three toolchains, and each earns its place:
#   stable  -- what `cargo test`, clippy and rustfmt run on
#   $MSRV   -- the `cargo +$MSRV check` job, and bm-phy-adin2111, whose own
#              rust-toolchain.toml pins it and which is linted there too
#   nightly -- cargo fuzz, and nothing else
# A stale cached stable is the whole reason this hook exists, but a failed
# update is survivable -- the version gate below decides whether what we have
# is good enough.
log 'updating stable'
rustup update --no-self-update stable \
  || log 'could not update stable; continuing with whatever is installed'

# This one is not survivable: without it there is no MSRV check, and
# bm-phy-adin2111's rust-toolchain.toml pins it, so that workspace will not
# build either.
log "installing $MSRV"
if ! rustup toolchain install "$MSRV" --profile minimal \
     --component rustfmt --component clippy --no-self-update; then
  log "could not install toolchain $MSRV."
  log 'If rustup says there is no such release, then the MSRV in Cargo.toml'
  log 'names a version that does not exist yet. That is a finding: report it.'
  exit 1
fi

# Fuzzing only, and fuzzing is the one part of CLAUDE.md's Verifying list that
# CI runs nightly rather than per-push. Not worth failing a session over.
log 'installing nightly'
rustup toolchain install nightly --profile minimal --no-self-update \
  || log 'nightly unavailable; `cargo fuzz` will not run'

# The two embedded targets, on both toolchains that build for them. A miss here
# surfaces clearly at the point of use, so warn rather than abort.
for toolchain in stable "$MSRV"; do
  log "adding embedded targets to $toolchain"
  rustup target add --toolchain "$toolchain" \
    thumbv7em-none-eabihf thumbv8m.main-none-eabihf \
    || log "could not add embedded targets to $toolchain; the thumb builds will fail"
done

# The case that is genuinely worth a human's attention: stable is still behind
# the MSRV, because the update above could not reach the network and the cached
# toolchain predates it. Everything else here is setup; this is a finding, and
# it is the reason the session cannot proceed rather than an inconvenience.
stable_version="$(rustup run stable rustc --version | awk '{print $2}')"
if [ "$(printf '%s\n%s\n' "$MSRV" "$stable_version" | sort -V | head -n1)" != "$MSRV" ]; then
  log "stable is $stable_version, older than the declared MSRV $MSRV, and it"
  log 'could not be updated. Nothing in the workspace will build. Report this:'
  log 'either the network is blocked or the MSRV claim needs revisiting.'
  exit 1
fi
log "stable is $stable_version, at or past the MSRV"

# --- fuzzing ---------------------------------------------------------------
# Best effort from here down. A card that never fuzzes should not fail to start
# because crates.io was unreachable.
if command -v cargo-fuzz >/dev/null 2>&1; then
  log 'cargo-fuzz already present'
else
  log 'installing cargo-fuzz'
  cargo install cargo-fuzz --locked \
    || log 'cargo-fuzz install failed; `cargo fuzz` will need installing by hand'
fi

# --- warm the caches -------------------------------------------------------
# The container image is snapshotted once this finishes, so anything fetched
# here is free for every later session. Fetch only: building bm-wire-sys means
# compiling the whole bm_core C tree through bindgen, which belongs in the
# session where its output can be read.
log 'fetching dependencies'
cargo fetch --locked || log 'root workspace fetch failed; cargo will retry in-session'
(cd bm-wire/fuzz && cargo fetch --locked) \
  || log 'fuzz workspace fetch failed; cargo will retry in-session'
# Its own workspace, its own pinned toolchain, and it pulls embassy from git.
(cd bm-phy-adin2111 && cargo fetch --locked) \
  || log 'bm-phy-adin2111 fetch failed; it needs network, see CLAUDE.md'

log 'ready'
