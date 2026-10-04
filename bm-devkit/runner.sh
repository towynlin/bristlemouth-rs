#!/bin/sh
# cargo's runner (.cargo/config.toml): `runner.sh <elf> [probe-rs attach args]`.
#
# Builds <elf>.dfu.bin with bm-image, signed with $BM_IMAGE_KEY when that is
# set, programs it at slot 1, resets into the bootloader and attaches for
# defmt. The bootloader at 0x08000000 is not written. `probe-rs run` cannot do
# this: it programs the ELF, which has no MCUboot header, and the bootloader
# refuses a slot without one.
set -eu

CHIP=STM32U575CITxQ
SLOT1=0x0800C000

elf=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
shift
root=$(cd "$(dirname "$0")/.." && pwd)

# From the repository root: under bm-devkit/ cargo would read its
# .cargo/config.toml and build bm-image for the thumb target.
if [ -n "${BM_IMAGE_KEY:-}" ]; then
    key=$(cd "$(dirname "$BM_IMAGE_KEY")" && pwd)/$(basename "$BM_IMAGE_KEY")
    (cd "$root" && cargo run --quiet --locked -p bm-image -- dfu "$elf" --key "$key" -o "$elf.dfu.bin")
else
    (cd "$root" && cargo run --quiet --locked -p bm-image -- dfu "$elf" -o "$elf.dfu.bin")
fi

probe-rs download --chip "$CHIP" --binary-format bin --base-address "$SLOT1" "$elf.dfu.bin"
probe-rs reset --chip "$CHIP"
# `--no-catch-reset`: stay attached across a reset, the button's or the
# firmware's own; every DFU ends in one. Needs probe-rs 0.32 or later.
exec probe-rs attach --chip "$CHIP" --no-catch-reset "$elf" "$@"
