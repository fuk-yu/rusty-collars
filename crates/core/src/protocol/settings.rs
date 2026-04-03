use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSettings {
    #[serde(default)]
    pub device_id: String,
    #[serde(alias = "led_pin")]
    pub tx_led_pin: u8,
    #[serde(default = "default_rx_led_pin")]
    pub rx_led_pin: u8,
    pub rf_tx_pin: u8,
    pub rf_rx_pin: u8,
    #[serde(default = "default_true")]
    pub wifi_client_enabled: bool,
    #[serde(default)]
    pub wifi_ssid: String,
    #[serde(default)]
    pub wifi_password: String,
    #[serde(default = "default_true")]
    pub ap_enabled: bool,
    #[serde(default = "default_ap_password")]
    pub ap_password: String,
    #[serde(default = "default_max_clients")]
    pub max_clients: u8,
    #[serde(default = "default_true")]
    pub ntp_enabled: bool,
    #[serde(default = "default_ntp_server")]
    pub ntp_server: String,
    #[serde(default)]
    pub remote_control_enabled: bool,
    #[serde(default)]
    pub remote_control_url: String,
    #[serde(default = "default_true")]
    pub remote_control_validate_cert: bool,
    #[serde(default)]
    pub record_event_log: bool,
}

fn default_true() -> bool {
    true
}

fn default_ap_password() -> String {
    "rfcollars".to_string()
}

fn default_max_clients() -> u8 {
    8
}

fn default_ntp_server() -> String {
    "pool.ntp.org".to_string()
}

fn default_rx_led_pin() -> u8 {
    DeviceSettings::default_pins().1
}

impl DeviceSettings {
    pub fn default_pins() -> (u8, u8, u8, u8) {
        #[cfg(esp32c6)]
        {
            (8, 8, 10, 11)
        }
        #[cfg(esp32p4)]
        {
            (33, 8, 23, 5)
        }
        #[cfg(not(any(esp32c6, esp32p4)))]
        {
            (2, 2, 16, 15)
        }
    }
}

impl Default for DeviceSettings {
    fn default() -> Self {
        let (tx_led_pin, rx_led_pin, rf_tx_pin, rf_rx_pin) = Self::default_pins();
        Self {
            device_id: String::new(),
            tx_led_pin,
            rx_led_pin,
            rf_tx_pin,
            rf_rx_pin,
            wifi_client_enabled: true,
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            ap_enabled: true,
            ap_password: "rfcollars".to_string(),
            max_clients: 8,
            ntp_enabled: true,
            ntp_server: default_ntp_server(),
            remote_control_enabled: false,
            remote_control_url: String::new(),
            remote_control_validate_cert: true,
            record_event_log: false,
        }
    }
}

pub const AP_SSID: &str = "rfcollars";
