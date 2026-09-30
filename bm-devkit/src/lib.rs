//! Board support for the Bristlemouth dev kit's mote: an STM32U575CI with an
//! ADIN2111 on SPI3.
//!
//! [`start`] brings the board up and returns a [`Board`]: the ADIN2111 as a
//! [`bm_stack::Phy`], the driver runner the firmware must spawn, and the node
//! id the C firmware would use on the same chip. [`Devkit`] is the node type
//! with this board's identity and a RAM config store.
//!
//! Every pin, clock and sequence here is taken from bm_protocol's
//! `bm_mote_v1.0` BSP; `README.md` in this crate records each with its source.

#![no_std]
#![warn(missing_docs)]

use bm_phy_adin2111::{Adin2111Phy, Runner, State, Tc6};
use bm_stack::node::{
    INFO_REQUESTS_DEFAULT, PING_PAYLOAD_BYTES, RESOURCE_REQUESTS_DEFAULT, RESOURCES_DEFAULT,
    SUBSCRIPTIONS_DEFAULT,
};
use bm_stack::{Config, Identity, Node, RamConfigStorage, SoftRtc};
use bm_wire::bcmp::DeviceInfo;
use bm_wire::bcmp::info::CACHED_STRING_BYTES;
use bm_wire::bcmp::resource::RESOURCE_NAME_BYTES;
use bm_wire::configuration::Layout;
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::Async;
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{bind_interrupts, dma, interrupt, peripherals};
use embassy_time::{Delay, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;

/// Bristlemouth ports on the ADIN2111.
pub const PORTS: u8 = 2;

/// SPI3 clock: SYSCLK 160 MHz through `SPI_BAUDRATEPRESCALER_8`, as
/// `MX_SPI3_Init` configures it.
pub const ADIN_SPI_HZ: u32 = 20_000_000;

/// Time from `ADIN_PWR` high to the driver's reset pulse. The C firmware
/// raises `ADIN_PWR` in `bspInit` and pulses reset much later, in
/// `bcl_power_callback`; this is the settle time the embassy example uses.
pub const ADIN_POWER_SETTLE_MS: u64 = 90;

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

/// A node on this board: [`DevkitIdentity`], a clock set over the network,
/// and a config store in RAM, lost on reset.
pub type Devkit = Node<
    DevkitIdentity,
    SoftRtc,
    4,
    4,
    PING_PAYLOAD_BYTES,
    INFO_REQUESTS_DEFAULT,
    CACHED_STRING_BYTES,
    RESOURCES_DEFAULT,
    RESOURCE_NAME_BYTES,
    RESOURCE_REQUESTS_DEFAULT,
    SUBSCRIPTIONS_DEFAULT,
    Config<RamConfigStorage>,
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
}

/// The clock tree `SystemClock_Config` sets up: MSIS at 48 MHz, PLL1 `/3 *10
/// /1`, SYSCLK 160 MHz.
#[must_use]
pub fn config() -> embassy_stm32::Config {
    use embassy_stm32::rcc::{MSIRange, Pll, PllDiv, PllMul, PllPreDiv, PllSource, Sysclk};

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
    config
}

/// Initialise the chip with [`config`], power the ADIN2111 and bring it up.
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

    Board {
        node_id,
        phy,
        adin_runner,
        adin_power,
    }
}

/// A [`Devkit`] node with id `node_id` and this chip's UID as its name, and
/// an empty RAM config store in the layout `arm-none-eabi-gcc` gives
/// bm_protocol's.
#[must_use]
pub fn node(node_id: u64) -> Devkit {
    Node::with_config(
        DevkitIdentity::new(node_id, embassy_stm32::uid::uid()),
        SoftRtc::new(),
        Config::load(Layout::ARM_EABI_GCC, RamConfigStorage::new()),
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

/// What a dev kit says about itself, as `bcl_init` fills `DeviceCfg`: vendor,
/// product and hardware version 0, the placeholder serial number, and the UID
/// string as device name.
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
            ..DeviceInfo::default()
        }
    }

    fn version_string(&self) -> &[u8] {
        concat!("bm-devkit ", env!("CARGO_PKG_VERSION")).as_bytes()
    }

    fn device_name(&self) -> &[u8] {
        &self.name
    }
}
