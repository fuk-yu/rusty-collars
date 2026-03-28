use anyhow::anyhow;
use anyhow::Result;

use crate::protocol::{
    encode_rf_frame, Collar, CommandMode, Preset, PresetPreview, PresetPreviewEvent,
};
use crate::rf_timing::{
    RF_COMMAND_COVERAGE_US, RF_COMMAND_REPEAT_SPAN_US, RF_COMMAND_TRANSMIT_DURATION_US,
};

const MICROS_PER_MILLISECOND: u64 = 1_000;

/// A single scheduled RF transmission event.
#[derive(Debug, Clone)]
pub struct PresetEvent {
    pub time_us: u64,
    pub collar_id: u16,
    pub channel: u8,
    pub mode_byte: u8,
    pub intensity: u8,
}

/// Schedule RF retransmission events for a single preset step.
///
/// Transmits at step start, then schedules later retransmits as late as possible
/// while keeping the command refreshed through the whole step. A command may
/// occupy the transmitter past step end, but all repeated frame starts remain
/// within the step.
pub fn schedule_step_events(
    events: &mut Vec<PresetEvent>,
    start_time_us: u64,
    end_time_us: u64,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
) -> Result<()> {
    if start_time_us >= end_time_us {
        return Ok(());
    }

    let step_duration_us = end_time_us - start_time_us;
    if step_duration_us < RF_COMMAND_REPEAT_SPAN_US {
        return Err(anyhow!(
            "step duration {}us is shorter than RF repeat span {}us",
            step_duration_us,
            RF_COMMAND_REPEAT_SPAN_US
        ));
    }

    let latest_start_us = end_time_us - RF_COMMAND_REPEAT_SPAN_US;
    let mut t = start_time_us;
    loop {
        events.push(PresetEvent {
            time_us: t,
            collar_id,
            channel,
            mode_byte,
            intensity,
        });

        if t + RF_COMMAND_COVERAGE_US >= end_time_us {
            return Ok(());
        }

        let next_earliest = t + RF_COMMAND_TRANSMIT_DURATION_US;
        let next_latest = (t + RF_COMMAND_COVERAGE_US).min(latest_start_us);
        if next_earliest > next_latest {
            return Err(anyhow!(
                "step cannot be sustained without overlapping transmissions"
            ));
        }
        t = next_latest;
    }
}

#[derive(Debug, Clone)]
struct ScheduledPresetEvent {
    event: PresetEvent,
    requested_time_us: u64,
    track_index: usize,
    step_index: usize,
    collar_name: String,
    mode: CommandMode,
}

pub fn schedule_preset_events(preset: &Preset, collars: &[Collar]) -> Result<Vec<PresetEvent>> {
    let mut events = collect_preset_events(preset, collars)?;
    serialize_preset_events(&mut events);

    Ok(events
        .into_iter()
        .map(|scheduled| scheduled.event)
        .collect())
}

pub fn preview_preset(preset: &Preset, collars: &[Collar]) -> Result<PresetPreview> {
    let total_duration_us = preset
        .tracks
        .iter()
        .map(|track| {
            track
                .steps
                .iter()
                .map(|step| u64::from(step.duration_ms) * MICROS_PER_MILLISECOND)
                .sum::<u64>()
        })
        .max()
        .unwrap_or(0);
    let mut events = collect_preset_events(preset, collars)?;
    serialize_preset_events(&mut events);

    Ok(PresetPreview {
        total_duration_us,
        events: events
            .into_iter()
            .map(|scheduled| {
                let raw = encode_rf_frame(
                    scheduled.event.collar_id,
                    scheduled.event.channel,
                    scheduled.event.mode_byte,
                    scheduled.event.intensity,
                );
                PresetPreviewEvent {
                    requested_time_us: scheduled.requested_time_us,
                    actual_time_us: scheduled.event.time_us,
                    track_index: scheduled.track_index,
                    step_index: scheduled.step_index,
                    transmit_duration_us: RF_COMMAND_TRANSMIT_DURATION_US,
                    collar_name: scheduled.collar_name,
                    collar_id: scheduled.event.collar_id,
                    channel: scheduled.event.channel,
                    mode: scheduled.mode,
                    mode_byte: scheduled.event.mode_byte,
                    intensity: scheduled.event.intensity,
                    raw_hex: format_rf_frame_hex(&raw),
                }
            })
            .collect(),
    })
}

fn collect_preset_events(preset: &Preset, collars: &[Collar]) -> Result<Vec<ScheduledPresetEvent>> {
    let mut events: Vec<ScheduledPresetEvent> = Vec::new();

    for (track_index, track) in preset.tracks.iter().enumerate() {
        let collar = collars
            .iter()
            .find(|collar| collar.name == track.collar_name)
            .ok_or_else(|| {
                anyhow!(
                    "Preset '{}' track {} references unknown collar '{}'",
                    preset.name,
                    track_index,
                    track.collar_name
                )
            })?;

        let mut time_us = 0u64;
        for (step_index, step) in track.steps.iter().enumerate() {
            let end_time_us = time_us + u64::from(step.duration_ms) * MICROS_PER_MILLISECOND;
            if let Some(mode) = step.mode.to_command_mode() {
                let mut step_events = Vec::new();
                schedule_step_events(
                    &mut step_events,
                    time_us,
                    end_time_us,
                    collar.collar_id,
                    collar.channel,
                    mode.to_rf_byte(),
                    step.intensity,
                )
                .map_err(|err| {
                    anyhow!(
                        "Preset '{}' track {} step {} is unschedulable: {err}",
                        preset.name,
                        track_index,
                        step_index
                    )
                })?;

                events.extend(step_events.into_iter().map(|event| ScheduledPresetEvent {
                    requested_time_us: event.time_us,
                    event,
                    track_index,
                    step_index,
                    collar_name: track.collar_name.clone(),
                    mode,
                }));
            }
            time_us = end_time_us;
        }
    }

    Ok(events)
}

fn serialize_preset_events(events: &mut [ScheduledPresetEvent]) {
    events.sort_by(|left, right| {
        left.event
            .time_us
            .cmp(&right.event.time_us)
            .then(left.track_index.cmp(&right.track_index))
            .then(left.step_index.cmp(&right.step_index))
    });

    let mut transmitter_free_at_us = 0u64;
    for scheduled in events {
        if scheduled.event.time_us < transmitter_free_at_us {
            scheduled.event.time_us = transmitter_free_at_us;
        }
        transmitter_free_at_us = scheduled.event.time_us + RF_COMMAND_TRANSMIT_DURATION_US;
    }
}

fn format_rf_frame_hex(frame: &[u8; 5]) -> String {
    let mut raw_hex = String::with_capacity(frame.len() * 2);
    for byte in frame {
        use core::fmt::Write as _;

        write!(&mut raw_hex, "{byte:02X}").unwrap();
    }
    raw_hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rf_timing::{
        RETRANSMIT_INTERVAL_US, RF_COMMAND_COVERAGE_US, RF_COMMAND_REPEAT_SPAN_US,
        RF_COMMAND_TRANSMIT_DURATION_US,
    };

    fn times(events: &[PresetEvent]) -> Vec<u64> {
        events.iter().map(|e| e.time_us).collect()
    }

    #[test]
    fn very_short_step_single_event() {
        let mut events = Vec::new();
        let err = schedule_step_events(&mut events, 0, 88_000, 0x1234, 0, 2, 50).unwrap_err();
        assert!(err.to_string().contains("shorter than RF repeat span"));
        assert!(events.is_empty());
    }

    #[test]
    fn step_130ms_single_event() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 130_000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0]);
    }

    #[test]
    fn step_201ms_single_event() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 201_000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0]);
    }

    #[test]
    fn step_329ms_two_events() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 329_000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 240_700]);
    }

    #[test]
    fn step_500ms_two_events() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 500_000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, RF_COMMAND_COVERAGE_US]);
    }

    #[test]
    fn one_second_step() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 1_000_000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 288_300, 576_600, 864_900]);
    }

    #[test]
    fn two_second_step() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 2_000_000, 0x1234, 0, 2, 50).unwrap();
        let t = times(&events);
        assert_eq!(t.len(), 7);
        assert_eq!(t[0], 0);
        assert_eq!(t[6], 1_729_800);
    }

    #[test]
    fn step_with_offset_start() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 500_000, 1_500_000, 0x1234, 0, 1, 30).unwrap();
        assert_eq!(times(&events), vec![500_000, 788_300, 1_076_600, 1_364_900]);
    }

    #[test]
    fn inverted_range_produces_nothing() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 1_000_000, 500_000, 0x1234, 0, 2, 50).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn zero_duration_produces_nothing() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 1_000_000, 1_000_000, 0x1234, 0, 2, 50).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn event_fields_correct() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 100_000, 250_000, 0xABCD, 2, 3, 77).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time_us, 100_000);
        assert_eq!(events[0].collar_id, 0xABCD);
        assert_eq!(events[0].channel, 2);
        assert_eq!(events[0].mode_byte, 3);
        assert_eq!(events[0].intensity, 77);
    }

    #[test]
    fn no_event_past_end() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 400_000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 288_300]);
    }

    #[test]
    fn exact_timing_constants_match_waveform() {
        assert_eq!(RF_COMMAND_REPEAT_SPAN_US, 88_300);
        assert_eq!(RETRANSMIT_INTERVAL_US, 200_000);
        assert_eq!(RF_COMMAND_TRANSMIT_DURATION_US, 132_450);
    }

    #[test]
    fn preset_schedule_serializes_overlapping_tracks() {
        let collars = vec![
            Collar {
                name: "Rex".to_string(),
                collar_id: 0x1234,
                channel: 0,
            },
            Collar {
                name: "Max".to_string(),
                collar_id: 0x5678,
                channel: 1,
            },
        ];
        let preset = Preset {
            name: "test".to_string(),
            tracks: vec![
                crate::protocol::PresetTrack {
                    collar_name: "Rex".to_string(),
                    steps: vec![crate::protocol::PresetStep {
                        mode: crate::protocol::PresetStepMode::Vibrate,
                        intensity: 50,
                        duration_ms: 500,
                    }],
                },
                crate::protocol::PresetTrack {
                    collar_name: "Max".to_string(),
                    steps: vec![crate::protocol::PresetStep {
                        mode: crate::protocol::PresetStepMode::Vibrate,
                        intensity: 50,
                        duration_ms: 500,
                    }],
                },
            ],
        };

        let events = schedule_preset_events(&preset, &collars).unwrap();
        assert_eq!(times(&events), vec![0, 132_450, 288_300, 420_750]);
    }

    #[test]
    fn preview_reports_requested_and_actual_times() {
        let collars = vec![
            Collar {
                name: "Rex".to_string(),
                collar_id: 0x1234,
                channel: 0,
            },
            Collar {
                name: "Max".to_string(),
                collar_id: 0x5678,
                channel: 1,
            },
        ];
        let preset = Preset {
            name: "test".to_string(),
            tracks: vec![
                crate::protocol::PresetTrack {
                    collar_name: "Rex".to_string(),
                    steps: vec![crate::protocol::PresetStep {
                        mode: crate::protocol::PresetStepMode::Beep,
                        intensity: 0,
                        duration_ms: 500,
                    }],
                },
                crate::protocol::PresetTrack {
                    collar_name: "Max".to_string(),
                    steps: vec![crate::protocol::PresetStep {
                        mode: crate::protocol::PresetStepMode::Shock,
                        intensity: 25,
                        duration_ms: 500,
                    }],
                },
            ],
        };

        let preview = preview_preset(&preset, &collars).unwrap();
        assert_eq!(preview.total_duration_us, 500_000);
        assert_eq!(preview.events.len(), 4);
        assert_eq!(preview.events[0].requested_time_us, 0);
        assert_eq!(preview.events[0].actual_time_us, 0);
        assert_eq!(preview.events[0].transmit_duration_us, 132_450);
        assert_eq!(preview.events[1].requested_time_us, 0);
        assert_eq!(preview.events[1].actual_time_us, 132_450);
        assert_eq!(preview.events[1].mode, CommandMode::Shock);
        assert_eq!(preview.events[1].raw_hex, "56781119F8");
    }
}
