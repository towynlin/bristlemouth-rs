//! The hello-world app: subscribes to [`SUBSCRIPTION`], sends `hello world`
//! to the Spotter console with `spotter_log` every 10 s, and logs over defmt
//! what a bench check reads:
//! heartbeats, echo requests, and publications received. It also
//! subscribes to [`utc_time::TOPIC`] and sets the node's clock from it, as C
//! dev kits do, and logs BCMP time messages, each set, and every 10 s the
//! node's RTC reading. It takes updates over DFU into slot 2 and logs their
//! progress; the slot's erase time also goes to the Spotter console, as
//! `dfu: slot 2 erased in <n> ms`, in place of the next `hello world`.
//!
//! It lists the services a C dev kit lists, in `app_main.cpp`'s order:
//! metrics at construction, then echo, sys_info and config_map, so a
//! Bridge's topology, sensor and metrics samplers can ask it. Its sys_info
//! app name is `hello_world`, as bm_protocol's `bm_devkit/hello_world`'s is.
//!
//! ```text
//! cd bm-devkit && cargo run --release --bin hello_world
//! ```

#![no_std]
#![no_main]

use core::fmt::{self, Write};

use bm_devkit::{AdinRunner, Devkit, DevkitResources, slot};
use bm_stack::utc_time::{self, UtcTimeSetter};
use bm_stack::{App, Event, Outbound, Rtc};
use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::time::{SystemTimeRequest, SystemTimeResponse, SystemTimeSet};
use bm_wire::configuration::Partition;
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

/// The DFU messages logged: all but the per-chunk `DFU_PAYLOAD_REQ` and
/// `DFU_PAYLOAD`. `bm_devkit::slot` logs the erase and every 64 KiB written.
const DFU_MESSAGES: [MessageType; 7] = [
    MessageType::DFU_START,
    MessageType::DFU_END,
    MessageType::DFU_ACK,
    MessageType::DFU_ABORT,
    MessageType::DFU_REBOOT_REQ,
    MessageType::DFU_REBOOT,
    MessageType::DFU_BOOT_COMPLETE,
];

#[embassy_executor::task]
async fn adin(runner: AdinRunner) -> ! {
    runner.run().await
}

/// A line of text for `spotter_log`, formatted without `alloc`. Truncated at
/// its capacity.
struct Text {
    buf: [u8; 48],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self {
            buf: [0; 48],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl fmt::Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// `text` to the Spotter console with `spotter_log`, logged over defmt too.
fn log_to_spotter<'n>(node: &'n mut Devkit, text: &[u8]) -> Option<Outbound<'n>> {
    match node.spotter_log(0, None, USE_TIMESTAMP, text) {
        Ok(outbound) => {
            info!("spotter_log: {=[u8]:a}", text);
            Some(outbound)
        }
        Err(error) => {
            warn!("spotter_log: {}", error);
            None
        }
    }
}

struct Hello {
    ticker: Ticker,
    utc_time: UtcTimeSetter,
}

impl App<Devkit> for Hello {
    async fn ready(&mut self) {
        if self.utc_time.is_pending() {
            return;
        }
        // Cancel-safe: the deadline lives in the ticker.
        self.ticker.next().await;
    }

    fn act<'n>(&mut self, node: &'n mut Devkit, _now_ms: u32) -> Option<Outbound<'n>> {
        if let Some((time, set)) = self.utc_time.apply(node.rtc_mut()) {
            if set {
                info!("rtc set to {=u64} us", time.to_utc_micros());
            } else {
                warn!("rtc refused {=u64} us", time.to_utc_micros());
            }
            return None;
        }
        // In place of this tick's `hello world`: the Spotter console is
        // where the erase time can be read without a probe.
        if let Some(ms) = slot::take_erase_ms() {
            let mut text = Text::new();
            let _ = write!(text, "dfu: slot 2 erased in {ms} ms");
            return log_to_spotter(node, text.as_bytes());
        }
        match node.rtc().get() {
            Some(time) => info!("rtc: {=u64} us", time.to_utc_micros()),
            None => info!("rtc: not set"),
        }
        log_to_spotter(node, HELLO)
    }

    fn on_event(&mut self, event: Event<'_>) {
        if let Some(Err(error)) = self.utc_time.on_event(&event) {
            warn!("utc-time: {}", error);
        }
        match event {
            Event::Message {
                message_type: MessageType::HEARTBEAT,
                source,
                ..
            } => {
                info!("heartbeat from {=u64:016x}", source);
            }
            Event::Message {
                message_type: MessageType::ECHO_REQUEST,
                source,
                ..
            } => {
                info!("echo request from {=u64:016x}", source);
            }
            Event::Message {
                message_type: MessageType::SYSTEM_TIME_REQUEST,
                source,
                payload,
                ..
            } => match SystemTimeRequest::decode(payload) {
                Ok(request) => info!(
                    "time request from {=u64:016x} for {=u64:016x}",
                    source, request.header.target_node_id
                ),
                Err(_) => warn!("short time request from {=u64:016x}", source),
            },
            Event::Message {
                message_type: MessageType::SYSTEM_TIME_SET,
                source,
                payload,
                ..
            } => match SystemTimeSet::decode(payload) {
                Ok(set) => info!(
                    "time set from {=u64:016x} for {=u64:016x}: {=u64} us",
                    source, set.header.target_node_id, set.utc_time_us
                ),
                Err(_) => warn!("short time set from {=u64:016x}", source),
            },
            Event::Message {
                message_type: MessageType::SYSTEM_TIME_RESPONSE,
                source,
                payload,
                ..
            } => match SystemTimeResponse::decode(payload) {
                Ok(response) => info!(
                    "time response from {=u64:016x}: {=u64} us",
                    source, response.utc_time_us
                ),
                Err(_) => warn!("short time response from {=u64:016x}", source),
            },
            Event::Message {
                message_type,
                source,
                ..
            } if DFU_MESSAGES.contains(&message_type) => {
                info!("dfu: {=u16:#x} from {=u64:016x}", message_type.0, source);
            }
            Event::DfuUpdateFinished(finished) => {
                info!("dfu: finished {}", finished);
            }
            Event::Publication {
                source,
                subscription,
                topic,
                kind,
                version,
                data,
            } => {
                info!(
                    "publication from {=u64:016x} on {=[u8]:a} via {=[u8]:a}, type {=u8} version {=u8}: {=[u8]:a}",
                    source, topic, subscription, kind, version, data
                );
            }
            _ => {}
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut board = bm_devkit::start(spawner).await;
    info!("node id {=u64:016x}", board.node_id);
    info!("reset reason: {}", board.reset_reason);
    spawner.spawn(adin(board.adin_runner).expect("one adin task"));

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
    // `app_main.cpp:412-415`: the utc-time subscription, then the services.
    for topic in [SUBSCRIPTION, utc_time::TOPIC] {
        if let Err(error) = node.subscribe(topic) {
            warn!("subscribe {=[u8]:a}: {}", topic, error);
        }
    }
    bm_devkit::register_services(node);
    for (name, _) in node.service_table().iter() {
        info!("service {=[u8]:a}", name);
    }
    info!(
        "sys_config_crc {=u32:08x}",
        node.config()
            .store
            .partition(Partition::System)
            .cbor_map_crc32()
    );
    let mut app = Hello {
        ticker: Ticker::every(Duration::from_secs(10)),
        utc_time: UtcTimeSetter::new(),
    };
    let error = node.run_app(&mut board.phy, &mut app).await;
    warn!("node stopped: {}", error);
}
