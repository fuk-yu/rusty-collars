use std::io::Write;

fn main() {
    embuild::espidf::sysenv::output();

    // Determine if this build has WiFi support.
    // ESP32, ESP32-C6, etc: always have WiFi (built-in radio).
    // ESP32-P4: only with the "p4-wifi" feature (WiFi via companion ESP32-C6 chip).
    let mcu = std::env::var("MCU").unwrap_or_default();
    let has_wifi = match mcu.as_str() {
        "esp32p4" => std::env::var("CARGO_FEATURE_P4_WIFI").is_ok(),
        _ => true,
    };
    if has_wifi {
        println!("cargo:rustc-cfg=has_wifi");
    }
    println!("cargo:rerun-if-env-changed=MCU");

    // Gzip the Vite-built frontend HTML at build time (saves ~29KB heap at runtime).
    // Run `cd frontend && npm run build` before `cargo build` if dist is missing.
    let version = app_version();
    let frontend_path = "frontend/dist/index.html";
    let html = std::fs::read_to_string(frontend_path).unwrap_or_else(|_| {
        panic!(
            "{frontend_path} not found — run `cd frontend && npm install && npm run build` first"
        )
    });
    let html = html.replace("__RUSTY_COLLARS_APP_VERSION__", &version);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(html.as_bytes())
        .expect("gzip encode failed");
    let compressed = encoder.finish().expect("gzip finish failed");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let gz_path = std::path::Path::new(&out_dir).join("frontend.html.gz");
    std::fs::write(&gz_path, &compressed).expect("write frontend.html.gz failed");
    println!(
        "cargo:warning=Frontend: {}B raw -> {}B gzip ({:.0}% reduction)",
        html.len(),
        compressed.len(),
        (1.0 - compressed.len() as f64 / html.len() as f64) * 100.0
    );

    println!(
        "cargo:rustc-env=RUSTY_COLLARS_APP_VERSION={}",
        app_version()
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=frontend");
}

fn app_version() -> String {
    let package_version = std::env::var("CARGO_PKG_VERSION").expect("missing CARGO_PKG_VERSION");
    let git_revision =
        git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "nogit".to_string());
    let dirty_suffix = if git_is_dirty() { "-dirty" } else { "" };
    let build_unix_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before UNIX_EPOCH")
        .as_secs();

    format!("{package_version}+{git_revision}{dirty_suffix}.{build_unix_s}")
}

fn git_is_dirty() -> bool {
    match std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
    {
        Ok(output) => !output.stdout.is_empty(),
        Err(_) => false,
    }
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
