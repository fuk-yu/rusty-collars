use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{error, info, warn};

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};
use crate::scheduling::PresetEvent;
use rusty_collars_core::rf_timing::RF_COMMAND_TRANSMIT_DURATION_US;

use super::{free_heap, rf_send_with_led, ActionKey, AppCtx};

const HTTP_BUF_SIZE: usize = 1024;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_millis(10);
const TX_DURATION: Duration = Duration::from_micros(RF_COMMAND_TRANSMIT_DURATION_US);

struct ActivePreset {
    events: Vec<PresetEvent>,
    preset_name: String,
    run_id: u32,
    started_at: Instant,
    event_index: usize,
}

#[derive(Clone)]
struct ActionSnapshot {
    key: ActionKey,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
}

pub(super) fn run_transmission_worker(ctx: &AppCtx) {
    info!("Transmission worker started");

    let mut active_preset: Option<ActivePreset> = None;
    let mut last_seen_run_id = ctx.worker.preset_run_id.load(Ordering::SeqCst);
    let mut round_robin_idx: usize = 0;

    loop {
        // Remove expired manual actions
        drain_expired_actions(ctx);

        // Check for preset changes
        let current_run_id = ctx.worker.preset_run_id.load(Ordering::SeqCst);
        if current_run_id != last_seen_run_id {
            last_seen_run_id = current_run_id;
            let pending = ctx.domain.lock().unwrap().pending_preset.take();
            if let Some(pending) = pending {
                info!("Worker: starting preset '{}'", pending.preset_name);
                active_preset = Some(ActivePreset {
                    events: pending.events,
                    preset_name: pending.preset_name,
                    run_id: current_run_id,
                    started_at: Instant::now(),
                    event_index: 0,
                });
            } else {
                active_preset = None;
            }
        }

        // Try to transmit a preset event if one is due
        if let Some(ref mut preset) = active_preset {
            if preset.event_index < preset.events.len() {
                let event = &preset.events[preset.event_index];
                let target = Duration::from_micros(event.time_us);
                let elapsed = preset.started_at.elapsed();

                if elapsed >= target {
                    // Event is due — transmit it
                    if let Err(err) = rf_send_with_led(
                        &ctx.hardware.rf,
                        &ctx.hardware.tx_led,
                        event.collar_id,
                        event.channel,
                        event.mode_byte,
                        event.intensity,
                    ) {
                        error!("RF error during preset: {err}");
                    }
                    preset.event_index += 1;

                    // Check if preset completed
                    if preset.event_index >= preset.events.len() {
                        info!("Preset '{}' completed", preset.preset_name);
                        let name = preset.preset_name.clone();
                        let run_id = preset.run_id;
                        active_preset = None;
                        // Clear preset_name if this preset is still the active one
                        if ctx.worker.preset_run_id.load(Ordering::SeqCst) == run_id {
                            let mut domain = ctx.domain.lock().unwrap();
                            if domain.preset_name.as_deref() == Some(&name) {
                                domain.preset_name = None;
                            }
                        }
                        ctx.broadcast_state();
                    }
                    continue;
                }

                // Event is in the future — can we fit a manual action in the gap?
                let time_until_event = target - elapsed;
                if time_until_event > TX_DURATION {
                    if let Some(action) = next_manual_action(ctx, &mut round_robin_idx) {
                        transmit_action(ctx, &action);
                        continue;
                    }
                }

                // Wait for the preset event (interruptible via condvar for cancellation)
                let wait = time_until_event.min(WORKER_IDLE_TIMEOUT);
                wait_for_notify(ctx, wait);
                continue;
            } else {
                // All events consumed but we didn't detect completion above (shouldn't happen)
                active_preset = None;
            }
        }

        // No preset — fill with manual actions
        if let Some(action) = next_manual_action(ctx, &mut round_robin_idx) {
            transmit_action(ctx, &action);
            continue;
        }

        // Nothing to do — wait for notification
        wait_for_notify(ctx, WORKER_IDLE_TIMEOUT);
    }
}

fn drain_expired_actions(ctx: &AppCtx) {
    let now = Instant::now();
    let expired: Vec<(ActionKey, super::ActiveActionHandle)> = {
        let mut active = ctx.worker.active_actions.lock().unwrap();
        let expired_keys: Vec<ActionKey> = active
            .iter()
            .filter(|(_, handle)| matches!(handle.deadline, Some(deadline) if now >= deadline))
            .map(|(key, _)| key.clone())
            .collect();
        expired_keys
            .into_iter()
            .filter_map(|k| {
                let handle = active.remove(&k)?;
                Some((k, handle))
            })
            .collect()
    };
    for (key, handle) in &expired {
        let elapsed_ms = handle
            .started_at
            .elapsed()
            .as_millis()
            .min(u32::MAX as u128) as u32;
        ctx.record_event(
            handle.source,
            crate::protocol::EventLogEntryKind::Action {
                collar_name: key.collar_name.clone(),
                mode: key.mode,
                intensity: key.mode.has_intensity().then_some(handle.intensity),
                duration_ms: elapsed_ms,
            },
        );
    }
}

fn next_manual_action(ctx: &AppCtx, round_robin_idx: &mut usize) -> Option<ActionSnapshot> {
    let active = ctx.worker.active_actions.lock().unwrap();
    if active.is_empty() {
        return None;
    }
    // Collect keys for stable round-robin ordering
    let mut keys: Vec<&ActionKey> = active.keys().collect();
    keys.sort_by(|a, b| {
        a.collar_name
            .cmp(&b.collar_name)
            .then(a.mode.to_rf_byte().cmp(&b.mode.to_rf_byte()))
    });

    *round_robin_idx = *round_robin_idx % keys.len();
    let key = keys[*round_robin_idx];
    let handle = &active[key];
    let snapshot = ActionSnapshot {
        key: key.clone(),
        collar_id: handle.collar_id,
        channel: handle.channel,
        mode_byte: handle.mode_byte,
        intensity: handle.intensity,
    };
    *round_robin_idx = (*round_robin_idx + 1) % keys.len();
    Some(snapshot)
}

fn transmit_action(ctx: &AppCtx, action: &ActionSnapshot) {
    if let Err(err) = rf_send_with_led(
        &ctx.hardware.rf,
        &ctx.hardware.tx_led,
        action.collar_id,
        action.channel,
        action.mode_byte,
        action.intensity,
    ) {
        error!(
            "RF error during action ({} {:?}): {err}",
            action.key.collar_name, action.key.mode
        );
    }
}

fn wait_for_notify(ctx: &AppCtx, timeout: Duration) {
    let lock = ctx.worker.worker_notify.0.lock().unwrap();
    let _ = ctx
        .worker
        .worker_notify
        .1
        .wait_timeout(lock, timeout)
        .unwrap();
}

pub(super) fn ensure_rf_debug_worker(ctx: &AppCtx) {
    if ctx
        .debug
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
            let Some(mut receiver) = worker_ctx.hardware.rf_receiver.lock().unwrap().take() else {
                worker_ctx
                    .debug
                    .rf_debug_worker_spawned
                    .store(false, Ordering::SeqCst);
                error!("RF debug receiver missing when worker started");
                return;
            };

            info!("RF debug worker started");
            loop {
                if !worker_ctx.debug.rf_debug_enabled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(super::RF_DEBUG_DISABLED_SLEEP_MS));
                    continue;
                }

                match receiver.listen_until_disabled(&worker_ctx.debug.rf_debug_enabled) {
                    Ok(Some(event)) => {
                        worker_ctx.hardware.rx_led.lock().unwrap().set(true);
                        worker_ctx.push_rf_debug_event(event);
                        std::thread::sleep(Duration::from_millis(50));
                        worker_ctx.hardware.rx_led.lock().unwrap().set(false);
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
        ctx.debug
            .rf_debug_worker_spawned
            .store(false, Ordering::SeqCst);
        error!("Failed to spawn RF debug worker: {err}");
    }
}

pub fn run_server(ctx: AppCtx) -> Result<()> {
    let worker_ctx = ctx.clone();
    std::thread::Builder::new()
        .name("rf-tx-worker".into())
        .stack_size(32768)
        .spawn(move || run_transmission_worker(&worker_ctx))
        .expect("failed to spawn RF transmission worker");

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
