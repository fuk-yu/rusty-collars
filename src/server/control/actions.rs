use std::time::{Duration, Instant};

use log::{error, info};

use crate::error::ControlError;
use crate::protocol::{ButtonAction, CommandMode, Distribution, MAX_INTENSITY};
use crate::{scheduling, validation};

use super::super::{
    command_intensity, event_source, resolve_random_duration, resolve_random_u8,
    rf_lockout_remaining_ms, rf_send_with_led, ActionKey, ActionOwner, ActiveActionHandle, AppCtx,
    ControlResult, RandomResolver,
};
use super::ControlDispatcher;

pub(super) fn send_command(
    ctx: &AppCtx,
    collar_name: String,
    mode: CommandMode,
    intensity: u8,
) -> ControlResult {
    let (collar, intensity) = resolve_collar_command(ctx, &collar_name, mode, intensity)?;
    if let Err(err) = rf_send_with_led(
        &ctx.hardware.rf,
        &ctx.hardware.tx_led,
        collar.collar_id,
        collar.channel,
        mode.to_rf_byte(),
        intensity,
    ) {
        error!("RF send error: {err:#}");
    }
    Ok(Vec::new())
}

pub(super) fn record_button_event(
    collar_name: String,
    mode: CommandMode,
    intensity: u8,
    action: ButtonAction,
) -> ControlResult {
    if cfg!(debug_assertions) {
        info!(
            "Button {:?}: collar={collar_name} mode={mode:?} intensity={intensity}",
            action
        );
    }
    Ok(Vec::new())
}

pub(super) fn run_action(
    dispatcher: &ControlDispatcher<'_>,
    collar_name: String,
    mode: CommandMode,
    intensity: u8,
    duration_ms: u32,
    intensity_max: Option<u8>,
    duration_max_ms: Option<u32>,
    intensity_distribution: Option<Distribution>,
    duration_distribution: Option<Distribution>,
) -> ControlResult {
    start_manual_action(
        dispatcher.ctx,
        collar_name,
        mode,
        intensity,
        intensity_max,
        Some(duration_ms),
        duration_max_ms,
        intensity_distribution.unwrap_or_default(),
        duration_distribution.unwrap_or_default(),
        event_source(dispatcher.origin),
        dispatcher.owner,
        false,
    )?;
    Ok(Vec::new())
}

pub(super) fn start_action(
    dispatcher: &ControlDispatcher<'_>,
    collar_name: String,
    mode: CommandMode,
    intensity: u8,
    intensity_max: Option<u8>,
    intensity_distribution: Option<Distribution>,
) -> ControlResult {
    start_manual_action(
        dispatcher.ctx,
        collar_name,
        mode,
        intensity,
        intensity_max,
        None,
        None,
        intensity_distribution.unwrap_or_default(),
        Distribution::default(),
        event_source(dispatcher.origin),
        dispatcher.owner,
        true,
    )?;
    Ok(Vec::new())
}

pub(super) fn stop_action(ctx: &AppCtx, collar_name: String, mode: CommandMode) -> ControlResult {
    ctx.cancel_manual_action(ActionKey { collar_name, mode });
    Ok(Vec::new())
}

pub(super) fn run_preset(dispatcher: &ControlDispatcher<'_>, name: String) -> ControlResult {
    let source = event_source(dispatcher.origin);
    let (preset_name, resolved_preset_for_log, events) = {
        let domain = dispatcher.ctx.domain.lock().unwrap();
        if rf_lockout_remaining_ms(&domain) > 0 {
            return Err(ControlError::TransmissionLockout);
        }

        let Some(preset) = domain
            .presets
            .iter()
            .find(|preset| preset.name == name)
            .cloned()
        else {
            return Err(ControlError::UnknownPreset(name.clone()));
        };
        validation::validate_preset(&preset, &domain.collars)
            .map_err(|err| ControlError::Validation(err.to_string()))?;

        let has_random = preset
            .tracks
            .iter()
            .any(|track| track.steps.iter().any(|step| step.has_random()));
        let mut rng = dispatcher.ctx.worker.rng.lock().unwrap();
        let mut resolver = RandomResolver { rng: &mut *rng };
        let resolved = scheduling::resolve_preset(&preset, &mut resolver);
        let events = scheduling::schedule_preset_events(
            &resolved,
            &domain.collars,
            &mut scheduling::MidpointResolver,
        )
        .map_err(|err| ControlError::Validation(err.to_string()))?;
        let resolved_for_log = has_random.then_some(resolved);

        (preset.name.clone(), resolved_for_log, events)
    };

    dispatcher
        .ctx
        .start_preset_run(preset_name, source, resolved_preset_for_log, events)
}

pub(super) fn stop_preset(ctx: &AppCtx) -> ControlResult {
    ctx.stop_preset_run()
}

pub(super) fn stop_all(ctx: &AppCtx) -> ControlResult {
    ctx.stop_all_run()
}

pub(super) fn cancel_owned_manual_actions(ctx: &AppCtx, owner: ActionOwner) {
    ctx.cancel_owned_manual_actions(owner);
}

fn resolve_collar_command(
    ctx: &AppCtx,
    collar_name: &str,
    mode: CommandMode,
    intensity: u8,
) -> core::result::Result<(crate::protocol::Collar, u8), ControlError> {
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
        return Err(ControlError::TransmissionLockout);
    }
    if mode.has_intensity() && intensity > MAX_INTENSITY {
        return Err(ControlError::InvalidIntensity {
            intensity,
            max: MAX_INTENSITY,
        });
    }

    let collar = collar.ok_or_else(|| ControlError::UnknownCollar(collar_name.to_string()))?;
    Ok((collar, command_intensity(mode, intensity)))
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
) -> core::result::Result<(), ControlError> {
    let (collar, normalized_intensity) =
        resolve_collar_command(ctx, &collar_name, mode, intensity)?;
    if matches!(duration_ms, Some(0)) {
        return Err(ControlError::ActionDurationZero);
    }
    if cancel_on_disconnect && owner.is_none() {
        return Err(ControlError::HeldActionRequiresOwner);
    }

    let actual_intensity = match intensity_max {
        Some(max) if max > normalized_intensity && mode.has_intensity() => {
            let mut rng = ctx.worker.rng.lock().unwrap();
            resolve_random_u8(&mut *rng, normalized_intensity, max, intensity_distribution)
        }
        _ => normalized_intensity,
    };

    let now = Instant::now();
    let actual_duration_ms = match (duration_ms, duration_max_ms) {
        (Some(min), Some(max)) if max > min => {
            let mut rng = ctx.worker.rng.lock().unwrap();
            Some(resolve_random_duration(
                &mut *rng,
                min,
                max,
                duration_distribution,
            ))
        }
        (duration, _) => duration,
    };
    let deadline =
        actual_duration_ms.map(|duration_ms| now + Duration::from_millis(duration_ms as u64));

    let key = ActionKey { collar_name, mode };
    let handle = ActiveActionHandle {
        owner,
        cancel_on_disconnect,
        collar_id: collar.collar_id,
        channel: collar.channel,
        mode_byte: mode.to_rf_byte(),
        intensity: actual_intensity,
        deadline,
        started_at: now,
        source,
    };

    ctx.set_manual_action(key, handle);
    Ok(())
}
