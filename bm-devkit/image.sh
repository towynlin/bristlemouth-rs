#!/bin/sh
# `image.sh <elf>...`: each ELF's MCUboot images, next to it.
#
# | Output | When |
# |---|---|
# | `<elf>.dfu.bin` | always; signed with `$BM_IMAGE_KEY` when that is set |
# | `<elf>.unified.bin` | when `$BM_BOOTLOADER` names the bootloader, an ELF or flat binary |
#
# Prints each output's path and the version note `bm-image info` reads from it.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# From this directory `.cargo/config.toml` targets the Cortex-M33, so bm-image
# is built for the host explicitly.
host=$(rustc -vV | sed -n 's/^host: //p')
bm_image() {
    (cd "$here" && cargo run --quiet --locked --manifest-path ../Cargo.toml \
        --target "$host" -p bm-image -- "$@")
}
abs() {
    echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
}

for elf in "$@"; do
    elf=$(abs "$elf")
    if [ -n "${BM_IMAGE_KEY:-}" ]; then
        bm_image dfu "$elf" --key "$(abs "$BM_IMAGE_KEY")" -o "$elf.dfu.bin"
    else
        bm_image dfu "$elf" -o "$elf.dfu.bin"
    fi
    echo "$elf.dfu.bin"
    bm_image info "$elf.dfu.bin" | sed -n 's/^note: */  /p'
    if [ -n "${BM_BOOTLOADER:-}" ]; then
        bm_image unified "$(abs "$BM_BOOTLOADER")" "$elf.dfu.bin" -o "$elf.unified.bin"
        echo "$elf.unified.bin"
    fi
done
