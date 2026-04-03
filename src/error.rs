pub use rusty_collars_app::ControlError;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteControlError {
    #[error("Connect failed: {0}")]
    Connect(String),
    #[error("{0}")]
    Disconnected(String),
    #[error("WebSocket send failed: {0}")]
    WebSocketSend(String),
    #[error("Connect timeout after {timeout_ms}ms")]
    ConnectTimeout { timeout_ms: u64 },
    #[error("{phase} ping timeout for nonce {nonce}")]
    PingTimeout { nonce: u32, phase: &'static str },
    #[error("Remote control event channel closed")]
    EventChannelClosed,
}
