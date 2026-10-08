# bm-devkit

Board support for the Bristlemouth dev kit's mote. `src/lib.rs` brings the
board up; `src/bin/bringup.rs` runs a node on it, and `src/bin/hello_world.rs`
is the hello-world app.

```
cd bm-devkit && ./build.sh --release                  # ELFs and their .dfu.bin
cd bm-devkit && cargo run --release --bin hello_world  # program slot 1 with a probe, defmt over RTT
```

`build.sh` runs `cargo build` with its arguments, then `image.sh` on every
binary it built. Each ELF in `target/thumbv8m.main-none-eabihf/<profile>/`
gets, beside it:

| File | When | For |
|---|---|---|
| `<bin>.dfu.bin` | always; signed when `BM_IMAGE_KEY` names an ed25519 PEM | DFU from a Spotter's SD card; `probe-rs` at `0x0800C000` |
| `<bin>.unified.bin` | when `BM_BOOTLOADER` names the bootloader ELF or binary | bootloader and image together, at `0x08000000` |

It prints each image's version note. Only a commit changes the git SHA in
it: with uncommitted changes `build.sh` warns, and a node already running
HEAD refuses the image as `BmDfuErrSameVer`. A bare `cargo build` makes no
image and leaves an older `.dfu.bin` in place.

The images link for MCUboot slot 1 and need bm_protocol's bootloader on the
chip: "Installing the C bootloader", below. `cargo run` is `runner.sh`:

| Step | Command |
|---|---|
| Image | `image.sh <elf>` |
| Program slot 1 | `probe-rs download --binary-format bin --base-address 0x0800C000 <elf>.dfu.bin`. The bootloader's pages are not written. |
| Start | `probe-rs reset`, so the bootloader runs first |
| Log | `probe-rs attach --no-catch-reset <elf>`: logging continues across a reset. probe-rs 0.21 has no such flag: `cargo install probe-rs-tools --locked` |

Arguments after `cargo run ... --` go to `probe-rs attach`.

Its own workspace, beside `bm-phy-adin2111` and for the same reason (git
embassy, `links = "embassy-time"`). `Cargo.lock` pins the same embassy commit
as `bm-phy-adin2111/Cargo.lock`; move both together.

## Sources

Everything below was read from these trees; no other copy is in this repo.

| Tree | Commit | Path |
|---|---|---|
| `bristlemouth/bm_protocol` | `62d8b5d0ad5aa71a6a7b69b157a6ebba8fc77929` | BSP `src/bsp/bm_mote_v1.0/`; integration `src/lib/`; app `src/apps/bm_devkit/` |
| `towynlin/embassy` | `2735ead18a538aca3bc083614f91e9938b532046` | `examples/stm32u575/src/bin/spe_adin2111_http_server.rs`, a working ADIN2111 example on a dev kit |
| `embassy-rs/embassy` | `c7e712858f18d71fda040b603e068a502da8ad49` (the pin) | the same example, later revision, with per-port beacons |

The dev kit apps build with `-DBSP=bm_mote_v1.0`
(`src/apps/bm_devkit/BMDK_README.md:14`).

## MCU

| Item | Value | Source |
|---|---|---|
| Part | STM32U575CITxQ, LQFP48, 2 MB flash, 768 KB SRAM1–3 + 16 KB SRAM4 | `bm_mote_v1.0.ioc` `Mcu.UserName`, `Mcu.Package`; `STM32U575CITXQ_FLASH.ld` |
| embassy-stm32 feature | `stm32u575ci` | |
| probe-rs chip | `STM32U575CITxQ` | |
| Power supply | SMPS (`PWR_SMPS_SUPPLY`) | `.ioc` `PWR.PowerMode`. Not configured here: the bootloader selects it and it stays selected ("What the bootloader leaves"). |
| SYSCLK | 160 MHz: MSI range 0 (48 MHz), PLL1 M=3 N=10 R=1, MBOOST /4, AHB/APB /1, flash latency 4, VOS scale 1 | `Core/Src/main.c` `SystemClock_Config` |
| LSE | 32.768 kHz crystal on PC14/PC15, drive high | `.ioc`; `SystemClock_Config` |
| HSE | none; PH0/PH1 are GPIOs | `.ioc` |
| UID | at `UID_BASE` `0x0BFA0700` (`bsp.h`'s `STM32_UUID` `0x1FFF7A10` is stale and unused) | `stm32u575xx.h:1999`; `device_info.c:11` |

## Pins

From `Core/Inc/main.h:76-127` and `bm_mote_v1.0.ioc`. All GPIO outputs start
low (`Core/Src/gpio.c:56-66`).

| Pin | Name | Use |
|---|---|---|
| PA0 | `ADIN_RST` | ADIN2111 reset, active low |
| PA15 | `ADIN_CS` | ADIN2111 chip select, active low |
| PB3 / PB4 / PB5 | `ADIN_SCK` / `ADIN_MISO` / `ADIN_MOSI` | SPI3, AF6 |
| PB8 | `ADIN_INT` | ADIN2111 interrupt, EXTI8, falling edge, no pull |
| PH1 | `ADIN_PWR` | ADIN2111 load switches, high = on |
| PB13 / PB14 / PB15 | `FLASH_SCK` / `FLASH_MISO` / `FLASH_MOSI` | SPI2, external NOR flash |
| PA8 | `FLASH_CS` | NOR flash chip select |
| PB6 / PB7 | I2C1 SCL / SDA | Bristlefin: PCA9535 `0x21`, TCA9546A mux `0x70`, INA232 `0x41`/`0x43`, BME280 `0x76` behind mux channel 0 |
| PA10 | `IOEXP_INT` | PCA9535 interrupt, EXTI10 |
| PA1 | `I2C_MUX_RESET` | TCA9546A reset, active low; `bspInit` drives it high |
| PB1 | `VBUS_BF_EN` | Bristlefin VBUS enable; C leaves it low |
| PA2 / PA3 | `PAYLOAD_TX` / `PAYLOAD_RX` | LPUART1, the payload UART |
| PA7 / PA5 | `BM_MOSI_TX3` / `BM_SCK_RX3` | USART3 TX / RX, the serial console when USB is absent (`app_main.cpp` `usart3`) |
| PA4 / PA6 / PB0 | `BM_CS` / `BM_MISO` / `BM_INT` | SPI1 header |
| PA11 / PA12 | USB OTG FS DM / DP | the C console and pcap stream (CDC 0 and 1) |
| PA9 | `VUSB_DETECT` | EXTI9, both edges |
| PH3 | `BOOT_LED` | BOOT0; input |
| PH0 / PC13 | `GPIO1` / `GPIO2` | spare outputs |
| PA13 / PA14 | SWDIO / SWCLK | debug probe |

GPDMA1 channels: 12 = SPI3 RX, 13 = SPI3 TX (`.ioc:21-22`); 8/9 = I2C1 in the
embassy example.

Bristlefin PCA9535 pins (`bsp_pins.c`): 0 `BF_IO1` in, 1 `BF_IO2` in, 2
`BF_HFIO`, 3 `BF_3V3_EN`, 4 `BF_5V_EN`, 5 `BF_IMU_INT` in, 6 `BF_IMU_RST`, 7
`BF_SDI12_OE`, 8 `BF_TP16` in, 9 `BF_LED_G1`, 10 `BF_LED_R1`, 11 `BF_LED_G2`,
12 `BF_LED_R2`, 13 `BF_PL_BUCK_EN`, 14–15 `BF_TP7`/`BF_TP8` in. LEDs are
active low. The embassy example's `Bristlefin` driver shows that 3V3 (pin 3)
must be high before the pin is made an output: the I2C pull-ups are on that
rail, and losing it needs a power cycle.

## ADIN2111

| Item | C (bm_protocol) | Here |
|---|---|---|
| Bus | SPI3, mode 0, 8-bit, MSB first, software NSS, prescaler 8 from 160 MHz = 20 MHz (`Core/Src/spi.c` `MX_SPI3_Init`) | 20 MHz, embassy default mode 0 |
| Power | `bspInit` sets `ADIN_PWR` high (`bsp.cpp:69`) | `start` sets PH1 high, waits 90 ms |
| Reset | `bcl_power_callback` (`src/lib/bm_integration/bristlemouth_client.cpp:43-52`): `ADIN_PWR` high, `ADIN_CS` high, `ADIN_RST` low 1 ms, high, wait 100 ms | `embassy_net_adin1110::new_tc6`: `RST` low 30 ms, high, wait 90 ms, poll `PHYID` |
| Interrupt | `ADIN_INT` callback calls `bm_l2_handle_device_interrupt` (`bristlemouth_client.cpp:36-41,55`) | driver runner awaits the line |
| MAC | `mac_address`: `00:00` + low 32 bits of node id (`device_info.c:171-177`) | `bm_phy_adin2111::for_node` |
| Frame filter | bm_core's ADIN driver | `new_tc6`: own MAC and broadcast filters; `CONFIG2.P{1,2}_FWD_UNK2HOST` set and cut-through off, so IPv6 multicast (`33:33:…`) reaches the host |

## Identity

`getNodeId` (`src/lib/common/device_info.c:180-191`) is
`fnv_64a_buf(UID, 12, 0)`: FNV-1a 64 over the 12 UID bytes in memory order,
**starting from 0**, not the FNV offset basis. `node_id_from_uid` matches it;
the embassy example derives the same id. Nothing is provisioned: the id is a
function of the chip.

`bcl_init` (`bristlemouth_client.cpp:54-76`) fills `DeviceCfg` with vendor,
product and hardware version 0, serial number `"0123456789abcdef"`, device
name `getUIDStr()` (`%08x%08x%08x` of `UID[2]`, `UID[1]`, `UID[0]`,
`device_info.c:92-101`), and the firmware version and git SHA.
`DevkitIdentity` does the same with this crate's version and the first 8 hex
digits of `HEAD` (`build.rs`); its version string is
`bm-devkit@v<version>+<sha>`, after the C's `<app>@<describe>+<sha>`.
The constants are `src/version.rs`'s, which also puts them in the image's
version note ("Version note", below).

`bm_app_name`, sent in a sys_info reply, is `APP_NAME`
(`src/lib/bm_integration/bm_config.h`), the app directory's name
(`src/CMakeLists.txt`: `get_filename_component(APP_NAME ${APP} NAME)`), so
`hello_world` for `src/apps/bm_devkit/hello_world`. `bm_devkit::node` takes it;
each binary passes `env!("CARGO_BIN_NAME")`.

## Services

| Order | C | Here |
|---|---|---|
| 1 | `metrics_service_init`, from `bristlemouth_init` (`bm_metrics_enabled`), in `bcl_init` | `Node::with_config`, as `Services::METRICS` defaults to true |
| 2 | `echo_service_init()`, `app_main.cpp:413` | `hello_world`: `Node::register_echo_service` |
| 3 | `sys_info_service_init()`, `app_main.cpp:414` | `Node::register_sys_info_service` |
| 4 | `config_cbor_map_service_init()`, `app_main.cpp:415` | `Node::register_config_map_service` |

Not reproduced: `memory_metrics_init()`, after `bcl_init()` in
`defaultTask`, adds a `memory` metrics component of FreeRTOS heap
statistics, so a C dev kit's metrics reply has one component and
`hello_world`'s has none.

## Configuration storage

**External NOR flash, not internal.** `app_main.cpp` (`defaultTask`) builds a
`spiflash::W25` on SPI2 with `FLASH_CS` and hands `NvmPartition`s over it to
`bm_config_read`/`bm_config_write` (`src/lib/sys/bm_config_wrapper.cpp`).

| Partition | Offset | Size | `BmConfigPartition` |
|---|---|---|---|
| hardware | `0x00000` | 10240 | `BM_CFG_PARTITION_HARDWARE` |
| system | `0x03000` | 10240 | `BM_CFG_PARTITION_SYSTEM` |
| user | `0x06000` | 10240 | `BM_CFG_PARTITION_USER` |
| cli | `0x09000` | 10240 | — |
| dfu | `0x0C000` | 2048000 | — (the DFU host's image, below) |

Offsets from `src/lib/common/external_flash_partitions.c`, whose
`END + (END % 4096)` rounding happens to land on 4 KB boundaries for 10 KB
partitions. `NvmPartition::read`/`write` add the offset and assert
`offset + len < size` (`src/lib/common/nvmPartition.cpp`).

W25 driver (`src/lib/drivers/w25.cpp`): part W25Q64JV, 8 MB (driver says
8000000), 4 KB sectors, 256-byte pages, 24-bit addresses. `write` is
read-modify-write per 4 KB sector: read sector, patch, `0x20` sector erase,
then 16 × (`0x06` WREN, `0x02` page program). Reads are `0x03`. SPI2 is set up
as SPI3 (mode 0, prescaler 8, 20 MHz).

The image layout is `bm_wire::configuration::Layout::ARM_EABI_GCC`: the C is
built with `arm-none-eabi-gcc` and no `-fno-short-enums`
(`src/bsp/bm_mote_v1.0/CMakeLists.txt:75`), which gives a 4359-byte image.
Not yet checked against a kit's flash.

Here:

| C | Rust |
|---|---|
| `spiflash::W25` | `w25::W25`, over `embedded-hal` blocking `SpiDevice` and `DelayNs`; `start` builds it on SPI2 at 20 MHz with `FLASH_CS` (PA8) |
| `NvmPartition` + `bm_config_wrapper.cpp` | `storage::FlashConfigStorage`, a `bm_stack::ConfigStorage` |
| `bm_config_reset`: `resetSystem(RESET_REASON_CONFIG)` | `noinit::reset(ResetReason::Config)` |

Differences from the C, none visible in flash contents after a completed
write:

| C | Rust |
|---|---|
| `_write` counts `ceil(len / 4096)` sectors, plus one if the write ends inside a later sector: a 4359-byte image rewrites three sectors, the third unchanged | rewrites the two the image overlaps |
| out-of-range `offset + len` asserts (`configASSERT`) | `read`/`write` return `false` |
| `timeout_ms` bounds the wait for the driver's mutex | unused: the store owns the part |
| sector buffer allocated per write | a 4 KB field of `W25`, so it lives in the node's `StaticCell` |
| status polled back to back until a tick deadline | polled every 10 µs, a timeout counting polls |

A write interrupted between a sector's erase and its last page program loses
that sector, in both.

## Flash layout, bootloader, no-init RAM

`src/CMakeLists.txt:72-133` and `src/bsp/common/linker/bs_stm32u575.ld`,
built with `USE_BOOTLOADER=1`, which is what dev kits ship with:

| Area | Address | Size |
|---|---|---|
| MCUboot bootloader | `0x08000000` | `0xC000` |
| slot 1: 512-byte MCUboot header, then the image | `0x0800C000` | `0xF2000` |
| slot 2 | `0x080FE000` | `0xF2000` |
| scratch | `0x081F0000` | `0x10000` |
| `NOINIT` | `0x200BFE00` | 512, the top of RAM |

The C also keeps a memfault coredump region at the end of flash. This is the
only layout linked here; there is no build for `0x08000000`.

| File | What |
|---|---|
| `memory.x` | `FLASH` from `0x0800C200`, `0xF0520` bytes: `bm-image`'s limit for a signed image's body (`docs/mcuboot-todo.md`, contract 3) |
| `devkit.x` | cortex-m-rt 0.7.7's `link.x` with two changes, below. `build.rs` links with it; `Cargo.toml` pins `cortex-m-rt = "=0.7.7"` |

| `devkit.x` change | Why |
|---|---|
| `.note.sofar.version` at `ORIGIN(FLASH) + 0x238`, `_stext` after it | The C's address for the version note. The link fails if the vector table runs into it or the note is absent. |
| The vector table's alignment assertion asks for 128 bytes | cortex-m-rt asks for the next power of two above the table's size, `0x400` for `0x238` bytes, which `0x0800C200` is not. That is ARMv7-M's rule; ARMv8-M's `VTOR` holds bits 31:7, and the C image runs from the same address. |

The bootloader validates slot 1 on every boot (`MCUBOOT_VALIDATE_PRIMARY_SLOT`),
so an ELF programmed at `0x0800C200` without its header and hash TLV does not
boot. `runner.sh` and `bm-image unified` both write a whole image from
`0x0800C000`.

### Version note

`src/version.rs` `NOTE` is bm_protocol's `versionNote`
(`src/lib/common/version.h`, filled by `cmake/git_version.cmake`,
placed by `bs_stm32u575.ld:104-108`): an ELF note header (`namesz` 8,
`descsz` 118, type `0x10`), `"VERSION\0"`, and a packed `versionInfo_t`.

| Field | C | Here |
|---|---|---|
| address | `0x0800C438`, after the `0x238`-byte vector table; the magic is at file offset `0x44C` of the `.dfu.bin` | the same |
| `gitSHA` | `git describe --match ForceNone --abbrev=8 --always` | `version::GIT_SHA`, as `DevkitIdentity` |
| `maj`, `min`, `rev` | from a `vX.Y.Z` tag, else `0xFF` each | this crate's Cargo version |
| `hwVersion` | 0 | 0 |
| `flags` | bit 0 eng unless `RELEASE=1`; bit 1 dirty | bit 0 set; dirty not computed |
| `versionStr` | `git describe --always --dirty --abbrev=8` | `DevkitIdentity`'s version string |
| section type | `SHT_NOTE` | `SHT_PROGBITS`; tools find the magic in the binary |

`bm-image dfu` fills `ih_ver` from the note, so the three versions of
contract 8 come from `src/version.rs`.

### What the bootloader leaves

`src/apps/bootloader/app_main.c`: `boot_port_init` (`:24-44`), then
`boot_port_startup` (`:56-78`) suspends the HAL tick, calls `HAL_DeInit`,
sets `VTOR`, loads `MSP` and calls the reset vector. `HAL_DeInit` resets the
peripherals, not RCC, PWR or the IWDG.

| State at the image's reset vector | Source | Here |
|---|---|---|
| SYSCLK 160 MHz from PLL1: MSIS range 2 (16 MHz), M=3, N=30, R=1, MBOOST /1; flash latency 4; VOS range 1 | `src/bsp/bootloader/Core/Src/main.c:117-167` | `embassy_stm32::init` (`rcc/u5.rs`) switches SYSCLK to HSI before it changes MSIS or a PLL, then sets up `config()`'s tree: MSIS 48 MHz, N=10, MBOOST /4. `config()` is unchanged. |
| LSI on | the same | left on; the IWDG holds it on |
| SMPS selected (`PWR_CR3.REGSEL`) | `main.c:173-188` | not written; stays SMPS |
| IWDG running: prescaler 32, reload 4095, about 4.1 s | `Core/Src/iwdg.c:28-51` | `src/watchdog.rs`: `start` feeds it, then spawns `watchdog::task`, which feeds it every second. The C feeds from its lowest-priority task (`src/lib/common/watchdog.c`). |
| `SCB->CCR` `DIV_0_TRP` set | `app_main.c:40` | left set |
| `ulBootloaderMagic` zeroed | `bootloader_helper.c:28-43` | "No-init RAM" |

Read from a kit running `hello_world` under the bootloader:

| Register | Value | Meaning |
|---|---|---|
| `SCB->VTOR` | `0x0800C200` | |
| `IWDG_PR`, `IWDG_RLR` | 3, `0xFFF` | the bootloader's /32 and 4095 |
| `RCC_CFGR1` | `0x0000000F` | SYSCLK is PLL1R |
| `RCC_ICSCR1` | `0x0485AD68` | MSIS range 0, 48 MHz |
| `RCC_PLL1CFGR` | `0x0007220D` | source MSIS, M=3, MBOOST /4 |
| `PWR_CR3` | `0x00000002` | SMPS |
| `PWR_VOSR` | `0x0007C000` | range 1, booster on |

### Installing the C bootloader

Once per kit, and again after anything writes `0x08000000`. Build
bm_protocol's `bootloader` preset (`CMakePresets.json`), then either:

```
probe-rs download --chip STM32U575CITxQ <bm_protocol>/preset-builds/bootloader/src/bootloader-bootloader.elf
```

or build a `.unified.bin`, bootloader and Rust image together, and program
it at `0x08000000`:

```
BM_BOOTLOADER=<bootloader.elf> ./build.sh --release      # in bm-devkit/: <elf>.unified.bin
probe-rs download --chip STM32U575CITxQ --binary-format bin --base-address 0x08000000 <elf>.unified.bin
dfu-util -a 0 -s 0x08000000:leave -D <elf>.unified.bin    # without a probe, from the ROM bootloader
```

That preset's bootloader takes unsigned images and ignores a signature. One
built with `SIGN_IMAGES=1` refuses an image not signed with its key:
`BM_IMAGE_KEY`.

A kit that had Rust firmware linked at `0x08000000` keeps that image's
remains between the bootloader's end and `0x0800C000`; nothing reads them.

### Restoring C firmware

Program a C `.elf.unified.bin` (bm_protocol's `hello-world` preset writes
`preset-builds/hello-world/src/bm_mote_v1.0-hello_world-dbg.elf.unified.bin`)
at `0x08000000` with either command above. With the bootloader already
installed, its `.elf.dfu.bin` at `0x0800C000` is enough.

### On a bench

With the `bootloader` preset's ELF at bm_protocol `62d8b5d0` installed, on a
bus with a Bridge and a C dev kit:

| Step | Result |
|---|---|
| `cargo run --release --bin hello_world` | boots; logs node id, services, both neighbours' heartbeats and the Bridge's `spotter/utc-time`; no reset in 40 s |
| `bm-image unified` of that bootloader and the Rust `.dfu.bin`, programmed at `0x08000000` with `probe-rs download` | the same, 25 s |
| the same `.unified.bin` with `dfu-util`, from the ROM bootloader | boots the same |
| the topology a Spotter reports from the Bridge | lists the node |

### No-init RAM

`bs_stm32u575.ld:57-71` takes the top 512 bytes of RAM as `NOINIT`, and
`:225-234` fixes the order of what goes in it. `memory.x` leaves the same
bytes out of `RAM`; `src/noinit.rs` reads and writes them at fixed addresses.

| Address | Size | Symbol | Source | `noinit` |
|---|---|---|---|---|
| `0x200BFE00` | 4 | `ulBootloaderMagic` | `src/lib/common/bootloader_helper.c:13`. `enterBootloaderIfNeeded` (`:28-43`, called from `src/apps/bootloader/app_main.c:83`) zeroes it on every boot and jumps to the ROM bootloader on `0xB8278F6D` | never written |
| `0x200BFE04` | 4 | `resetReason`, a `ResetReason_t` (`reset_reason.h:13-28`; no `-fshort-enums`) | `src/lib/common/reset_reason.c:11` | `reset`, `take_reset_reason` |
| `0x200BFE08` | 4 | `ulResetReasonMagic`, `0xB8278F7D` (`reset_reason.h:11`) | `reset_reason.c:10` | `reset`, `take_reset_reason` |
| `0x200BFE0C` | `0x40` | `s_reboot_tracking` | `src/lib/memfault/memfault_platform_core_u5.c:40-41` | never written |
| `0x200BFE4C` | 18 | `client_update_reboot_info`, a packed `ReboootClientUpdateInfo` | `src/lib/bm_core/bcmp/dfu_core.c:27`; `bm_noinit_ram_attribute` is `section(".noinit")` (`src/lib/bm_integration/bm_config.h:10`) | `NoInit`, through `RebootInfo::encode`/`decode` |

Addresses are from two link maps, which agree:

| Map | Build |
|---|---|
| `preset-builds/hello-world/src/bm_mote_v1.0-hello_world-dbg.elf.map` | the `hello-world` preset |
| `bridge_v1_0-bridge-dbg.elf.map` | the `bridge` preset's cache variables (`APP=bridge`, `BSP=bridge_v1_0`), configured into a directory outside the checkout |

The Bridge's map has one more object, `_reboot_info` (8 bytes,
`src/lib/bm_ncp/ncp_dfu.cpp`, section `.noinit._reboot_info`), at
`0x200BFE60`; the linker script places that section last. The bootloader's
map (`preset-builds/bootloader/src/`) has only `ulBootloaderMagic`, at
`0x200BFE00`.

`client_update_reboot_info`'s address depends on the link: it is the first
object in plain `.noinit`, after the three named sections. An app without
memfault's U5 core, or with another `.noinit` object linked ahead of
`libbcmp.a`, would move it.

| C | Rust |
|---|---|
| `resetSystem(reason)` (`reset_reason.c:14-30`): magic, reason, `NVIC_SystemReset` | `noinit::reset(reason)`: the same writes, then `SCB::sys_reset` |
| `checkResetReason()` (`reset_reason.c:32-54`): the stored reason if the magic is set, else `RESET_REASON_INVALID`; zeroes the magic and sets the stored reason to `RESET_REASON_INVALID`; later calls return a cached value | `noinit::take_reset_reason()`: the same reads and writes, no cache. `start` calls it once; the result is `Board::reset_reason`, which both binaries log |
| reasons written on the DFU path: `RESET_REASON_MCUBOOT` (4) and `RESET_REASON_UPDATE_FAILED` (6), `src/lib/drivers/bm_dfu_wrapper.cpp:26`, `:31` | `ResetReason::Mcuboot`, `ResetReason::UpdateFailed`; not written until the node has a `DfuSlot` |

`bm_devkit::node` still builds its node with `NoDfu`: `Node` takes one type
for `DfuSlot` and `NoInitRam`, so `NoInit` goes in with the slot.

### DFU slot

`src/slot.rs` `DevkitSlot` is `bm_stack::DfuSlot` on slot 2 and
`bm_stack::NoInitRam` through `noinit::NoInit`. `start` builds it from
`p.FLASH`; `node` passes it to `Node::with_dfu`.

| Hook | C | Here |
|---|---|---|
| open, close, size | `bm_dfu_wrapper.cpp:35-65`, `port_flash.c:77-88` | `true`, `true`, `0xF2000` |
| erase | `port_flash.c:140-169`, `stm32_flash_u5.c:8-93`: page-aligned whole pages, else failure; erased per bank; read back as `0xFF` | the same, one page per `embassy_stm32::flash::Flash::blocking_erase`, `watchdog::feed` before each |
| write | `port_flash.c:110-137`, `stm32_flash_u5.c:95-125`: quad-words, then a `memcmp` of `len` bytes | the same, one quad-word per `blocking_write` |
| read | the host's `bm_dfu_host_get_chunk` reads the W25 `dfu` partition (`bm_dfu_wrapper.cpp:67-75`) | slot 2, memory-mapped |
| `set_pending_and_reset` | `boot_set_pending(0)`, result ignored; `resetSystem(RESET_REASON_MCUBOOT)` (`bm_dfu_wrapper.cpp:24-28`) | `bm_mcuboot::set_pending(slot 2, Trailer::BM, false)`, result logged; `noinit::reset(ResetReason::Mcuboot)` |
| `set_confirmed` | `boot_set_confirmed()` on slot 1 (`:17-22`) | `bm_mcuboot::set_confirmed(slot 1, Trailer::BM)` |
| `fail_update_and_reset` | `resetSystem(RESET_REASON_UPDATE_FAILED)` (`:30-33`) | `noinit::reset(ResetReason::UpdateFailed)` |

Where the C's behaviour is not reproduced:

| Case | C | Here |
|---|---|---|
| A write's tail shorter than 16 bytes | `flashWrite` copies 16 bytes from the source (`stm32_flash_u5.c:103`), past its end, and programs them | `0xFF` past the tail |
| An erase past the slot's end | `flash_area_erase` does not check `fa_size`; `flashErase` checks only the chip's flash, so it erases into the next area | `false` |
| A write at an offset not a multiple of 16 | `HAL_FLASH_Program` fails, `flashWrite` returns `true` regardless (`:119`), the read-back fails | `false`, nothing programmed |

bm_core's client writes 2048-byte pages and one remainder from offset 0, so
only the first case occurs; it changes bytes past the image that nothing
reads.

The ICACHE is off: neither the bootloader nor `embassy_stm32::init`
enables it, so reads after an erase or write see flash.

`erase` blocks the executor for the whole slot, as `flashErase` blocks the
C's DFU task; the ADIN2111 runner, the heartbeat and the watchdog task wait.
It records its duration: defmt logs `slot: erased 0xf2000 bytes at 0x0 in
<n> ms`, and `hello_world` sends `dfu: slot 2 erased in <n> ms` to the
Spotter console in place of its next `hello world`, within 10 s.

A neighbour takes a node offline after two advertised heartbeat periods
without a heartbeat: 20 s for bm_core's 10 s (`bm_wire::neighbor`,
`Neighbor::lease_ms`). The Bridge logs each change of a neighbour on its own
ports as `Neighbor <node id> added` or `Neighbor <node id> lost`
(`src/apps/bridge/app_main.cpp:333-338`), to its console and on
`bridge/printf`, its system log.

#### On a bench

| Step | Result |
|---|---|
| Rust `hello_world` to Rust `hello_world` with another git SHA, from a Spotter's SD card through a Bridge | success; the Bridge reports the new SHA |
| Slot erase time | 240 ms, from `dfu: slot 2 erased in 240 ms` |
| A neighbour timing the node out during the erase | none: no `Neighbor <node id> lost` from the Bridge |

Measuring the last two again. The node must be cabled to the Bridge itself, and
the image it runs **before** the update must already contain the erase
report, since the old image is the one that erases:

1. Bring the node to an image built from this code: `./build.sh --release`,
   then DFU `target/thumbv8m.main-none-eabihf/release/hello_world.dfu.bin`
   from the Spotter, or `cargo run --release --bin hello_world` with a
   probe.
2. Commit any change, so the next image's git SHA differs. Run
   `./build.sh --release`; the printed note must show the new SHA. Copy the
   new `hello_world.dfu.bin` to the SD card.
3. Start the DFU from the Spotter, without force.
4. Erase time: the node's `dfu: slot 2 erased in <n> ms` line on the
   Spotter console. With a probe still attached from step 1, also the
   `slot: erased …` defmt line. After the update's reset the attached
   session decodes with the old ELF; re-attach with the new one
   (`probe-rs attach --chip STM32U575CITxQ --no-catch-reset <new elf>`).
5. Timeout: the Bridge's log from the DFU's start to the node's reset at
   its end. Pass: no `Neighbor <node id> lost` for this node. One `Neighbor
   <node id> added` after the reset is the restart (`bm_wire::neighbor`,
   a lower `time_since_boot_us`), not a timeout.
6. Record `<n>` and the result in this table.

## DFU image locations

The client receives into internal flash; only the host reads the NOR flash.

| `bm_dfu_generic.h` hook | C (`src/lib/drivers/bm_dfu_wrapper.cpp`) | Storage |
|---|---|---|
| `bm_dfu_client_flash_area_open`, `_erase`, `_write`, `_get_size` | `flash_area_*` on `FLASH_AREA_IMAGE_SECONDARY(0)` (`:35-65`) | MCUboot slot 2, internal flash: `secondary_img0`, `FLASH_DEVICE_INTERNAL_FLASH` (`src/lib/mcuboot/port_flash.c:43-48`), at `0x080FE000`, `0xF2000` bytes (`src/CMakeLists.txt:114-121`) |
| `bm_dfu_host_get_chunk` | `dfu_partition_global->read` (`:66-74`) | the W25 `dfu` partition (`src/apps/bm_devkit/bmdk_common/app_main.cpp:390-391`) |

On the client side, `port_flash.c` erases only whole 8 KB internal pages
(`FLASH_PAGE_SIZE`, `port_flash.c:145-149`) and checks every write and erase
by reading back (`MCUBOOT_VERIFY_WE`). bm_core erases the whole slot before the first
chunk (`bcmp/dfu_client.c:297-302`).

The host side is the debug CLI's: `nvm b64write dfu …` writes an image (a
`BmDfuImgInfo` header, then the image) into the `dfu` partition, and `dfu
start <node> <filter_key> <timeout>` checks its CRC and calls
`bm_dfu_initiate_update` with `internal` set
(`src/lib/debug/debug_dfu.cpp:91-106`).
So the NOR flash is shared between config and DFU only for
`bm_stack::DfuSlot::read`, which never writes.

## Time

The C sets its RTC from the Spotter's `spotter/utc-time`, and answers BCMP
time messages from the same RTC.

| Item | C | Here |
|---|---|---|
| Subscription | `bm_sub(APP_PUB_SUB_UTC_TOPIC, handle_bm_subscriptions)`, `src/apps/bm_devkit/bmdk_common/app_main.cpp:412` | `hello_world` subscribes to `bm_stack::utc_time::TOPIC` |
| Handler | `handle_bm_subscriptions`, `app_main.cpp:238-272` | `bm_stack::utc_time::UtcTimeSetter` |
| Topic check | `strncmp(APP_PUB_SUB_UTC_TOPIC, topic, topic_len) == 0` | the same, in `utc_time::decode` |
| Type, version | both 1 (`bmdk_common/app_pub_sub.h:10-12`); else prints "Unrecognized version" | `UtcTimeError::Unrecognized` |
| Data | `bm_common_pub_sub_utc_t` (bm_core `bm_common_messages/bm_common_pub_sub.h:21-23`), a packed `uint64_t` of UTC µs (its comment says ns); `data_len` not checked | first 8 bytes, little-endian; fewer is `UtcTimeError::Short` |
| Conversion | `dateTimeFromUtc`, `.ms = usec / 1000` | `RtcTimeAndDate::from_utc_micros` |
| BCMP `bm_rtc_get`/`bm_rtc_set` | `src/lib/drivers/bm_rtc_wrapper.c`, onto `rtcGet`/`rtcSet` | `bm_stack::Rtc` on the node |
| Clock | the STM32 RTC (`src/lib/drivers/stm32_rtc.c`), below | `bm_devkit::rtc::DevkitRtc` |

`stm32_rtc.c`, started by `rtcInit` from `defaultTask` (`app_main.cpp:346`)
after `MX_RTC_Init` (`Core/Src/rtc.c:28-66`) has done the same:

| Item | C | `DevkitRtc` |
|---|---|---|
| Clock source | LSE, `RCC_LSE_ON` (LSESYSEN set), `RCC_LSEDRIVE_HIGH` (`main.c:148-155`); `stm32_rtc.c:44` | `config`: `LsConfig::default_lse()`, drive `High`, `peripherals_clocked` |
| Prescalers | asynchronous 127, synchronous 255: 1 Hz, 1/256 s subseconds, written on every boot (`:51`) | embassy `Rtc::new` at 256 Hz: the same, written only when they differ |
| Shadow registers | bypassed (`BYPSHAD`, `:57-60`) | the same |
| "Set" flag | `0x836A20DD` in backup register `DR0` after a successful `rtcSet`; `rtcGet` fails without it (`:9`, `:72`, `:261`) | the same; `DR0` is `TAMP_BKP0R` on the U5 |
| Year | two BCD digits, offset 2000 (`:184`, `:239`); a year outside 2000-2099 is written truncated | the same; outside 2000-2099 `set` refuses |
| Weekday | always Monday (`:235`) | the same |
| Milliseconds | `SHIFTR` with `ADD1S` and `(1000·256 − ms·256) / 1000`, then waits for `SHPF` (`:250-258`) | the same arithmetic |
| Read | `TR`/`DR` reread until two reads agree; `calculate_rtc_ms`; one second back while `SSR > PREDIV_S` (`:155-199`) | the same; each register read once per pass |
| Backup-register protection | `LL_RTC_SetBackupRegProtection(RTC, DR0, DR0)`, `LL_RTC_SetRtcPrivilege` (`rtc.c:59-61`) | not written; they matter only with TrustZone, which neither enables |

The calendar and `DR0` are in the backup domain, so a time set before a
reset still reads after it (checked on a bench). A time set by C firmware
should read in Rust firmware flashed over it, since embassy does not reset
the backup domain on the U5; not yet run.

Two `stm32_rtc.c` quirks, reproduced (bm_protocol application code, so not
in `docs/c-divergences.md`):

| Quirk | Effect |
|---|---|
| `calculate_rtc_ms` counts from `2 * PREDIV_S`, not `2 * PREDIV_S + 1`, while `SSR > PREDIV_S` | 1/256 s low; at `SSR` 511 the `uint32_t` wraps and `ms` reads 65531 |
| `SSR > PREDIV_S` follows every `rtcSet`, since the shift adds `adjust` (up to 256) to `SSR` | the `decrement_one_second` workaround runs for up to a second after each set |

## Not used yet

USB (the C console and pcap), the Bristlefin expander and LEDs, and
low-power management.
