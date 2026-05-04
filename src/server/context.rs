use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use async_broadcast::{InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender};
use rand::rngs::SmallRng;
use rand::SeedableRng;

use crate::led::Led;
use crate::protocol::{
    ClientInfo, Collar, CommandMode, DeviceSettings, EventLogEntryKind, EventSource, ExportData,
    MqttStatus, Preset, RemoteControlStatus,
};
use crate::repository::RepositoryServices;
use crate::rf::{RfReceiver, RfTransmitter};

use super::status;
use super::{
    rf_lockout_remaining_ms, uptime_seconds, AppCommand, AppEvent, DebugCtx, DomainState,
    HardwareCtx, SessionCtx, TransmissionCommand, WorkerCtx,
};

#[derive(Clone)]
pub struct AppCtx {
    domain: Arc<Mutex<DomainState>>,
    repository_services: RepositoryServices,
    hardware: HardwareCtx,
    sessions: SessionCtx,
    worker: WorkerCtx,
    debug: DebugCtx,
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
        broadcast_tx: BroadcastSender<AppEvent>,
        broadcast_keepalive: InactiveReceiver<AppEvent>,
        rf_receiver: RfReceiver,
        device_settings: DeviceSettings,
        repository_services: RepositoryServices,
        collars: Vec<Collar>,
        presets: Vec<Preset>,
    ) -> Self {
        let remote_control_status = status::remote_control_status_from_settings(&device_settings);
        let mqtt_status = status::mqtt_status_from_settings(&device_settings);
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
                mqtt_status,
            })),
            repository_services,
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
                mqtt_settings_revision: Arc::new(AtomicU32::new(0)),
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

    pub(crate) fn broadcast_event(&self, event: AppEvent) {
        let _ = self.sessions.broadcast_tx.try_broadcast(event);
    }

    pub(crate) fn with_domain<T>(&self, f: impl FnOnce(&DomainState) -> T) -> T {
        let domain = self.domain.lock().unwrap();
        f(&domain)
    }

    pub(crate) fn with_domain_mut<T>(&self, f: impl FnOnce(&mut DomainState) -> T) -> T {
        let mut domain = self.domain.lock().unwrap();
        f(&mut domain)
    }

    pub(crate) fn with_rng<T>(&self, f: impl FnOnce(&mut SmallRng) -> T) -> T {
        let mut rng = self.worker.rng.lock().unwrap();
        f(&mut rng)
    }

    pub(crate) fn device_settings(&self) -> DeviceSettings {
        self.with_domain(|domain| domain.device_settings.clone())
    }

    pub(crate) fn collars_snapshot(&self) -> Vec<Collar> {
        self.with_domain(|domain| domain.collars.clone())
    }

    pub(crate) fn export_data_snapshot(&self) -> ExportData {
        self.with_domain(|domain| ExportData {
            collars: domain.collars.clone(),
            presets: domain.presets.clone(),
        })
    }

    pub(crate) fn register_ws_client(&self, conn_id: u32, info: ClientInfo) {
        self.sessions
            .ws_clients
            .lock()
            .unwrap()
            .push((conn_id, info));
    }

    pub(crate) fn unregister_ws_client(&self, conn_id: u32) {
        self.sessions
            .ws_clients
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != conn_id);
    }

    pub(crate) fn ws_clients_snapshot(&self) -> Vec<ClientInfo> {
        self.sessions
            .ws_clients
            .lock()
            .unwrap()
            .iter()
            .map(|(_, info)| info.clone())
            .collect()
    }

    pub(crate) fn new_broadcast_receiver(&self) -> BroadcastReceiver<AppEvent> {
        self.sessions.broadcast_tx.new_receiver()
    }

    pub(crate) fn remote_control_settings_revision(&self) -> u32 {
        self.sessions
            .remote_control_settings_revision
            .load(Ordering::SeqCst)
    }

    pub(crate) fn bump_remote_control_settings_revision(&self) {
        self.sessions
            .remote_control_settings_revision
            .fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn mqtt_settings_revision(&self) -> u32 {
        self.sessions
            .mqtt_settings_revision
            .load(Ordering::SeqCst)
    }

    pub(crate) fn bump_mqtt_settings_revision(&self) {
        self.sessions
            .mqtt_settings_revision
            .fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn next_event_log_sequence(&self) -> u64 {
        u64::from(
            self.worker
                .event_log_sequence
                .fetch_add(1, Ordering::SeqCst)
                + 1,
        )
    }

    pub(crate) fn repository_services(&self) -> &RepositoryServices {
        &self.repository_services
    }

    pub(crate) fn max_clients(&self) -> u32 {
        self.with_domain(|domain| domain.device_settings.max_clients as u32)
    }

    pub(crate) fn transmit_rf_command_now(
        &self,
        collar_id: u16,
        channel: u8,
        mode_byte: u8,
        intensity: u8,
    ) -> anyhow::Result<()> {
        super::rf_send_with_led(
            &self.hardware.rf,
            &self.hardware.tx_led,
            collar_id,
            channel,
            mode_byte,
            intensity,
        )
    }

    pub(crate) fn take_rf_receiver(&self) -> Option<RfReceiver> {
        self.hardware.rf_receiver.lock().unwrap().take()
    }

    pub(crate) fn set_rx_led(&self, enabled: bool) {
        self.hardware.rx_led.lock().unwrap().set(enabled);
    }

    pub(crate) fn try_mark_rf_debug_worker_spawned(&self) -> bool {
        self.debug
            .rf_debug_worker_spawned
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn clear_rf_debug_worker_spawned(&self) {
        self.debug
            .rf_debug_worker_spawned
            .store(false, Ordering::SeqCst);
    }

    pub(crate) fn set_rf_debug_enabled(&self, enabled: bool) {
        self.debug.rf_debug_enabled.store(enabled, Ordering::SeqCst);
    }

    pub(crate) fn rf_debug_enabled_handle(&self) -> Arc<AtomicBool> {
        self.debug.rf_debug_enabled.clone()
    }

    pub(crate) fn increment_rf_debug_listener_count(&self) {
        self.debug
            .rf_debug_listener_count
            .fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn release_rf_debug_listener(&self) -> bool {
        self.debug
            .rf_debug_listener_count
            .fetch_sub(1, Ordering::SeqCst)
            <= 1
    }

    pub(crate) fn broadcast_state(&self) {
        self.broadcast_event(self.state_event());
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

    pub(crate) fn clear_rf_debug_events(&self, listening: bool) -> AppEvent {
        self.call_app(|reply| AppCommand::ClearRfDebugEvents { listening, reply })
    }

    pub(crate) fn complete_preset(&self, preset_name: String) {
        self.send_app_command(AppCommand::CompletePreset { preset_name });
    }

    pub(crate) fn state_event(&self) -> AppEvent {
        self.with_domain(|domain| AppEvent::State {
            device_id: domain.device_settings.device_id.clone(),
            app_version: crate::build_info::APP_VERSION,
            server_uptime_s: uptime_seconds(),
            collars: domain.collars.clone(),
            presets: domain.presets.clone(),
            preset_running: domain.preset_name.clone(),
            rf_lockout_remaining_ms: rf_lockout_remaining_ms(domain),
        })
    }

    pub(crate) fn rf_debug_state_event(&self, listening: bool) -> AppEvent {
        self.with_domain(|domain| AppEvent::RfDebugState {
            listening,
            events: domain.rf_debug_events.clone(),
        })
    }

    pub(crate) fn remote_control_status_event(&self) -> AppEvent {
        let status = self.with_domain(|domain| domain.remote_control_status.clone());
        AppEvent::RemoteControlStatus { status }
    }

    pub(crate) fn event_log_state_event(&self) -> AppEvent {
        self.with_domain(|domain| AppEvent::EventLogState {
            enabled: domain.device_settings.record_event_log,
            events: domain.event_log_events.clone(),
        })
    }

    pub(crate) fn remote_sync_events(&self) -> [AppEvent; 3] {
        [
            self.remote_control_status_event(),
            self.state_event(),
            self.event_log_state_event(),
        ]
    }

    pub(crate) fn local_ui_sync_events(&self, listening_rf_debug: bool) -> [AppEvent; 5] {
        [
            self.state_event(),
            self.remote_control_status_event(),
            self.mqtt_status_event(),
            self.event_log_state_event(),
            self.rf_debug_state_event(listening_rf_debug),
        ]
    }

    pub(crate) fn set_remote_control_status(&self, status: RemoteControlStatus) {
        self.send_app_command(AppCommand::SetRemoteControlStatus { status });
    }

    pub(crate) fn set_mqtt_status(&self, status: MqttStatus) {
        self.send_app_command(AppCommand::SetMqttStatus { status });
    }

    pub(crate) fn mqtt_status_event(&self) -> AppEvent {
        let status = self.with_domain(|domain| domain.mqtt_status.clone());
        AppEvent::MqttStatus { status }
    }

    pub(crate) fn broadcast_action_fired(
        &self,
        collar_name: String,
        mode: CommandMode,
        intensity: u8,
    ) {
        self.broadcast_event(AppEvent::ActionFired {
            collar_name,
            mode,
            intensity,
        });
    }

    pub(crate) fn record_event(&self, source: EventSource, kind: EventLogEntryKind) {
        self.send_app_command(AppCommand::RecordEvent { source, kind });
    }

    pub(crate) fn push_rf_debug_event(&self, event: crate::protocol::RfDebugFrame) {
        self.send_app_command(AppCommand::PushRfDebugEvent { event });
    }
}
