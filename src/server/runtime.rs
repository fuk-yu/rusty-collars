use std::thread::JoinHandle;

use super::AppCtx;

mod app_worker;
mod server_loop;
mod transmission;

use app_worker::run_app_worker;
pub use server_loop::run_server;
use transmission::run_transmission_worker;

pub struct TransmissionWorkerHandle {
    _join: JoinHandle<()>,
}

pub struct AppWorkerHandle {
    _join: JoinHandle<()>,
}

pub fn start_app_worker(ctx: AppCtx) -> AppWorkerHandle {
    let command_rx = ctx.take_app_rx();
    let join = std::thread::Builder::new()
        .name("app-worker".into())
        .stack_size(32768)
        .spawn(move || run_app_worker(ctx, command_rx))
        .expect("failed to spawn app worker");

    AppWorkerHandle { _join: join }
}

pub fn start_transmission_worker(ctx: AppCtx) -> TransmissionWorkerHandle {
    let command_rx = ctx.take_transmission_rx();
    let join = std::thread::Builder::new()
        .name("rf-tx-worker".into())
        .stack_size(32768)
        .spawn(move || run_transmission_worker(&ctx, command_rx))
        .expect("failed to spawn RF transmission worker");

    TransmissionWorkerHandle { _join: join }
}

pub(super) fn ensure_rf_debug_worker(ctx: &AppCtx) {
    server_loop::ensure_rf_debug_worker(ctx);
}
