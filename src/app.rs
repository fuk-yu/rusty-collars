use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::info;

use crate::build_info::APP_VERSION;
use crate::led::Led;
use crate::net::{self, NetworkHandle};
use crate::remote_control::{self, RemoteControlHandle};
use crate::repository::{RepositoryServices, SharedRepository};
use crate::rf::{RfReceiver, RfTransmitter};
use crate::server::{self, AppCtx, AppWorkerHandle, TransmissionWorkerHandle};
use crate::storage::Storage;
use crate::time_sync::{self, TimeSyncHandle};

pub struct ApplicationBuilder;

pub struct Application {
    ctx: AppCtx,
    network: NetworkHandle,
    background_services_enabled: bool,
}

pub struct RunningApplication {
    network: NetworkHandle,
    _ctx: AppCtx,
    _app_worker: AppWorkerHandle,
    _time_sync: Option<TimeSyncHandle>,
    _remote_control: Option<RemoteControlHandle>,
    _transmission_worker: TransmissionWorkerHandle,
    _server: ServerHandle,
}

pub struct ServerHandle {
    _join: JoinHandle<()>,
}

impl ApplicationBuilder {
    pub fn new() -> Self {
        Self
    }

    pub fn build(self) -> Result<Application> {
        esp_idf_svc::sys::link_patches();
        esp_idf_svc::log::EspLogger::initialize_default();

        info!("Starting rusty-collars {}...", APP_VERSION);
        log_heap("boot");

        let peripherals = Peripherals::take()?;
        #[cfg(all(esp32p4, not(has_wifi)))]
        let _ = &peripherals;
        let sys_loop = EspSystemEventLoop::take()?;
        let nvs_partition = EspDefaultNvsPartition::take()?;

        register_eventfd()?;

        let repository: SharedRepository =
            Arc::new(Mutex::new(Box::new(Storage::new(nvs_partition.clone())?)));
        let repository_services = RepositoryServices::new(repository);
        let mut device_settings = repository_services.load_settings()?;
        repository_services.ensure_device_id(&mut device_settings)?;

        #[cfg(all(esp32p4, has_wifi))]
        {
            const SDIO_GPIOS: [u8; 6] = [14, 15, 16, 17, 18, 19];
            let safe: [u8; 4] = [7, 8, 5, 6];
            let pins = [
                &mut device_settings.tx_led_pin,
                &mut device_settings.rx_led_pin,
                &mut device_settings.rf_tx_pin,
                &mut device_settings.rf_rx_pin,
            ];
            for (index, pin) in pins.into_iter().enumerate() {
                if SDIO_GPIOS.contains(pin) {
                    log::warn!(
                        "GPIO{} conflicts with SDIO bus, overriding to GPIO{}",
                        *pin,
                        safe[index]
                    );
                    *pin = safe[index];
                }
            }
        }

        info!(
            "GPIO settings: TX LED={}, RX LED={}, RF TX={}, RF RX={}",
            device_settings.tx_led_pin,
            device_settings.rx_led_pin,
            device_settings.rf_tx_pin,
            device_settings.rf_rx_pin
        );

        use esp_idf_svc::hal::gpio::{AnyInputPin, AnyOutputPin};

        let tx_led_pin = unsafe { AnyOutputPin::steal(device_settings.tx_led_pin) };
        let rx_led_pin = unsafe { AnyOutputPin::steal(device_settings.rx_led_pin) };
        let tx_pin = unsafe { AnyOutputPin::steal(device_settings.rf_tx_pin) };
        let rx_pin = unsafe { AnyInputPin::steal(device_settings.rf_rx_pin) };

        let rf_tx = RfTransmitter::new(tx_pin)?;
        let rf_rx = RfReceiver::new(rx_pin)?;

        log_heap("after peripherals");

        #[cfg(esp32)]
        let network = net::connect(
            peripherals.modem,
            peripherals.mac,
            sys_loop.clone(),
            nvs_partition.clone(),
            &device_settings,
        )?;
        #[cfg(all(esp32p4, has_wifi))]
        let network = net::connect(
            peripherals.modem,
            sys_loop.clone(),
            nvs_partition.clone(),
            &device_settings,
        )?;
        #[cfg(all(esp32p4, not(has_wifi)))]
        let network = net::connect(sys_loop.clone(), nvs_partition.clone(), &device_settings)?;
        #[cfg(not(any(esp32, esp32p4)))]
        let network = net::connect(
            peripherals.modem,
            sys_loop.clone(),
            nvs_partition.clone(),
            &device_settings,
        )?;
        log_heap("after network");

        let background_services_enabled = network.supports_time_sync();

        let tx_led = Arc::new(Mutex::new(Led::new(tx_led_pin)?));
        let rx_led = Arc::new(Mutex::new(Led::new(rx_led_pin)?));

        let collars = repository_services.load_collars()?;
        let presets = repository_services.load_presets()?;
        info!(
            "Loaded {} collars and {} presets",
            collars.len(),
            presets.len()
        );

        let (mut broadcast_tx, initial_rx) = async_broadcast::broadcast(16);
        broadcast_tx.set_overflow(true);
        let broadcast_keepalive = initial_rx.deactivate();

        let ctx = server::AppCtx::new(
            Arc::new(Mutex::new(rf_tx)),
            tx_led,
            rx_led,
            broadcast_tx,
            broadcast_keepalive,
            rf_rx,
            device_settings,
            repository_services,
            collars,
            presets,
        );
        log_heap("after app context");

        Ok(Application {
            ctx,
            network,
            background_services_enabled,
        })
    }
}

impl Application {
    pub fn start(self) -> Result<RunningApplication> {
        let Application {
            ctx,
            network,
            background_services_enabled,
        } = self;

        let app_worker = server::start_app_worker(ctx.clone());
        let time_sync = if background_services_enabled {
            let time_sync_settings = ctx.device_settings();
            let time_sync_ctx = ctx.clone();
            time_sync::maybe_start(&time_sync_settings, move |server| {
                time_sync_ctx.record_event(
                    crate::protocol::EventSource::System,
                    crate::protocol::EventLogEntryKind::NtpSync { server },
                );
            })?
        } else {
            info!("Skipping NTP time sync because this target has no network stack");
            None
        };

        let remote_control = if background_services_enabled {
            Some(remote_control::start(ctx.clone())?)
        } else {
            info!("Skipping remote control because this target has no network stack");
            None
        };

        let transmission_worker = server::start_transmission_worker(ctx.clone());
        let server_ctx = ctx.clone();
        let server_join = std::thread::Builder::new()
            .name("http-server".into())
            .stack_size(65536)
            .spawn(move || {
                if let Err(err) = server::run_server(server_ctx) {
                    log::error!("Server error: {err:#}");
                }
            })?;
        log_heap("after server spawn");

        Ok(RunningApplication {
            network,
            _ctx: ctx,
            _app_worker: app_worker,
            _time_sync: time_sync,
            _remote_control: remote_control,
            _transmission_worker: transmission_worker,
            _server: ServerHandle { _join: server_join },
        })
    }
}

impl RunningApplication {
    pub fn install_watchdog(&self) {
        unsafe {
            esp_idf_svc::sys::esp_task_wdt_add(esp_idf_svc::sys::xTaskGetCurrentTaskHandle());
        }
    }

    pub fn poll(&mut self) {
        self.network.poll();
    }

    pub fn reset_watchdog(&self) {
        unsafe { esp_idf_svc::sys::esp_task_wdt_reset() };
    }
}

fn register_eventfd() -> Result<()> {
    unsafe {
        let config = esp_idf_svc::sys::esp_vfs_eventfd_config_t { max_fds: 5 };
        let err = esp_idf_svc::sys::esp_vfs_eventfd_register(&config);
        assert_eq!(
            err,
            esp_idf_svc::sys::ESP_OK,
            "esp_vfs_eventfd_register failed: {err}"
        );
    }

    Ok(())
}

fn log_heap(label: &str) {
    let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
    let min = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
    info!(
        "[heap] {label}: free={free}B ({:.1}KB), min_ever={min}B ({:.1}KB)",
        free as f64 / 1024.0,
        min as f64 / 1024.0
    );
}
