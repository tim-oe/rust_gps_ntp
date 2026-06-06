use esp_idf_svc::hal::i2c;

#[derive(Debug, Clone, Default)]
pub struct BatterySnapshot {
    pub voltage_v: f32,
    pub percent: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum BatteryMonitor {
    Max17048,
    Lc709203,
}

fn read_max17048(i2c: &mut i2c::I2cDriver<'_>) -> Option<BatterySnapshot> {
    const MAX17048_ADDR: u8 = 0x36;
    const REG_VCELL: u8 = 0x02;
    const REG_SOC: u8 = 0x04;
    let mut vcell = [0_u8; 2];
    let mut soc = [0_u8; 2];
    i2c.write_read(MAX17048_ADDR, &[REG_VCELL], &mut vcell, 50).ok()?;
    i2c.write_read(MAX17048_ADDR, &[REG_SOC], &mut soc, 50).ok()?;

    let vraw = u16::from_be_bytes(vcell);
    let voltage_v = (vraw as f32) * 78.125e-6;
    let percent = (soc[0] as f32) + ((soc[1] as f32) / 256.0);

    Some(BatterySnapshot { voltage_v, percent })
}

fn read_lc709203(i2c: &mut i2c::I2cDriver<'_>) -> Option<BatterySnapshot> {
    const LC709203_ADDR: u8 = 0x0B;
    const REG_VCELL_MV: u8 = 0x09;
    const REG_RSOC: u8 = 0x0D;

    let mut vcell = [0_u8; 2];
    let mut rsoc = [0_u8; 2];
    i2c.write_read(LC709203_ADDR, &[REG_VCELL_MV], &mut vcell, 50)
        .ok()?;
    i2c.write_read(LC709203_ADDR, &[REG_RSOC], &mut rsoc, 50)
        .ok()?;

    // LC709203 uses little-endian 16-bit register values.
    let voltage_mv = u16::from_le_bytes(vcell) as f32;
    let percent = u16::from_le_bytes(rsoc) as f32;

    Some(BatterySnapshot {
        voltage_v: voltage_mv / 1000.0,
        percent,
    })
}

pub fn detect_monitor(i2c: &mut i2c::I2cDriver<'_>) -> Option<BatteryMonitor> {
    if read_max17048(i2c).is_some() {
        return Some(BatteryMonitor::Max17048);
    }
    if read_lc709203(i2c).is_some() {
        return Some(BatteryMonitor::Lc709203);
    }
    None
}

pub fn read_battery(i2c: &mut i2c::I2cDriver<'_>, monitor: BatteryMonitor) -> Option<BatterySnapshot> {
    match monitor {
        BatteryMonitor::Max17048 => read_max17048(i2c),
        BatteryMonitor::Lc709203 => read_lc709203(i2c),
    }
}
