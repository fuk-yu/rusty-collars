use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{error, info, warn};

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};
use crate::protocol::EventLogEntryKind;
use crate::scheduling::PresetEvent;
use rusty_collars_core::rf_timing::RF_COMMAND_TRANSMIT_DURATION_US;

use super::{
    free_heap, rf_send_with_led, ActionKey, ActiveActionHandle, AppCtx, TransmissionCommand,
};

const HTTP_BUF_SIZE: usize = 1024;
const WORKER_IDLE_TIMEOUT: Duration = Duration::from_millis(10);
const TX_DURATION: Duration = Duration::from_micros(RF_COMMAND_TRANSMIT_DURATION_US);

struct ActivePreset {
    events: Vec<PresetEvent>,
    preset_name: String,
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

pub struct TransmissionWorkerHandle {
    _join: JoinHandle<()>,
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

fn run_transmission_worker(ctx: &AppCtx, command_rx: Receiver<TransmissionCommand>) {
    info!("Transmission worker started");

    let mut active_preset: Option<ActivePreset> = None;
    let mut active_actions: HashMap<ActionKey, ActiveActionHandle> = HashMap::new();
    let mut round_robin_idx: usize = 0;

    loop {
        drain_expired_actions(ctx, &mut active_actions);
        drain_pending_commands(ctx, &command_rx, &mut active_preset, &mut active_actions);

        if let Some(ref mut preset) = active_preset {
            if preset.event_index < preset.events.len() {
                let event = &preset.events[preset.event_index];
                let target = Duration::from_micros(event.time_us);
                let elapsed = preset.started_at.elapsed();

                if elapsed >= target {
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

                    if preset.event_index >= preset.events.len() {
                        info!("Preset '{}' completed", preset.preset_name);
                        let completed_name = preset.preset_name.clone();
                        active_preset = None;
                        clear_active_preset_name(ctx, &completed_name);
                        ctx.broadcast_state();
                    }
                    continue;
                }

                let time_until_event = target - elapsed;
                if time_until_event > TX_DURATION {
                    if let Some(action) = next_manual_action(&active_actions, &mut round_robin_idx)
                    {
                        transmit_action(ctx, &action);
                        continue;
                    }
                }

                wait_for_command(
                    ctx,
                    &command_rx,
                    time_until_event.min(WORKER_IDLE_TIMEOUT),
                    &mut active_preset,
                    &mut active_actions,
                );
                continue;
            }

            let completed_name = preset.preset_name.clone();
            active_preset = None;
            clear_active_preset_name(ctx, &completed_name);
            ctx.broadcast_state();
        }

        if let Some(action) = next_manual_action(&active_actions, &mut round_robin_idx) {
            transmit_action(ctx, &action);
            continue;
        }

        wait_for_command(
            ctx,
            &command_rx,
            WORKER_IDLE_TIMEOUT,
            &mut active_preset,
            &mut active_actions,
        );
    }
}

fn drain_expired_actions(
    ctx: &AppCtx,
    active_actions: &mut HashMap<ActionKey, ActiveActionHandle>,
) {
    let now = Instant::now();
    let expired_keys: Vec<ActionKey> = active_actions
        .iter()
        .filter(|(_, handle)| matches!(handle.deadline, Some(deadline) if now >= deadline))
        .map(|(key, _)| key.clone())
        .collect();

    for key in expired_keys {
        if let Some(handle) = active_actions.remove(&key) {
            record_action_completion(ctx, &key, &handle);
        }
    }
}

fn drain_pending_commands(
    ctx: &AppCtx,
    command_rx: &Receiver<TransmissionCommand>,
    active_preset: &mut Option<ActivePreset>,
    active_actions: &mut HashMap<ActionKey, ActiveActionHandle>,
) {
    while let Ok(command) = command_rx.try_recv() {
        apply_command(ctx, command, active_preset, active_actions);
    }
}

fn wait_for_command(
    ctx: &AppCtx,
    command_rx: &Receiver<TransmissionCommand>,
    timeout: Duration,
    active_preset: &mut Option<ActivePreset>,
    active_actions: &mut HashMap<ActionKey, ActiveActionHandle>,
) {
    match command_rx.recv_timeout(timeout) {
        Ok(command) => apply_command(ctx, command, active_preset, active_actions),
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => {
            panic!("transmission worker command channel closed")
        }
    }
}

fn apply_command(
    ctx: &AppCtx,
    command: TransmissionCommand,
    active_preset: &mut Option<ActivePreset>,
    active_actions: &mut HashMap<ActionKey, ActiveActionHandle>,
) {
    match command {
        TransmissionCommand::UpsertAction { key, handle } => {
            if let Some(previous) = active_actions.insert(key.clone(), handle) {
                record_action_completion(ctx, &key, &previous);
            }
        }
        TransmissionCommand::CancelAction { key } => {
            if let Some(handle) = active_actions.remove(&key) {
                record_action_completion(ctx, &key, &handle);
            }
        }
        TransmissionCommand::CancelOwnedActions { owner } => {
            cancel_manual_actions(ctx, active_actions, |_, handle| {
                handle.cancel_on_disconnect && handle.owner == Some(owner)
            });
        }
        TransmissionCommand::CancelAllActions => {
            cancel_manual_actions(ctx, active_actions, |_, _| true);
        }
        TransmissionCommand::StartPreset {
            preset_name,
            events,
        } => {
            info!("Worker: starting preset '{preset_name}'");
            *active_preset = Some(ActivePreset {
                events,
                preset_name,
                started_at: Instant::now(),
                event_index: 0,
            });
        }
        TransmissionCommand::StopPreset => {
            *active_preset = None;
        }
        TransmissionCommand::StopAll => {
            *active_preset = None;
            cancel_manual_actions(ctx, active_actions, |_, _| true);
        }
    }
}

fn cancel_manual_actions(
    ctx: &AppCtx,
    active_actions: &mut HashMap<ActionKey, ActiveActionHandle>,
    predicate: impl Fn(&ActionKey, &ActiveActionHandle) -> bool,
) {
    let keys: Vec<ActionKey> = active_actions
        .iter()
        .filter(|(key, handle)| predicate(key, handle))
        .map(|(key, _)| key.clone())
        .collect();

    for key in keys {
        if let Some(handle) = active_actions.remove(&key) {
            record_action_completion(ctx, &key, &handle);
        }
    }
}

fn record_action_completion(ctx: &AppCtx, key: &ActionKey, handle: &ActiveActionHandle) {
    let elapsed_ms = handle
        .started_at
        .elapsed()
        .as_millis()
        .min(u32::MAX as u128) as u32;
    ctx.record_event(
        handle.source,
        EventLogEntryKind::Action {
            collar_name: key.collar_name.clone(),
            mode: key.mode,
            intensity: key.mode.has_intensity().then_some(handle.intensity),
            duration_ms: elapsed_ms,
        },
    );
}

fn next_manual_action(
    active_actions: &HashMap<ActionKey, ActiveActionHandle>,
    round_robin_idx: &mut usize,
) -> Option<ActionSnapshot> {
    if active_actions.is_empty() {
        return None;
    }

    let mut keys: Vec<&ActionKey> = active_actions.keys().collect();
    keys.sort_by(|a, b| {
        a.collar_name
            .cmp(&b.collar_name)
            .then(a.mode.to_rf_byte().cmp(&b.mode.to_rf_byte()))
    });

    *round_robin_idx %= keys.len();
    let key = keys[*round_robin_idx];
    let handle = &active_actions[key];
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

fn clear_active_preset_name(ctx: &AppCtx, completed_name: &str) {
    let mut domain = ctx.domain.lock().unwrap();
    if domain.preset_name.as_deref() == Some(completed_name) {
        domain.preset_name = None;
    }
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
