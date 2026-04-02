use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_broadcast::Sender as BroadcastSender;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use crate::build_info::APP_VERSION;
use crate::led::Led;
use crate::protocol::{
    Collar, CommandMode, DeviceSettings, Distribution, EventLogEntry, EventLogEntryKind,
    EventSource, Preset, RemoteControlStatus, RfDebugFrame, ServerMessage,
};
use crate::rf::{RfReceiver, RfTransmitter};
use crate::scheduling::{PresetEvent, StepResolver};
use crate::storage::Storage;

mod control;
mod http;
mod runtime;
mod status;
mod ws;

const HAS_WIFI: bool = cfg!(has_wifi);
const MAX_EVENT_LOG_ENTRIES: usize = 100;
const MAX_RF_DEBUG_EVENTS: usize = 100;
const RF_DEBUG_DISABLED_SLEEP_MS: u64 = 100;
const RF_STOP_LOCKOUT_MS: u64 = 10_000;
const VALID_UNIX_TIME_THRESHOLD_MS: u64 = 946_684_800_000;

pub(crate) use control::{cancel_owned_manual_actions, pong_json, process_control_message};
pub use runtime::run_server;
pub(crate) use status::{
    parse_remote_control_url, remote_control_endpoint_url, remote_control_status,
};

struct RandomResolver<'a> {
    rng: &'a mut SmallRng,
}

impl StepResolver for RandomResolver<'_> {
    fn resolve_duration(&mut self, min: u32, max: u32, distribution: Distribution) -> u32 {
        resolve_random_duration(self.rng, min, max, distribution)
    }

    fn resolve_intensity(&mut self, min: u8, max: u8, distribution: Distribution) -> u8 {
        resolve_random_u8(self.rng, min, max, distribution)
    }
}

fn resolve_random_duration(
    rng: &mut SmallRng,
    min: u32,
    max: u32,
    distribution: Distribution,
) -> u32 {
    let min_steps = min / 500;
    let max_steps = max / 500;
    match distribution {
        Distribution::Uniform => rng.random_range(min_steps..=max_steps) * 500,
        Distribution::Gaussian => {
            let value = gaussian_sample(rng, min_steps as f32, max_steps as f32);
            (value.round().clamp(min_steps as f32, max_steps as f32) as u32) * 500
        }
    }
}

fn resolve_random_u8(rng: &mut SmallRng, min: u8, max: u8, distribution: Distribution) -> u8 {
    match distribution {
        Distribution::Uniform => rng.random_range(min..=max),
        Distribution::Gaussian => {
            let value = gaussian_sample(rng, min as f32, max as f32);
            value.round().clamp(min as f32, max as f32) as u8
        }
    }
}

fn gaussian_sample(rng: &mut SmallRng, lo: f32, hi: f32) -> f32 {
    let mean = (lo + hi) / 2.0;
    let sigma = (hi - lo) / 4.0;
    let u1: f32 = rng.random::<f32>().max(f32::EPSILON);
    let u2: f32 = rng.random::<f32>();
    let z = (-2.0 * u1.ln()).sqrt() * (2.0 * core::f32::consts::PI * u2).cos();
    mean + sigma * z
}

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

type ControlResult = core::result::Result<Vec<String>, String>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ActionKey {
    collar_name: String,
    mode: CommandMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ActionOwner {
    LocalWs(u32),
    RemoteControl,
}

struct ActiveActionHandle {
    owner: Option<ActionOwner>,
    cancel_on_disconnect: bool,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
    deadline: Option<Instant>,
    started_at: Instant,
    source: EventSource,
}

pub(crate) struct PendingPreset {
    pub events: Vec<PresetEvent>,
    pub preset_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteControlUrlKind {
    Ws,
    Wss,
}

pub struct DomainState {
    pub device_settings: DeviceSettings,
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
    pub preset_name: Option<String>,
    pub pending_preset: Option<PendingPreset>,
    pub rf_lockout_until_ms: u64,
    pub rf_debug_events: VecDeque<RfDebugFrame>,
    pub event_log_events: Vec<EventLogEntry>,
    pub remote_control_status: RemoteControlStatus,
}

#[derive(Clone)]
pub struct AppCtx {
    pub domain: Arc<Mutex<DomainState>>,
    pub storage: Arc<Mutex<Storage>>,
    pub rf: Arc<Mutex<RfTransmitter>>,
    pub tx_led: Arc<Mutex<Led>>,
    pub rx_led: Arc<Mutex<Led>>,
    pub broadcast_tx: BroadcastSender<BroadcastMsg>,
    pub rf_debug_enabled: Arc<AtomicBool>,
    pub rf_debug_listener_count: Arc<AtomicU32>,
    pub rf_debug_worker_spawned: Arc<AtomicBool>,
    pub rf_receiver: Arc<Mutex<Option<RfReceiver>>>,
    pub preset_run_id: Arc<AtomicU32>,
    active_actions: Arc<Mutex<HashMap<ActionKey, ActiveActionHandle>>>,
    event_log_sequence: Arc<AtomicU32>,
    pub remote_control_settings_revision: Arc<AtomicU32>,
    pub rng: Arc<Mutex<SmallRng>>,
    pub ws_clients: Arc<Mutex<Vec<(u32, String)>>>,
    worker_notify: Arc<(Mutex<()>, Condvar)>,
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
        rf_receiver: RfReceiver,
        device_settings: DeviceSettings,
        storage: Storage,
        collars: Vec<Collar>,
        presets: Vec<Preset>,
    ) -> Self {
        let remote_control_status = status::remote_control_status_from_settings(&device_settings);
        Self {
            domain: Arc::new(Mutex::new(DomainState {
                device_settings,
                collars,
                presets,
                preset_name: None,
                pending_preset: None,
                rf_lockout_until_ms: 0,
                rf_debug_events: VecDeque::new(),
                event_log_events: Vec::new(),
                remote_control_status,
            })),
            storage: Arc::new(Mutex::new(storage)),
            rf,
            tx_led,
            rx_led,
            broadcast_tx,
            rf_debug_enabled: Arc::new(AtomicBool::new(false)),
            rf_debug_listener_count: Arc::new(AtomicU32::new(0)),
            rf_debug_worker_spawned: Arc::new(AtomicBool::new(false)),
            rf_receiver: Arc::new(Mutex::new(Some(rf_receiver))),
            preset_run_id: Arc::new(AtomicU32::new(0)),
            active_actions: Arc::new(Mutex::new(HashMap::new())),
            event_log_sequence: Arc::new(AtomicU32::new(0)),
            remote_control_settings_revision: Arc::new(AtomicU32::new(0)),
            rng: Arc::new(Mutex::new(SmallRng::seed_from_u64(unsafe {
                esp_idf_svc::sys::esp_random()
            } as u64))),
            ws_clients: Arc::new(Mutex::new(Vec::new())),
            worker_notify: Arc::new((Mutex::new(()), Condvar::new())),
        }
    }

    fn broadcast_json(&self, json: Arc<str>, rf_debug: bool) {
        let _ = self
            .broadcast_tx
            .try_broadcast(BroadcastMsg { json, rf_debug });
    }

    pub(crate) fn broadcast_state(&self) {
        self.broadcast_json(self.state_json(), false);
    }

    pub(crate) fn notify_worker(&self) {
        let _lock = self.worker_notify.0.lock().unwrap();
        self.worker_notify.1.notify_one();
    }

    pub(crate) fn state_json(&self) -> Arc<str> {
        let domain = self.domain.lock().unwrap();
        Arc::from(
            serde_json::to_string(&ServerMessage::State {
                device_id: &domain.device_settings.device_id,
                app_version: APP_VERSION,
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
        let changed = {
            let mut domain = self.domain.lock().unwrap();
            if domain.remote_control_status == status {
                false
            } else {
                domain.remote_control_status = status;
                true
            }
        };

        if changed {
            self.broadcast_remote_control_status();
        }
    }

    pub(crate) fn record_event(&self, source: EventSource, kind: EventLogEntryKind) {
        let entry = {
            let mut domain = self.domain.lock().unwrap();
            if !domain.device_settings.record_event_log {
                return;
            }

            let entry = EventLogEntry {
                sequence: u64::from(self.event_log_sequence.fetch_add(1, Ordering::SeqCst) + 1),
                monotonic_ms: now_millis(),
                unix_ms: current_unix_ms(),
                source,
                kind,
            };

            domain.event_log_events.push(entry.clone());
            if domain.event_log_events.len() > MAX_EVENT_LOG_ENTRIES {
                let excess = domain.event_log_events.len() - MAX_EVENT_LOG_ENTRIES;
                domain.event_log_events.drain(0..excess);
            }

            entry
        };

        self.broadcast_json(
            Arc::from(
                serde_json::to_string(&ServerMessage::EventLogEvent { event: &entry }).unwrap(),
            ),
            false,
        );
    }

    fn persist_collars(&self, collars: &[Collar]) {
        if let Err(err) = self.storage.lock().unwrap().save_collars(collars) {
            log::error!("NVS save_collars failed: {err:#}");
        }
    }

    fn persist_presets(&self, presets: &[Preset]) {
        if let Err(err) = self.storage.lock().unwrap().save_presets(presets) {
            log::error!("NVS save_presets failed: {err:#}");
        }
    }

    fn persist_settings(&self, settings: &DeviceSettings) -> Result<()> {
        self.storage
            .lock()
            .unwrap()
            .save_settings(settings)
            .map_err(Into::into)
    }
}

fn rf_lockout_remaining_ms(domain: &DomainState) -> u64 {
    domain.rf_lockout_until_ms.saturating_sub(now_millis())
}

fn now_millis() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1000 }
}

fn current_unix_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .filter(|unix_ms| *unix_ms >= VALID_UNIX_TIME_THRESHOLD_MS)
}

fn uptime_seconds() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1_000_000 }
}

fn free_heap() -> u32 {
    unsafe { esp_idf_svc::sys::esp_get_free_heap_size() }
}

fn command_intensity(mode: CommandMode, intensity: u8) -> u8 {
    if mode.has_intensity() {
        intensity
    } else {
        0
    }
}

fn event_source(origin: MessageOrigin) -> EventSource {
    match origin {
        MessageOrigin::LocalUi => EventSource::LocalUi,
        MessageOrigin::RemoteControl => EventSource::RemoteControl,
    }
}

fn device_settings_reboot_required(previous: &DeviceSettings, next: &DeviceSettings) -> bool {
    previous.tx_led_pin != next.tx_led_pin
        || previous.rx_led_pin != next.rx_led_pin
        || previous.rf_tx_pin != next.rf_tx_pin
        || previous.rf_rx_pin != next.rf_rx_pin
        || previous.wifi_client_enabled != next.wifi_client_enabled
        || previous.wifi_ssid != next.wifi_ssid
        || previous.wifi_password != next.wifi_password
        || previous.ap_enabled != next.ap_enabled
        || previous.ap_password != next.ap_password
        || previous.max_clients != next.max_clients
        || previous.ntp_enabled != next.ntp_enabled
        || previous.ntp_server != next.ntp_server
}

fn stop_all_transmissions(domain: &mut DomainState, preset_run_id: &AtomicU32) {
    domain.pending_preset = None;
    domain.preset_name = None;
    domain.rf_lockout_until_ms = now_millis() + RF_STOP_LOCKOUT_MS;
    preset_run_id.fetch_add(1, Ordering::SeqCst);
}

fn stop_active_preset(domain: &mut DomainState, preset_run_id: &AtomicU32) {
    if domain.preset_name.is_some() {
        domain.pending_preset = None;
        domain.preset_name = None;
        preset_run_id.fetch_add(1, Ordering::SeqCst);
    }
}

struct TxLedGuard<'a> {
    tx_led: &'a Mutex<Led>,
}

impl<'a> TxLedGuard<'a> {
    fn new(tx_led: &'a Mutex<Led>) -> Self {
        tx_led.lock().unwrap().set(true);
        Self { tx_led }
    }
}

impl Drop for TxLedGuard<'_> {
    fn drop(&mut self) {
        self.tx_led.lock().unwrap().set(false);
    }
}

fn rf_send_with_led(
    rf: &Mutex<RfTransmitter>,
    tx_led: &Mutex<Led>,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
) -> Result<()> {
    let _tx_led_guard = TxLedGuard::new(tx_led);
    rf.lock()
        .unwrap()
        .send_command(collar_id, channel, mode_byte, intensity)
        .map_err(Into::into)
}

pub(crate) fn error_json(message: impl Into<String>) -> String {
    serde_json::to_string(&ServerMessage::Error {
        message: message.into(),
    })
    .unwrap()
}
