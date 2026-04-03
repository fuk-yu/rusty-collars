use crate::error::ControlError;
use crate::protocol::{ExportData, Preset, ServerMessage};
use crate::server::ControlResult;
use crate::validation;

use super::{ensure_local_ui, json_message, ControlDispatcher};

pub(super) fn export(dispatcher: &ControlDispatcher<'_>) -> ControlResult {
    ensure_local_ui(dispatcher.origin, "export")?;

    let domain = dispatcher.ctx.domain.lock().unwrap();
    let mut data = ExportData {
        collars: domain.collars.clone(),
        presets: domain.presets.clone(),
    };
    drop(domain);
    data.presets.iter_mut().for_each(Preset::normalize);
    json_message(&ServerMessage::ExportData { data: &data })
}

pub(super) fn import(dispatcher: &ControlDispatcher<'_>, mut data: ExportData) -> ControlResult {
    ensure_local_ui(dispatcher.origin, "import")?;

    data.presets.iter_mut().for_each(Preset::normalize);
    validation::validate_export_data(&data)
        .map_err(|err| ControlError::Validation(err.to_string()))?;
    dispatcher.ctx.import_data(data)
}
