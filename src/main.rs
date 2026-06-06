//! ESP32 firmware entrypoint.

/// ESP-IDF firmware entry: link patches and delegate to [`rust_gps_ntp::app`].
///
/// # Parameters
/// - None.
///
/// # Returns
/// - `Ok(())` only if the firmware main loop exits cleanly.
/// - `Err` when initialization in [`rust_gps_ntp::app::run`] fails.
#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    rust_gps_ntp::app::run()
}

/// Host stub entrypoint used when building for non-ESP targets.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - No return value.
#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("This project targets ESP32 with ESP-IDF (run through cargo espflash).");
}
