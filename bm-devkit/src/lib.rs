//! Board support for the Bristlemouth dev kit's mote: an STM32U575CI with an
//! ADIN2111 on SPI3.
//!
//! [`start`] brings the board up and returns a [`Board`]: the ADIN2111 as a
//! [`bm_stack::Phy`], the driver runner the firmware must spawn, and the node
//! id the C firmware would use on the same chip, and the NOR flash that holds
//! the config partitions, the RTC, MCUboot's slots, the reason for the
//! last reset, and the peripherals `start` does not use ([`Spare`]).
//! [`Devkit`] is the node type with this board's identity, clock, config
//! store and DFU slot. [`bm_header_spi`] sets up the BM header's SPI1 as the
//! soft module's BSP does.
//!
//! Every pin, clock and sequence here is taken from bm_protocol's
//! `bm_mote_v1.0` BSP, and `bm_mote_spi_v1_0` for the soft module's
//! header; `README.md` in this crate records each with its source.

#![no_std]
#![warn(missing_docs)]

pub mod noinit;
pub mod rtc;
pub mod slot;
pub mod storage;
pub mod version;
pub mod w25;
pub mod watchdog;

use bm_phy_adin2111::{Adin2111Phy, Runner, State, StaticPool, Tc6};
use bm_stack::{Config, Identity, Node, NodeResources, Parts};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::configuration::Layout;
use embassy_executor::Spawner;
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::flash::Flash as InternalFlash;
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{Peri, bind_interrupts, dma, interrupt, peripherals};
use embassy_time::{Delay, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;

use crate::noinit::ResetReason;
use crate::rtc::DevkitRtc;
use crate::slot::DevkitSlot;
use crate::storage::FlashConfigStorage;
use crate::w25::W25;

/// Bristlemouth ports on the ADIN2111.
pub const PORTS: u8 = 2;

/// SPI3 clock: SYSCLK 160 MHz through `SPI_BAUDRATEPRESCALER_8`, as
/// `MX_SPI3_Init` configures it.
pub const ADIN_SPI_HZ: u32 = 20_000_000;

/// Time from `ADIN_PWR` high to the driver's reset pulse. The C firmware
/// raises `ADIN_PWR` in `bspInit` and pulses reset much later, in
/// `bcl_power_callback`; this is the settle time the embassy example uses.
pub const ADIN_POWER_SETTLE_MS: u64 = 90;

/// SPI2 clock: `MX_SPI2_Init` sets it up as SPI3.
pub const FLASH_SPI_HZ: u32 = 20_000_000;

/// SPI1 clock on the soft module: SYSCLK 160 MHz through
/// `SPI_BAUDRATEPRESCALER_128`, as `bm_mote_spi_v1_0`'s `MX_SPI1_Init`
/// configures it.
pub const BM_HEADER_SPI_HZ: u32 = 1_250_000;

bind_interrupts!(
    /// The ADIN2111's interrupts: its `INT` line on EXTI8, and the two GPDMA
    /// channels SPI3 uses, as in `bm_mote_v1.0.ioc`. Also the two channels
    /// [`bm_header_spi`] gives SPI1.
    pub struct Irqs {
        EXTI8 => exti::InterruptHandler<interrupt::typelevel::EXTI8>;
        GPDMA1_CHANNEL0 => dma::InterruptHandler<peripherals::GPDMA1_CH0>;
        GPDMA1_CHANNEL1 => dma::InterruptHandler<peripherals::GPDMA1_CH1>;
        GPDMA1_CHANNEL12 => dma::InterruptHandler<peripherals::GPDMA1_CH12>;
        GPDMA1_CHANNEL13 => dma::InterruptHandler<peripherals::GPDMA1_CH13>;
    }
);

/// SPI3 with `ADIN_CS` as its chip select.
pub type AdinSpi = ExclusiveDevice<Spi<'static, Async, Master>, Output<'static>, Delay>;
/// `ADIN_INT`.
pub type AdinInt = ExtiInput<'static, Async>;
/// `ADIN_RST`.
pub type AdinReset = Output<'static>;
/// The ADIN2111 driver's runner. Spawn it: until it runs no frame moves.
pub type AdinRunner = Runner<'static, Tc6<AdinSpi>, AdinInt, AdinReset>;

/// SPI2, blocking, with `FLASH_CS` as its chip select.
pub type FlashSpi = ExclusiveDevice<Spi<'static, Blocking, Master>, Output<'static>, Delay>;
/// The W25Q64JV on SPI2.
pub type Flash = W25<FlashSpi, Delay>;
/// The config partitions on [`Flash`].
pub type DevkitConfigStorage = FlashConfigStorage<FlashSpi, Delay>;

/// SPI1 on the BM header with `BM_CS` as its chip select: [`bm_header_spi`].
pub type BmHeaderSpi = ExclusiveDevice<Spi<'static, Async, Master>, Output<'static>, Delay>;

/// How many topics a [`Devkit`] advertises, publishers and subscribers
/// together. `hello_world` uses 11: six subscriptions (`spotter/*`,
/// `spotter/utc-time`, and `<id>/<service>/req` for metrics, echo, sys_info
/// and config_map), `spotter/printf`, and `<id>/<service>/rep` for each
/// service that answers. Past the ceiling a reply is still sent but its topic
/// is not advertised, where a C node's `PUB_LIST` would list it.
pub const RESOURCES: usize = 16;

/// The memory a [`Devkit`] runs in: [`NodeResources`] with room for
/// [`RESOURCES`] topics.
pub type DevkitResources = NodeResources<RESOURCES>;

/// A node on this board: [`DevkitIdentity`], the RTC, the config
/// partitions in NOR flash, and MCUboot's slot 2 for DFU.
pub type Devkit = Node<'static, DevkitIdentity, DevkitRtc, Config<DevkitConfigStorage>, DevkitSlot>;

/// The brought-up board.
pub struct Board {
    /// This chip's node id, [`node_id_from_uid`] of its UID.
    pub node_id: u64,
    /// The ADIN2111, as a [`bm_stack::Phy`].
    pub phy: Adin2111Phy<'static>,
    /// The driver's runner. Spawn it.
    pub adin_runner: AdinRunner,
    /// `ADIN_PWR`. Dropping an embassy `Output` disconnects the pin, which
    /// turns the ADIN2111 off, so it is held here.
    pub adin_power: Output<'static>,
    /// The NOR flash. [`node`] takes it.
    pub flash: Flash,
    /// The RTC on LSE. [`node`] takes it.
    pub rtc: DevkitRtc,
    /// MCUboot's slots in internal flash. [`node`] takes it.
    pub slot: DevkitSlot,
    /// Why the chip last reset: [`noinit::take_reset_reason`], read once.
    pub reset_reason: ResetReason,
    /// The peripherals `start` does not use.
    pub spare: Spare,
}

/// The peripherals [`start`] leaves untouched, named as `embassy_stm32`
/// names them. Pin uses are `README.md`'s, "Pins".
#[allow(missing_docs)]
pub struct Spare {
    /// The BM header: SPI1 and `BM_CS` / SCK / MISO / MOSI. On the soft
    /// module [`bm_header_spi`] takes them; on the dev kit PA5 and PA7 are
    /// USART3 RX and TX.
    pub spi1: Peri<'static, peripherals::SPI1>,
    pub pa4: Peri<'static, peripherals::PA4>,
    pub pa5: Peri<'static, peripherals::PA5>,
    pub pa6: Peri<'static, peripherals::PA6>,
    pub pa7: Peri<'static, peripherals::PA7>,
    /// `BM_INT`, and its EXTI line.
    pub pb0: Peri<'static, peripherals::PB0>,
    pub exti0: Peri<'static, peripherals::EXTI0>,
    /// I2C1 on PB6 (SCL) and PB7 (SDA): the INA232s, and the Bristlefin.
    pub i2c1: Peri<'static, peripherals::I2C1>,
    pub pb6: Peri<'static, peripherals::PB6>,
    pub pb7: Peri<'static, peripherals::PB7>,
    /// `IOEXP_INT`, and its EXTI line.
    pub pa10: Peri<'static, peripherals::PA10>,
    pub exti10: Peri<'static, peripherals::EXTI10>,
    /// `I2C_MUX_RESET`.
    pub pa1: Peri<'static, peripherals::PA1>,
    /// `VBUS_BF_EN`.
    pub pb1: Peri<'static, peripherals::PB1>,
    /// GPDMA1 channels. [`Irqs`] binds 0 and 1, for [`bm_header_spi`]; 12
    /// and 13 are SPI3's.
    pub gpdma1_ch0: Peri<'static, peripherals::GPDMA1_CH0>,
    pub gpdma1_ch1: Peri<'static, peripherals::GPDMA1_CH1>,
    pub gpdma1_ch2: Peri<'static, peripherals::GPDMA1_CH2>,
    pub gpdma1_ch3: Peri<'static, peripherals::GPDMA1_CH3>,
    pub gpdma1_ch4: Peri<'static, peripherals::GPDMA1_CH4>,
    pub gpdma1_ch5: Peri<'static, peripherals::GPDMA1_CH5>,
    pub gpdma1_ch6: Peri<'static, peripherals::GPDMA1_CH6>,
    pub gpdma1_ch7: Peri<'static, peripherals::GPDMA1_CH7>,
    pub gpdma1_ch8: Peri<'static, peripherals::GPDMA1_CH8>,
    pub gpdma1_ch9: Peri<'static, peripherals::GPDMA1_CH9>,
    pub gpdma1_ch10: Peri<'static, peripherals::GPDMA1_CH10>,
    pub gpdma1_ch11: Peri<'static, peripherals::GPDMA1_CH11>,
    pub gpdma1_ch14: Peri<'static, peripherals::GPDMA1_CH14>,
    pub gpdma1_ch15: Peri<'static, peripherals::GPDMA1_CH15>,
}

/// The clock tree `SystemClock_Config` sets up: MSIS at 48 MHz, PLL1 `/3 *10
/// /1`, SYSCLK 160 MHz; LSE on, drive high, clocking the RTC.
///
/// The bootloader jumps here with SYSCLK on PLL1 from MSIS at 16 MHz
/// (`README.md`, "What the bootloader leaves"). `embassy_stm32::init` moves
/// SYSCLK to HSI before it touches MSIS or a PLL, so the same config applies
/// from that state as from reset.
#[must_use]
pub fn config() -> embassy_stm32::Config {
    use embassy_stm32::rcc::{
        LsConfig, LseDrive, LseMode, MSIRange, Pll, PllDiv, PllMul, PllPreDiv, PllSource, Sysclk,
    };

    let mut config = embassy_stm32::Config::default();
    config.rcc.msis = Some(MSIRange::Range48mhz);
    config.rcc.pll1 = Some(Pll {
        source: PllSource::Msis,
        prediv: PllPreDiv::Div3,
        mul: PllMul::Mul10,
        divp: Some(PllDiv::Div2),
        divq: Some(PllDiv::Div2),
        divr: Some(PllDiv::Div1),
    });
    config.rcc.sys = Sysclk::Pll1R;
    // `RCC_LSE_ON` with `RCC_LSEDRIVE_HIGH`; `RCC_LSE_ON` also sets LSESYSEN.
    // The C turns LSI on too, for nothing this crate uses.
    let mut ls = LsConfig::default_lse();
    if let Some(lse) = ls.lse.as_mut() {
        lse.mode = LseMode::Oscillator(LseDrive::High);
        lse.peripherals_clocked = true;
    }
    config.rcc.ls = ls;
    config
}

/// Initialise the chip with [`config`], spawn [`watchdog::task`], power the
/// ADIN2111 and bring it up, set up SPI2 for the NOR flash, start the RTC,
/// take the internal flash for [`DevkitSlot`], and take the reset reason from
/// no-init RAM.
///
/// The watchdog task is spawned before the first await, so it is fed while
/// the ADIN2111 comes up.
///
/// Consumes every peripheral. Those in [`Spare`] are returned in
/// [`Board::spare`]; the rest are dropped.
///
/// # Panics
///
/// If called twice, and wherever the driver panics: an ADIN2111 that never
/// answers or never completes reset.
pub async fn start(spawner: Spawner) -> Board {
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    // Frames in flight: up to 8 queued each way, plus one being sent.
    static POOL: StaticPool<1514, 20> = StaticPool::new();

    watchdog::feed();
    let p = embassy_stm32::init(config());
    spawner.spawn(watchdog::task().expect("one watchdog task"));
    let node_id = node_id();
    let reset_reason = noinit::take_reset_reason();
    let rtc = DevkitRtc::new(p.RTC);
    let slot = DevkitSlot::new(InternalFlash::new_blocking(p.FLASH));

    // ADIN_PWR (PH1) drives the ADIN2111's load switches.
    let adin_power = Output::new(p.PH1, Level::High, Speed::Low);
    Timer::after_millis(ADIN_POWER_SETTLE_MS).await;

    let reset = Output::new(p.PA0, Level::Low, Speed::Low);
    let int = ExtiInput::new(p.PB8, p.EXTI8, Pull::None, Irqs);
    let cs = Output::new(p.PA15, Level::High, Speed::High);

    let mut spi_config = spi::Config::default();
    spi_config.frequency = Hertz(ADIN_SPI_HZ);
    let spi = Spi::new(
        p.SPI3,
        p.PB3,
        p.PB5,
        p.PB4,
        p.GPDMA1_CH13,
        p.GPDMA1_CH12,
        Irqs,
        spi_config,
    );
    let spi = ExclusiveDevice::new(spi, cs, Delay);

    let (phy, adin_runner) = bm_phy_adin2111::for_node(
        node_id,
        STATE.init(State::new()),
        &POOL,
        spi,
        int,
        reset,
        false,
    )
    .await;

    let flash_cs = Output::new(p.PA8, Level::High, Speed::High);
    let mut flash_config = spi::Config::default();
    flash_config.frequency = Hertz(FLASH_SPI_HZ);
    let flash_spi = Spi::new_blocking(p.SPI2, p.PB13, p.PB15, p.PB14, flash_config);
    let flash = W25::new(ExclusiveDevice::new(flash_spi, flash_cs, Delay), Delay);

    let spare = Spare {
        spi1: p.SPI1,
        pa4: p.PA4,
        pa5: p.PA5,
        pa6: p.PA6,
        pa7: p.PA7,
        pb0: p.PB0,
        exti0: p.EXTI0,
        i2c1: p.I2C1,
        pb6: p.PB6,
        pb7: p.PB7,
        pa10: p.PA10,
        exti10: p.EXTI10,
        pa1: p.PA1,
        pb1: p.PB1,
        gpdma1_ch0: p.GPDMA1_CH0,
        gpdma1_ch1: p.GPDMA1_CH1,
        gpdma1_ch2: p.GPDMA1_CH2,
        gpdma1_ch3: p.GPDMA1_CH3,
        gpdma1_ch4: p.GPDMA1_CH4,
        gpdma1_ch5: p.GPDMA1_CH5,
        gpdma1_ch6: p.GPDMA1_CH6,
        gpdma1_ch7: p.GPDMA1_CH7,
        gpdma1_ch8: p.GPDMA1_CH8,
        gpdma1_ch9: p.GPDMA1_CH9,
        gpdma1_ch10: p.GPDMA1_CH10,
        gpdma1_ch11: p.GPDMA1_CH11,
        gpdma1_ch14: p.GPDMA1_CH14,
        gpdma1_ch15: p.GPDMA1_CH15,
    };

    Board {
        node_id,
        phy,
        adin_runner,
        adin_power,
        flash,
        rtc,
        slot,
        reset_reason,
        spare,
    }
}

/// SPI1 on the BM header as `bm_mote_spi_v1_0` sets it up (`MX_SPI1_Init`,
/// `HAL_SPI_MspInit`): master, mode 0, 8-bit, MSB first, software chip
/// select, [`BM_HEADER_SPI_HZ`]; SCK, MISO and MOSI on PA5, PA6 and PA7 at
/// AF5. `BM_CS` (PA4) is a push-pull output, driven high. GPDMA1 channel 0
/// transmits and 1 receives; the C uses no DMA here.
#[must_use]
pub fn bm_header_spi(
    spi1: Peri<'static, peripherals::SPI1>,
    cs: Peri<'static, peripherals::PA4>,
    sck: Peri<'static, peripherals::PA5>,
    miso: Peri<'static, peripherals::PA6>,
    mosi: Peri<'static, peripherals::PA7>,
    tx_dma: Peri<'static, peripherals::GPDMA1_CH0>,
    rx_dma: Peri<'static, peripherals::GPDMA1_CH1>,
) -> BmHeaderSpi {
    let cs = Output::new(cs, Level::High, Speed::Low);
    let mut config = spi::Config::default();
    config.frequency = Hertz(BM_HEADER_SPI_HZ);
    config.mode = spi::MODE_0;
    config.bit_order = spi::BitOrder::MsbFirst;
    config.gpio_speed = Speed::Low;
    let spi = Spi::new(spi1, sck, mosi, miso, tx_dma, rx_dma, Irqs, config);
    ExclusiveDevice::new(spi, cs, Delay)
}

/// A [`Devkit`] node with id `node_id`, this chip's UID as its name and
/// `app_name` as its `bm_app_name`, its config partitions loaded from `flash`
/// in the layout `arm-none-eabi-gcc` gives bm_protocol's, and DFU into
/// `slot`, running in `resources`.
///
/// bm_protocol's `bm_app_name` is the app directory's name
/// (`src/CMakeLists.txt`, `get_filename_component(APP_NAME ${APP} NAME)`);
/// a binary here passes `env!("CARGO_BIN_NAME")`, which is the same for
/// `hello_world`.
#[must_use]
pub fn node(
    resources: &'static mut DevkitResources,
    app_name: &'static str,
    node_id: u64,
    flash: Flash,
    rtc: DevkitRtc,
    slot: DevkitSlot,
) -> Devkit {
    let identity = DevkitIdentity::new(node_id, embassy_stm32::uid::uid(), app_name);
    let config = Config::load(Layout::ARM_EABI_GCC, FlashConfigStorage::new(flash));
    Node::new(
        resources,
        Parts::new(identity, rtc).with_config(config).with_dfu(slot),
        PORTS,
    )
}

/// Register echo, sys_info and config_map, in `app_main.cpp`'s order
/// (`defaultTask`, after `bm_sub` of the utc-time topic), after the metrics
/// service `Node::new` registers. A failure is logged and the rest are
/// still registered.
pub fn register_services(node: &mut Devkit) {
    for (name, result) in [
        ("echo", node.register_echo_service()),
        ("sys_info", node.register_sys_info_service()),
        ("config_map", node.register_config_map_service()),
    ] {
        if let Err(error) = result {
            defmt::warn!("register {=str}: {}", name, error);
        }
    }
}

/// This chip's node id.
#[must_use]
pub fn node_id() -> u64 {
    node_id_from_uid(embassy_stm32::uid::uid())
}

/// bm_protocol's `getNodeId`: 64-bit FNV-1a over the 12 UID bytes in memory
/// order, from a starting hash of 0 rather than FNV's offset basis.
#[must_use]
pub const fn node_id_from_uid(uid: &[u8; 12]) -> u64 {
    const FNV_64_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = 0u64;
    let mut i = 0;
    while i < uid.len() {
        hash ^= uid[i] as u64;
        hash = hash.wrapping_mul(FNV_64_PRIME);
        i += 1;
    }
    hash
}

/// bm_protocol's `getUIDStr`: the UID words `UID[2]`, `UID[1]`, `UID[0]`,
/// each as 8 lower-case hex digits.
#[must_use]
pub fn uid_string(uid: &[u8; 12]) -> [u8; 24] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 24];
    for (slot, word) in [2usize, 1, 0].into_iter().enumerate() {
        let value = u32::from_le_bytes([
            uid[4 * word],
            uid[4 * word + 1],
            uid[4 * word + 2],
            uid[4 * word + 3],
        ]);
        for digit in 0..8 {
            let nibble = (value >> (28 - 4 * digit)) & 0xF;
            out[8 * slot + digit] = HEX[nibble as usize];
        }
    }
    out
}

/// What a dev kit says about itself, as `bcl_init` fills `DeviceCfg`: vendor,
/// product and hardware version 0, the placeholder serial number, and the UID
/// string as device name. Firmware version and git SHA are [`version`]'s; the
/// app name, sent in a sys_info reply, is the binary's.
#[derive(Debug, Clone, Copy)]
pub struct DevkitIdentity {
    node_id: u64,
    name: [u8; 24],
    app_name: &'static str,
}

impl DevkitIdentity {
    /// The identity of the chip with this node id and UID, running
    /// `app_name`.
    #[must_use]
    pub fn new(node_id: u64, uid: &[u8; 12], app_name: &'static str) -> Self {
        Self {
            node_id,
            name: uid_string(uid),
            app_name,
        }
    }
}

impl Identity for DevkitIdentity {
    fn node_id(&self) -> u64 {
        self.node_id
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            serial_num: *b"0123456789abcdef",
            git_sha: version::GIT_SHA,
            ver_major: version::MAJOR,
            ver_minor: version::MINOR,
            ver_rev: version::REVISION,
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        version::VERSION_STRING.as_bytes()
    }

    fn device_name(&self) -> &[u8] {
        &self.name
    }

    fn app_name(&self) -> &[u8] {
        self.app_name.as_bytes()
    }
}
