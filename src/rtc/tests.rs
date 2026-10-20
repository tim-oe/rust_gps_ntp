use super::*;

#[test]
fn bcd_round_trip() {
    assert_eq!(bcd_to_dec(0x59), 59);
    assert_eq!(dec_to_bcd(59), 0x59);
}

#[test]
fn decode_valid_rtc_regs() {
    // 2024-06-07 14:30:45 (weekday Fri=5) — OS stopped bit clear.
    // Layout: ss mm hh dd wd MM yy per PCF8523 register map.
    let regs = [0x45, 0x30, 0x14, 0x07, 0x05, 0x06, 0x24];
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
    let regs = [0xC5, 0x30, 0x14, 0x07, 0x05, 0x06, 0x24];
    assert!(decode_datetime_regs(regs).is_none());
}

#[test]
fn decode_rejects_pre_2020() {
    let regs = [0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x19];
    assert!(decode_datetime_regs(regs).is_none());
}

#[test]
fn encode_decode_register_round_trip() {
    let dt = RtcDateTime {
        year: 2031,
        month: 12,
        day: 31,
        hour: 23,
        minute: 59,
        second: 59,
    };
    let regs = encode_datetime_regs(dt);
    // Register layout: ss mm hh dd wd MM yy, all BCD.
    assert_eq!(regs, [0x59, 0x59, 0x23, 0x31, 1, 0x12, 0x31]);
    assert_eq!(decode_datetime_regs(regs), Some(dt));
}

#[test]
fn encode_clamps_year_to_two_digit_range() {
    let dt = RtcDateTime {
        year: 2150,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };
    assert_eq!(encode_datetime_regs(dt)[6], 0x99);
    let early = RtcDateTime { year: 1999, ..dt };
    assert_eq!(encode_datetime_regs(early)[6], 0x00);
}

#[test]
fn decode_rejects_out_of_range_fields() {
    let good = [0x45, 0x30, 0x14, 0x07, 0x05, 0x06, 0x24];
    let mut bad_month = good;
    bad_month[5] = 0x13;
    assert!(decode_datetime_regs(bad_month).is_none());
    let mut bad_day = good;
    bad_day[3] = 0x00;
    assert!(decode_datetime_regs(bad_day).is_none());
    let mut bad_hour = good;
    bad_hour[2] = 0x24;
    assert!(decode_datetime_regs(bad_hour).is_none());
    let mut bad_second = good;
    bad_second[0] = 0x60;
    assert!(decode_datetime_regs(bad_second).is_none());
}

#[test]
fn unix_seconds_to_datetime_matches_known_epoch() {
    // 2024-06-07T14:30:45Z
    let dt = unix_seconds_to_datetime(1_717_770_645).expect("valid");
    assert_eq!(
        dt,
        RtcDateTime {
            year: 2024,
            month: 6,
            day: 7,
            hour: 14,
            minute: 30,
            second: 45,
        }
    );
    assert_eq!(datetime_to_unix_seconds(dt), Some(1_717_770_645));
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
