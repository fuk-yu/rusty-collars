use std::sync::{Arc, Mutex};

use anyhow::Result;

use rusty_collars_core::protocol::{Collar, DeviceSettings, Preset};

pub trait SettingsRepository {
    fn ensure_device_id(&mut self, settings: &mut DeviceSettings) -> Result<()>;
    fn load_settings(&mut self) -> Result<DeviceSettings>;
    fn save_settings(&mut self, settings: &DeviceSettings) -> Result<()>;
}

pub trait CollarRepository {
    fn load_collars(&mut self) -> Result<Vec<Collar>>;
    fn save_collars(&mut self, collars: &[Collar]) -> Result<()>;
}

pub trait PresetRepository {
    fn load_presets(&mut self) -> Result<Vec<Preset>>;
    fn save_presets(&mut self, presets: &[Preset]) -> Result<()>;
}

pub trait AppRepository: SettingsRepository + CollarRepository + PresetRepository + Send {}

impl<T> AppRepository for T where T: SettingsRepository + CollarRepository + PresetRepository + Send {}

pub type SharedRepository = Arc<Mutex<Box<dyn AppRepository>>>;
