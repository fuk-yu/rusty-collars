use std::sync::atomic::Ordering;

use log::{error, info};

use crate::error::ControlError;
use crate::protocol::{DeviceSettings, ServerMessage};
use crate::server::{device_settings_reboot_required, ControlResult, HAS_WIFI};

use super::{ensure_local_ui, json_message, ControlDispatcher};

pub(super) fn get_device_settings(dispatcher: &ControlDispatcher<'_>) -> ControlResult {
    ensure_local_ui(dispatcher.origin, "get_device_settings")?;
    let settings = dispatcher
        .ctx
        .domain
        .lock()
        .unwrap()
        .device_settings
        .clone();
    json_message(&ServerMessage::DeviceSettings {
        settings,
        reboot_required: false,
        has_wifi: HAS_WIFI,
    })
}

pub(super) fn get_network_status(dispatcher: &ControlDispatcher<'_>) -> ControlResult {
    ensure_local_ui(dispatcher.origin, "get_network_status")?;
    let settings = dispatcher
        .ctx
        .domain
        .lock()
        .unwrap()
        .device_settings
        .clone();
    json_message(&super::super::status::gather_network_status(&settings))
}

pub(super) fn save_device_settings(
    dispatcher: &ControlDispatcher<'_>,
    mut settings: DeviceSettings,
) -> ControlResult {
    ensure_local_ui(dispatcher.origin, "save_device_settings")?;

    if settings.device_id.is_empty() {
        settings.device_id = dispatcher
            .ctx
            .domain
            .lock()
            .unwrap()
            .device_settings
            .device_id
            .clone();
    }
    settings.ntp_server = settings.ntp_server.trim().to_string();
    settings.remote_control_url = settings.remote_control_url.trim().to_string();

    if settings.ntp_enabled && settings.ntp_server.is_empty() {
        return Err(ControlError::EmptyNtpServer);
    }
    if settings.remote_control_enabled {
        super::super::parse_remote_control_url(&settings.remote_control_url)
            .map_err(ControlError::RemoteControlUrl)?;
    }

    info!("Saving device settings...");
    let settings_to_save = settings.clone();
    let (reboot_required, remote_settings_changed, event_log_changed) = {
        let mut domain = dispatcher.ctx.domain.lock().unwrap();
        let previous_settings = domain.device_settings.clone();
        let reboot_required = device_settings_reboot_required(&previous_settings, &settings);
        let remote_settings_changed = previous_settings.remote_control_enabled
            != settings.remote_control_enabled
            || previous_settings.remote_control_url != settings.remote_control_url
            || previous_settings.remote_control_validate_cert
                != settings.remote_control_validate_cert;
        let event_log_changed = previous_settings.record_event_log != settings.record_event_log;

        domain.device_settings = settings;
        if remote_settings_changed {
            domain.remote_control_status =
                super::super::status::remote_control_status_from_settings(&domain.device_settings);
        }
        if !domain.device_settings.record_event_log {
            domain.event_log_events.clear();
        }

        (reboot_required, remote_settings_changed, event_log_changed)
    };

    if remote_settings_changed {
        dispatcher
            .ctx
            .sessions
            .remote_control_settings_revision
            .fetch_add(1, Ordering::SeqCst);
    }

    match dispatcher.ctx.persist_settings(&settings_to_save) {
        Ok(()) => info!("Device settings saved to NVS"),
        Err(err) => error!("NVS save_settings failed: {err:#}"),
    }

    if remote_settings_changed {
        dispatcher.ctx.broadcast_remote_control_status();
    }
    if event_log_changed {
        dispatcher.ctx.broadcast_event_log_state();
    }

    json_message(&ServerMessage::DeviceSettings {
        settings: settings_to_save,
        reboot_required,
        has_wifi: HAS_WIFI,
    })
}
