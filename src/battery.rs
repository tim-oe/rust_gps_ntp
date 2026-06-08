//! Battery monitor detection and sampling helpers.
//!
//! This module supports the two fuel gauges used in project hardware variants:
//! MAX17048 and LC709203.

#[cfg(target_os = "espidf")]
use crate::i2c_bus::{FeatherI2cBus, I2cDevice};

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

/// Detected battery fuel gauge on the shared Feather I2C bus.
#[cfg(target_os = "espidf")]
#[derive(Debug, Clone, Copy)]
pub struct BatteryDevice {
    /// Recognized gauge type, if any.
    pub monitor: Option<BatteryMonitor>,
}

#[cfg(target_os = "espidf")]
struct Max17048;

#[cfg(target_os = "espidf")]
impl I2cDevice for Max17048 {
    const ADDR: u8 = 0x36;
}

#[cfg(target_os = "espidf")]
struct Lc709203;

#[cfg(target_os = "espidf")]
impl I2cDevice for Lc709203 {
    const ADDR: u8 = 0x0B;
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
fn read_max17048(bus: &mut FeatherI2cBus) -> anyhow::Result<BatterySnapshot> {
    const REG_VCELL: u8 = 0x02;
    const REG_SOC: u8 = 0x04;
    let mut vcell = [0_u8; 2];
    let mut soc = [0_u8; 2];
    bus.write_read::<Max17048>(&[REG_VCELL], &mut vcell)?;
    bus.write_read::<Max17048>(&[REG_SOC], &mut soc)?;

    let voltage_v = decode_max17048_vcell(vcell);
    let percent = decode_max17048_soc(soc);

    Ok(BatterySnapshot { voltage_v, percent })
}

/// Read battery telemetry from an LC709203 fuel gauge.
#[cfg(target_os = "espidf")]
fn read_lc709203(bus: &mut FeatherI2cBus) -> anyhow::Result<BatterySnapshot> {
    const REG_VCELL_MV: u8 = 0x09;
    const REG_RSOC: u8 = 0x0D;

    let mut vcell = [0_u8; 2];
    let mut rsoc = [0_u8; 2];
    bus.write_read::<Lc709203>(&[REG_VCELL_MV], &mut vcell)?;
    bus.write_read::<Lc709203>(&[REG_RSOC], &mut rsoc)?;

    Ok(BatterySnapshot {
        voltage_v: decode_lc709203_voltage_mv(vcell) / 1000.0,
        percent: decode_lc709203_percent(rsoc),
    })
}

/// Probe known battery monitor addresses and return the detected chip.
#[cfg(target_os = "espidf")]
pub fn detect_monitor(bus: &mut FeatherI2cBus) -> Option<BatteryMonitor> {
    if read_max17048(bus).is_ok() {
        return Some(BatteryMonitor::Max17048);
    }
    if read_lc709203(bus).is_ok() {
        return Some(BatteryMonitor::Lc709203);
    }
    None
}

/// Probe the shared I2C bus and log the detected fuel gauge.
#[cfg(target_os = "espidf")]
impl BatteryDevice {
    /// Detect a supported battery monitor on the Feather I2C bus.
    pub fn detect(bus: &mut FeatherI2cBus) -> Self {
        let monitor = detect_monitor(bus);
        match monitor {
            Some(BatteryMonitor::Max17048) => {
                log::info!("Battery: monitor detected MAX17048 @ 0x36");
            }
            Some(BatteryMonitor::Lc709203) => {
                log::info!("Battery: monitor detected LC709203 @ 0x0B");
            }
            None => {
                log::warn!(
                    "Battery: monitor not detected on I2C (tried 0x36 MAX17048 and 0x0B LC709203)"
                );
            }
        }
        Self { monitor }
    }
}

/// Read battery telemetry using the previously detected monitor type.
#[cfg(target_os = "espidf")]
pub fn read_battery(
    bus: &mut FeatherI2cBus,
    monitor: BatteryMonitor,
) -> anyhow::Result<BatterySnapshot> {
    match monitor {
        BatteryMonitor::Max17048 => read_max17048(bus),
        BatteryMonitor::Lc709203 => read_lc709203(bus),
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
