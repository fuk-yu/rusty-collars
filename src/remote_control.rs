use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::warn;

use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::io::EspIOError;
use esp_idf_svc::sys::ESP_ERR_INVALID_ARG;
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, EspWebSocketTransport, FrameType,
    WebSocketEventType,
};

use crate::protocol::{ClientMessage, DeviceSettings, EventLogEntryKind, EventSource};
use crate::server::{
    self, cancel_owned_manual_actions, pong_json, remote_control_status, ActionOwner, AppCtx,
    MessageOrigin, RemoteControlUrlKind,
};

const DISABLED_POLL_INTERVAL_MS: u64 = 500;
const EVENT_LOOP_TICK_MS: u64 = 100;
const CONNECT_TIMEOUT_MS: u64 = 3_000;
const INITIAL_SYNC_DELAY_MS: u64 = 500;
const RECONNECT_BASE_DELAY_MS: u64 = 1_000;
const RECONNECT_MAX_DELAY_MS: u64 = 10_000;
const REMOTE_PING_INTERVAL_MS: u64 = 5_000;
const REMOTE_PING_TIMEOUT_MS: u64 = 15_000;
const WS_BUFFER_SIZE: usize = 8 * 1024;
const WS_NETWORK_TIMEOUT: Duration = Duration::from_secs(10);
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(10);

enum RemoteWsEvent {
    Connected,
    Disconnected(String),
    Text(String),
    Error(String),
}

enum RemoteTextOutcome {
    None,
    Pong(u32),
}

enum RunLoopExit {
    SettingsChanged { was_connected: bool },
    Disconnected { was_connected: bool, reason: String },
}

pub fn start(ctx: AppCtx) -> Result<()> {
    std::thread::Builder::new()
        .name("remote-control".into())
        .stack_size(65536)
        .spawn(move || worker(ctx))?;
    Ok(())
}

fn worker(ctx: AppCtx) {
    let mut reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;

    loop {
        let settings = ctx.domain.lock().unwrap().device_settings.clone();
        if !settings.remote_control_enabled {
            ctx.set_remote_control_status(remote_control_status(&settings, false, None, "Off"));
            reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
            std::thread::sleep(Duration::from_millis(DISABLED_POLL_INTERVAL_MS));
            continue;
        }

        let url = settings.remote_control_url.trim().to_string();
        let (url_kind, endpoint_url) = match server::remote_control_endpoint_url(&settings) {
            Ok(endpoint) => endpoint,
            Err(err) => {
                ctx.set_remote_control_status(remote_control_status(&settings, false, None, err));
                reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
                std::thread::sleep(Duration::from_millis(DISABLED_POLL_INTERVAL_MS));
                continue;
            }
        };

        ctx.set_remote_control_status(remote_control_status(
            &settings,
            false,
            None,
            "Connecting...",
        ));

        let settings_revision = ctx.remote_control_settings_revision.load(Ordering::SeqCst);
        let exit = run_connection(&ctx, &settings, &endpoint_url, url_kind, settings_revision);

        match exit {
            RunLoopExit::SettingsChanged { was_connected } => {
                if was_connected {
                    cancel_owned_manual_actions(&ctx, ActionOwner::RemoteControl);
                    ctx.record_event(
                        EventSource::System,
                        EventLogEntryKind::RemoteControlConnection {
                            connected: false,
                            url: url.clone(),
                            reason: Some("Settings changed".to_string()),
                        },
                    );
                }
                reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
            }
            RunLoopExit::Disconnected {
                was_connected,
                reason,
            } => {
                if was_connected {
                    cancel_owned_manual_actions(&ctx, ActionOwner::RemoteControl);
                    ctx.record_event(
                        EventSource::System,
                        EventLogEntryKind::RemoteControlConnection {
                            connected: false,
                            url: url.clone(),
                            reason: Some(reason.clone()),
                        },
                    );
                } else {
                    warn!("Remote control connect failed: {reason}");
                }

                let retry_status =
                    format!("Retrying in {:.1}s", reconnect_delay_ms as f64 / 1000.0);
                ctx.set_remote_control_status(remote_control_status(
                    &settings,
                    false,
                    None,
                    retry_status,
                ));

                std::thread::sleep(Duration::from_millis(reconnect_delay_ms));
                reconnect_delay_ms = (reconnect_delay_ms * 2).min(RECONNECT_MAX_DELAY_MS);
            }
        }
    }
}

fn run_connection(
    ctx: &AppCtx,
    settings: &DeviceSettings,
    endpoint_url: &str,
    url_kind: RemoteControlUrlKind,
    settings_revision: u32,
) -> RunLoopExit {
    let (event_tx, event_rx) = mpsc::channel::<RemoteWsEvent>();
    let client_result = connect_client(settings, endpoint_url, url_kind, event_tx);
    let mut client = match client_result {
        Ok(client) => client,
        Err(err) => {
            return RunLoopExit::Disconnected {
                was_connected: false,
                reason: format!("Connect failed: {err:#}"),
            };
        }
    };

    let exit = run_event_loop(ctx, settings, &mut client, &event_rx, settings_revision);
    safe_drop_ws_client(client);
    exit
}

/// The upstream `EspWebSocketClient::Drop` (esp-idf-svc 0.52) calls `.unwrap()`
/// on `esp_websocket_client_close` / `esp_websocket_client_destroy`, which panics
/// (aborting the process on ESP-IDF) when the transport was never established or
/// was already torn down. Work around this by extracting the raw C handle,
/// forgetting the Rust wrapper, and manually performing cleanup without unwrap.
///
/// Uses `esp_websocket_client_stop` instead of `close` because `close` sends a
/// CLOSE frame and waits for the server's response — which hangs when the server
/// is already dead. `stop` tears down the TCP connection immediately.
///
/// The callback closure (~64 bytes) is leaked each time. This is acceptable for
/// a reconnection that happens at most every few seconds.
fn safe_drop_ws_client(client: EspWebSocketClient<'static>) {
    let handle = client.handle();
    std::mem::forget(client);
    unsafe {
        let _ = esp_idf_svc::sys::esp_websocket_client_stop(handle);
        let _ = esp_idf_svc::sys::esp_websocket_client_destroy(handle);
    }
}

fn run_event_loop(
    ctx: &AppCtx,
    settings: &DeviceSettings,
    client: &mut EspWebSocketClient<'_>,
    event_rx: &mpsc::Receiver<RemoteWsEvent>,
    settings_revision: u32,
) -> RunLoopExit {
    let mut broadcast_rx = ctx.broadcast_tx.new_receiver();
    let mut connected = false;
    let mut transport_connected = false;
    let connect_started_at = Instant::now();
    let mut initial_sync_due_at: Option<Instant> = None;
    let mut next_ping_at = Instant::now();
    let mut next_ping_nonce = 0u32;
    let mut pending_ping: Option<(u32, Instant)> = None;

    loop {
        if ctx.remote_control_settings_revision.load(Ordering::SeqCst) != settings_revision {
            return RunLoopExit::SettingsChanged {
                was_connected: connected,
            };
        }

        if !transport_connected && client.is_connected() {
            transport_connected = true;
            initial_sync_due_at =
                Some(Instant::now() + Duration::from_millis(INITIAL_SYNC_DELAY_MS));
        }

        if transport_connected && !connected && pending_ping.is_none() {
            if let Some(sync_due_at) = initial_sync_due_at {
                let now = Instant::now();
                if now >= sync_due_at {
                    next_ping_nonce = next_ping_nonce.wrapping_add(1);
                    if let Err(err) = send_text(client, &ping_json(next_ping_nonce)) {
                        return RunLoopExit::Disconnected {
                            was_connected: false,
                            reason: err,
                        };
                    }

                    pending_ping = Some((next_ping_nonce, now));
                    initial_sync_due_at = None;
                }
            }
        }

        if connected {
            while let Ok(message) = broadcast_rx.try_recv() {
                if message.rf_debug {
                    continue;
                }

                if let Err(err) = send_text(client, &message.json) {
                    return RunLoopExit::Disconnected {
                        was_connected: connected,
                        reason: err,
                    };
                }
            }

            let now = Instant::now();
            if let Some((nonce, started_at)) = pending_ping {
                if now.duration_since(started_at).as_millis() as u64 >= REMOTE_PING_TIMEOUT_MS {
                    return RunLoopExit::Disconnected {
                        was_connected: connected,
                        reason: format!("Ping timeout for nonce {nonce}"),
                    };
                }
            } else if now >= next_ping_at {
                next_ping_nonce = next_ping_nonce.wrapping_add(1);
                if let Err(err) = send_text(client, &ping_json(next_ping_nonce)) {
                    return RunLoopExit::Disconnected {
                        was_connected: connected,
                        reason: err,
                    };
                }

                pending_ping = Some((next_ping_nonce, now));
                next_ping_at = now + Duration::from_millis(REMOTE_PING_INTERVAL_MS);
            }
        }

        match event_rx.recv_timeout(Duration::from_millis(EVENT_LOOP_TICK_MS)) {
            Ok(RemoteWsEvent::Connected) => {
                if !transport_connected {
                    transport_connected = true;
                    initial_sync_due_at =
                        Some(Instant::now() + Duration::from_millis(INITIAL_SYNC_DELAY_MS));
                }
            }
            Ok(RemoteWsEvent::Disconnected(reason)) => {
                return RunLoopExit::Disconnected {
                    was_connected: connected,
                    reason,
                };
            }
            Ok(RemoteWsEvent::Text(text)) => match handle_text(ctx, client, &text) {
                Ok(RemoteTextOutcome::None) => {}
                Ok(RemoteTextOutcome::Pong(nonce)) => {
                    if let Some((pending_nonce, started_at)) = pending_ping {
                        if pending_nonce == nonce {
                            let rtt_ms = now_rtt_ms(started_at);
                            pending_ping = None;
                            if !connected {
                                connected = true;
                                next_ping_at =
                                    Instant::now() + Duration::from_millis(REMOTE_PING_INTERVAL_MS);
                                if let Err(err) = send_initial_state(ctx, settings, client) {
                                    return RunLoopExit::Disconnected {
                                        was_connected: connected,
                                        reason: err,
                                    };
                                }
                            }
                            ctx.set_remote_control_status(remote_control_status(
                                settings,
                                true,
                                Some(rtt_ms),
                                "Connected",
                            ));
                        }
                    }
                }
                Err(err) => {
                    return RunLoopExit::Disconnected {
                        was_connected: connected,
                        reason: err,
                    };
                }
            },
            Ok(RemoteWsEvent::Error(reason)) => {
                return RunLoopExit::Disconnected {
                    was_connected: connected,
                    reason,
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !transport_connected
                    && connect_started_at.elapsed().as_millis() as u64 >= CONNECT_TIMEOUT_MS
                {
                    return RunLoopExit::Disconnected {
                        was_connected: false,
                        reason: format!("Connect timeout after {CONNECT_TIMEOUT_MS}ms"),
                    };
                }
                if !connected {
                    if let Some((nonce, started_at)) = pending_ping {
                        if started_at.elapsed().as_millis() as u64 >= REMOTE_PING_TIMEOUT_MS {
                            return RunLoopExit::Disconnected {
                                was_connected: false,
                                reason: format!("Handshake ping timeout for nonce {nonce}"),
                            };
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return RunLoopExit::Disconnected {
                    was_connected: connected,
                    reason: "Remote control event channel closed".to_string(),
                };
            }
        }
    }
}

fn connect_client(
    settings: &DeviceSettings,
    url: &str,
    url_kind: RemoteControlUrlKind,
    event_tx: mpsc::Sender<RemoteWsEvent>,
) -> Result<EspWebSocketClient<'static>> {
    let transport = match url_kind {
        RemoteControlUrlKind::Ws => EspWebSocketTransport::TransportOverTCP,
        RemoteControlUrlKind::Wss => EspWebSocketTransport::TransportOverSSL,
    };

    let config = EspWebSocketClientConfig {
        disable_auto_reconnect: true,
        buffer_size: WS_BUFFER_SIZE,
        network_timeout_ms: WS_NETWORK_TIMEOUT,
        transport,
        skip_cert_common_name_check: !settings.remote_control_validate_cert,
        #[cfg(not(esp_idf_version_major = "4"))]
        crt_bundle_attach: if matches!(url_kind, RemoteControlUrlKind::Wss)
            && settings.remote_control_validate_cert
        {
            Some(esp_idf_svc::sys::esp_crt_bundle_attach)
        } else {
            None
        },
        ..Default::default()
    };

    let event_tx_for_callback = event_tx.clone();
    Ok(EspWebSocketClient::new(
        url,
        &config,
        WS_SEND_TIMEOUT,
        move |event| {
            let outgoing = match event {
                Ok(event) => match event.event_type {
                    WebSocketEventType::Connected => Some(RemoteWsEvent::Connected),
                    WebSocketEventType::Disconnected => {
                        Some(RemoteWsEvent::Disconnected("Disconnected".to_string()))
                    }
                    WebSocketEventType::Closed => {
                        Some(RemoteWsEvent::Disconnected("Closed".to_string()))
                    }
                    WebSocketEventType::Close(reason) => Some(RemoteWsEvent::Disconnected(
                        format!("Close frame: {reason:?}"),
                    )),
                    WebSocketEventType::Text(text) => Some(RemoteWsEvent::Text(text.to_string())),
                    WebSocketEventType::Binary(_) => None,
                    WebSocketEventType::Ping => None,
                    WebSocketEventType::Pong => None,
                    WebSocketEventType::BeforeConnect => None,
                },
                Err(err) => {
                    if should_ignore_callback_error(&err) {
                        None
                    } else {
                        Some(RemoteWsEvent::Error(format!("{err:?}")))
                    }
                }
            };

            if let Some(event) = outgoing {
                let _ = event_tx_for_callback.send(event);
            }
        },
    )?)
}

fn handle_text(
    ctx: &AppCtx,
    client: &mut EspWebSocketClient<'_>,
    text: &str,
) -> core::result::Result<RemoteTextOutcome, String> {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            send_error(client, format!("Invalid remote control message: {err}"))?;
            return Ok(RemoteTextOutcome::None);
        }
    };

    let Some(message_type) = value.get("type").and_then(|value| value.as_str()) else {
        send_error(client, "Invalid remote control message: missing type")?;
        return Ok(RemoteTextOutcome::None);
    };

    match message_type {
        "pong" => {
            let Some(nonce) = extract_nonce(&value) else {
                send_error(client, "Invalid pong: missing or invalid nonce")?;
                return Ok(RemoteTextOutcome::None);
            };
            Ok(RemoteTextOutcome::Pong(nonce))
        }
        "ping" => {
            let Some(nonce) = extract_nonce(&value) else {
                send_error(client, "Invalid ping: missing or invalid nonce")?;
                return Ok(RemoteTextOutcome::None);
            };
            send_text(client, &pong_json(ctx, nonce))?;
            Ok(RemoteTextOutcome::None)
        }
        _ => {
            let msg: ClientMessage = match serde_json::from_value(value) {
                Ok(msg) => msg,
                Err(err) => {
                    send_error(client, format!("Invalid remote control message: {err}"))?;
                    return Ok(RemoteTextOutcome::None);
                }
            };

            match server::process_control_message(
                ctx,
                msg,
                MessageOrigin::RemoteControl,
                Some(ActionOwner::RemoteControl),
            ) {
                Ok(messages) => {
                    for message in messages {
                        send_text(client, &message)?;
                    }
                }
                Err(message) => {
                    send_error(client, message)?;
                }
            }

            Ok(RemoteTextOutcome::None)
        }
    }
}

fn send_text(client: &mut EspWebSocketClient<'_>, text: &str) -> core::result::Result<(), String> {
    client
        .send(FrameType::Text(false), text.as_bytes())
        .map_err(|err| format!("WebSocket send failed: {err:?}"))
}

fn send_error(
    client: &mut EspWebSocketClient<'_>,
    message: impl Into<String>,
) -> core::result::Result<(), String> {
    let json = server::error_json(message);
    send_text(client, &json)
}

fn should_ignore_callback_error(err: &EspIOError) -> bool {
    // esp-idf-svc 0.52.1 does not decode some newer websocket callback event ids
    // exposed by the managed esp_websocket_client component and reports them as
    // ESP_ERR_INVALID_ARG. These are benign for our usage.
    err.0.code() == ESP_ERR_INVALID_ARG
}

fn send_initial_state(
    ctx: &AppCtx,
    settings: &DeviceSettings,
    client: &mut EspWebSocketClient<'_>,
) -> core::result::Result<(), String> {
    ctx.set_remote_control_status(remote_control_status(settings, true, None, "Connected"));
    ctx.record_event(
        EventSource::System,
        EventLogEntryKind::RemoteControlConnection {
            connected: true,
            url: settings.remote_control_url.trim().to_string(),
            reason: None,
        },
    );

    for json in ctx.remote_sync_jsons() {
        send_text(client, &json)?;
    }
    Ok(())
}

fn extract_nonce(value: &serde_json::Value) -> Option<u32> {
    value
        .get("nonce")?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
}

fn ping_json(nonce: u32) -> String {
    serde_json::json!({"type": "ping", "nonce": nonce}).to_string()
}

fn now_rtt_ms(started_at: Instant) -> u32 {
    started_at
        .elapsed()
        .as_millis()
        .max(1)
        .min(u32::MAX as u128) as u32
}
