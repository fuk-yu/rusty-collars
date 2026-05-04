use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rand::rngs::SmallRng;
use rand::Rng;

use crate::error::ControlError;
use crate::led::Led;
use crate::protocol::{CommandMode, Distribution, EventSource};
use crate::rf::RfTransmitter;

mod admin;
mod context;
mod control;
mod debug;
mod http;
mod runtime;
mod state;
mod status;
mod ws;

const HAS_WIFI: bool = cfg!(has_wifi);
const RF_DEBUG_DISABLED_SLEEP_MS: u64 = 100;
const VALID_UNIX_TIME_THRESHOLD_MS: u64 = 946_684_800_000;

pub use context::{AppCtx, ConnectionState};
pub(crate) use control::{
    cancel_owned_manual_actions, local_ui_dispatcher, mqtt_dispatcher, pong_json,
    remote_control_dispatcher,
};
pub use runtime::{
    run_server, start_app_worker, start_transmission_worker, AppWorkerHandle,
    TransmissionWorkerHandle,
};
pub(crate) use state::{
    ActionKey, ActionOwner, ActiveActionHandle, AppCommand, AppEvent, DebugCtx, DomainState,
    HardwareCtx, MessageOrigin, RemoteControlUrlKind, SessionCtx, TransmissionCommand, WorkerCtx,
};
pub(crate) use status::{
    mqtt_status, parse_remote_control_url, remote_control_endpoint_url, remote_control_status,
};

type ControlResult = core::result::Result<Vec<String>, ControlError>;

struct RandomResolver<'a> {
    rng: &'a mut SmallRng,
}

impl crate::scheduling::StepResolver for RandomResolver<'_> {
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
        MessageOrigin::Mqtt => EventSource::Mqtt,
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
    serde_json::to_string(&crate::protocol::ServerMessage::Error {
        message: message.into(),
    })
    .unwrap()
}
