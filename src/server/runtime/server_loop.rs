use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use log::{error, info, warn};

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};

use super::super::{free_heap, AppCtx, ConnectionState, RF_DEBUG_DISABLED_SLEEP_MS};

const HTTP_BUF_SIZE: usize = 1024;

pub(super) fn ensure_rf_debug_worker(ctx: &AppCtx) {
    if !ctx.try_mark_rf_debug_worker_spawned() {
        return;
    }

    let worker_ctx = ctx.clone();
    let result = std::thread::Builder::new()
        .name("rf-debug-rx".into())
        .stack_size(16384)
        .spawn(move || {
            let Some(mut receiver) = worker_ctx.take_rf_receiver() else {
                worker_ctx.clear_rf_debug_worker_spawned();
                error!("RF debug receiver missing when worker started");
                return;
            };

            info!("RF debug worker started");
            let enabled = worker_ctx.rf_debug_enabled_handle();
            loop {
                if !enabled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(RF_DEBUG_DISABLED_SLEEP_MS));
                    continue;
                }

                match receiver.listen_until_disabled(enabled.as_ref()) {
                    Ok(Some(event)) => {
                        worker_ctx.set_rx_led(true);
                        worker_ctx.push_rf_debug_event(event);
                        std::thread::sleep(Duration::from_millis(50));
                        worker_ctx.set_rx_led(false);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        error!("RF debug receiver error: {err:#}");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        });

    if let Err(err) = result {
        ctx.clear_rf_debug_worker_spawned();
        error!("Failed to spawn RF debug worker: {err}");
    }
}

pub fn run_server(ctx: AppCtx) -> Result<()> {
    let max_clients = ctx.max_clients();
    let app_ctx = ctx;
    let base_app = super::super::http::make_app();
    let shared_app = base_app.shared();
    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: picoserve::time::Duration::from_secs(5),
        persistent_start_read_request: picoserve::time::Duration::from_secs(5),
        read_request: picoserve::time::Duration::from_secs(1),
        write: picoserve::time::Duration::from_secs(1),
    })
    .close_connection_after_response();

    let executor = async_executor::LocalExecutor::new();
    let active = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let next_conn_id = std::rc::Rc::new(std::cell::Cell::new(1u32));

    futures_lite::future::block_on(executor.run(async {
        let listener = async_io::Async::<std::net::TcpListener>::bind(([0, 0, 0, 0], 80))
            .expect("failed to bind port 80");
        info!("picoserve listening on port 80 (max {max_clients} concurrent)");

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let count = active.get();
                    if count >= max_clients {
                        warn!("Rejecting {addr}: at capacity ({count}/{max_clients})");
                        drop(stream);
                        continue;
                    }

                    let conn_id = next_conn_id.get();
                    next_conn_id.set(conn_id + 1);
                    let free_heap = free_heap();
                    info!(
                        "[#{conn_id}] Connection from {addr} ({count}/{max_clients}, heap: {free_heap}B)"
                    );

                    let config_ref = &config;
                    let active_ref = active.clone();
                    active_ref.set(active_ref.get() + 1);
                    let conn_state = ConnectionState {
                        app: app_ctx.clone(),
                        conn_id,
                        conn_addr: addr.ip().to_string(),
                    };

                    executor
                        .spawn(async move {
                            let app = shared_app.with_state(conn_state);
                            let socket = AsyncIoSocket(stream);
                            let mut http_buf = vec![0u8; HTTP_BUF_SIZE];
                            let server = picoserve::Server::custom(
                                &app,
                                AsyncIoTimer,
                                config_ref,
                                &mut http_buf,
                            );
                            match server.serve(socket).await {
                                Ok(_) => info!("[#{conn_id}] Connection from {addr} closed"),
                                Err(err) => {
                                    warn!("[#{conn_id}] Connection from {addr} error: {err:?}")
                                }
                            }
                            active_ref.set(active_ref.get() - 1);
                        })
                        .detach();
                }
                Err(err) => {
                    error!("Accept error: {err}");
                    async_io::Timer::after(Duration::from_millis(100)).await;
                }
            }
        }
    }))
}
