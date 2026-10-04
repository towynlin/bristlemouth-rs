# bm-mcuboot-sys

[MCUboot](https://github.com/mcu-tools/mcuboot) v1.9.0's `bootutil`, compiled
for the host with bm_protocol's bootloader configuration and its flash map
over RAM. The oracle for `docs/mcuboot-todo.md`: what Rust writes to a slot is
compared with what `boot_set_pending` and `boot_set_confirmed` write, and an
image Rust builds is one `boot_go` boots.

Host-only. `build.rs` panics for a `target_os = "none"` build. No bindgen:
`src/lib.rs` declares the eight functions of `csrc/bm_mcuboot.h` by hand.

```
git submodule update --init bm-mcuboot-sys/vendor/mcuboot   # not --recursive
cargo test -p bm-mcuboot-sys
```

`vendor/mcuboot` is `c657cbea`, the commit bm_protocol pins. Its own
submodules (mbedtls, esp-idf, the Cypress libraries) are not used; tinycrypt,
fiat and mbedtls-asn1 are in its tree.

## What is compiled

`build.rs` lists the sources bm_protocol's bootloader compiles
(`src/apps/bootloader/CMakeLists.txt:7-32`, `:77-99`), less its port files,
plus `csrc/`:

| File | Mirrors |
|---|---|
| `csrc/mcuboot_config/mcuboot_config.h` | bm_protocol `src/lib/mcuboot/include/mcuboot_config/mcuboot_config.h` |
| `csrc/mcuboot_config/mcuboot_logging.h` | logging off, as the bootloader builds |
| `csrc/mcuboot_config/mcuboot_assert.h` | see "Asserts" |
| `csrc/sysflash/sysflash.h` | `include/sysflash/sysflash.h` |
| `csrc/flash_map_backend/flash_map_backend.h` | `include/flash_map_backend/flash_map_backend.h` |
| `csrc/bm_mcuboot.c` | `port_flash.c`, `src/apps/bootloader/keys.c`, and the entry points |
| `csrc/test_ed25519_pub_key.c` | `imgtool getpub` of the test key |

`MCUBOOT_BOOT_MAX_ALIGN=16` is a compiler define, as in bm_protocol
(`src/CMakeLists.txt:124`, `:356`).

## Two builds

| `Build` | Archive | Signature type |
|---|---|---|
| `Unsigned` | `libbm_mcuboot.a` | none: an image needs a SHA-256 TLV |
| `Ed25519` | `libbm_mcuboot_ed25519.a` | `MCUBOOT_SIGN_ED25519` (`CONFIG_BOOT_SIGN_ED25519`, bm_protocol's `SIGN_IMAGES=1`) |

Both link into one binary. `build.rs` runs `nm` on the first archive and
compiles the second with a force-included header of
`#define sym ed25519_sym` for every external symbol, `csrc/` included. So
each build has its own flash array and its own `bootutil` statics. `NM`
overrides the tool.

## Flash

One 2 MiB array per build, erased to `0xFF` by `Oracle::reset`.

| `Area` | id | Offset | Size |
|---|---|---|---|
| `Bootloader` | 0 | `0x0` | `0xC000` |
| `Primary` (slot 1) | 1 | `0xC000` | `0xF2000` |
| `Secondary` (slot 2) | 2 | `0xFE000` | `0xF2000` |
| `Scratch` | 3 | `0x1F0000` | `0x10000` |

Offsets are from `0x08000000`. Sectors are 8 KiB, `flash_area_align` is 16.

What `bootutil` sees, as `port_flash.c`:

| Call | Behaviour |
|---|---|
| `flash_area_write` | Clears bits only (`dst &= src`), then fails unless the bytes read back equal to `src`, as `MCUBOOT_VERIFY_WE` does. Any offset and length inside the area. |
| `flash_area_erase` | Whole 8 KiB pages at a page offset, else -1. |
| read, write, erase | -1 outside the area. `port_flash.c` does not check erase's range. |

Not modelled: the STM32U5 refusing to program a quad-word that is not erased,
bank boundaries, power loss.

`Oracle::read` and `Oracle::write` are the test's access, not `bootutil`'s:
`write` stores bytes with no flash semantics.

## API

`lock(build)` takes the one process-global lock and returns an `Oracle`.
Flash persists between locks.

| Method | C | Result |
|---|---|---|
| `reset` | — | all of this build's flash to `0xFF` |
| `read`, `read_area`, `write` | — | panics outside the area |
| `set_pending(permanent)` | `boot_set_pending` | `Result<(), Refusal>` |
| `set_confirmed()` | `boot_set_confirmed` | `Result<(), Refusal>` |
| `swap_type()` | `boot_swap_type` | a `swap_type` constant |
| `boot_go()` | `boot_go` | `Booted { image_off, flash_dev_id, header }` |

`sha256` is tinycrypt's, for building a test image's hash TLV.

`Booted::image_off` is `br_image_off`, `0xC000`; `header` is the 32 bytes of
`*br_hdr`. `boot_go` returns `Refusal::Code(1)` when slot 1 holds no image it
accepts. A secondary slot whose image it refuses is erased.

### Asserts

bm_protocol's `assert` in the bootloader resets the MCU. Here
`mcuboot_assert.h`'s `assert` leaves the call with `longjmp` and the method
returns `Refusal::Asserted`. tinycrypt and fiat use libc's `assert`, which
aborts.

## Test data

| File | From |
|---|---|
| `testdata/test_ed25519_key.pem` | `imgtool.py keygen -k test_ed25519_key.pem -t ed25519`. **Test-only**; it signs nothing that ships. |
| `csrc/test_ed25519_pub_key.c` | `imgtool.py getpub -k testdata/test_ed25519_key.pem` |
| `testdata/body.bin` | bytes `0x00..=0x3F` |
| `testdata/body.signed.dfu.bin` | below |

```
imgtool.py sign --header-size 0x200 --align 16 --slot-size 0xF2000 \
  --version 1.2.3+4 --pad-header --key testdata/test_ed25519_key.pem \
  testdata/body.bin testdata/body.signed.dfu.bin
```

`imgtool.py` is `vendor/mcuboot/scripts/imgtool.py`; it needs `cryptography`,
`intelhex`, `click` and `cbor2`.
