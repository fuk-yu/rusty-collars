use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{mpsc, Arc, Mutex};

use async_broadcast::{InactiveReceiver, Sender as BroadcastSender};
use rand::rngs::SmallRng;
use rand::SeedableRng;

use crate::led::Led;
use crate::protocol::{
    Collar, DeviceSettings, EventLogEntryKind, EventSource, ExportData, Preset,
    RemoteControlStatus, ServerMessage,
};
use crate::repository::SharedRepository;
use crate::rf::{RfReceiver, RfTransmitter};

use super::status;
use super::{
    rf_lockout_remaining_ms, uptime_seconds, AppCommand, BroadcastMsg, DebugCtx, DomainState,
    HardwareCtx, SessionCtx, TransmissionCommand, WorkerCtx,
};

#[derive(Clone)]
pub struct AppCtx {
    pub domain: Arc<Mutex<DomainState>>,
    pub repository: SharedRepository,
    pub hardware: HardwareCtx,
    pub sessions: SessionCtx,
    pub worker: WorkerCtx,
    pub debug: DebugCtx,
}

#[derive(Clone)]
pub struct ConnectionState {
    pub app: AppCtx,
    pub conn_id: u32,
    pub conn_addr: String,
}

impl AppCtx {
    pub fn new(
        rf: Arc<Mutex<RfTransmitter>>,
        tx_led: Arc<Mutex<Led>>,
        rx_led: Arc<Mutex<Led>>,
        broadcast_tx: BroadcastSender<BroadcastMsg>,
        broadcast_keepalive: InactiveReceiver<BroadcastMsg>,
        rf_receiver: RfReceiver,
        device_settings: DeviceSettings,
        repository: SharedRepository,
        collars: Vec<Collar>,
        presets: Vec<Preset>,
    ) -> Self {
        let remote_control_status = status::remote_control_status_from_settings(&device_settings);
        let (app_tx, app_rx) = mpsc::channel();
        let (transmission_tx, transmission_rx) = std::sync::mpsc::channel();
        Self {
            domain: Arc::new(Mutex::new(DomainState {
                device_settings,
                collars,
                presets,
                preset_name: None,
                rf_lockout_until_ms: 0,
                rf_debug_events: Default::default(),
                event_log_events: Vec::new(),
                remote_control_status,
            })),
            repository,
            hardware: HardwareCtx {
                rf,
                tx_led,
                rx_led,
                rf_receiver: Arc::new(Mutex::new(Some(rf_receiver))),
            },
            sessions: SessionCtx {
                broadcast_tx,
                _broadcast_keepalive: broadcast_keepalive,
                ws_clients: Arc::new(Mutex::new(Vec::new())),
                remote_control_settings_revision: Arc::new(AtomicU32::new(0)),
            },
            worker: WorkerCtx {
                app_tx,
                app_rx: Arc::new(Mutex::new(Some(app_rx))),
                transmission_tx,
                transmission_rx: Arc::new(Mutex::new(Some(transmission_rx))),
                rng: Arc::new(Mutex::new(SmallRng::seed_from_u64(unsafe {
                    esp_idf_svc::sys::esp_random()
                }
                    as u64))),
                event_log_sequence: Arc::new(AtomicU32::new(0)),
            },
            debug: DebugCtx {
                rf_debug_enabled: Arc::new(AtomicBool::new(false)),
                rf_debug_listener_count: Arc::new(AtomicU32::new(0)),
                rf_debug_worker_spawned: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    pub(crate) fn broadcast_json(&self, json: Arc<str>, rf_debug: bool) {
        let _ = self
            .sessions
            .broadcast_tx
            .try_broadcast(BroadcastMsg { json, rf_debug });
    }

    pub(crate) fn broadcast_state(&self) {
        self.broadcast_json(self.state_json(), false);
    }

    pub(crate) fn set_manual_action(
        &self,
        key: super::ActionKey,
        handle: super::ActiveActionHandle,
    ) {
        self.send_transmission_command(TransmissionCommand::UpsertAction { key, handle });
    }

    pub(crate) fn cancel_manual_action(&self, key: super::ActionKey) {
        self.send_transmission_command(TransmissionCommand::CancelAction { key });
    }

    pub(crate) fn cancel_owned_manual_actions(&self, owner: super::ActionOwner) {
        self.send_transmission_command(TransmissionCommand::CancelOwnedActions { owner });
    }

    pub(crate) fn cancel_all_manual_actions(&self) {
        self.send_transmission_command(TransmissionCommand::CancelAllActions);
    }

    pub(crate) fn start_preset_execution(
        &self,
        preset_name: String,
        events: Vec<crate::scheduling::PresetEvent>,
    ) {
        self.send_transmission_command(TransmissionCommand::StartPreset {
            preset_name,
            events,
        });
    }

    pub(crate) fn stop_preset_execution(&self) {
        self.send_transmission_command(TransmissionCommand::StopPreset);
    }

    pub(crate) fn stop_all_execution(&self) {
        self.send_transmission_command(TransmissionCommand::StopAll);
    }

    pub(crate) fn take_transmission_rx(&self) -> std::sync::mpsc::Receiver<TransmissionCommand> {
        self.worker
            .transmission_rx
            .lock()
            .unwrap()
            .take()
            .expect("transmission worker receiver already taken")
    }

    pub(crate) fn take_app_rx(&self) -> std::sync::mpsc::Receiver<AppCommand> {
        self.worker
            .app_rx
            .lock()
            .unwrap()
            .take()
            .expect("app worker receiver already taken")
    }

    fn send_transmission_command(&self, command: TransmissionCommand) {
        self.worker
            .transmission_tx
            .send(command)
            .expect("transmission worker command channel closed");
    }

    fn send_app_command(&self, command: AppCommand) {
        self.worker
            .app_tx
            .send(command)
            .expect("app worker command channel closed");
    }

    fn call_app<T>(&self, build: impl FnOnce(mpsc::SyncSender<T>) -> AppCommand) -> T {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.send_app_command(build(reply_tx));
        reply_rx.recv().expect("app worker reply channel closed")
    }

    pub(crate) fn add_collar(&self, collar: Collar) -> super::ControlResult {
        self.call_app(|reply| AppCommand::AddCollar { collar, reply })
    }

    pub(crate) fn update_collar(
        &self,
        original_name: String,
        updated: Collar,
    ) -> super::ControlResult {
        self.call_app(|reply| AppCommand::UpdateCollar {
            original_name,
            updated,
            reply,
        })
    }

    pub(crate) fn delete_collar(&self, name: String) -> super::ControlResult {
        self.call_app(|reply| AppCommand::DeleteCollar { name, reply })
    }

    pub(crate) fn save_preset(
        &self,
        original_name: Option<String>,
        preset: Preset,
    ) -> super::ControlResult {
        self.call_app(|reply| AppCommand::SavePreset {
            original_name,
            preset,
            reply,
        })
    }

    pub(crate) fn delete_preset(&self, name: String) -> super::ControlResult {
        self.call_app(|reply| AppCommand::DeletePreset { name, reply })
    }

    pub(crate) fn reorder_presets(&self, names: Vec<String>) -> super::ControlResult {
        self.call_app(|reply| AppCommand::ReorderPresets { names, reply })
    }

    pub(crate) fn import_data(&self, data: ExportData) -> super::ControlResult {
        self.call_app(|reply| AppCommand::ImportData { data, reply })
    }

    pub(crate) fn save_device_settings(&self, settings: DeviceSettings) -> super::ControlResult {
        self.call_app(|reply| AppCommand::SaveDeviceSettings { settings, reply })
    }

    pub(crate) fn start_preset_run(
        &self,
        preset_name: String,
        source: EventSource,
        resolved_preset: Option<Preset>,
        events: Vec<crate::scheduling::PresetEvent>,
    ) -> super::ControlResult {
        self.call_app(|reply| AppCommand::StartPresetExecution {
            preset_name,
            source,
            resolved_preset,
            events,
            reply,
        })
    }

    pub(crate) fn stop_preset_run(&self) -> super::ControlResult {
        self.call_app(|reply| AppCommand::StopPreset { reply })
    }

    pub(crate) fn stop_all_run(&self) -> super::ControlResult {
        self.call_app(|reply| AppCommand::StopAll { reply })
    }

    pub(crate) fn clear_rf_debug_events(&self, listening: bool) -> Arc<str> {
        self.call_app(|reply| AppCommand::ClearRfDebugEvents { listening, reply })
    }

    pub(crate) fn complete_preset(&self, preset_name: String) {
        self.send_app_command(AppCommand::CompletePreset { preset_name });
    }

    pub(crate) fn state_json(&self) -> Arc<str> {
        let domain = self.domain.lock().unwrap();
        Arc::from(
            serde_json::to_string(&ServerMessage::State {
                device_id: &domain.device_settings.device_id,
                app_version: crate::build_info::APP_VERSION,
                server_uptime_s: uptime_seconds(),
                collars: &domain.collars,
                presets: &domain.presets,
                preset_running: domain.preset_name.as_deref(),
                rf_lockout_remaining_ms: rf_lockout_remaining_ms(&domain),
            })
            .unwrap(),
        )
    }

    pub(crate) fn rf_debug_state_json(&self, listening: bool) -> Arc<str> {
        let domain = self.domain.lock().unwrap();
        Arc::from(
            serde_json::to_string(&ServerMessage::RfDebugState {
                listening,
                events: &domain.rf_debug_events,
            })
            .unwrap(),
        )
    }

    pub(crate) fn remote_control_status_json(&self) -> Arc<str> {
        let status = self.domain.lock().unwrap().remote_control_status.clone();
        Arc::from(serde_json::to_string(&ServerMessage::RemoteControlStatus { status }).unwrap())
    }

    pub(crate) fn event_log_state_json(&self) -> Arc<str> {
        let domain = self.domain.lock().unwrap();
        Arc::from(
            serde_json::to_string(&ServerMessage::EventLogState {
                enabled: domain.device_settings.record_event_log,
                events: &domain.event_log_events,
            })
            .unwrap(),
        )
    }

    pub(crate) fn remote_sync_jsons(&self) -> [Arc<str>; 3] {
        [
            self.remote_control_status_json(),
            self.state_json(),
            self.event_log_state_json(),
        ]
    }

    pub(crate) fn local_ui_sync_jsons(&self, listening_rf_debug: bool) -> [Arc<str>; 4] {
        [
            self.state_json(),
            self.remote_control_status_json(),
            self.event_log_state_json(),
            self.rf_debug_state_json(listening_rf_debug),
        ]
    }

    pub(crate) fn broadcast_remote_control_status(&self) {
        self.broadcast_json(self.remote_control_status_json(), false);
    }

    pub(crate) fn broadcast_event_log_state(&self) {
        self.broadcast_json(self.event_log_state_json(), false);
    }

    pub(crate) fn set_remote_control_status(&self, status: RemoteControlStatus) {
        self.send_app_command(AppCommand::SetRemoteControlStatus { status });
    }

    pub(crate) fn record_event(&self, source: EventSource, kind: EventLogEntryKind) {
        self.send_app_command(AppCommand::RecordEvent { source, kind });
    }

    pub(crate) fn persist_collars(&self, collars: &[Collar]) {
        if let Err(err) = self.repository.lock().unwrap().save_collars(collars) {
            log::error!("NVS save_collars failed: {err:#}");
        }
    }

    pub(crate) fn persist_presets(&self, presets: &[Preset]) {
        if let Err(err) = self.repository.lock().unwrap().save_presets(presets) {
            log::error!("NVS save_presets failed: {err:#}");
        }
    }

    pub(crate) fn persist_settings(&self, settings: &DeviceSettings) -> anyhow::Result<()> {
        self.repository
            .lock()
            .unwrap()
            .save_settings(settings)
            .map_err(Into::into)
    }

    pub(crate) fn push_rf_debug_event(&self, event: crate::protocol::RfDebugFrame) {
        self.send_app_command(AppCommand::PushRfDebugEvent { event });
    }
}
