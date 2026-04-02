use std::collections::HashMap;

use crate::error::ControlError;
use crate::protocol::{Preset, ServerMessage};
use crate::server::{stop_active_preset, AppCtx, ControlResult};
use crate::{scheduling, validation};

use super::json_message;

pub(super) fn save(
    ctx: &AppCtx,
    original_name: Option<String>,
    mut preset: Preset,
) -> ControlResult {
    preset.normalize();
    let (presets, preset_stopped) = {
        let mut domain = ctx.domain.lock().unwrap();
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
        let preset_stopped = stop_active_preset(&mut domain);
        domain.presets = updated;
        (domain.presets.clone(), preset_stopped)
    };
    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

pub(super) fn delete(ctx: &AppCtx, name: String) -> ControlResult {
    let (presets, preset_stopped) = {
        let mut domain = ctx.domain.lock().unwrap();
        let before = domain.presets.len();
        domain.presets.retain(|preset| preset.name != name);
        if domain.presets.len() == before {
            return Err(ControlError::UnknownPreset(name));
        }
        let preset_stopped = stop_active_preset(&mut domain);
        (domain.presets.clone(), preset_stopped)
    };
    if preset_stopped {
        ctx.stop_preset_execution();
    }
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

pub(super) fn preview(ctx: &AppCtx, nonce: u32, mut preset: Preset) -> ControlResult {
    preset.normalize();
    let collars = ctx.domain.lock().unwrap().collars.clone();
    let (preview, error) = match validation::validate_preset(&preset, &collars)
        .and_then(|()| scheduling::preview_preset(&preset, &collars))
    {
        Ok(preview) => (Some(preview), None),
        Err(err) => (None, Some(err.to_string())),
    };
    json_message(&ServerMessage::PresetPreview {
        nonce,
        preview,
        error,
    })
}

pub(super) fn reorder(ctx: &AppCtx, names: Vec<String>) -> ControlResult {
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
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}
