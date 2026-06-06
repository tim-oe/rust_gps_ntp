//! ESP32 firmware entrypoint.

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    rust_gps_ntp::app::run()
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("This project targets ESP32 with ESP-IDF (run through cargo espflash).");
}
