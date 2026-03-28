#![allow(unexpected_cfgs)]

use std::sync::{Arc, Mutex};

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::info;

mod async_runtime;
mod build_info;
mod led;
#[allow(unexpected_cfgs)]
mod net;
mod ota;
mod rf;
mod server;
mod storage;
#[cfg(not(esp32p4))]
mod wifi;

pub use rusty_collars_core::{protocol, scheduling, validation};

fn log_heap(label: &str) {
    let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
    let min = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
    info!("[heap] {label}: free={free}B ({:.1}KB), min_ever={min}B ({:.1}KB)",
        free as f64 / 1024.0, min as f64 / 1024.0);
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Starting rusty-collars {}...", build_info::APP_VERSION);
    log_heap("boot");

    let _peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs_partition = EspDefaultNvsPartition::take()?;

    // Register eventfd VFS driver (required by async-io/polling on ESP-IDF)
    unsafe {
        let config = esp_idf_svc::sys::esp_vfs_eventfd_config_t { max_fds: 5 };
        let err = esp_idf_svc::sys::esp_vfs_eventfd_register(&config);
        assert_eq!(err, esp_idf_svc::sys::ESP_OK, "esp_vfs_eventfd_register failed: {err}");
    }

    // Load GPIO settings from NVS (before consuming peripherals)
    let temp_storage = storage::Storage::new(nvs_partition.clone())?;
    let device_settings = temp_storage.load_settings()?;
    drop(temp_storage);
    info!(
        "GPIO settings: TX LED={}, RX LED={}, RF TX={}, RF RX={}",
        device_settings.tx_led_pin, device_settings.rx_led_pin,
        device_settings.rf_tx_pin, device_settings.rf_rx_pin
    );

    // Create pins from settings (unsafe: we trust the stored pin numbers)
    use esp_idf_svc::hal::gpio::{AnyInputPin, AnyOutputPin};
    let tx_led_pin = unsafe { AnyOutputPin::steal(device_settings.tx_led_pin) };
    let rx_led_pin = unsafe { AnyOutputPin::steal(device_settings.rx_led_pin) };
    let tx_pin = unsafe { AnyOutputPin::steal(device_settings.rf_tx_pin) };
    let rx_pin = unsafe { AnyInputPin::steal(device_settings.rf_rx_pin) };

    let rf_tx = rf::RfTransmitter::new(tx_pin)?;
    let rf_rx = rf::RfReceiver::new(rx_pin)?;

    log_heap("after peripherals");

    // Network: WiFi on ESP32/C6, Ethernet on P4, OpenETH on QEMU
    #[cfg(esp32)]
    let mut network = net::connect(_peripherals.modem, _peripherals.mac, sys_loop.clone(), nvs_partition.clone(), &device_settings)?;
    #[cfg(esp32p4)]
    let mut network = net::connect(sys_loop.clone(), nvs_partition.clone(), &device_settings)?;
    #[cfg(not(any(esp32, esp32p4)))]
    let mut network = net::connect(_peripherals.modem, sys_loop.clone(), nvs_partition.clone(), &device_settings)?;
    log_heap("after network");

    let tx_led = Arc::new(Mutex::new(led::Led::new(tx_led_pin)?));
    let rx_led = Arc::new(Mutex::new(led::Led::new(rx_led_pin)?));

    // Storage
    let storage = storage::Storage::new(nvs_partition)?;

    // Load saved state
    let collars = storage.load_collars()?;
    let presets = storage.load_presets()?;
    info!("Loaded {} collars and {} presets", collars.len(), presets.len());

    // Broadcast channel for server-push (background threads → WS clients)
    let (mut broadcast_tx, _initial_rx) = async_broadcast::broadcast(16);
    broadcast_tx.set_overflow(true); // drop oldest messages instead of failing

    // Shared application context
    let ctx = server::AppCtx::new(
        Arc::new(Mutex::new(rf_tx)),
        tx_led,
        rx_led,
        broadcast_tx,
        rf_rx,
        device_settings,
        storage,
        collars,
        presets,
    );
    log_heap("after app context");

    // Start picoserve HTTP+WS server on a dedicated thread
    let server_ctx = ctx.clone();
    let _server = std::thread::Builder::new()
        .name("http-server".into())
        .stack_size(65536)
        .spawn(move || {
            if let Err(e) = server::run_server(server_ctx) {
                log::error!("Server error: {e:#}");
            }
        })?;
    log_heap("after server spawn");

    // Main thread: WiFi polling + watchdog
    unsafe {
        esp_idf_svc::sys::esp_task_wdt_add(esp_idf_svc::sys::xTaskGetCurrentTaskHandle());
    }

    info!("Server running. Waiting for connections...");

    loop {
        network.poll();
        unsafe { esp_idf_svc::sys::esp_task_wdt_reset() };
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}
