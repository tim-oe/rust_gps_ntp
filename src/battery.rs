//! Battery monitor detection and sampling helpers.
//!
//! This module supports the two fuel gauges used in project hardware variants:
//! MAX17048 and LC709203.

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

/// Read battery telemetry from a MAX17048 fuel gauge.
///
/// # Parameters
/// - `i2c`: I2C driver used to read MAX17048 registers.
///
/// # Returns
/// - `Ok(BatterySnapshot)` when both voltage and SOC reads succeed.
/// - `Err` when an I2C transaction fails.
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

    let vraw = u16::from_be_bytes(vcell);
    let voltage_v = (vraw as f32) * 78.125e-6;
    let percent = (soc[0] as f32) + ((soc[1] as f32) / 256.0);

    Ok(BatterySnapshot { voltage_v, percent })
}

/// Read battery telemetry from an LC709203 fuel gauge.
///
/// # Parameters
/// - `i2c`: I2C driver used to read LC709203 registers.
///
/// # Returns
/// - `Ok(BatterySnapshot)` when both voltage and RSOC reads succeed.
/// - `Err` when an I2C transaction fails.
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

    // LC709203 uses little-endian 16-bit register values.
    let voltage_mv = u16::from_le_bytes(vcell) as f32;
    let percent = u16::from_le_bytes(rsoc) as f32;

    Ok(BatterySnapshot {
        voltage_v: voltage_mv / 1000.0,
        percent,
    })
}

/// Probe known battery monitor addresses and return the detected chip.
///
/// # Parameters
/// - `i2c`: I2C driver used for probe reads.
///
/// # Returns
/// - `Some(BatteryMonitor)` for the first recognized monitor.
/// - `None` when no supported monitor responds.
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
pub fn read_battery(
    i2c: &mut i2c::I2cDriver<'_>,
    monitor: BatteryMonitor,
) -> anyhow::Result<BatterySnapshot> {
    match monitor {
        BatteryMonitor::Max17048 => read_max17048(i2c),
        BatteryMonitor::Lc709203 => read_lc709203(i2c),
    }
}
