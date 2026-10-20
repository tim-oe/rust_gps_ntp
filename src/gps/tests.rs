use super::*;
use serial_test::serial;

fn reset_runtime_timezone() {
    if let Ok(mut guard) = RUNTIME_TZ.write() {
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
fn nmea_checksum_accepts_known_good_sentence() {
    assert!(nmea_checksum_valid(
        "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A"
    ));
}

/// UART noise can yield multibyte UTF-8 inside the sentence.  The validator
/// must reject it rather than panic on a non-char-boundary slice (which it
/// did when `*` was followed by a multibyte character).
#[test]
fn nmea_checksum_rejects_multibyte_without_panicking() {
    // Multibyte right after '*': the old `[star+1..star+3]` slice split 'é'.
    assert!(!nmea_checksum_valid("$GPRMC,123519,A*é"));
    assert!(!nmea_checksum_valid("$GPRMC,123519,A*6é"));
    // Multibyte in the body, checksum field otherwise well-formed.
    assert!(!nmea_checksum_valid("$GPRMC,12351é,A*6A"));
    // Non-ASCII '$'-like prefix and an emoji body.
    assert!(!nmea_checksum_valid("$GP\u{1F6F0},123519,A*6A"));
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

/// A receiver that loses its fix emits status `V` with empty lat/lon fields.
/// The parser must apply the status so `fix` is cleared (it gates PPS
/// discipline and stratum 1) instead of bailing on the missing position.
#[test]
#[serial]
fn parse_rmc_clears_fix_when_status_void_and_position_empty() {
    reset_runtime_timezone();
    let mut gps = GpsSnapshot::default();
    let good = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
    assert_eq!(parse_rmc(good, &mut gps), Some(()));
    assert!(gps.fix);
    let lat = gps.lat;
    let lon = gps.lon;

    // Status V, empty position, otherwise valid (checksum 0x4B).
    let no_fix = "$GPRMC,235947.000,V,,,,,0.00,0.00,010180,,,N*4B";
    assert_eq!(
        parse_rmc(no_fix, &mut gps),
        Some(()),
        "status-only RMC must still be accepted"
    );
    assert!(!gps.fix, "fix must be cleared by a V status");
    assert_eq!(gps.lat, lat, "last known position is retained");
    assert_eq!(gps.lon, lon, "last known position is retained");
    // Time still updates without a fix; the retained longitude (11.5°E) gives
    // a UTC+1 longitude estimate, so 23:59:47 UTC renders as 00:59:47 local.
    assert_eq!(gps.tz_offset_hours, 1);
    assert_eq!(gps.local_time, "00:59:47");
    assert!(gps.utc_unix_seconds.is_some());
}

#[test]
#[serial]
fn parse_rmc_void_status_with_position_clears_fix() {
    reset_runtime_timezone();
    let mut gps = GpsSnapshot {
        fix: true,
        ..GpsSnapshot::default()
    };
    // Same as the known-good sentence with status A → V (checksum 6A ^ 'A' ^ 'V' = 0x7D).
    let rmc = "$GPRMC,123519,V,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*7D";
    assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
    assert!(!gps.fix);
    assert!((gps.lat - 48.1173).abs() < 1e-4);
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
#[serial]
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
