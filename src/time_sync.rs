use anyhow::{anyhow, Result};
use esp_idf_svc::sntp::{EspSntp, SntpConf};
use log::info;

use crate::protocol::DeviceSettings;

const DAILY_SYNC_INTERVAL_MS: u32 = 24 * 60 * 60 * 1000;

pub struct TimeSyncHandle {
    _sntp: EspSntp<'static>,
}

pub fn maybe_start(settings: &DeviceSettings) -> Result<Option<TimeSyncHandle>> {
    if !settings.ntp_enabled {
        info!("NTP time sync disabled");
        return Ok(None);
    }

    let server = settings.ntp_server.trim();
    if server.is_empty() {
        return Err(anyhow!(
            "NTP server cannot be empty when time sync is enabled"
        ));
    }

    let mut conf = SntpConf::default();
    conf.servers[0] = server;

    let server_for_callback = server.to_string();
    let sntp = EspSntp::new_with_callback(&conf, move |_| {
        info!("NTP time synchronized via '{}'", server_for_callback);
    })?;

    unsafe {
        esp_idf_svc::sys::sntp_set_sync_interval(DAILY_SYNC_INTERVAL_MS);
        assert!(
            esp_idf_svc::sys::sntp_restart(),
            "sntp_restart failed after setting sync interval"
        );
    }

    info!(
        "NTP time sync enabled: server='{}', interval={}s",
        server,
        DAILY_SYNC_INTERVAL_MS / 1000
    );

    Ok(Some(TimeSyncHandle { _sntp: sntp }))
}
