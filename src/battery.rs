//! Battery monitor detection and sampling helpers.
//!
//! This module supports the two fuel gauges used in project hardware variants:
//! MAX17048 and LC709203.

#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::i2c;

/// Last sampled battery telemetry values.
#[derive(Debug, Clone, Default)]
pub struct BatterySnapshot {
    /// Battery voltage in volts.
    pub voltage_v: f32,
    /// State-of-charge percentage in the range reported by the gauge.
    pub percent: f32,
}

/// Supported on-board battery monitor chips.
#[derive(Debug, Clone, Copy)]
pub enum BatteryMonitor {
    /// Maxim MAX17048 gauge at I2C address `0x36`.
    Max17048,
    /// ON Semiconductor LC709203 gauge at I2C address `0x0B`.
    Lc709203,
}

/// Decode MAX17048 VCELL register bytes into volts.
///
/// # Parameters
/// - `vcell`: Big-endian two-byte VCELL register value.
///
/// # Returns
/// - Cell voltage in volts.
pub fn decode_max17048_vcell(vcell: [u8; 2]) -> f32 {
    let vraw = u16::from_be_bytes(vcell);
    (vraw as f32) * 78.125e-6
}

/// Decode MAX17048 SOC register bytes into percent.
///
/// # Parameters
/// - `soc`: Two-byte SOC register value from the gauge.
///
/// # Returns
/// - State-of-charge percentage.
pub fn decode_max17048_soc(soc: [u8; 2]) -> f32 {
    (soc[0] as f32) + ((soc[1] as f32) / 256.0)
}

/// Decode LC709203 cell voltage register bytes into millivolts.
///
/// # Parameters
/// - `vcell`: Little-endian two-byte VCELL register value.
///
/// # Returns
/// - Cell voltage in millivolts.
pub fn decode_lc709203_voltage_mv(vcell: [u8; 2]) -> f32 {
    u16::from_le_bytes(vcell) as f32
}

/// Decode LC709203 RSOC register bytes into percent.
///
/// # Parameters
/// - `rsoc`: Little-endian two-byte RSOC register value.
///
/// # Returns
/// - Reported state-of-charge percentage.
pub fn decode_lc709203_percent(rsoc: [u8; 2]) -> f32 {
    u16::from_le_bytes(rsoc) as f32
}

/// Read battery telemetry from a MAX17048 fuel gauge.
#[cfg(target_os = "espidf")]
fn read_max17048(i2c: &mut i2c::I2cDriver<'_>) -> anyhow::Result<BatterySnapshot> {
    const MAX17048_ADDR: u8 = 0x36;
    const REG_VCELL: u8 = 0x02;
    const REG_SOC: u8 = 0x04;
    let mut vcell = [0_u8; 2];
    let mut soc = [0_u8; 2];
    i2c.write_read(MAX17048_ADDR, &[REG_VCELL], &mut vcell, 50)
        .map_err(|e| anyhow::anyhow!("MAX17048 read VCELL failed: {e}"))?;
    i2c.write_read(MAX17048_ADDR, &[REG_SOC], &mut soc, 50)
        .map_err(|e| anyhow::anyhow!("MAX17048 read SOC failed: {e}"))?;

    let voltage_v = decode_max17048_vcell(vcell);
    let percent = decode_max17048_soc(soc);

    Ok(BatterySnapshot { voltage_v, percent })
}

/// Read battery telemetry from an LC709203 fuel gauge.
#[cfg(target_os = "espidf")]
fn read_lc709203(i2c: &mut i2c::I2cDriver<'_>) -> anyhow::Result<BatterySnapshot> {
    const LC709203_ADDR: u8 = 0x0B;
    const REG_VCELL_MV: u8 = 0x09;
    const REG_RSOC: u8 = 0x0D;

    let mut vcell = [0_u8; 2];
    let mut rsoc = [0_u8; 2];
    i2c.write_read(LC709203_ADDR, &[REG_VCELL_MV], &mut vcell, 50)
        .map_err(|e| anyhow::anyhow!("LC709203 read VCELL failed: {e}"))?;
    i2c.write_read(LC709203_ADDR, &[REG_RSOC], &mut rsoc, 50)
        .map_err(|e| anyhow::anyhow!("LC709203 read RSOC failed: {e}"))?;

    Ok(BatterySnapshot {
        voltage_v: decode_lc709203_voltage_mv(vcell) / 1000.0,
        percent: decode_lc709203_percent(rsoc),
    })
}

/// Probe known battery monitor addresses and return the detected chip.
///
/// # Parameters
/// - `i2c`: I2C driver used for probe reads.
///
/// # Returns
/// - `Some(BatteryMonitor)` for the first recognized gauge.
/// - `None` when no supported monitor responds.
#[cfg(target_os = "espidf")]
pub fn detect_monitor(i2c: &mut i2c::I2cDriver<'_>) -> Option<BatteryMonitor> {
    if read_max17048(i2c).is_ok() {
        return Some(BatteryMonitor::Max17048);
    }
    if read_lc709203(i2c).is_ok() {
        return Some(BatteryMonitor::Lc709203);
    }
    None
}

/// Read battery telemetry using the previously detected monitor type.
///
/// # Parameters
/// - `i2c`: I2C driver used for register access.
/// - `monitor`: Detected monitor type to query.
///
/// # Returns
/// - `Ok(BatterySnapshot)` when a read succeeds for the selected monitor.
/// - `Err` when the underlying monitor read fails.
#[cfg(target_os = "espidf")]
pub fn read_battery(
    i2c: &mut i2c::I2cDriver<'_>,
    monitor: BatteryMonitor,
) -> anyhow::Result<BatterySnapshot> {
    match monitor {
        BatteryMonitor::Max17048 => read_max17048(i2c),
        BatteryMonitor::Lc709203 => read_lc709203(i2c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_max17048_vcell_known_value() {
        // 0x9800 -> 38912 * 78.125uV = 3.04V (typical register encoding)
        let volts = decode_max17048_vcell([0x98, 0x00]);
        assert!((volts - 3.04).abs() < 0.001);
    }

    #[test]
    fn decode_max17048_soc_whole_and_fraction() {
        assert!((decode_max17048_soc([75, 128]) - 75.5).abs() < 0.01);
    }

    #[test]
    fn decode_lc709203_registers() {
        assert_eq!(decode_lc709203_voltage_mv([0x10, 0x0E]), 3600.0);
        assert_eq!(decode_lc709203_percent([100, 0]), 100.0);
    }
}
