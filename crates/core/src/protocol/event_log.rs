use serde::{Deserialize, Serialize};

use super::{CommandMode, Preset};

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
