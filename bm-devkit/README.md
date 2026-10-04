# bm-devkit

Board support for the Bristlemouth dev kit's mote. `src/lib.rs` brings the
board up; `src/bin/bringup.rs` runs a node on it, and `src/bin/hello_world.rs`
is the hello-world app.

```
cd bm-devkit && cargo build --target thumbv8m.main-none-eabihf
cd bm-devkit && cargo run --release --bin bringup     # probe-rs, defmt over RTT
cd bm-devkit && cargo run --release --bin hello_world
# The runner passes --no-catch-reset, so logging continues across a reset.
# probe-rs 0.21 has no such flag: cargo install probe-rs-tools --locked
```

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
| Power supply | SMPS (`PWR_SMPS_SUPPLY`) | `.ioc` `PWR.PowerMode`. Not configured here; embassy's default (LDO) is what the embassy example runs on. |
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
| `bm_config_reset`: `resetSystem(RESET_REASON_CONFIG)` | `SCB::sys_reset`; no reset reason, since the no-init block is not placed |

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

`src/CMakeLists.txt:72-133` and `src/bsp/common/linker/bs_stm32u575.ld`:

| Build | Layout |
|---|---|
| default | application at `0x08000000` |
| `USE_BOOTLOADER=1` | MCUboot 48 KB at `0x08000000`; slot 1 at `0x0800C000`, `0xF2000` bytes, 512-byte MCUboot header before the vector table; slot 2 after it; 64 KB scratch |
| both | top 512 bytes of RAM (`0x200BFE00`) are `NOINIT`; memfault coredump region at the end of flash |

`memory.x` links at `0x08000000`, so `probe-rs run` replaces whatever is
installed, bootloader included. Restoring C firmware means flashing its
bootloader and image again. `memory.x` leaves the no-init 512 bytes out of
`RAM` so a later DFU card can place `NoInitRam` where the C bootloader
expects it.

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

USB (the C console and pcap), the Bristlefin expander and LEDs, LSI, the
watchdog (`MX_IWDG_Init`), low-power management, and the SMPS.
