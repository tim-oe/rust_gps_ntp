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

    let acl_cidr =
        read_sdkconfig_string("sdkconfig.defaults", "CONFIG_GPS_NTP_ACL_CIDR").unwrap_or_default();
    if !acl_cidr.is_empty() && parse_ipv4_cidr(&acl_cidr).is_none() {
        panic!(
            "invalid CONFIG_GPS_NTP_ACL_CIDR={acl_cidr:?} in sdkconfig.defaults \
             (expected format: 192.168.1.0/24)"
        );
    }
    println!("cargo::rustc-env=NTP_ACL_CIDR={acl_cidr}");
}

/// Parse `a.b.c.d/prefix` (prefix 0–32). Returns `None` for empty/invalid input.
fn parse_ipv4_cidr(s: &str) -> Option<(u8, u8, u8, u8, u8)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (addr, prefix) = s.split_once('/')?;
    let prefix: u8 = prefix.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mut octets = addr.split('.');
    let a: u8 = octets.next()?.parse().ok()?;
    let b: u8 = octets.next()?.parse().ok()?;
    let c: u8 = octets.next()?.parse().ok()?;
    let d: u8 = octets.next()?.parse().ok()?;
    if octets.next().is_some() {
        return None;
    }
    Some((a, b, c, d, prefix))
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
