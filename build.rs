fn main() {
    println!("cargo:rerun-if-env-changed=WIFI_SSID");
    println!("cargo:rerun-if-env-changed=WIFI_PASS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }
    // Declare ESP-IDF component cfg keys so rustc's check-cfg lint does not
    // flag them as unknown.  These are set by the embuild/ESP-IDF build system
    // when the corresponding component is enabled in sdkconfig.
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_mdns_enabled)");
    println!("cargo::rustc-check-cfg=cfg(esp_idf_comp_espressif__mdns_enabled)");

    // Expose CONFIG_LWIP_LOCAL_HOSTNAME from sdkconfig.defaults as a Rust
    // compile-time env var so source files can use env!("DEVICE_HOSTNAME")
    // instead of a hardcoded string.  Rebuild if the file changes.
    println!("cargo::rerun-if-changed=sdkconfig.defaults");
    let hostname = read_sdkconfig_string("sdkconfig.defaults", "CONFIG_LWIP_LOCAL_HOSTNAME")
        .unwrap_or_else(|| "espressif".to_owned());
    println!("cargo::rustc-env=DEVICE_HOSTNAME={hostname}");

    let nmea_pps_fudge =
        read_sdkconfig_string("sdkconfig.defaults", "CONFIG_GPS_NTP_NMEA_PPS_FUDGE_S")
            .and_then(|v| if v == "0" { Some(0i64) } else { None })
            .unwrap_or(1i64);
    println!("cargo::rustc-env=NMEA_PPS_FUDGE_S={nmea_pps_fudge}");
}

/// Parse a quoted string value from a Kconfig-style `sdkconfig.defaults` file.
/// Returns `None` if the key is absent or the file cannot be read.
fn read_sdkconfig_string(path: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key)
            && let Some(rest) = rest.strip_prefix('=')
        {
            // Strip surrounding quotes if present.
            let value = rest.trim_matches('"');
            return Some(value.to_owned());
        }
    }
    None
}
