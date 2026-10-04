//! The IWDG the bootloader starts before it jumps here (`MX_IWDG_Init`,
//! bm_protocol `src/bsp/bootloader/Core/Src/iwdg.c`): prescaler 32, reload
//! 4095, about 4.1 s on LSI. It cannot be stopped and is not reconfigured
//! here.
//!
//! The C feeds it from its lowest-priority task
//! (`src/lib/common/watchdog.c`); here [`task`] does, on the one executor,
//! so a task that never yields is reset as a C task that never blocks is.

use embassy_stm32::pac::IWDG;
use embassy_stm32::pac::iwdg::vals::Key;
use embassy_time::{Duration, Ticker};

/// The period [`task`] feeds at.
pub const FEED_PERIOD: Duration = Duration::from_secs(1);

/// Reload the counter. For code that keeps the executor longer than the
/// timeout, as a slot erase does.
pub fn feed() {
    IWDG.kr().write(|w| w.set_key(Key::Reset));
}

/// Feeds the watchdog every [`FEED_PERIOD`]. [`crate::start`] spawns it.
#[embassy_executor::task]
pub async fn task() -> ! {
    let mut ticker = Ticker::every(FEED_PERIOD);
    loop {
        feed();
        ticker.next().await;
    }
}
