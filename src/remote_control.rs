use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use log::warn;

use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, EspWebSocketTransport, FrameType,
    WebSocketEventType,
};

use crate::protocol::{
    ClientMessage, DeviceSettings, EventLogEntryKind, EventSource, RemoteControlStatus,
    ServerMessage,
};
use crate::server::{
    self, cancel_owned_manual_actions, pong_json, ActionOwner, AppCtx, MessageOrigin,
    RemoteControlUrlKind,
};

const DISABLED_POLL_INTERVAL_MS: u64 = 500;
const EVENT_LOOP_TICK_MS: u64 = 100;
const RECONNECT_BASE_DELAY_MS: u64 = 1_000;
const RECONNECT_MAX_DELAY_MS: u64 = 30_000;
const REMOTE_PING_INTERVAL_MS: u64 = 5_000;
const REMOTE_PING_TIMEOUT_MS: u64 = 15_000;
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
            ctx.set_remote_control_status(status_from_settings(&settings, false, None, "Off"));
            reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
            std::thread::sleep(Duration::from_millis(DISABLED_POLL_INTERVAL_MS));
            continue;
        }

        let url = settings.remote_control_url.trim().to_string();
        let url_kind = match server::parse_remote_control_url(&url) {
            Ok(kind) => kind,
            Err(err) => {
                ctx.set_remote_control_status(status_from_settings(&settings, false, None, err));
                reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
                std::thread::sleep(Duration::from_millis(DISABLED_POLL_INTERVAL_MS));
                continue;
            }
        };

        ctx.set_remote_control_status(status_from_settings(
            &settings,
            false,
            None,
            "Connecting...",
        ));

        let settings_revision = ctx.remote_control_settings_revision.load(Ordering::SeqCst);
        let exit = run_connection(&ctx, &settings, url_kind, settings_revision);

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
                ctx.set_remote_control_status(status_from_settings(
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
    url_kind: RemoteControlUrlKind,
    settings_revision: u32,
) -> RunLoopExit {
    let (event_tx, event_rx) = mpsc::channel::<RemoteWsEvent>();
    let client_result = connect_client(settings, url_kind, event_tx);
    let mut client = match client_result {
        Ok(client) => client,
        Err(err) => {
            return RunLoopExit::Disconnected {
                was_connected: false,
                reason: format!("Connect failed: {err:#}"),
            };
        }
    };

    let mut broadcast_rx = ctx.broadcast_tx.new_receiver();
    let mut connected = false;
    let mut next_ping_at = Instant::now();
    let mut next_ping_nonce = 0u32;
    let mut pending_ping: Option<(u32, Instant)> = None;

    loop {
        if ctx.remote_control_settings_revision.load(Ordering::SeqCst) != settings_revision {
            return RunLoopExit::SettingsChanged {
                was_connected: connected,
            };
        }

        if connected {
            while let Ok(message) = broadcast_rx.try_recv() {
                if message.rf_debug {
                    continue;
                }

                if let Err(err) = send_text(&mut client, &message.json) {
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
                let ping = serde_json::json!({
                    "type": "ping",
                    "nonce": next_ping_nonce,
                })
                .to_string();

                if let Err(err) = send_text(&mut client, &ping) {
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
                if connected {
                    continue;
                }

                connected = true;
                pending_ping = None;
                next_ping_at = Instant::now();
                ctx.set_remote_control_status(status_from_settings(
                    settings,
                    true,
                    None,
                    "Connected",
                ));
                ctx.record_event(
                    EventSource::System,
                    EventLogEntryKind::RemoteControlConnection {
                        connected: true,
                        url: settings.remote_control_url.trim().to_string(),
                        reason: None,
                    },
                );

                if let Err(err) = send_text(&mut client, &ctx.state_json()) {
                    return RunLoopExit::Disconnected {
                        was_connected: connected,
                        reason: err,
                    };
                }
                if let Err(err) = send_text(&mut client, &ctx.event_log_state_json()) {
                    return RunLoopExit::Disconnected {
                        was_connected: connected,
                        reason: err,
                    };
                }
            }
            Ok(RemoteWsEvent::Disconnected(reason)) => {
                return RunLoopExit::Disconnected {
                    was_connected: connected,
                    reason,
                };
            }
            Ok(RemoteWsEvent::Text(text)) => match handle_text(ctx, &mut client, &text) {
                Ok(RemoteTextOutcome::None) => {}
                Ok(RemoteTextOutcome::Pong(nonce)) => {
                    if let Some((pending_nonce, started_at)) = pending_ping {
                        if pending_nonce == nonce {
                            let rtt_ms = now_rtt_ms(started_at);
                            pending_ping = None;
                            ctx.set_remote_control_status(status_from_settings(
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
            Err(mpsc::RecvTimeoutError::Timeout) => {}
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
    url_kind: RemoteControlUrlKind,
    event_tx: mpsc::Sender<RemoteWsEvent>,
) -> Result<EspWebSocketClient<'static>> {
    let transport = match url_kind {
        RemoteControlUrlKind::Ws => EspWebSocketTransport::TransportOverTCP,
        RemoteControlUrlKind::Wss => EspWebSocketTransport::TransportOverSSL,
    };

    let config = EspWebSocketClientConfig {
        disable_auto_reconnect: true,
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

    let url = settings.remote_control_url.trim().to_string();
    let event_tx_for_callback = event_tx.clone();
    Ok(EspWebSocketClient::new(
        &url,
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
                Err(err) => Some(RemoteWsEvent::Error(format!("{err:?}"))),
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
            let Some(nonce) = value
                .get("nonce")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
            else {
                send_error(client, "Invalid pong: missing or invalid nonce")?;
                return Ok(RemoteTextOutcome::None);
            };

            Ok(RemoteTextOutcome::Pong(nonce))
        }
        "ping" => {
            let Some(nonce) = value
                .get("nonce")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
            else {
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
    let json = serde_json::to_string(&ServerMessage::Error {
        message: message.into(),
    })
    .unwrap();
    send_text(client, &json)
}

fn status_from_settings(
    settings: &DeviceSettings,
    connected: bool,
    rtt_ms: Option<u32>,
    status_text: impl Into<String>,
) -> RemoteControlStatus {
    RemoteControlStatus {
        enabled: settings.remote_control_enabled,
        connected,
        url: settings.remote_control_url.trim().to_string(),
        validate_cert: settings.remote_control_validate_cert,
        rtt_ms,
        status_text: status_text.into(),
    }
}

fn now_rtt_ms(started_at: Instant) -> u32 {
    started_at
        .elapsed()
        .as_millis()
        .max(1)
        .min(u32::MAX as u128) as u32
}
