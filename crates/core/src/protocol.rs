use serde::{Deserialize, Serialize};

pub const MAX_INTENSITY: u8 = 99;
pub const MAX_CHANNEL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    // GPIO
    #[serde(alias = "led_pin")]
    pub tx_led_pin: u8,
    #[serde(default = "default_rx_led_pin")]
    pub rx_led_pin: u8,
    pub rf_tx_pin: u8,
    pub rf_rx_pin: u8,
    // WiFi STA (client)
    #[serde(default)]
    pub wifi_ssid: String,
    #[serde(default)]
    pub wifi_password: String,
    // WiFi AP
    #[serde(default = "default_true")]
    pub ap_enabled: bool,
    #[serde(default = "default_ap_password")]
    pub ap_password: String,
    // Server
    #[serde(default = "default_max_clients")]
    pub max_clients: u8,
    // Time sync
    #[serde(default = "default_true")]
    pub ntp_enabled: bool,
    #[serde(default = "default_ntp_server")]
    pub ntp_server: String,
}

fn default_true() -> bool {
    true
}
fn default_ap_password() -> String {
    "rfcollars".to_string()
}
fn default_max_clients() -> u8 {
    8
}
fn default_ntp_server() -> String {
    "pool.ntp.org".to_string()
}
fn default_rx_led_pin() -> u8 {
    DeviceSettings::default_pins().1
}

impl DeviceSettings {
    /// Returns (tx_led_pin, rx_led_pin, rf_tx_pin, rf_rx_pin)
    pub fn default_pins() -> (u8, u8, u8, u8) {
        #[cfg(esp32c6)]
        {
            (8, 8, 10, 11)
        } // C6: single LED on 8 for both
        #[cfg(esp32p4)]
        {
            (14, 17, 15, 18)
        } // P4-ETH: TX LED=GPIO17, RX LED=GPIO18, RF TX=GPIO6, RF RX=GPIO5
        #[cfg(not(any(esp32c6, esp32p4)))]
        {
            (2, 2, 16, 15)
        } // ESP32: single LED on 2 for both
    }
}

impl Default for DeviceSettings {
    fn default() -> Self {
        let (tx_led_pin, rx_led_pin, rf_tx_pin, rf_rx_pin) = Self::default_pins();
        Self {
            tx_led_pin,
            rx_led_pin,
            rf_tx_pin,
            rf_rx_pin,
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            ap_enabled: true,
            ap_password: "rfcollars".to_string(),
            max_clients: 8,
            ntp_enabled: true,
            ntp_server: default_ntp_server(),
        }
    }
}

/// AP SSID is always fixed.
pub const AP_SSID: &str = "rfcollars";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandMode {
    Shock,
    Vibrate,
    Beep,
}

impl CommandMode {
    pub fn to_rf_byte(self) -> u8 {
        match self {
            Self::Shock => 1,
            Self::Vibrate => 2,
            Self::Beep => 3,
        }
    }

    pub fn from_rf_byte(mode: u8) -> Option<Self> {
        match mode {
            1 => Some(Self::Shock),
            2 => Some(Self::Vibrate),
            3 => Some(Self::Beep),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonAction {
    Press,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collar {
    pub name: String,
    pub collar_id: u16,
    pub channel: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub tracks: Vec<PresetTrack>,
}

impl Preset {
    /// Zero out intensity for beep/pause steps (intensity is meaningless for these modes).
    pub fn normalize(&mut self) {
        for track in &mut self.tracks {
            for step in &mut track.steps {
                if !step.mode.has_intensity() {
                    step.intensity = 0;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetTrack {
    pub collar_name: String,
    pub steps: Vec<PresetStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetStep {
    pub mode: PresetStepMode,
    pub intensity: u8,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetStepMode {
    Shock,
    Vibrate,
    Beep,
    Pause,
}

impl PresetStepMode {
    pub fn to_command_mode(self) -> Option<CommandMode> {
        match self {
            Self::Shock => Some(CommandMode::Shock),
            Self::Vibrate => Some(CommandMode::Vibrate),
            Self::Beep => Some(CommandMode::Beep),
            Self::Pause => None,
        }
    }

    /// Whether this mode uses the intensity field.
    pub fn has_intensity(self) -> bool {
        matches!(self, Self::Shock | Self::Vibrate)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Single-shot RF command (frontend repeats while button held)
    Command {
        collar_name: String,
        mode: CommandMode,
        intensity: u8,
    },
    ButtonEvent {
        collar_name: String,
        mode: CommandMode,
        intensity: u8,
        action: ButtonAction,
    },
    AddCollar {
        name: String,
        collar_id: u16,
        channel: u8,
    },
    UpdateCollar {
        original_name: String,
        name: String,
        collar_id: u16,
        channel: u8,
    },
    DeleteCollar {
        name: String,
    },
    SavePreset {
        original_name: Option<String>,
        preset: Preset,
    },
    Ping {
        nonce: u32,
    },
    DeletePreset {
        name: String,
    },
    RunPreset {
        name: String,
    },
    StopPreset,
    StopAll,
    StartRfDebug,
    StopRfDebug,
    ClearRfDebug,
    Reboot,
    GetDeviceSettings,
    SaveDeviceSettings {
        settings: DeviceSettings,
    },
    PreviewPreset {
        nonce: u32,
        preset: Preset,
    },
    ReorderPresets {
        names: Vec<String>,
    },
    Export,
    Import {
        data: ExportData,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage<'a> {
    State {
        app_version: &'a str,
        server_uptime_s: u64,
        collars: &'a [Collar],
        presets: &'a [Preset],
        preset_running: Option<&'a str>,
        rf_lockout_remaining_ms: u64,
    },
    ExportData {
        data: &'a ExportData,
    },
    RfDebugState {
        listening: bool,
        events: &'a [RfDebugFrame],
    },
    RfDebugEvent {
        event: &'a RfDebugFrame,
    },
    Pong {
        nonce: u32,
        server_uptime_s: u64,
        free_heap_bytes: u32,
        connected_clients: u32,
        client_ips: Vec<String>,
    },
    DeviceSettings {
        settings: DeviceSettings,
        reboot_required: bool,
    },
    PresetPreview {
        nonce: u32,
        preview: Option<PresetPreview>,
        error: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportData {
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetPreview {
    pub total_duration_ms: u64,
    pub events: Vec<PresetPreviewEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetPreviewEvent {
    pub requested_time_ms: u64,
    pub actual_time_ms: u64,
    pub track_index: usize,
    pub step_index: usize,
    pub collar_name: String,
    pub collar_id: u16,
    pub channel: u8,
    pub mode: CommandMode,
    pub mode_byte: u8,
    pub intensity: u8,
    pub raw_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfDebugFrame {
    pub received_at_ms: u64,
    pub raw_hex: String,
    pub collar_id: u16,
    pub channel: u8,
    pub mode_raw: u8,
    pub mode: Option<CommandMode>,
    pub intensity: u8,
    pub checksum_ok: bool,
}

/// Encode a Type-1 RF frame (pure math, no hardware dependency).
/// Returns 5 bytes: [id_hi, id_lo, channel<<4|mode, intensity, checksum].
pub fn encode_rf_frame(collar_id: u16, channel: u8, mode: u8, intensity: u8) -> [u8; 5] {
    assert!(
        intensity <= MAX_INTENSITY,
        "intensity {intensity} exceeds MAX_INTENSITY {MAX_INTENSITY}"
    );
    let b0 = (collar_id >> 8) as u8;
    let b1 = (collar_id & 0xFF) as u8;
    let b2 = (channel << 4) | (mode & 0x0F);
    let b3 = intensity;
    let b4 = b0.wrapping_add(b1).wrapping_add(b2).wrapping_add(b3);
    [b0, b1, b2, b3, b4]
}

/// Decode a 5-byte Type-1 RF frame. Returns (collar_id, channel, mode_raw, intensity, checksum_ok).
pub fn decode_rf_frame(raw: &[u8; 5]) -> (u16, u8, u8, u8, bool) {
    let collar_id = u16::from(raw[0]) << 8 | u16::from(raw[1]);
    let channel = raw[2] >> 4;
    let mode_raw = raw[2] & 0x0F;
    let intensity = raw[3];
    let checksum_ok = raw[4]
        == raw[0]
            .wrapping_add(raw[1])
            .wrapping_add(raw[2])
            .wrapping_add(raw[3]);
    (collar_id, channel, mode_raw, intensity, checksum_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- CommandMode ---

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

    // --- PresetStepMode ---

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
                    },
                    PresetStep {
                        mode: PresetStepMode::Vibrate,
                        intensity: 30,
                        duration_ms: 500,
                    },
                    PresetStep {
                        mode: PresetStepMode::Beep,
                        intensity: 99,
                        duration_ms: 200,
                    },
                    PresetStep {
                        mode: PresetStepMode::Pause,
                        intensity: 42,
                        duration_ms: 300,
                    },
                ],
            }],
        };
        preset.normalize();
        assert_eq!(preset.tracks[0].steps[0].intensity, 50); // shock: unchanged
        assert_eq!(preset.tracks[0].steps[1].intensity, 30); // vibrate: unchanged
        assert_eq!(preset.tracks[0].steps[2].intensity, 0); // beep: zeroed
        assert_eq!(preset.tracks[0].steps[3].intensity, 0); // pause: zeroed
    }

    // --- RF frame encoding ---

    #[test]
    fn encode_basic_frame() {
        let frame = encode_rf_frame(0x1234, 0, 1, 50);
        assert_eq!(frame[0], 0x12); // id hi
        assert_eq!(frame[1], 0x34); // id lo
        assert_eq!(frame[2], 0x01); // ch0 | shock
        assert_eq!(frame[3], 50); // intensity
                                  // checksum: 0x12 + 0x34 + 0x01 + 0x32 = 0x79
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
        assert_eq!(frame[2], (2 << 4) | 3); // ch2 | beep
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
        frame[4] = frame[4].wrapping_add(1); // corrupt checksum
        let (_, _, _, _, checksum_ok) = decode_rf_frame(&frame);
        assert!(!checksum_ok);
    }

    // --- Serde ---

    #[test]
    fn collar_json_round_trip() {
        let c = Collar {
            name: "Rex".to_string(),
            collar_id: 0x1234,
            channel: 1,
        };
        let json = serde_json::to_string(&c).unwrap();
        let c2: Collar = serde_json::from_str(&json).unwrap();
        assert_eq!(c, c2);
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
                    }],
                }],
            }],
        };
        let json = serde_json::to_string(&data).unwrap();
        let data2: ExportData = serde_json::from_str(&json).unwrap();
        assert_eq!(data.collars.len(), data2.collars.len());
        assert_eq!(data.collars[0], data2.collars[0]);
        assert_eq!(data.presets[0].name, data2.presets[0].name);
    }
}
