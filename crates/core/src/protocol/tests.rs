use super::*;

#[test]
fn command_mode_round_trip() {
    for (mode, byte) in [
        (CommandMode::Shock, 1),
        (CommandMode::Vibrate, 2),
        (CommandMode::Beep, 3),
    ] {
        assert_eq!(mode.to_rf_byte(), byte);
        assert_eq!(CommandMode::from_rf_byte(byte), Some(mode));
    }
}

#[test]
fn command_mode_from_invalid_byte() {
    assert_eq!(CommandMode::from_rf_byte(0), None);
    assert_eq!(CommandMode::from_rf_byte(4), None);
    assert_eq!(CommandMode::from_rf_byte(255), None);
}

#[test]
fn preset_step_mode_to_command() {
    assert_eq!(
        PresetStepMode::Shock.to_command_mode(),
        Some(CommandMode::Shock)
    );
    assert_eq!(
        PresetStepMode::Vibrate.to_command_mode(),
        Some(CommandMode::Vibrate)
    );
    assert_eq!(
        PresetStepMode::Beep.to_command_mode(),
        Some(CommandMode::Beep)
    );
    assert_eq!(PresetStepMode::Pause.to_command_mode(), None);
}

#[test]
fn has_intensity() {
    assert!(PresetStepMode::Shock.has_intensity());
    assert!(PresetStepMode::Vibrate.has_intensity());
    assert!(!PresetStepMode::Beep.has_intensity());
    assert!(!PresetStepMode::Pause.has_intensity());
}

#[test]
fn command_mode_has_intensity() {
    assert!(CommandMode::Shock.has_intensity());
    assert!(CommandMode::Vibrate.has_intensity());
    assert!(!CommandMode::Beep.has_intensity());
}

#[test]
fn device_settings_defaults_include_remote_control_and_event_log() {
    let settings = DeviceSettings::default();
    assert_eq!(settings.device_id, "");
    assert!(!settings.remote_control_enabled);
    assert_eq!(settings.remote_control_url, "");
    assert!(settings.remote_control_validate_cert);
    assert!(!settings.record_event_log);
}

#[test]
fn preset_normalize_zeros_beep_pause_intensity() {
    let mut preset = Preset {
        name: "test".to_string(),
        tracks: vec![PresetTrack {
            collar_name: "Rex".to_string(),
            steps: vec![
                PresetStep {
                    mode: PresetStepMode::Shock,
                    intensity: 50,
                    duration_ms: 1000,
                    intensity_max: None,
                    duration_max_ms: None,
                    intensity_distribution: None,
                    duration_distribution: None,
                },
                PresetStep {
                    mode: PresetStepMode::Vibrate,
                    intensity: 30,
                    duration_ms: 500,
                    intensity_max: Some(60),
                    duration_max_ms: None,
                    intensity_distribution: None,
                    duration_distribution: None,
                },
                PresetStep {
                    mode: PresetStepMode::Beep,
                    intensity: 99,
                    duration_ms: 200,
                    intensity_max: Some(99),
                    duration_max_ms: None,
                    intensity_distribution: None,
                    duration_distribution: None,
                },
                PresetStep {
                    mode: PresetStepMode::Pause,
                    intensity: 42,
                    duration_ms: 300,
                    intensity_max: Some(50),
                    duration_max_ms: None,
                    intensity_distribution: None,
                    duration_distribution: None,
                },
            ],
        }],
    };
    preset.normalize();
    assert_eq!(preset.tracks[0].steps[0].intensity, 50);
    assert_eq!(preset.tracks[0].steps[0].intensity_max, None);
    assert_eq!(preset.tracks[0].steps[1].intensity, 30);
    assert_eq!(preset.tracks[0].steps[1].intensity_max, Some(60));
    assert_eq!(preset.tracks[0].steps[2].intensity, 0);
    assert_eq!(preset.tracks[0].steps[2].intensity_max, None);
    assert_eq!(preset.tracks[0].steps[3].intensity, 0);
    assert_eq!(preset.tracks[0].steps[3].intensity_max, None);
}

#[test]
fn preset_step_midpoint_fixed() {
    let step = PresetStep {
        mode: PresetStepMode::Shock,
        intensity: 50,
        duration_ms: 2000,
        intensity_max: None,
        duration_max_ms: None,
        intensity_distribution: None,
        duration_distribution: None,
    };
    assert_eq!(step.midpoint_intensity(), 50);
    assert_eq!(step.midpoint_duration(), 2000);
}

#[test]
fn preset_step_midpoint_random() {
    let step = PresetStep {
        mode: PresetStepMode::Vibrate,
        intensity: 20,
        duration_ms: 1000,
        intensity_max: Some(80),
        duration_max_ms: Some(5000),
        intensity_distribution: None,
        duration_distribution: None,
    };
    assert_eq!(step.midpoint_intensity(), 50);
    assert_eq!(step.midpoint_duration(), 3000);
}

#[test]
fn run_action_with_random_fields_deserializes() {
    let json = r#"{"type":"run_action","collar_name":"Rex","mode":"shock","intensity":10,"duration_ms":1000,"intensity_max":50,"duration_max_ms":3000}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::RunAction {
            intensity,
            duration_ms,
            intensity_max,
            duration_max_ms,
            ..
        } => {
            assert_eq!(intensity, 10);
            assert_eq!(duration_ms, 1000);
            assert_eq!(intensity_max, Some(50));
            assert_eq!(duration_max_ms, Some(3000));
        }
        other => panic!("Expected RunAction, got {:?}", other),
    }
}

#[test]
fn run_action_without_random_fields_deserializes() {
    let json = r#"{"type":"run_action","collar_name":"Rex","mode":"shock","intensity":25,"duration_ms":1500}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::RunAction {
            intensity_max,
            duration_max_ms,
            ..
        } => {
            assert_eq!(intensity_max, None);
            assert_eq!(duration_max_ms, None);
        }
        other => panic!("Expected RunAction, got {:?}", other),
    }
}

#[test]
fn encode_basic_frame() {
    let frame = encode_rf_frame(0x1234, 0, 1, 50);
    assert_eq!(frame[0], 0x12);
    assert_eq!(frame[1], 0x34);
    assert_eq!(frame[2], 0x01);
    assert_eq!(frame[3], 50);
    assert_eq!(
        frame[4],
        0x12u8
            .wrapping_add(0x34)
            .wrapping_add(0x01)
            .wrapping_add(50)
    );
}

#[test]
fn encode_with_channel() {
    let frame = encode_rf_frame(0xABCD, 2, 3, 99);
    assert_eq!(frame[2], (2 << 4) | 3);
}

#[test]
#[should_panic(expected = "intensity")]
fn encode_rejects_excess_intensity() {
    encode_rf_frame(0x0000, 0, 1, 255);
}

#[test]
fn encode_decode_round_trip() {
    let frame = encode_rf_frame(0x9B7A, 1, 2, 75);
    let (id, ch, mode, intensity, checksum_ok) = decode_rf_frame(&frame);
    assert_eq!(id, 0x9B7A);
    assert_eq!(ch, 1);
    assert_eq!(mode, 2);
    assert_eq!(intensity, 75);
    assert!(checksum_ok);
}

#[test]
fn decode_bad_checksum() {
    let mut frame = encode_rf_frame(0x1234, 0, 1, 50);
    frame[4] = frame[4].wrapping_add(1);
    let (_, _, _, _, checksum_ok) = decode_rf_frame(&frame);
    assert!(!checksum_ok);
}

#[test]
fn collar_json_round_trip() {
    let collar = Collar {
        name: "Rex".to_string(),
        collar_id: 0x1234,
        channel: 1,
    };
    let json = serde_json::to_string(&collar).unwrap();
    let decoded: Collar = serde_json::from_str(&json).unwrap();
    assert_eq!(collar, decoded);
}

#[test]
fn command_mode_serializes_snake_case() {
    let json = serde_json::to_string(&CommandMode::Shock).unwrap();
    assert_eq!(json, "\"shock\"");
    let json = serde_json::to_string(&CommandMode::Vibrate).unwrap();
    assert_eq!(json, "\"vibrate\"");
}

#[test]
fn preset_step_mode_serializes_pause() {
    let json = serde_json::to_string(&PresetStepMode::Pause).unwrap();
    assert_eq!(json, "\"pause\"");
}

#[test]
fn client_message_add_collar_deserialization() {
    let json = r#"{"type":"add_collar","name":"Rex","collar_id":4660,"channel":0}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::AddCollar {
            name,
            collar_id,
            channel,
        } => {
            assert_eq!(name, "Rex");
            assert_eq!(collar_id, 0x1234);
            assert_eq!(channel, 0);
        }
        other => panic!("Expected AddCollar, got {:?}", other),
    }
}

#[test]
fn client_message_command_deserialization() {
    let json = r#"{"type":"command","collar_name":"Rex","mode":"vibrate","intensity":50}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Command {
            collar_name,
            mode,
            intensity,
        } => {
            assert_eq!(collar_name, "Rex");
            assert_eq!(mode, CommandMode::Vibrate);
            assert_eq!(intensity, 50);
        }
        other => panic!("Expected Command, got {:?}", other),
    }
}

#[test]
fn client_message_run_action_deserialization() {
    let json = r#"{"type":"run_action","collar_name":"Rex","mode":"shock","intensity":25,"duration_ms":1500}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::RunAction {
            collar_name,
            mode,
            intensity,
            duration_ms,
            ..
        } => {
            assert_eq!(collar_name, "Rex");
            assert_eq!(mode, CommandMode::Shock);
            assert_eq!(intensity, 25);
            assert_eq!(duration_ms, 1500);
        }
        other => panic!("Expected RunAction, got {:?}", other),
    }
}

#[test]
fn export_data_round_trip() {
    let data = ExportData {
        collars: vec![Collar {
            name: "Rex".to_string(),
            collar_id: 0xABCD,
            channel: 2,
        }],
        presets: vec![Preset {
            name: "test".to_string(),
            tracks: vec![PresetTrack {
                collar_name: "Rex".to_string(),
                steps: vec![PresetStep {
                    mode: PresetStepMode::Vibrate,
                    intensity: 30,
                    duration_ms: 1500,
                    intensity_max: None,
                    duration_max_ms: None,
                    intensity_distribution: None,
                    duration_distribution: None,
                }],
            }],
        }],
    };
    let json = serde_json::to_string(&data).unwrap();
    let decoded: ExportData = serde_json::from_str(&json).unwrap();
    assert_eq!(data.collars.len(), decoded.collars.len());
    assert_eq!(data.collars[0], decoded.collars[0]);
    assert_eq!(data.presets[0].name, decoded.presets[0].name);
}
