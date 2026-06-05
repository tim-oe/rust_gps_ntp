use esp_idf_svc::hal::i2c;

#[derive(Debug, Clone, Default)]
pub struct BatterySnapshot {
    pub voltage_v: f32,
    pub percent: f32,
}

pub fn read_battery(i2c: &mut i2c::I2cDriver<'_>) -> Option<BatterySnapshot> {
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
