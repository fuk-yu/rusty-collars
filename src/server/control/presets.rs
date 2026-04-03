use crate::protocol::{Preset, ServerMessage};
use crate::server::{AppCtx, ControlResult};
use crate::{scheduling, validation};

use super::json_message;

pub(super) fn save(
    ctx: &AppCtx,
    original_name: Option<String>,
    mut preset: Preset,
) -> ControlResult {
    preset.normalize();
    ctx.save_preset(original_name, preset)
}

pub(super) fn delete(ctx: &AppCtx, name: String) -> ControlResult {
    ctx.delete_preset(name)
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
    ctx.reorder_presets(names)
}
