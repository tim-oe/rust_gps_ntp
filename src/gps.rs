//! GPS sentence parsing and display-oriented snapshot shaping.
//!
//! The parser ingests NMEA RMC/GGA sentences and maintains a lightweight state
//! object that is consumed by display and NTP paths.
//!
//! On ESP-IDF, [`GpsUart`] owns UART ingest and calls [`GpsConsumer`] callbacks
//! synchronously when sentences parse (no event queue).

use chrono::{
    Duration as ChronoDuration, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc,
};
use chrono_tz::Tz;
use std::str::FromStr;
use std::sync::{OnceLock, RwLock};

#[cfg(target_os = "espidf")]
use anyhow::Context;
#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::gpio;
#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::peripheral::Peripheral;
#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::prelude::*;
#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::uart::{self, UartDriver};

/// GPS module UART TX pin (NMEA output from the FeatherWing).
#[cfg(target_os = "espidf")]
pub const UART_TX_PIN: i32 = 1;
/// GPS module UART RX pin (NMEA input to the ESP32).
#[cfg(target_os = "espidf")]
pub const UART_RX_PIN: i32 = 2;

/// GPS NMEA UART driver with GPIO lines claimed from [`crate::pins::PinPool`].
#[cfg(target_os = "espidf")]
pub struct GpsUart {
    driver: UartDriver<'static>,
    snapshot: GpsSnapshot,
    rx_buf: [u8; 256],
    line_buf: String,
    bytes_seen: u64,
}

/// Synchronous callbacks invoked by [`GpsUart::poll`] when NMEA sentences parse.
///
/// Consumers are called directly in the ingest path (no queue) so UTC and fix
/// updates reach NTP with minimal latency.
#[cfg(target_os = "espidf")]
pub trait GpsConsumer {
    /// Latest snapshot changed (RMC or GGA).
    fn on_snapshot(&mut self, gps: &GpsSnapshot);
    /// RMC reported an active fix; called after [`Self::on_snapshot`].
    fn on_fix(&mut self, gps: &GpsSnapshot);
}

#[cfg(target_os = "espidf")]
impl GpsUart {
    const MODULE: &'static str = "gps";

    /// Initialize UART1 for GPS NMEA at 9600 baud.
    pub fn init<UART: uart::Uart>(
        pool: &mut crate::pins::PinPool,
        uart_peripheral: impl Peripheral<P = UART> + 'static,
    ) -> anyhow::Result<Self> {
        let tx = pool.take_gpio1(Self::MODULE).map_err(anyhow::Error::from)?;
        let rx = pool.take_gpio2(Self::MODULE).map_err(anyhow::Error::from)?;
        let cfg = uart::config::Config::default().baudrate(Hertz(9_600));
        let driver = UartDriver::new(
            uart_peripheral,
            tx,
            rx,
            Option::<gpio::Gpio0>::None,
            Option::<gpio::Gpio1>::None,
            &cfg,
        )
        .context("failed to initialize GPS UART")?;
        log::info!(
            "GPS: listening for raw NMEA on UART1 (9600 baud), TX=GPIO{}, RX=GPIO{}",
            UART_TX_PIN,
            UART_RX_PIN
        );
        Ok(Self {
            driver,
            snapshot: GpsSnapshot::default(),
            rx_buf: [0_u8; 256],
            line_buf: String::new(),
            bytes_seen: 0,
        })
    }

    /// Replace the working snapshot (for example with a boot-time timezone seed).
    pub fn set_snapshot(&mut self, snapshot: GpsSnapshot) {
        self.snapshot = snapshot;
    }

    /// Read-only view of the latest parsed GPS state.
    pub fn snapshot(&self) -> &GpsSnapshot {
        &self.snapshot
    }

    /// Mutable view of the latest parsed GPS state.
    pub fn snapshot_mut(&mut self) -> &mut GpsSnapshot {
        &mut self.snapshot
    }

    /// Total UART bytes received since boot (diagnostics).
    pub fn bytes_seen(&self) -> u64 {
        self.bytes_seen
    }

    /// Non-blocking UART read, NMEA parse, and direct consumer notification.
    ///
    /// Uses `timeout = 0` on the UART read so the main loop never blocks waiting
    /// for GPS bytes and NTP packets are not queued behind UART I/O.
    pub fn poll<C: GpsConsumer>(&mut self, consumer: &mut C) {
        let Ok(read) = self.driver.read(&mut self.rx_buf, 0) else {
            return;
        };
        if read == 0 {
            return;
        }

        self.bytes_seen += read as u64;
        let Ok(chunk) = core::str::from_utf8(&self.rx_buf[..read]) else {
            log::info!("GPS: UART received {} non-UTF8 bytes", read);
            return;
        };

        self.line_buf.push_str(chunk);
        let mut pending_line = String::new();
        while let Some(newline_idx) = self.line_buf.find('\n') {
            pending_line.clear();
            pending_line.push_str(self.line_buf[..newline_idx].trim_end_matches('\r').trim());
            self.line_buf.drain(..=newline_idx);
            let trimmed = pending_line.as_str();

            if !trimmed.starts_with('$') {
                continue;
            }

            if trimmed.starts_with("$GNRMC") || trimmed.starts_with("$GPRMC") {
                if parse_rmc(trimmed, &mut self.snapshot).is_some() {
                    consumer.on_snapshot(&self.snapshot);
                    if self.snapshot.fix {
                        consumer.on_fix(&self.snapshot);
                    }
                }
            } else if trimmed.starts_with("$GNGGA") || trimmed.starts_with("$GPGGA") {
                if parse_gga(trimmed, &mut self.snapshot).is_some() {
                    consumer.on_snapshot(&self.snapshot);
                }
            }
        }
    }

    /// Release GPIO claims held by this UART driver.
    pub fn close(self, pool: &mut crate::pins::PinPool) {
        pool.release(UART_TX_PIN);
        pool.release(UART_RX_PIN);
    }
}

#[cfg(target_os = "espidf")]
impl std::ops::Deref for GpsUart {
    type Target = UartDriver<'static>;

    fn deref(&self) -> &Self::Target {
        &self.driver
    }
}

static RUNTIME_TZ: OnceLock<RwLock<Option<Tz>>> = OnceLock::new();

/// Current GPS-derived values used by UI and NTP logic.
#[derive(Debug, Clone, Default)]
pub struct GpsSnapshot {
    /// Local date string derived from UTC and longitude-based timezone estimate.
    pub local_date: String,
    /// Local time string derived from UTC and longitude-based timezone estimate.
    pub local_time: String,
    /// Estimated local timezone offset in hours.
    pub tz_offset_hours: i8,
    /// Parsed UTC seconds since Unix epoch from the latest valid RMC sentence.
    pub utc_unix_seconds: Option<i64>,
    /// Latitude in signed decimal degrees.
    pub lat: f32,
    /// Longitude in signed decimal degrees.
    pub lon: f32,
    /// True when RMC status reports an active fix.
    pub fix: bool,
    /// Satellite count from the latest GGA sentence.
    pub sats: u8,
    /// Antenna altitude above mean sea level in meters (GGA field 9).
    pub altitude_m: Option<f32>,
}

/// Validate the NMEA XOR checksum suffix (`*$HH`).
///
/// # Parameters
/// - `sentence`: Full NMEA sentence including `$` prefix and `*HH` checksum field.
///
/// # Returns
/// - `true` when the checksum field matches the XOR of bytes between `$` and `*`.
/// - `false` when the sentence format is invalid or the checksum does not match.
pub fn nmea_checksum_valid(sentence: &str) -> bool {
    let bytes = sentence.as_bytes();
    if bytes.first() != Some(&b'$') {
        return false;
    }
    let Some(star_idx) = sentence.rfind('*') else {
        return false;
    };
    if star_idx + 3 > sentence.len() {
        return false;
    }
    let Ok(expected) = u8::from_str_radix(&sentence[star_idx + 1..star_idx + 3], 16) else {
        return false;
    };
    let mut computed = 0_u8;
    for &byte in &bytes[1..star_idx] {
        computed ^= byte;
    }
    computed == expected
}

/// Validate and normalize an NMEA `hhmmss` time field.
///
/// # Parameters
/// - `raw`: Raw NMEA field that should begin with `hhmmss`.
///
/// # Returns
/// - `Some(&str)` containing the normalized first 6 characters.
/// - `None` when the field is too short or non-ASCII.
fn parse_hhmmss(raw: &str) -> Option<&str> {
    if raw.len() < 6 || !raw.is_ascii() {
        return None;
    }
    Some(&raw[..6])
}

/// Format a normalized `hhmmss` field as `HH:MM:SS`.
///
/// # Parameters
/// - `raw6`: Normalized six-character time field.
///
/// # Returns
/// - Formatted `HH:MM:SS` string.
fn format_hhmmss(raw6: &str) -> String {
    format!("{}:{}:{}", &raw6[0..2], &raw6[2..4], &raw6[4..6])
}

/// Validate and normalize an NMEA `ddmmyy` date field.
///
/// # Parameters
/// - `raw`: Raw NMEA field that should begin with `ddmmyy`.
///
/// # Returns
/// - `Some(&str)` containing the normalized first 6 characters.
/// - `None` when the field is too short or non-ASCII.
fn parse_ddmmyy(raw: &str) -> Option<&str> {
    if raw.len() < 6 || !raw.is_ascii() {
        return None;
    }
    Some(&raw[..6])
}

/// Format a normalized `ddmmyy` field as `YYYY-MM-DD`.
///
/// # Parameters
/// - `raw6`: Normalized six-character date field.
///
/// # Returns
/// - Formatted `YYYY-MM-DD` string using a 2000-based century.
fn format_ddmmyy(raw6: &str) -> String {
    format!("20{}-{}-{}", &raw6[4..6], &raw6[2..4], &raw6[0..2])
}

/// Convert NMEA `ddmm.mmmm`/`dddmm.mmmm` coordinates to decimal degrees.
///
/// # Parameters
/// - `value`: Coordinate field in NMEA degree-minute format.
/// - `dir`: Hemisphere designator (`N`, `S`, `E`, or `W`).
///
/// # Returns
/// - `Some(f32)` signed decimal degrees.
/// - `None` when parsing fails.
fn nmea_to_decimal(value: &str, dir: &str) -> Option<f32> {
    let raw: f32 = value.parse().ok()?;
    let degrees = (raw / 100.0).floor();
    let minutes = raw - (degrees * 100.0);
    let mut decimal = degrees + (minutes / 60.0);
    if dir == "S" || dir == "W" {
        decimal = -decimal;
    }
    Some(decimal)
}

/// Estimate local datetime from UTC fields and longitude-derived timezone.
///
/// # Parameters
/// - `utc_date`: Date field in `ddmmyy` format.
/// - `utc_time`: Time field in `hhmmss` format.
/// - `lon`: Longitude used to estimate timezone offset (`round(lon/15)`).
///
/// # Returns
/// - `Some((local_date, local_time, tz_offset_hours))` when conversion succeeds.
/// - `None` when UTC date/time parsing fails.
fn local_datetime_from_utc(
    utc_date: &str,
    utc_time: &str,
    lon: f32,
) -> Option<(String, String, i8)> {
    let ddmmyy = parse_ddmmyy(utc_date)?;
    let hhmmss = parse_hhmmss(utc_time)?;

    let day: u32 = ddmmyy[0..2].parse().ok()?;
    let month: u32 = ddmmyy[2..4].parse().ok()?;
    let year: i32 = 2000 + ddmmyy[4..6].parse::<i32>().ok()?;
    let hour: u32 = hhmmss[0..2].parse().ok()?;
    let minute: u32 = hhmmss[2..4].parse().ok()?;
    let second: u32 = hhmmss[4..6].parse().ok()?;

    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_opt(hour, minute, second)?;
    let utc_dt = NaiveDateTime::new(date, time).and_utc();

    if let Some(tz) = configured_timezone() {
        let local = utc_dt.with_timezone(&tz);
        let tz_offset_h = (local.offset().fix().local_minus_utc() / 3600) as i8;
        Some((
            local.date_naive().format("%Y-%m-%d").to_string(),
            local.time().format("%H:%M:%S").to_string(),
            tz_offset_h,
        ))
    } else {
        let tz_offset_h = (lon / 15.0).round() as i8;
        let dt = utc_dt.naive_utc() + ChronoDuration::hours(tz_offset_h as i64);
        Some((
            dt.date().format("%Y-%m-%d").to_string(),
            dt.time().format("%H:%M:%S").to_string(),
            tz_offset_h,
        ))
    }
}

fn configured_timezone() -> Option<Tz> {
    runtime_timezone().or_else(|| {
        static TZ: OnceLock<Option<Tz>> = OnceLock::new();
        *TZ.get_or_init(|| option_env!("LOCAL_TZ").and_then(|name| Tz::from_str(name).ok()))
    })
}

fn runtime_timezone() -> Option<Tz> {
    let lock = RUNTIME_TZ.get_or_init(|| RwLock::new(None));
    lock.read().ok().and_then(|guard| *guard)
}

/// Set the runtime timezone from an IANA timezone name.
///
/// # Parameters
/// - `tz_name`: IANA timezone identifier, for example `America/Chicago`.
///
/// # Returns
/// - `true` if parsing and update succeeded.
/// - `false` if `tz_name` is invalid.
pub fn set_runtime_timezone(tz_name: &str) -> bool {
    let Some(tz) = Tz::from_str(tz_name).ok() else {
        return false;
    };
    let lock = RUNTIME_TZ.get_or_init(|| RwLock::new(None));
    if let Ok(mut guard) = lock.write() {
        *guard = Some(tz);
        true
    } else {
        false
    }
}

/// Local offset in whole hours for `utc_unix_seconds` using the active IANA TZ.
///
/// Uses the runtime timezone when set, otherwise the `LOCAL_TZ` build-time default.
/// Returns `0` when neither is configured.
pub fn tz_offset_hours_at_unix(utc_unix_seconds: i64) -> i8 {
    let Some(tz) = configured_timezone() else {
        return 0;
    };
    let Some(utc_dt) = Utc.timestamp_opt(utc_unix_seconds, 0).single() else {
        return 0;
    };
    let local = utc_dt.with_timezone(&tz);
    (local.offset().fix().local_minus_utc() / 3600) as i8
}

/// Format UTC Unix seconds as local date/time using the active IANA timezone.
///
/// Uses the runtime timezone when set, otherwise the `LOCAL_TZ` build-time default.
/// Returns `None` when neither is configured.
pub fn local_from_utc_unix(utc_unix_seconds: i64) -> Option<(String, String)> {
    let tz = configured_timezone()?;
    let utc_dt = Utc.timestamp_opt(utc_unix_seconds, 0).single()?;
    let local = utc_dt.with_timezone(&tz);
    Some((
        local.date_naive().format("%Y-%m-%d").to_string(),
        local.time().format("%H:%M:%S").to_string(),
    ))
}

/// Convert raw UTC date/time NMEA fields into a UTC `NaiveDateTime`.
///
/// # Parameters
/// - `utc_date`: Date field in `ddmmyy` format.
/// - `utc_time`: Time field in `hhmmss` format.
///
/// # Returns
/// - `Some(NaiveDateTime)` when both fields parse correctly.
/// - `None` when parsing fails.
fn utc_datetime_from_fields(utc_date: &str, utc_time: &str) -> Option<NaiveDateTime> {
    let ddmmyy = parse_ddmmyy(utc_date)?;
    let hhmmss = parse_hhmmss(utc_time)?;

    let day: u32 = ddmmyy[0..2].parse().ok()?;
    let month: u32 = ddmmyy[2..4].parse().ok()?;
    let year: i32 = 2000 + ddmmyy[4..6].parse::<i32>().ok()?;
    let hour: u32 = hhmmss[0..2].parse().ok()?;
    let minute: u32 = hhmmss[2..4].parse().ok()?;
    let second: u32 = hhmmss[4..6].parse().ok()?;

    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_opt(hour, minute, second)?;
    Some(NaiveDateTime::new(date, time))
}

/// Parse an RMC sentence and update position, fix, local time and UTC epoch.
///
/// # Parameters
/// - `sentence`: Full NMEA RMC sentence.
/// - `gps`: Mutable snapshot updated in place on successful parse.
///
/// # Returns
/// - `Some(())` when required RMC fields parse successfully.
/// - `None` when required fields are missing or malformed.
pub fn parse_rmc(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
    if !nmea_checksum_valid(sentence) {
        log::trace!("GPS RMC rejected: checksum invalid");
        return None;
    }
    log::trace!("GPS RMC raw: {}", sentence);
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.len() < 10 {
        return None;
    }

    let time = parse_hhmmss(fields[1])?;
    let status = fields[2];
    let date = parse_ddmmyy(fields[9])?;
    let lat = nmea_to_decimal(fields[3], fields[4])?;
    let lon = nmea_to_decimal(fields[5], fields[6])?;

    let (local_date, local_time, tz_offset_hours) = local_datetime_from_utc(date, time, lon)
        .unwrap_or_else(|| (format_ddmmyy(date), format_hhmmss(time), 0));
    gps.local_date = local_date;
    gps.local_time = local_time;
    gps.tz_offset_hours = tz_offset_hours;
    gps.utc_unix_seconds = utc_datetime_from_fields(date, time).map(|dt| dt.and_utc().timestamp());
    gps.lat = lat;
    gps.lon = lon;
    gps.fix = status == "A";
    log::trace!(
        "GPS RMC parsed: local={} {} tz={:+}h fix={} lat={:.6} lon={:.6}",
        gps.local_date,
        gps.local_time,
        gps.tz_offset_hours,
        gps.fix,
        gps.lat,
        gps.lon
    );

    Some(())
}

/// Parse a GGA sentence and update satellite count.
///
/// # Parameters
/// - `sentence`: Full NMEA GGA sentence.
/// - `gps`: Mutable snapshot updated in place on successful parse.
///
/// # Returns
/// - `Some(())` when satellite count parses successfully.
/// - `None` when required GGA fields are missing or malformed.
pub fn parse_gga(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
    if !nmea_checksum_valid(sentence) {
        log::trace!("GPS GGA rejected: checksum invalid");
        return None;
    }
    log::trace!("GPS GGA raw: {}", sentence);
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.len() < 8 {
        return None;
    }
    gps.sats = fields[7].parse::<u8>().ok()?;
    if fields.len() >= 10 {
        gps.altitude_m = fields[9].parse::<f32>().ok();
    }
    log::trace!("GPS GGA parsed: sats={} alt={:?}", gps.sats, gps.altitude_m);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn reset_runtime_timezone() {
        let lock = RUNTIME_TZ.get_or_init(|| RwLock::new(None));
        if let Ok(mut guard) = lock.write() {
            *guard = None;
        }
    }

    #[test]
    fn nmea_checksum_rejects_missing_dollar_prefix() {
        assert!(!nmea_checksum_valid("GPRMC,123519,A*00"));
    }

    #[test]
    fn nmea_checksum_rejects_truncated_checksum_field() {
        assert!(!nmea_checksum_valid("$GPRMC,123519,A*0"));
    }

    #[test]
    #[serial]
    fn parse_rmc_falls_back_to_raw_date_time_on_invalid_calendar() {
        reset_runtime_timezone();
        let mut gps = GpsSnapshot::default();
        let rmc = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,320266,003.1,W*66";

        assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
        assert_eq!(gps.local_date, "2066-02-32");
        assert_eq!(gps.local_time, "12:35:19");
        assert_eq!(gps.tz_offset_hours, 0);
        assert!(gps.utc_unix_seconds.is_none());
    }

    #[test]
    fn parse_gga_rejects_malformed_satellite_field() {
        let mut gps = GpsSnapshot::default();
        let gga = "$GPGGA,123520,4807.038,N,01131.000,E,1,08*7D";
        assert_eq!(parse_gga(gga, &mut gps), None);
    }

    #[test]
    fn nmea_checksum_rejects_missing_star() {
        assert!(!nmea_checksum_valid("$GPRMC,123519,A"));
    }

    #[test]
    fn nmea_checksum_rejects_non_hex_suffix() {
        assert!(!nmea_checksum_valid("$GPRMC,123519,A*GH"));
    }

    #[test]
    fn set_runtime_timezone_rejects_invalid_name() {
        assert!(!set_runtime_timezone("Not/A/Timezone"));
    }

    #[test]
    #[serial]
    fn parse_rmc_rejects_short_sentence() {
        reset_runtime_timezone();
        let mut gps = GpsSnapshot::default();
        let rmc = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4*32";
        assert_eq!(parse_rmc(rmc, &mut gps), None);
    }

    #[test]
    fn parse_gga_rejects_invalid_checksum() {
        let mut gps = GpsSnapshot::default();
        let gga = "$GPGGA,123520,4807.038,N,01131.000,E,1,08,1.0,545.4,M,46.9,M,,*00";
        assert_eq!(parse_gga(gga, &mut gps), None);
    }

    #[test]
    fn nmea_checksum_valid_accepts_known_sentence() {
        let rmc = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
        assert!(nmea_checksum_valid(rmc));
    }

    #[test]
    fn nmea_checksum_invalid_rejects_parse() {
        let mut gps = GpsSnapshot::default();
        let bad = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*00";
        assert!(!nmea_checksum_valid(bad));
        assert_eq!(parse_rmc(bad, &mut gps), None);
    }

    #[test]
    #[serial]
    fn parse_rmc_populates_local_fields_and_coords() {
        reset_runtime_timezone();
        let mut gps = GpsSnapshot::default();
        let rmc = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";

        assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
        assert_eq!(gps.local_date, "2094-03-23");
        assert_eq!(gps.local_time, "13:35:19");
        assert_eq!(gps.tz_offset_hours, 1);
        assert!(gps.fix);
        assert!((gps.lat - 48.1173).abs() < 0.0001);
        assert!((gps.lon - 11.516667).abs() < 0.0001);
    }

    #[test]
    #[serial]
    fn parse_rmc_marks_invalid_fix_status() {
        reset_runtime_timezone();
        let mut gps = GpsSnapshot::default();
        let rmc = "$GPRMC,225446,V,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E*7F";

        assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
        assert!(!gps.fix);
        assert!(gps.lon < 0.0);
    }

    #[test]
    fn parse_gga_updates_satellite_count() {
        let mut gps = GpsSnapshot::default();
        let gga = "$GPGGA,123520,4807.038,N,01131.000,E,1,08,1.0,545.4,M,46.9,M,,*45";

        assert_eq!(parse_gga(gga, &mut gps), Some(()));
        assert_eq!(gps.sats, 8);
        assert_eq!(gps.altitude_m, Some(545.4));
    }

    #[test]
    #[serial]
    fn runtime_timezone_override_applies_dst_rules_for_summer() {
        assert!(set_runtime_timezone("America/Chicago"));
        let mut gps = GpsSnapshot::default();
        // 2026-06-01 12:00:00 UTC should be 07:00:00 CDT (UTC-5).
        let rmc = "$GPRMC,120000,A,3853.647,N,09011.516,W,0.0,0.0,010626,0.0,E*67";

        assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
        assert_eq!(gps.local_date, "2026-06-01");
        assert_eq!(gps.local_time, "07:00:00");
        assert_eq!(gps.tz_offset_hours, -5);
    }

    #[test]
    #[serial]
    fn local_from_utc_unix_uses_runtime_timezone() {
        assert!(set_runtime_timezone("America/Chicago"));
        // 2026-06-01 18:00:00 UTC -> 13:00:00 CDT.
        let (date, time) = local_from_utc_unix(1_780_336_800).expect("local time");
        assert_eq!(date, "2026-06-01");
        assert_eq!(time, "13:00:00");
        assert_eq!(tz_offset_hours_at_unix(1_780_336_800), -5);
    }

    #[test]
    fn runtime_timezone_override_applies_dst_rules_for_winter() {
        assert!(set_runtime_timezone("America/Chicago"));
        let mut gps = GpsSnapshot::default();
        // 2026-01-01 12:00:00 UTC should be 06:00:00 CST (UTC-6).
        let rmc = "$GPRMC,120000,A,3853.647,N,09011.516,W,0.0,0.0,010126,0.0,E*60";

        assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
        assert_eq!(gps.local_date, "2026-01-01");
        assert_eq!(gps.local_time, "06:00:00");
        assert_eq!(gps.tz_offset_hours, -6);
    }
}
