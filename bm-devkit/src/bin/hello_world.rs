//! The hello-world app: subscribes to [`SUBSCRIPTION`], sends `hello world`
//! to the Spotter console with `spotter_log` every 10 s, and logs over defmt
//! what the four checks of `docs/hello-world-todo.md`'s "The target" are read
//! from: heartbeats, echo requests, and publications received.
//!
//! ```text
//! cd bm-devkit && cargo run --release --bin hello_world
//! ```

#![no_std]
#![no_main]

use bm_devkit::{AdinRunner, Devkit};
use bm_stack::{App, Event, Outbound};
use bm_wire::bcmp::MessageType;
use bm_wire::spotter::USE_TIMESTAMP;
use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

/// Matches every topic a C dev kit's `spotter_log`, `spotter_log_console` and
/// `spotter_tx_data` publish to, so stock C firmware exercises the receive
/// path and the wildcard match.
const SUBSCRIPTION: &[u8] = b"spotter/*";

const HELLO: &[u8] = b"hello world";

#[embassy_executor::task]
async fn adin(runner: AdinRunner) -> ! {
    runner.run().await
}

struct Hello {
    ticker: Ticker,
}

impl App<Devkit> for Hello {
    async fn ready(&mut self) {
        // Cancel-safe: the deadline lives in the ticker.
        self.ticker.next().await;
    }

    fn act<'n>(&mut self, node: &'n mut Devkit, _now_ms: u32) -> Option<Outbound<'n>> {
        match node.spotter_log(0, None, USE_TIMESTAMP, HELLO) {
            Ok(outbound) => {
                info!("spotter_log: {=[u8]:a}", HELLO);
                Some(outbound)
            }
            Err(error) => {
                warn!("spotter_log: {}", defmt::Debug2Format(&error));
                None
            }
        }
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
            Event::Message {
                message_type,
                source,
                ..
            } if message_type == MessageType::ECHO_REQUEST => {
                info!("echo request from {=u64:016x}", source);
            }
            Event::Publication {
                source,
                topic,
                data,
                ..
            } => {
                info!(
                    "publication from {=u64:016x} on {=[u8]:a}: {=[u8]:a}",
                    source, topic, data
                );
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
    let node = NODE.init_with(|| bm_devkit::node(board.node_id, board.flash));
    if let Err(error) = node.subscribe(SUBSCRIPTION) {
        warn!("subscribe: {}", defmt::Debug2Format(&error));
    }
    let mut app = Hello {
        ticker: Ticker::every(Duration::from_secs(10)),
    };
    let error = node.run_app(&mut board.phy, &mut app).await;
    warn!("node stopped: {}", defmt::Debug2Format(&error));
}
