use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{error, info};

use crate::protocol::{
    ClientMessage, Collar, CommandMode, Distribution, EventLogEntryKind, ExportData, Preset,
    ServerMessage, MAX_INTENSITY,
};
use crate::{scheduling, validation};

use super::{
    command_intensity, device_settings_reboot_required, event_source, log_storage_result,
    resolve_random_duration, resolve_random_u8, rf_lockout_remaining_ms, rf_send_with_led,
    rollback_failed_preset_start, save_collars, save_presets, save_settings, stop_active_preset,
    stop_all_transmissions, ActionKey, ActionOwner, ActiveActionHandle, AppCtx, ControlResult,
    ManualActionSpec, MessageOrigin, RandomResolver, HAS_WIFI,
};

const MANUAL_ACTION_REPEAT_MS: u64 = 200;
const MANUAL_ACTION_SLEEP_SLICE_MS: u64 = 50;

pub(crate) fn pong_json(ctx: &AppCtx, nonce: u32) -> String {
    let client_ips: Vec<String> = ctx
        .ws_clients
        .lock()
        .unwrap()
        .iter()
        .map(|(_, addr)| addr.clone())
        .collect();

    serde_json::to_string(&ServerMessage::Pong {
        nonce,
        server_uptime_s: super::uptime_seconds(),
        free_heap_bytes: super::free_heap(),
        connected_clients: client_ips.len() as u32,
        client_ips,
    })
    .unwrap()
}

pub(crate) fn cancel_owned_manual_actions(ctx: &AppCtx, owner: ActionOwner) {
    stop_handles({
        let mut active_actions = ctx.active_actions.lock().unwrap();
        let keys_to_remove: Vec<ActionKey> = active_actions
            .iter()
            .filter_map(|(key, handle)| {
                (handle.cancel_on_disconnect && handle.owner == Some(owner)).then(|| key.clone())
            })
            .collect();

        keys_to_remove
            .into_iter()
            .filter_map(|key| active_actions.remove(&key))
            .collect()
    });
}

pub(crate) fn process_control_message(
    ctx: &AppCtx,
    msg: ClientMessage,
    origin: MessageOrigin,
    owner: Option<ActionOwner>,
) -> ControlResult {
    match msg {
        ClientMessage::Command {
            collar_name,
            mode,
            intensity,
        } => {
            let (collar, intensity) = resolve_collar_command(ctx, &collar_name, mode, intensity)?;
            if let Err(err) = rf_send_with_led(
                &ctx.rf,
                &ctx.tx_led,
                collar.collar_id,
                collar.channel,
                mode.to_rf_byte(),
                intensity,
            ) {
                error!("RF send error: {err:#}");
            }
            Ok(Vec::new())
        }

        ClientMessage::ButtonEvent {
            collar_name,
            mode,
            intensity,
            action,
        } => {
            if cfg!(debug_assertions) {
                info!(
                    "Button {:?}: collar={collar_name} mode={mode:?} intensity={intensity}",
                    action
                );
            }
            Ok(Vec::new())
        }

        ClientMessage::RunAction {
            collar_name,
            mode,
            intensity,
            duration_ms,
            intensity_max,
            duration_max_ms,
            intensity_distribution,
            duration_distribution,
        } => {
            start_manual_action(
                ctx,
                collar_name,
                mode,
                intensity,
                intensity_max,
                Some(duration_ms),
                duration_max_ms,
                intensity_distribution.unwrap_or_default(),
                duration_distribution.unwrap_or_default(),
                event_source(origin),
                owner,
                false,
            )?;
            Ok(Vec::new())
        }

        ClientMessage::StartAction {
            collar_name,
            mode,
            intensity,
            intensity_max,
            intensity_distribution,
        } => {
            start_manual_action(
                ctx,
                collar_name,
                mode,
                intensity,
                intensity_max,
                None,
                None,
                intensity_distribution.unwrap_or_default(),
                Distribution::default(),
                event_source(origin),
                owner,
                true,
            )?;
            Ok(Vec::new())
        }

        ClientMessage::StopAction { collar_name, mode } => {
            stop_manual_action(ctx, &collar_name, mode);
            Ok(Vec::new())
        }

        ClientMessage::AddCollar {
            name,
            collar_id,
            channel,
        } => {
            let collar = Collar {
                name,
                collar_id,
                channel,
            };
            let collars = {
                let mut domain = ctx.domain.lock().unwrap();
                validation::validate_collar(&collar).map_err(|err| err.to_string())?;
                if domain
                    .collars
                    .iter()
                    .any(|existing| existing.name == collar.name)
                {
                    return Err(format!("Collar '{}' already exists", collar.name));
                }
                domain.collars.push(collar);
                domain.collars.clone()
            };
            log_storage_result("save_collars", save_collars(ctx, &collars));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::UpdateCollar {
            original_name,
            name,
            collar_id,
            channel,
        } => {
            let updated = Collar {
                name,
                collar_id,
                channel,
            };
            let (collars, presets) = {
                let mut domain = ctx.domain.lock().unwrap();
                let Some(index) = domain
                    .collars
                    .iter()
                    .position(|collar| collar.name == original_name)
                else {
                    return Err(format!("Unknown collar: {original_name}"));
                };
                validation::validate_collar(&updated).map_err(|err| err.to_string())?;
                if domain
                    .collars
                    .iter()
                    .enumerate()
                    .any(|(existing_index, collar)| {
                        existing_index != index && collar.name == updated.name
                    })
                {
                    return Err(format!("Collar '{}' already exists", updated.name));
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
                stop_active_preset(&mut domain, &ctx.preset_run_id);
                (domain.collars.clone(), domain.presets.clone())
            };
            cancel_all_manual_actions(ctx);
            log_storage_result("save_collars", save_collars(ctx, &collars));
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::DeleteCollar { name } => {
            let collars = {
                let mut domain = ctx.domain.lock().unwrap();
                if domain
                    .presets
                    .iter()
                    .any(|preset| preset.tracks.iter().any(|track| track.collar_name == name))
                {
                    return Err(format!("Cannot delete '{name}': presets reference it"));
                }
                let before = domain.collars.len();
                domain.collars.retain(|collar| collar.name != name);
                if domain.collars.len() == before {
                    return Err(format!("Unknown collar: {name}"));
                }
                stop_active_preset(&mut domain, &ctx.preset_run_id);
                domain.collars.clone()
            };
            cancel_all_manual_actions(ctx);
            log_storage_result("save_collars", save_collars(ctx, &collars));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::SavePreset {
            original_name,
            mut preset,
        } => {
            preset.normalize();
            let presets = {
                let mut domain = ctx.domain.lock().unwrap();
                validation::validate_preset(&preset, &domain.collars)
                    .map_err(|err| err.to_string())?;
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
                        return Err(format!("Unknown preset: {original_name}"));
                    };
                    if updated
                        .iter()
                        .enumerate()
                        .any(|(existing_index, existing)| {
                            existing_index != index && existing.name == preset.name
                        })
                    {
                        return Err(format!("Preset '{}' already exists", preset.name));
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
                    .map_err(|err| err.to_string())?;
                stop_active_preset(&mut domain, &ctx.preset_run_id);
                domain.presets = updated;
                domain.presets.clone()
            };
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::Ping { nonce } => Ok(vec![pong_json(ctx, nonce)]),

        ClientMessage::DeletePreset { name } => {
            let presets = {
                let mut domain = ctx.domain.lock().unwrap();
                let before = domain.presets.len();
                domain.presets.retain(|preset| preset.name != name);
                if domain.presets.len() == before {
                    return Err(format!("Unknown preset: {name}"));
                }
                stop_active_preset(&mut domain, &ctx.preset_run_id);
                domain.presets.clone()
            };
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::RunPreset { name } => {
            let source = event_source(origin);
            let (preset_name, resolved_preset_for_log, events, run_id) = {
                let mut domain = ctx.domain.lock().unwrap();
                if rf_lockout_remaining_ms(&domain) > 0 {
                    return Err("Transmissions locked after STOP".to_string());
                }

                let Some(preset) = domain
                    .presets
                    .iter()
                    .find(|preset| preset.name == name)
                    .cloned()
                else {
                    return Err(format!("Unknown preset: {name}"));
                };
                validation::validate_preset(&preset, &domain.collars)
                    .map_err(|err| err.to_string())?;

                let has_random = preset
                    .tracks
                    .iter()
                    .any(|track| track.steps.iter().any(|step| step.has_random()));
                let mut rng = ctx.rng.lock().unwrap();
                let mut resolver = RandomResolver { rng: &mut *rng };
                let resolved = scheduling::resolve_preset(&preset, &mut resolver);
                let events = scheduling::schedule_preset_events(
                    &resolved,
                    &domain.collars,
                    &mut scheduling::MidpointResolver,
                )
                .map_err(|err| err.to_string())?;
                let resolved_for_log = has_random.then_some(resolved);
                let run_id = ctx.preset_run_id.fetch_add(1, Ordering::SeqCst) + 1;
                domain.preset_name = Some(name.clone());
                (preset.name.clone(), resolved_for_log, events, run_id)
            };

            let ctx2 = ctx.clone();
            let preset_name_for_thread = preset_name.clone();
            std::thread::Builder::new()
                .name("preset".into())
                .stack_size(32768)
                .spawn(move || {
                    super::runtime::run_preset(&preset_name_for_thread, events, &ctx2, run_id);
                    if ctx2.preset_run_id.load(Ordering::SeqCst) == run_id {
                        let mut domain = ctx2.domain.lock().unwrap();
                        if domain.preset_name.as_deref() == Some(preset_name_for_thread.as_str()) {
                            domain.preset_name = None;
                        }
                    }
                    ctx2.broadcast_state();
                })
                .map_err(|err| {
                    rollback_failed_preset_start(ctx, &preset_name, run_id);
                    format!("Failed to spawn preset thread: {err}")
                })?;

            ctx.record_event(
                source,
                EventLogEntryKind::PresetRun {
                    preset_name: preset_name.clone(),
                    resolved_preset: resolved_preset_for_log,
                },
            );
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::StopPreset => {
            {
                let mut domain = ctx.domain.lock().unwrap();
                ctx.preset_run_id.fetch_add(1, Ordering::SeqCst);
                domain.preset_name = None;
            }
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::StopAll => {
            {
                let mut domain = ctx.domain.lock().unwrap();
                stop_all_transmissions(&mut domain, &ctx.preset_run_id);
            }
            cancel_all_manual_actions(ctx);
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::StartRfDebug => {
            Err("RF debug control is only available on the local UI".to_string())
        }
        ClientMessage::StopRfDebug => {
            Err("RF debug control is only available on the local UI".to_string())
        }
        ClientMessage::ClearRfDebug => {
            Err("RF debug control is only available on the local UI".to_string())
        }
        ClientMessage::Reboot => Err("Device reboot is only available on the local UI".to_string()),

        ClientMessage::GetDeviceSettings => {
            ensure_local_ui(origin, "get_device_settings")?;
            let settings = ctx.domain.lock().unwrap().device_settings.clone();
            json_message(&ServerMessage::DeviceSettings {
                settings,
                reboot_required: false,
                has_wifi: HAS_WIFI,
            })
        }

        ClientMessage::GetNetworkStatus => {
            ensure_local_ui(origin, "get_network_status")?;
            let settings = ctx.domain.lock().unwrap().device_settings.clone();
            json_message(&super::status::gather_network_status(&settings))
        }

        ClientMessage::SaveDeviceSettings { mut settings } => {
            ensure_local_ui(origin, "save_device_settings")?;

            if settings.device_id.is_empty() {
                settings.device_id = ctx.domain.lock().unwrap().device_settings.device_id.clone();
            }
            settings.ntp_server = settings.ntp_server.trim().to_string();
            settings.remote_control_url = settings.remote_control_url.trim().to_string();

            if settings.ntp_enabled && settings.ntp_server.is_empty() {
                return Err("NTP server cannot be empty when time sync is enabled".to_string());
            }
            if settings.remote_control_enabled {
                super::parse_remote_control_url(&settings.remote_control_url)?;
            }

            info!("Saving device settings...");
            let settings_to_save = settings.clone();
            let (reboot_required, remote_settings_changed, event_log_changed) = {
                let mut domain = ctx.domain.lock().unwrap();
                let previous_settings = domain.device_settings.clone();
                let reboot_required =
                    device_settings_reboot_required(&previous_settings, &settings);
                let remote_settings_changed = previous_settings.remote_control_enabled
                    != settings.remote_control_enabled
                    || previous_settings.remote_control_url != settings.remote_control_url
                    || previous_settings.remote_control_validate_cert
                        != settings.remote_control_validate_cert;
                let event_log_changed =
                    previous_settings.record_event_log != settings.record_event_log;

                domain.device_settings = settings;
                if remote_settings_changed {
                    domain.remote_control_status =
                        super::status::remote_control_status_from_settings(&domain.device_settings);
                }
                if !domain.device_settings.record_event_log {
                    domain.event_log_events.clear();
                }

                (reboot_required, remote_settings_changed, event_log_changed)
            };

            if remote_settings_changed {
                ctx.remote_control_settings_revision
                    .fetch_add(1, Ordering::SeqCst);
            }

            match save_settings(ctx, &settings_to_save) {
                Ok(()) => info!("Device settings saved to NVS"),
                Err(err) => error!("NVS save_settings failed: {err:#}"),
            }

            if remote_settings_changed {
                ctx.broadcast_remote_control_status();
            }
            if event_log_changed {
                ctx.broadcast_event_log_state();
            }

            json_message(&ServerMessage::DeviceSettings {
                settings: settings_to_save,
                reboot_required,
                has_wifi: HAS_WIFI,
            })
        }

        ClientMessage::PreviewPreset { nonce, mut preset } => {
            preset.normalize();
            let collars = ctx.domain.lock().unwrap().collars.clone();
            let message = match validation::validate_preset(&preset, &collars) {
                Ok(()) => match scheduling::preview_preset(&preset, &collars) {
                    Ok(preview) => ServerMessage::PresetPreview {
                        nonce,
                        preview: Some(preview),
                        error: None,
                    },
                    Err(err) => ServerMessage::PresetPreview {
                        nonce,
                        preview: None,
                        error: Some(err.to_string()),
                    },
                },
                Err(err) => ServerMessage::PresetPreview {
                    nonce,
                    preview: None,
                    error: Some(err.to_string()),
                },
            };
            json_message(&message)
        }

        ClientMessage::ReorderPresets { names } => {
            let presets = {
                let mut domain = ctx.domain.lock().unwrap();
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
            };
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::Export => {
            ensure_local_ui(origin, "export")?;

            let domain = ctx.domain.lock().unwrap();
            let mut data = ExportData {
                collars: domain.collars.clone(),
                presets: domain.presets.clone(),
            };
            drop(domain);
            normalize_presets(&mut data.presets);
            json_message(&ServerMessage::ExportData { data: &data })
        }

        ClientMessage::Import { mut data } => {
            ensure_local_ui(origin, "import")?;

            normalize_presets(&mut data.presets);
            validation::validate_export_data(&data).map_err(|err| err.to_string())?;
            let (collars, presets) = {
                let mut domain = ctx.domain.lock().unwrap();
                stop_active_preset(&mut domain, &ctx.preset_run_id);
                domain.collars = data.collars;
                domain.presets = data.presets;
                (domain.collars.clone(), domain.presets.clone())
            };
            cancel_all_manual_actions(ctx);
            log_storage_result("save_collars", save_collars(ctx, &collars));
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }
    }
}

fn resolve_collar_command(
    ctx: &AppCtx,
    collar_name: &str,
    mode: CommandMode,
    intensity: u8,
) -> core::result::Result<(Collar, u8), String> {
    let (collar, lockout) = {
        let domain = ctx.domain.lock().unwrap();
        (
            domain
                .collars
                .iter()
                .find(|collar| collar.name == collar_name)
                .cloned(),
            rf_lockout_remaining_ms(&domain),
        )
    };

    if lockout > 0 {
        return Err("Transmissions locked after STOP".to_string());
    }
    if mode.has_intensity() && intensity > MAX_INTENSITY {
        return Err(format!(
            "Intensity {} exceeds max {}",
            intensity, MAX_INTENSITY
        ));
    }

    let collar = collar.ok_or_else(|| format!("Unknown collar: {collar_name}"))?;
    Ok((collar, command_intensity(mode, intensity)))
}

fn stop_manual_action(ctx: &AppCtx, collar_name: &str, mode: CommandMode) {
    let key = ActionKey {
        collar_name: collar_name.to_string(),
        mode,
    };
    if let Some(handle) = ctx.active_actions.lock().unwrap().remove(&key) {
        handle.cancel.store(false, Ordering::SeqCst);
    }
}

fn cancel_all_manual_actions(ctx: &AppCtx) {
    stop_handles(
        ctx.active_actions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, handle)| handle)
            .collect(),
    );
}

fn stop_handles(handles: Vec<ActiveActionHandle>) {
    for handle in handles {
        handle.cancel.store(false, Ordering::SeqCst);
    }
}

fn start_manual_action(
    ctx: &AppCtx,
    collar_name: String,
    mode: CommandMode,
    intensity: u8,
    intensity_max: Option<u8>,
    duration_ms: Option<u32>,
    duration_max_ms: Option<u32>,
    intensity_distribution: Distribution,
    duration_distribution: Distribution,
    source: crate::protocol::EventSource,
    owner: Option<ActionOwner>,
    cancel_on_disconnect: bool,
) -> core::result::Result<(), String> {
    let (collar, normalized_intensity) =
        resolve_collar_command(ctx, &collar_name, mode, intensity)?;
    if matches!(duration_ms, Some(0)) {
        return Err("Action duration must be greater than zero".to_string());
    }
    if cancel_on_disconnect && owner.is_none() {
        return Err("Held actions require an owning connection".to_string());
    }

    let spec = ManualActionSpec {
        key: ActionKey { collar_name, mode },
        collar_id: collar.collar_id,
        channel: collar.channel,
        mode,
        intensity: normalized_intensity,
        intensity_max,
        duration_ms,
        duration_max_ms,
        intensity_distribution,
        duration_distribution,
        source,
    };

    let run_id = ctx.next_action_id.fetch_add(1, Ordering::SeqCst) + 1;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let mut active_actions = ctx.active_actions.lock().unwrap();
        if let Some(previous) = active_actions.insert(
            spec.key.clone(),
            ActiveActionHandle {
                run_id,
                cancel: cancel.clone(),
                owner,
                cancel_on_disconnect,
            },
        ) {
            previous.cancel.store(false, Ordering::SeqCst);
        }
    }

    let ctx2 = ctx.clone();
    std::thread::Builder::new()
        .name("manual-action".into())
        .stack_size(32768)
        .spawn(move || {
            run_manual_action(spec, &ctx2, run_id, cancel);
        })
        .map_err(|err| format!("Failed to spawn manual action thread: {err}"))?;

    Ok(())
}

fn run_manual_action(
    spec: ManualActionSpec,
    ctx: &AppCtx,
    run_id: u32,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    let cleanup_key = spec.key.clone();
    let started_at = Instant::now();
    let actual_intensity = match spec.intensity_max {
        Some(max) if max > spec.intensity && spec.mode.has_intensity() => {
            let mut rng = ctx.rng.lock().unwrap();
            resolve_random_u8(&mut *rng, spec.intensity, max, spec.intensity_distribution)
        }
        _ => spec.intensity,
    };
    let actual_duration_ms = match (spec.duration_ms, spec.duration_max_ms) {
        (Some(min), Some(max)) if max > min => {
            let mut rng = ctx.rng.lock().unwrap();
            Some(resolve_random_duration(
                &mut *rng,
                min,
                max,
                spec.duration_distribution,
            ))
        }
        (duration, _) => duration,
    };
    let deadline = actual_duration_ms
        .map(|duration_ms| started_at + Duration::from_millis(duration_ms as u64));

    'outer: loop {
        if !cancel.load(Ordering::SeqCst) {
            break;
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                break;
            }
        }

        if let Err(err) = rf_send_with_led(
            &ctx.rf,
            &ctx.tx_led,
            spec.collar_id,
            spec.channel,
            spec.mode.to_rf_byte(),
            actual_intensity,
        ) {
            error!("RF send error during manual action: {err:#}");
        }

        let wait_deadline = Instant::now() + Duration::from_millis(MANUAL_ACTION_REPEAT_MS);
        loop {
            if !cancel.load(Ordering::SeqCst) {
                break 'outer;
            }
            let now = Instant::now();
            let next_tick = match deadline {
                Some(deadline) if deadline < wait_deadline => deadline,
                _ => wait_deadline,
            };
            if now >= next_tick {
                break;
            }
            std::thread::sleep(
                (next_tick - now).min(Duration::from_millis(MANUAL_ACTION_SLEEP_SLICE_MS)),
            );
        }
    }

    {
        let mut active_actions = ctx.active_actions.lock().unwrap();
        if active_actions.get(&cleanup_key).map(|handle| handle.run_id) == Some(run_id) {
            active_actions.remove(&cleanup_key);
        }
    }

    let elapsed_ms = started_at.elapsed().as_millis().min(u32::MAX as u128) as u32;
    ctx.record_event(
        spec.source,
        EventLogEntryKind::Action {
            collar_name: spec.key.collar_name,
            mode: spec.mode,
            intensity: spec.mode.has_intensity().then_some(actual_intensity),
            duration_ms: elapsed_ms,
        },
    );
}

fn ensure_local_ui(origin: MessageOrigin, operation: &str) -> core::result::Result<(), String> {
    if origin == MessageOrigin::RemoteControl {
        Err(format!("{operation} is not available over remote control"))
    } else {
        Ok(())
    }
}

fn json_message(message: &impl serde::Serialize) -> ControlResult {
    Ok(vec![serde_json::to_string(message).unwrap()])
}

fn normalize_presets(presets: &mut [Preset]) {
    for preset in presets {
        preset.normalize();
    }
}
