use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use async_broadcast::{InactiveReceiver, Sender as BroadcastSender};

use crate::led::Led;
use crate::protocol::{
    Collar, CommandMode, DeviceSettings, EventLogEntry, EventLogEntryKind, EventSource, ExportData,
    Preset, RemoteControlStatus, RfDebugFrame,
};
use crate::rf::{RfReceiver, RfTransmitter};

#[derive(Clone)]
pub struct BroadcastMsg {
    pub json: Arc<str>,
    pub rf_debug: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageOrigin {
    LocalUi,
    RemoteControl,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ActionKey {
    pub collar_name: String,
    pub mode: CommandMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ActionOwner {
    LocalWs(u32),
    RemoteControl,
}

pub(crate) struct ActiveActionHandle {
    pub owner: Option<ActionOwner>,
    pub cancel_on_disconnect: bool,
    pub collar_id: u16,
    pub channel: u8,
    pub mode_byte: u8,
    pub intensity: u8,
    pub deadline: Option<Instant>,
    pub started_at: Instant,
    pub source: EventSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteControlUrlKind {
    Ws,
    Wss,
}

pub(crate) enum TransmissionCommand {
    UpsertAction {
        key: ActionKey,
        handle: ActiveActionHandle,
    },
    CancelAction {
        key: ActionKey,
    },
    CancelOwnedActions {
        owner: ActionOwner,
    },
    CancelAllActions,
    StartPreset {
        preset_name: String,
        events: Vec<crate::scheduling::PresetEvent>,
    },
    StopPreset,
    StopAll,
}

pub(crate) enum AppCommand {
    AddCollar {
        collar: Collar,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    UpdateCollar {
        original_name: String,
        updated: Collar,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    DeleteCollar {
        name: String,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    SavePreset {
        original_name: Option<String>,
        preset: Preset,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    DeletePreset {
        name: String,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    ReorderPresets {
        names: Vec<String>,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    ImportData {
        data: ExportData,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    SaveDeviceSettings {
        settings: DeviceSettings,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    StartPresetExecution {
        preset_name: String,
        source: EventSource,
        resolved_preset: Option<Preset>,
        events: Vec<crate::scheduling::PresetEvent>,
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    StopPreset {
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    StopAll {
        reply: mpsc::SyncSender<super::ControlResult>,
    },
    SetRemoteControlStatus {
        status: RemoteControlStatus,
    },
    RecordEvent {
        source: EventSource,
        kind: EventLogEntryKind,
    },
    PushRfDebugEvent {
        event: RfDebugFrame,
    },
    ClearRfDebugEvents {
        listening: bool,
        reply: mpsc::SyncSender<Arc<str>>,
    },
    CompletePreset {
        preset_name: String,
    },
}

pub struct DomainState {
    pub device_settings: DeviceSettings,
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
    pub preset_name: Option<String>,
    pub rf_lockout_until_ms: u64,
    pub rf_debug_events: VecDeque<RfDebugFrame>,
    pub event_log_events: Vec<EventLogEntry>,
    pub remote_control_status: RemoteControlStatus,
}

#[derive(Clone)]
pub struct HardwareCtx {
    pub rf: Arc<Mutex<RfTransmitter>>,
    pub tx_led: Arc<Mutex<Led>>,
    pub rx_led: Arc<Mutex<Led>>,
    pub rf_receiver: Arc<Mutex<Option<RfReceiver>>>,
}

#[derive(Clone)]
pub struct SessionCtx {
    pub broadcast_tx: BroadcastSender<BroadcastMsg>,
    /// Keeps the broadcast channel alive even when no active receivers exist.
    pub(crate) _broadcast_keepalive: InactiveReceiver<BroadcastMsg>,
    pub ws_clients: Arc<Mutex<Vec<(u32, String)>>>,
    pub remote_control_settings_revision: Arc<AtomicU32>,
}

#[derive(Clone)]
pub struct WorkerCtx {
    pub app_tx: mpsc::Sender<AppCommand>,
    pub app_rx: Arc<Mutex<Option<mpsc::Receiver<AppCommand>>>>,
    pub transmission_tx: mpsc::Sender<TransmissionCommand>,
    pub transmission_rx: Arc<Mutex<Option<mpsc::Receiver<TransmissionCommand>>>>,
    pub rng: Arc<Mutex<rand::rngs::SmallRng>>,
    pub event_log_sequence: Arc<AtomicU32>,
}

#[derive(Clone)]
pub struct DebugCtx {
    pub rf_debug_enabled: Arc<AtomicBool>,
    pub rf_debug_listener_count: Arc<AtomicU32>,
    pub rf_debug_worker_spawned: Arc<AtomicBool>,
}
