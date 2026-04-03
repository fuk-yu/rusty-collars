use std::time::Duration;

use anyhow::Result;

pub(super) async fn perform_ota_update<R: picoserve::io::Read>(
    content_length: usize,
    reader: &mut R,
) -> Result<usize> {
    crate::ota::perform_update(content_length, reader).await
}

pub(super) fn schedule_reboot(delay: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        unsafe {
            esp_idf_svc::sys::esp_restart();
        }
    });
}
