//! The soft module app, without its sensor: bm_protocol's
//! `src/apps/bm_soft_module` on the `bm_mote_spi_v1_0` BSP, as far as a
//! Bridge sees it before the first temperature reading.
//!
//! | Item | Here |
//! |---|---|
//! | sys_info app name | `bm_soft_module`, the binary's name |
//! | Services | metrics at construction, then echo, sys_info, config_map, in `app_main.cpp`'s order |
//! | Subscriptions | [`utc_time::TOPIC`] only; sets the RTC from it |
//! | DFU | into slot 2 |
//! | BM header | SPI1 at 1.25 MHz, mode 0, `BM_CS` high ([`bm_devkit::bm_header_spi`]); nothing on it yet |
//!
//! ```text
//! cd bm-devkit && cargo run --release --bin bm_soft_module
//! ```

#![no_std]
#![no_main]

use bm_devkit::{AdinRunner, Devkit, DevkitResources};
use bm_stack::utc_time::{self, UtcTimeSetter};
use bm_stack::{App, Event, Outbound};
use defmt::{info, warn};
use embassy_executor::Spawner;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::task]
async fn adin(runner: AdinRunner) -> ! {
    runner.run().await
}

/// Sets the RTC from `spotter/utc-time`; nothing else yet.
struct Soft {
    utc_time: UtcTimeSetter,
}

impl App<Devkit> for Soft {
    async fn ready(&mut self) {
        if self.utc_time.is_pending() {
            return;
        }
        core::future::pending::<()>().await;
    }

    fn act<'n>(&mut self, node: &'n mut Devkit, _now_ms: u32) -> Option<Outbound<'n>> {
        if let Some((time, set)) = self.utc_time.apply(node.rtc_mut()) {
            if set {
                info!("rtc set to {=u64} us", time.to_utc_micros());
            } else {
                warn!("rtc refused {=u64} us", time.to_utc_micros());
            }
        }
        None
    }

    fn on_event(&mut self, event: Event<'_>) {
        if let Some(Err(error)) = self.utc_time.on_event(&event) {
            warn!("utc-time: {}", error);
        }
        if let Event::DfuUpdateFinished(finished) = event {
            info!("dfu: finished {}", finished);
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut board = bm_devkit::start(spawner).await;
    info!("node id {=u64:016x}", board.node_id);
    info!("reset reason: {}", board.reset_reason);
    spawner.spawn(adin(board.adin_runner).expect("one adin task"));

    // `bm_mote_spi_v1_0`'s `MX_SPI1_Init`. Held for the TSYS01 driver;
    // dropping it would release the pins.
    let spare = board.spare;
    let _sensor_spi = bm_devkit::bm_header_spi(
        spare.spi1,
        spare.pa4,
        spare.pa5,
        spare.pa6,
        spare.pa7,
        spare.gpdma1_ch0,
        spare.gpdma1_ch1,
    );

    static MEMORY: StaticCell<DevkitResources> = StaticCell::new();
    static NODE: StaticCell<Devkit> = StaticCell::new();
    let memory = MEMORY.init_with(DevkitResources::new);
    let node = NODE.init_with(|| {
        bm_devkit::node(
            memory,
            env!("CARGO_BIN_NAME"),
            board.node_id,
            board.flash,
            board.rtc,
            board.slot,
        )
    });
    // `app_main.cpp:411-414`: the utc-time subscription, then the services.
    if let Err(error) = node.subscribe(utc_time::TOPIC) {
        warn!("subscribe {=[u8]:a}: {}", utc_time::TOPIC, error);
    }
    bm_devkit::register_services(node);
    for (name, _) in node.service_table().iter() {
        info!("service {=[u8]:a}", name);
    }
    let mut app = Soft {
        utc_time: UtcTimeSetter::new(),
    };
    let error = node.run_app(&mut board.phy, &mut app).await;
    warn!("node stopped: {}", error);
}
