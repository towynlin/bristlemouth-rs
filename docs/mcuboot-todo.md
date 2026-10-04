# MCUboot todo

Running `bm-devkit` firmware under the MCUboot bootloader that C nodes carry,
and updating it over Bristlemouth DFU, as dependency-ordered cards sized for
one agent each. Same card format as `docs/services-todo.md`.
`docs/bcmp-port-todo.md`'s "The shared contract" applies where a card ports C.

Why: a deployed module is potted. USB and SWD are not reachable; the only way
in is Bristlemouth DFU of a `.dfu.bin` from a Spotter's SD card, through a
Bridge, into MCUboot's secondary slot. `bm-devkit` today links at
`0x08000000`, overwrites the bootloader, and has `NoDfu`.

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
| Vendoring bm_protocol | Never. The bootloader comes by path (I1). |
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
   (`app_main.c:57-78`). The image and its 40-byte TLV area must fit in the
   slot ahead of the trailer and MCUboot's swap status area; I1 computes the
   limit and L1 puts it in `memory.x`.
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
   links memfault's U5 core. N1 checks a second app's map.
10. **Flash hooks** (`bm_dfu_wrapper.cpp`, `port_flash.c`): erase is whole
    8 KB pages, page-aligned, else failure; write and erase are verified by
    reading back; out-of-range is failure. bm_core erases the whole slot
    (`0xF2000`, 121 pages) before the first chunk (`dfu_client.c:297-302`).
    `fail_update_and_reset` only resets, with `RESET_REASON_UPDATE_FAILED`.
11. **Proven, not assumed.** MCUboot v1.9.0's `bootutil` is compiled on the
    host as an oracle (O1), with bm_protocol's configuration and flash map
    over RAM. What Rust writes to a slot is what `boot_set_pending` and
    `boot_set_confirmed` write; an image I1 builds is one `boot_go` boots.
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

### M1 — Header, TLVs and trailer

**Taken:** claude/mcuboot-m1-codecs

- **Rust:** new crate `bm-mcuboot`: `no_std`, no `alloc`, no dependencies,
  `forbid(unsafe_code)`. Explicit little-endian codecs for `image_header`,
  `image_version` and the TLV area; the trailer offsets for a slot of a
  given size and alignment (`boot_magic_off`, `boot_image_ok_off`,
  `boot_copy_done_off`, `boot_swap_info_off`); `set_pending` and
  `set_confirmed` over a small flash trait (`read`, `write`), each making
  the writes `boot_set_pending(0)` and `boot_set_confirmed()` make,
  including when they refuse (bad magic, already set).
- **Diff:** tests against O1, in a host-only test crate or `bm-wire-diff`:
  from the same starting flash, both sides' slot bytes are equal after
  `set_pending`, and after a swap and `set_confirmed`. Starting states: an
  erased trailer, a pending one, a confirmed one, a slot with no image.
- **Blocked by:** nothing.
- **Done:** the diff tests pass; `cargo build -p bm-mcuboot --target
  thumbv8m.main-none-eabihf`.

### I1 — Image tool

- **Rust:** new host crate `bm-image` in the root workspace; the SHA-256
  dependency lives here, not in `bm-mcuboot`. A library over bytes and a
  CLI:

  | Command | Output |
  |---|---|
  | `bm-image dfu <elf> [--key <ed25519.pem>] -o <out.dfu.bin>` | Contract 2's file. The binary is the ELF's loadable sections from `0x0800C200`, gaps `0xFF`. Version and git SHA come from the image's own `versionInfo_t` (contract 8), so the header cannot disagree with it; an ELF without one is an error. With `--key`, signed as contract 2 says; the key is a PEM as `imgtool keygen -t ed25519` writes, unencrypted. |
  | `bm-image unified <bootloader> <dfu.bin> -o <out.unified.bin>` | The bootloader, an ELF or a flat binary given by path (in a bm_protocol checkout, `preset-builds/bootloader/src/bootloader-bootloader.elf`), padded with `0xFF` to `0xC000`, then the `.dfu.bin` (`src/CMakeLists.txt:683-684`). |
  | `bm-image info <file>` | Header, TLVs (signed or not, and the key hash), version note, CRC-16 (kermit) and size: `BmDfuImgInfo`'s fields. |

  Rejects an image too large for the slot (contract 3) and states the limit.
- **Gold:** a small binary run through bm_protocol's `imgtool.py` with
  contract 2's arguments, once without a key and once with O1's test key;
  input and both outputs committed as test data with the commands;
  `bm-image`'s outputs are byte-identical (ed25519 signatures are
  deterministic). Header fields asserted
  against the C `hello_world` `.dfu.bin`'s first 32 bytes (`3db8f396
  00000000 0002 0000 b4da0300 00000000 000d0c00 d0b5d862 00000000`).
- **Oracle:** O1's `boot_go` boots a `bm-image` file from slot 1, and from
  slot 2 after `bm_mcuboot::set_pending`: unsigned on the unsigned build,
  signed on both. On the signing build an unsigned image, one signed with
  another key, and one with a flipped body byte are each refused; assert
  what `boot_go` does with slot 2 and which image it boots.
- **Blocked by:** M1.
- **Done:** gold and oracle tests pass in `cargo test`.

### L1 — Link for slot 1

- **Rust, `bm-devkit`:**

  | Change | Detail |
  |---|---|
  | `memory.x` | `FLASH` origin `0x0800C200`, length from contract 3. No other layout. |
  | Version note | A `#[used]` static in the C's layout (contract 8), in a section a linker fragment places at `0x0800C438`, padding after the vector table if cortex-m-rt's ends earlier. Fields from the values `DevkitIdentity` reports. |
  | Watchdog | Fed from a task spawned in `start`, period 1 s, before anything that can wait. |
  | Clocks | `config()` reaches the same tree from the bootloader's state (contract 6) as from reset. |
  | Runner | `.cargo/config.toml`'s runner builds the `.dfu.bin` with `bm-image`, programs it at `0x0800C000`, and attaches for defmt. `BM_IMAGE_KEY`, when set, is the `--key` path. `cargo run` keeps working and no longer touches the bootloader. |
  | CI | builds a `.dfu.bin` for `hello_world` and runs `bm-image info` on it. |

- **README:** replace "Flash layout, bootloader, no-init RAM"'s
  no-bootloader paragraph; add how to install the C bootloader once and how
  to restore C firmware.
- **Bench:** with the C bootloader installed, `cargo run --bin hello_world`
  boots, logs, stays up past 10 s (the IWDG), and appears in a Bridge's
  topology. `dfu-util` of a `.unified.bin` built from the Rust image boots
  the same.
- **Blocked by:** I1.
- **Done:** the bench line.

### N1 — No-init RAM and reset reason

- **Rust, `bm-devkit`:** `src/noinit.rs`: `bm_stack::NoInitRam` at
  `0x200BFE4C` through `RebootInfo::encode`/`decode`; a reset function that
  writes `resetReason` and its magic, then `SCB::sys_reset`; a read of the
  reset reason at boot, clearing the magic as `checkResetReason` does,
  logged over defmt. Volatile access to fixed addresses; `memory.x` already
  keeps the region out of `RAM`. Nothing writes `0x200BFE00` or the
  memfault bytes.
- **Check:** contract 9's addresses in a second bm_protocol app's map (for
  example `bm_devkit/bm_soft_module`, or the Bridge's, noting any
  difference).
- **Bench:** a value stored, then a reset with a reason, reads back with
  that reason.
- **Blocked by:** nothing.
- **Done:** builds for the thumb target; the bench line.

### S1 — The slot

- **Rust, `bm-devkit`:** `src/slot.rs`: `bm_stack::DfuSlot` on slot 2 over
  `embassy_stm32::flash` (blocking), per contract 10.

  | Method | Behaviour |
  |---|---|
  | `open`, `close`, `size` | `true`, `true`, `0xF2000` |
  | `erase` | page-aligned whole pages or `false`; verified; feeds the IWDG between pages |
  | `write` | any offset and length a DFU chunk can have, as `flashWrite` handles a tail shorter than 16 bytes; verified |
  | `read` | from the memory-mapped slot |
  | `set_pending_and_reset` | `bm_mcuboot::set_pending`, then N1's reset with `RESET_REASON_MCUBOOT` |
  | `set_confirmed` | `bm_mcuboot::set_confirmed` on slot 1 |
  | `fail_update_and_reset` | N1's reset with `RESET_REASON_UPDATE_FAILED` |

  `bm_devkit::node` builds a `Devkit` with the slot and N1's `NoInitRam` in
  place of `NoDfu`. `hello_world` logs DFU progress over defmt.
- **Measure:** the time to erase the slot, and that a neighbour does not
  time the node out while it is erased; record both in the README.
- **Bench:** a dev kit running Rust `hello_world` accepts a Rust
  `.dfu.bin` with a different git SHA from a Bridge, reboots into it, and
  the Bridge reports success.
- **Blocked by:** M1, L1, N1.
- **Done:** the bench line.

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

- **Docs:** results in `bm-devkit/README.md`; a "Releasing an image"
  section: the commands from source to `.dfu.bin` and `.unified.bin`, unsigned and signed.
- **Blocked by:** S1.
- **Done:** every row run and recorded; this file marked complete.

## Order

| Wave | Cards | Each needs |
|---|---|---|
| 1 | M1, N1 | nothing |
| 2 | I1 | M1 |
| 3 | L1 | I1 |
| 4 | S1 | M1, L1, N1 |
| 5 | B1 | S1 |

Cards within a wave can run in parallel.

## What the landed cards left for the rest

Landed: O1.

### O1 — `bm-mcuboot-sys`

`bm-mcuboot-sys/README.md` is the contract. For M1 and I1:

| Need | Where |
|---|---|
| The oracle | `bm_mcuboot_sys::lock(Build) -> Oracle`. One process-global lock for both builds; flash persists between locks, so start with `reset()`. |
| Flash | `Oracle::read(area, off, &mut buf)`, `read_area(area) -> Vec<u8>`, `write(area, off, &data)`, over `Area::{Bootloader, Primary, Secondary, Scratch}` with `offset()` and `size()`. `write` stores bytes with no flash semantics. |
| `bootutil` | `set_pending(permanent: bool)`, `set_confirmed()`, `swap_type()` (`swap_type::{NONE, TEST, PERM, REVERT, FAIL, PANIC}`), `boot_go() -> Result<Booted, Refusal>`. `Booted` is `image_off` (`0xC000`), `flash_dev_id` and the 32 header bytes. |
| The two builds | `Build::Unsigned` and `Build::Ed25519`, both linked into every binary, each with its own flash. No feature or environment variable. |
| The test key | Private: `bm-mcuboot-sys/testdata/test_ed25519_key.pem`, as `imgtool keygen -t ed25519` writes it. Public: `bm-mcuboot-sys/csrc/test_ed25519_pub_key.c`. |
| A signed example | `bm-mcuboot-sys/testdata/body.signed.dfu.bin`, from `body.bin` with contract 2's arguments; the command is in the README. Its TLV area is 144 bytes: SHA-256, `KEYHASH`, `ED25519`. |
| `imgtool` | `bm-mcuboot-sys/vendor/mcuboot/scripts/imgtool.py`; needs `cryptography`, `intelhex`, `click`, `cbor2`. bm_protocol's pixi environment has them. |

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
| Both builds in one binary, the signing build's symbols prefixed `ed25519_` by a generated force-included header | I1's tests need both builds in one `cargo test`; a cargo feature would give one per build of the crate. `build.rs` needs `nm`. |
| A bootutil `assert` returns `Refusal::Asserted` | The bootloader resets there; aborting would end the test process. |
| `flash_area_write` clears bits and compares, as `port_flash.c`'s read-back does | A second write to bytes that are not erased fails unless it changes nothing. A flash trait M1 compares against needs the same rule. |
| No bindgen | The API is this crate's own eight C functions. |
| `sha256` exported | Test images need a hash TLV and the crate has no Rust dependencies. I1 has its own. |

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
