use std::time::Duration;

use log::{info, warn};
use picoserve::futures::Either;
use picoserve::response::ws::{self, Message};

use crate::protocol::{ClientInfo, ClientMessage};

use super::{
    cancel_owned_manual_actions, error_json, local_ui_dispatcher, ActionOwner, ConnectionState,
};

const WS_BUF_SIZE: usize = 2048;

pub(super) struct WsHandler {
    pub forwarded_for: Option<String>,
    pub user_agent: Option<String>,
}

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

        ctx.register_ws_client(
            ws_id,
            ClientInfo {
                ip: ws_addr.clone(),
                forwarded_for: self.forwarded_for,
                user_agent: self.user_agent,
            },
        );
        for event in ctx.local_ui_sync_events(false) {
            let json = event.json();
            tx.send_text(&json).await?;
        }

        let mut broadcast_rx = ctx.new_broadcast_receiver();
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
                Ok(Either::Second(Ok(event))) => {
                    if event.is_rf_debug() && !listening_rf_debug {
                        continue;
                    }
                    if event.is_action_fired() {
                        continue;
                    }
                    let json = event.json();
                    tx.send_text(&json).await?;
                }
                Ok(Either::Second(Err(_))) => break,
                Err(_) => break,
            }
        }

        super::debug::release_rf_debug_listener(ctx, listening_rf_debug);
        cancel_owned_manual_actions(ctx, ActionOwner::LocalWs(ws_id));
        ctx.unregister_ws_client(ws_id);

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
            let event = super::debug::start_rf_debug_listener(ctx, *listening_rf_debug);
            *listening_rf_debug = true;
            let json = event.json();
            tx.send_text(&json).await?;
        }
        ClientMessage::StopRfDebug => {
            let event = super::debug::stop_rf_debug_listener(ctx, *listening_rf_debug);
            *listening_rf_debug = false;
            let json = event.json();
            tx.send_text(&json).await?;
        }
        ClientMessage::ClearRfDebug => {
            let event = super::debug::clear_rf_debug_events(ctx, *listening_rf_debug);
            let json = event.json();
            tx.send_text(&json).await?;
        }
        ClientMessage::Reboot => {
            info!("Reboot requested via WebSocket");
            tx.send_text(r#"{"type":"state","rebooting":true}"#).await?;
            super::admin::schedule_reboot(Duration::from_millis(200));
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
