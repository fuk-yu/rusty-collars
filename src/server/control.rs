mod actions;
mod collars;
mod data;
mod presets;
mod settings;

use crate::error::ControlError;
use crate::protocol::{ClientMessage, ServerMessage};

use super::{ActionOwner, AppCtx, ControlResult, MessageOrigin};

pub(crate) struct ControlDispatcher<'a> {
    ctx: &'a AppCtx,
    origin: MessageOrigin,
    owner: Option<ActionOwner>,
}

impl<'a> ControlDispatcher<'a> {
    pub(crate) fn new(ctx: &'a AppCtx, origin: MessageOrigin, owner: Option<ActionOwner>) -> Self {
        Self { ctx, origin, owner }
    }

    pub(crate) fn handle(&self, msg: ClientMessage) -> ControlResult {
        match msg {
            ClientMessage::Command {
                collar_name,
                mode,
                intensity,
            } => actions::send_command(self.ctx, collar_name, mode, intensity),

            ClientMessage::ButtonEvent {
                collar_name,
                mode,
                intensity,
                action,
            } => actions::record_button_event(collar_name, mode, intensity, action),

            ClientMessage::RunAction {
                collar_name,
                mode,
                intensity,
                duration_ms,
                intensity_max,
                duration_max_ms,
                intensity_distribution,
                duration_distribution,
            } => actions::run_action(
                self,
                collar_name,
                mode,
                intensity,
                duration_ms,
                intensity_max,
                duration_max_ms,
                intensity_distribution,
                duration_distribution,
            ),

            ClientMessage::StartAction {
                collar_name,
                mode,
                intensity,
                intensity_max,
                intensity_distribution,
            } => actions::start_action(
                self,
                collar_name,
                mode,
                intensity,
                intensity_max,
                intensity_distribution,
            ),

            ClientMessage::StopAction { collar_name, mode } => {
                actions::stop_action(self.ctx, collar_name, mode)
            }

            ClientMessage::AddCollar {
                name,
                collar_id,
                channel,
            } => collars::add(self.ctx, name, collar_id, channel),

            ClientMessage::UpdateCollar {
                original_name,
                name,
                collar_id,
                channel,
            } => collars::update(self.ctx, original_name, name, collar_id, channel),

            ClientMessage::DeleteCollar { name } => collars::delete(self.ctx, name),

            ClientMessage::SavePreset {
                original_name,
                preset,
            } => presets::save(self.ctx, original_name, preset),

            ClientMessage::Ping { nonce } => Ok(vec![pong_json(self.ctx, nonce)]),

            ClientMessage::DeletePreset { name } => presets::delete(self.ctx, name),
            ClientMessage::RunPreset { name } => actions::run_preset(self, name),
            ClientMessage::StopPreset => actions::stop_preset(self.ctx),
            ClientMessage::StopAll => actions::stop_all(self.ctx),

            ClientMessage::StartRfDebug
            | ClientMessage::StopRfDebug
            | ClientMessage::ClearRfDebug => Err(ControlError::LocalUiOnly {
                operation: "RF debug control",
            }),
            ClientMessage::Reboot => Err(ControlError::LocalUiOnly {
                operation: "Device reboot",
            }),

            ClientMessage::GetDeviceSettings => settings::get_device_settings(self),
            ClientMessage::GetNetworkStatus => settings::get_network_status(self),
            ClientMessage::SaveDeviceSettings { settings } => {
                settings::save_device_settings(self, settings)
            }

            ClientMessage::PreviewPreset { nonce, preset } => {
                presets::preview(self.ctx, nonce, preset)
            }
            ClientMessage::ReorderPresets { names } => presets::reorder(self.ctx, names),
            ClientMessage::Export => data::export(self),
            ClientMessage::Import { data } => data::import(self, data),
        }
    }
}

pub(crate) fn local_ui_dispatcher<'a>(
    ctx: &'a AppCtx,
    owner: ActionOwner,
) -> ControlDispatcher<'a> {
    ControlDispatcher::new(ctx, MessageOrigin::LocalUi, Some(owner))
}

pub(crate) fn remote_control_dispatcher(ctx: &AppCtx) -> ControlDispatcher<'_> {
    ControlDispatcher::new(
        ctx,
        MessageOrigin::RemoteControl,
        Some(ActionOwner::RemoteControl),
    )
}

pub(crate) fn mqtt_dispatcher(ctx: &AppCtx) -> ControlDispatcher<'_> {
    ControlDispatcher::new(ctx, MessageOrigin::Mqtt, Some(ActionOwner::Mqtt))
}

pub(crate) fn pong_json(ctx: &AppCtx, nonce: u32) -> String {
    let client_ips = ctx.client_ips();

    serde_json::to_string(&ServerMessage::Pong {
        nonce,
        server_uptime_s: super::uptime_seconds(),
        free_heap_bytes: super::free_heap(),
        connected_clients: client_ips.len() as u32,
        client_ips,
    })
    .unwrap()
}

pub(crate) fn cancel_owned_manual_actions(ctx: &AppCtx, owner: ActionOwner) {
    actions::cancel_owned_manual_actions(ctx, owner);
}

fn ensure_local_ui(
    origin: MessageOrigin,
    operation: &'static str,
) -> core::result::Result<(), ControlError> {
    if origin != MessageOrigin::LocalUi {
        Err(ControlError::LocalUiOnly { operation })
    } else {
        Ok(())
    }
}

fn json_message(message: &impl serde::Serialize) -> ControlResult {
    Ok(vec![serde_json::to_string(message)?])
}
