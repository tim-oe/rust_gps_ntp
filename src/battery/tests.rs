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
