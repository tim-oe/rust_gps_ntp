//! PCF8523 real-time clock on the Adafruit Adalogger FeatherWing.
//!
//! The RTC shares the Feather I2C bus with the battery fuel gauge and provides
//! battery-backed time that survives reboots and brief GPS outages.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc};

#[cfg(target_os = "espidf")]
use crate::i2c_bus::{FeatherI2cBus, I2cDevice};

/// PCF8523 7-bit I2C address on the Adalogger FeatherWing.
pub const PCF8523_ADDR: u8 = 0x68;

/// Minimum interval between PCF8523 writebacks from disciplined GPS time.
pub const WRITEBACK_INTERVAL_US: i64 = 60_000_000;
/// How often to feed cached RTC UTC into NTP when GPS fix is lost.
pub const FALLBACK_INTERVAL_US: i64 = 1_000_000;

/// Decoded calendar fields from PCF8523 time registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Reject RTC timestamps before this year (factory default / unset coin cell).
const MIN_VALID_YEAR: i32 = 2020;

/// Cached RTC sample published by the UI task for GPS-loss fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct RtcSnapshot {
    pub detected: bool,
    pub utc_unix_seconds: Option<i64>,
}

/// Convert a BCD byte from the PCF8523 into decimal.
pub fn bcd_to_dec(byte: u8) -> u8 {
    (byte & 0x0F) + ((byte >> 4) & 0x0F) * 10
}

/// Encode a decimal value (0–99) into BCD for PCF8523 registers.
pub fn dec_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Decode seven PCF8523 time registers starting at `0x03` (seconds).
pub fn decode_datetime_regs(regs: [u8; 7]) -> Option<RtcDateTime> {
    if regs[0] & 0x80 != 0 {
        return None;
    }

    let second = bcd_to_dec(regs[0] & 0x7F);
    let minute = bcd_to_dec(regs[1] & 0x7F);
    let hour = bcd_to_dec(regs[2] & 0x3F);
    let day = bcd_to_dec(regs[3] & 0x3F);
    let month = bcd_to_dec(regs[4] & 0x1F);
    let year = 2000 + bcd_to_dec(regs[6]) as i32;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
        || year < MIN_VALID_YEAR
    {
        return None;
    }

    Some(RtcDateTime {
        year,
        month: month as u32,
        day: day as u32,
        hour: hour as u32,
        minute: minute as u32,
        second: second as u32,
    })
}

/// Encode a UTC instant into seven PCF8523 time registers.
pub fn encode_datetime_regs(dt: RtcDateTime) -> [u8; 7] {
    let year_two_digit = (dt.year - 2000).clamp(0, 99) as u8;
    [
        dec_to_bcd(dt.second as u8),
        dec_to_bcd(dt.minute as u8),
        dec_to_bcd(dt.hour as u8),
        dec_to_bcd(dt.day as u8),
        1, // weekday unused for Unix conversion; keep Monday as placeholder
        dec_to_bcd(dt.month as u8),
        dec_to_bcd(year_two_digit),
    ]
}

/// Convert decoded RTC fields to Unix UTC seconds.
pub fn datetime_to_unix_seconds(dt: RtcDateTime) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(dt.year, dt.month, dt.day)?;
    let time = NaiveTime::from_hms_opt(dt.hour, dt.minute, dt.second)?;
    let naive = NaiveDateTime::new(date, time);
    Some(naive.and_utc().timestamp())
}

/// Convert Unix UTC seconds into PCF8523 register fields.
pub fn unix_seconds_to_datetime(unix_seconds: i64) -> Option<RtcDateTime> {
    let dt = Utc.timestamp_opt(unix_seconds, 0).single()?;
    Some(RtcDateTime {
        year: dt.year(),
        month: dt.month(),
        day: dt.day(),
        hour: dt.hour(),
        minute: dt.minute(),
        second: dt.second(),
    })
}

/// Format UTC Unix seconds as local date and time using a fixed hour offset.
pub fn local_date_time_from_utc(
    utc_unix_seconds: i64,
    tz_offset_hours: i8,
) -> Option<(String, String)> {
    let dt = Utc.timestamp_opt(utc_unix_seconds, 0).single()?;
    let local = dt + Duration::hours(i64::from(tz_offset_hours));
    Some((
        format!(
            "{:04}-{:02}-{:02}",
            local.year(),
            local.month(),
            local.day()
        ),
        format!(
            "{:02}:{:02}:{:02}",
            local.hour(),
            local.minute(),
            local.second()
        ),
    ))
}

#[cfg(target_os = "espidf")]
const REG_CONTROL1: u8 = 0x00;
#[cfg(target_os = "espidf")]
const REG_CONTROL2: u8 = 0x01;
#[cfg(target_os = "espidf")]
const REG_CONTROL3: u8 = 0x02;
#[cfg(target_os = "espidf")]
const REG_SECONDS: u8 = 0x03;
#[cfg(target_os = "espidf")]
const STOP_BIT: u8 = 0x20;

/// Result of probing the PCF8523 on I2C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcProbe {
    /// Chip responded on I2C.
    Present,
    /// No device at `0x68`.
    Absent,
}

#[cfg(target_os = "espidf")]
struct Pcf8523;

#[cfg(target_os = "espidf")]
impl I2cDevice for Pcf8523 {
    const ADDR: u8 = PCF8523_ADDR;
}

/// Boot-time RTC probe result on the shared Feather I2C bus.
#[cfg(target_os = "espidf")]
#[derive(Debug, Clone, Copy)]
pub struct RtcDevice {
    /// Whether a PCF8523 responded on I2C.
    pub present: bool,
    /// Cached UTC seconds read at boot, when valid.
    pub boot_utc: Option<i64>,
}

#[cfg(target_os = "espidf")]
impl RtcDevice {
    /// Probe, initialize, and read the Adalogger PCF8523 RTC.
    pub fn init(bus: &mut FeatherI2cBus) -> Self {
        let present = detect(bus);
        let boot_utc = if present {
            if let Err(err) = init_chip(bus) {
                log::warn!("RTC: PCF8523 init failed: {err}");
            }
            match read_unix_seconds(bus) {
                Ok(secs) => {
                    log::info!("RTC: PCF8523 @ 0x{PCF8523_ADDR:02X} reads UTC {secs}");
                    Some(secs)
                }
                Err(err) => {
                    log::warn!(
                        "RTC: PCF8523 present @ 0x{PCF8523_ADDR:02X} but time unset/invalid ({err}); waiting for GPS"
                    );
                    None
                }
            }
        } else {
            log::warn!(
                "RTC: no PCF8523 response @ 0x{PCF8523_ADDR:02X} — check Adalogger stack and CR1220"
            );
            None
        };
        Self { present, boot_utc }
    }
}

/// Check whether a PCF8523 responds on I2C (does not require valid time).
#[cfg(target_os = "espidf")]
pub fn probe(bus: &mut FeatherI2cBus) -> RtcProbe {
    let mut control = [0_u8];
    match bus.write_read::<Pcf8523>(&[REG_CONTROL1], &mut control) {
        Ok(()) => RtcProbe::Present,
        Err(err) => {
            log::debug!("RTC: I2C probe @ 0x{PCF8523_ADDR:02X} failed: {err}");
            RtcProbe::Absent
        }
    }
}

/// Clear stop/run flags and set battery switchover defaults (matches RTClib `begin()`).
#[cfg(target_os = "espidf")]
pub fn init_chip(bus: &mut FeatherI2cBus) -> anyhow::Result<()> {
    bus.write::<Pcf8523>(&[REG_CONTROL1, 0x00])?;
    bus.write::<Pcf8523>(&[REG_CONTROL2, 0x00])?;
    bus.write::<Pcf8523>(&[REG_CONTROL3, 0x00])?;
    Ok(())
}

/// Probe the shared I2C bus for a PCF8523 RTC.
#[cfg(target_os = "espidf")]
pub fn detect(bus: &mut FeatherI2cBus) -> bool {
    matches!(probe(bus), RtcProbe::Present)
}

/// Read the current RTC time as Unix UTC seconds.
#[cfg(target_os = "espidf")]
pub fn read_unix_seconds(bus: &mut FeatherI2cBus) -> anyhow::Result<i64> {
    let dt = read_datetime(bus)?;
    datetime_to_unix_seconds(dt)
        .ok_or_else(|| anyhow::anyhow!("PCF8523 datetime invalid after decode"))
}

/// Read and decode the PCF8523 calendar registers.
#[cfg(target_os = "espidf")]
pub fn read_datetime(bus: &mut FeatherI2cBus) -> anyhow::Result<RtcDateTime> {
    let mut regs = [0_u8; 7];
    bus.write_read::<Pcf8523>(&[REG_SECONDS], &mut regs)?;

    decode_datetime_regs(regs)
        .ok_or_else(|| anyhow::anyhow!("PCF8523 returned invalid or unset time"))
}

/// Write UTC time to the PCF8523 (clock is briefly stopped during the write).
#[cfg(target_os = "espidf")]
pub fn write_unix_seconds(bus: &mut FeatherI2cBus, unix_seconds: i64) -> anyhow::Result<()> {
    let dt = unix_seconds_to_datetime(unix_seconds)
        .ok_or_else(|| anyhow::anyhow!("UTC {unix_seconds} out of PCF8523 range"))?;
    write_datetime(bus, dt)
}

/// Write calendar fields to the PCF8523.
#[cfg(target_os = "espidf")]
pub fn write_datetime(bus: &mut FeatherI2cBus, dt: RtcDateTime) -> anyhow::Result<()> {
    let mut control = [0_u8];
    bus.write_read::<Pcf8523>(&[REG_CONTROL1], &mut control)?;

    let stop = control[0] | STOP_BIT;
    bus.write::<Pcf8523>(&[REG_CONTROL1, stop])?;

    let payload = encode_datetime_regs(dt);
    let mut buf = [0_u8; 8];
    buf[0] = REG_SECONDS;
    buf[1..].copy_from_slice(&payload);
    let write_result = bus.write::<Pcf8523>(&buf);

    let run = control[0] & !STOP_BIT;
    if let Err(err) = bus.write::<Pcf8523>(&[REG_CONTROL1, run]) {
        log::warn!("PCF8523 restart clock failed: {err}");
    }

    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcd_round_trip() {
        assert_eq!(bcd_to_dec(0x59), 59);
        assert_eq!(dec_to_bcd(59), 0x59);
    }

    #[test]
    fn decode_valid_rtc_regs() {
        // 2024-06-07 14:30:45 — OS stopped bit clear
        let regs = [0x45, 0x30, 0x14, 0x07, 0x06, 0x05, 0x24];
        let dt = decode_datetime_regs(regs).expect("valid regs");
        assert_eq!(dt.year, 2024);
        assert_eq!(dt.month, 6);
        assert_eq!(dt.day, 7);
        assert_eq!(dt.hour, 14);
        assert_eq!(dt.minute, 30);
        assert_eq!(dt.second, 45);
    }

    #[test]
    fn decode_rejects_os_stop_bit() {
        let regs = [0xC5, 0x30, 0x14, 0x07, 0x06, 0x05, 0x24];
        assert!(decode_datetime_regs(regs).is_none());
    }

    #[test]
    fn decode_rejects_pre_2020() {
        let regs = [0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x19];
        assert!(decode_datetime_regs(regs).is_none());
    }

    #[test]
    fn unix_round_trip() {
        let dt = RtcDateTime {
            year: 2024,
            month: 6,
            day: 7,
            hour: 14,
            minute: 30,
            second: 45,
        };
        let unix = datetime_to_unix_seconds(dt).expect("valid datetime");
        let back = unix_seconds_to_datetime(unix).expect("valid unix");
        assert_eq!(back, dt);
    }

    #[test]
    fn local_date_time_applies_tz_offset() {
        // 2024-06-07 14:30:45 UTC -> 09:30:45 at UTC-5
        let unix = datetime_to_unix_seconds(RtcDateTime {
            year: 2024,
            month: 6,
            day: 7,
            hour: 14,
            minute: 30,
            second: 45,
        })
        .expect("valid datetime");
        let (date, time) = local_date_time_from_utc(unix, -5).expect("valid offset");
        assert_eq!(date, "2024-06-07");
        assert_eq!(time, "09:30:45");
    }
}
