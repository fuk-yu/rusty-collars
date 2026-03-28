use std::collections::HashSet;

use anyhow::anyhow;
use anyhow::Result;

use crate::protocol::{Collar, ExportData, Preset, MAX_CHANNEL, MAX_INTENSITY};

pub fn validate_collar(collar: &Collar) -> Result<()> {
    if collar.name.trim().is_empty() {
        return Err(anyhow!("Collar name cannot be empty"));
    }
    if collar.channel > MAX_CHANNEL {
        return Err(anyhow!(
            "Collar '{}' has invalid channel {}",
            collar.name,
            collar.channel
        ));
    }
    Ok(())
}

pub fn validate_collars(collars: &[Collar]) -> Result<()> {
    let mut names = HashSet::new();
    for collar in collars {
        validate_collar(collar)?;
        if !names.insert(collar.name.as_str()) {
            return Err(anyhow!("Duplicate collar name '{}'", collar.name));
        }
    }
    Ok(())
}

pub fn validate_preset(preset: &Preset, collars: &[Collar]) -> Result<()> {
    if preset.name.trim().is_empty() {
        return Err(anyhow!("Preset name cannot be empty"));
    }
    if preset.tracks.is_empty() {
        return Err(anyhow!("Preset '{}' has no tracks", preset.name));
    }

    let known_collars: HashSet<&str> = collars.iter().map(|c| c.name.as_str()).collect();
    let mut runnable_steps = 0usize;

    for (track_index, track) in preset.tracks.iter().enumerate() {
        if track.collar_name.trim().is_empty() {
            return Err(anyhow!(
                "Preset '{}' track {} has no collar selected",
                preset.name,
                track_index
            ));
        }
        if !known_collars.contains(track.collar_name.as_str()) {
            return Err(anyhow!(
                "Preset '{}' track {} references unknown collar '{}'",
                preset.name,
                track_index,
                track.collar_name
            ));
        }
        if track.steps.is_empty() {
            return Err(anyhow!(
                "Preset '{}' track {} has no steps",
                preset.name,
                track_index
            ));
        }

        for (step_index, step) in track.steps.iter().enumerate() {
            if step.duration_ms == 0 {
                return Err(anyhow!(
                    "Preset '{}' track {} step {} has zero duration",
                    preset.name,
                    track_index,
                    step_index
                ));
            }
            if step.mode.to_command_mode().is_some() {
                if step.intensity > MAX_INTENSITY {
                    return Err(anyhow!(
                        "Preset '{}' track {} step {} has invalid intensity {}",
                        preset.name,
                        track_index,
                        step_index,
                        step.intensity
                    ));
                }
                runnable_steps += 1;
            }
        }
    }

    if runnable_steps == 0 {
        return Err(anyhow!(
            "Preset '{}' has no runnable command steps",
            preset.name
        ));
    }

    Ok(())
}

pub fn validate_presets(presets: &[Preset], collars: &[Collar]) -> Result<()> {
    let mut names = HashSet::new();
    for preset in presets {
        validate_preset(preset, collars)?;
        if !names.insert(preset.name.as_str()) {
            return Err(anyhow!("Duplicate preset name '{}'", preset.name));
        }
    }
    Ok(())
}

pub fn validate_export_data(data: &ExportData) -> Result<()> {
    validate_collars(&data.collars)?;
    validate_presets(&data.presets, &data.collars)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PresetStep, PresetStepMode, PresetTrack};

    fn collar(name: &str, id: u16, ch: u8) -> Collar {
        Collar {
            name: name.to_string(),
            collar_id: id,
            channel: ch,
        }
    }

    fn step(mode: PresetStepMode, intensity: u8, duration_ms: u32) -> PresetStep {
        PresetStep {
            mode,
            intensity,
            duration_ms,
        }
    }

    fn preset(name: &str, tracks: Vec<PresetTrack>) -> Preset {
        Preset {
            name: name.to_string(),
            tracks,
        }
    }

    fn track(collar_name: &str, steps: Vec<PresetStep>) -> PresetTrack {
        PresetTrack {
            collar_name: collar_name.to_string(),
            steps,
        }
    }

    // --- validate_collar ---

    #[test]
    fn collar_valid() {
        assert!(validate_collar(&collar("Rex", 0x1234, 0)).is_ok());
        assert!(validate_collar(&collar("Max", 0xFFFF, 2)).is_ok());
    }

    #[test]
    fn collar_empty_name() {
        assert!(validate_collar(&collar("", 0x1234, 0)).is_err());
    }

    #[test]
    fn collar_whitespace_name() {
        assert!(validate_collar(&collar("   ", 0x1234, 0)).is_err());
    }

    #[test]
    fn collar_channel_out_of_range() {
        assert!(validate_collar(&collar("Rex", 0x1234, 3)).is_err());
        assert!(validate_collar(&collar("Rex", 0x1234, 255)).is_err());
    }

    // --- validate_collars ---

    #[test]
    fn collars_no_duplicates() {
        let collars = vec![collar("Rex", 0x1234, 0), collar("Max", 0xABCD, 1)];
        assert!(validate_collars(&collars).is_ok());
    }

    #[test]
    fn collars_duplicate_name() {
        let collars = vec![collar("Rex", 0x1234, 0), collar("Rex", 0xABCD, 1)];
        assert!(validate_collars(&collars).is_err());
    }

    #[test]
    fn collars_empty_is_valid() {
        assert!(validate_collars(&[]).is_ok());
    }

    // --- validate_preset ---

    #[test]
    fn preset_valid_simple() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![step(PresetStepMode::Vibrate, 50, 1000)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_ok());
    }

    #[test]
    fn preset_empty_name() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "",
            vec![track(
                "Rex",
                vec![step(PresetStepMode::Vibrate, 50, 1000)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_no_tracks() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset("test", vec![]);
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_unknown_collar() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Unknown",
                vec![step(PresetStepMode::Vibrate, 50, 1000)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_empty_steps() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset("test", vec![track("Rex", vec![])]);
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_zero_duration_step() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![step(PresetStepMode::Vibrate, 50, 0)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_intensity_too_high() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![step(PresetStepMode::Shock, 100, 1000)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_intensity_at_max() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![step(PresetStepMode::Shock, 99, 1000)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_ok());
    }

    #[test]
    fn preset_pause_only_rejected() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![step(PresetStepMode::Pause, 0, 1000)],
            )],
        );
        assert!(validate_preset(&p, &collars).is_err());
    }

    #[test]
    fn preset_pause_plus_command_ok() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![
                    step(PresetStepMode::Pause, 0, 500),
                    step(PresetStepMode::Vibrate, 30, 1000),
                ],
            )],
        );
        assert!(validate_preset(&p, &collars).is_ok());
    }

    #[test]
    fn preset_pause_intensity_ignored() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let p = preset(
            "test",
            vec![track(
                "Rex",
                vec![
                    step(PresetStepMode::Pause, 255, 500),
                    step(PresetStepMode::Beep, 50, 1000),
                ],
            )],
        );
        // Pause with intensity 255 is allowed (intensity not validated for pause)
        assert!(validate_preset(&p, &collars).is_ok());
    }

    // --- validate_presets ---

    #[test]
    fn presets_duplicate_name() {
        let collars = vec![collar("Rex", 0x1234, 0)];
        let presets = vec![
            preset(
                "test",
                vec![track(
                    "Rex",
                    vec![step(PresetStepMode::Vibrate, 50, 1000)],
                )],
            ),
            preset(
                "test",
                vec![track(
                    "Rex",
                    vec![step(PresetStepMode::Beep, 30, 500)],
                )],
            ),
        ];
        assert!(validate_presets(&presets, &collars).is_err());
    }

    // --- validate_export_data ---

    #[test]
    fn export_data_valid() {
        let data = ExportData {
            collars: vec![collar("Rex", 0x1234, 0)],
            presets: vec![preset(
                "test",
                vec![track(
                    "Rex",
                    vec![step(PresetStepMode::Vibrate, 50, 1000)],
                )],
            )],
        };
        assert!(validate_export_data(&data).is_ok());
    }

    #[test]
    fn export_data_empty() {
        let data = ExportData {
            collars: vec![],
            presets: vec![],
        };
        assert!(validate_export_data(&data).is_ok());
    }

    #[test]
    fn export_data_preset_references_missing_collar() {
        let data = ExportData {
            collars: vec![],
            presets: vec![preset(
                "test",
                vec![track(
                    "Ghost",
                    vec![step(PresetStepMode::Vibrate, 50, 1000)],
                )],
            )],
        };
        assert!(validate_export_data(&data).is_err());
    }
}
