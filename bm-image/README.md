# bm-image

Builds and reads the files a Bristlemouth node's MCUboot bootloader and DFU
take, as bm_protocol's build makes them (`docs/mcuboot-todo.md`, contracts 2,
3, 8 and 13). Host-only. A library over bytes (`src/lib.rs`) and a CLI.

```
cargo run -p bm-image -- dfu <elf> [--key <ed25519.pem>] -o <out.dfu.bin>
cargo run -p bm-image -- unified <bootloader> <dfu.bin> -o <out.unified.bin>
cargo run -p bm-image -- info <file>
```

| Command | Output |
|---|---|
| `dfu` | `imgtool sign --header-size 0x200 --align 16 --slot-size 0xF2000 --version <maj>.<min>.<rev>+<gitSHA> --pad-header [--key <pem>]` of the ELF's binary. Programmed at `0x0800C000`, or sent by DFU. |
| `unified` | The bootloader padded with `0xFF` to `0xC000`, then the `.dfu.bin`. Programmed at `0x08000000`. |
| `info` | Header, TLVs, whether the SHA-256 TLV matches, whether it is signed and the key hash, the version note, and `BmDfuImgInfo`'s `image_size`, `crc16`, `major_ver`, `minor_ver` and `gitSHA`. The signature is not checked. |

A failure prints one line to stderr, exits 1 and writes no file.

## What `dfu` reads from the ELF

| Item | Rule |
|---|---|
| The binary | Every section that is allocated, not `NOBITS` and not empty, at its load address, from `0x0800C200`; `0xFF` between sections. As `objcopy --gap-fill 0xFF -O binary`. |
| Load address | That of the `PT_LOAD` segment holding the section's file bytes, so `.data` is where the startup code copies it from. |
| Version | The first `versionInfo_t` in the binary, found by its magic as `tools/scripts/util/fwinfo.py` does. `ih_ver` is `maj.min.rev+gitSHA`; `0xFF.0xFF.0xFF`, which `git_version.cmake` writes for an untagged build, is `0.0.0`. |
| Key | PKCS#8 PEM, unencrypted, as `imgtool keygen -t ed25519` writes. `KEYHASH` is the SHA-256 of the public key's `SubjectPublicKeyInfo` DER; the signature is of the image's SHA-256 digest. |

Refused:

| Case | Why |
|---|---|
| No section starts at `0x0800C200`, or one is below it | Not linked for slot 1. |
| No `versionInfo_t` | The DFU client compares the running image's git SHA with the one offered (contract 8). |
| Header, body and TLVs longer than `0xF07B0` | `imgtool`'s limit with bm_protocol's arguments (contract 3). |

`unified` takes the bootloader as an ELF, laid out from `0x08000000` the same
way, or as a flat binary. It refuses one longer than `0xC000` and a second
file that is not an MCUboot image.

## Against bm_protocol

bm_protocol's `.bin` is `objcopy -O binary` with no `--gap-fill`
(`src/CMakeLists.txt:523`), so alignment gaps between sections are `0x00`
there and `0xFF` here. `bm-image dfu` of the C `hello_world` ELF at
`62d8b5d0` differs from the C `.dfu.bin` in 15 gap bytes and the hash;
`bm-image unified` of the C bootloader ELF and the C `.dfu.bin` is the C
`.unified.bin` byte for byte. Neither is a test: bm_protocol is not vendored.

## Test data

`testdata/`, made with bm_protocol's toolchain (Arm GNU Toolchain 13.2.rel1, binutils
2.41, from its pixi environment) and `imgtool.py` at `c657cbea`:

| File | From |
|---|---|
| `app.s`, `app.ld` | A vector table at `0x0800C200`, a `versionInfo_t` note at `0x0800C438` (0.13.12, git SHA `62d8b5d0`), code, `.data` loaded from flash, and `NOBITS` sections. |
| `app.elf` | below |
| `app.bin` | below; what `elf::flat` must produce |
| `app.dfu.bin`, `app.signed.dfu.bin` | below; what `dfu` must produce |

```
arm-none-eabi-as -mcpu=cortex-m33 -o app.o app.s
arm-none-eabi-ld -T app.ld --no-warn-rwx-segments -s -o app.elf app.o
arm-none-eabi-objcopy --gap-fill 0xFF -O binary app.elf app.bin
imgtool.py sign --header-size 0x200 --align 16 --slot-size 0xF2000 \
  --version 0.13.12+1658369488 --pad-header app.bin app.dfu.bin
imgtool.py sign --header-size 0x200 --align 16 --slot-size 0xF2000 \
  --version 0.13.12+1658369488 --pad-header \
  --key ../../bm-mcuboot-sys/testdata/test_ed25519_key.pem \
  app.bin app.signed.dfu.bin
```

`imgtool.py` is `bm-mcuboot-sys/vendor/mcuboot/scripts/imgtool.py`, the
commit bm_protocol's `src/third_party/mcuboot` pins. The key is
`bm-mcuboot-sys`'s test-only key.

## Tests

| File | Asserts |
|---|---|
| `tests/gold.rs` | `elf::flat` is `app.bin`; `dfu` is `app.dfu.bin` and, with the key, `app.signed.dfu.bin`; `info` on both; the CLI writes the same bytes. |
| `tests/oracle.rs` | On `bm-mcuboot-sys`'s `boot_go`: see below. |
| `src/image.rs` | The header is the C `hello_world`'s first 32 bytes; the limit. |

On the oracle, each image from slot 1 and from slot 2 after
`bm_mcuboot::set_pending`:

| Image | Unsigned build | Signing build |
|---|---|---|
| unsigned | boots | refused |
| signed with the test key | boots | boots |
| signed with another key | — | refused |
| signed, one body byte flipped | refused | refused |
| signed, `0xF07B0` bytes | — | boots |

Refused from slot 1 is `boot_go` returning 1. Refused from slot 2: `boot_go`
erases slot 2 and boots the image already in slot 1, and `boot_swap_type` is
then `NONE`. Booted from slot 2: the old image is in slot 2 and
`boot_swap_type` is `REVERT`.
