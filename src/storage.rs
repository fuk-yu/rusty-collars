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

    /// Generates and persists a device UUID if one doesn't exist yet.
    pub fn ensure_device_id(&self, settings: &mut DeviceSettings) -> Result<()> {
        if settings.device_id.is_empty() {
            settings.device_id = generate_uuid_v4();
            self.save_settings(settings)?;
            info!("Generated new device ID: {}", settings.device_id);
        }
        Ok(())
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

/// Generate a UUID v4 using the ESP-IDF hardware RNG.
fn generate_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    unsafe {
        esp_idf_svc::sys::esp_fill_random(bytes.as_mut_ptr() as *mut core::ffi::c_void, 16);
    }
    // Set version (4) and variant (1) bits per RFC 4122
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}
