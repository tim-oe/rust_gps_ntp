//! PCF8523 real-time clock on the Adafruit Adalogger FeatherWing.
//!
//! The RTC shares the Feather I2C bus with the battery fuel gauge and provides
//! battery-backed time that survives reboots and brief GPS outages.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc};

#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::i2c;

/// PCF8523 7-bit I2C address on the Adalogger FeatherWing.
pub const PCF8523_ADDR: u8 = 0x68;

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

/// Check whether a PCF8523 responds on I2C (does not require valid time).
#[cfg(target_os = "espidf")]
pub fn probe(i2c: &mut i2c::I2cDriver<'_>) -> RtcProbe {
    let mut control = [0_u8];
    match i2c.write_read(PCF8523_ADDR, &[REG_CONTROL1], &mut control, 50) {
        Ok(()) => RtcProbe::Present,
        Err(err) => {
            log::debug!("RTC: I2C probe @ 0x{PCF8523_ADDR:02X} failed: {err}");
            RtcProbe::Absent
        }
    }
}

/// Clear stop/run flags and set battery switchover defaults (matches RTClib `begin()`).
#[cfg(target_os = "espidf")]
pub fn init(i2c: &mut i2c::I2cDriver<'_>) -> anyhow::Result<()> {
    i2c.write(PCF8523_ADDR, &[REG_CONTROL1, 0x00], 50)
        .map_err(|e| anyhow::anyhow!("PCF8523 init CONTROL1 failed: {e}"))?;
    i2c.write(PCF8523_ADDR, &[REG_CONTROL2, 0x00], 50)
        .map_err(|e| anyhow::anyhow!("PCF8523 init CONTROL2 failed: {e}"))?;
    i2c.write(PCF8523_ADDR, &[REG_CONTROL3, 0x00], 50)
        .map_err(|e| anyhow::anyhow!("PCF8523 init CONTROL3 failed: {e}"))?;
    Ok(())
}

/// Probe the shared I2C bus for a PCF8523 RTC.
#[cfg(target_os = "espidf")]
pub fn detect(i2c: &mut i2c::I2cDriver<'_>) -> bool {
    matches!(probe(i2c), RtcProbe::Present)
}

/// Read the current RTC time as Unix UTC seconds.
#[cfg(target_os = "espidf")]
pub fn read_unix_seconds(i2c: &mut i2c::I2cDriver<'_>) -> anyhow::Result<i64> {
    let dt = read_datetime(i2c)?;
    datetime_to_unix_seconds(dt)
        .ok_or_else(|| anyhow::anyhow!("PCF8523 datetime invalid after decode"))
}

/// Read and decode the PCF8523 calendar registers.
#[cfg(target_os = "espidf")]
pub fn read_datetime(i2c: &mut i2c::I2cDriver<'_>) -> anyhow::Result<RtcDateTime> {
    let mut regs = [0_u8; 7];
    i2c.write_read(PCF8523_ADDR, &[REG_SECONDS], &mut regs, 50)
        .map_err(|e| anyhow::anyhow!("PCF8523 read time failed: {e}"))?;

    decode_datetime_regs(regs)
        .ok_or_else(|| anyhow::anyhow!("PCF8523 returned invalid or unset time"))
}

/// Write UTC time to the PCF8523 (clock is briefly stopped during the write).
#[cfg(target_os = "espidf")]
pub fn write_unix_seconds(i2c: &mut i2c::I2cDriver<'_>, unix_seconds: i64) -> anyhow::Result<()> {
    let dt = unix_seconds_to_datetime(unix_seconds)
        .ok_or_else(|| anyhow::anyhow!("UTC {unix_seconds} out of PCF8523 range"))?;
    write_datetime(i2c, dt)
}

/// Write calendar fields to the PCF8523.
#[cfg(target_os = "espidf")]
pub fn write_datetime(i2c: &mut i2c::I2cDriver<'_>, dt: RtcDateTime) -> anyhow::Result<()> {
    let mut control = [0_u8];
    i2c.write_read(PCF8523_ADDR, &[REG_CONTROL1], &mut control, 50)
        .map_err(|e| anyhow::anyhow!("PCF8523 read CONTROL1 failed: {e}"))?;

    let stop = control[0] | STOP_BIT;
    i2c.write(PCF8523_ADDR, &[REG_CONTROL1, stop], 50)
        .map_err(|e| anyhow::anyhow!("PCF8523 stop clock failed: {e}"))?;

    let payload = encode_datetime_regs(dt);
    let mut buf = [0_u8; 8];
    buf[0] = REG_SECONDS;
    buf[1..].copy_from_slice(&payload);
    let write_result = i2c.write(PCF8523_ADDR, &buf, 50);

    let run = control[0] & !STOP_BIT;
    if let Err(err) = i2c.write(PCF8523_ADDR, &[REG_CONTROL1, run], 50) {
        log::warn!("PCF8523 restart clock failed: {err}");
    }

    write_result.map_err(|e| anyhow::anyhow!("PCF8523 write time failed: {e}"))
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
}
