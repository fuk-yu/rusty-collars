use std::time::{Duration, Instant};

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use log::{info, warn};

use crate::protocol::{DeviceSettings, AP_SSID};

const WIFI_RECONNECT_BASE_DELAY: Duration = Duration::from_millis(500);
const WIFI_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(5);
const WIFI_CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);

/// Returns true if the STA client will actually attempt connections,
/// considering both the enable flag and whether an SSID is available.
pub fn sta_will_connect(settings: &DeviceSettings) -> bool {
    settings.wifi_client_enabled && !settings.wifi_ssid.is_empty()
}

fn make_ap_config(settings: &DeviceSettings) -> Result<AccessPointConfiguration> {
    Ok(AccessPointConfiguration {
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
    })
}

pub struct WifiController {
    wifi: BlockingWifi<EspWifi<'static>>,
    sta_ssid: String,
    sta_enabled: bool,
    reconnect_delay: Duration,
    reconnect_at: Option<Instant>,
    connect_deadline: Option<Instant>,
    connecting: bool,
}

impl WifiController {
    /// Non-blocking poll. Safe to call from the main loop with watchdog.
    pub fn poll(&mut self) {
        if !self.sta_enabled {
            return;
        }

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

    /// Returns true if the STA interface has an IP address.
    pub fn sta_has_ip(&self) -> bool {
        if !self.sta_enabled {
            return false;
        }
        self.wifi
            .wifi()
            .sta_netif()
            .get_ip_info()
            .map(|info| !info.ip.is_unspecified())
            .unwrap_or(false)
    }

    /// Force-enable AP mode. Used when initial connectivity fails and the board
    /// needs to be reachable for configuration.
    /// Rebuilds the configuration from scratch using stored state (avoids
    /// `get_configuration()` which requires RPC calls unsupported on P4 EPPP).
    pub fn force_enable_ap(&mut self, settings: &DeviceSettings) -> Result<()> {
        warn!("Force-enabling AP '{}' for emergency access", AP_SSID);
        let ap_config = make_ap_config(settings)?;

        self.wifi.stop()?;

        let new_config = if self.sta_enabled {
            let sta_config = ClientConfiguration {
                ssid: self
                    .sta_ssid
                    .as_str()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("SSID too long"))?,
                password: settings
                    .wifi_password
                    .as_str()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Password too long"))?,
                auth_method: AuthMethod::WPA2Personal,
                ..Default::default()
            };
            Configuration::Mixed(sta_config, ap_config)
        } else {
            Configuration::AccessPoint(ap_config)
        };

        self.wifi.set_configuration(&new_config)?;
        self.wifi.start()?;

        if self.sta_enabled {
            let _ = self.wifi.wifi_mut().connect();
            self.connecting = true;
            self.connect_deadline = Some(Instant::now() + WIFI_CONNECT_ATTEMPT_TIMEOUT);
        }

        if let Ok(ap_info) = self.wifi.wifi().ap_netif().get_ip_info() {
            info!("Forced AP '{}' running at IP: {}", AP_SSID, ap_info.ip);
        }

        Ok(())
    }
}

/// Connect WiFi using device settings.
/// Supports Mixed mode (STA + AP), AP-only, or STA-only.
///
/// `force_ap`: force AP on regardless of settings (for wifi-only boards with no STA).
pub fn connect(
    modem: Modem<'static>,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    settings: &DeviceSettings,
    force_ap: bool,
) -> Result<WifiController> {
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sys_loop.clone(), Some(nvs))?, sys_loop)?;

    let sta_enabled = sta_will_connect(settings);
    let effective_ap_enabled = settings.ap_enabled || force_ap;

    assert!(
        sta_enabled || effective_ap_enabled,
        "Neither WiFi client nor AP is configured - cannot start WiFi"
    );

    let sta_ssid = if sta_enabled {
        settings.wifi_ssid.clone()
    } else {
        String::new()
    };
    let sta_password = if sta_enabled {
        settings.wifi_password.clone()
    } else {
        String::new()
    };

    let sta_config = if sta_enabled {
        Some(ClientConfiguration {
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
        })
    } else {
        None
    };

    let ap_config = if effective_ap_enabled {
        Some(make_ap_config(settings)?)
    } else {
        None
    };

    let config = match (sta_config, ap_config) {
        (Some(sta), Some(ap)) => {
            info!("WiFi Mixed mode: STA='{}' + AP='{}'", sta_ssid, AP_SSID);
            Configuration::Mixed(sta, ap)
        }
        (Some(sta), None) => {
            info!("WiFi STA mode: '{}'", sta_ssid);
            Configuration::Client(sta)
        }
        (None, Some(ap)) => {
            info!("WiFi AP-only mode: '{}'", AP_SSID);
            Configuration::AccessPoint(ap)
        }
        (None, None) => unreachable!(),
    };

    wifi.set_configuration(&config)?;

    // Start WiFi (brings up AP immediately in Mixed/AP mode)
    wifi.start()?;

    if effective_ap_enabled {
        if let Ok(ap_info) = wifi.wifi().ap_netif().get_ip_info() {
            info!("AP '{}' running at IP: {}", AP_SSID, ap_info.ip);
        }
    }

    // Try STA connection, but don't fail if it times out -
    // the AP should still work and the poll loop will retry STA.
    if sta_enabled {
        info!("Connecting STA to '{}'...", sta_ssid);
        match connect_station(&mut wifi) {
            Ok(()) => {}
            Err(e) => {
                warn!("Initial STA connection failed: {e:#}");
                if effective_ap_enabled {
                    warn!("AP is still running. STA will retry in background.");
                }
            }
        }
    }

    Ok(WifiController {
        wifi,
        sta_ssid,
        sta_enabled,
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
