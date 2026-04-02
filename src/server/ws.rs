use std::sync::atomic::Ordering;
use std::time::Duration;

use log::{info, warn};
use picoserve::futures::Either;
use picoserve::response::ws::{self, Message};

use crate::protocol::ClientMessage;

use super::{
    cancel_owned_manual_actions, error_json, local_ui_dispatcher, ActionOwner, ConnectionState,
};

const WS_BUF_SIZE: usize = 2048;

pub(super) struct WsHandler;

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

        ctx.sessions
            .ws_clients
            .lock()
            .unwrap()
            .push((ws_id, ws_addr.clone()));
        for json in ctx.local_ui_sync_jsons(false) {
            tx.send_text(&json).await?;
        }

        let mut broadcast_rx = ctx.sessions.broadcast_tx.new_receiver();
        let mut listening_rf_debug = false;
        let mut buf = vec![0u8; WS_BUF_SIZE];

        loop {
            match rx.next_message(&mut buf, broadcast_rx.recv()).await {
                Ok(Either::First(Ok(Message::Text(text)))) => {
                    if let Err(err) = handle_text_message(
                        ctx,
                        &mut tx,
                        text,
                        &mut listening_rf_debug,
                        ActionOwner::LocalWs(ws_id),
                    )
                    .await
                    {
                        warn!("WS handler error: {err:#}");
                        break;
                    }
                }
                Ok(Either::First(Ok(Message::Binary(_)))) => {}
                Ok(Either::First(Ok(Message::Ping(data)))) => {
                    tx.send_pong(data).await?;
                }
                Ok(Either::First(Ok(Message::Pong(_)))) => {}
                Ok(Either::First(Ok(Message::Close(_)))) => break,
                Ok(Either::First(Err(err))) => {
                    warn!("WS read error: {err:?}");
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
            let previous = ctx
                .debug
                .rf_debug_listener_count
                .fetch_sub(1, Ordering::SeqCst);
            if previous <= 1 {
                ctx.debug.rf_debug_enabled.store(false, Ordering::SeqCst);
            }
        }
        cancel_owned_manual_actions(ctx, ActionOwner::LocalWs(ws_id));
        ctx.sessions
            .ws_clients
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != ws_id);

        info!("[#{ws_id}] WebSocket disconnected from {ws_addr}");
        Ok(())
    }
}

async fn handle_text_message<W: picoserve::io::Write>(
    ctx: &super::AppCtx,
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
            ctx.debug
                .rf_debug_listener_count
                .fetch_add(1, Ordering::SeqCst);
            ctx.debug.rf_debug_enabled.store(true, Ordering::SeqCst);
            super::runtime::ensure_rf_debug_worker(ctx);
            let json = ctx.rf_debug_state_json(true);
            tx.send_text(&json).await?;
        }
        ClientMessage::StopRfDebug => {
            if *listening_rf_debug {
                *listening_rf_debug = false;
                let previous = ctx
                    .debug
                    .rf_debug_listener_count
                    .fetch_sub(1, Ordering::SeqCst);
                if previous <= 1 {
                    ctx.debug.rf_debug_enabled.store(false, Ordering::SeqCst);
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
        msg => match local_ui_dispatcher(ctx, owner).handle(msg) {
            Ok(messages) => {
                for message in messages {
                    tx.send_text(&message).await?;
                }
            }
            Err(message) => {
                send_error(tx, message.to_string()).await?;
            }
        },
    }

    Ok(())
}

async fn send_error<W: picoserve::io::Write>(
    tx: &mut ws::SocketTx<W>,
    message: impl Into<String>,
) -> Result<(), W::Error> {
    let json = error_json(message);
    tx.send_text(&json).await
}
