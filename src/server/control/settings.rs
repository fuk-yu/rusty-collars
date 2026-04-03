use log::info;

use crate::error::ControlError;
use crate::protocol::{DeviceSettings, ServerMessage};
use crate::server::{ControlResult, HAS_WIFI};

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
    dispatcher.ctx.save_device_settings(settings)
}
