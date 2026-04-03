use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender};

use log::{error, info};
use serde::Serialize;

use crate::error::ControlError;
use crate::protocol::{
    Collar, DeviceSettings, EventLogEntry, EventLogEntryKind, EventSource, ExportData, Preset,
    RemoteControlStatus, RfDebugFrame, ServerMessage,
};
use crate::scheduling::PresetEvent;
use crate::validation;

use super::super::{
    current_unix_ms, device_settings_reboot_required, now_millis, stop_active_preset,
    stop_all_transmissions, AppCommand, AppCtx, AppEvent, ControlResult, HAS_WIFI,
    MAX_EVENT_LOG_ENTRIES, MAX_RF_DEBUG_EVENTS,
};

pub(super) fn run_app_worker(ctx: AppCtx, command_rx: Receiver<AppCommand>) {
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
                    super::super::status::remote_control_status_from_settings(
                        &domain.device_settings,
                    );
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
