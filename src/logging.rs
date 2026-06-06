use std::ffi::CString;

type EspLogLevel = esp_idf_svc::sys::esp_log_level_t;

fn parse_level(value: &str) -> Option<EspLogLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(esp_idf_svc::sys::esp_log_level_t_ESP_LOG_NONE),
        "error" => Some(esp_idf_svc::sys::esp_log_level_t_ESP_LOG_ERROR),
        "warn" | "warning" => Some(esp_idf_svc::sys::esp_log_level_t_ESP_LOG_WARN),
        "info" => Some(esp_idf_svc::sys::esp_log_level_t_ESP_LOG_INFO),
        "debug" => Some(esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG),
        "trace" | "verbose" => Some(esp_idf_svc::sys::esp_log_level_t_ESP_LOG_VERBOSE),
        _ => None,
    }
}

fn set_level(tag: &str, level: EspLogLevel) {
    if let Ok(c_tag) = CString::new(tag) {
        unsafe {
            esp_idf_svc::sys::esp_log_level_set(c_tag.as_ptr(), level);
        }
    }
}

fn level_from_sdkconfig_defaults(key: &str) -> Option<&'static str> {
    for raw_line in include_str!("../sdkconfig.defaults").lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lhs, rhs) = line.split_once('=')?;
        if lhs.trim() == key {
            return Some(rhs.trim().trim_matches('"'));
        }
    }
    None
}

fn set_level_from_key(tag: &str, key: &str, build_env_value: Option<&'static str>) {
    if let Some(raw) = build_env_value.or_else(|| level_from_sdkconfig_defaults(key)) {
        if let Some(level) = parse_level(raw) {
            set_level(tag, level);
            log::info!("log level override: {}={} (key: {})", tag, raw, key);
        } else {
            log::warn!("invalid log level for {}: '{}'", key, raw);
        }
    }
}

/// Initialize ESP logger backend and apply optional per-module level overrides.
///
/// Build-time overrides (set before `cargo`/`just flash-monitor`) are:
/// - LOG_WIFI_LEVEL
/// - LOG_GPS_LEVEL
/// - LOG_DISPLAY_LEVEL
/// - LOG_BATTERY_LEVEL
/// - LOG_PPS_LEVEL (main loop / PPS ISR task context logs)
pub fn init() {
    esp_idf_svc::log::EspLogger::initialize_default();

    set_level_from_key(
        "rust_gps_ntp::wifi",
        "LOG_WIFI_LEVEL",
        option_env!("LOG_WIFI_LEVEL"),
    );
    set_level_from_key(
        "rust_gps_ntp::gps",
        "LOG_GPS_LEVEL",
        option_env!("LOG_GPS_LEVEL"),
    );
    set_level_from_key(
        "rust_gps_ntp::display",
        "LOG_DISPLAY_LEVEL",
        option_env!("LOG_DISPLAY_LEVEL"),
    );
    set_level_from_key(
        "rust_gps_ntp::battery",
        "LOG_BATTERY_LEVEL",
        option_env!("LOG_BATTERY_LEVEL"),
    );
    set_level_from_key("rust_gps_ntp", "LOG_PPS_LEVEL", option_env!("LOG_PPS_LEVEL"));
}
