use chrono::{Duration as ChronoDuration, NaiveDate, NaiveDateTime, NaiveTime};

#[derive(Debug, Clone, Default)]
pub struct GpsSnapshot {
    pub local_date: String,
    pub local_time: String,
    pub tz_offset_hours: i8,
    pub lat: f32,
    pub lon: f32,
    pub fix: bool,
    pub sats: u8,
}

fn parse_hhmmss(raw: &str) -> Option<&str> {
    if raw.len() < 6 || !raw.is_ascii() {
        return None;
    }
    Some(&raw[..6])
}

fn format_hhmmss(raw6: &str) -> String {
    format!("{}:{}:{}", &raw6[0..2], &raw6[2..4], &raw6[4..6])
}

fn parse_ddmmyy(raw: &str) -> Option<&str> {
    if raw.len() < 6 || !raw.is_ascii() {
        return None;
    }
    Some(&raw[..6])
}

fn format_ddmmyy(raw6: &str) -> String {
    format!("20{}-{}-{}", &raw6[4..6], &raw6[2..4], &raw6[0..2])
}

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

fn local_datetime_from_utc(
    utc_date: &str,
    utc_time: &str,
    lon: f32,
) -> Option<(String, String, i8)> {
    let tz_offset_h = (lon / 15.0).round() as i8;
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
    let dt = NaiveDateTime::new(date, time) + ChronoDuration::hours(tz_offset_h as i64);
    Some((
        dt.date().format("%Y-%m-%d").to_string(),
        dt.time().format("%H:%M:%S").to_string(),
        tz_offset_h,
    ))
}

pub fn parse_rmc(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
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

pub fn parse_gga(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
    log::trace!("GPS GGA raw: {}", sentence);
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.len() < 8 {
        return None;
    }
    gps.sats = fields[7].parse::<u8>().ok()?;
    log::trace!("GPS GGA parsed: sats={}", gps.sats);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rmc_populates_local_fields_and_coords() {
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
    fn parse_rmc_marks_invalid_fix_status() {
        let mut gps = GpsSnapshot::default();
        let rmc = "$GPRMC,225446,V,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E*68";

        assert_eq!(parse_rmc(rmc, &mut gps), Some(()));
        assert!(!gps.fix);
        assert!(gps.lon < 0.0);
    }

    #[test]
    fn parse_gga_updates_satellite_count() {
        let mut gps = GpsSnapshot::default();
        let gga = "$GPGGA,123520,4807.038,N,01131.000,E,1,08,1.0,545.4,M,46.9,M,,*47";

        assert_eq!(parse_gga(gga, &mut gps), Some(()));
        assert_eq!(gps.sats, 8);
    }
}
