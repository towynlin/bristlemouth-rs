//! Board bring-up: a node that heartbeats, answers BCMP, and logs what it
//! hears over defmt. The manual check for card B1 is that a C node on the
//! same bus lists it as a neighbour.
//!
//! ```text
//! cd bm-devkit && cargo run --release --bin bringup
//! ```

#![no_std]
#![no_main]

use bm_devkit::{AdinRunner, Devkit};
use bm_stack::{App, Event, Outbound};
use bm_wire::bcmp::MessageType;
use defmt::{info, warn};
use embassy_executor::Spawner;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::task]
async fn adin(runner: AdinRunner) -> ! {
    runner.run().await
}

/// Never acts; logs the events a bring-up is checked by.
struct Log;

impl App<Devkit> for Log {
    async fn ready(&mut self) {
        core::future::pending::<()>().await;
    }

    fn act<'n>(&mut self, _node: &'n mut Devkit, _now_ms: u32) -> Option<Outbound<'n>> {
        None
    }

    fn on_event(&mut self, event: Event<'_>) {
        match event {
            Event::Message {
                message_type,
                source,
                ..
            } if message_type == MessageType::HEARTBEAT => {
                info!("heartbeat from {=u64:016x}", source);
            }
            Event::DeviceInfo { source, reply } => info!(
                "{=u64:016x} is {=[u8]:a} {=[u8]:a}",
                source, reply.device_name, reply.version_string
            ),
            Event::Publication { source, topic, .. } => {
                info!("publication from {=u64:016x}: {=[u8]:a}", source, topic);
            }
            _ => {}
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut board = bm_devkit::start().await;
    info!("node id {=u64:016x}", board.node_id);
    spawner.spawn(adin(board.adin_runner).expect("one adin task"));

    static NODE: StaticCell<Devkit> = StaticCell::new();
    let node = NODE.init_with(|| bm_devkit::node(board.node_id));
    let error = node.run_app(&mut board.phy, &mut Log).await;
    warn!("node stopped: {}", defmt::Debug2Format(&error));
}
