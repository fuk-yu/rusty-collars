#![allow(unexpected_cfgs)]

use anyhow::Result;
use log::info;

mod app;
mod async_runtime;
mod build_info;
mod error;
mod led;
#[cfg(has_mqtt)]
mod mqtt;
#[allow(unexpected_cfgs)]
mod net;
mod ota;
mod remote_control;
mod repository;
mod rf;
mod server;
mod storage;
mod time_sync;
#[cfg(has_wifi)]
mod wifi;

pub use rusty_collars_core::{protocol, scheduling, validation};

fn main() -> Result<()> {
    let application = app::ApplicationBuilder::new().build()?;
    let mut running = application.start()?;

    running.install_watchdog();

    info!("Server running. Waiting for connections...");

    loop {
        running.poll();
        running.reset_watchdog();
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
