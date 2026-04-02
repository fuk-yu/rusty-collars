use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use log::{error, info, warn};

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};
use crate::protocol::ServerMessage;
use crate::scheduling::PresetEvent;

use super::{free_heap, rf_send_with_led, AppCtx, BroadcastMsg};

const HTTP_BUF_SIZE: usize = 1024;

pub(super) fn run_preset(preset_name: &str, events: Vec<PresetEvent>, ctx: &AppCtx, run_id: u32) {
    let started_at = std::time::Instant::now();

    for event in &events {
        if ctx.preset_run_id.load(Ordering::SeqCst) != run_id {
            return;
        }

        let target = Duration::from_micros(event.time_us);
        let elapsed = started_at.elapsed();
        if target > elapsed {
            let wait = target - elapsed;
            let chunks = wait.as_millis() as u64 / 50;
            for _ in 0..chunks {
                if ctx.preset_run_id.load(Ordering::SeqCst) != run_id {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let remainder = wait - Duration::from_millis(chunks * 50);
            if !remainder.is_zero() {
                std::thread::sleep(remainder);
            }
        }

        if ctx.preset_run_id.load(Ordering::SeqCst) != run_id {
            return;
        }

        if let Err(err) = rf_send_with_led(
            &ctx.rf,
            &ctx.tx_led,
            event.collar_id,
            event.channel,
            event.mode_byte,
            event.intensity,
        ) {
            error!("RF error during preset: {err}");
        }
    }

    info!("Preset '{preset_name}' completed");
}

pub(super) fn ensure_rf_debug_worker(ctx: &AppCtx) {
    if ctx
        .rf_debug_worker_spawned
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let worker_ctx = ctx.clone();
    let result = std::thread::Builder::new()
        .name("rf-debug-rx".into())
        .stack_size(16384)
        .spawn(move || {
            let Some(mut receiver) = worker_ctx.rf_receiver.lock().unwrap().take() else {
                worker_ctx
                    .rf_debug_worker_spawned
                    .store(false, Ordering::SeqCst);
                error!("RF debug receiver missing when worker started");
                return;
            };

            info!("RF debug worker started");
            loop {
                if !worker_ctx.rf_debug_enabled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(super::RF_DEBUG_DISABLED_SLEEP_MS));
                    continue;
                }

                match receiver.listen_until_disabled(&worker_ctx.rf_debug_enabled) {
                    Ok(Some(event)) => {
                        worker_ctx.rx_led.lock().unwrap().set(true);
                        {
                            let mut domain = worker_ctx.domain.lock().unwrap();
                            domain.rf_debug_events.push_back(event.clone());
                            if domain.rf_debug_events.len() > super::MAX_RF_DEBUG_EVENTS {
                                domain.rf_debug_events.pop_front();
                            }
                        }
                        worker_ctx
                            .broadcast_tx
                            .try_broadcast(BroadcastMsg {
                                json: Arc::from(
                                    serde_json::to_string(&ServerMessage::RfDebugEvent {
                                        event: &event,
                                    })
                                    .unwrap(),
                                ),
                                rf_debug: true,
                            })
                            .ok();
                        std::thread::sleep(Duration::from_millis(50));
                        worker_ctx.rx_led.lock().unwrap().set(false);
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
        ctx.rf_debug_worker_spawned.store(false, Ordering::SeqCst);
        error!("Failed to spawn RF debug worker: {err}");
    }
}

pub fn run_server(ctx: AppCtx) -> Result<()> {
    let max_clients = ctx.domain.lock().unwrap().device_settings.max_clients as u32;
    let app_ctx = ctx;
    let base_app = super::http::make_app();
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
                    let conn_state = super::ConnectionState {
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
