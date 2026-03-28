use anyhow::anyhow;
use anyhow::Result;

use crate::protocol::{Collar, Preset};

/// Each RF command occupies the transmitter for roughly 130ms (3x43ms).
pub const RF_COMMAND_TRANSMIT_DURATION_MS: u64 = 130;

/// Maximum gap between command starts for sustained collar activation.
pub const RETRANSMIT_INTERVAL_MS: u64 = 200;

/// A single scheduled RF transmission event.
#[derive(Debug, Clone)]
pub struct PresetEvent {
    pub time_ms: u64,
    pub collar_id: u16,
    pub channel: u8,
    pub mode_byte: u8,
    pub intensity: u8,
}

/// Schedule RF retransmission events for a single preset step.
///
/// Transmits at step start, then schedules later retransmits as late as possible
/// while keeping no more than RETRANSMIT_INTERVAL_MS between command starts and
/// ensuring every transmission completes at or before step end.
pub fn schedule_step_events(
    events: &mut Vec<PresetEvent>,
    start_ms: u64,
    end_ms: u64,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
) -> Result<()> {
    if start_ms >= end_ms {
        return Ok(());
    }

    let step_duration_ms = end_ms - start_ms;
    if step_duration_ms < RF_COMMAND_TRANSMIT_DURATION_MS {
        return Err(anyhow!(
            "step duration {}ms is shorter than RF transmit time {}ms",
            step_duration_ms,
            RF_COMMAND_TRANSMIT_DURATION_MS
        ));
    }

    let mut t = start_ms;
    loop {
        events.push(PresetEvent {
            time_ms: t,
            collar_id,
            channel,
            mode_byte,
            intensity,
        });

        if end_ms - t <= RETRANSMIT_INTERVAL_MS {
            return Ok(());
        }

        let next_earliest = t + RF_COMMAND_TRANSMIT_DURATION_MS;
        let next_latest =
            (t + RETRANSMIT_INTERVAL_MS).min(end_ms - RF_COMMAND_TRANSMIT_DURATION_MS);
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
    track_index: usize,
    step_index: usize,
}

pub fn schedule_preset_events(preset: &Preset, collars: &[Collar]) -> Result<Vec<PresetEvent>> {
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

        let mut time_ms = 0u64;
        for (step_index, step) in track.steps.iter().enumerate() {
            let end_ms = time_ms + u64::from(step.duration_ms);
            if let Some(mode) = step.mode.to_command_mode() {
                let mut step_events = Vec::new();
                schedule_step_events(
                    &mut step_events,
                    time_ms,
                    end_ms,
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
                    event,
                    track_index,
                    step_index,
                }));
            }
            time_ms = end_ms;
        }
    }

    events.sort_by_key(|scheduled| scheduled.event.time_ms);

    let mut previous_end_ms = None;
    let mut previous_track_index = 0usize;
    let mut previous_step_index = 0usize;
    for scheduled in &events {
        if let Some(previous_end_ms) = previous_end_ms {
            if scheduled.event.time_ms < previous_end_ms {
                return Err(anyhow!(
                    "Preset '{}' cannot overlap RF transmissions on a single transmitter: track {} step {} starts at {}ms before track {} step {} ends at {}ms",
                    preset.name,
                    scheduled.track_index,
                    scheduled.step_index,
                    scheduled.event.time_ms,
                    previous_track_index,
                    previous_step_index,
                    previous_end_ms
                ));
            }
        }
        previous_end_ms = Some(scheduled.event.time_ms + RF_COMMAND_TRANSMIT_DURATION_MS);
        previous_track_index = scheduled.track_index;
        previous_step_index = scheduled.step_index;
    }

    Ok(events
        .into_iter()
        .map(|scheduled| scheduled.event)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(events: &[PresetEvent]) -> Vec<u64> {
        events.iter().map(|e| e.time_ms).collect()
    }

    #[test]
    fn very_short_step_single_event() {
        let mut events = Vec::new();
        let err = schedule_step_events(&mut events, 0, 100, 0x1234, 0, 2, 50).unwrap_err();
        assert!(err.to_string().contains("shorter than RF transmit time"));
        assert!(events.is_empty());
    }

    #[test]
    fn step_130ms_single_event() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 130, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0]);
    }

    #[test]
    fn step_201ms_unschedulable() {
        let mut events = Vec::new();
        let err = schedule_step_events(&mut events, 0, 201, 0x1234, 0, 2, 50).unwrap_err();
        assert!(err
            .to_string()
            .contains("cannot be sustained without overlapping transmissions"));
    }

    #[test]
    fn step_329ms_two_events() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 329, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 199]);
    }

    #[test]
    fn step_500ms_three_events() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 500, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 200, 370]);
    }

    #[test]
    fn one_second_step() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 1000, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 200, 400, 600, 800]);
    }

    #[test]
    fn two_second_step() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 2000, 0x1234, 0, 2, 50).unwrap();
        let t = times(&events);
        assert_eq!(t.len(), 10);
        assert_eq!(t[0], 0);
        assert_eq!(t[9], 1800);
    }

    #[test]
    fn step_with_offset_start() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 500, 1500, 0x1234, 0, 1, 30).unwrap();
        assert_eq!(times(&events), vec![500, 700, 900, 1100, 1300]);
    }

    #[test]
    fn inverted_range_produces_nothing() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 1000, 500, 0x1234, 0, 2, 50).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn zero_duration_produces_nothing() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 1000, 1000, 0x1234, 0, 2, 50).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn event_fields_correct() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 100, 250, 0xABCD, 2, 3, 77).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time_ms, 100);
        assert_eq!(events[0].collar_id, 0xABCD);
        assert_eq!(events[0].channel, 2);
        assert_eq!(events[0].mode_byte, 3);
        assert_eq!(events[0].intensity, 77);
    }

    #[test]
    fn no_event_past_end() {
        let mut events = Vec::new();
        schedule_step_events(&mut events, 0, 400, 0x1234, 0, 2, 50).unwrap();
        assert_eq!(times(&events), vec![0, 200]);
    }

    #[test]
    fn preset_schedule_rejects_overlapping_tracks() {
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

        let err = schedule_preset_events(&preset, &collars).unwrap_err();
        assert!(err.to_string().contains("cannot overlap RF transmissions"));
    }
}
