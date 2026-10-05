#!/bin/sh
# `build.sh [cargo build args]`: `cargo build`, then `image.sh` on every binary
# it built, so each ELF has its `.dfu.bin` (and `.unified.bin` with
# `$BM_BOOTLOADER`). For example:
#
#     ./build.sh --release
#     BM_IMAGE_KEY=key.pem ./build.sh --release --bin hello_world
#
# Warns when the tree has uncommitted changes: the image then carries HEAD's
# git SHA, and a node already running HEAD refuses it as `BmDfuErrSameVer`.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
cd "$here"

messages=$(mktemp)
trap 'rm -f "$messages"' EXIT
cargo build --locked --message-format=json-render-diagnostics "$@" >"$messages"
elfs=$(sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' "$messages")
[ -n "$elfs" ] || { echo "build.sh: cargo built no binary" >&2; exit 1; }

# shellcheck disable=SC2086 # paths from cargo's target dir, no spaces
./image.sh $elfs

if [ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]; then
    echo "build.sh: uncommitted changes; the images carry HEAD's git SHA" >&2
fi
