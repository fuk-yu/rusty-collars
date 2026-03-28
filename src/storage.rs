use anyhow::Result;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::info;

use crate::protocol::{Collar, DeviceSettings, Preset};

const NAMESPACE: &str = "collars";
const KEY_COLLARS: &str = "col";
const KEY_PRESETS: &str = "pre";
const KEY_GPIO: &str = "gpio";

pub struct Storage {
    nvs: EspNvs<NvsDefault>,
}

impl Storage {
    pub fn new(partition: EspDefaultNvsPartition) -> Result<Self> {
        let nvs = EspNvs::new(partition, NAMESPACE, true)?;
        info!("NVS storage initialized");
        Ok(Self { nvs })
    }

    pub fn load_collars(&self) -> Result<Vec<Collar>> {
        self.load_json(KEY_COLLARS)
    }

    pub fn save_collars(&self, collars: &[Collar]) -> Result<()> {
        self.save_json(KEY_COLLARS, collars)
    }

    pub fn load_presets(&self) -> Result<Vec<Preset>> {
        self.load_json(KEY_PRESETS)
    }

    pub fn save_presets(&self, presets: &[Preset]) -> Result<()> {
        self.save_json(KEY_PRESETS, presets)
    }

    pub fn load_settings(&self) -> Result<DeviceSettings> {
        match self.nvs.str_len(KEY_GPIO)? {
            Some(len) if len > 0 => {
                let mut buf = vec![0u8; len];
                match self.nvs.get_str(KEY_GPIO, &mut buf)? {
                    Some(s) => Ok(serde_json::from_str(s)?),
                    None => Ok(DeviceSettings::default()),
                }
            }
            _ => Ok(DeviceSettings::default()),
        }
    }

    pub fn save_settings(&self, settings: &DeviceSettings) -> Result<()> {
        let json = serde_json::to_string(settings)?;
        self.nvs.set_str(KEY_GPIO, &json)?;
        Ok(())
    }

    fn load_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Vec<T>> {
        match self.nvs.str_len(key)? {
            Some(len) if len > 0 => {
                let mut buf = vec![0u8; len];
                match self.nvs.get_str(key, &mut buf)? {
                    Some(s) => Ok(serde_json::from_str(s)?),
                    None => Ok(Vec::new()),
                }
            }
            _ => Ok(Vec::new()),
        }
    }

    fn save_json<T: serde::Serialize>(&self, key: &str, data: &[T]) -> Result<()> {
        let json = serde_json::to_string(data)?;
        self.nvs.set_str(key, &json)?;
        Ok(())
    }
}
