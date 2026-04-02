use crate::protocol::{
    ApClientInfo, ApStatus, DeviceSettings, InterfaceStatus, MemoryRegion, RemoteControlStatus,
    ServerMessage,
};

use super::RemoteControlUrlKind;
use url::Url;

const HAS_WIFI: bool = cfg!(has_wifi);

#[derive(Debug, Clone, Copy)]
struct MemRegion {
    total: u32,
    free: u32,
}

pub(crate) fn parse_remote_control_url(
    url: &str,
) -> core::result::Result<RemoteControlUrlKind, String> {
    parsed_remote_control_url(url).map(|(kind, _)| kind)
}

pub(crate) fn remote_control_endpoint_url(
    settings: &DeviceSettings,
) -> core::result::Result<(RemoteControlUrlKind, String), String> {
    let (kind, mut url) = parsed_remote_control_url(settings.remote_control_url.trim())?;
    if !settings.device_id.is_empty() {
        {
            let mut path_segments = url
                .path_segments_mut()
                .map_err(|_| "Remote control URL cannot accept path segments".to_string())?;
            path_segments.pop_if_empty();
            path_segments.push(&settings.device_id);
        }
    }
    Ok((kind, url.to_string()))
}

pub(crate) fn remote_control_status(
    settings: &DeviceSettings,
    connected: bool,
    rtt_ms: Option<u32>,
    status_text: impl Into<String>,
) -> RemoteControlStatus {
    RemoteControlStatus {
        enabled: settings.remote_control_enabled,
        connected,
        url: settings.remote_control_url.trim().to_string(),
        validate_cert: settings.remote_control_validate_cert,
        rtt_ms,
        status_text: status_text.into(),
    }
}

pub(super) fn remote_control_status_from_settings(
    settings: &DeviceSettings,
) -> RemoteControlStatus {
    let trimmed_url = settings.remote_control_url.trim();
    let status_text = if !settings.remote_control_enabled {
        "Off"
    } else if trimmed_url.is_empty() {
        "Missing URL"
    } else if parse_remote_control_url(trimmed_url).is_err() {
        "Invalid URL"
    } else {
        "Connecting..."
    };
    remote_control_status(settings, false, None, status_text)
}

pub(super) fn gather_network_status(settings: &DeviceSettings) -> ServerMessage<'static> {
    use esp_idf_svc::sys::*;

    let board_mac = {
        let mut mac = [0u8; 6];
        unsafe { esp_efuse_mac_get_default(mac.as_mut_ptr()) };
        format_mac(&mac)
    };

    let ethernet = interface_status(b"ETH_DEF\0", true);
    let wifi_sta = if HAS_WIFI {
        interface_status(
            b"WIFI_STA_DEF\0",
            settings.wifi_client_enabled && !settings.wifi_ssid.is_empty(),
        )
    } else {
        disabled_interface_status()
    };

    let wifi_ap = if HAS_WIFI {
        let mac = netif_mac(b"WIFI_AP_DEF\0");
        let ip = netif_ip(b"WIFI_AP_DEF\0");
        let available = !mac.is_empty();
        ApStatus {
            available,
            enabled: settings.ap_enabled,
            mac,
            ip,
            clients: if available {
                gather_ap_clients()
            } else {
                Vec::new()
            },
        }
    } else {
        ApStatus {
            available: false,
            enabled: false,
            mac: String::new(),
            ip: String::new(),
            clients: Vec::new(),
        }
    };

    let min_free_heap_bytes = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
    let memory = [
        ("Internal", MALLOC_CAP_INTERNAL),
        ("PSRAM", MALLOC_CAP_SPIRAM),
        ("DMA", MALLOC_CAP_DMA),
        ("RTCRAM", MALLOC_CAP_RTCRAM),
        ("TCM", MALLOC_CAP_TCM),
    ]
    .into_iter()
    .filter_map(|(name, cap)| {
        let region = mem_region(cap);
        (region.total > 0).then(|| MemoryRegion {
            name: name.to_string(),
            total_bytes: region.total,
            free_bytes: region.free,
        })
    })
    .collect();

    ServerMessage::NetworkStatus {
        board_mac,
        memory,
        min_free_heap_bytes,
        ethernet,
        wifi_sta,
        wifi_ap,
    }
}

fn parsed_remote_control_url(
    url: &str,
) -> core::result::Result<(RemoteControlUrlKind, Url), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("Remote control URL host cannot be empty".to_string());
    }

    let parsed = Url::parse(trimmed).map_err(|err| format!("Invalid remote control URL: {err}"))?;
    let kind = match parsed.scheme() {
        "ws" => RemoteControlUrlKind::Ws,
        "wss" => RemoteControlUrlKind::Wss,
        _ => return Err("Remote control URL must start with ws:// or wss://".to_string()),
    };

    if parsed.host_str().is_none() {
        return Err("Remote control URL host cannot be empty".to_string());
    }

    Ok((kind, parsed))
}

fn mem_region(cap: u32) -> MemRegion {
    use esp_idf_svc::sys::*;

    unsafe {
        MemRegion {
            total: heap_caps_get_total_size(cap) as u32,
            free: heap_caps_get_free_size(cap) as u32,
        }
    }
}

fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn format_ip(addr: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        addr & 0xFF,
        (addr >> 8) & 0xFF,
        (addr >> 16) & 0xFF,
        (addr >> 24) & 0xFF
    )
}

fn netif_handle(key: &[u8]) -> *mut esp_idf_svc::sys::esp_netif_t {
    unsafe { esp_idf_svc::sys::esp_netif_get_handle_from_ifkey(key.as_ptr() as *const _) }
}

fn netif_ip(key: &[u8]) -> String {
    use esp_idf_svc::sys::*;

    let netif = netif_handle(key);
    if netif.is_null() {
        return String::new();
    }

    unsafe {
        let mut ip_info: esp_netif_ip_info_t = core::mem::zeroed();
        if esp_netif_get_ip_info(netif, &mut ip_info) == ESP_OK && ip_info.ip.addr != 0 {
            format_ip(ip_info.ip.addr)
        } else {
            String::new()
        }
    }
}

fn netif_mac(key: &[u8]) -> String {
    use esp_idf_svc::sys::*;

    let netif = netif_handle(key);
    if netif.is_null() {
        return String::new();
    }

    unsafe {
        let mut mac = [0u8; 6];
        if esp_netif_get_mac(netif, mac.as_mut_ptr()) == ESP_OK {
            format_mac(&mac)
        } else {
            String::new()
        }
    }
}

fn interface_status(key: &[u8], enabled: bool) -> InterfaceStatus {
    let mac = netif_mac(key);
    let ip = netif_ip(key);
    let available = !mac.is_empty();
    InterfaceStatus {
        available,
        enabled,
        mac,
        connected: !ip.is_empty(),
        ip,
    }
}

fn disabled_interface_status() -> InterfaceStatus {
    InterfaceStatus {
        available: false,
        enabled: false,
        mac: String::new(),
        ip: String::new(),
        connected: false,
    }
}

fn gather_ap_clients() -> Vec<ApClientInfo> {
    use esp_idf_svc::sys::*;

    unsafe {
        let mut sta_list: wifi_sta_list_t = core::mem::zeroed();
        if esp_wifi_ap_get_sta_list(&mut sta_list) != ESP_OK {
            return Vec::new();
        }

        (0..sta_list.num as usize)
            .map(|index| ApClientInfo {
                mac: format_mac(&sta_list.sta[index].mac),
                ip: String::new(),
            })
            .collect()
    }
}
