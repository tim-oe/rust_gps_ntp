//! Logging configuration helpers for ESP-IDF tagged logging.
//!
//! This module applies per-module log level overrides sourced from build-time
//! environment variables or `sdkconfig.defaults`.

use std::ffi::CString;

type EspLogLevel = esp_idf_svc::sys::esp_log_level_t;

/// Parse a textual log level into the corresponding ESP-IDF enum.
///
/// # Parameters
/// - `value`: User-provided log level string.
///
/// # Returns
/// - `Some(EspLogLevel)` when the value is recognized.
/// - `None` when the value is invalid.
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

/// Apply a log level override for a specific ESP-IDF log tag.
///
/// # Parameters
/// - `tag`: ESP-IDF log tag string.
/// - `level`: Desired ESP-IDF log level.
///
/// # Returns
/// - No return value.
fn set_level(tag: &str, level: EspLogLevel) {
    if let Ok(c_tag) = CString::new(tag) {
        unsafe {
            esp_idf_svc::sys::esp_log_level_set(c_tag.as_ptr(), level);
        }
    }
}

/// Read a key value from `sdkconfig.defaults` bundled at compile time.
///
/// # Parameters
/// - `key`: Configuration key to search for.
///
/// # Returns
/// - `Some(&'static str)` with the configured value when found.
/// - `None` when the key is not present.
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

/// Resolve an effective level from build env first, then `sdkconfig.defaults`.
///
/// # Parameters
/// - `key`: Configuration key name.
/// - `build_env_value`: Optional compile-time environment override.
///
/// # Returns
/// - `Some(EspLogLevel)` when a valid value is resolved.
/// - `None` when no value exists or parsing fails.
fn level_from_key(key: &str, build_env_value: Option<&'static str>) -> Option<EspLogLevel> {
    let raw = build_env_value.or_else(|| level_from_sdkconfig_defaults(key))?;
    parse_level(raw)
}

/// Return true when a boolean-like sdkconfig key is enabled.
///
/// # Parameters
/// - `key`: Boolean sdkconfig key name.
///
/// # Returns
/// - `true` when the key is set to a truthy value.
/// - `false` otherwise.
fn sdkconfig_flag_enabled(key: &str) -> bool {
    matches!(
        level_from_sdkconfig_defaults(key),
        Some("y" | "1" | "true" | "yes")
    )
}

/// Compute global default log level from sdkconfig defaults.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Effective default ESP-IDF log level.
fn global_default_level() -> EspLogLevel {
    if sdkconfig_flag_enabled("CONFIG_LOG_DEFAULT_LEVEL_VERBOSE") {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_VERBOSE
    } else if sdkconfig_flag_enabled("CONFIG_LOG_DEFAULT_LEVEL_DEBUG") {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG
    } else if sdkconfig_flag_enabled("CONFIG_LOG_DEFAULT_LEVEL_INFO") {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_INFO
    } else if sdkconfig_flag_enabled("CONFIG_LOG_DEFAULT_LEVEL_WARN") {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_WARN
    } else if sdkconfig_flag_enabled("CONFIG_LOG_DEFAULT_LEVEL_ERROR") {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_ERROR
    } else if sdkconfig_flag_enabled("CONFIG_LOG_DEFAULT_LEVEL_NONE") {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_NONE
    } else {
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_INFO
    }
}

/// Apply a level override for one module tag if the key is set.
///
/// # Parameters
/// - `tag`: ESP-IDF log tag to override.
/// - `key`: Config key checked in env and sdkconfig defaults.
/// - `build_env_value`: Optional compile-time environment override.
///
/// # Returns
/// - No return value.
fn set_level_from_key(tag: &str, key: &str, build_env_value: Option<&'static str>) {
    if let Some(raw) = build_env_value.or_else(|| level_from_sdkconfig_defaults(key)) {
        if let Some(level) = level_from_key(key, build_env_value) {
            set_level(tag, level);
            log::info!("Logging: override {}={} (key={})", tag, raw, key);
        } else {
            log::warn!("Logging: invalid level for {}: '{}'", key, raw);
        }
    }
}

/// Return whether the display boot test should run once at startup.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - `true` when effective display log level is `debug` or `trace`.
/// - `false` otherwise.
pub fn display_boot_test_enabled() -> bool {
    let effective_display_level =
        level_from_key("LOG_DISPLAY_LEVEL", option_env!("LOG_DISPLAY_LEVEL"))
            .unwrap_or_else(global_default_level);
    matches!(
        effective_display_level,
        esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG
            | esp_idf_svc::sys::esp_log_level_t_ESP_LOG_VERBOSE
    )
}

/// Initialize ESP logger backend and apply optional per-module level overrides.
///
/// Build-time overrides (set before `cargo`/`just flash-monitor`) are:
/// - LOG_WIFI_LEVEL
/// - LOG_GPS_LEVEL
/// - LOG_DISPLAY_LEVEL
/// - LOG_BATTERY_LEVEL
/// - LOG_NTP_LEVEL
/// - LOG_PPS_LEVEL (main loop / PPS ISR task context logs)
///
/// # Parameters
/// - None.
///
/// # Returns
/// - No return value.
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
    set_level_from_key(
        "rust_gps_ntp::ntp",
        "LOG_NTP_LEVEL",
        option_env!("LOG_NTP_LEVEL"),
    );
    set_level_from_key(
        "rust_gps_ntp",
        "LOG_PPS_LEVEL",
        option_env!("LOG_PPS_LEVEL"),
    );
}
