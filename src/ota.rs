use anyhow::{Context, Result};
use esp_idf_svc::ota::EspOta;
use log::info;

/// Perform OTA update by reading firmware data from an async reader.
/// Returns total bytes written on success.
pub async fn perform_update<R: picoserve::io::Read>(
    content_length: usize,
    reader: &mut R,
) -> Result<usize> {
    info!("OTA: starting update ({content_length} bytes)");

    let mut ota = EspOta::new().context("EspOta::new failed")?;
    let mut update = ota
        .initiate_update_with_known_size(content_length)
        .context("initiate_update failed")?;

    let mut buf = [0u8; 4096];
    let mut written = 0usize;

    loop {
        let n = reader.read(&mut buf).await.map_err(|e| anyhow::anyhow!("read error: {e:?}"))?;
        if n == 0 {
            break;
        }
        update.write(&buf[..n]).with_context(|| format!("OTA write failed at {written}"))?;
        written += n;

        if written % (64 * 1024) < n {
            info!("OTA: {written}/{content_length} bytes ({:.0}%)", written as f64 / content_length as f64 * 100.0);
        }
    }

    assert!(written == content_length, "OTA: expected {content_length} bytes, got {written}");

    update.complete().context("OTA complete failed")?;
    info!("OTA: update complete ({written} bytes), rebooting...");

    Ok(written)
}
