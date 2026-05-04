use std::sync::mpsc::{Receiver, SyncSender};

use log::info;
use rusty_collars_app::{
    CollarService, DataService, EventLogService, ExecutionService, MqttService, PresetService,
    RemoteControlService, RfDebugService, SettingsService,
};
use serde::Serialize;

use crate::protocol::{
    Collar, DeviceSettings, EventLogEntry, EventLogEntryKind, EventSource, MqttStatus, Preset,
    RemoteControlStatus, RfDebugFrame, ServerMessage,
};
use crate::scheduling::PresetEvent;

use super::super::{
    current_unix_ms, now_millis, AppCommand, AppCtx, AppEvent, ControlResult, HAS_WIFI,
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
        AppCommand::SetMqttStatus { status } => handle_set_mqtt_status(ctx, status),
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
    let change = ctx.with_domain_mut(|domain| CollarService::add(domain, collar))?;

    ctx.repository_services().save_collars(&change.collars)?;
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_update_collar(ctx: &AppCtx, original_name: String, updated: Collar) -> ControlResult {
    let change =
        ctx.with_domain_mut(|domain| CollarService::update(domain, original_name, updated))?;

    if change.preset_stopped {
        ctx.stop_preset_execution();
    }
    if change.cancel_manual_actions {
        ctx.cancel_all_manual_actions();
    }
    ctx.repository_services().save_collars(&change.collars)?;
    if let Some(presets) = change.presets.as_ref() {
        ctx.repository_services().save_presets(presets)?;
    }
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_delete_collar(ctx: &AppCtx, name: String) -> ControlResult {
    let change = ctx.with_domain_mut(|domain| CollarService::delete(domain, name))?;

    if change.preset_stopped {
        ctx.stop_preset_execution();
    }
    if change.cancel_manual_actions {
        ctx.cancel_all_manual_actions();
    }
    ctx.repository_services().save_collars(&change.collars)?;
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_save_preset(
    ctx: &AppCtx,
    original_name: Option<String>,
    preset: Preset,
) -> ControlResult {
    let change =
        ctx.with_domain_mut(|domain| PresetService::save(domain, original_name, preset))?;

    if change.preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.repository_services().save_presets(&change.presets)?;
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_delete_preset(ctx: &AppCtx, name: String) -> ControlResult {
    let change = ctx.with_domain_mut(|domain| PresetService::delete(domain, name))?;

    if change.preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.repository_services().save_presets(&change.presets)?;
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_reorder_presets(ctx: &AppCtx, names: Vec<String>) -> ControlResult {
    let change = ctx.with_domain_mut(|domain| PresetService::reorder(domain, names));

    ctx.repository_services().save_presets(&change.presets)?;
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_import_data(ctx: &AppCtx, data: crate::protocol::ExportData) -> ControlResult {
    let change = ctx.with_domain_mut(|domain| DataService::import(domain, data));

    if change.preset_stopped {
        ctx.stop_preset_execution();
    }
    if change.cancel_manual_actions {
        ctx.cancel_all_manual_actions();
    }
    ctx.repository_services().save_collars(&change.collars)?;
    ctx.repository_services().save_presets(&change.presets)?;
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_save_device_settings(ctx: &AppCtx, settings: DeviceSettings) -> ControlResult {
    let change = ctx.with_domain_mut(|domain| SettingsService::apply(domain, settings));

    if change.remote_settings_changed {
        ctx.bump_remote_control_settings_revision();
    }
    if change.mqtt_settings_changed {
        ctx.bump_mqtt_settings_revision();
    }
    ctx.repository_services().save_settings(&change.settings)?;

    if change.remote_settings_changed {
        ctx.broadcast_event(ctx.remote_control_status_event());
    }
    if change.mqtt_settings_changed {
        ctx.broadcast_event(ctx.mqtt_status_event());
    }
    if change.event_log_changed {
        ctx.broadcast_event(ctx.event_log_state_event());
    }

    json_message(&ServerMessage::DeviceSettings {
        settings: change.settings,
        reboot_required: change.reboot_required,
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
    ctx.with_domain_mut(|domain| ExecutionService::start_preset(domain, preset_name.clone()));
    ctx.start_preset_execution(preset_name.clone(), events);

    if let Some(entry) = ctx.with_domain_mut(|domain| {
        EventLogService::append(
            domain,
            || ctx.next_event_log_sequence(),
            now_millis(),
            current_unix_ms(),
            source,
            EventLogEntryKind::PresetRun {
                preset_name,
                resolved_preset,
            },
        )
    }) {
        broadcast_event_log_entry(ctx, &entry);
    }

    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_stop_preset(ctx: &AppCtx) -> ControlResult {
    let stopped = ctx.with_domain_mut(ExecutionService::stop_preset);

    if stopped {
        ctx.stop_preset_execution();
    }
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_stop_all(ctx: &AppCtx) -> ControlResult {
    let now_ms = now_millis();
    ctx.with_domain_mut(|domain| ExecutionService::stop_all(domain, now_ms));
    ctx.stop_all_execution();
    ctx.broadcast_state();
    Ok(Vec::new())
}

fn handle_set_remote_control_status(ctx: &AppCtx, status: RemoteControlStatus) {
    let changed = ctx.with_domain_mut(|domain| RemoteControlService::set_status(domain, status));

    if changed {
        ctx.broadcast_event(ctx.remote_control_status_event());
    }
}

fn handle_set_mqtt_status(ctx: &AppCtx, status: MqttStatus) {
    let changed = ctx.with_domain_mut(|domain| MqttService::set_status(domain, status));

    if changed {
        ctx.broadcast_event(ctx.mqtt_status_event());
    }
}

fn handle_record_event(ctx: &AppCtx, source: EventSource, kind: EventLogEntryKind) {
    if let Some(entry) = ctx.with_domain_mut(|domain| {
        EventLogService::append(
            domain,
            || ctx.next_event_log_sequence(),
            now_millis(),
            current_unix_ms(),
            source,
            kind,
        )
    }) {
        broadcast_event_log_entry(ctx, &entry);
    }
}

fn handle_push_rf_debug_event(ctx: &AppCtx, event: RfDebugFrame) {
    ctx.with_domain_mut(|domain| RfDebugService::push_event(domain, event.clone()));
    ctx.broadcast_event(AppEvent::RfDebugEvent { event });
}

fn handle_clear_rf_debug_events(ctx: &AppCtx, listening: bool) -> AppEvent {
    ctx.with_domain_mut(RfDebugService::clear_events);
    ctx.rf_debug_state_event(listening)
}

fn handle_complete_preset(ctx: &AppCtx, preset_name: String) {
    let changed =
        ctx.with_domain_mut(|domain| ExecutionService::complete_preset(domain, &preset_name));

    if changed {
        ctx.broadcast_state();
    }
}

fn broadcast_event_log_entry(ctx: &AppCtx, entry: &EventLogEntry) {
    ctx.broadcast_event(AppEvent::EventLogEvent {
        event: entry.clone(),
    });
}

fn json_message(message: &impl Serialize) -> ControlResult {
    Ok(vec![serde_json::to_string(message)?])
}
