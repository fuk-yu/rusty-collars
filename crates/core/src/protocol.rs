use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

pub const MAX_INTENSITY: u8 = 99;
pub const MAX_CHANNEL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    // Device identity
    #[serde(default)]
    pub device_id: String,
    // GPIO
    #[serde(alias = "led_pin")]
    pub tx_led_pin: u8,
    #[serde(default = "default_rx_led_pin")]
    pub rx_led_pin: u8,
    pub rf_tx_pin: u8,
    pub rf_rx_pin: u8,
    // WiFi STA (client)
    #[serde(default = "default_true")]
    pub wifi_client_enabled: bool,
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
    // Reverse remote control
    #[serde(default)]
    pub remote_control_enabled: bool,
    #[serde(default)]
    pub remote_control_url: String,
    #[serde(default = "default_true")]
    pub remote_control_validate_cert: bool,
    // Diagnostics
    #[serde(default)]
    pub record_event_log: bool,
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
            (7, 8, 5, 6)
        } // P4: avoid GPIO14-19 (used by SDIO on P4-WiFi boards)
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
            device_id: String::new(),
            tx_led_pin,
            rx_led_pin,
            rf_tx_pin,
            rf_rx_pin,
            wifi_client_enabled: true,
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            ap_enabled: true,
            ap_password: "rfcollars".to_string(),
            max_clients: 8,
            ntp_enabled: true,
            ntp_server: default_ntp_server(),
            remote_control_enabled: false,
            remote_control_url: String::new(),
            remote_control_validate_cert: true,
            record_event_log: false,
        }
    }
}

/// AP SSID is always fixed.
pub const AP_SSID: &str = "rfcollars";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    pub fn has_intensity(self) -> bool {
        !matches!(self, Self::Beep)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonAction {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distribution {
    Uniform,
    Gaussian,
}

impl Default for Distribution {
    fn default() -> Self {
        Self::Uniform
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collar {
    pub name: String,
    pub collar_id: u16,
    pub channel: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
                    step.intensity_max = None;
                    step.intensity_distribution = None;
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetTrack {
    pub collar_name: String,
    pub steps: Vec<PresetStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetStep {
    pub mode: PresetStepMode,
    pub intensity: u8,
    pub duration_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity_max: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_max_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity_distribution: Option<Distribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_distribution: Option<Distribution>,
}

impl PresetStep {
    pub fn midpoint_duration(&self) -> u32 {
        match self.duration_max_ms {
            Some(max) if max > self.duration_ms => (self.duration_ms + max) / 2,
            _ => self.duration_ms,
        }
    }

    pub fn midpoint_intensity(&self) -> u8 {
        match self.intensity_max {
            Some(max) if max > self.intensity => {
                ((self.intensity as u16 + max as u16) / 2) as u8
            }
            _ => self.intensity,
        }
    }

    /// Returns true if this step has any random ranges.
    pub fn has_random(&self) -> bool {
        self.intensity_max.map_or(false, |m| m > self.intensity)
            || self.duration_max_ms.map_or(false, |m| m > self.duration_ms)
    }
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
    RunAction {
        collar_name: String,
        mode: CommandMode,
        intensity: u8,
        duration_ms: u32,
        #[serde(default)]
        intensity_max: Option<u8>,
        #[serde(default)]
        duration_max_ms: Option<u32>,
        #[serde(default)]
        intensity_distribution: Option<Distribution>,
        #[serde(default)]
        duration_distribution: Option<Distribution>,
    },
    StartAction {
        collar_name: String,
        mode: CommandMode,
        intensity: u8,
        #[serde(default)]
        intensity_max: Option<u8>,
        #[serde(default)]
        intensity_distribution: Option<Distribution>,
    },
    StopAction {
        collar_name: String,
        mode: CommandMode,
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
    GetNetworkStatus,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage<'a> {
    State {
        device_id: &'a str,
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
        events: &'a VecDeque<RfDebugFrame>,
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
        has_wifi: bool,
    },
    PresetPreview {
        nonce: u32,
        preview: Option<PresetPreview>,
        error: Option<String>,
    },
    RemoteControlStatus {
        status: RemoteControlStatus,
    },
    EventLogState {
        enabled: bool,
        events: &'a [EventLogEntry],
    },
    EventLogEvent {
        event: &'a EventLogEntry,
    },
    NetworkStatus {
        board_mac: String,
        memory: Vec<MemoryRegion>,
        min_free_heap_bytes: u32,
        ethernet: InterfaceStatus,
        wifi_sta: InterfaceStatus,
        wifi_ap: ApStatus,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegion {
    pub name: String,
    pub total_bytes: u32,
    pub free_bytes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceStatus {
    pub available: bool,
    pub enabled: bool,
    pub mac: String,
    pub ip: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApStatus {
    pub available: bool,
    pub enabled: bool,
    pub mac: String,
    pub ip: String,
    pub clients: Vec<ApClientInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApClientInfo {
    pub mac: String,
    pub ip: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteControlStatus {
    pub enabled: bool,
    pub connected: bool,
    pub url: String,
    pub validate_cert: bool,
    pub rtt_ms: Option<u32>,
    pub status_text: String,
}

impl Default for RemoteControlStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            connected: false,
            url: String::new(),
            validate_cert: true,
            rtt_ms: None,
            status_text: "Off".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    LocalUi,
    RemoteControl,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventLogEntry {
    pub sequence: u64,
    pub monotonic_ms: u64,
    pub unix_ms: Option<u64>,
    pub source: EventSource,
    #[serde(flatten)]
    pub kind: EventLogEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventLogEntryKind {
    Action {
        collar_name: String,
        mode: CommandMode,
        intensity: Option<u8>,
        duration_ms: u32,
    },
    PresetRun {
        preset_name: String,
        /// When the preset contains random steps, this holds a copy with the
        /// concrete values that were selected for this particular run.
        #[serde(skip_serializing_if = "Option::is_none")]
        resolved_preset: Option<Preset>,
    },
    NtpSync {
        server: String,
    },
    RemoteControlConnection {
        connected: bool,
        url: String,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportData {
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetPreview {
    pub total_duration_us: u64,
    pub events: Vec<PresetPreviewEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetPreviewEvent {
    pub requested_time_us: u64,
    pub actual_time_us: u64,
    pub track_index: usize,
    pub step_index: usize,
    pub transmit_duration_us: u64,
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
        assert_eq!(preset.tracks[0].steps[0].intensity, 50); // shock: unchanged
        assert_eq!(preset.tracks[0].steps[0].intensity_max, None);
        assert_eq!(preset.tracks[0].steps[1].intensity, 30); // vibrate: unchanged
        assert_eq!(preset.tracks[0].steps[1].intensity_max, Some(60)); // kept
        assert_eq!(preset.tracks[0].steps[2].intensity, 0); // beep: zeroed
        assert_eq!(preset.tracks[0].steps[2].intensity_max, None); // cleared
        assert_eq!(preset.tracks[0].steps[3].intensity, 0); // pause: zeroed
        assert_eq!(preset.tracks[0].steps[3].intensity_max, None); // cleared
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
        let data2: ExportData = serde_json::from_str(&json).unwrap();
        assert_eq!(data.collars.len(), data2.collars.len());
        assert_eq!(data.collars[0], data2.collars[0]);
        assert_eq!(data.presets[0].name, data2.presets[0].name);
    }
}
