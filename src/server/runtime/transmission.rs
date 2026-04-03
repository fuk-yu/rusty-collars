use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use log::{error, info};

use crate::protocol::EventLogEntryKind;

use super::super::{ActionKey, ActiveActionHandle, AppCtx, TransmissionCommand};

const WORKER_IDLE_TIMEOUT: Duration = Duration::from_millis(10);
const TX_DURATION: Duration =
    Duration::from_micros(rusty_collars_core::rf_timing::RF_COMMAND_TRANSMIT_DURATION_US);

struct ActivePreset {
    events: Vec<crate::scheduling::PresetEvent>,
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

pub(super) fn run_transmission_worker(ctx: &AppCtx, command_rx: Receiver<TransmissionCommand>) {
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
                    if let Err(err) = ctx.transmit_rf_command_now(
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
    if let Err(err) = ctx.transmit_rf_command_now(
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
    ctx.complete_preset(completed_name.to_string());
}
