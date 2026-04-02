use crate::error::ControlError;
use crate::protocol::Collar;
use crate::server::{stop_active_preset, AppCtx, ControlResult};
use crate::validation;

use super::actions::cancel_all_manual_actions;

pub(super) fn add(ctx: &AppCtx, name: String, collar_id: u16, channel: u8) -> ControlResult {
    let collar = Collar {
        name,
        collar_id,
        channel,
    };
    let collars = {
        let mut domain = ctx.domain.lock().unwrap();
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
        domain.collars.clone()
    };
    ctx.persist_collars(&collars);
    ctx.broadcast_state();
    Ok(Vec::new())
}

pub(super) fn update(
    ctx: &AppCtx,
    original_name: String,
    name: String,
    collar_id: u16,
    channel: u8,
) -> ControlResult {
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
        stop_active_preset(&mut domain, &ctx.worker.preset_run_id);
        (domain.collars.clone(), domain.presets.clone())
    };
    cancel_all_manual_actions(ctx);
    ctx.persist_collars(&collars);
    ctx.persist_presets(&presets);
    ctx.broadcast_state();
    Ok(Vec::new())
}

pub(super) fn delete(ctx: &AppCtx, name: String) -> ControlResult {
    let collars = {
        let mut domain = ctx.domain.lock().unwrap();
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
        stop_active_preset(&mut domain, &ctx.worker.preset_run_id);
        domain.collars.clone()
    };
    cancel_all_manual_actions(ctx);
    ctx.persist_collars(&collars);
    ctx.broadcast_state();
    Ok(Vec::new())
}
