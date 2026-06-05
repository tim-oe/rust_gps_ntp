use chrono::{Duration as ChronoDuration, NaiveDate, NaiveDateTime, NaiveTime};

#[derive(Debug, Clone, Default)]
pub struct GpsSnapshot {
    pub utc_date: String,
    pub utc_time: String,
    pub local_time: String,
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

fn local_time_from_utc(utc_date: &str, utc_time: &str, lon: f32) -> Option<String> {
    let tz_offset_h = (lon / 15.0).round() as i64;
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
    let dt = NaiveDateTime::new(date, time) + ChronoDuration::hours(tz_offset_h);

    Some(format!(
        "{} ({:+}h)",
        dt.time().format("%H:%M:%S"),
        tz_offset_h
    ))
}

pub fn parse_rmc(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.len() < 10 {
        return None;
    }

    let time = parse_hhmmss(fields[1])?;
    let status = fields[2];
    let date = parse_ddmmyy(fields[9])?;
    let lat = nmea_to_decimal(fields[3], fields[4])?;
    let lon = nmea_to_decimal(fields[5], fields[6])?;

    gps.utc_date = format_ddmmyy(date);
    gps.utc_time = format_hhmmss(time);
    gps.local_time = local_time_from_utc(date, time, lon).unwrap_or_else(|| "n/a".to_owned());
    gps.lat = lat;
    gps.lon = lon;
    gps.fix = status == "A";

    Some(())
}

pub fn parse_gga(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.len() < 8 {
        return None;
    }
    gps.sats = fields[7].parse::<u8>().ok()?;
    Some(())
}
