use std::collections::HashMap;

use anyhow::Result;

use rusty_collars_core::protocol::{
    Collar, DeviceSettings, EventLogEntry, EventLogEntryKind, EventSource, ExportData, MqttStatus,
    Preset, RemoteControlStatus, RfDebugFrame,
};
use rusty_collars_core::validation;
use url::Url;

use crate::{
    AppRepository, CollarRepository, ControlError, DomainState, PresetRepository,
    SettingsRepository, SharedRepository,
};

const MAX_EVENT_LOG_ENTRIES: usize = 100;
const MAX_RF_DEBUG_EVENTS: usize = 100;
const RF_STOP_LOCKOUT_MS: u64 = 10_000;

#[derive(Clone)]
pub struct RepositoryServices {
    repository: SharedRepository,
}

impl RepositoryServices {
    pub fn new(repository: SharedRepository) -> Self {
        Self { repository }
    }

    pub fn ensure_device_id(&self, settings: &mut DeviceSettings) -> Result<(), ControlError> {
        self.with_repository("ensure device id", |repository| {
            SettingsRepository::ensure_device_id(repository, settings)
        })
    }

    pub fn load_settings(&self) -> Result<DeviceSettings, ControlError> {
        self.with_repository("load settings", |repository| {
            SettingsRepository::load_settings(repository)
        })
    }

    pub fn save_settings(&self, settings: &DeviceSettings) -> Result<(), ControlError> {
        self.with_repository("save settings", |repository| {
            SettingsRepository::save_settings(repository, settings)
        })
    }

    pub fn load_collars(&self) -> Result<Vec<Collar>, ControlError> {
        self.with_repository("load collars", |repository| {
            CollarRepository::load_collars(repository)
        })
    }

    pub fn save_collars(&self, collars: &[Collar]) -> Result<(), ControlError> {
        self.with_repository("save collars", |repository| {
            CollarRepository::save_collars(repository, collars)
        })
    }

    pub fn load_presets(&self) -> Result<Vec<Preset>, ControlError> {
        self.with_repository("load presets", |repository| {
            PresetRepository::load_presets(repository)
        })
    }

    pub fn save_presets(&self, presets: &[Preset]) -> Result<(), ControlError> {
        self.with_repository("save presets", |repository| {
            PresetRepository::save_presets(repository, presets)
        })
    }

    fn with_repository<T>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&mut dyn AppRepository) -> Result<T>,
    ) -> Result<T, ControlError> {
        let mut repository = self.repository.lock().unwrap();
        f(repository.as_mut()).map_err(|source| ControlError::Persistence { operation, source })
    }
}

#[derive(Debug, Clone)]
pub struct CollarChange {
    pub collars: Vec<Collar>,
    pub presets: Option<Vec<Preset>>,
    pub preset_stopped: bool,
    pub cancel_manual_actions: bool,
}

pub struct CollarService;

impl CollarService {
    pub fn add(domain: &mut DomainState, collar: Collar) -> Result<CollarChange, ControlError> {
        validation::validate_collar(&collar)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        if domain
            .collars
            .iter()
            .any(|existing| existing.name == collar.name)
        {
            return Err(ControlError::DuplicateCollar(collar.name));
        }

        domain.collars.push(collar);
        Ok(CollarChange {
            collars: domain.collars.clone(),
            presets: None,
            preset_stopped: false,
            cancel_manual_actions: false,
        })
    }

    pub fn update(
        domain: &mut DomainState,
        original_name: String,
        updated: Collar,
    ) -> Result<CollarChange, ControlError> {
        let Some(index) = domain
            .collars
            .iter()
            .position(|collar| collar.name == original_name)
        else {
            return Err(ControlError::UnknownCollar(original_name));
        };

        validation::validate_collar(&updated)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        if domain
            .collars
            .iter()
            .enumerate()
            .any(|(existing_index, collar)| existing_index != index && collar.name == updated.name)
        {
            return Err(ControlError::DuplicateCollar(updated.name));
        }

        let renamed = original_name != updated.name;
        let updated_name = updated.name.clone();
        domain.collars[index] = updated;
        if renamed {
            for preset in &mut domain.presets {
                for track in &mut preset.tracks {
                    if track.collar_name == original_name {
                        track.collar_name = updated_name.clone();
                    }
                }
            }
        }

        let preset_stopped = stop_active_preset(domain);
        Ok(CollarChange {
            collars: domain.collars.clone(),
            presets: renamed.then(|| domain.presets.clone()),
            preset_stopped,
            cancel_manual_actions: true,
        })
    }

    pub fn delete(domain: &mut DomainState, name: String) -> Result<CollarChange, ControlError> {
        if domain
            .presets
            .iter()
            .any(|preset| preset.tracks.iter().any(|track| track.collar_name == name))
        {
            return Err(ControlError::CollarReferencedByPreset(name));
        }

        let before = domain.collars.len();
        domain.collars.retain(|collar| collar.name != name);
        if domain.collars.len() == before {
            return Err(ControlError::UnknownCollar(name));
        }

        let preset_stopped = stop_active_preset(domain);
        Ok(CollarChange {
            collars: domain.collars.clone(),
            presets: None,
            preset_stopped,
            cancel_manual_actions: true,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PresetChange {
    pub presets: Vec<Preset>,
    pub preset_stopped: bool,
}

pub struct PresetService;

impl PresetService {
    pub fn save(
        domain: &mut DomainState,
        original_name: Option<String>,
        preset: Preset,
    ) -> Result<PresetChange, ControlError> {
        validation::validate_preset(&preset, &domain.collars)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        let original_name = original_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let mut updated = domain.presets.clone();
        if let Some(original_name) = original_name {
            let Some(index) = updated
                .iter()
                .position(|existing| existing.name == original_name)
            else {
                return Err(ControlError::UnknownPreset(original_name.to_string()));
            };
            if updated
                .iter()
                .enumerate()
                .any(|(existing_index, existing)| {
                    existing_index != index && existing.name == preset.name
                })
            {
                return Err(ControlError::DuplicatePreset(preset.name));
            }
            updated[index] = preset;
        } else if let Some(existing) = updated
            .iter_mut()
            .find(|existing| existing.name == preset.name)
        {
            *existing = preset;
        } else {
            updated.push(preset);
        }

        validation::validate_presets(&updated, &domain.collars)
            .map_err(|err| ControlError::Validation(err.to_string()))?;
        let preset_stopped = stop_active_preset(domain);
        domain.presets = updated;
        Ok(PresetChange {
            presets: domain.presets.clone(),
            preset_stopped,
        })
    }

    pub fn delete(domain: &mut DomainState, name: String) -> Result<PresetChange, ControlError> {
        let before = domain.presets.len();
        domain.presets.retain(|preset| preset.name != name);
        if domain.presets.len() == before {
            return Err(ControlError::UnknownPreset(name));
        }

        let preset_stopped = stop_active_preset(domain);
        Ok(PresetChange {
            presets: domain.presets.clone(),
            preset_stopped,
        })
    }

    pub fn reorder(domain: &mut DomainState, names: Vec<String>) -> PresetChange {
        let order_by_name: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(idx, name)| (name.as_str(), idx))
            .collect();
        let mut reordered_slots = vec![None; names.len()];
        let mut remaining = Vec::with_capacity(domain.presets.len());
        for preset in domain.presets.drain(..) {
            match order_by_name.get(preset.name.as_str()) {
                Some(&idx) if reordered_slots[idx].is_none() => {
                    reordered_slots[idx] = Some(preset);
                }
                _ => remaining.push(preset),
            }
        }
        let mut reordered = Vec::with_capacity(remaining.len() + names.len());
        reordered.extend(reordered_slots.into_iter().flatten());
        reordered.extend(remaining);
        domain.presets = reordered;

        PresetChange {
            presets: domain.presets.clone(),
            preset_stopped: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DataChange {
    pub collars: Vec<Collar>,
    pub presets: Vec<Preset>,
    pub preset_stopped: bool,
    pub cancel_manual_actions: bool,
}

pub struct DataService;

impl DataService {
    pub fn import(domain: &mut DomainState, data: ExportData) -> DataChange {
        let preset_stopped = stop_active_preset(domain);
        domain.collars = data.collars;
        domain.presets = data.presets;
        DataChange {
            collars: domain.collars.clone(),
            presets: domain.presets.clone(),
            preset_stopped,
            cancel_manual_actions: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsChange {
    pub settings: DeviceSettings,
    pub reboot_required: bool,
    pub remote_settings_changed: bool,
    pub mqtt_settings_changed: bool,
    pub event_log_changed: bool,
}

pub struct SettingsService;

impl SettingsService {
    pub fn apply(domain: &mut DomainState, settings: DeviceSettings) -> SettingsChange {
        let previous_settings = domain.device_settings.clone();
        let reboot_required = device_settings_reboot_required(&previous_settings, &settings);
        let remote_settings_changed = previous_settings.remote_control_enabled
            != settings.remote_control_enabled
            || previous_settings.remote_control_url != settings.remote_control_url
            || previous_settings.remote_control_validate_cert
                != settings.remote_control_validate_cert;
        let mqtt_settings_changed = previous_settings.mqtt_enabled != settings.mqtt_enabled
            || previous_settings.mqtt_server != settings.mqtt_server
            || previous_settings.mqtt_port != settings.mqtt_port
            || previous_settings.mqtt_username != settings.mqtt_username
            || previous_settings.mqtt_password != settings.mqtt_password;
        let event_log_changed = previous_settings.record_event_log != settings.record_event_log;

        domain.device_settings = settings.clone();
        if remote_settings_changed {
            domain.remote_control_status =
                remote_control_status_from_settings(&domain.device_settings);
        }
        if mqtt_settings_changed {
            domain.mqtt_status = mqtt_status_from_settings(&domain.device_settings);
        }
        if !domain.device_settings.record_event_log {
            domain.event_log_events.clear();
        }

        SettingsChange {
            settings,
            reboot_required,
            remote_settings_changed,
            mqtt_settings_changed,
            event_log_changed,
        }
    }
}

pub struct ExecutionService;

impl ExecutionService {
    pub fn start_preset(domain: &mut DomainState, preset_name: String) {
        domain.preset_name = Some(preset_name);
    }

    pub fn stop_preset(domain: &mut DomainState) -> bool {
        stop_active_preset(domain)
    }

    pub fn stop_all(domain: &mut DomainState, now_ms: u64) {
        domain.preset_name = None;
        domain.rf_lockout_until_ms = now_ms + RF_STOP_LOCKOUT_MS;
    }

    pub fn complete_preset(domain: &mut DomainState, preset_name: &str) -> bool {
        if domain.preset_name.as_deref() == Some(preset_name) {
            domain.preset_name = None;
            true
        } else {
            false
        }
    }
}

pub struct RemoteControlService;

impl RemoteControlService {
    pub fn set_status(domain: &mut DomainState, status: RemoteControlStatus) -> bool {
        if domain.remote_control_status == status {
            false
        } else {
            domain.remote_control_status = status;
            true
        }
    }
}

pub struct MqttService;

impl MqttService {
    pub fn set_status(domain: &mut DomainState, status: MqttStatus) -> bool {
        if domain.mqtt_status == status {
            false
        } else {
            domain.mqtt_status = status;
            true
        }
    }
}

pub struct EventLogService;

impl EventLogService {
    pub fn append(
        domain: &mut DomainState,
        next_sequence: impl FnOnce() -> u64,
        monotonic_ms: u64,
        unix_ms: Option<u64>,
        source: EventSource,
        kind: EventLogEntryKind,
    ) -> Option<EventLogEntry> {
        if !domain.device_settings.record_event_log {
            return None;
        }

        let entry = EventLogEntry {
            sequence: next_sequence(),
            monotonic_ms,
            unix_ms,
            source,
            kind,
        };
        domain.event_log_events.push(entry.clone());
        if domain.event_log_events.len() > MAX_EVENT_LOG_ENTRIES {
            let excess = domain.event_log_events.len() - MAX_EVENT_LOG_ENTRIES;
            domain.event_log_events.drain(0..excess);
        }
        Some(entry)
    }
}

pub struct RfDebugService;

impl RfDebugService {
    pub fn push_event(domain: &mut DomainState, event: RfDebugFrame) {
        domain.rf_debug_events.push_back(event);
        if domain.rf_debug_events.len() > MAX_RF_DEBUG_EVENTS {
            domain.rf_debug_events.pop_front();
        }
    }

    pub fn clear_events(domain: &mut DomainState) {
        domain.rf_debug_events.clear();
    }
}

fn stop_active_preset(domain: &mut DomainState) -> bool {
    if domain.preset_name.is_some() {
        domain.preset_name = None;
        true
    } else {
        false
    }
}

/// Exhaustive destructuring ensures adding a new field to DeviceSettings
/// fails to compile until explicitly classified here.
fn device_settings_reboot_required(previous: &DeviceSettings, next: &DeviceSettings) -> bool {
    let DeviceSettings {
        // Hardware fields — changes require reboot
        tx_led_pin,
        rx_led_pin,
        rf_tx_pin,
        rf_rx_pin,
        wifi_client_enabled,
        wifi_ssid,
        wifi_password,
        ap_enabled,
        ap_password,
        max_clients,
        ntp_enabled,
        ntp_server,
        // Hot-reloadable fields — no reboot needed
        device_id: _,
        remote_control_enabled: _,
        remote_control_url: _,
        remote_control_validate_cert: _,
        record_event_log: _,
        mqtt_enabled: _,
        mqtt_server: _,
        mqtt_port: _,
        mqtt_username: _,
        mqtt_password: _,
    } = previous;

    *tx_led_pin != next.tx_led_pin
        || *rx_led_pin != next.rx_led_pin
        || *rf_tx_pin != next.rf_tx_pin
        || *rf_rx_pin != next.rf_rx_pin
        || *wifi_client_enabled != next.wifi_client_enabled
        || *wifi_ssid != next.wifi_ssid
        || *wifi_password != next.wifi_password
        || *ap_enabled != next.ap_enabled
        || *ap_password != next.ap_password
        || *max_clients != next.max_clients
        || *ntp_enabled != next.ntp_enabled
        || *ntp_server != next.ntp_server
}

fn remote_control_status_from_settings(settings: &DeviceSettings) -> RemoteControlStatus {
    let trimmed_url = settings.remote_control_url.trim();
    let status_text = if !settings.remote_control_enabled {
        "Off"
    } else if trimmed_url.is_empty() {
        "Missing URL"
    } else if parse_remote_control_url(trimmed_url).is_err() {
        "Invalid URL"
    } else {
        "Connecting..."
    };

    RemoteControlStatus {
        enabled: settings.remote_control_enabled,
        connected: false,
        url: trimmed_url.to_string(),
        validate_cert: settings.remote_control_validate_cert,
        rtt_ms: None,
        status_text: status_text.to_string(),
    }
}

fn mqtt_status_from_settings(settings: &DeviceSettings) -> MqttStatus {
    let status_text = if !settings.mqtt_enabled {
        "Off"
    } else if settings.mqtt_server.trim().is_empty() {
        "Missing server"
    } else {
        "Connecting..."
    };

    MqttStatus {
        enabled: settings.mqtt_enabled,
        connected: false,
        server: settings.mqtt_server.trim().to_string(),
        status_text: status_text.to_string(),
    }
}

fn parse_remote_control_url(url: &str) -> Result<(), ()> {
    let parsed = Url::parse(url).map_err(|_| ())?;
    match parsed.scheme() {
        "ws" | "wss" => {}
        _ => return Err(()),
    }
    if parsed.host_str().is_none() {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use anyhow::{anyhow, Result};

    use rusty_collars_core::protocol::{
        Collar, CommandMode, DeviceSettings, Distribution, EventLogEntryKind, EventSource, Preset,
        PresetStep, PresetStepMode, PresetTrack, RemoteControlStatus, RfDebugFrame,
    };

    use super::{
        CollarService, DataService, EventLogService, ExecutionService, PresetService,
        RemoteControlService, RepositoryServices, RfDebugService, SettingsService,
    };
    use crate::{
        AppRepository, CollarRepository, ControlError, DomainState, PresetRepository,
        SettingsRepository, SharedRepository,
    };

    #[derive(Default)]
    struct FakeRepoState {
        settings: DeviceSettings,
        collars: Vec<Collar>,
        presets: Vec<Preset>,
        fail_operation: Option<&'static str>,
    }

    struct FakeRepository {
        state: Arc<Mutex<FakeRepoState>>,
    }

    impl FakeRepository {
        fn new(state: Arc<Mutex<FakeRepoState>>) -> Self {
            Self { state }
        }

        fn maybe_fail(&self, operation: &'static str) -> Result<()> {
            if self.state.lock().unwrap().fail_operation == Some(operation) {
                Err(anyhow!("boom"))
            } else {
                Ok(())
            }
        }
    }

    impl SettingsRepository for FakeRepository {
        fn ensure_device_id(&mut self, settings: &mut DeviceSettings) -> Result<()> {
            self.maybe_fail("ensure device id")?;
            if settings.device_id.is_empty() {
                settings.device_id = "generated-id".to_string();
            }
            self.state.lock().unwrap().settings = settings.clone();
            Ok(())
        }

        fn load_settings(&mut self) -> Result<DeviceSettings> {
            self.maybe_fail("load settings")?;
            Ok(self.state.lock().unwrap().settings.clone())
        }

        fn save_settings(&mut self, settings: &DeviceSettings) -> Result<()> {
            self.maybe_fail("save settings")?;
            self.state.lock().unwrap().settings = settings.clone();
            Ok(())
        }
    }

    impl CollarRepository for FakeRepository {
        fn load_collars(&mut self) -> Result<Vec<Collar>> {
            self.maybe_fail("load collars")?;
            Ok(self.state.lock().unwrap().collars.clone())
        }

        fn save_collars(&mut self, collars: &[Collar]) -> Result<()> {
            self.maybe_fail("save collars")?;
            self.state.lock().unwrap().collars = collars.to_vec();
            Ok(())
        }
    }

    impl PresetRepository for FakeRepository {
        fn load_presets(&mut self) -> Result<Vec<Preset>> {
            self.maybe_fail("load presets")?;
            Ok(self.state.lock().unwrap().presets.clone())
        }

        fn save_presets(&mut self, presets: &[Preset]) -> Result<()> {
            self.maybe_fail("save presets")?;
            self.state.lock().unwrap().presets = presets.to_vec();
            Ok(())
        }
    }

    fn test_repository(state: Arc<Mutex<FakeRepoState>>) -> SharedRepository {
        Arc::new(Mutex::new(
            Box::new(FakeRepository::new(state)) as Box<dyn AppRepository>
        ))
    }

    fn sample_collar(name: &str) -> Collar {
        Collar {
            name: name.to_string(),
            collar_id: 42,
            channel: 1,
        }
    }

    fn sample_preset(name: &str, collar_name: &str) -> Preset {
        Preset {
            name: name.to_string(),
            tracks: vec![PresetTrack {
                collar_name: collar_name.to_string(),
                steps: vec![PresetStep {
                    mode: PresetStepMode::Shock,
                    intensity: 5,
                    duration_ms: 500,
                    intensity_max: None,
                    duration_max_ms: None,
                    intensity_distribution: Some(Distribution::Uniform),
                    duration_distribution: None,
                }],
            }],
        }
    }

    fn domain_state() -> DomainState {
        DomainState {
            device_settings: DeviceSettings::default(),
            collars: vec![sample_collar("alpha")],
            presets: vec![sample_preset("preset", "alpha")],
            preset_name: Some("preset".to_string()),
            rf_lockout_until_ms: 0,
            rf_debug_events: VecDeque::new(),
            event_log_events: Vec::new(),
            remote_control_status: RemoteControlStatus::default(),
            mqtt_status: Default::default(),
        }
    }

    #[test]
    fn repository_services_round_trip() {
        let state = Arc::new(Mutex::new(FakeRepoState {
            settings: DeviceSettings::default(),
            collars: vec![sample_collar("alpha")],
            presets: vec![sample_preset("preset", "alpha")],
            fail_operation: None,
        }));
        let services = RepositoryServices::new(test_repository(state.clone()));

        let mut settings = services.load_settings().unwrap();
        services.ensure_device_id(&mut settings).unwrap();
        services.save_settings(&settings).unwrap();
        services.save_collars(&[sample_collar("beta")]).unwrap();
        services
            .save_presets(&[sample_preset("new-preset", "beta")])
            .unwrap();

        let state = state.lock().unwrap();
        assert_eq!(state.settings.device_id, "generated-id");
        assert_eq!(state.collars[0].name, "beta");
        assert_eq!(state.presets[0].name, "new-preset");
    }

    #[test]
    fn repository_services_map_errors() {
        let state = Arc::new(Mutex::new(FakeRepoState {
            fail_operation: Some("save collars"),
            ..FakeRepoState::default()
        }));
        let services = RepositoryServices::new(test_repository(state));
        let error = services
            .save_collars(&[sample_collar("alpha")])
            .unwrap_err();

        match error {
            ControlError::Persistence { operation, .. } => {
                assert_eq!(operation, "save collars");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn collar_service_rename_updates_presets_and_stops_active_run() {
        let mut domain = domain_state();
        let updated = Collar {
            name: "beta".to_string(),
            ..sample_collar("alpha")
        };

        let change = CollarService::update(&mut domain, "alpha".to_string(), updated).unwrap();

        assert_eq!(change.collars[0].name, "beta");
        assert_eq!(
            change.presets.as_ref().unwrap()[0].tracks[0].collar_name,
            "beta"
        );
        assert!(change.preset_stopped);
        assert!(change.cancel_manual_actions);
        assert!(domain.preset_name.is_none());
    }

    #[test]
    fn settings_service_updates_remote_state_and_clears_event_log() {
        let mut domain = domain_state();
        domain.device_settings.record_event_log = true;
        EventLogService::append(
            &mut domain,
            || 1,
            10,
            Some(20),
            EventSource::System,
            EventLogEntryKind::NtpSync {
                server: "pool.ntp.org".to_string(),
            },
        )
        .unwrap();

        let mut settings = domain.device_settings.clone();
        settings.remote_control_enabled = true;
        settings.remote_control_url = "wss://example.com/ws".to_string();
        settings.record_event_log = false;

        let change = SettingsService::apply(&mut domain, settings.clone());

        assert!(change.remote_settings_changed);
        assert!(change.event_log_changed);
        assert!(domain.event_log_events.is_empty());
        assert_eq!(domain.remote_control_status.status_text, "Connecting...");
        assert_eq!(change.settings, settings);
    }

    #[test]
    fn event_log_service_skips_when_disabled_and_caps_entries() {
        let mut domain = domain_state();
        domain.device_settings.record_event_log = false;
        assert!(EventLogService::append(
            &mut domain,
            || 1,
            10,
            None,
            EventSource::LocalUi,
            EventLogEntryKind::Action {
                collar_name: "alpha".to_string(),
                mode: CommandMode::Beep,
                intensity: None,
                duration_ms: 100,
            },
        )
        .is_none());

        domain.device_settings.record_event_log = true;
        for sequence in 1..=105 {
            EventLogService::append(
                &mut domain,
                || sequence,
                sequence,
                None,
                EventSource::LocalUi,
                EventLogEntryKind::Action {
                    collar_name: "alpha".to_string(),
                    mode: CommandMode::Beep,
                    intensity: None,
                    duration_ms: 100,
                },
            )
            .unwrap();
        }

        assert_eq!(domain.event_log_events.len(), 100);
        assert_eq!(domain.event_log_events[0].sequence, 6);
    }

    #[test]
    fn execution_and_rf_debug_services_update_domain_state() {
        let mut domain = domain_state();
        ExecutionService::start_preset(&mut domain, "manual".to_string());
        assert_eq!(domain.preset_name.as_deref(), Some("manual"));
        assert!(ExecutionService::complete_preset(&mut domain, "manual"));
        assert!(domain.preset_name.is_none());

        ExecutionService::stop_all(&mut domain, 1_000);
        assert_eq!(domain.rf_lockout_until_ms, 11_000);

        let frame = RfDebugFrame {
            received_at_ms: 1,
            raw_hex: "010203".to_string(),
            collar_id: 42,
            channel: 1,
            mode_raw: 3,
            mode: Some(CommandMode::Beep),
            intensity: 0,
            checksum_ok: true,
        };
        RfDebugService::push_event(&mut domain, frame.clone());
        assert_eq!(domain.rf_debug_events.len(), 1);
        RfDebugService::clear_events(&mut domain);
        assert!(domain.rf_debug_events.is_empty());

        assert!(RemoteControlService::set_status(
            &mut domain,
            RemoteControlStatus {
                enabled: true,
                connected: false,
                url: "ws://example.com".to_string(),
                validate_cert: true,
                rtt_ms: None,
                status_text: "Connecting...".to_string(),
            },
        ));
        let unchanged_status = domain.remote_control_status.clone();
        assert!(!RemoteControlService::set_status(
            &mut domain,
            unchanged_status,
        ));
    }

    #[test]
    fn preset_and_data_services_preserve_expected_side_effects() {
        let mut domain = domain_state();
        let preset = sample_preset("second", "alpha");
        let change = PresetService::save(&mut domain, None, preset).unwrap();
        assert!(change.preset_stopped);
        assert_eq!(change.presets.len(), 2);

        let import = DataService::import(
            &mut domain,
            rusty_collars_core::protocol::ExportData {
                collars: vec![sample_collar("gamma")],
                presets: vec![sample_preset("imported", "gamma")],
            },
        );
        assert!(import.preset_stopped || import.cancel_manual_actions);
        assert_eq!(import.collars[0].name, "gamma");
        assert_eq!(import.presets[0].name, "imported");
    }

    // --- CollarService edge cases ---

    #[test]
    fn collar_service_add_duplicate_rejected() {
        let mut domain = domain_state();
        let err = CollarService::add(&mut domain, sample_collar("alpha")).unwrap_err();
        assert!(matches!(err, ControlError::DuplicateCollar(name) if name == "alpha"));
    }

    #[test]
    fn collar_service_add_validation_failure() {
        let mut domain = domain_state();
        let bad_collar = Collar {
            name: "".to_string(),
            collar_id: 1,
            channel: 0,
        };
        let err = CollarService::add(&mut domain, bad_collar).unwrap_err();
        assert!(matches!(err, ControlError::Validation(_)));
    }

    #[test]
    fn collar_service_add_success() {
        let mut domain = domain_state();
        let change = CollarService::add(&mut domain, sample_collar("beta")).unwrap();
        assert_eq!(change.collars.len(), 2);
        assert!(!change.preset_stopped);
        assert!(!change.cancel_manual_actions);
        assert!(change.presets.is_none());
    }

    #[test]
    fn collar_service_delete_unknown_rejected() {
        let mut domain = domain_state();
        let err = CollarService::delete(&mut domain, "nonexistent".to_string()).unwrap_err();
        assert!(matches!(err, ControlError::UnknownCollar(name) if name == "nonexistent"));
    }

    #[test]
    fn collar_service_delete_referenced_by_preset_rejected() {
        let mut domain = domain_state();
        let err = CollarService::delete(&mut domain, "alpha".to_string()).unwrap_err();
        assert!(matches!(err, ControlError::CollarReferencedByPreset(name) if name == "alpha"));
    }

    #[test]
    fn collar_service_delete_unreferenced_succeeds() {
        let mut domain = domain_state();
        domain.presets.clear();
        domain.preset_name = None;
        let change = CollarService::delete(&mut domain, "alpha".to_string()).unwrap();
        assert!(change.collars.is_empty());
    }

    #[test]
    fn collar_service_update_unknown_rejected() {
        let mut domain = domain_state();
        let err =
            CollarService::update(&mut domain, "nonexistent".to_string(), sample_collar("beta"))
                .unwrap_err();
        assert!(matches!(err, ControlError::UnknownCollar(name) if name == "nonexistent"));
    }

    #[test]
    fn collar_service_update_duplicate_name_rejected() {
        let mut domain = domain_state();
        domain.collars.push(Collar {
            name: "beta".to_string(),
            collar_id: 99,
            channel: 0,
        });
        let err =
            CollarService::update(&mut domain, "alpha".to_string(), sample_collar("beta"))
                .unwrap_err();
        assert!(matches!(err, ControlError::DuplicateCollar(name) if name == "beta"));
    }

    #[test]
    fn collar_service_update_same_name_no_preset_change() {
        let mut domain = domain_state();
        domain.preset_name = None;
        let updated = Collar {
            name: "alpha".to_string(),
            collar_id: 999,
            channel: 0,
        };
        let change = CollarService::update(&mut domain, "alpha".to_string(), updated).unwrap();
        assert!(change.presets.is_none());
        assert!(!change.preset_stopped);
        assert_eq!(change.collars[0].collar_id, 999);
    }

    // --- PresetService edge cases ---

    #[test]
    fn preset_service_save_update_by_original_name() {
        let mut domain = domain_state();
        domain.preset_name = None;
        let updated = sample_preset("renamed", "alpha");
        let change =
            PresetService::save(&mut domain, Some("preset".to_string()), updated).unwrap();
        assert_eq!(change.presets.len(), 1);
        assert_eq!(change.presets[0].name, "renamed");
    }

    #[test]
    fn preset_service_save_unknown_original_rejected() {
        let mut domain = domain_state();
        let err = PresetService::save(
            &mut domain,
            Some("nonexistent".to_string()),
            sample_preset("new", "alpha"),
        )
        .unwrap_err();
        assert!(matches!(err, ControlError::UnknownPreset(name) if name == "nonexistent"));
    }

    #[test]
    fn preset_service_save_duplicate_name_on_rename_rejected() {
        let mut domain = domain_state();
        domain.presets.push(sample_preset("second", "alpha"));
        let err = PresetService::save(
            &mut domain,
            Some("preset".to_string()),
            sample_preset("second", "alpha"),
        )
        .unwrap_err();
        assert!(matches!(err, ControlError::DuplicatePreset(name) if name == "second"));
    }

    #[test]
    fn preset_service_save_upsert_existing_by_name() {
        let mut domain = domain_state();
        domain.preset_name = None;
        let updated = sample_preset("preset", "alpha");
        let change = PresetService::save(&mut domain, None, updated).unwrap();
        assert_eq!(change.presets.len(), 1);
        assert_eq!(change.presets[0].name, "preset");
    }

    #[test]
    fn preset_service_save_new_appended() {
        let mut domain = domain_state();
        domain.preset_name = None;
        let new = sample_preset("new-preset", "alpha");
        let change = PresetService::save(&mut domain, None, new).unwrap();
        assert_eq!(change.presets.len(), 2);
        assert_eq!(change.presets[1].name, "new-preset");
    }

    #[test]
    fn preset_service_delete_unknown_rejected() {
        let mut domain = domain_state();
        let err = PresetService::delete(&mut domain, "nonexistent".to_string()).unwrap_err();
        assert!(matches!(err, ControlError::UnknownPreset(name) if name == "nonexistent"));
    }

    #[test]
    fn preset_service_delete_success() {
        let mut domain = domain_state();
        let change = PresetService::delete(&mut domain, "preset".to_string()).unwrap();
        assert!(change.presets.is_empty());
        assert!(change.preset_stopped);
    }

    #[test]
    fn preset_service_reorder_preserves_all() {
        let mut domain = domain_state();
        domain.preset_name = None;
        domain.presets.push(sample_preset("b", "alpha"));
        domain.presets.push(sample_preset("c", "alpha"));
        let change =
            PresetService::reorder(&mut domain, vec!["c".to_string(), "preset".to_string()]);
        assert_eq!(change.presets[0].name, "c");
        assert_eq!(change.presets[1].name, "preset");
        assert_eq!(change.presets[2].name, "b");
    }

    #[test]
    fn preset_service_reorder_with_unknown_names() {
        let mut domain = domain_state();
        domain.preset_name = None;
        let change = PresetService::reorder(
            &mut domain,
            vec!["unknown".to_string(), "preset".to_string()],
        );
        assert_eq!(change.presets.len(), 1);
        assert_eq!(change.presets[0].name, "preset");
    }

    // --- SettingsService: reboot detection ---

    #[test]
    fn settings_service_hardware_change_requires_reboot() {
        let mut domain = domain_state();
        let mut settings = domain.device_settings.clone();
        settings.rf_tx_pin = 99;
        let change = SettingsService::apply(&mut domain, settings);
        assert!(change.reboot_required);
    }

    #[test]
    fn settings_service_wifi_change_requires_reboot() {
        let mut domain = domain_state();
        let mut settings = domain.device_settings.clone();
        settings.wifi_ssid = "new-ssid".to_string();
        let change = SettingsService::apply(&mut domain, settings);
        assert!(change.reboot_required);
    }

    #[test]
    fn settings_service_ntp_change_requires_reboot() {
        let mut domain = domain_state();
        let mut settings = domain.device_settings.clone();
        settings.ntp_server = "time.google.com".to_string();
        let change = SettingsService::apply(&mut domain, settings);
        assert!(change.reboot_required);
    }

    #[test]
    fn settings_service_non_hardware_change_no_reboot() {
        let mut domain = domain_state();
        let mut settings = domain.device_settings.clone();
        settings.remote_control_enabled = !settings.remote_control_enabled;
        settings.record_event_log = !settings.record_event_log;
        settings.remote_control_url = "wss://new.example.com/ws".to_string();
        let change = SettingsService::apply(&mut domain, settings);
        assert!(!change.reboot_required);
    }

    #[test]
    fn settings_service_identical_settings_no_reboot() {
        let mut domain = domain_state();
        let settings = domain.device_settings.clone();
        let change = SettingsService::apply(&mut domain, settings);
        assert!(!change.reboot_required);
        assert!(!change.remote_settings_changed);
        assert!(!change.event_log_changed);
    }

    // --- remote_control_status_from_settings ---

    #[test]
    fn remote_control_status_disabled() {
        let settings = DeviceSettings {
            remote_control_enabled: false,
            ..DeviceSettings::default()
        };
        let status = super::remote_control_status_from_settings(&settings);
        assert_eq!(status.status_text, "Off");
        assert!(!status.connected);
    }

    #[test]
    fn remote_control_status_empty_url() {
        let settings = DeviceSettings {
            remote_control_enabled: true,
            remote_control_url: "".to_string(),
            ..DeviceSettings::default()
        };
        let status = super::remote_control_status_from_settings(&settings);
        assert_eq!(status.status_text, "Missing URL");
    }

    #[test]
    fn remote_control_status_invalid_url() {
        let settings = DeviceSettings {
            remote_control_enabled: true,
            remote_control_url: "not-a-url".to_string(),
            ..DeviceSettings::default()
        };
        let status = super::remote_control_status_from_settings(&settings);
        assert_eq!(status.status_text, "Invalid URL");
    }

    #[test]
    fn remote_control_status_valid_url() {
        let settings = DeviceSettings {
            remote_control_enabled: true,
            remote_control_url: "wss://example.com/ws".to_string(),
            ..DeviceSettings::default()
        };
        let status = super::remote_control_status_from_settings(&settings);
        assert_eq!(status.status_text, "Connecting...");
    }

    #[test]
    fn remote_control_status_http_url_invalid() {
        let settings = DeviceSettings {
            remote_control_enabled: true,
            remote_control_url: "https://example.com/ws".to_string(),
            ..DeviceSettings::default()
        };
        let status = super::remote_control_status_from_settings(&settings);
        assert_eq!(status.status_text, "Invalid URL");
    }

    // --- ExecutionService edge cases ---

    #[test]
    fn execution_service_complete_wrong_preset_ignored() {
        let mut domain = domain_state();
        assert!(!ExecutionService::complete_preset(&mut domain, "wrong"));
        assert!(domain.preset_name.is_some());
    }

    #[test]
    fn execution_service_stop_preset_when_none_running() {
        let mut domain = domain_state();
        domain.preset_name = None;
        assert!(!ExecutionService::stop_preset(&mut domain));
    }

    // --- RfDebugService cap ---

    #[test]
    fn rf_debug_service_caps_at_max() {
        let mut domain = domain_state();
        for i in 0..150 {
            RfDebugService::push_event(
                &mut domain,
                RfDebugFrame {
                    received_at_ms: i,
                    raw_hex: format!("{i:04X}"),
                    collar_id: 1,
                    channel: 0,
                    mode_raw: 1,
                    mode: Some(CommandMode::Shock),
                    intensity: 10,
                    checksum_ok: true,
                },
            );
        }
        assert_eq!(domain.rf_debug_events.len(), super::MAX_RF_DEBUG_EVENTS);
        assert_eq!(domain.rf_debug_events.front().unwrap().received_at_ms, 50);
    }
}
