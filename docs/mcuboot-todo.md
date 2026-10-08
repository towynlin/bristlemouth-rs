# MCUboot todo

Running `bm-devkit` firmware under the MCUboot bootloader that C nodes carry,
and updating it over Bristlemouth DFU, as dependency-ordered cards sized for
one agent each. Same card format as `docs/services-todo.md`.
`docs/bcmp-port-todo.md`'s "The shared contract" applies where a card ports C.

Why: a deployed module is potted. USB and SWD are not reachable; the only way
in is Bristlemouth DFU of a `.dfu.bin` from a Spotter's SD card, through a
Bridge, into MCUboot's secondary slot. `bm-devkit` links for slot 1 and
takes updates into slot 2.

| In scope | Source |
|---|---|
| Linking for slot 1, behind the C bootloader | bm_protocol `src/CMakeLists.txt:72-135`, `src/bsp/common/linker/bs_stm32u575.ld` |
| What the bootloader leaves running | `src/apps/bootloader/app_main.c`, `src/bsp/bootloader/Core/Src/{main,iwdg}.c` |
| The image: header, TLVs, version note | MCUboot v1.9.0 `scripts/imgtool`, `boot/bootutil/include/bootutil/image.h`; `cmake/git_version.cmake`, `src/lib/common/version.h` |
| `.dfu.bin` and `.unified.bin`, unsigned and ed25519-signed | `src/CMakeLists.txt:372-400`, `:570-690` |
| The slot trailer: pending, confirmed | MCUboot `boot/bootutil/src/bootutil_public.c` |
| DFU's flash hooks | `src/lib/drivers/bm_dfu_wrapper.cpp`, `src/lib/mcuboot/port_flash.c`, `src/lib/drivers/stm32_flash_u5.c` |
| No-init RAM | `bs_stm32u575.ld` `.noinit`, `src/lib/common/reset_reason.c`, `bootloader_helper.c`, bm_core `bcmp/dfu_core.c:27` |

Out of scope:

| Item | Why |
|---|---|
| A bootloader in Rust | Deployed nodes have the C one; it is used unchanged. |
| Encrypted images (`ENCRYPT_IMAGES`, x25519) | `CMakePresets.json` does not enable it. Signing is in scope: contract 13. |
| A no-bootloader link at `0x08000000` | One layout, the deployed one. |
| Vendoring bm_protocol | Never. The bootloader comes by path (`bm-image unified`). |
| DFU host on a dev kit (image in the W25 `dfu` partition) | The Bridge is the host. `bm-devkit/README.md`, "DFU image locations". |
| Memfault reboot tracking and coredumps | Rust firmware has no memfault. Its no-init bytes are left untouched. |
| `update confirm`, `mcuboot_cli.c` | USB console. |

## Working a card

Before starting:

1. Pick a card whose **Blocked by** is "nothing" and that has no
   **Taken** line. Add `**Taken:** <branch>` under its title as the first
   commit of the branch, so parallel agents do not collide. If the card turns
   out to be two, split it here in that commit.
2. Read "MCUboot contract" and "What the landed cards left for the rest".

One branch and one PR per card; open the PR without being asked
(`CLAUDE.md`, "Pull requests"). When the code is done and verified, edit
this file in a separate commit, the last of the branch:

| Section | Edit |
|---|---|
| The card | Delete it. Git history is the record. |
| What the landed cards left for the rest | Add what a remaining card needs: API shape, a limit or gap left open, and **the reason for any decision between options**. Delete entries no remaining card needs. Keep the heading's card list current. |
| Other cards | Remove the card from every **Blocked by**; write "nothing" where none remain. |
| Order | Remove it from the graph. |

New files, crates or verify commands go into `CLAUDE.md`'s layout and
"Verifying" sections in the card's code commits. bm_protocol facts (addresses,
file and line references) go into `bm-devkit/README.md`.

A card with a **Bench** line needs a dev kit and a probe. An agent without
them finishes the code, lists the bench steps in the PR description under
"not verified", and leaves the card open with a `**Bench pending:** <PR>`
line instead of deleting it.

The PR description reports, per `CLAUDE.md` "Writing style": what changed,
each verify command run, and what was not verified.

When the last card lands, mark this file "Complete and closed" as the other
plans are.

## MCUboot contract

Read from bm_protocol at `62d8b5d0` (bm_core v0.13.12) and its build of
`bm_devkit/hello_world` with the `CMakePresets.json` bootloader preset.

1. **Flash map** (`src/CMakeLists.txt:113-124`, `port_flash.c:25-57`).
   Internal flash, 8 KB pages, erased value `0xFF`, write alignment 16
   (`MCUBOOT_BOOT_MAX_ALIGN`; the U5 programs quad-words).

   | Area | Address | Size |
   |---|---|---|
   | bootloader | `0x08000000` | `0xC000` |
   | slot 1 (primary) | `0x0800C000` | `0xF2000` |
   | slot 2 (secondary) | `0x080FE000` | `0xF2000` |
   | scratch | `0x081F0000` | `0x10000` |

   Slot 2 crosses the bank boundary at `0x08100000`; `stm32_flash_u5.c`
   erases per bank.
2. **The image** is `imgtool sign --header-size 0x200 --align 16 --slot-size
   0xF2000 --version <maj>.<min>.<rev>+<git sha, decimal> --pad-header`
   with no `--pad`, and `--key <ED25519_KEY_FILE>` when `SIGN_IMAGES` is 1
   (`src/CMakeLists.txt:570-590`). Unsigned: So: a 32-byte
   `image_header` (`ih_magic` `0x96f3b83d`, `ih_load_addr` 0, `ih_hdr_size`
   `0x200`, `ih_protect_tlv_size` 0, `ih_img_size`, `ih_flags` 0, `ih_ver`),
   `0xFF` to `0x200`, the binary, then a TLV area: info magic `0x6907`, total
   length 40, one TLV type `0x10` (SHA-256 of header and body). No trailer in
   the file. The C build's file is 512 + image + 40 bytes. Signed adds a
   `KEYHASH` TLV (`0x01`, SHA-256 of the public key) and an `ED25519` TLV
   (`0x24`, the signature of the SHA-256 digest), as imgtool's `image.py`
   writes them.
3. **Link address.** The vector table is at `0x0800C200`: the bootloader
   sets `VTOR` and loads SP and PC from `0x0800C000 + ih_hdr_size`
   (`app_main.c:57-78`). Header, body and TLV area together are at most
   `0xF07B0` bytes, the limit `imgtool` enforces for a bm_protocol build
   and the strictest of the three M1 found (below); chosen so that any
   image built here is one bm_protocol's tooling accepts. With the 512-byte
   header and the signed TLV area's 144 bytes, the body is at most
   `0xF0520`, which is `memory.x`'s `FLASH` length (L1).
4. **The bootloader validates slot 1 on every boot**
   (`MCUBOOT_VALIDATE_PRIMARY_SLOT`). A bare ELF programmed at `0x0800C200`
   does not boot; `boot_go` fails and the bootloader panics. Every flashing
   path writes a whole image from `0x0800C000`.
5. **The bootloader starts the IWDG** (`MX_IWDG_Init`: prescaler 32, reload
   4095, about 4.1 s on LSI) and it cannot be stopped. The application must
   feed it from its first second, including through a slot erase. The C
   feeds it from its lowest-priority task (`src/lib/common/watchdog.c`).
6. **The bootloader leaves the clock tree configured**: MSI and PLL on,
   SYSCLK from the PLL (`src/bsp/bootloader/Core/Src/main.c`
   `SystemClock_Config`), after `HAL_DeInit`, which does not reset RCC.
   `embassy_stm32::init` runs from that state, not from reset.
7. **Swap with scratch, test then confirm.** `boot_set_pending(0)` marks
   slot 2 for a test swap; MCUboot swaps on the next boot; the new image
   calls `boot_set_confirmed()`, or the next reset swaps back. bm_core's
   client confirms after the host acknowledges boot-complete
   (`bcmp/dfu_client.c:551-589`), which `bm_wire::bcmp::dfu_client` already
   does through `DfuSlot::set_confirmed`. An image that hangs before then is
   reset by the IWDG and reverted.
8. **Three places carry the version and must agree**, or the update is
   reverted (`dfu_client.c:554`, `git_sha() == reboot_info.gitSHA`):

   | Where | Read by |
   |---|---|
   | `Identity::device_info().git_sha`, `ver_major`, `ver_minor` | the running node, after the swap |
   | `versionInfo_t` in the image (`version.h`; magic `0xDF7F9AFDEC06627C`, then `gitSHA`, `maj`, `min`, `rev`, `hwVersion`, `flags`, `versionStrLen`, `versionStr[96]`, packed) inside an ELF note named `VERSION`, type `0x10` | tools that scan a `.dfu.bin` for the magic (`tools/scripts/util/fwinfo.py`, `tools/scripts/dfu/bm_load_img_to_flash.py`) to fill `BmDfuImgInfo` |
   | `ih_ver` in the MCUboot header | MCUboot logs only (no downgrade prevention configured) |

   In the C image the note follows the vector table: `0x0800C438`, file
   offset `0x438`. The Spotter is a conduit and checks no version; only the
   updating mote does. The Bridge takes `gitSHA`, `major_ver` and
   `minor_ver` from the Spotter's `dfu_start` message
   (`src/lib/bm_ncp/ncp_dfu.cpp:124-127`); how the Spotter reads them from
   the file is not in bm_protocol, so the note goes at the C's offset.
9. **No-init RAM**, top 512 bytes of RAM from `0x200BFE00`, in
   `bm_mote_v1.0-hello_world-dbg.elf.map`:

   | Address | Symbol | Rule for Rust |
   |---|---|---|
   | `0x200BFE00` | `ulBootloaderMagic`, u32 | The bootloader reads and zeroes it on every boot; `0xB8278F6D` jumps to the ROM bootloader. Never write it. |
   | `0x200BFE04` | `resetReason`, u32 enum | Write with the magic before a reset, as `resetSystem` does: `RESET_REASON_MCUBOOT` 4, `RESET_REASON_UPDATE_FAILED` 6. |
   | `0x200BFE08` | `ulResetReasonMagic`, `0xB8278F7D` | As above. |
   | `0x200BFE0C` | memfault `s_reboot_tracking`, `0x40` bytes | Leave untouched. |
   | `0x200BFE4C` | `client_update_reboot_info`, 18 bytes | `bm_wire::bcmp::dfu_core::RebootInfo::encode`/`decode`. The old image writes it, the new one reads it, so C and Rust must use this address. |

   The order is the linker script's; the addresses hold for any app that
   links memfault's U5 core; the Bridge's map agrees.
10. **Flash hooks** (`bm_dfu_wrapper.cpp`, `port_flash.c`): erase is whole
    8 KB pages, page-aligned, else failure; write and erase are verified by
    reading back; out-of-range is failure. bm_core erases the whole slot
    (`0xF2000`, 121 pages) before the first chunk (`dfu_client.c:297-302`).
    `fail_update_and_reset` only resets, with `RESET_REASON_UPDATE_FAILED`.
11. **Proven, not assumed.** MCUboot v1.9.0's `bootutil` is compiled on the
    host as an oracle (O1), with bm_protocol's configuration and flash map
    over RAM. What Rust writes to a slot is what `boot_set_pending` and
    `boot_set_confirmed` write; an image `bm-image` builds is one `boot_go` boots.
12. **A dev kit is recoverable; a potted node is not.** Bench cards run on
    dev kits. `dfu-util` of a C `.elf.unified.bin` at `0x08000000` restores
    one.

13. **Signing depends on the module, and this repo builds both.** Dev kits
    ship `hello_world` behind a bootloader that takes unsigned images, and
    their owners may flash any firmware. So do some third-party modules
    built on Sofar's line. Modules Sofar sells have a bootloader built with
    `SIGN_IMAGES=1` (`CONFIG_BOOT_SIGN_ED25519`), which refuses an image not
    signed with its key. bm_protocol passes the key as a path,
    `ED25519_KEY_FILE`, defaulting to the development key
    `src/apps/bootloader/ed25519_key.pem`; here it is a path given to
    `bm-image`. No private key is committed except a test-only one generated
    for this repo's tests.

## Cards

### B1 — On a bus

- **Bench matrix**, each from a Spotter's SD card through a Bridge:

  | From | To | Expect |
  |---|---|---|
  | C `hello_world` | Rust `hello_world` | success reported, Rust running |
  | Rust | Rust, new SHA | success |
  | Rust | C `hello_world` | success, C running |
  | Rust | Rust, same SHA | refused, `BmDfuErrSameVer` |
  | Rust | a Rust image that never confirms (a build with the confirm removed) | previous image running after the next reset |
  | Rust | a Rust image that hangs at start | reverted by the IWDG |
  | power removed mid-swap | — | swap completes on the next boot |

  Then on a dev kit whose bootloader is built with `SIGN_IMAGES=1` and
  bm_protocol's development key:

  | From | To | Expect |
  |---|---|---|
  | C, signed | Rust, signed with that key | success, Rust running |
  | Rust, signed | Rust, unsigned | previous image running; record the error the Bridge reports |
  | Rust, signed | C, signed | success, C running |

- **Docs:** results in `bm-devkit/README.md`. Images come from
  `bm-devkit/build.sh` (S1); the README's opening section has the commands.
- **Blocked by:** nothing.
- **Done:** every row run and recorded; this file marked complete.

## Order

| Wave | Cards | Each needs |
|---|---|---|
| 1 | B1 | nothing |

Cards within a wave can run in parallel.

## What the landed cards left for the rest

Landed: O1, N1, M1, I1, L1, S1.

### O1 — `bm-mcuboot-sys`

`bm-mcuboot-sys/README.md` is the contract.

Observed on the oracle, asserted in `bm-mcuboot-sys/tests/smoke.rs`:

| Case | Result |
|---|---|
| Slot 1 holds no image `boot_go` accepts | `Err(Refusal::Code(1))`, from `boot_validate_slot`; not `FIH_FAILURE` (-1) |
| Slot 2 pending, its image refused | slot 2 is erased; slot 1 boots |
| Signed image on the unsigned build | boots; the extra TLVs are ignored, and a changed signature still boots |
| After a test swap, before `set_confirmed` | `swap_type()` is `REVERT` |

Decisions:

| Decision | Reason |
|---|---|
| Both builds in one binary, the signing build's symbols prefixed `ed25519_` by a generated force-included header | `bm-image`'s tests need both builds in one `cargo test`; a cargo feature would give one per build of the crate. `build.rs` needs `nm`. |
| A bootutil `assert` returns `Refusal::Asserted` | The bootloader resets there; aborting would end the test process. |
| `flash_area_write` clears bits and compares, as `port_flash.c`'s read-back does | A second write to bytes that are not erased fails unless it changes nothing. A flash trait M1 compares against needs the same rule. |
| No bindgen | The API is this crate's own eight C functions. |
| `sha256` exported | Test images need a hash TLV and the crate has no Rust dependencies. |

Left open:

- M1's diff tests in `bm-wire-diff` would make it depend on
  `bm-mcuboot-sys`: all four lockfiles move, and `fuzz.yml`, which checks
  out `bm_core` only, would need `vendor/mcuboot`. A separate host-only
  test crate avoids both.
- The oracle does not model the U5 refusing to program a quad-word that is
  not erased, bank boundaries, or power loss mid-swap.
- CI and `session-start.sh` check out `vendor/mcuboot` without
  `--recursive`. A job that adds `submodules: recursive` would clone
  MCUboot's mbedtls, esp-idf and Cypress submodules.

### M1 — `bm-mcuboot`, `bm-mcuboot-diff`

`bm-mcuboot` has no SHA-256 and no signing; `bm-image` has both.

Limits of the slot's contents, from the start of the slot:

| Limit | Value | From |
|---|---|---|
| The bootloader's trailer | `0xF0900` (`0xF2000` - 5888) | `boot_trailer_sz`: 121 sectors × 3 × 16, plus 80 |
| `imgtool`'s check of header, body and TLVs | `0xF07B0` (`0xF2000` - 6224) | `image.py` `_trailer_size` with bm_protocol's arguments: its default 128 sectors × 3 × 16, plus 80 |
| The bootloader's own check | `ih_hdr_size + ih_img_size < 0xF2000` | `loader.c` `boot_is_header_valid`; v1.9.0 does not compare the image with the trailer |

The C build is held to `imgtool`'s.

Limits of the crate:

- `Trailer` has one `align` for `BOOT_MAX_ALIGN` and `flash_area_align`.
  8 and 32 follow the C by reading and have unit tests only; the oracle
  is 16.
- `Trailer::new` refuses a slot that is not a multiple of `align` or is
  smaller than the trailer's fields.
- TLV decoding refuses what `tlv.c` reads past (`src/tlv.rs`, module doc).
  Protected TLVs are unit-tested on hand-built bytes; there is no `imgtool`
  sample with one.
- No encryption fields.
- Flash read failures are unit-tested only: the oracle's reads cannot fail
  inside a slot.
- No cargo-fuzz target. `tests/trailer.rs` `random_trailers` is 5000
  trailers from a fixed seed.

Decisions:

| Decision | Reason |
|---|---|
| `bm-mcuboot-diff`, not `bm-wire-diff` | Only the root `Cargo.lock` moved, and `fuzz.yml` does not need `vendor/mcuboot`. |
| `Flash` has `erase`, which the card did not list | `boot_set_pending` erases the slot on a bad magic. |
| `FlashError` is a unit struct, not an associated type | `bootutil` reports every flash failure as `BOOT_EFLASH`. |
| `Trailer` is a value passed to free functions, with `Trailer::BM` | One `Flash` per slot serves both functions; `bm-image` uses the offsets without flash. |
| `Header::decode` checks nothing | The C casts the bytes. Validation is `boot_go`'s, on the oracle. |
| `SwapState::swap_type` is a `u8` | The C keeps a `swap_info` nibble of 0, which is none of its constants. |
| `permanent` is implemented | Three more lines, and diffed. S1 passes `false`. |

What `bootutil_public.c` does, reproduced and compared with the oracle
(`bm-mcuboot/src/trailer.rs`, module doc):

| Case | Result |
|---|---|
| `set_pending` or `set_confirmed` on a slot with no image | Neither reads the image. An empty slot 2 is marked pending; `boot_go` then erases it. |
| `set_pending(permanent)` on a slot pending a test | 0, nothing written: the swap stays a test. |
| `set_pending`, bad magic | Slot erased, `BOOT_EBADIMAGE` (3). |
| `set_pending`, `swap_info`'s block not erased | `BOOT_EFLASH` (1) with the magic written: the slot is pending, and `boot_swap_type` says `TEST`. |
| `set_confirmed`, no magic | 0, nothing written: an image that was not swapped in has nothing to confirm. |
| `set_confirmed`, bad magic | `BOOT_EBADVECT` (4). |
| `set_confirmed`, `image_ok` neither `0x01` nor erased | 0, nothing written; `boot_swap_type` is `NONE`, so the image stays. |
| A failed write | The oracle's flash has already cleared the bits. `RamSlot` does the same. |

### I1 — `bm-image`

`bm-image/README.md` is the contract. For L1:

| Need | Where |
|---|---|
| The runner's and CI's command | `bm-image dfu <elf> [--key <pem>] -o <out.dfu.bin>`, then `bm-image info <out.dfu.bin>`. Exit 1 with one line on stderr and no file on failure. |
| Running it from `bm-devkit/` | `cargo run --manifest-path ../Cargo.toml -p bm-image` there builds `bm-image` for the thumb target, which fails: cargo reads `bm-devkit/.cargo/config.toml` from the working directory. Run cargo from the repository root, or pass `--target` with the host triple. |
| What `dfu` wants of the ELF | A loaded section starting at exactly `0x0800C200` and none below it; a `versionInfo_t` anywhere in the loaded bytes. It is found by its magic, not by parsing ELF notes, and its offset is not checked. |
| Where the note is | `info` prints the magic's file offset. In the C image and in `bm-image/testdata/app.elf` it is `0x44C`: the note section at `0x438`, then the 12-byte note header and the 8-byte name. `testdata/app.s` and `app.ld` are a section layout that produces it. |
| `ih_ver` | `maj.min.rev+gitSHA` from the note. All three of `maj`, `min`, `rev` at `0xFF` become `0.0.0`, as `git_version.cmake` writes for an untagged build. |
| The body's limit | `0xF0520` signed, `0xF0588` unsigned: `bm_image::MAX_IMAGE_LEN` (`0xF07B0`) less the header and the TLV area. `memory.x`'s `0xF0520` fits both. |
| `unified` | The bootloader as an ELF with a section at `0x08000000`, or a flat binary; at most `0xC000` bytes. |

Observed, asserted in `bm-image/tests/oracle.rs`, from slot 1 and from slot 2
after `bm_mcuboot::set_pending`:

| Image | Unsigned bootloader | Signing bootloader |
|---|---|---|
| unsigned | boots | refused |
| signed with its key | boots | boots |
| signed with another key | not run | refused |
| signed, a body byte flipped | refused | refused |
| signed, `0xF07B0` bytes | not run | boots |

Refused from slot 2: `boot_go` erases slot 2 and boots slot 1's image, and
`boot_swap_type` is `NONE`. B1's "Rust, signed → Rust, unsigned" row is
this case.

Checked by hand against bm_protocol's build at `62d8b5d0`, not in a test:

| Input | Result |
|---|---|
| `bm-image unified` of the C bootloader ELF and the C `hello_world` `.dfu.bin` | the C `.unified.bin`, byte for byte |
| `bm-image dfu` of the C `hello_world` ELF | the C `.dfu.bin` except 15 gap bytes and the hash |
| `imgtool` on a body one byte past the limit, unsigned and signed | refused, `Image size (0xf07b1) + trailer (0x1850) exceeds requested size 0xf2000`; at the limit, accepted |

Decisions:

| Decision | Reason |
|---|---|
| Gaps between sections are `0xFF`, where bm_protocol's `objcopy -O binary` writes `0x00` | The card's rule: erased flash. The gaps are alignment padding nothing reads. |
| The ELF is read by `src/elf.rs`, not the `object` crate | ELF32 little-endian section and program headers are all it needs. |
| Sections, not segments, placed by the segment holding their file bytes | As `objcopy`; a linker may put the ELF headers in the first segment. |
| `ed25519-dalek` 3 with `pkcs8` and `pem`, `sha2` 0.11 | The PEM is parsed by the library that defines it. Only the root `Cargo.lock` moved. |
| `bm-image` depends on `bm-wire` | `info`'s CRC is `bm_wire::crc::crc16_ccitt`, the function the DFU client checks an image with. |
| `info` does not verify the signature | The file holds the key's hash, not the key. |
| The test ELF is assembled and committed, with its sources | CI has no ARM toolchain or `imgtool`. |
| Arguments parsed by hand | Three commands, two options. |

Limits:

- No encryption, no `--pad`, no protected TLVs, no Intel HEX.
- `info` reads a `.dfu.bin`, not a `.unified.bin`.
- An ELF with no section headers is refused.
- A signed gold image exists for the test key only; bm_protocol's
  development key is not in this repo.

### S1, for B1

`bm-devkit/README.md`, "DFU slot", is the record, bench results included:
Rust to Rust with a new SHA from a Spotter succeeded; the slot erase took
240 ms; the Bridge logged no `Neighbor <node id> lost` during it.

| Item | Use |
|---|---|
| `bm_devkit::slot::DevkitSlot` | `Board::slot`, the `D` of `Devkit`. `node` takes it. |
| An image that never confirms | Make `DevkitSlot::set_confirmed` do nothing: it is the only call to `bm_mcuboot::set_confirmed`. |
| Another git SHA | Another commit, or `BM_DEVKIT_GIT_SHA` forced in `build.rs` (L1). |
| Images | `cd bm-devkit && ./build.sh --release`: every binary's `.dfu.bin`; signed with `BM_IMAGE_KEY`; `.unified.bin` with `BM_BOOTLOADER`. Warns when the tree is dirty, since the image then carries HEAD's SHA. |
| Log lines | `dfu: slot 2 erased in N ms` on the Spotter console; `slot: erased … in N ms`; `slot: N bytes written` every 64 KiB; `slot: pending (code), resetting`; after the swap `reset reason: Mcuboot`, then `slot: image confirmed`; `dfu: 0x… from …` for each DFU message except payloads. |

Decisions:

| Decision | Reason |
|---|---|
| One `blocking_erase` per page, `watchdog::feed` before each | Contract 5; `watchdog::task` cannot run while `erase` holds the executor. |
| One `blocking_write` per quad-word | The tail pad and the read-back are per quad-word anyway; no call spans the bank boundary. |
| A write's short tail padded with `0xFF`, not the source's following bytes | The C reads past its buffer (`stm32_flash_u5.c:103`). The bytes are past the image. |
| An erase past the slot's end refused | `flash_area_erase` does not check `fa_size` and would erase scratch. bm_core never asks for it. |
| `set_pending`'s result logged, not acted on | `bm_dfu_wrapper.cpp:25` ignores `boot_set_pending`'s. |
| `read` is slot 2 | The C's host reads the W25 `dfu` partition, which is out of scope. |
| ICACHE not invalidated | Neither the bootloader nor `embassy_stm32::init` enables it. |
| The erase time also goes to the Spotter console | A potted node has no probe; `slot::take_erase_ms` is read by `hello_world`. |
| `build.sh` and `image.sh`, scripts beside `runner.sh` | Cargo has no post-build step, and a stale `.dfu.bin` beside a new ELF was sent once by mistake. |

Not run: a C image reading the `client_update_reboot_info` a Rust image
wrote at `0x200BFE4C` (`bm_devkit::noinit::NoInit`), or the reverse. B1's
C-to-Rust and Rust-to-C rows are the first runs that hand it over.
