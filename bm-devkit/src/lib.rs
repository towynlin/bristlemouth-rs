//! Board support for the Bristlemouth dev kit's mote: an STM32U575CI with an
//! ADIN2111 on SPI3.
//!
//! [`start`] brings the board up and returns a [`Board`]: the ADIN2111 as a
//! [`bm_stack::Phy`], the driver runner the firmware must spawn, and the node
//! id the C firmware would use on the same chip, and the NOR flash that holds
//! the config partitions, and the RTC. [`Devkit`] is the node type with this
//! board's identity, clock and config store.
//!
//! Every pin, clock and sequence here is taken from bm_protocol's
//! `bm_mote_v1.0` BSP; `README.md` in this crate records each with its source.

#![no_std]
#![warn(missing_docs)]

pub mod rtc;
pub mod storage;
pub mod w25;

use bm_phy_adin2111::{Adin2111Phy, Runner, State, Tc6};
use bm_stack::node::{
    INFO_REQUESTS_DEFAULT, PING_PAYLOAD_BYTES, RESOURCE_REQUESTS_DEFAULT, RESOURCES_DEFAULT,
    SUBSCRIPTIONS_DEFAULT,
};
use bm_stack::{Config, Identity, Node};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::bcmp::info::CACHED_STRING_BYTES;
use bm_wire::bcmp::resource::RESOURCE_NAME_BYTES;
use bm_wire::configuration::Layout;
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::{Async, Blocking};
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{bind_interrupts, dma, interrupt, peripherals};
use embassy_time::{Delay, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;

use crate::rtc::DevkitRtc;
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

bind_interrupts!(
    /// The interrupts the ADIN2111 needs: its `INT` line on EXTI8, and the
    /// two GPDMA channels SPI3 uses, as in `bm_mote_v1.0.ioc`.
    pub struct Irqs {
        EXTI8 => exti::InterruptHandler<interrupt::typelevel::EXTI8>;
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

/// A node on this board: [`DevkitIdentity`], the RTC, and the config
/// partitions in NOR flash.
pub type Devkit = Node<
    DevkitIdentity,
    DevkitRtc,
    4,
    4,
    PING_PAYLOAD_BYTES,
    INFO_REQUESTS_DEFAULT,
    CACHED_STRING_BYTES,
    RESOURCES_DEFAULT,
    RESOURCE_NAME_BYTES,
    RESOURCE_REQUESTS_DEFAULT,
    SUBSCRIPTIONS_DEFAULT,
    Config<DevkitConfigStorage>,
>;

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
}

/// The clock tree `SystemClock_Config` sets up: MSIS at 48 MHz, PLL1 `/3 *10
/// /1`, SYSCLK 160 MHz; LSE on, drive high, clocking the RTC.
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

/// Initialise the chip with [`config`], power the ADIN2111 and bring it up,
/// set up SPI2 for the NOR flash, and start the RTC.
///
/// Consumes every peripheral; the ones not listed in `README.md` are dropped.
///
/// # Panics
///
/// If called twice, and wherever the driver panics: an ADIN2111 that never
/// answers or never completes reset.
pub async fn start() -> Board {
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();

    let p = embassy_stm32::init(config());
    let node_id = node_id();
    let rtc = DevkitRtc::new(p.RTC);

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

    let (phy, adin_runner) =
        bm_phy_adin2111::for_node(node_id, STATE.init(State::new()), spi, int, reset, false).await;

    let flash_cs = Output::new(p.PA8, Level::High, Speed::High);
    let mut flash_config = spi::Config::default();
    flash_config.frequency = Hertz(FLASH_SPI_HZ);
    let flash_spi = Spi::new_blocking(p.SPI2, p.PB13, p.PB15, p.PB14, flash_config);
    let flash = W25::new(ExclusiveDevice::new(flash_spi, flash_cs, Delay), Delay);

    Board {
        node_id,
        phy,
        adin_runner,
        adin_power,
        flash,
        rtc,
    }
}

/// A [`Devkit`] node with id `node_id` and this chip's UID as its name, its
/// config partitions loaded from `flash` in the layout `arm-none-eabi-gcc`
/// gives bm_protocol's.
#[must_use]
pub fn node(node_id: u64, flash: Flash, rtc: DevkitRtc) -> Devkit {
    Node::with_config(
        DevkitIdentity::new(node_id, embassy_stm32::uid::uid()),
        rtc,
        Config::load(Layout::ARM_EABI_GCC, FlashConfigStorage::new(flash)),
        PORTS,
    )
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

/// The first 8 hex digits of the commit built, as bm_protocol reports its
/// own; 0 outside a git checkout.
const GIT_SHA: u32 = match u32::from_str_radix(env!("BM_DEVKIT_GIT_SHA"), 16) {
    Ok(sha) => sha,
    Err(_) => 0,
};

/// A Cargo version component as a `u8`, saturating, as `DeviceInfo` carries it.
const fn version_part(part: &str) -> u8 {
    match u8::from_str_radix(part, 10) {
        Ok(n) => n,
        Err(_) => u8::MAX,
    }
}

/// What a dev kit says about itself, as `bcl_init` fills `DeviceCfg`: vendor,
/// product and hardware version 0, the placeholder serial number, and the UID
/// string as device name. Firmware version and git SHA are this crate's.
#[derive(Debug, Clone, Copy)]
pub struct DevkitIdentity {
    node_id: u64,
    name: [u8; 24],
}

impl DevkitIdentity {
    /// The identity of the chip with this node id and UID.
    #[must_use]
    pub fn new(node_id: u64, uid: &[u8; 12]) -> Self {
        Self {
            node_id,
            name: uid_string(uid),
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
            git_sha: GIT_SHA,
            ver_major: version_part(env!("CARGO_PKG_VERSION_MAJOR")),
            ver_minor: version_part(env!("CARGO_PKG_VERSION_MINOR")),
            ver_rev: version_part(env!("CARGO_PKG_VERSION_PATCH")),
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        concat!(
            "bm-devkit@v",
            env!("CARGO_PKG_VERSION"),
            "+",
            env!("BM_DEVKIT_GIT_SHA")
        )
        .as_bytes()
    }

    fn device_name(&self) -> &[u8] {
        &self.name
    }
}
