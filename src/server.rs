use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_broadcast::Sender as BroadcastSender;
use picoserve::futures::Either;
use log::{error, info, warn};
use picoserve::response::ws::{self, Message};
use picoserve::routing::{get, get_service, post_service};

use crate::ota;

use crate::async_runtime::{AsyncIoSocket, AsyncIoTimer};
use crate::build_info::APP_VERSION;
use crate::led::Led;
use crate::protocol::{self, DeviceSettings, *};
use crate::rf::{RfReceiver, RfTransmitter};
use crate::scheduling::{self, PresetEvent};
use crate::storage::Storage;
use crate::validation;

const FRONTEND_HTML_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/frontend.html.gz"));
const MAX_RF_DEBUG_EVENTS: usize = 100;
const RF_STOP_LOCKOUT_MS: u64 = 10_000;
const HTTP_BUF_SIZE: usize = 1024;
const WS_BUF_SIZE: usize = 2048;

#[derive(Clone)]
pub struct BroadcastMsg {
    pub json: Arc<str>,
    pub rf_debug: bool,
}

// --- Shared state ---

pub struct DomainState {
    pub device_settings: DeviceSettings,
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
    pub preset_name: Option<String>,
    pub rf_lockout_until_ms: u64,
    pub rf_debug_events: Vec<RfDebugFrame>,
    pub storage: Storage,
}

#[derive(Clone)]
pub struct AppCtx {
    pub domain: Arc<Mutex<DomainState>>,
    pub rf: Arc<Mutex<RfTransmitter>>,
    pub tx_led: Arc<Mutex<Led>>,
    pub rx_led: Arc<Mutex<Led>>,
    pub broadcast_tx: BroadcastSender<BroadcastMsg>,
    pub rf_debug_enabled: Arc<AtomicBool>,
    pub rf_debug_listener_count: Arc<AtomicU32>,
    pub rf_receiver: Arc<Mutex<Option<RfReceiver>>>,
    pub preset_run_id: Arc<AtomicU32>,
    /// Monotonic connection ID, set before each serve() call. Single-threaded, so no race.
    pub last_conn_id: Arc<AtomicU32>,
    /// IP addresses of last accepted connection (set before serve, read by WS handler).
    pub last_conn_addr: Arc<Mutex<String>>,
    /// Active WS client addresses, keyed by conn_id.
    pub ws_clients: Arc<Mutex<Vec<(u32, String)>>>,
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
        Self {
            domain: Arc::new(Mutex::new(DomainState {
                device_settings,
                collars,
                presets,
                preset_name: None,
                rf_lockout_until_ms: 0,
                rf_debug_events: Vec::new(),
                storage,
            })),
            rf,
            tx_led,
            rx_led,
            broadcast_tx,
            rf_debug_enabled: Arc::new(AtomicBool::new(false)),
            rf_debug_listener_count: Arc::new(AtomicU32::new(0)),
            rf_receiver: Arc::new(Mutex::new(Some(rf_receiver))),
            preset_run_id: Arc::new(AtomicU32::new(0)),
            last_conn_id: Arc::new(AtomicU32::new(0)),
            last_conn_addr: Arc::new(Mutex::new(String::new())),
            ws_clients: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn broadcast_state(&self) {
        let _ = self.broadcast_tx.try_broadcast(BroadcastMsg {
            json: self.state_json(),
            rf_debug: false,
        });
    }

    fn state_json(&self) -> Arc<str> {
        let d = self.domain.lock().unwrap();
        let msg = ServerMessage::State {
            app_version: APP_VERSION,
            server_uptime_s: uptime_seconds(),
            collars: &d.collars,
            presets: &d.presets,
            preset_running: d.preset_name.as_deref(),
            rf_lockout_remaining_ms: rf_lockout_remaining_ms(&d),
        };
        Arc::from(serde_json::to_string(&msg).unwrap())
    }

    fn rf_debug_state_json(&self, listening: bool) -> Arc<str> {
        let d = self.domain.lock().unwrap();
        let msg = ServerMessage::RfDebugState {
            listening,
            events: &d.rf_debug_events,
        };
        Arc::from(serde_json::to_string(&msg).unwrap())
    }
}

fn rf_lockout_remaining_ms(d: &DomainState) -> u64 {
    d.rf_lockout_until_ms.saturating_sub(now_millis())
}

fn now_millis() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1000 }
}

fn uptime_seconds() -> u64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() as u64 / 1_000_000 }
}

fn free_heap() -> u32 {
    unsafe { esp_idf_svc::sys::esp_get_free_heap_size() }
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

fn rf_send_with_led(
    rf: &Mutex<RfTransmitter>,
    tx_led: &Mutex<Led>,
    collar_id: u16,
    channel: u8,
    mode_byte: u8,
    intensity: u8,
) -> Result<()> {
    tx_led.lock().unwrap().set(true);
    let result = rf.lock().unwrap().send_command(collar_id, channel, mode_byte, intensity);
    tx_led.lock().unwrap().set(false);
    result.map_err(Into::into)
}

// --- picoserve app ---

// SVG favicon: zap emoji on dark background
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#1a1a2e"/><text x="16" y="24" font-size="22" text-anchor="middle">&#x26A1;</text></svg>"##;

pub fn make_app() -> picoserve::Router<impl picoserve::routing::PathRouter<AppCtx>, AppCtx> {
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

impl picoserve::routing::RequestHandlerService<AppCtx> for FrontendService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &AppCtx,
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

impl picoserve::routing::RequestHandlerService<AppCtx> for FaviconService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &AppCtx,
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

impl picoserve::routing::RequestHandlerService<AppCtx> for OtaService {
    async fn call_request_handler_service<
        R: picoserve::io::Read,
        W: picoserve::response::ResponseWriter<Error = R::Error>,
    >(
        &self,
        _state: &AppCtx,
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
            let mut reader = body
                .reader()
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
                    unsafe { esp_idf_svc::sys::esp_restart(); }
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

impl ws::WebSocketCallbackWithState<AppCtx> for WsHandler {
    async fn run_with_state<
        R: picoserve::io::Read,
        W: picoserve::io::Write<Error = R::Error>,
    >(
        self,
        ctx: &AppCtx,
        mut rx: ws::SocketRx<R>,
        mut tx: ws::SocketTx<W>,
    ) -> Result<(), W::Error> {
        let ws_id = ctx.last_conn_id.load(Ordering::Relaxed);
        let ws_addr = ctx.last_conn_addr.lock().unwrap().clone();
        info!("[#{ws_id}] WebSocket connected from {ws_addr}");

        // Register this client
        ctx.ws_clients.lock().unwrap().push((ws_id, ws_addr.clone()));

        let state_json = ctx.state_json();
        tx.send_text(&state_json).await?;
        let rf_debug_json = ctx.rf_debug_state_json(false);
        tx.send_text(&rf_debug_json).await?;

        let mut broadcast_rx = ctx.broadcast_tx.new_receiver();
        let mut listening_rf_debug = false;
        let mut buf = vec![0u8; WS_BUF_SIZE];

        loop {
            match rx.next_message(&mut buf, broadcast_rx.recv()).await {
                Ok(Either::First(Ok(Message::Text(text)))) => {
                    if let Err(e) = handle_text_message(ctx, &mut tx, text, &mut listening_rf_debug).await {
                        warn!("WS handler error: {e:#}");
                        break;
                    }
                }
                Ok(Either::First(Ok(Message::Binary(_)))) => {}
                Ok(Either::First(Ok(Message::Ping(data)))) => { tx.send_pong(data).await?; }
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

        // Deregister this client
        ctx.ws_clients.lock().unwrap().retain(|(id, _)| *id != ws_id);

        info!("[#{ws_id}] WebSocket disconnected from {ws_addr}");
        Ok(())
    }
}

async fn handle_text_message<W: picoserve::io::Write>(
    ctx: &AppCtx,
    tx: &mut ws::SocketTx<W>,
    text: &str,
    listening_rf_debug: &mut bool,
) -> Result<(), W::Error> {
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            warn!("Invalid WS message: {e}");
            let _ = send_error(tx, format!("Invalid message: {e}")).await;
            return Ok(());
        }
    };

    match msg {
        ClientMessage::Command { collar_name, mode, intensity } => {
            let (collar, lockout) = {
                let d = ctx.domain.lock().unwrap();
                (d.collars.iter().find(|c| c.name == collar_name).cloned(), rf_lockout_remaining_ms(&d))
            };
            if lockout > 0 { return Ok(()); }
            if intensity > protocol::MAX_INTENSITY {
                send_error(tx, format!("Intensity {} exceeds max {}", intensity, protocol::MAX_INTENSITY)).await?;
                return Ok(());
            }
            match collar {
                Some(c) => {
                    if let Err(e) = rf_send_with_led(&ctx.rf, &ctx.tx_led, c.collar_id, c.channel, mode.to_rf_byte(), intensity) { error!("RF send error: {e:#}"); }
                }
                None => send_error(tx, format!("Unknown collar: {collar_name}")).await?,
            }
        }

        ClientMessage::ButtonEvent { collar_name, mode, intensity, action } => {
            if cfg!(debug_assertions) {
                info!("Button {:?}: collar={collar_name} mode={mode:?} intensity={intensity}", action);
            }
        }

        ClientMessage::AddCollar { name, collar_id, channel } => {
            let collar = Collar { name, collar_id, channel };
            {
                let mut d = ctx.domain.lock().unwrap();
                if let Err(e) = validation::validate_collar(&collar) {
                    drop(d);
                    send_error(tx, e.to_string()).await?;
                    return Ok(());
                }
                if d.collars.iter().any(|c| c.name == collar.name) {
                    drop(d);
                    send_error(tx, format!("Collar '{}' already exists", collar.name)).await?;
                    return Ok(());
                }
                d.collars.push(collar);
                d.storage.save_collars(&d.collars).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::UpdateCollar { original_name, name, collar_id, channel } => {
            let updated = Collar { name, collar_id, channel };
            {
                let mut d = ctx.domain.lock().unwrap();
                let Some(idx) = d.collars.iter().position(|c| c.name == original_name) else {
                    drop(d);
                    send_error(tx, format!("Unknown collar: {original_name}")).await?;
                    return Ok(());
                };
                if let Err(e) = validation::validate_collar(&updated) {
                    drop(d);
                    send_error(tx, e.to_string()).await?;
                    return Ok(());
                }
                if d.collars.iter().enumerate().any(|(i, c)| i != idx && c.name == updated.name) {
                    drop(d);
                    send_error(tx, format!("Collar '{}' already exists", updated.name)).await?;
                    return Ok(());
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
                d.storage.save_collars(&d.collars).ok();
                d.storage.save_presets(&d.presets).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::DeleteCollar { name } => {
            {
                let mut d = ctx.domain.lock().unwrap();
                if d.presets.iter().any(|p| p.tracks.iter().any(|t| t.collar_name == name)) {
                    drop(d);
                    send_error(tx, format!("Cannot delete '{name}': presets reference it")).await?;
                    return Ok(());
                }
                let before = d.collars.len();
                d.collars.retain(|c| c.name != name);
                if d.collars.len() == before {
                    drop(d);
                    send_error(tx, format!("Unknown collar: {name}")).await?;
                    return Ok(());
                }
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.storage.save_collars(&d.collars).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::SavePreset { original_name, mut preset } => {
            preset.normalize();
            {
                let mut d = ctx.domain.lock().unwrap();
                if let Err(e) = validation::validate_preset(&preset, &d.collars) {
                    drop(d);
                    send_error(tx, e.to_string()).await?;
                    return Ok(());
                }
                let orig = original_name.as_deref().map(str::trim).filter(|n| !n.is_empty());
                let mut updated = d.presets.clone();
                if let Some(orig) = orig {
                    let Some(idx) = updated.iter().position(|p| p.name == orig) else {
                        drop(d);
                        send_error(tx, format!("Unknown preset: {orig}")).await?;
                        return Ok(());
                    };
                    if updated.iter().enumerate().any(|(i, p)| i != idx && p.name == preset.name) {
                        drop(d);
                        send_error(tx, format!("Preset '{}' already exists", preset.name)).await?;
                        return Ok(());
                    }
                    updated[idx] = preset;
                } else if let Some(existing) = updated.iter_mut().find(|p| p.name == preset.name) {
                    *existing = preset;
                } else {
                    updated.push(preset);
                }
                if let Err(e) = validation::validate_presets(&updated, &d.collars) {
                    drop(d);
                    send_error(tx, e.to_string()).await?;
                    return Ok(());
                }
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.presets = updated;
                d.storage.save_presets(&d.presets).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::Ping { nonce } => {
            let client_ips: Vec<String> = ctx.ws_clients.lock().unwrap()
                .iter().map(|(_, addr)| addr.clone()).collect();
            let msg = serde_json::to_string(&ServerMessage::Pong {
                nonce,
                server_uptime_s: uptime_seconds(),
                free_heap_bytes: free_heap(),
                connected_clients: client_ips.len() as u32,
                client_ips,
            }).unwrap();
            tx.send_text(&msg).await?;
        }

        ClientMessage::DeletePreset { name } => {
            {
                let mut d = ctx.domain.lock().unwrap();
                let before = d.presets.len();
                d.presets.retain(|p| p.name != name);
                if d.presets.len() == before {
                    drop(d);
                    send_error(tx, format!("Unknown preset: {name}")).await?;
                    return Ok(());
                }
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.storage.save_presets(&d.presets).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::RunPreset { name } => {
            let (preset, collars, run_id) = {
                let mut d = ctx.domain.lock().unwrap();
                if rf_lockout_remaining_ms(&d) > 0 {
                    drop(d);
                    send_error(tx, "Transmissions locked after STOP".to_string()).await?;
                    return Ok(());
                }
                let Some(preset) = d.presets.iter().find(|p| p.name == name).cloned() else {
                    drop(d);
                    send_error(tx, format!("Unknown preset: {name}")).await?;
                    return Ok(());
                };
                if let Err(e) = validation::validate_preset(&preset, &d.collars) {
                    drop(d);
                    send_error(tx, e.to_string()).await?;
                    return Ok(());
                }
                let run_id = ctx.preset_run_id.fetch_add(1, Ordering::SeqCst) + 1;
                d.preset_name = Some(name.clone());
                (preset, d.collars.clone(), run_id)
            };

            let ctx2 = ctx.clone();
            std::thread::Builder::new()
                .name("preset".into())
                .stack_size(32768)
                .spawn(move || {
                    run_preset(&preset, &collars, &ctx2, run_id);
                    if ctx2.preset_run_id.load(Ordering::SeqCst) == run_id {
                        let mut d = ctx2.domain.lock().unwrap();
                        if d.preset_name.as_deref() == Some(preset.name.as_str()) {
                            d.preset_name = None;
                        }
                    }
                    ctx2.broadcast_state();
                }).ok();

            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::StopPreset => {
            {
                let mut d = ctx.domain.lock().unwrap();
                ctx.preset_run_id.fetch_add(1, Ordering::SeqCst);
                d.preset_name = None;
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::StopAll => {
            {
                let mut d = ctx.domain.lock().unwrap();
                stop_all_transmissions(&mut d, &ctx.preset_run_id);
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::StartRfDebug => {
            *listening_rf_debug = true;
            let prev_count = ctx.rf_debug_listener_count.fetch_add(1, Ordering::SeqCst);
            ctx.rf_debug_enabled.store(true, Ordering::SeqCst);
            // Spawn RF debug worker on first listener (lazy - saves 16KB stack when unused)
            if prev_count == 0 {
                spawn_rf_debug_worker(ctx);
            }
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
            // Small delay to let the response flush
            async_io::Timer::after(Duration::from_millis(200)).await;
            unsafe { esp_idf_svc::sys::esp_restart(); }
        }

        ClientMessage::GetDeviceSettings => {
            let d = ctx.domain.lock().unwrap();
            let msg = serde_json::to_string(&ServerMessage::DeviceSettings {
                settings: d.device_settings.clone(),
                reboot_required: false,
            }).unwrap();
            tx.send_text(&msg).await?;
        }

        ClientMessage::SaveDeviceSettings { settings } => {
            info!("Saving device settings...");
            let (msg, save_err) = {
                let mut d = ctx.domain.lock().unwrap();
                let changed = d.device_settings != settings;
                d.device_settings = settings.clone();
                let save_err = d.storage.save_settings(&settings).err();
                let msg = serde_json::to_string(&ServerMessage::DeviceSettings {
                    settings,
                    reboot_required: changed,
                }).unwrap();
                (msg, save_err)
                // mutex dropped here
            };
            if let Some(e) = save_err {
                error!("NVS save_settings failed: {e:#}");
            } else {
                info!("Device settings saved to NVS");
            }
            tx.send_text(&msg).await?;
        }

        ClientMessage::ReorderPresets { names } => {
            {
                let mut d = ctx.domain.lock().unwrap();
                let mut reordered = Vec::with_capacity(d.presets.len());
                for name in &names {
                    if let Some(idx) = d.presets.iter().position(|p| &p.name == name) {
                        reordered.push(d.presets[idx].clone());
                    }
                }
                // Keep any presets not mentioned in the reorder list
                for p in &d.presets {
                    if !names.contains(&p.name) {
                        reordered.push(p.clone());
                    }
                }
                d.presets = reordered;
                d.storage.save_presets(&d.presets).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }

        ClientMessage::Export => {
            let d = ctx.domain.lock().unwrap();
            let mut data = ExportData { collars: d.collars.clone(), presets: d.presets.clone() };
            drop(d);
            for preset in &mut data.presets {
                preset.normalize();
            }
            let msg = serde_json::to_string(&ServerMessage::ExportData { data: &data }).unwrap();
            tx.send_text(&msg).await?;
        }

        ClientMessage::Import { mut data } => {
            for preset in &mut data.presets {
                preset.normalize();
            }
            if let Err(e) = validation::validate_export_data(&data) {
                send_error(tx, e.to_string()).await?;
                return Ok(());
            }
            {
                let mut d = ctx.domain.lock().unwrap();
                stop_active_preset(&mut d, &ctx.preset_run_id);
                d.collars = data.collars;
                d.presets = data.presets;
                d.storage.save_collars(&d.collars).ok();
                d.storage.save_presets(&d.presets).ok();
            }
            send_state(ctx, tx).await?;
            ctx.broadcast_state();
        }
    }

    Ok(())
}

async fn send_state<W: picoserve::io::Write>(ctx: &AppCtx, tx: &mut ws::SocketTx<W>) -> Result<(), W::Error> {
    tx.send_text(&ctx.state_json()).await
}

async fn send_error<W: picoserve::io::Write>(tx: &mut ws::SocketTx<W>, message: impl Into<String>) -> Result<(), W::Error> {
    let msg = serde_json::to_string(&ServerMessage::Error { message: message.into() }).unwrap();
    tx.send_text(&msg).await
}

// --- Preset execution (runs on std::thread, not async) ---

fn run_preset(preset: &Preset, collars: &[Collar], ctx: &AppCtx, run_id: u32) {
    let mut events: Vec<PresetEvent> = Vec::new();
    for track in &preset.tracks {
        let Some(collar) = collars.iter().find(|c| c.name == track.collar_name) else { continue };
        let mut time_ms = 0u64;
        for step in &track.steps {
            let end_ms = time_ms + step.duration_ms as u64;
            if let Some(mode) = step.mode.to_command_mode() {
                scheduling::schedule_step_events(&mut events, time_ms, end_ms, collar.collar_id, collar.channel, mode.to_rf_byte(), step.intensity);
            }
            time_ms = end_ms;
        }
    }
    events.sort_by_key(|e| e.time_ms);

    let start = Instant::now();
    for event in &events {
        if ctx.preset_run_id.load(Ordering::SeqCst) != run_id { return; }
        let target = Duration::from_millis(event.time_ms);
        let elapsed = start.elapsed();
        if target > elapsed {
            let wait = target - elapsed;
            let chunks = wait.as_millis() as u64 / 50;
            for _ in 0..chunks {
                if ctx.preset_run_id.load(Ordering::SeqCst) != run_id { return; }
                std::thread::sleep(Duration::from_millis(50));
            }
            let remainder = wait - Duration::from_millis(chunks * 50);
            if !remainder.is_zero() { std::thread::sleep(remainder); }
        }
        if let Err(e) = rf_send_with_led(&ctx.rf, &ctx.tx_led, event.collar_id, event.channel, event.mode_byte, event.intensity) {
            error!("RF error during preset: {e}");
        }
    }
    info!("Preset '{}' completed", preset.name);
}

/// Spawn RF debug worker thread on demand (takes the receiver from AppCtx).
/// The thread runs until `rf_debug_enabled` is set to false (last listener leaves),
/// then returns the receiver back into AppCtx for reuse.
fn spawn_rf_debug_worker(ctx: &AppCtx) {
    let Some(mut receiver) = ctx.rf_receiver.lock().unwrap().take() else {
        warn!("RF debug receiver already in use by another worker");
        return;
    };
    let ctx = ctx.clone();
    let result = std::thread::Builder::new()
        .name("rf-debug-rx".into())
        .stack_size(16384)
        .spawn(move || {
            info!("RF debug worker started");
            loop {
                if !ctx.rf_debug_enabled.load(Ordering::SeqCst) {
                    break;
                }
                match receiver.listen_until_disabled(&ctx.rf_debug_enabled) {
                    Ok(Some(event)) => {
                        ctx.rx_led.lock().unwrap().set(true);
                        {
                            let mut d = ctx.domain.lock().unwrap();
                            d.rf_debug_events.push(event.clone());
                            if d.rf_debug_events.len() > MAX_RF_DEBUG_EVENTS {
                                let excess = d.rf_debug_events.len() - MAX_RF_DEBUG_EVENTS;
                                d.rf_debug_events.drain(0..excess);
                            }
                        }
                        let json = serde_json::to_string(&ServerMessage::RfDebugEvent { event: &event }).unwrap();
                        let _ = ctx.broadcast_tx.try_broadcast(BroadcastMsg {
                            json: Arc::from(json),
                            rf_debug: true,
                        });
                        // Keep LED visible for a short time
                        std::thread::sleep(Duration::from_millis(50));
                        ctx.rx_led.lock().unwrap().set(false);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        error!("RF debug receiver error: {err:#}");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            // Return the receiver for reuse
            *ctx.rf_receiver.lock().unwrap() = Some(receiver);
            info!("RF debug worker stopped");
        });
    if let Err(e) = result {
        error!("Failed to spawn RF debug worker: {e}");
    }
}

// --- Server startup ---

pub fn run_server(ctx: AppCtx) -> Result<()> {
    let conn_id_store = ctx.last_conn_id.clone();
    let conn_addr_store = ctx.last_conn_addr.clone();
    let max_clients = ctx.domain.lock().unwrap().device_settings.max_clients as u32;
    let app = make_app().with_state(ctx);

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

                    let app_ref = &app;
                    let config_ref = &config;
                    let active_ref = active.clone();
                    active_ref.set(active_ref.get() + 1);
                    // Set conn_id + addr for the WS handler to read (single-threaded, no race)
                    conn_id_store.store(conn_id, Ordering::Relaxed);
                    *conn_addr_store.lock().unwrap() = addr.ip().to_string();

                    ex.spawn(async move {
                        let socket = AsyncIoSocket(stream);
                        let mut http_buf = vec![0u8; HTTP_BUF_SIZE];
                        let server = picoserve::Server::custom(
                            app_ref, AsyncIoTimer, config_ref, &mut http_buf,
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
