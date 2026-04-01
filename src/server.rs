use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_broadcast::Sender as BroadcastSender;
use log::{error, info, warn};
use picoserve::futures::Either;
use picoserve::response::ws::{self, Message};
use picoserve::routing::{get, get_service, post_service};

use crate::ota;

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};
use crate::build_info::APP_VERSION;
use crate::led::Led;
use crate::protocol::{
    ApClientInfo, ApStatus, ClientMessage, Collar, CommandMode, DeviceSettings, Distribution,
    EventLogEntry, EventLogEntryKind, EventSource, ExportData, InterfaceStatus, MemoryRegion,
    Preset, RemoteControlStatus, RfDebugFrame, ServerMessage, MAX_INTENSITY,
};
use crate::rf::{RfReceiver, RfTransmitter};
use crate::scheduling::{self, PresetEvent, StepResolver};
use crate::storage::Storage;
use crate::validation;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

const FRONTEND_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/frontend.html.gz"));
const HAS_WIFI: bool = cfg!(has_wifi);
const MAX_RF_DEBUG_EVENTS: usize = 100;
const MAX_EVENT_LOG_ENTRIES: usize = 100;
const RF_STOP_LOCKOUT_MS: u64 = 10_000;
const RF_DEBUG_DISABLED_SLEEP_MS: u64 = 100;
const MANUAL_ACTION_REPEAT_MS: u64 = 200;
const MANUAL_ACTION_SLEEP_SLICE_MS: u64 = 50;
const VALID_UNIX_TIME_THRESHOLD_MS: u64 = 946_684_800_000;
const HTTP_BUF_SIZE: usize = 1024;
const WS_BUF_SIZE: usize = 2048;

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

/// Sample a random duration in [min, max], snapped to 500ms steps.
fn resolve_random_duration(rng: &mut SmallRng, min: u32, max: u32, distribution: Distribution) -> u32 {
    let min_steps = min / 500;
    let max_steps = max / 500;
    match distribution {
        Distribution::Uniform => rng.random_range(min_steps..=max_steps) * 500,
        Distribution::Gaussian => {
            let v = gaussian_sample(rng, min_steps as f32, max_steps as f32);
            (v.round().clamp(min_steps as f32, max_steps as f32) as u32) * 500
        }
    }
}

/// Sample a random u8 in [min, max].
fn resolve_random_u8(rng: &mut SmallRng, min: u8, max: u8, distribution: Distribution) -> u8 {
    match distribution {
        Distribution::Uniform => rng.random_range(min..=max),
        Distribution::Gaussian => {
            let v = gaussian_sample(rng, min as f32, max as f32);
            v.round().clamp(min as f32, max as f32) as u8
        }
    }
}

/// Box-Muller gaussian sample centered on the midpoint of [lo, hi].
/// sigma = (hi - lo) / 4, so ~95% of samples fall within [lo, hi].
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
    run_id: u32,
    cancel: Arc<AtomicBool>,
    owner: Option<ActionOwner>,
    cancel_on_disconnect: bool,
}

#[derive(Clone)]
struct ManualActionSpec {
    key: ActionKey,
    collar_id: u16,
    channel: u8,
    mode: CommandMode,
    intensity: u8,
    intensity_max: Option<u8>,
    duration_ms: Option<u32>,
    duration_max_ms: Option<u32>,
    intensity_distribution: Distribution,
    duration_distribution: Distribution,
    source: EventSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteControlUrlKind {
    Ws,
    Wss,
}

// --- Shared state ---

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
    next_action_id: Arc<AtomicU32>,
    event_log_sequence: Arc<AtomicU32>,
    pub remote_control_settings_revision: Arc<AtomicU32>,
    pub rng: Arc<Mutex<SmallRng>>,
    /// Active WS client addresses, keyed by conn_id.
    pub ws_clients: Arc<Mutex<Vec<(u32, String)>>>,
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
        let remote_control_status = remote_control_status_from_settings(&device_settings);
        Self {
            domain: Arc::new(Mutex::new(DomainState {
                device_settings,
                collars,
                presets,
                preset_name: None,
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
            next_action_id: Arc::new(AtomicU32::new(0)),
            event_log_sequence: Arc::new(AtomicU32::new(0)),
            remote_control_settings_revision: Arc::new(AtomicU32::new(0)),
            rng: Arc::new(Mutex::new(SmallRng::seed_from_u64(
                unsafe { esp_idf_svc::sys::esp_random() } as u64,
            ))),
            ws_clients: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn broadcast_state(&self) {
        let _ = self.broadcast_tx.try_broadcast(BroadcastMsg {
            json: self.state_json(),
            rf_debug: false,
        });
    }

    pub(crate) fn state_json(&self) -> Arc<str> {
        let d = self.domain.lock().unwrap();
        let msg = ServerMessage::State {
            device_id: &d.device_settings.device_id,
            app_version: APP_VERSION,
            server_uptime_s: uptime_seconds(),
            collars: &d.collars,
            presets: &d.presets,
            preset_running: d.preset_name.as_deref(),
            rf_lockout_remaining_ms: rf_lockout_remaining_ms(&d),
        };
        Arc::from(serde_json::to_string(&msg).unwrap())
    }

    pub(crate) fn rf_debug_state_json(&self, listening: bool) -> Arc<str> {
        let d = self.domain.lock().unwrap();
        let msg = ServerMessage::RfDebugState {
            listening,
            events: &d.rf_debug_events,
        };
        Arc::from(serde_json::to_string(&msg).unwrap())
    }

    pub(crate) fn remote_control_status_json(&self) -> Arc<str> {
        let status = self.domain.lock().unwrap().remote_control_status.clone();
        Arc::from(serde_json::to_string(&ServerMessage::RemoteControlStatus { status }).unwrap())
    }

    pub(crate) fn event_log_state_json(&self) -> Arc<str> {
        let d = self.domain.lock().unwrap();
        let msg = ServerMessage::EventLogState {
            enabled: d.device_settings.record_event_log,
            events: &d.event_log_events,
        };
        Arc::from(serde_json::to_string(&msg).unwrap())
    }

    pub(crate) fn broadcast_remote_control_status(&self) {
        let _ = self.broadcast_tx.try_broadcast(BroadcastMsg {
            json: self.remote_control_status_json(),
            rf_debug: false,
        });
    }

    pub(crate) fn broadcast_event_log_state(&self) {
        let _ = self.broadcast_tx.try_broadcast(BroadcastMsg {
            json: self.event_log_state_json(),
            rf_debug: false,
        });
    }

    pub(crate) fn set_remote_control_status(&self, status: RemoteControlStatus) {
        let changed = {
            let mut d = self.domain.lock().unwrap();
            if d.remote_control_status == status {
                false
            } else {
                d.remote_control_status = status;
                true
            }
        };

        if changed {
            self.broadcast_remote_control_status();
        }
    }

    pub(crate) fn record_event(&self, source: EventSource, kind: EventLogEntryKind) {
        let entry = {
            let mut d = self.domain.lock().unwrap();
            if !d.device_settings.record_event_log {
                return;
            }

            let entry = EventLogEntry {
                sequence: u64::from(self.event_log_sequence.fetch_add(1, Ordering::SeqCst) + 1),
                monotonic_ms: now_millis(),
                unix_ms: current_unix_ms(),
                source,
                kind,
            };

            d.event_log_events.push(entry.clone());
            if d.event_log_events.len() > MAX_EVENT_LOG_ENTRIES {
                let excess = d.event_log_events.len() - MAX_EVENT_LOG_ENTRIES;
                d.event_log_events.drain(0..excess);
            }

            entry
        };

        let json = serde_json::to_string(&ServerMessage::EventLogEvent { event: &entry }).unwrap();
        let _ = self.broadcast_tx.try_broadcast(BroadcastMsg {
            json: Arc::from(json),
            rf_debug: false,
        });
    }
}

fn remote_control_status_from_settings(settings: &DeviceSettings) -> RemoteControlStatus {
    let trimmed_url = settings.remote_control_url.trim();
    let status_text = if !settings.remote_control_enabled {
        "Off".to_string()
    } else if trimmed_url.is_empty() {
        "Missing URL".to_string()
    } else if parse_remote_control_url(trimmed_url).is_err() {
        "Invalid URL".to_string()
    } else {
        "Connecting...".to_string()
    };

    RemoteControlStatus {
        enabled: settings.remote_control_enabled,
        connected: false,
        url: trimmed_url.to_string(),
        validate_cert: settings.remote_control_validate_cert,
        rtt_ms: None,
        status_text,
    }
}

fn rf_lockout_remaining_ms(d: &DomainState) -> u64 {
    d.rf_lockout_until_ms.saturating_sub(now_millis())
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

fn free_internal_heap() -> u32 {
    unsafe { esp_idf_svc::sys::esp_get_free_internal_heap_size() }
}

/// Memory region info: (total, free) in bytes.
#[derive(Debug, Clone, Copy)]
struct MemRegion {
    total: u32,
    free: u32,
}

fn mem_region(cap: u32) -> MemRegion {
    use esp_idf_svc::sys::*;
    unsafe {
        MemRegion {
            total: heap_caps_get_total_size(cap) as u32,
            free: heap_caps_get_free_size(cap) as u32,
        }
    }
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

pub(crate) fn parse_remote_control_url(
    url: &str,
) -> core::result::Result<RemoteControlUrlKind, String> {
    let trimmed = url.trim();
    let kind = if trimmed.starts_with("ws://") {
        RemoteControlUrlKind::Ws
    } else if trimmed.starts_with("wss://") {
        RemoteControlUrlKind::Wss
    } else {
        return Err("Remote control URL must start with ws:// or wss://".to_string());
    };

    let remainder = &trimmed[(if matches!(kind, RemoteControlUrlKind::Ws) {
        5
    } else {
        6
    })..];
    if remainder.is_empty() {
        return Err("Remote control URL host cannot be empty".to_string());
    }
    if remainder.starts_with('/') || remainder.starts_with('?') || remainder.starts_with('#') {
        return Err("Remote control URL must include a host".to_string());
    }
    if remainder.chars().any(char::is_whitespace) {
        return Err("Remote control URL cannot contain whitespace".to_string());
    }

    Ok(kind)
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

fn stop_all_transmissions(d: &mut DomainState, preset_run_id: &AtomicU32) {
    preset_run_id.fetch_add(1, Ordering::SeqCst);
    d.preset_name = None;
    d.rf_lockout_until_ms = now_millis() + RF_STOP_LOCKOUT_MS;
}

fn stop_active_preset(d: &mut DomainState, preset_run_id: &AtomicU32) {
    if d.preset_name.is_some() {
        preset_run_id.fetch_add(1, Ordering::SeqCst);
        d.preset_name = None;
    }
}

fn rollback_failed_preset_start(ctx: &AppCtx, preset_name: &str, run_id: u32) {
    let rolled_back = {
        let mut d = ctx.domain.lock().unwrap();
        if ctx.preset_run_id.load(Ordering::SeqCst) != run_id
            || d.preset_name.as_deref() != Some(preset_name)
        {
            false
        } else {
            let previous_run_id = ctx.preset_run_id.fetch_sub(1, Ordering::SeqCst);
            assert_eq!(
                previous_run_id, run_id,
                "preset run id changed while rolling back failed preset start"
            );
            d.preset_name = None;
            true
        }
    };

    if rolled_back {
        ctx.broadcast_state();
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
    let result = rf
        .lock()
        .unwrap()
        .send_command(collar_id, channel, mode_byte, intensity);
    result.map_err(Into::into)
}

fn save_collars(ctx: &AppCtx, collars: &[Collar]) -> Result<()> {
    ctx.storage
        .lock()
        .unwrap()
        .save_collars(collars)
        .map_err(Into::into)
}

fn save_presets(ctx: &AppCtx, presets: &[Preset]) -> Result<()> {
    ctx.storage
        .lock()
        .unwrap()
        .save_presets(presets)
        .map_err(Into::into)
}

fn save_settings(ctx: &AppCtx, settings: &DeviceSettings) -> Result<()> {
    ctx.storage
        .lock()
        .unwrap()
        .save_settings(settings)
        .map_err(Into::into)
}

fn log_storage_result(operation: &str, result: Result<()>) {
    if let Err(err) = result {
        error!("NVS {operation} failed: {err:#}");
    }
}

// --- Network status ---

fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn gather_network_status(settings: &DeviceSettings) -> ServerMessage<'static> {
    use esp_idf_svc::sys::*;

    // Board base MAC (unique chip identifier)
    let board_mac = {
        let mut mac = [0u8; 6];
        unsafe { esp_efuse_mac_get_default(mac.as_mut_ptr()) };
        format_mac(&mac)
    };

    // Helper: get IP string from a netif, or empty string
    let get_netif_ip = |key: &[u8]| -> String {
        unsafe {
            let netif = esp_netif_get_handle_from_ifkey(key.as_ptr() as *const _);
            if netif.is_null() {
                return String::new();
            }
            let mut ip_info: esp_netif_ip_info_t = core::mem::zeroed();
            if esp_netif_get_ip_info(netif, &mut ip_info) == ESP_OK && ip_info.ip.addr != 0 {
                let ip = ip_info.ip.addr;
                format!(
                    "{}.{}.{}.{}",
                    ip & 0xFF,
                    (ip >> 8) & 0xFF,
                    (ip >> 16) & 0xFF,
                    (ip >> 24) & 0xFF
                )
            } else {
                String::new()
            }
        }
    };

    // Helper: get MAC from a netif
    let get_netif_mac = |key: &[u8]| -> String {
        unsafe {
            let netif = esp_netif_get_handle_from_ifkey(key.as_ptr() as *const _);
            if netif.is_null() {
                return String::new();
            }
            let mut mac = [0u8; 6];
            if esp_netif_get_mac(netif, mac.as_mut_ptr()) == ESP_OK {
                format_mac(&mac)
            } else {
                String::new()
            }
        }
    };

    // Ethernet
    let ethernet = {
        let mac = get_netif_mac(b"ETH_DEF\0");
        let ip = get_netif_ip(b"ETH_DEF\0");
        let available = !mac.is_empty();
        let connected = !ip.is_empty();
        InterfaceStatus {
            available,
            enabled: available, // ethernet is always enabled when available
            mac,
            ip,
            connected,
        }
    };

    // WiFi STA
    let wifi_sta = if HAS_WIFI {
        let mac = get_netif_mac(b"WIFI_STA_DEF\0");
        let ip = get_netif_ip(b"WIFI_STA_DEF\0");
        let available = !mac.is_empty();
        let connected = !ip.is_empty();
        let enabled = settings.wifi_client_enabled && !settings.wifi_ssid.is_empty();
        InterfaceStatus {
            available,
            enabled,
            mac,
            ip,
            connected,
        }
    } else {
        InterfaceStatus {
            available: false,
            enabled: false,
            mac: String::new(),
            ip: String::new(),
            connected: false,
        }
    };

    // WiFi AP
    let wifi_ap = if HAS_WIFI {
        let mac = get_netif_mac(b"WIFI_AP_DEF\0");
        let ip = get_netif_ip(b"WIFI_AP_DEF\0");
        let available = !mac.is_empty();
        let enabled = settings.ap_enabled;

        // Get connected clients
        let clients = if available {
            gather_ap_clients()
        } else {
            Vec::new()
        };

        ApStatus {
            available,
            enabled,
            mac,
            ip,
            clients,
        }
    } else {
        ApStatus {
            available: false,
            enabled: false,
            mac: String::new(),
            ip: String::new(),
            clients: Vec::new(),
        }
    };

    let min_free = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };

    // Enumerate all memory regions
    use esp_idf_svc::sys::*;
    let regions: &[(&str, u32)] = &[
        ("Internal", MALLOC_CAP_INTERNAL),
        ("PSRAM", MALLOC_CAP_SPIRAM),
        ("DMA", MALLOC_CAP_DMA),
        ("RTCRAM", MALLOC_CAP_RTCRAM),
        ("TCM", MALLOC_CAP_TCM),
    ];
    let memory: Vec<MemoryRegion> = regions
        .iter()
        .map(|(name, cap)| {
            let r = mem_region(*cap);
            MemoryRegion {
                name: name.to_string(),
                total_bytes: r.total,
                free_bytes: r.free,
            }
        })
        .filter(|r| r.total_bytes > 0)
        .collect();

    ServerMessage::NetworkStatus {
        board_mac,
        memory,
        min_free_heap_bytes: min_free,
        ethernet,
        wifi_sta,
        wifi_ap,
    }
}

fn gather_ap_clients() -> Vec<ApClientInfo> {
    use esp_idf_svc::sys::*;

    unsafe {
        let mut sta_list: wifi_sta_list_t = core::mem::zeroed();
        if esp_wifi_ap_get_sta_list(&mut sta_list) != ESP_OK {
            return Vec::new();
        }

        (0..sta_list.num as usize)
            .map(|i| {
                let sta = &sta_list.sta[i];
                ApClientInfo {
                    mac: format_mac(&sta.mac),
                    ip: String::new(),
                }
            })
            .collect()
    }
}

// --- picoserve app ---

// SVG favicon: zap emoji on dark background
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#1a1a2e"/><text x="16" y="24" font-size="22" text-anchor="middle">&#x26A1;</text></svg>"##;

pub fn make_app(
) -> picoserve::Router<impl picoserve::routing::PathRouter<ConnectionState>, ConnectionState> {
    picoserve::Router::new()
        .route("/", get_service(FrontendService))
        .route("/favicon.ico", get_service(FaviconService))
        .route("/ota", post_service(OtaService))
        .route(
            "/ws",
            get(|upgrade: ws::WebSocketUpgrade| async move {
                upgrade.on_upgrade_using_state(WsHandler)
            }),
        )
}

struct FrontendService;

impl picoserve::routing::RequestHandlerService<ConnectionState> for FrontendService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &ConnectionState,
        _path_parameters: (),
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let connection = request.body_connection.finalize().await?;
        response_writer
            .write_response(
                connection,
                picoserve::response::Response::new(
                    picoserve::response::StatusCode::OK,
                    FRONTEND_HTML_GZ,
                )
                .with_header("Content-Type", "text/html; charset=utf-8")
                .with_header("Content-Encoding", "gzip")
                .with_header("Cache-Control", "no-store"),
            )
            .await
    }
}

/// Custom Content type for SVG with correct content-type.
struct SvgContent(&'static str);

impl picoserve::response::Content for SvgContent {
    fn content_type(&self) -> &'static str {
        "image/svg+xml"
    }
    fn content_length(&self) -> usize {
        self.0.len()
    }
    async fn write_content<W: picoserve::io::Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0.as_bytes()).await
    }
}

struct FaviconService;

impl picoserve::routing::RequestHandlerService<ConnectionState> for FaviconService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &ConnectionState,
        _path_parameters: (),
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let connection = request.body_connection.finalize().await?;
        response_writer
            .write_response(
                connection,
                picoserve::response::Response::new(
                    picoserve::response::StatusCode::OK,
                    SvgContent(FAVICON_SVG),
                )
                .with_header("Cache-Control", "max-age=86400"),
            )
            .await
    }
}

// --- OTA update handler ---

struct OtaService;

struct TextContent(String);

impl picoserve::response::Content for TextContent {
    fn content_type(&self) -> &'static str {
        "text/plain"
    }
    fn content_length(&self) -> usize {
        self.0.len()
    }
    async fn write_content<W: picoserve::io::Write>(self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0.as_bytes()).await
    }
}

impl picoserve::routing::RequestHandlerService<ConnectionState> for OtaService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &ConnectionState,
        _path_parameters: (),
        mut request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let content_length = request.body_connection.content_length();

        if content_length == 0 {
            let connection = request.body_connection.finalize().await?;
            return response_writer
                .write_response(
                    connection,
                    picoserve::response::Response::new(
                        picoserve::response::StatusCode::BAD_REQUEST,
                        TextContent("Content-Length required".to_string()),
                    ),
                )
                .await;
        }

        info!("OTA upload: {content_length} bytes");

        // Read body with a long timeout (120s for large firmware uploads)
        let result = {
            let body = request.body_connection.body();
            let mut reader =
                body.reader()
                    .with_different_timeout_signal(Box::pin(async_io::Timer::after(
                        Duration::from_secs(120),
                    )));
            ota::perform_update(content_length, &mut reader).await
        };

        let connection = request.body_connection.finalize().await?;

        match result {
            Ok(written) => {
                let msg = format!("OTA OK: {written} bytes written, rebooting...");
                // Schedule reboot after response is sent
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(500));
                    unsafe {
                        esp_idf_svc::sys::esp_restart();
                    }
                });
                response_writer
                    .write_response(
                        connection,
                        picoserve::response::Response::new(
                            picoserve::response::StatusCode::OK,
                            TextContent(msg),
                        ),
                    )
                    .await
            }
            Err(e) => {
                error!("OTA failed: {e:#}");
                response_writer
                    .write_response(
                        connection,
                        picoserve::response::Response::new(
                            picoserve::response::StatusCode::INTERNAL_SERVER_ERROR,
                            TextContent(format!("OTA failed: {e:#}")),
                        ),
                    )
                    .await
            }
        }
    }
}

// --- WebSocket handler ---

struct WsHandler;

impl ws::WebSocketCallbackWithState<ConnectionState> for WsHandler {
    async fn run_with_state<R: picoserve::io::Read, W: picoserve::io::Write<Error = R::Error>>(
        self,
        state: &ConnectionState,
        mut rx: ws::SocketRx<R>,
        mut tx: ws::SocketTx<W>,
    ) -> Result<(), W::Error> {
        let ctx = &state.app;
        let ws_id = state.conn_id;
        let ws_addr = state.conn_addr.clone();
        info!("[#{ws_id}] WebSocket connected from {ws_addr}");

        // Register this client
        ctx.ws_clients
            .lock()
            .unwrap()
            .push((ws_id, ws_addr.clone()));

        let state_json = ctx.state_json();
        tx.send_text(&state_json).await?;
        let remote_status_json = ctx.remote_control_status_json();
        tx.send_text(&remote_status_json).await?;
        let event_log_json = ctx.event_log_state_json();
        tx.send_text(&event_log_json).await?;
        let rf_debug_json = ctx.rf_debug_state_json(false);
        tx.send_text(&rf_debug_json).await?;

        let mut broadcast_rx = ctx.broadcast_tx.new_receiver();
        let mut listening_rf_debug = false;
        let mut buf = vec![0u8; WS_BUF_SIZE];

        loop {
            match rx.next_message(&mut buf, broadcast_rx.recv()).await {
                Ok(Either::First(Ok(Message::Text(text)))) => {
                    if let Err(e) = handle_text_message(
                        ctx,
                        &mut tx,
                        text,
                        &mut listening_rf_debug,
                        ActionOwner::LocalWs(ws_id),
                    )
                    .await
                    {
                        warn!("WS handler error: {e:#}");
                        break;
                    }
                }
                Ok(Either::First(Ok(Message::Binary(_)))) => {}
                Ok(Either::First(Ok(Message::Ping(data)))) => {
                    tx.send_pong(data).await?;
                }
                Ok(Either::First(Ok(Message::Pong(_)))) => {}
                Ok(Either::First(Ok(Message::Close(_)))) => break,
                Ok(Either::First(Err(e))) => {
                    warn!("WS read error: {e:?}");
                    break;
                }
                Ok(Either::Second(Ok(msg))) => {
                    if msg.rf_debug && !listening_rf_debug {
                        continue;
                    }
                    tx.send_text(&msg.json).await?;
                }
                Ok(Either::Second(Err(_))) => break,
                Err(_) => break,
            }
        }

        if listening_rf_debug {
            let prev = ctx.rf_debug_listener_count.fetch_sub(1, Ordering::SeqCst);
            if prev <= 1 {
                ctx.rf_debug_enabled.store(false, Ordering::SeqCst);
            }
        }
        cancel_owned_manual_actions(ctx, ActionOwner::LocalWs(ws_id));

        // Deregister this client
        ctx.ws_clients
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != ws_id);

        info!("[#{ws_id}] WebSocket disconnected from {ws_addr}");
        Ok(())
    }
}

pub(crate) fn pong_json(ctx: &AppCtx, nonce: u32) -> String {
    let client_ips: Vec<String> = ctx
        .ws_clients
        .lock()
        .unwrap()
        .iter()
        .map(|(_, addr)| addr.clone())
        .collect();
    serde_json::to_string(&ServerMessage::Pong {
        nonce,
        server_uptime_s: uptime_seconds(),
        free_heap_bytes: free_heap(),
        connected_clients: client_ips.len() as u32,
        client_ips,
    })
    .unwrap()
}

fn resolve_collar_command(
    ctx: &AppCtx,
    collar_name: &str,
    mode: CommandMode,
    intensity: u8,
) -> core::result::Result<(Collar, u8), String> {
    let (collar, lockout) = {
        let d = ctx.domain.lock().unwrap();
        (
            d.collars.iter().find(|c| c.name == collar_name).cloned(),
            rf_lockout_remaining_ms(&d),
        )
    };

    if lockout > 0 {
        return Err("Transmissions locked after STOP".to_string());
    }
    if mode.has_intensity() && intensity > MAX_INTENSITY {
        return Err(format!(
            "Intensity {} exceeds max {}",
            intensity, MAX_INTENSITY
        ));
    }

    let collar = collar.ok_or_else(|| format!("Unknown collar: {collar_name}"))?;
    Ok((collar, command_intensity(mode, intensity)))
}

fn stop_manual_action(ctx: &AppCtx, collar_name: &str, mode: CommandMode) {
    let key = ActionKey {
        collar_name: collar_name.to_string(),
        mode,
    };

    if let Some(handle) = ctx.active_actions.lock().unwrap().remove(&key) {
        handle.cancel.store(false, Ordering::SeqCst);
    }
}

fn cancel_all_manual_actions(ctx: &AppCtx) {
    let handles: Vec<ActiveActionHandle> = ctx
        .active_actions
        .lock()
        .unwrap()
        .drain()
        .map(|(_, handle)| handle)
        .collect();

    for handle in handles {
        handle.cancel.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn cancel_owned_manual_actions(ctx: &AppCtx, owner: ActionOwner) {
    let handles: Vec<ActiveActionHandle> = {
        let mut active_actions = ctx.active_actions.lock().unwrap();
        let keys_to_remove: Vec<ActionKey> = active_actions
            .iter()
            .filter_map(|(key, handle)| {
                if handle.cancel_on_disconnect && handle.owner == Some(owner) {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();

        keys_to_remove
            .into_iter()
            .filter_map(|key| active_actions.remove(&key))
            .collect()
    };

    for handle in handles {
        handle.cancel.store(false, Ordering::SeqCst);
    }
}

fn start_manual_action(
    ctx: &AppCtx,
    collar_name: String,
    mode: CommandMode,
    intensity: u8,
    intensity_max: Option<u8>,
    duration_ms: Option<u32>,
    duration_max_ms: Option<u32>,
    intensity_distribution: Distribution,
    duration_distribution: Distribution,
    source: EventSource,
    owner: Option<ActionOwner>,
    cancel_on_disconnect: bool,
) -> core::result::Result<(), String> {
    let (collar, normalized_intensity) =
        resolve_collar_command(ctx, &collar_name, mode, intensity)?;
    if let Some(duration_ms) = duration_ms {
        if duration_ms == 0 {
            return Err("Action duration must be greater than zero".to_string());
        }
    }
    if cancel_on_disconnect && owner.is_none() {
        return Err("Held actions require an owning connection".to_string());
    }

    let spec = ManualActionSpec {
        key: ActionKey { collar_name, mode },
        collar_id: collar.collar_id,
        channel: collar.channel,
        mode,
        intensity: normalized_intensity,
        intensity_max,
        duration_ms,
        duration_max_ms,
        intensity_distribution,
        duration_distribution,
        source,
    };

    let run_id = ctx.next_action_id.fetch_add(1, Ordering::SeqCst) + 1;
    let cancel = Arc::new(AtomicBool::new(true));

    {
        let mut active_actions = ctx.active_actions.lock().unwrap();
        if let Some(previous) = active_actions.insert(
            spec.key.clone(),
            ActiveActionHandle {
                run_id,
                cancel: cancel.clone(),
                owner,
                cancel_on_disconnect,
            },
        ) {
            previous.cancel.store(false, Ordering::SeqCst);
        }
    }

    let ctx2 = ctx.clone();
    std::thread::Builder::new()
        .name("manual-action".into())
        .stack_size(32768)
        .spawn(move || {
            run_manual_action(spec, &ctx2, run_id, cancel);
        })
        .map_err(|err| format!("Failed to spawn manual action thread: {err}"))?;

    Ok(())
}

fn run_manual_action(spec: ManualActionSpec, ctx: &AppCtx, run_id: u32, cancel: Arc<AtomicBool>) {
    let cleanup_key = spec.key.clone();
    let started_at = Instant::now();

    // Resolve random intensity once at action start
    let actual_intensity = match spec.intensity_max {
        Some(max) if max > spec.intensity && spec.mode.has_intensity() => {
            let mut rng = ctx.rng.lock().unwrap();
            resolve_random_u8(&mut *rng, spec.intensity, max, spec.intensity_distribution)
        }
        _ => spec.intensity,
    };

    // Resolve random duration once at action start
    let actual_duration_ms: Option<u32> = match (spec.duration_ms, spec.duration_max_ms) {
        (Some(min), Some(max)) if max > min => {
            let mut rng = ctx.rng.lock().unwrap();
            Some(resolve_random_duration(&mut *rng, min, max, spec.duration_distribution))
        }
        (dur, _) => dur,
    };

    let deadline = actual_duration_ms
        .map(|duration_ms| started_at + Duration::from_millis(duration_ms as u64));

    'outer: loop {
        if !cancel.load(Ordering::SeqCst) {
            break;
        }

        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                break;
            }
        }

        if let Err(err) = rf_send_with_led(
            &ctx.rf,
            &ctx.tx_led,
            spec.collar_id,
            spec.channel,
            spec.mode.to_rf_byte(),
            actual_intensity,
        ) {
            error!("RF send error during manual action: {err:#}");
        }

        let wait_deadline = Instant::now() + Duration::from_millis(MANUAL_ACTION_REPEAT_MS);
        loop {
            if !cancel.load(Ordering::SeqCst) {
                break 'outer;
            }

            let now = Instant::now();
            let next_tick = match deadline {
                Some(deadline) if deadline < wait_deadline => deadline,
                _ => wait_deadline,
            };

            if now >= next_tick {
                break;
            }

            let sleep_for =
                (next_tick - now).min(Duration::from_millis(MANUAL_ACTION_SLEEP_SLICE_MS));
            std::thread::sleep(sleep_for);
        }
    }

    {
        let mut active_actions = ctx.active_actions.lock().unwrap();
        if active_actions.get(&cleanup_key).map(|handle| handle.run_id) == Some(run_id) {
            active_actions.remove(&cleanup_key);
        }
    }

    let elapsed_ms = started_at.elapsed().as_millis().min(u32::MAX as u128) as u32;
    ctx.record_event(
        spec.source,
        EventLogEntryKind::Action {
            collar_name: spec.key.collar_name,
            mode: spec.mode,
            intensity: if spec.mode.has_intensity() {
                Some(actual_intensity)
            } else {
                None
            },
            duration_ms: elapsed_ms,
        },
    );
}

pub(crate) fn process_control_message(
    ctx: &AppCtx,
    msg: ClientMessage,
    origin: MessageOrigin,
    owner: Option<ActionOwner>,
) -> ControlResult {
    match msg {
        ClientMessage::Command {
            collar_name,
            mode,
            intensity,
        } => {
            let (collar, intensity) = resolve_collar_command(ctx, &collar_name, mode, intensity)?;
            if let Err(err) = rf_send_with_led(
                &ctx.rf,
                &ctx.tx_led,
                collar.collar_id,
                collar.channel,
                mode.to_rf_byte(),
                intensity,
            ) {
                error!("RF send error: {err:#}");
            }

            Ok(Vec::new())
        }

        ClientMessage::ButtonEvent {
            collar_name,
            mode,
            intensity,
            action,
        } => {
            if cfg!(debug_assertions) {
                info!(
                    "Button {:?}: collar={collar_name} mode={mode:?} intensity={intensity}",
                    action
                );
            }

            Ok(Vec::new())
        }

        ClientMessage::RunAction {
            collar_name,
            mode,
            intensity,
            duration_ms,
            intensity_max,
            duration_max_ms,
            intensity_distribution,
            duration_distribution,
        } => {
            start_manual_action(
                ctx,
                collar_name,
                mode,
                intensity,
                intensity_max,
                Some(duration_ms),
                duration_max_ms,
                intensity_distribution.unwrap_or_default(),
                duration_distribution.unwrap_or_default(),
                event_source(origin),
                owner,
                false,
            )?;
            Ok(Vec::new())
        }

        ClientMessage::StartAction {
            collar_name,
            mode,
            intensity,
            intensity_max,
            intensity_distribution,
        } => {
            start_manual_action(
                ctx,
                collar_name,
                mode,
                intensity,
                intensity_max,
                None,
                None,
                intensity_distribution.unwrap_or_default(),
                Distribution::default(),
                event_source(origin),
                owner,
                true,
            )?;
            Ok(Vec::new())
        }

        ClientMessage::StopAction { collar_name, mode } => {
            stop_manual_action(ctx, &collar_name, mode);
            Ok(Vec::new())
        }

        ClientMessage::AddCollar {
            name,
            collar_id,
            channel,
        } => {
            let collar = Collar {
                name,
                collar_id,
                channel,
            };
            let collars = {
                let mut d = ctx.domain.lock().unwrap();
                validation::validate_collar(&collar).map_err(|err| err.to_string())?;
                if d.collars
                    .iter()
                    .any(|existing| existing.name == collar.name)
                {
                    return Err(format!("Collar '{}' already exists", collar.name));
                }
                d.collars.push(collar);
                d.collars.clone()
            };
            log_storage_result("save_collars", save_collars(ctx, &collars));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::UpdateCollar {
            original_name,
            name,
            collar_id,
            channel,
        } => {
            let updated = Collar {
                name,
                collar_id,
                channel,
            };
            let (collars, presets) = {
                let mut d = ctx.domain.lock().unwrap();
                let Some(idx) = d.collars.iter().position(|c| c.name == original_name) else {
                    return Err(format!("Unknown collar: {original_name}"));
                };
                validation::validate_collar(&updated).map_err(|err| err.to_string())?;
                if d.collars
                    .iter()
                    .enumerate()
                    .any(|(i, collar)| i != idx && collar.name == updated.name)
                {
                    return Err(format!("Collar '{}' already exists", updated.name));
                }
                d.collars[idx] = updated.clone();
                if original_name != updated.name {
                    for preset in &mut d.presets {
                        for track in &mut preset.tracks {
                            if track.collar_name == original_name {
                                track.collar_name = updated.name.clone();
                            }
                        }
                    }
                }
                stop_active_preset(&mut d, &ctx.preset_run_id);
                (d.collars.clone(), d.presets.clone())
            };
            cancel_all_manual_actions(ctx);
            log_storage_result("save_collars", save_collars(ctx, &collars));
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::DeleteCollar { name } => {
            let collars = {
                let mut d = ctx.domain.lock().unwrap();
                if d.presets
                    .iter()
                    .any(|preset| preset.tracks.iter().any(|track| track.collar_name == name))
                {
                    return Err(format!("Cannot delete '{name}': presets reference it"));
                }
                let before = d.collars.len();
                d.collars.retain(|collar| collar.name != name);
                if d.collars.len() == before {
                    return Err(format!("Unknown collar: {name}"));
                }
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.collars.clone()
            };
            cancel_all_manual_actions(ctx);
            log_storage_result("save_collars", save_collars(ctx, &collars));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::SavePreset {
            original_name,
            mut preset,
        } => {
            preset.normalize();
            let presets = {
                let mut d = ctx.domain.lock().unwrap();
                validation::validate_preset(&preset, &d.collars).map_err(|err| err.to_string())?;
                let original_name = original_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty());
                let mut updated = d.presets.clone();
                if let Some(original_name) = original_name {
                    let Some(idx) = updated
                        .iter()
                        .position(|existing| existing.name == original_name)
                    else {
                        return Err(format!("Unknown preset: {original_name}"));
                    };
                    if updated
                        .iter()
                        .enumerate()
                        .any(|(i, existing)| i != idx && existing.name == preset.name)
                    {
                        return Err(format!("Preset '{}' already exists", preset.name));
                    }
                    updated[idx] = preset;
                } else if let Some(existing) = updated
                    .iter_mut()
                    .find(|existing| existing.name == preset.name)
                {
                    *existing = preset;
                } else {
                    updated.push(preset);
                }
                validation::validate_presets(&updated, &d.collars)
                    .map_err(|err| err.to_string())?;
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.presets = updated;
                d.presets.clone()
            };
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::Ping { nonce } => Ok(vec![pong_json(ctx, nonce)]),

        ClientMessage::DeletePreset { name } => {
            let presets = {
                let mut d = ctx.domain.lock().unwrap();
                let before = d.presets.len();
                d.presets.retain(|preset| preset.name != name);
                if d.presets.len() == before {
                    return Err(format!("Unknown preset: {name}"));
                }
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.presets.clone()
            };
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::RunPreset { name } => {
            let source = event_source(origin);
            let (preset_name, resolved_preset_for_log, events, run_id) = {
                let mut d = ctx.domain.lock().unwrap();
                if rf_lockout_remaining_ms(&d) > 0 {
                    return Err("Transmissions locked after STOP".to_string());
                }
                let Some(preset) = d.presets.iter().find(|preset| preset.name == name).cloned()
                else {
                    return Err(format!("Unknown preset: {name}"));
                };
                // Validate with midpoint values (ensures minimum durations are schedulable)
                validation::validate_preset(&preset, &d.collars)
                    .map_err(|err| err.to_string())?;
                // Resolve random ranges into a concrete preset copy, then schedule from that
                let has_random = preset
                    .tracks
                    .iter()
                    .any(|t| t.steps.iter().any(|s| s.has_random()));
                let mut rng = ctx.rng.lock().unwrap();
                let mut resolver = RandomResolver { rng: &mut *rng };
                let resolved = scheduling::resolve_preset(&preset, &mut resolver);
                // Schedule the resolved (concrete) preset — no ranges left, MidpointResolver is fine
                let events =
                    scheduling::schedule_preset_events(&resolved, &d.collars, &mut scheduling::MidpointResolver)
                        .map_err(|err| err.to_string())?;
                let resolved_for_log = if has_random {
                    Some(resolved)
                } else {
                    None
                };
                let run_id = ctx.preset_run_id.fetch_add(1, Ordering::SeqCst) + 1;
                d.preset_name = Some(name.clone());
                (preset.name.clone(), resolved_for_log, events, run_id)
            };

            let ctx2 = ctx.clone();
            let preset_name_for_thread = preset_name.clone();
            std::thread::Builder::new()
                .name("preset".into())
                .stack_size(32768)
                .spawn(move || {
                    run_preset(&preset_name_for_thread, events, &ctx2, run_id);
                    if ctx2.preset_run_id.load(Ordering::SeqCst) == run_id {
                        let mut d = ctx2.domain.lock().unwrap();
                        if d.preset_name.as_deref() == Some(preset_name_for_thread.as_str()) {
                            d.preset_name = None;
                        }
                    }
                    ctx2.broadcast_state();
                })
                .map_err(|err| {
                    rollback_failed_preset_start(ctx, &preset_name, run_id);
                    format!("Failed to spawn preset thread: {err}")
                })?;

            ctx.record_event(
                source,
                EventLogEntryKind::PresetRun {
                    preset_name: preset_name.clone(),
                    resolved_preset: resolved_preset_for_log,
                },
            );

            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::StopPreset => {
            {
                let mut d = ctx.domain.lock().unwrap();
                ctx.preset_run_id.fetch_add(1, Ordering::SeqCst);
                d.preset_name = None;
            }
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::StopAll => {
            {
                let mut d = ctx.domain.lock().unwrap();
                stop_all_transmissions(&mut d, &ctx.preset_run_id);
            }
            cancel_all_manual_actions(ctx);
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::StartRfDebug => {
            Err("RF debug control is only available on the local UI".to_string())
        }
        ClientMessage::StopRfDebug => {
            Err("RF debug control is only available on the local UI".to_string())
        }
        ClientMessage::ClearRfDebug => {
            Err("RF debug control is only available on the local UI".to_string())
        }
        ClientMessage::Reboot => Err("Device reboot is only available on the local UI".to_string()),

        ClientMessage::GetDeviceSettings => {
            if origin == MessageOrigin::RemoteControl {
                return Err("get_device_settings is not available over remote control".to_string());
            }

            let settings = ctx.domain.lock().unwrap().device_settings.clone();
            Ok(vec![serde_json::to_string(
                &ServerMessage::DeviceSettings {
                    settings,
                    reboot_required: false,
                    has_wifi: HAS_WIFI,
                },
            )
            .unwrap()])
        }

        ClientMessage::GetNetworkStatus => {
            if origin == MessageOrigin::RemoteControl {
                return Err("get_network_status is not available over remote control".to_string());
            }

            let settings = ctx.domain.lock().unwrap().device_settings.clone();
            let status = gather_network_status(&settings);
            Ok(vec![serde_json::to_string(&status).unwrap()])
        }

        ClientMessage::SaveDeviceSettings { mut settings } => {
            if origin == MessageOrigin::RemoteControl {
                return Err("save_device_settings is not available over remote control".to_string());
            }

            // Preserve device_id from current settings if the incoming payload
            // doesn't provide one (e.g. older frontend that doesn't know about it).
            if settings.device_id.is_empty() {
                settings.device_id = ctx.domain.lock().unwrap().device_settings.device_id.clone();
            }

            settings.ntp_server = settings.ntp_server.trim().to_string();
            settings.remote_control_url = settings.remote_control_url.trim().to_string();
            if settings.ntp_enabled && settings.ntp_server.is_empty() {
                return Err("NTP server cannot be empty when time sync is enabled".to_string());
            }
            if settings.remote_control_enabled {
                parse_remote_control_url(&settings.remote_control_url)?;
            }

            info!("Saving device settings...");
            let settings_to_save = settings.clone();
            let (reboot_required, remote_settings_changed, event_log_changed) = {
                let mut d = ctx.domain.lock().unwrap();
                let previous_settings = d.device_settings.clone();
                let reboot_required =
                    device_settings_reboot_required(&previous_settings, &settings);
                let remote_settings_changed = previous_settings.remote_control_enabled
                    != settings.remote_control_enabled
                    || previous_settings.remote_control_url != settings.remote_control_url
                    || previous_settings.remote_control_validate_cert
                        != settings.remote_control_validate_cert;
                let event_log_changed =
                    previous_settings.record_event_log != settings.record_event_log;

                d.device_settings = settings;
                if remote_settings_changed {
                    d.remote_control_status =
                        remote_control_status_from_settings(&d.device_settings);
                }
                if !d.device_settings.record_event_log {
                    d.event_log_events.clear();
                }

                (reboot_required, remote_settings_changed, event_log_changed)
            };

            if remote_settings_changed {
                ctx.remote_control_settings_revision
                    .fetch_add(1, Ordering::SeqCst);
            }

            match save_settings(ctx, &settings_to_save) {
                Ok(()) => info!("Device settings saved to NVS"),
                Err(err) => error!("NVS save_settings failed: {err:#}"),
            }

            if remote_settings_changed {
                ctx.broadcast_remote_control_status();
            }
            if event_log_changed {
                ctx.broadcast_event_log_state();
            }

            Ok(vec![serde_json::to_string(
                &ServerMessage::DeviceSettings {
                    settings: settings_to_save,
                    reboot_required,
                    has_wifi: HAS_WIFI,
                },
            )
            .unwrap()])
        }

        ClientMessage::PreviewPreset { nonce, mut preset } => {
            preset.normalize();
            let collars = ctx.domain.lock().unwrap().collars.clone();
            let msg = match validation::validate_preset(&preset, &collars) {
                Ok(()) => match scheduling::preview_preset(&preset, &collars) {
                    Ok(preview) => ServerMessage::PresetPreview {
                        nonce,
                        preview: Some(preview),
                        error: None,
                    },
                    Err(err) => ServerMessage::PresetPreview {
                        nonce,
                        preview: None,
                        error: Some(err.to_string()),
                    },
                },
                Err(err) => ServerMessage::PresetPreview {
                    nonce,
                    preview: None,
                    error: Some(err.to_string()),
                },
            };
            Ok(vec![serde_json::to_string(&msg).unwrap()])
        }

        ClientMessage::ReorderPresets { names } => {
            let presets = {
                let mut d = ctx.domain.lock().unwrap();
                let order_by_name: HashMap<&str, usize> = names
                    .iter()
                    .enumerate()
                    .map(|(idx, name)| (name.as_str(), idx))
                    .collect();
                let mut reordered_slots = vec![None; names.len()];
                let mut remaining = Vec::with_capacity(d.presets.len());
                for preset in d.presets.drain(..) {
                    match order_by_name.get(preset.name.as_str()) {
                        Some(&idx) if reordered_slots[idx].is_none() => {
                            reordered_slots[idx] = Some(preset);
                        }
                        _ => remaining.push(preset),
                    }
                }
                let mut reordered = Vec::with_capacity(remaining.len() + names.len());
                reordered.extend(reordered_slots.into_iter().flatten());
                reordered.extend(remaining);
                d.presets = reordered;
                d.presets.clone()
            };
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }

        ClientMessage::Export => {
            if origin == MessageOrigin::RemoteControl {
                return Err("export is not available over remote control".to_string());
            }

            let d = ctx.domain.lock().unwrap();
            let mut data = ExportData {
                collars: d.collars.clone(),
                presets: d.presets.clone(),
            };
            drop(d);
            for preset in &mut data.presets {
                preset.normalize();
            }
            Ok(vec![serde_json::to_string(&ServerMessage::ExportData {
                data: &data,
            })
            .unwrap()])
        }

        ClientMessage::Import { mut data } => {
            if origin == MessageOrigin::RemoteControl {
                return Err("import is not available over remote control".to_string());
            }

            for preset in &mut data.presets {
                preset.normalize();
            }
            validation::validate_export_data(&data).map_err(|err| err.to_string())?;
            let (collars, presets) = {
                let mut d = ctx.domain.lock().unwrap();
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.collars = data.collars;
                d.presets = data.presets;
                (d.collars.clone(), d.presets.clone())
            };
            cancel_all_manual_actions(ctx);
            log_storage_result("save_collars", save_collars(ctx, &collars));
            log_storage_result("save_presets", save_presets(ctx, &presets));
            ctx.broadcast_state();
            Ok(Vec::new())
        }
    }
}

async fn handle_text_message<W: picoserve::io::Write>(
    ctx: &AppCtx,
    tx: &mut ws::SocketTx<W>,
    text: &str,
    listening_rf_debug: &mut bool,
    owner: ActionOwner,
) -> Result<(), W::Error> {
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(msg) => msg,
        Err(err) => {
            warn!("Invalid WS message: {err}");
            let _ = send_error(tx, format!("Invalid message: {err}")).await;
            return Ok(());
        }
    };

    match msg {
        ClientMessage::StartRfDebug => {
            *listening_rf_debug = true;
            ctx.rf_debug_listener_count.fetch_add(1, Ordering::SeqCst);
            ctx.rf_debug_enabled.store(true, Ordering::SeqCst);
            ensure_rf_debug_worker(ctx);
            let json = ctx.rf_debug_state_json(true);
            tx.send_text(&json).await?;
        }

        ClientMessage::StopRfDebug => {
            if *listening_rf_debug {
                *listening_rf_debug = false;
                let prev = ctx.rf_debug_listener_count.fetch_sub(1, Ordering::SeqCst);
                if prev <= 1 {
                    ctx.rf_debug_enabled.store(false, Ordering::SeqCst);
                }
            }
            let json = ctx.rf_debug_state_json(false);
            tx.send_text(&json).await?;
        }

        ClientMessage::ClearRfDebug => {
            ctx.domain.lock().unwrap().rf_debug_events.clear();
            let json = ctx.rf_debug_state_json(*listening_rf_debug);
            tx.send_text(&json).await?;
        }

        ClientMessage::Reboot => {
            info!("Reboot requested via WebSocket");
            tx.send_text(r#"{"type":"state","rebooting":true}"#).await?;
            async_io::Timer::after(Duration::from_millis(200)).await;
            unsafe {
                esp_idf_svc::sys::esp_restart();
            }
        }

        msg => match process_control_message(ctx, msg, MessageOrigin::LocalUi, Some(owner)) {
            Ok(messages) => {
                for message in messages {
                    tx.send_text(&message).await?;
                }
            }
            Err(message) => {
                send_error(tx, message).await?;
            }
        },
    }

    Ok(())
}

async fn send_error<W: picoserve::io::Write>(
    tx: &mut ws::SocketTx<W>,
    message: impl Into<String>,
) -> Result<(), W::Error> {
    let msg = serde_json::to_string(&ServerMessage::Error {
        message: message.into(),
    })
    .unwrap();
    tx.send_text(&msg).await
}

// --- Preset execution (runs on std::thread, not async) ---

fn run_preset(preset_name: &str, events: Vec<PresetEvent>, ctx: &AppCtx, run_id: u32) {
    let start = Instant::now();
    for event in &events {
        if ctx.preset_run_id.load(Ordering::SeqCst) != run_id {
            return;
        }
        let target = Duration::from_micros(event.time_us);
        let elapsed = start.elapsed();
        if target > elapsed {
            let wait = target - elapsed;
            let chunks = wait.as_millis() as u64 / 50;
            for _ in 0..chunks {
                if ctx.preset_run_id.load(Ordering::SeqCst) != run_id {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let remainder = wait - Duration::from_millis(chunks * 50);
            if !remainder.is_zero() {
                std::thread::sleep(remainder);
            }
        }
        if ctx.preset_run_id.load(Ordering::SeqCst) != run_id {
            return;
        }
        if let Err(e) = rf_send_with_led(
            &ctx.rf,
            &ctx.tx_led,
            event.collar_id,
            event.channel,
            event.mode_byte,
            event.intensity,
        ) {
            error!("RF error during preset: {e}");
        }
    }
    info!("Preset '{}' completed", preset_name);
}

/// Spawn the RF debug worker lazily on first use. The worker stays alive and
/// idles when debug listening is disabled, which avoids races when listeners
/// stop and restart quickly.
fn ensure_rf_debug_worker(ctx: &AppCtx) {
    if ctx
        .rf_debug_worker_spawned
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let worker_ctx = ctx.clone();
    let result = std::thread::Builder::new()
        .name("rf-debug-rx".into())
        .stack_size(16384)
        .spawn(move || {
            let Some(mut receiver) = worker_ctx.rf_receiver.lock().unwrap().take() else {
                worker_ctx
                    .rf_debug_worker_spawned
                    .store(false, Ordering::SeqCst);
                error!("RF debug receiver missing when worker started");
                return;
            };
            info!("RF debug worker started");
            loop {
                if !worker_ctx.rf_debug_enabled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(RF_DEBUG_DISABLED_SLEEP_MS));
                    continue;
                }
                match receiver.listen_until_disabled(&worker_ctx.rf_debug_enabled) {
                    Ok(Some(event)) => {
                        worker_ctx.rx_led.lock().unwrap().set(true);
                        {
                            let mut d = worker_ctx.domain.lock().unwrap();
                            d.rf_debug_events.push_back(event.clone());
                            if d.rf_debug_events.len() > MAX_RF_DEBUG_EVENTS {
                                d.rf_debug_events.pop_front();
                            }
                        }
                        let json =
                            serde_json::to_string(&ServerMessage::RfDebugEvent { event: &event })
                                .unwrap();
                        let _ = worker_ctx.broadcast_tx.try_broadcast(BroadcastMsg {
                            json: Arc::from(json),
                            rf_debug: true,
                        });
                        // Keep LED visible for a short time
                        std::thread::sleep(Duration::from_millis(50));
                        worker_ctx.rx_led.lock().unwrap().set(false);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        error!("RF debug receiver error: {err:#}");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        });
    if let Err(e) = result {
        ctx.rf_debug_worker_spawned.store(false, Ordering::SeqCst);
        error!("Failed to spawn RF debug worker: {e}");
    }
}

// --- Server startup ---

pub fn run_server(ctx: AppCtx) -> Result<()> {
    let max_clients = ctx.domain.lock().unwrap().device_settings.max_clients as u32;
    let app_ctx = ctx;
    let base_app = make_app();
    let shared_app = base_app.shared();

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: picoserve::time::Duration::from_secs(5),
        persistent_start_read_request: picoserve::time::Duration::from_secs(5),
        read_request: picoserve::time::Duration::from_secs(1),
        write: picoserve::time::Duration::from_secs(1),
    })
    .close_connection_after_response();

    let ex = async_executor::LocalExecutor::new();
    let active = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let next_conn_id = std::rc::Rc::new(std::cell::Cell::new(1u32));

    futures_lite::future::block_on(ex.run(async {
        let listener = async_io::Async::<std::net::TcpListener>::bind(([0, 0, 0, 0], 80))
            .expect("failed to bind port 80");
        info!("picoserve listening on port 80 (max {max_clients} concurrent)");

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let count = active.get();
                    if count >= max_clients {
                        warn!("Rejecting {addr}: at capacity ({count}/{max_clients})");
                        drop(stream);
                        continue;
                    }

                    let conn_id = next_conn_id.get();
                    next_conn_id.set(conn_id + 1);
                    let free_heap = free_heap();
                    info!("[#{conn_id}] Connection from {addr} ({count}/{max_clients}, heap: {free_heap}B)");

                    let config_ref = &config;
                    let active_ref = active.clone();
                    active_ref.set(active_ref.get() + 1);
                    let conn_state = ConnectionState {
                        app: app_ctx.clone(),
                        conn_id,
                        conn_addr: addr.ip().to_string(),
                    };

                    ex.spawn(async move {
                        let app = shared_app.with_state(conn_state);
                        let socket = AsyncIoSocket(stream);
                        let mut http_buf = vec![0u8; HTTP_BUF_SIZE];
                        let server = picoserve::Server::custom(
                            &app, AsyncIoTimer, config_ref, &mut http_buf,
                        );
                        match server.serve(socket).await {
                            Ok(_) => info!("[#{conn_id}] Connection from {addr} closed"),
                            Err(e) => warn!("[#{conn_id}] Connection from {addr} error: {e:?}"),
                        }
                        active_ref.set(active_ref.get() - 1);
                    })
                    .detach();
                }
                Err(e) => {
                    error!("Accept error: {e}");
                    async_io::Timer::after(Duration::from_millis(100)).await;
                }
            }
        }
    }))
}
