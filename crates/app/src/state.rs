use std::collections::VecDeque;

use rusty_collars_core::protocol::{
    Collar, DeviceSettings, EventLogEntry, MqttStatus, Preset, RemoteControlStatus, RfDebugFrame,
};

#[derive(Debug, Clone)]
pub struct DomainState {
    pub device_settings: DeviceSettings,
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
    pub preset_name: Option<String>,
    pub rf_lockout_until_ms: u64,
    pub rf_debug_events: VecDeque<RfDebugFrame>,
    pub event_log_events: Vec<EventLogEntry>,
    pub remote_control_status: RemoteControlStatus,
    pub mqtt_status: MqttStatus,
}
