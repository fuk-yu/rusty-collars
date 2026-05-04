use serde::{Deserialize, Serialize};

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
            Some(max) if max > self.intensity => ((self.intensity as u16 + max as u16) / 2) as u8,
            _ => self.intensity,
        }
    }

    pub fn has_random(&self) -> bool {
        self.intensity_max.is_some_and(|max| max > self.intensity)
            || self
                .duration_max_ms
                .is_some_and(|max| max > self.duration_ms)
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

    pub fn has_intensity(self) -> bool {
        self.to_command_mode()
            .is_some_and(|mode| mode.has_intensity())
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MqttStatus {
    pub enabled: bool,
    pub connected: bool,
    pub server: String,
    pub status_text: String,
}

impl Default for MqttStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            connected: false,
            server: String::new(),
            status_text: "Off".to_string(),
        }
    }
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
