//! Network abstraction: WiFi on ESP32/C6, Ethernet on P4, OpenETH on QEMU.

use crate::protocol::DeviceSettings;
use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::{info, warn};

#[cfg(esp32)]
type EthernetKeepalive =
    esp_idf_svc::eth::BlockingEth<esp_idf_svc::eth::EspEth<'static, esp_idf_svc::eth::OpenEth>>;

#[cfg(not(esp32))]
type EthernetKeepalive = ();

/// Returns true if running in QEMU (only used on ESP32 with OpenETH).
#[allow(dead_code)]
pub fn is_qemu() -> bool {
    let mut mac = [0u8; 6];
    let err = unsafe { esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
    if err != esp_idf_svc::sys::ESP_OK {
        return false;
    }
    mac[0] == 0 && mac[1] == 0 && mac[2] == 0
}

#[allow(dead_code)]
pub enum NetworkHandle {
    #[cfg(has_wifi)]
    Wifi(crate::wifi::WifiController),
    Eth(EthernetKeepalive),
    None,
}

impl NetworkHandle {
    pub fn poll(&mut self) {
        #[cfg(has_wifi)]
        if let Self::Wifi(wifi) = self {
            wifi.poll();
        }
    }

    pub fn supports_time_sync(&self) -> bool {
        match self {
            #[cfg(has_wifi)]
            Self::Wifi(_) => true,
            Self::Eth(_) => true,
            Self::None => false,
        }
    }
}

// --- ESP32: WiFi + OpenETH for QEMU ---

#[cfg(esp32)]
pub fn connect(
    modem: esp_idf_svc::hal::modem::Modem<'static>,
    mac: esp_idf_svc::hal::mac::MAC<'static>,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    settings: &DeviceSettings,
) -> Result<NetworkHandle> {
    if is_qemu() {
        info!("QEMU detected - skipping WiFi PHY initialization");
        return connect_qemu_eth(mac, sys_loop);
    }
    // ESP32 is a wifi-capable board; force AP if no other reachability
    let sta_will = crate::wifi::sta_will_connect(settings);
    let force_ap = !sta_will && !settings.ap_enabled;
    if force_ap {
        warn!("WiFi board: forcing AP (WiFi client disabled/unconfigured and AP is off)");
    }
    let wifi = crate::wifi::connect(modem, sys_loop, nvs, settings, force_ap)?;
    Ok(NetworkHandle::Wifi(wifi))
}

// --- ESP32-P4: Ethernet via raw ESP-IDF ---

/// Start P4 Ethernet (raw ESP-IDF). Returns the netif handle.
/// Does NOT wait for DHCP — the caller decides whether to block.
#[cfg(esp32p4)]
unsafe fn start_p4_ethernet() -> *mut esp_idf_svc::sys::esp_netif_t {
    use esp_idf_svc::sys::*;

    // Initialize TCP/IP stack and default event loop (required before esp_netif/eth)
    let err = esp_netif_init();
    assert!(
        err == ESP_OK || err == ESP_ERR_INVALID_STATE,
        "esp_netif_init failed: {err}"
    );
    let err = esp_event_loop_create_default();
    // ESP_ERR_INVALID_STATE means it's already created (e.g. by EspSystemEventLoop::take)
    assert!(
        err == ESP_OK || err == ESP_ERR_INVALID_STATE,
        "esp_event_loop_create_default failed: {err}"
    );

    // Create default Ethernet netif
    let netif_cfg = esp_netif_config_t {
        base: &_g_esp_netif_inherent_eth_config,
        driver: core::ptr::null(),
        stack: _g_esp_netif_netstack_default_eth,
    };
    let netif = esp_netif_new(&netif_cfg);
    assert!(!netif.is_null(), "esp_netif_new failed");

    // MAC: internal EMAC with default P4 pin config (ETH_ESP32_EMAC_DEFAULT_CONFIG)
    let emac_config: eth_esp32_emac_config_t = {
        let mut c: eth_esp32_emac_config_t = core::mem::zeroed();
        c.__bindgen_anon_1.smi_gpio.mdc_num = 31;
        c.__bindgen_anon_1.smi_gpio.mdio_num = 52;
        c.interface = eth_data_interface_t_EMAC_DATA_INTERFACE_RMII;
        c.clock_config.rmii.clock_mode = emac_rmii_clock_mode_t_EMAC_CLK_EXT_IN;
        c.clock_config.rmii.clock_gpio = 50;
        c.dma_burst_len = eth_mac_dma_burst_len_t_ETH_DMA_BURST_LEN_32;
        c.emac_dataif_gpio.rmii.tx_en_num = 49;
        c.emac_dataif_gpio.rmii.txd0_num = 34;
        c.emac_dataif_gpio.rmii.txd1_num = 35;
        c.emac_dataif_gpio.rmii.crs_dv_num = 28;
        c.emac_dataif_gpio.rmii.rxd0_num = 29;
        c.emac_dataif_gpio.rmii.rxd1_num = 30;
        c.clock_config_out_in.rmii.clock_mode = emac_rmii_clock_mode_t_EMAC_CLK_EXT_IN;
        c.clock_config_out_in.rmii.clock_gpio = -1;
        c
    };

    // ETH_MAC_DEFAULT_CONFIG
    let mac_config = eth_mac_config_t {
        sw_reset_timeout_ms: 100,
        rx_task_stack_size: 4096,
        rx_task_prio: 15,
        flags: 0,
    };

    let mac = esp_eth_mac_new_esp32(&emac_config, &mac_config);
    assert!(!mac.is_null(), "esp_eth_mac_new_esp32 failed");

    // PHY: IP101 at address 1, RST on GPIO51 (ESP32-P4-Function-EV defaults)
    let phy_config = eth_phy_config_t {
        phy_addr: 1,
        reset_timeout_ms: 100,
        autonego_timeout_ms: 4000,
        reset_gpio_num: 51,
        ..Default::default()
    };
    let phy = esp_eth_phy_new_ip101(&phy_config);
    assert!(!phy.is_null(), "esp_eth_phy_new_ip101 failed");

    // Install Ethernet driver
    let eth_config = esp_eth_config_t {
        mac,
        phy,
        check_link_period_ms: 2000,
        ..Default::default()
    };
    let mut eth_handle: esp_eth_handle_t = core::ptr::null_mut();
    let err = esp_eth_driver_install(&eth_config, &mut eth_handle);
    assert_eq!(err, ESP_OK, "esp_eth_driver_install failed: {err}");

    // Attach Ethernet to netif
    let glue = esp_eth_new_netif_glue(eth_handle);
    let err = esp_netif_attach(netif, glue as *mut _);
    assert_eq!(err, ESP_OK, "esp_netif_attach failed: {err}");

    // Start Ethernet
    let err = esp_eth_start(eth_handle);
    assert_eq!(err, ESP_OK, "esp_eth_start failed: {err}");

    info!("Ethernet started, waiting for IP via DHCP...");

    netif
}

/// Wait for an IP address on the given netif via DHCP.
/// Returns true if IP was obtained within the timeout.
#[cfg(esp32p4)]
#[allow(dead_code)]
unsafe fn wait_netif_ip(
    netif: *mut esp_idf_svc::sys::esp_netif_t,
    timeout: std::time::Duration,
) -> bool {
    use esp_idf_svc::sys::*;

    let start = esp_timer_get_time();
    let timeout_us = timeout.as_micros() as i64;
    loop {
        let mut ip_info: esp_netif_ip_info_t = core::mem::zeroed();
        if esp_netif_get_ip_info(netif, &mut ip_info) == ESP_OK && ip_info.ip.addr != 0 {
            let ip = ip_info.ip.addr;
            info!(
                "Ethernet connected! IP: {}.{}.{}.{}",
                ip & 0xFF,
                (ip >> 8) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 24) & 0xFF
            );
            return true;
        }
        let elapsed = esp_timer_get_time() - start;
        if elapsed > timeout_us {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Check if a netif currently has an IP address.
#[cfg(esp32p4)]
#[allow(dead_code)]
unsafe fn check_netif_ip(netif: *mut esp_idf_svc::sys::esp_netif_t) -> bool {
    use esp_idf_svc::sys::*;
    let mut ip_info: esp_netif_ip_info_t = core::mem::zeroed();
    esp_netif_get_ip_info(netif, &mut ip_info) == ESP_OK && ip_info.ip.addr != 0
}

// --- ESP32-P4-ETH: Ethernet only (no WiFi, no modem peripheral) ---

#[cfg(all(esp32p4, not(has_wifi)))]
pub fn connect(
    _sys_loop: EspSystemEventLoop,
    _nvs: EspDefaultNvsPartition,
    _settings: &DeviceSettings,
) -> Result<NetworkHandle> {
    info!("ESP32-P4: starting Ethernet...");
    let netif = unsafe { start_p4_ethernet() };
    let got_ip = unsafe { wait_netif_ip(netif, std::time::Duration::from_secs(30)) };
    assert!(got_ip, "Ethernet DHCP timeout (30s)");
    Ok(NetworkHandle::Eth(()))
}

// --- ESP32-P4-WiFi: Ethernet + WiFi via companion ESP32-C6 (esp_hosted over SDIO) ---
// The P4 chip has no built-in radio; WiFi is provided by a companion ESP32-C6
// connected over SDIO, using the esp_hosted + esp_wifi_remote components.
// With esp_wifi_remote enabled, the standard esp-idf-svc WiFi types (EspWifi, Modem)
// are available.

#[cfg(all(esp32p4, has_wifi))]
pub fn connect(
    modem: esp_idf_svc::hal::modem::Modem<'static>,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    settings: &DeviceSettings,
) -> Result<NetworkHandle> {
    info!("ESP32-P4-WiFi: starting Ethernet...");
    let eth_netif = unsafe { start_p4_ethernet() };

    info!("ESP32-P4-WiFi: starting WiFi...");
    let sta_will = crate::wifi::sta_will_connect(settings);
    // If STA won't connect and AP is off, force AP preemptively
    // (ethernet might also fail, and we need at least one way to reach the board)
    let force_ap_preemptive = !sta_will && !settings.ap_enabled;
    if force_ap_preemptive {
        warn!("Forcing AP preemptively (WiFi client disabled/unconfigured, AP off, ethernet uncertain)");
    }

    let mut wifi = crate::wifi::connect(modem, sys_loop, nvs, settings, force_ap_preemptive)?;

    // By now wifi::connect() has done its initial STA attempt (blocking, ~15s timeout).
    // Ethernet has also been running in the background during that time.
    let eth_has_ip = unsafe { check_netif_ip(eth_netif) };
    let wifi_has_ip = wifi.sta_has_ip();
    let ap_running = settings.ap_enabled || force_ap_preemptive;

    if eth_has_ip {
        info!("ESP32-P4-WiFi: Ethernet connected");
    } else {
        info!(
            "ESP32-P4-WiFi: Ethernet not connected yet, will auto-connect when cable is plugged in"
        );
    }

    // If both ethernet and wifi STA failed initially AND AP is not running, force AP
    if !eth_has_ip && !wifi_has_ip && !ap_running {
        warn!("No initial connectivity (Ethernet + WiFi STA both failed) - forcing AP");
        wifi.force_enable_ap(settings)?;
    }

    Ok(NetworkHandle::Wifi(wifi))
}

// --- ESP32-C6 and others: WiFi only ---

#[cfg(not(any(esp32, esp32p4)))]
pub fn connect(
    modem: esp_idf_svc::hal::modem::Modem<'static>,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    settings: &DeviceSettings,
) -> Result<NetworkHandle> {
    if is_qemu() {
        info!("QEMU detected - no Ethernet MAC on this chip, running without network");
        return Ok(NetworkHandle::None);
    }
    // WiFi-only board: force AP if no other way to reach the device
    let sta_will = crate::wifi::sta_will_connect(settings);
    let force_ap = !sta_will && !settings.ap_enabled;
    if force_ap {
        warn!("WiFi-only board: forcing AP (WiFi client disabled/unconfigured and AP is off)");
    }
    let wifi = crate::wifi::connect(modem, sys_loop, nvs, settings, force_ap)?;
    Ok(NetworkHandle::Wifi(wifi))
}

// --- OpenETH (QEMU virtual Ethernet, ESP32 only) ---

#[cfg(esp32)]
fn connect_qemu_eth(
    mac: esp_idf_svc::hal::mac::MAC<'static>,
    sys_loop: EspSystemEventLoop,
) -> Result<NetworkHandle> {
    #[cfg(esp_idf_eth_use_openeth)]
    {
        use esp_idf_svc::eth::{BlockingEth, EspEth, EthDriver};

        info!("Starting OpenETH (QEMU virtual ethernet)...");

        let eth_driver = EthDriver::new_openeth(mac, sys_loop.clone())?;
        let eth = EspEth::wrap(eth_driver)?;
        let mut blocking_eth = BlockingEth::wrap(eth, sys_loop)?;

        blocking_eth.start()?;
        info!("OpenETH started, waiting for IP via DHCP...");
        blocking_eth.wait_netif_up()?;

        let ip_info = blocking_eth.eth().netif().get_ip_info()?;
        info!("OpenETH connected! IP: {}", ip_info.ip);

        return Ok(NetworkHandle::Eth(blocking_eth));
    }

    #[cfg(not(esp_idf_eth_use_openeth))]
    {
        let _ = (mac, sys_loop);
        info!("OpenETH not compiled in - running without network");
        Ok(NetworkHandle::None)
    }
}
