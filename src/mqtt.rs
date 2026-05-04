use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use esp_idf_svc::mqtt::client::{
    Details, EspMqttClient, EspMqttEvent, EventPayload, LwtConfiguration,
    MqttClientConfiguration, QoS,
};
use log::{error, info, warn};

use crate::protocol::{ClientMessage, Collar, CommandMode, DeviceSettings, Preset};
use crate::server::{
    cancel_owned_manual_actions, mqtt_dispatcher, mqtt_status, ActionOwner, AppCtx, AppEvent,
};

const DISABLED_POLL_INTERVAL_MS: u64 = 500;
const EVENT_LOOP_TICK_MS: u64 = 100;
const RECONNECT_BASE_DELAY_MS: u64 = 2_000;
const RECONNECT_MAX_DELAY_MS: u64 = 30_000;

pub struct MqttHandle {
    _join: std::thread::JoinHandle<()>,
}

pub fn start(ctx: AppCtx) -> Result<MqttHandle> {
    let join = std::thread::Builder::new()
        .name("mqtt".into())
        .stack_size(16384)
        .spawn(move || worker(ctx))?;
    Ok(MqttHandle { _join: join })
}

enum SessionExit {
    SettingsChanged,
    Disconnected { reason: String },
}

enum MqttEvent {
    Connected,
    Disconnected,
    Message { topic: String, payload: Vec<u8> },
}

fn worker(ctx: AppCtx) {
    let mut reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;

    loop {
        let settings = ctx.device_settings();
        if !settings.mqtt_enabled || settings.mqtt_server.trim().is_empty() {
            ctx.set_mqtt_status(mqtt_status(&settings, false, "Off"));
            reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
            std::thread::sleep(Duration::from_millis(DISABLED_POLL_INTERVAL_MS));
            continue;
        }

        let settings_revision = ctx.mqtt_settings_revision();
        ctx.set_mqtt_status(mqtt_status(&settings, false, "Connecting..."));

        match run_session(&ctx, &settings, settings_revision) {
            SessionExit::SettingsChanged => {
                cancel_owned_manual_actions(&ctx, ActionOwner::Mqtt);
                reconnect_delay_ms = RECONNECT_BASE_DELAY_MS;
            }
            SessionExit::Disconnected { reason } => {
                cancel_owned_manual_actions(&ctx, ActionOwner::Mqtt);
                warn!("MQTT disconnected: {reason}");
                let text = format!("Reconnecting in {reconnect_delay_ms}ms...");
                ctx.set_mqtt_status(mqtt_status(&settings, false, text));
                std::thread::sleep(Duration::from_millis(reconnect_delay_ms));
                reconnect_delay_ms = (reconnect_delay_ms * 2).min(RECONNECT_MAX_DELAY_MS);
            }
        }
    }
}

fn run_session(ctx: &AppCtx, settings: &DeviceSettings, settings_revision: u32) -> SessionExit {
    let device_id = &settings.device_id;
    let url = format!("mqtt://{}:{}", settings.mqtt_server.trim(), settings.mqtt_port);
    let availability_topic = format!("rusty_collars/{device_id}/availability");

    let lwt = LwtConfiguration {
        topic: &availability_topic,
        payload: b"offline",
        qos: QoS::AtLeastOnce,
        retain: true,
    };

    let username_str = settings.mqtt_username.trim();
    let password_str = settings.mqtt_password.trim();

    let mqtt_conf = MqttClientConfiguration {
        client_id: Some(device_id),
        lwt: Some(lwt),
        username: if username_str.is_empty() {
            None
        } else {
            Some(username_str)
        },
        password: if password_str.is_empty() {
            None
        } else {
            Some(password_str)
        },
        keep_alive_interval: Some(Duration::from_secs(30)),
        buffer_size: 2048,
        out_buffer_size: 2048,
        ..Default::default()
    };

    let (event_tx, event_rx) = mpsc::channel();
    let client = EspMqttClient::new_cb(&url, &mqtt_conf, move |event: EspMqttEvent<'_>| {
        let outgoing = match event.payload() {
            EventPayload::Connected(_) => Some(MqttEvent::Connected),
            EventPayload::Disconnected => Some(MqttEvent::Disconnected),
            EventPayload::Received {
                topic: Some(topic),
                data,
                details: Details::Complete,
                ..
            } => Some(MqttEvent::Message {
                topic: topic.to_string(),
                payload: data.to_vec(),
            }),
            _ => None,
        };
        if let Some(ev) = outgoing {
            let _ = event_tx.send(ev);
        }
    });

    let mut client = match client {
        Ok(client) => client,
        Err(err) => {
            return SessionExit::Disconnected {
                reason: format!("Connect failed: {err}"),
            }
        }
    };

    // Wait for Connected event
    let connected = wait_for_connect(&event_rx, settings_revision, ctx);
    match connected {
        Some(SessionExit::SettingsChanged) => return SessionExit::SettingsChanged,
        Some(exit) => return exit,
        None => {} // Connected successfully
    }

    info!("MQTT connected to {url}");
    ctx.set_mqtt_status(mqtt_status(settings, true, "Connected"));

    // Publish availability
    if let Err(err) = client.publish(&availability_topic, QoS::AtLeastOnce, true, b"online") {
        return SessionExit::Disconnected {
            reason: format!("Publish availability failed: {err}"),
        };
    }

    // Take initial snapshot and publish discovery
    let mut state = MqttState::new(ctx, device_id);
    if let Err(err) = state.publish_all_discovery(&mut client, device_id) {
        return SessionExit::Disconnected {
            reason: format!("Discovery publish failed: {err}"),
        };
    }

    // Subscribe to all command topics under our prefix
    let subscribe_topic = format!("rusty_collars/{device_id}/#");
    if let Err(err) = client.subscribe(&subscribe_topic, QoS::AtLeastOnce) {
        return SessionExit::Disconnected {
            reason: format!("Subscribe failed: {err}"),
        };
    }

    // Publish initial states
    if let Err(err) = state.publish_all_states(&mut client, device_id) {
        return SessionExit::Disconnected {
            reason: format!("State publish failed: {err}"),
        };
    }

    let mut broadcast_rx = ctx.new_broadcast_receiver();

    // Event loop
    loop {
        // Check settings revision
        if ctx.mqtt_settings_revision() != settings_revision {
            return SessionExit::SettingsChanged;
        }

        // Process MQTT events
        match event_rx.recv_timeout(Duration::from_millis(EVENT_LOOP_TICK_MS)) {
            Ok(MqttEvent::Disconnected) => {
                return SessionExit::Disconnected {
                    reason: "Broker disconnected".to_string(),
                };
            }
            Ok(MqttEvent::Message { topic, payload }) => {
                handle_incoming_message(
                    ctx,
                    &mut client,
                    &mut state,
                    device_id,
                    &topic,
                    &payload,
                );
            }
            Ok(MqttEvent::Connected) => {
                // Reconnected after brief interruption
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return SessionExit::Disconnected {
                    reason: "Event channel closed".to_string(),
                };
            }
        }

        // Process broadcast events
        while let Ok(event) = broadcast_rx.try_recv() {
            match &event {
                AppEvent::State {
                    collars,
                    presets,
                    preset_running,
                    ..
                } => {
                    // Check if collars/presets changed
                    if state.collars_changed(collars) || state.presets_changed(presets) {
                        state.update_entities(ctx, collars, presets);
                        if let Err(err) =
                            state.republish_discovery(&mut client, device_id, collars, presets)
                        {
                            error!("Discovery republish failed: {err}");
                        }
                    }
                    // Update preset_running state
                    let running = if preset_running.is_some() {
                        "ON"
                    } else {
                        "OFF"
                    };
                    let topic = format!("rusty_collars/{device_id}/preset_running/state");
                    let _ = client.publish(&topic, QoS::AtMostOnce, true, running.as_bytes());
                }
                AppEvent::ActionFired {
                    collar_name,
                    mode,
                    intensity,
                } => {
                    if let Some(slug) = state.collar_slug(collar_name) {
                        let mode_str = match mode {
                            CommandMode::Shock => "shock",
                            CommandMode::Vibrate => "vibrate",
                            CommandMode::Beep => "beep",
                        };
                        let topic =
                            format!("rusty_collars/{device_id}/collar/{slug}/event");
                        let payload = format!(
                            r#"{{"event_type":"{}","intensity":{}}}"#,
                            mode_str, intensity
                        );
                        let _ = client.publish(&topic, QoS::AtMostOnce, false, payload.as_bytes());
                    }
                }
                _ => {}
            }
        }
    }
}

fn wait_for_connect(
    event_rx: &mpsc::Receiver<MqttEvent>,
    settings_revision: u32,
    ctx: &AppCtx,
) -> Option<SessionExit> {
    loop {
        if ctx.mqtt_settings_revision() != settings_revision {
            return Some(SessionExit::SettingsChanged);
        }
        match event_rx.recv_timeout(Duration::from_millis(EVENT_LOOP_TICK_MS)) {
            Ok(MqttEvent::Connected) => return None,
            Ok(MqttEvent::Disconnected) => {
                return Some(SessionExit::Disconnected {
                    reason: "Disconnected while connecting".to_string(),
                });
            }
            Ok(MqttEvent::Message { .. }) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Some(SessionExit::Disconnected {
                    reason: "Event channel closed".to_string(),
                });
            }
        }
    }
}

// ----- MQTT State Tracking -----

const DEFAULT_ACTION_DURATION_MS: u32 = 500;

struct MqttState {
    collar_names: Vec<String>,
    collar_slugs: HashMap<String, String>,
    slug_to_collar: HashMap<String, String>,
    preset_names: Vec<String>,
    preset_slugs: HashMap<String, String>,
    slug_to_preset: HashMap<String, String>,
    intensities: HashMap<String, u8>,
    action_duration_ms: u32,
}

impl MqttState {
    fn new(ctx: &AppCtx, _device_id: &str) -> Self {
        let (collars, presets) = ctx.with_domain(|d| {
            (d.collars.clone(), d.presets.clone())
        });

        let mut state = Self {
            collar_names: Vec::new(),
            collar_slugs: HashMap::new(),
            slug_to_collar: HashMap::new(),
            preset_names: Vec::new(),
            preset_slugs: HashMap::new(),
            slug_to_preset: HashMap::new(),
            intensities: HashMap::new(),
            action_duration_ms: DEFAULT_ACTION_DURATION_MS,
        };
        state.rebuild_collars(&collars);
        state.rebuild_presets(&presets);
        state
    }

    fn rebuild_collars(&mut self, collars: &[Collar]) {
        self.collar_names = collars.iter().map(|c| c.name.clone()).collect();
        self.collar_slugs.clear();
        self.slug_to_collar.clear();
        for collar in collars {
            let slug = slugify(&collar.name);
            self.collar_slugs.insert(collar.name.clone(), slug.clone());
            self.slug_to_collar.insert(slug.clone(), collar.name.clone());
            self.intensities.entry(collar.name.clone()).or_insert(1);
        }
    }

    fn rebuild_presets(&mut self, presets: &[Preset]) {
        self.preset_names = presets.iter().map(|p| p.name.clone()).collect();
        self.preset_slugs.clear();
        self.slug_to_preset.clear();
        for preset in presets {
            let slug = slugify(&preset.name);
            self.preset_slugs.insert(preset.name.clone(), slug.clone());
            self.slug_to_preset.insert(slug.clone(), preset.name.clone());
        }
    }

    fn collars_changed(&self, collars: &[Collar]) -> bool {
        let names: Vec<_> = collars.iter().map(|c| &c.name).collect();
        let current: Vec<_> = self.collar_names.iter().collect();
        names != current
    }

    fn presets_changed(&self, presets: &[Preset]) -> bool {
        let names: Vec<_> = presets.iter().map(|p| &p.name).collect();
        let current: Vec<_> = self.preset_names.iter().collect();
        names != current
    }

    fn collar_slug(&self, name: &str) -> Option<String> {
        self.collar_slugs.get(name).cloned()
    }

    fn update_entities(&mut self, _ctx: &AppCtx, collars: &[Collar], presets: &[Preset]) {
        self.rebuild_collars(collars);
        self.rebuild_presets(presets);
    }

    fn publish_all_discovery(
        &self,
        client: &mut EspMqttClient<'static>,
        device_id: &str,
    ) -> Result<(), esp_idf_svc::sys::EspError> {
        let device_json = device_block(device_id);
        let availability_json = availability_block(device_id);

        for (name, slug) in &self.collar_slugs {
            publish_collar_discovery(client, device_id, name, slug, &device_json, &availability_json)?;
        }
        for (name, slug) in &self.preset_slugs {
            publish_preset_discovery(client, device_id, name, slug, &device_json, &availability_json)?;
        }
        publish_preset_running_discovery(client, device_id, &device_json, &availability_json)?;
        publish_action_duration_discovery(client, device_id, &device_json, &availability_json)?;
        Ok(())
    }

    fn publish_all_states(
        &self,
        client: &mut EspMqttClient<'static>,
        device_id: &str,
    ) -> Result<(), esp_idf_svc::sys::EspError> {
        // Publish intensity states
        for (name, slug) in &self.collar_slugs {
            let intensity = self.intensities.get(name).copied().unwrap_or(1);
            let topic = format!("rusty_collars/{device_id}/collar/{slug}/intensity/state");
            client.publish(&topic, QoS::AtLeastOnce, true, intensity.to_string().as_bytes())?;
        }
        // Publish preset_running state
        let topic = format!("rusty_collars/{device_id}/preset_running/state");
        client.publish(&topic, QoS::AtLeastOnce, true, b"OFF")?;
        // Publish action duration state
        let topic = format!("rusty_collars/{device_id}/action_duration/state");
        client.publish(&topic, QoS::AtLeastOnce, true, self.action_duration_ms.to_string().as_bytes())?;
        Ok(())
    }

    fn republish_discovery(
        &self,
        client: &mut EspMqttClient<'static>,
        device_id: &str,
        new_collars: &[Collar],
        new_presets: &[Preset],
    ) -> Result<(), esp_idf_svc::sys::EspError> {
        // Remove stale collar discovery configs
        let new_collar_slugs: HashMap<_, _> = new_collars
            .iter()
            .map(|c| (c.name.clone(), slugify(&c.name)))
            .collect();
        for (_, old_slug) in &self.collar_slugs {
            if !new_collar_slugs.values().any(|s| s == old_slug) {
                remove_collar_discovery(client, device_id, old_slug)?;
            }
        }

        // Remove stale preset discovery configs
        let new_preset_slugs: HashMap<_, _> = new_presets
            .iter()
            .map(|p| (p.name.clone(), slugify(&p.name)))
            .collect();
        for (_, old_slug) in &self.preset_slugs {
            if !new_preset_slugs.values().any(|s| s == old_slug) {
                remove_preset_discovery(client, device_id, old_slug)?;
            }
        }

        // Publish new discovery configs
        let device_json = device_block(device_id);
        let availability_json = availability_block(device_id);
        for (name, slug) in &new_collar_slugs {
            publish_collar_discovery(client, device_id, name, slug, &device_json, &availability_json)?;
        }
        for (name, slug) in &new_preset_slugs {
            publish_preset_discovery(client, device_id, name, slug, &device_json, &availability_json)?;
        }

        // Publish states for new collars
        for (name, slug) in &new_collar_slugs {
            let intensity = self.intensities.get(name).copied().unwrap_or(1);
            let topic = format!("rusty_collars/{device_id}/collar/{slug}/intensity/state");
            client.publish(&topic, QoS::AtLeastOnce, true, intensity.to_string().as_bytes())?;
        }

        Ok(())
    }
}

// ----- Incoming Message Handling -----

fn handle_incoming_message(
    ctx: &AppCtx,
    client: &mut EspMqttClient<'static>,
    state: &mut MqttState,
    device_id: &str,
    topic: &str,
    payload: &[u8],
) {
    let prefix = format!("rusty_collars/{device_id}/");
    let Some(suffix) = topic.strip_prefix(&prefix) else {
        return;
    };

    // Only handle /set command topics
    let Some(suffix) = suffix.strip_suffix("/set") else {
        return;
    };

    let parts: Vec<&str> = suffix.split('/').collect();
    match parts.as_slice() {
        // collar/{slug}/intensity
        ["collar", slug, "intensity"] => {
            let payload_str = core::str::from_utf8(payload).unwrap_or("");
            if let Ok(intensity) = payload_str.trim().parse::<u8>() {
                let intensity = intensity.min(99);
                if let Some(name) = state.slug_to_collar.get(*slug) {
                    state.intensities.insert(name.clone(), intensity);
                    let state_topic = format!(
                        "rusty_collars/{device_id}/collar/{slug}/intensity/state"
                    );
                    let _ = client.publish(
                        &state_topic,
                        QoS::AtLeastOnce,
                        true,
                        intensity.to_string().as_bytes(),
                    );
                }
            }
        }
        // collar/{slug}/{shock|vibrate|beep}
        ["collar", slug, mode_str] => {
            let mode = match *mode_str {
                "shock" => Some(CommandMode::Shock),
                "vibrate" => Some(CommandMode::Vibrate),
                "beep" => Some(CommandMode::Beep),
                _ => None,
            };
            if let (Some(mode), Some(collar_name)) =
                (mode, state.slug_to_collar.get(*slug).cloned())
            {
                let intensity = state.intensities.get(&collar_name).copied().unwrap_or(1);
                let msg = ClientMessage::RunAction {
                    collar_name,
                    mode,
                    intensity,
                    duration_ms: state.action_duration_ms,
                    intensity_max: None,
                    duration_max_ms: None,
                    intensity_distribution: None,
                    duration_distribution: None,
                };
                let dispatcher = mqtt_dispatcher(ctx);
                if let Err(err) = dispatcher.handle(msg) {
                    error!("MQTT action dispatch failed: {err}");
                }
            }
        }
        // preset/{slug}
        ["preset", slug] => {
            if let Some(preset_name) = state.slug_to_preset.get(*slug).cloned() {
                let msg = ClientMessage::RunPreset { name: preset_name };
                let dispatcher = mqtt_dispatcher(ctx);
                if let Err(err) = dispatcher.handle(msg) {
                    error!("MQTT preset dispatch failed: {err}");
                }
            }
        }
        // action_duration
        ["action_duration"] => {
            let payload_str = core::str::from_utf8(payload).unwrap_or("");
            if let Ok(duration) = payload_str.trim().parse::<u32>() {
                let duration = duration.clamp(100, 10000);
                state.action_duration_ms = duration;
                let state_topic = format!("rusty_collars/{device_id}/action_duration/state");
                let _ = client.publish(
                    &state_topic,
                    QoS::AtLeastOnce,
                    true,
                    duration.to_string().as_bytes(),
                );
            }
        }
        _ => {}
    }
}

// ----- HA Discovery Helpers -----

fn device_block(device_id: &str) -> String {
    format!(
        r#""device":{{"identifiers":["{}"],"name":"Rusty Collars","model":"ESP32","manufacturer":"rusty-collars"}}"#,
        device_id
    )
}

fn availability_block(device_id: &str) -> String {
    format!(
        r#""availability":[{{"topic":"rusty_collars/{}/availability","payload_available":"online","payload_not_available":"offline"}}]"#,
        device_id
    )
}

fn publish_collar_discovery(
    client: &mut EspMqttClient<'static>,
    device_id: &str,
    collar_name: &str,
    slug: &str,
    device_json: &str,
    availability_json: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    // Number entity: intensity
    {
        let topic = format!("homeassistant/number/{device_id}/{slug}_intensity/config");
        let payload = format!(
            r#"{{{device_json},{availability_json},"name":"{collar_name} Intensity","unique_id":"{device_id}_{slug}_intensity","state_topic":"rusty_collars/{device_id}/collar/{slug}/intensity/state","command_topic":"rusty_collars/{device_id}/collar/{slug}/intensity/set","min":0,"max":99,"step":1,"mode":"slider"}}"#,
        );
        client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())?;
    }

    // Button entities: shock, vibrate, beep
    for mode in &["shock", "vibrate", "beep"] {
        let display_name = match *mode {
            "shock" => "Shock",
            "vibrate" => "Vibrate",
            "beep" => "Beep",
            _ => unreachable!(),
        };
        let topic = format!("homeassistant/button/{device_id}/{slug}_{mode}/config");
        let payload = format!(
            r#"{{{device_json},{availability_json},"name":"{collar_name} {display_name}","unique_id":"{device_id}_{slug}_{mode}","command_topic":"rusty_collars/{device_id}/collar/{slug}/{mode}/set","payload_press":"PRESS"}}"#,
        );
        client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())?;
    }

    // Event entity: activity
    {
        let topic = format!("homeassistant/event/{device_id}/{slug}_activity/config");
        let payload = format!(
            r#"{{{device_json},{availability_json},"name":"{collar_name} Activity","unique_id":"{device_id}_{slug}_activity","state_topic":"rusty_collars/{device_id}/collar/{slug}/event","event_types":["shock","vibrate","beep"]}}"#,
        );
        client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())?;
    }

    Ok(())
}

fn remove_collar_discovery(
    client: &mut EspMqttClient<'static>,
    device_id: &str,
    slug: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let topics = [
        format!("homeassistant/number/{device_id}/{slug}_intensity/config"),
        format!("homeassistant/button/{device_id}/{slug}_shock/config"),
        format!("homeassistant/button/{device_id}/{slug}_vibrate/config"),
        format!("homeassistant/button/{device_id}/{slug}_beep/config"),
        format!("homeassistant/event/{device_id}/{slug}_activity/config"),
    ];
    for topic in &topics {
        client.publish(topic, QoS::AtLeastOnce, true, b"")?;
    }
    Ok(())
}

fn publish_preset_discovery(
    client: &mut EspMqttClient<'static>,
    device_id: &str,
    preset_name: &str,
    slug: &str,
    device_json: &str,
    availability_json: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let topic = format!("homeassistant/button/{device_id}/preset_{slug}/config");
    let payload = format!(
        r#"{{{device_json},{availability_json},"name":"Preset: {preset_name}","unique_id":"{device_id}_preset_{slug}","command_topic":"rusty_collars/{device_id}/preset/{slug}/set","payload_press":"PRESS"}}"#,
    );
    client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())
        .map(|_| ())
}

fn remove_preset_discovery(
    client: &mut EspMqttClient<'static>,
    device_id: &str,
    slug: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let topic = format!("homeassistant/button/{device_id}/preset_{slug}/config");
    client.publish(&topic, QoS::AtLeastOnce, true, b"")
        .map(|_| ())
}

fn publish_preset_running_discovery(
    client: &mut EspMqttClient<'static>,
    device_id: &str,
    device_json: &str,
    availability_json: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let topic = format!("homeassistant/binary_sensor/{device_id}/preset_running/config");
    let payload = format!(
        r#"{{{device_json},{availability_json},"name":"Preset Running","unique_id":"{device_id}_preset_running","state_topic":"rusty_collars/{device_id}/preset_running/state","payload_on":"ON","payload_off":"OFF"}}"#,
    );
    client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())
        .map(|_| ())
}

fn publish_action_duration_discovery(
    client: &mut EspMqttClient<'static>,
    device_id: &str,
    device_json: &str,
    availability_json: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let topic = format!("homeassistant/number/{device_id}/action_duration/config");
    let payload = format!(
        r#"{{{device_json},{availability_json},"name":"Action Duration","unique_id":"{device_id}_action_duration","state_topic":"rusty_collars/{device_id}/action_duration/state","command_topic":"rusty_collars/{device_id}/action_duration/set","min":100,"max":10000,"step":100,"unit_of_measurement":"ms","mode":"slider","icon":"mdi:timer-outline"}}"#,
    );
    client.publish(&topic, QoS::AtLeastOnce, true, payload.as_bytes())
        .map(|_| ())
}

// ----- Utilities -----

fn slugify(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut prev_underscore = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            result.push('_');
            prev_underscore = true;
        }
    }
    if result.ends_with('_') {
        result.pop();
    }
    if result.is_empty() {
        result.push_str("unnamed");
    }
    result
}
