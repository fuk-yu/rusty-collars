use std::time::{Duration, Instant};

use anyhow::Result;
use embedded_svc::wifi::Wifi;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use log::{info, warn};

use crate::protocol::{DeviceSettings, AP_SSID};

/// Compile-time WiFi credentials from wifi.toml (fallback if NVS settings are empty).
const WIFI_SSID_DEFAULT: &str = env!("WIFI_SSID");
const WIFI_PASSWORD_DEFAULT: &str = env!("WIFI_PASSWORD");
const WIFI_RECONNECT_BASE_DELAY: Duration = Duration::from_millis(500);
const WIFI_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5);
const WIFI_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct WifiController {
    wifi: BlockingWifi<EspWifi<'static>>,
    sta_ssid: String,
    reconnect_delay: Duration,
    reconnect_at: Option<Instant>,
    connect_deadline: Option<Instant>,
    connecting: bool,
}

impl WifiController {
    /// Non-blocking poll. Safe to call from the main loop with watchdog.
    pub fn poll(&mut self) {
        let now = Instant::now();

        // If a background reconnect is in progress, check if it finished
        if self.connecting {
            match self.wifi.is_connected() {
                Ok(true) => {
                    if let Ok(ip_info) = self.wifi.wifi().sta_netif().get_ip_info() {
                        info!("WiFi STA connected! IP: {}", ip_info.ip);
                    }
                    self.connecting = false;
                    self.connect_deadline = None;
                    self.reconnect_delay = WIFI_RECONNECT_BASE_DELAY;
                    self.reconnect_at = None;
                    return;
                }
                Ok(false) => {}
                Err(err) => {
                    warn!("WiFi reconnect status check failed: {err:#}");
                }
            }

            let connect_deadline = self
                .connect_deadline
                .expect("missing connect deadline while reconnecting");
            if now < connect_deadline {
                return;
            }

            warn!(
                "WiFi reconnect to '{}' timed out after {}ms",
                self.sta_ssid,
                WIFI_CONNECT_ATTEMPT_TIMEOUT.as_millis()
            );
            if let Err(err) = self.wifi.wifi_mut().disconnect() {
                warn!("WiFi disconnect after timeout failed: {err:#}");
            }
            self.connecting = false;
            self.connect_deadline = None;
            let next_delay = next_reconnect_delay(self.reconnect_delay);
            self.reconnect_delay = next_delay;
            self.reconnect_at = Some(now + next_delay);
            return;
        }

        let connected = match self.wifi.is_connected() {
            Ok(c) => c,
            Err(err) => {
                if self.reconnect_at.is_none() {
                    warn!("WiFi status check failed: {err:#}");
                }
                false
            }
        };

        if connected {
            self.reconnect_delay = WIFI_RECONNECT_BASE_DELAY;
            self.reconnect_at = None;
            return;
        }

        if self.reconnect_at.is_none() {
            info!(
                "WiFi STA disconnected. Scheduling reconnect in {}ms",
                self.reconnect_delay.as_millis()
            );
        }

        let Some(reconnect_at) = self.reconnect_at else {
            self.reconnect_at = Some(now + self.reconnect_delay);
            return;
        };

        if now < reconnect_at {
            return;
        }

        info!(
            "Attempting WiFi reconnect to '{}' (backoff={}ms)...",
            self.sta_ssid,
            self.reconnect_delay.as_millis()
        );

        // Non-blocking connect: just initiate, don't wait for completion.
        // The next poll() will check is_connected().
        match self.wifi.wifi_mut().connect() {
            Ok(()) => {
                self.connecting = true;
                self.connect_deadline = Some(now + WIFI_CONNECT_ATTEMPT_TIMEOUT);
                self.reconnect_at = None;
            }
            Err(err) => {
                let next_delay = next_reconnect_delay(self.reconnect_delay);
                warn!(
                    "WiFi reconnect initiation failed: {err:#}. Retrying in {}ms",
                    next_delay.as_millis()
                );
                self.reconnect_delay = next_delay;
                self.reconnect_at = Some(Instant::now() + next_delay);
            }
        }
    }
}

/// Connect WiFi using device settings. Falls back to compile-time wifi.toml if
/// settings have empty SSID. Supports Mixed mode (STA + AP) when AP is enabled.
pub fn connect(
    modem: Modem<'static>,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    settings: &DeviceSettings,
) -> Result<WifiController> {
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sys_loop.clone(), Some(nvs))?, sys_loop)?;

    // Resolve STA credentials: NVS settings override compile-time defaults
    let sta_ssid = if settings.wifi_ssid.is_empty() {
        WIFI_SSID_DEFAULT.to_string()
    } else {
        settings.wifi_ssid.clone()
    };
    let sta_password = if settings.wifi_ssid.is_empty() {
        WIFI_PASSWORD_DEFAULT.to_string()
    } else {
        settings.wifi_password.clone()
    };

    let sta_config = ClientConfiguration {
        ssid: sta_ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("SSID too long (max 32 bytes)"))?,
        password: sta_password
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Password too long (max 64 bytes)"))?,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    };

    let config = if settings.ap_enabled {
        let ap_config = AccessPointConfiguration {
            ssid: AP_SSID
                .try_into()
                .map_err(|_| anyhow::anyhow!("AP SSID too long"))?,
            password: settings
                .ap_password
                .as_str()
                .try_into()
                .map_err(|_| anyhow::anyhow!("AP password too long"))?,
            auth_method: if settings.ap_password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            max_connections: 4,
            ..Default::default()
        };
        info!("WiFi Mixed mode: STA='{}' + AP='{}'", sta_ssid, AP_SSID);
        Configuration::Mixed(sta_config, ap_config)
    } else {
        info!("WiFi STA mode: '{}'", sta_ssid);
        Configuration::Client(sta_config)
    };

    wifi.set_configuration(&config)?;

    // Start WiFi (brings up AP immediately in Mixed mode)
    wifi.start()?;

    if settings.ap_enabled {
        if let Ok(ap_info) = wifi.wifi().ap_netif().get_ip_info() {
            info!("AP '{}' running at IP: {}", AP_SSID, ap_info.ip);
        }
    }

    // Try STA connection, but don't fail if it times out -
    // the AP should still work and the poll loop will retry STA.
    info!("Connecting STA to '{}'...", sta_ssid);
    match connect_station(&mut wifi) {
        Ok(()) => {}
        Err(e) => {
            warn!("Initial STA connection failed: {e:#}");
            warn!("AP is still running. STA will retry in background.");
        }
    }

    Ok(WifiController {
        wifi,
        sta_ssid,
        reconnect_delay: WIFI_RECONNECT_BASE_DELAY,
        reconnect_at: None,
        connect_deadline: None,
        connecting: false,
    })
}

fn connect_station(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<()> {
    wifi.connect()?;
    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("WiFi STA connected! IP: {}", ip_info.ip);

    Ok(())
}

fn next_reconnect_delay(current: Duration) -> Duration {
    let doubled = current.checked_mul(2).unwrap_or(WIFI_RECONNECT_MAX_DELAY);
    if doubled > WIFI_RECONNECT_MAX_DELAY {
        WIFI_RECONNECT_MAX_DELAY
    } else {
        doubled
    }
}
