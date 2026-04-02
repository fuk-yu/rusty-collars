use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use async_broadcast::{InactiveReceiver, Sender as BroadcastSender};
use rand::rngs::SmallRng;
use rand::SeedableRng;

use crate::led::Led;
use crate::protocol::{
    Collar, DeviceSettings, EventLogEntry, EventLogEntryKind, EventSource, Preset,
    RemoteControlStatus, ServerMessage,
};
use crate::repository::SharedRepository;
use crate::rf::{RfReceiver, RfTransmitter};

use super::status;
use super::{
    current_unix_ms, now_millis, rf_lockout_remaining_ms, uptime_seconds, BroadcastMsg, DebugCtx,
    DomainState, HardwareCtx, SessionCtx, WorkerCtx, MAX_EVENT_LOG_ENTRIES, MAX_RF_DEBUG_EVENTS,
};

#[derive(Clone)]
pub struct AppCtx {
    pub domain: Arc<Mutex<DomainState>>,
    pub repository: SharedRepository,
    pub hardware: HardwareCtx,
    pub sessions: SessionCtx,
    pub worker: WorkerCtx,
    pub debug: DebugCtx,
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
        broadcast_keepalive: InactiveReceiver<BroadcastMsg>,
        rf_receiver: RfReceiver,
        device_settings: DeviceSettings,
        repository: SharedRepository,
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
                rf_debug_events: Default::default(),
                event_log_events: Vec::new(),
                remote_control_status,
            })),
            repository,
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
            },
            worker: WorkerCtx {
                preset_run_id: Arc::new(AtomicU32::new(0)),
                active_actions: Arc::new(Mutex::new(HashMap::new())),
                worker_notify: Arc::new((Mutex::new(()), Condvar::new())),
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

    fn broadcast_json(&self, json: Arc<str>, rf_debug: bool) {
        let _ = self
            .sessions
            .broadcast_tx
            .try_broadcast(BroadcastMsg { json, rf_debug });
    }

    pub(crate) fn broadcast_state(&self) {
        self.broadcast_json(self.state_json(), false);
    }

    pub(crate) fn notify_worker(&self) {
        let _lock = self.worker.worker_notify.0.lock().unwrap();
        self.worker.worker_notify.1.notify_one();
    }

    pub(crate) fn state_json(&self) -> Arc<str> {
        let domain = self.domain.lock().unwrap();
        Arc::from(
            serde_json::to_string(&ServerMessage::State {
                device_id: &domain.device_settings.device_id,
                app_version: crate::build_info::APP_VERSION,
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
                sequence: u64::from(
                    self.worker
                        .event_log_sequence
                        .fetch_add(1, Ordering::SeqCst)
                        + 1,
                ),
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

    pub(crate) fn persist_collars(&self, collars: &[Collar]) {
        if let Err(err) = self.repository.lock().unwrap().save_collars(collars) {
            log::error!("NVS save_collars failed: {err:#}");
        }
    }

    pub(crate) fn persist_presets(&self, presets: &[Preset]) {
        if let Err(err) = self.repository.lock().unwrap().save_presets(presets) {
            log::error!("NVS save_presets failed: {err:#}");
        }
    }

    pub(crate) fn persist_settings(&self, settings: &DeviceSettings) -> anyhow::Result<()> {
        self.repository
            .lock()
            .unwrap()
            .save_settings(settings)
            .map_err(Into::into)
    }

    pub(crate) fn push_rf_debug_event(&self, event: crate::protocol::RfDebugFrame) {
        {
            let mut domain = self.domain.lock().unwrap();
            domain.rf_debug_events.push_back(event.clone());
            if domain.rf_debug_events.len() > MAX_RF_DEBUG_EVENTS {
                domain.rf_debug_events.pop_front();
            }
        }

        self.broadcast_json(
            Arc::from(
                serde_json::to_string(&ServerMessage::RfDebugEvent { event: &event }).unwrap(),
            ),
            true,
        );
    }
}
