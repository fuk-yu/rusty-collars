use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::{
    ApStatus, ButtonAction, CommandMode, DeviceSettings, Distribution, EventLogEntry, ExportData,
    InterfaceStatus, MemoryRegion, Preset, PresetPreview, RemoteControlStatus, RfDebugFrame,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
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
        collars: &'a [super::Collar],
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
