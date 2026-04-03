use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{error, info, warn};
use serde::Serialize;

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};
use crate::error::ControlError;
use crate::protocol::{
    Collar, DeviceSettings, EventLogEntry, EventLogEntryKind, EventSource, ExportData, Preset,
    RemoteControlStatus, RfDebugFrame, ServerMessage,
};
use crate::scheduling::PresetEvent;
use crate::validation;
use rusty_collars_core::rf_timing::RF_COMMAND_TRANSMIT_DURATION_US;

use super::{
    current_unix_ms, device_settings_reboot_required, free_heap, now_millis, stop_active_preset,
    stop_all_transmissions, ActionKey, ActiveActionHandle, AppCommand, AppCtx, AppEvent,
    ControlResult, TransmissionCommand, HAS_WIFI, MAX_EVENT_LOG_ENTRIES, MAX_RF_DEBUG_EVENTS,
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

fn run_app_worker(ctx: AppCtx, command_rx: Receiver<AppCommand>) {
    info!("App worker started");

    loop {
        let command = command_rx
            .recv()
            .expect("app worker command channel closed");
        handle_app_command(&ctx, command);
    }
}

fn handle_app_command(ctx: &AppCtx, command: AppCommand) {
    match command {
        AppCommand::AddCollar { collar, reply } => {
            send_reply(reply, handle_add_collar(ctx, collar))
        }
        AppCommand::UpdateCollar {
            original_name,
            updated,
            reply,
        } => send_reply(reply, handle_update_collar(ctx, original_name, updated)),
        AppCommand::DeleteCollar { name, reply } => {
            send_reply(reply, handle_delete_collar(ctx, name))
        }
        AppCommand::SavePreset {
            original_name,
            preset,
            reply,
        } => send_reply(reply, handle_save_preset(ctx, original_name, preset)),
        AppCommand::DeletePreset { name, reply } => {
            send_reply(reply, handle_delete_preset(ctx, name))
        }
        AppCommand::ReorderPresets { names, reply } => {
            send_reply(reply, handle_reorder_presets(ctx, names))
        }
        AppCommand::ImportData { data, reply } => send_reply(reply, handle_import_data(ctx, data)),
        AppCommand::SaveDeviceSettings { settings, reply } => {
            send_reply(reply, handle_save_device_settings(ctx, settings))
        }
        AppCommand::StartPresetExecution {
            preset_name,
            source,
            resolved_preset,
            events,
            reply,
        } => send_reply(
            reply,
            handle_start_preset_execution(ctx, preset_name, source, resolved_preset, events),
        ),
        AppCommand::StopPreset { reply } => send_reply(reply, handle_stop_preset(ctx)),
        AppCommand::StopAll { reply } => send_reply(reply, handle_stop_all(ctx)),
        AppCommand::SetRemoteControlStatus { status } => {
            handle_set_remote_control_status(ctx, status)
        }
        AppCommand::RecordEvent { source, kind } => handle_record_event(ctx, source, kind),
        AppCommand::PushRfDebugEvent { event } => handle_push_rf_debug_event(ctx, event),
        AppCommand::ClearRfDebugEvents { listening, reply } => {
            send_reply(reply, handle_clear_rf_debug_events(ctx, listening))
        }
        AppCommand::CompletePreset { preset_name } => handle_complete_preset(ctx, preset_name),
    }
}

fn send_reply<T>(reply: SyncSender<T>, value: T) {
    reply.send(value).expect("app worker reply channel closed");
}

fn handle_add_collar(ctx: &AppCtx, collar: Collar) -> ControlResult {
    let collars = ctx.with_domain_mut(|domain| {
        validation::validate_collar(&collar)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        if domain
            .collars
            .iter()
            .any(|existing| existing.name == collar.name)
        {
            return Err(ControlError::DuplicateCollar(collar.name.clone()));
        }
        domain.collars.push(collar);
        Ok(domain.collars.clone())
    })?;

    ctx.persist_collars(&collars);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_update_collar(ctx: &AppCtx, original_name: String, updated: Collar) -> ControlResult {
    let (collars, presets, preset_stopped) = ctx.with_domain_mut(|domain| {
        let Some(index) = domain
            .collars
            .iter()
            .position(|collar| collar.name == original_name)
        else {
            return Err(ControlError::UnknownCollar(original_name));
        };
        validation::validate_collar(&updated)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        if domain
            .collars
            .iter()
            .enumerate()
            .any(|(existing_index, collar)| existing_index != index && collar.name == updated.name)
        {
            return Err(ControlError::DuplicateCollar(updated.name.clone()));
        }

        domain.collars[index] = updated.clone();
        if original_name != updated.name {
            for preset in &mut domain.presets {
                for track in &mut preset.tracks {
                    if track.collar_name == original_name {
                        track.collar_name = updated.name.clone();
                    }
                }
            }
        }

        let preset_stopped = stop_active_preset(domain);
        Ok((
            domain.collars.clone(),
            domain.presets.clone(),
            preset_stopped,
        ))
    })?;

    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.cancel_all_manual_actions();
    ctx.persist_collars(&collars);
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_delete_collar(ctx: &AppCtx, name: String) -> ControlResult {
    let (collars, preset_stopped) = ctx.with_domain_mut(|domain| {
        if domain
            .presets
            .iter()
            .any(|preset| preset.tracks.iter().any(|track| track.collar_name == name))
        {
            return Err(ControlError::CollarReferencedByPreset(name));
        }
        let before = domain.collars.len();
        domain.collars.retain(|collar| collar.name != name);
        if domain.collars.len() == before {
            return Err(ControlError::UnknownCollar(name));
        }
        let preset_stopped = stop_active_preset(domain);
        Ok((domain.collars.clone(), preset_stopped))
    })?;

    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.cancel_all_manual_actions();
    ctx.persist_collars(&collars);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_save_preset(
    ctx: &AppCtx,
    original_name: Option<String>,
    preset: Preset,
) -> ControlResult {
    let (presets, preset_stopped) = ctx.with_domain_mut(|domain| {
        validation::validate_preset(&preset, &domain.collars)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        let original_name = original_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let mut updated = domain.presets.clone();
        if let Some(original_name) = original_name {
            let Some(index) = updated
                .iter()
                .position(|existing| existing.name == original_name)
            else {
                return Err(ControlError::UnknownPreset(original_name.to_string()));
            };
            if updated
                .iter()
                .enumerate()
                .any(|(existing_index, existing)| {
                    existing_index != index && existing.name == preset.name
                })
            {
                return Err(ControlError::DuplicatePreset(preset.name.clone()));
            }
            updated[index] = preset;
        } else if let Some(existing) = updated
            .iter_mut()
            .find(|existing| existing.name == preset.name)
        {
            *existing = preset;
        } else {
            updated.push(preset);
        }
        validation::validate_presets(&updated, &domain.collars)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        let preset_stopped = stop_active_preset(domain);
        domain.presets = updated;
        Ok((domain.presets.clone(), preset_stopped))
    })?;

    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_delete_preset(ctx: &AppCtx, name: String) -> ControlResult {
    let (presets, preset_stopped) = ctx.with_domain_mut(|domain| {
        let before = domain.presets.len();
        domain.presets.retain(|preset| preset.name != name);
        if domain.presets.len() == before {
            return Err(ControlError::UnknownPreset(name));
        }
        let preset_stopped = stop_active_preset(domain);
        Ok((domain.presets.clone(), preset_stopped))
    })?;

    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_reorder_presets(ctx: &AppCtx, names: Vec<String>) -> ControlResult {
    let presets = ctx.with_domain_mut(|domain| {
        let order_by_name: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(idx, name)| (name.as_str(), idx))
            .collect();
        let mut reordered_slots = vec![None; names.len()];
        let mut remaining = Vec::with_capacity(domain.presets.len());
        for preset in domain.presets.drain(..) {
            match order_by_name.get(preset.name.as_str()) {
                Some(&idx) if reordered_slots[idx].is_none() => {
                    reordered_slots[idx] = Some(preset);
                }
                _ => remaining.push(preset),
            }
        }
        let mut reordered = Vec::with_capacity(remaining.len() + names.len());
        reordered.extend(reordered_slots.into_iter().flatten());
        reordered.extend(remaining);
        domain.presets = reordered;
        domain.presets.clone()
    });

    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_import_data(ctx: &AppCtx, data: ExportData) -> ControlResult {
    let (collars, presets, preset_stopped) = ctx.with_domain_mut(|domain| {
        let preset_stopped = stop_active_preset(domain);
        domain.collars = data.collars;
        domain.presets = data.presets;
        (
            domain.collars.clone(),
            domain.presets.clone(),
            preset_stopped,
        )
    });

    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.cancel_all_manual_actions();
    ctx.persist_collars(&collars);
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_save_device_settings(ctx: &AppCtx, settings: DeviceSettings) -> ControlResult {
    let settings_to_save = settings.clone();
    let (reboot_required, remote_settings_changed, event_log_changed) =
        ctx.with_domain_mut(|domain| {
            let previous_settings = domain.device_settings.clone();
            let reboot_required = device_settings_reboot_required(&previous_settings, &settings);
            let remote_settings_changed = previous_settings.remote_control_enabled
                != settings.remote_control_enabled
                || previous_settings.remote_control_url != settings.remote_control_url
                || previous_settings.remote_control_validate_cert
                    != settings.remote_control_validate_cert;
            let event_log_changed = previous_settings.record_event_log != settings.record_event_log;

            domain.device_settings = settings;
            if remote_settings_changed {
                domain.remote_control_status =
                    super::status::remote_control_status_from_settings(&domain.device_settings);
            }
            if !domain.device_settings.record_event_log {
                domain.event_log_events.clear();
            }

            (reboot_required, remote_settings_changed, event_log_changed)
        });

    if remote_settings_changed {
        ctx.bump_remote_control_settings_revision();
    }

    match ctx.persist_settings(&settings_to_save) {
        Ok(()) => info!("Device settings saved to NVS"),
        Err(err) => error!("NVS save_settings failed: {err:#}"),
    }

    if remote_settings_changed {
        ctx.broadcast_event(ctx.remote_control_status_event());
    }
    if event_log_changed {
        ctx.broadcast_event(ctx.event_log_state_event());
    }

    json_message(&ServerMessage::DeviceSettings {
        settings: settings_to_save,
        reboot_required,
        has_wifi: HAS_WIFI,
    })
}

fn handle_start_preset_execution(
    ctx: &AppCtx,
    preset_name: String,
    source: EventSource,
    resolved_preset: Option<Preset>,
    events: Vec<PresetEvent>,
) -> ControlResult {
    ctx.with_domain_mut(|domain| {
        domain.preset_name = Some(preset_name.clone());
    });
    ctx.start_preset_execution(preset_name.clone(), events);

    if let Some(entry) = append_event_log(
        ctx,
        source,
        EventLogEntryKind::PresetRun {
            preset_name,
            resolved_preset,
        },
    ) {
        broadcast_event_log_entry(ctx, &entry);
    }

    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_stop_preset(ctx: &AppCtx) -> ControlResult {
    let stopped = ctx.with_domain_mut(stop_active_preset);

    if stopped {
        ctx.stop_preset_execution();
    }
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_stop_all(ctx: &AppCtx) -> ControlResult {
    ctx.with_domain_mut(stop_all_transmissions);
    ctx.stop_all_execution();
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_set_remote_control_status(ctx: &AppCtx, status: RemoteControlStatus) {
    let changed = ctx.with_domain_mut(|domain| {
        if domain.remote_control_status == status {
            false
        } else {
            domain.remote_control_status = status;
            true
        }
    });

    if changed {
        ctx.broadcast_event(ctx.remote_control_status_event());
    }
}

fn handle_record_event(ctx: &AppCtx, source: EventSource, kind: EventLogEntryKind) {
    if let Some(entry) = append_event_log(ctx, source, kind) {
        broadcast_event_log_entry(ctx, &entry);
    }
}

fn handle_push_rf_debug_event(ctx: &AppCtx, event: RfDebugFrame) {
    ctx.with_domain_mut(|domain| {
        domain.rf_debug_events.push_back(event.clone());
        if domain.rf_debug_events.len() > MAX_RF_DEBUG_EVENTS {
            domain.rf_debug_events.pop_front();
        }
    });

    ctx.broadcast_event(AppEvent::RfDebugEvent { event });
}

fn handle_clear_rf_debug_events(ctx: &AppCtx, listening: bool) -> AppEvent {
    ctx.with_domain_mut(|domain| domain.rf_debug_events.clear());
    ctx.rf_debug_state_event(listening)
}

fn handle_complete_preset(ctx: &AppCtx, preset_name: String) {
    let changed = ctx.with_domain_mut(|domain| {
        if domain.preset_name.as_deref() == Some(&preset_name) {
            domain.preset_name = None;
            true
        } else {
            false
        }
    });

    if changed {
        ctx.broadcast_state();
    }
}

fn append_event_log(
    ctx: &AppCtx,
    source: EventSource,
    kind: EventLogEntryKind,
) -> Option<EventLogEntry> {
    ctx.with_domain_mut(|domain| {
        if !domain.device_settings.record_event_log {
            return None;
        }

        let entry = EventLogEntry {
            sequence: ctx.next_event_log_sequence(),
            monotonic_ms: now_millis(),
            unix_ms: current_unix_ms(),
            source,
            kind,
        };

        domain.event_log_events.push(entry.clone());
        if domain.event_log_events.len() > MAX_EVENT_LOG_ENTRIES {
            let excess = domain.event_log_events.len() - MAX_EVENT_LOG_ENTRIES;
            domain.event_log_events.drain(0..excess);
        }

        Some(entry)
    })
}

fn broadcast_event_log_entry(ctx: &AppCtx, entry: &EventLogEntry) {
    ctx.broadcast_event(AppEvent::EventLogEvent {
        event: entry.clone(),
    });
}

fn json_message(message: &impl Serialize) -> ControlResult {
    Ok(vec![serde_json::to_string(message)?])
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
                    std::thread::sleep(Duration::from_millis(super::RF_DEBUG_DISABLED_SLEEP_MS));
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
