use super::*;

/// Build [`ServoStats`] for mode-6 builder tests.
fn stats(pps_offset_us: i32, pps_jitter_us: f32, proc_delay_us: f32, freq_ppm: f64) -> ServoStats {
    ServoStats {
        pps_offset_us,
        pps_jitter_us,
        proc_delay_us,
        freq_ppm,
    }
}

/// Decode the mode-6 status word (header bytes 4..6).
fn mode6_status(resp: &[u8]) -> u16 {
    u16::from_be_bytes([resp[4], resp[5]])
}

#[test]
fn build_response_sets_server_mode_and_stratum_when_synced() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 1.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0_u8; NTP_PACKET_LEN];
    let resp = build_response(&req, &dp, 0x0123_0000_0000_0000, MODE_SERVER);
    assert_eq!(resp[0] & 0x07, 4);
    assert_eq!(resp[1], 1);
    assert_eq!(&resp[12..16], b"GPS\0");
}

#[test]
fn build_response_marks_unsynced_with_init_refid() {
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        root_disp_short: 5 << 16,
        root_disp_ms: 5000.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0_u8; NTP_PACKET_LEN];
    let resp = build_response(&req, &dp, 0, MODE_SERVER);
    // RFC 5905 §7.3: internal stratum 16 is transmitted as 0.
    assert_eq!(resp[1], 0, "unsynchronised stratum must be 0 on the wire");
    assert_eq!(resp[0] >> 6, 3, "LI must be 3 (alarm) when unsynchronised");
    assert_eq!(&resp[12..16], b"INIT");
}

#[test]
fn build_mode6_readvar_includes_gps_peer_when_synced() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 1.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [
        0,
        MODE6_OPCODE_READVAR,
        0,
        0,
        0,
        0,
        0,
        MODE6_ASSOC_ID as u8,
        0,
        0,
        0,
        0,
    ];
    let resp = build_mode6_response(&req, 4, &dp, &stats(250, 500.0, 1200.0, 0.0));
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    assert!(payload.contains("refid=GPS"));
    // Trailing comma so "stratum=16" cannot satisfy the check.
    assert!(payload.contains("stratum=1,"), "{payload}");
    assert!(payload.contains("leap=00,"), "{payload}");
    assert!(payload.contains("offset=0.250,"), "{payload}");
    assert!(payload.contains("delay=1.200,"), "{payload}");
}

#[test]
fn write_u64_be_roundtrip() {
    let mut buf = [0_u8; 8];
    write_u64_be(&mut buf, 0x0123_4567_89ab_cdef);
    assert_eq!(buf, 0x0123_4567_89ab_cdef_u64.to_be_bytes());
}

#[test]
fn write_u32_be_roundtrip() {
    let mut buf = [0_u8; 4];
    write_u32_be(&mut buf, 0x0123_4567);
    assert_eq!(buf, 0x0123_4567_u32.to_be_bytes());
}

#[test]
fn ntp_timestamp_now_is_nonzero() {
    assert_ne!(ntp_timestamp_now(), 0);
}

#[test]
fn build_mode6_readstat_returns_association_row() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 1.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    assert_eq!(resp.len(), NTP_CONTROL_HEADER_LEN + 4);
    assert_eq!(resp[1] & MODE6_ERROR_BIT, 0);
    // Header carries the system status word; payload is (assoc id, peer status).
    assert_eq!(mode6_status(&resp) >> 8 & 0x3f, 4, "ClkSrc must be UHF/GPS");
    let assoc = u16::from_be_bytes([resp[12], resp[13]]);
    let peer_status = u16::from_be_bytes([resp[14], resp[15]]);
    assert_eq!(assoc, MODE6_ASSOC_ID);
    assert_eq!(peer_status >> 13, 6, "sel must be 6 (system peer)");
}

#[test]
fn build_mode6_readstat_for_known_assoc_returns_peer_status_in_header() {
    // RFC 1305 B.3: READSTAT with a non-zero association id returns that
    // peer's status word in the header status field and no payload.
    let dp = synced_dp();
    let req = [
        0,
        MODE6_OPCODE_READSTAT,
        0,
        0,
        0,
        0,
        0,
        MODE6_ASSOC_ID as u8,
        0,
        0,
        0,
        0,
    ];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    assert_eq!(
        resp.len(),
        NTP_CONTROL_HEADER_LEN,
        "no payload for one assoc"
    );
    assert_eq!(resp[1] & MODE6_ERROR_BIT, 0);
    assert_eq!(mode6_status(&resp), 0xD200, "sel=6, cfg, reach");
    assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), MODE6_ASSOC_ID);
}

#[test]
fn build_mode6_unknown_assoc_returns_bad_assoc_error_status() {
    // RFC 1305 B.2.4: unknown association id → error bit + code 4 in the
    // high byte of the status word.
    let dp = synced_dp();
    for opcode in [MODE6_OPCODE_READSTAT, MODE6_OPCODE_READVAR] {
        let req = [0, opcode, 0, 0, 0, 0, 0x12, 0x34, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
        assert_eq!(resp.len(), NTP_CONTROL_HEADER_LEN, "opcode {opcode}");
        assert_ne!(resp[1] & MODE6_ERROR_BIT, 0, "opcode {opcode}");
        assert_eq!(resp[1] & 0x1f, opcode, "opcode must be echoed");
        assert_eq!(
            mode6_status(&resp),
            MODE6_ERR_BAD_ASSOC << 8,
            "opcode {opcode}: error status word"
        );
        assert_eq!(&resp[6..8], &[0x12, 0x34], "assoc id must be echoed");
    }
}

#[test]
fn build_mode6_readvar_unsynced_system_vars() {
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        root_disp_short: 5 << 16,
        root_disp_ms: 5000.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    assert!(payload.contains("stratum=16"));
    assert!(payload.contains("refid=INIT"));
}

#[test]
fn build_mode6_readvar_unsynced_peer_vars() {
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        root_disp_short: 5 << 16,
        root_disp_ms: 5000.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [
        0,
        MODE6_OPCODE_READVAR,
        0,
        0,
        0,
        0,
        0,
        MODE6_ASSOC_ID as u8,
        0,
        0,
        0,
        0,
    ];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    assert!(payload.contains("srcadr=INIT"));
}

#[test]
fn build_mode6_unsupported_opcode_sets_error_bit() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 1.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    // Opcode 31 is the largest value that fits the 5-bit field.
    let req = [0, 31, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    assert_ne!(resp[1] & MODE6_ERROR_BIT, 0);
    assert_ne!(resp[1] & MODE6_RESPONSE_BIT, 0);
    assert_eq!(resp[1] & 0x1f, 31, "opcode must be echoed");
    // RFC 1305 B.2.4: error status word carries code 3 (bad opcode).
    assert_eq!(mode6_status(&resp), MODE6_ERR_BAD_OPCODE << 8);
    assert_eq!(
        resp.len(),
        NTP_CONTROL_HEADER_LEN,
        "error replies carry no data"
    );
}

#[test]
fn build_mode6_leap_text_is_two_bit_binary() {
    // ntpq renders leap as the 2-bit LI value in binary: 00, 01, 10, 11.
    for (leap, expected) in [(0_u8, "leap=00,"), (1, "leap=01,"), (2, "leap=10,")] {
        let dp = DisciplineParams {
            leap,
            ..synced_dp()
        };
        let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, &stats(0, 1.0, 1.0, 0.0));
        let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
        assert!(payload.contains(expected), "LI={leap}: {payload}");
        // The system status word LI field must agree with the text.
        assert_eq!(mode6_status(&resp) >> 14, leap as u16, "LI={leap}");
    }
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        ..synced_dp()
    };
    let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 1.0, 1.0, 0.0));
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    assert!(payload.contains("leap=11,"), "{payload}");
}

#[test]
fn build_mode6_invalid_version_defaults_to_v4() {
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        root_disp_short: 5 << 16,
        root_disp_ms: 5000.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 0, &dp, &stats(0, 0.0, 0.0, 0.0));
    assert_eq!((resp[0] >> 3) & 0x07, 4);
}

impl NtpServer {
    fn new_for_test() -> anyhow::Result<Self> {
        Self::new_loopback()
    }

    /// Test helper: call `observe_pps_pulse` using the current monotonic clock
    /// as the edge timestamp.  Production code should always pass the ISR-captured
    /// edge time; this wrapper exists only to keep test call sites concise.
    fn pps_now(&mut self, pps_interval_us: Option<u32>) {
        self.observe_pps_pulse(pps_interval_us, monotonic_us_now());
    }

    /// Test helper: simulate a PPS edge at an explicit monotonic timestamp.
    fn pps_at(&mut self, pps_interval_us: Option<u32>, edge_us: i64) {
        self.observe_pps_pulse(pps_interval_us, edge_us);
    }

    /// Test helper: simulate a PPS edge that occurred `ago_us` before now.
    fn pps_stale_ago(&mut self, pps_interval_us: Option<u32>, ago_us: i64) {
        self.pps_at(pps_interval_us, monotonic_us_now().saturating_sub(ago_us));
    }
}

/// Decode an NTP 64-bit timestamp to Unix epoch microseconds.
fn ntp_timestamp_to_unix_us(ts: u64) -> i64 {
    let ntp_seconds = (ts >> 32) as i64;
    let unix_seconds = ntp_seconds - NTP_UNIX_EPOCH_OFFSET_SECS as i64;
    let fraction = ts & 0xFFFF_FFFF;
    let micros = (fraction as i128 * 1_000_000 / (1i128 << 32)) as i64;
    unix_seconds * MICROS_PER_SEC + micros
}

fn client_ntp_request() -> [u8; NTP_PACKET_LEN] {
    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3;
    req
}

#[test]
fn update_gps_utc_seconds_establishes_anchor_without_nmea_fudge() {
    // Pre-lock the anchor is the NMEA second at parse time.  The PPS fudge
    // describes the *edge* ↔ sentence relationship and only applies once an
    // edge timestamp exists (see `gps_second_at_edge`).
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    assert!(server.clock_anchor.is_some());
    assert_eq!(server.last_gps_utc_seconds, Some(1_700_000_000));
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        1_700_000_000
    );
}

#[test]
fn update_gps_utc_seconds_tracks_latest_nmea_before_pps() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.update_gps_utc_seconds(1_700_000_010);
    let anchor = server.clock_anchor.expect("anchor");
    assert_eq!(anchor.unix_seconds, 1_700_000_010);
}

#[test]
fn update_gps_utc_seconds_overwrites_rtc_bootstrap_anchor() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.seed_utc_seconds(1_699_999_000);
    server.update_gps_utc_seconds(1_700_000_000);
    let anchor = server.clock_anchor.expect("anchor");
    assert_eq!(
        anchor.unix_seconds, 1_700_000_000,
        "GPS NMEA must replace a stale RTC bootstrap anchor"
    );
}

#[test]
fn seed_utc_seconds_does_not_satisfy_pps_gps_fresh_guard() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.seed_utc_seconds(1_700_000_000);
    server.pps_now(None);
    assert!(
        !server.pps_locked,
        "PPS must not lock on RTC seed alone; wait for real GPS NMEA"
    );
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    assert!(server.pps_locked);
}

#[test]
fn observe_pps_pulse_delta_does_not_lock_on_rtc_seed_without_gps() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.seed_utc_seconds(1_700_000_000);
    server.pps_now(Some(1_000_000));
    assert!(
        !server.pps_locked,
        "Delta path must not lock on RTC bootstrap without GPS UTC"
    );
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        1_700_000_000,
        "RTC anchor must not be incremented without GPS lock"
    );
}

#[test]
fn observe_pps_pulse_delta_first_lock_applies_nmea_pps_fudge() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.seed_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(Some(1_000_000));
    assert!(server.pps_locked);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        1_700_000_000 + NMEA_PPS_FUDGE_S
    );
}

#[test]
fn update_gps_utc_seconds_does_not_reanchor_when_pps_locked() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let before = server.clock_anchor.expect("anchor").unix_seconds;
    server.update_gps_utc_seconds(1_699_999_000);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        before,
        "lagged NMEA must not pull the PPS anchor backward"
    );
    assert_eq!(
        server.last_gps_utc_seconds,
        Some(1_699_999_000),
        "freshness is still updated while PPS-locked"
    );
}

#[test]
fn seed_utc_seconds_does_not_apply_nmea_fudge() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.seed_utc_seconds(1_700_000_000);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        1_700_000_000
    );
}

#[test]
fn seed_utc_seconds_does_not_overwrite_existing_anchor() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    let before = server.clock_anchor.expect("anchor").unix_seconds;
    server.seed_utc_seconds(1_699_999_000);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, before);
}

#[test]
fn observe_pps_pulse_first_pulse_locks_to_gps() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    assert!(server.pps_locked);
    assert!(server.last_pps_monotonic_us.is_some());
}

#[test]
fn first_pps_pulse_applies_nmea_pps_fudge_to_anchor() {
    let gps_utc = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(gps_utc);
    server.pps_now(None);
    let anchor = server.clock_anchor.expect("anchor");
    assert_eq!(anchor.unix_seconds, gps_utc + NMEA_PPS_FUDGE_S);
}

#[test]
fn observe_pps_pulse_valid_interval_advances_anchor() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 5 * MICROS_PER_SEC;
    server.nmea_for_edge(1_700_000_000, base);
    server.pps_at(None, base);
    let before = server.clock_anchor.expect("anchor").unix_seconds;
    assert_eq!(before, 1_700_000_000);
    server.nmea_for_edge(1_700_000_001, base + MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        before + 1
    );
    assert!(server.pps_has_sample);
    assert_eq!(server.anchor_step_count, 0, "no GPS correction needed");
}

// --- Integer-second accounting (missed / rejected pulses) ---

/// Simulated NMEA parse latency relative to the PPS edge it describes
/// (MTK3339 typically 150–600 ms after the edge).
const NMEA_LATENCY_US: i64 = 300_000;

impl NtpServer {
    /// Test helper: record an NMEA UTC second parsed at an explicit
    /// monotonic time, bypassing `update_gps_utc_seconds`' use of the real
    /// clock so edge/NMEA ordering can be scripted precisely.
    fn nmea_at(&mut self, utc_unix_seconds: i64, parsed_at_us: i64) {
        self.last_gps_utc_seconds = Some(utc_unix_seconds);
        self.last_gps_utc_update_us = Some(parsed_at_us);
    }

    /// Test helper: record the NMEA sentence describing the PPS edge at
    /// `edge_us`, placed after the edge for the default module behaviour
    /// (`NMEA_PPS_FUDGE_S = 1`) or before it for modules that transmit NMEA
    /// ahead of the pulse (`NMEA_PPS_FUDGE_S = 0`), so these tests hold for
    /// either `CONFIG_GPS_NTP_NMEA_PPS_FUDGE_S` setting.
    fn nmea_for_edge(&mut self, utc_unix_seconds: i64, edge_us: i64) {
        let parsed_at = if NMEA_PPS_FUDGE_S == 0 {
            edge_us - NMEA_LATENCY_US
        } else {
            edge_us + NMEA_LATENCY_US
        };
        self.nmea_at(utc_unix_seconds, parsed_at);
    }
}

/// Lock at `base` to UTC `utc0` using realistic NMEA timing and return `base`.
fn locked_server_at(server: &mut NtpServer, utc0: i64, base: i64) {
    server.nmea_for_edge(utc0, base);
    server.pps_at(None, base);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0);
    server.nmea_for_edge(utc0 + 1, base + MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 1);
}

/// Main loop stalled >1 s: two ISR edges collapse into one ~2 s interval
/// (rejected by the range filter).  The following pulse must account for
/// all elapsed seconds even with no fresh NMEA to cross-check against.
#[test]
fn missed_pps_pulse_does_not_shift_integer_second_without_gps() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    // Edges at +2 s and +3 s seen in one poll → interval 2 s → rejected.
    server.pps_at(Some(2_000_000), base + 3 * MICROS_PER_SEC);
    // Next edge at +4 s with a normal 1 s interval; NMEA is now stale.
    server.pps_at(Some(1_000_000), base + 4 * MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        utc0 + 4,
        "anchor must advance by elapsed seconds, not by pulse count"
    );
    assert_eq!(server.anchor_step_count, 0);
    // Served time at `now` (= base + 10 s) must be utc0 + 10.
    assert_eq!(server.current_utc_unix_seconds(), Some(utc0 + 10));
}

/// Same stall, but NMEA is flowing: predicted and GPS seconds agree, so no
/// step is recorded.
#[test]
fn missed_pps_pulse_does_not_shift_integer_second_with_gps() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    server.pps_at(Some(2_000_000), base + 3 * MICROS_PER_SEC);
    server.nmea_for_edge(utc0 + 3, base + 3 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 4 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 4);
    assert_eq!(server.anchor_step_count, 0);
}

/// Two consecutive outliers (+60 ms, −60 ms) are rejected; the next good
/// pulse must still land on the correct second.
#[test]
fn rejected_outlier_pair_does_not_shift_integer_second() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    server.pps_at(Some(1_060_000), base + 2 * MICROS_PER_SEC + 60_000);
    server.pps_at(Some(940_000), base + 3 * MICROS_PER_SEC);
    assert_eq!(server.pps_glitch_count, 2);
    server.pps_at(Some(1_000_000), base + 4 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 4);
    assert_eq!(server.current_utc_unix_seconds(), Some(utc0 + 10));
}

/// If the anchor's integer second has drifted from GPS (simulated here by
/// corrupting it), fresh NMEA must correct it once the disagreement has been
/// seen on two consecutive pulses, and the correction must be counted once.
#[test]
fn pps_second_corrected_from_fresh_gps_when_anchor_disagrees() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    if let Some(anchor) = server.clock_anchor.as_mut() {
        anchor.unix_seconds -= 1; // off by one second
    }
    // First disagreeing pulse: held pending, prediction still used.
    server.nmea_for_edge(utc0 + 2, base + 2 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 2 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 1);
    assert_eq!(server.anchor_step_count, 0);
    // Second consecutive disagreement with the same delta: GPS wins.
    server.nmea_for_edge(utc0 + 3, base + 3 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 3 * MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        utc0 + 3,
        "GPS must win over the pulse-count prediction"
    );
    assert_eq!(server.anchor_step_count, 1);
    assert_eq!(server.ntp_snapshot(true).anchor_step_count, 1);
}

/// Field regression: the main loop stalls ~0.8 s, so the sentence for second
/// N+2 (emitted 300 ms after its edge) is only parsed *after* edge N+3 has
/// fired, and in the same iteration that then processes edge N+3.  The
/// parse-time pairing reads one second low for that single pulse.  Previously
/// this produced a −1 step followed by a +1 step one second later; now the
/// transient must be ignored and no step recorded.
#[test]
fn late_parsed_backlog_sentence_does_not_step_anchor() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    // Sentence for utc0+2 parsed 50 ms after edge utc0+3 (stall), then the
    // edge for utc0+3 is processed with a clean 1 s interval.
    server.nmea_at(utc0 + 2, base + 3 * MICROS_PER_SEC + 50_000);
    server.pps_at(Some(1_000_000), base + 3 * MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        utc0 + 3,
        "pulse-count prediction must be kept for a single disagreeing pulse"
    );
    assert_eq!(server.anchor_step_count, 0);
    // Loop caught up: next sentence and edge are normal and agree.
    server.nmea_for_edge(utc0 + 3, base + 3 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 4 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 4);
    assert_eq!(server.anchor_step_count, 0, "no flip-flop step pair");
}

/// A transient disagreement in one direction followed by one in the other
/// must not be treated as confirmation: the delta has to match.
#[test]
fn opposite_sign_disagreements_do_not_confirm_a_step() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    // −1: backlog sentence for utc0+1 parsed after edge utc0+2.
    server.nmea_at(utc0 + 1, base + 2 * MICROS_PER_SEC + 50_000);
    server.pps_at(Some(1_000_000), base + 2 * MICROS_PER_SEC);
    // +1: (synthetic) sentence claiming utc0+4 for edge utc0+3.
    server.nmea_for_edge(utc0 + 4, base + 3 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 3 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 3);
    assert_eq!(server.anchor_step_count, 0);
}

/// Without fresh GPS the servo has nothing to cross-check against, so a
/// corrupted anchor second is carried forward (no silent "correction").
#[test]
fn pps_second_not_corrected_when_gps_is_stale() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    if let Some(anchor) = server.clock_anchor.as_mut() {
        anchor.unix_seconds -= 1;
    }
    // No NMEA since base+1.3 s; edge at +4 s is > 2 s later → stale.
    server.pps_at(Some(1_000_000), base + 4 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 3);
    assert_eq!(server.anchor_step_count, 0);
}

/// A PPS edge processed *after* the NMEA sentence for its own second was
/// parsed (main-loop latency) must map to that second, not the next one.
#[test]
fn late_processed_pps_edge_uses_nmea_parse_time() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 5 * MICROS_PER_SEC;
    // NMEA for utc0 parsed 300 ms after edge `base`; the edge itself is
    // only handed to the servo now (after the sentence).
    server.nmea_for_edge(utc0, base);
    server.pps_at(None, base);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0);
    // Likewise on the delta path while locked.
    server.nmea_for_edge(utc0 + 1, base + MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 1);
    assert_eq!(server.anchor_step_count, 0);
}

/// A dropped NMEA sentence (checksum failure) leaves the previous second as
/// the latest; the formula must still place the next edge correctly.
#[test]
fn dropped_nmea_sentence_does_not_misplace_next_edge() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 5 * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    // NMEA for utc0+2 dropped; last parsed is utc0+1 at base+1.3 s.
    server.pps_at(Some(1_000_000), base + 2 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, utc0 + 2);
    assert_eq!(server.anchor_step_count, 0);
}

/// Positive leap second.  NMEA emits 23:59:60, which does not parse, so the
/// receiver's next sentence (00:00:00) repeats the Unix second.  The pulse
/// count runs one ahead and must be pulled back by GPS once two consecutive
/// post-leap sentences confirm the offset (one pulse of confirmation delay).
#[test]
fn leap_second_is_corrected_from_gps_within_two_pulses() {
    // 2016-12-31T23:59:59Z = 1483228799; 2017-01-01T00:00:00Z = 1483228800.
    let last_sec = 1_483_228_799_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 10 * MICROS_PER_SEC;
    locked_server_at(&mut server, last_sec - 2, base);
    // Edge for 23:59:59; NMEA "235959" follows.
    server.nmea_for_edge(last_sec, base + 2 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 2 * MICROS_PER_SEC);
    assert_eq!(server.clock_anchor.expect("anchor").unix_seconds, last_sec);
    // Edge for 23:59:60; NMEA "235960" fails to parse → no update.
    server.pps_at(Some(1_000_000), base + 3 * MICROS_PER_SEC);
    // Edge for 00:00:00 (Unix 1483228800).  Processed before its NMEA.
    server.pps_at(Some(1_000_000), base + 4 * MICROS_PER_SEC);
    // Prediction ran ahead by the inserted second; NMEA "000000" arrives.
    server.nmea_for_edge(last_sec + 1, base + 4 * MICROS_PER_SEC);
    // Edge for 00:00:01: first disagreeing pulse, held pending.
    server.pps_at(Some(1_000_000), base + 5 * MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        last_sec + 3
    );
    assert_eq!(server.anchor_step_count, 0);
    // Edge for 00:00:02: same delta again, so GPS wins.
    server.nmea_for_edge(last_sec + 2, base + 5 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 6 * MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        last_sec + 3,
        "anchor must follow UTC across the leap second"
    );
    assert_eq!(server.anchor_step_count, 1, "exactly one GPS correction");
    // Steady state afterwards: no further corrections.
    server.nmea_for_edge(last_sec + 3, base + 6 * MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + 7 * MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        last_sec + 4
    );
    assert_eq!(server.anchor_step_count, 1);
}

/// Recovery after a long PPS outage: the next good pulse re-derives the
/// second from elapsed time and the (fresh) GPS second.
#[test]
fn pps_outage_recovery_lands_on_correct_second() {
    let utc0 = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let outage_s = 600_i64;
    let base = monotonic_us_now() - (outage_s + 5) * MICROS_PER_SEC;
    locked_server_at(&mut server, utc0, base);
    // PPS silent for `outage_s`; first returning edge has a 600 s interval
    // (rejected), the one after it is normal.
    let back = base + (outage_s + 1) * MICROS_PER_SEC;
    server.pps_at(Some(outage_s as u32 * 1_000_000), back);
    server.nmea_for_edge(utc0 + outage_s + 1, back);
    server.pps_at(Some(1_000_000), back + MICROS_PER_SEC);
    assert_eq!(
        server.clock_anchor.expect("anchor").unix_seconds,
        utc0 + outage_s + 2
    );
    assert_eq!(server.ntp_snapshot(true).stratum, 1);
}

#[test]
fn observe_pps_pulse_ignores_invalid_interval() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(Some(2_000_000));
    assert!(!server.pps_has_sample);
}

#[test]
fn poll_serves_client_time_request() {
    let gps_utc = 1_700_000_000_i64;
    let mut server = NtpServer::new_for_test().expect("test socket");
    let addr = server.socket.local_addr().expect("local addr");
    server.update_gps_utc_seconds(gps_utc);
    server.pps_now(None);

    let expected_us = ntp_timestamp_to_unix_us(server.current_ntp_timestamp());

    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    client
        .send_to(&client_ntp_request(), addr)
        .expect("send request");

    server.poll(true).expect("poll");
    let mut resp = [0_u8; NTP_PACKET_LEN];
    let (len, _) = client.recv_from(&mut resp).expect("response");
    assert_eq!(len, NTP_PACKET_LEN);
    assert_eq!(resp[0] & 0x07, 4);
    assert_eq!(resp[1], 1);

    let transmit_ts = u64::from_be_bytes(resp[40..48].try_into().unwrap());
    let transmit_us = ntp_timestamp_to_unix_us(transmit_ts);
    let anchor_unix = gps_utc + NMEA_PPS_FUDGE_S;
    let since_anchor_us = transmit_us - anchor_unix * MICROS_PER_SEC;
    assert!(
        (0..MICROS_PER_SEC).contains(&since_anchor_us),
        "transmit time should be within the anchored GPS second {anchor_unix} (got {since_anchor_us} µs past it)"
    );
    assert!(
        (transmit_us - expected_us).abs() < 5_000,
        "transmit timestamp should be within 5 ms of server clock"
    );
}

#[test]
fn poll_response_receive_timestamp_precedes_transmit() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let addr = server.socket.local_addr().expect("local addr");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    client
        .send_to(&client_ntp_request(), addr)
        .expect("send request");
    server.poll(true).expect("poll");

    let mut resp = [0_u8; NTP_PACKET_LEN];
    client.recv_from(&mut resp).expect("response");
    let receive_ts = u64::from_be_bytes(resp[32..40].try_into().unwrap());
    let transmit_ts = u64::from_be_bytes(resp[40..48].try_into().unwrap());
    assert_ne!(receive_ts, 0, "receive timestamp should be populated");
    assert!(
        receive_ts <= transmit_ts,
        "T2 (receive) must not follow T3 (transmit)"
    );
}

#[test]
fn poll_serves_mode6_readvar() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let addr = server.socket.local_addr().expect("local addr");
    let mut req = [0_u8; NTP_CONTROL_HEADER_LEN];
    req[0] = (4 << 3) | 6;
    req[1] = MODE6_OPCODE_READVAR;
    req[7] = MODE6_ASSOC_ID as u8;

    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    client.send_to(&req, addr).expect("send mode-6");

    server.poll(false).expect("poll");
    let mut resp = vec![0_u8; 512];
    let (len, _) = client.recv_from(&mut resp).expect("mode-6 response");
    assert!(len >= NTP_CONTROL_HEADER_LEN);
}

#[test]
fn poll_serves_unsynced_time_request_without_anchor() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let addr = server.socket.local_addr().expect("local addr");

    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    client
        .send_to(&client_ntp_request(), addr)
        .expect("send request");

    server.poll(false).expect("poll");
    let mut resp = [0_u8; NTP_PACKET_LEN];
    let (len, _) = client.recv_from(&mut resp).expect("response");
    assert_eq!(len, NTP_PACKET_LEN);
    // RFC 5905 §7.3: unsynchronised → stratum 0 on the wire, LI=3, refid INIT.
    assert_eq!(resp[1], 0);
    assert_eq!(resp[0] >> 6, 3);
    assert_eq!(&resp[12..16], b"INIT");
}

#[test]
fn poll_smoothes_processing_delay_after_second_request() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let addr = server.socket.local_addr().expect("local addr");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    for _ in 0..2 {
        client
            .send_to(&client_ntp_request(), addr)
            .expect("send request");
        server.poll(true).expect("poll");
        let mut resp = [0_u8; NTP_PACKET_LEN];
        client.recv_from(&mut resp).expect("response");
    }
    assert!(server.proc_delay_has_sample);
    assert!(server.proc_delay_us >= 1.0);
    let snap = server.ntp_snapshot(true);
    assert!(snap.proc_delay_has_sample);
    assert!(snap.proc_delay_us >= 1.0);
}

#[test]
fn observe_pps_pulse_smoothes_jitter_on_second_valid_interval() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.pps_now(Some(1_000_250));
    server.pps_now(Some(1_000_500));
    assert!(server.pps_has_sample);
    assert_eq!(server.pps_offset_us, 500);
    assert!(server.pps_jitter_us > 0.0);
}

#[test]
fn poll_ignores_short_client_packet() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let addr = server.socket.local_addr().expect("local addr");
    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    client.send_to(&[0_u8; 8], addr).expect("send short");
    server.poll(false).expect("poll");
    client
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("timeout");
    let mut resp = [0_u8; 64];
    assert!(client.recv_from(&mut resp).is_err());
}

// --- Servo and holdover tests ---

#[test]
fn servo_updates_freq_ppm_positive_for_fast_clock() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    // Interval longer than 1s: monotonic is fast relative to GPS.
    server.pps_now(Some(1_000_500));
    assert!(
        server.freq_ppm > 0.0,
        "freq_ppm should be positive when interval > 1s (fast clock)"
    );
    // Expected: 0.0 + 500 * 0.01 = 5.0 ppm
    assert!(
        (server.freq_ppm - 5.0).abs() < 0.001,
        "expected ~5.0 ppm, got {}",
        server.freq_ppm
    );
}

#[test]
fn servo_updates_freq_ppm_negative_for_slow_clock() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    // Interval shorter than 1s: monotonic is slow relative to GPS.
    server.pps_now(Some(999_500));
    assert!(
        server.freq_ppm < 0.0,
        "freq_ppm should be negative when interval < 1s (slow clock)"
    );
    // Expected: 0.0 + (-500) * 0.01 = -5.0 ppm
    assert!(
        (server.freq_ppm + 5.0).abs() < 0.001,
        "expected ~-5.0 ppm, got {}",
        server.freq_ppm
    );
}

/// A constant +100 ppm oscillator offset (every interval reads 1 000 100 µs)
/// must drive the EWMA estimate *toward* 100 ppm, not integrate past it.
#[test]
fn servo_converges_to_constant_frequency_offset() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    for _ in 0..10 {
        server.pps_now(Some(1_000_100));
    }
    // EWMA after 10 samples: 100 × (1 − 0.99^10) ≈ 9.56 ppm.
    let expected_10 = 100.0 * (1.0 - 0.99_f64.powi(10));
    assert!(
        (server.freq_ppm - expected_10).abs() < 0.01,
        "after 10 pulses expected ~{expected_10:.2} ppm, got {}",
        server.freq_ppm
    );
    for _ in 10..1_000 {
        server.pps_now(Some(1_000_100));
    }
    assert!(
        (server.freq_ppm - 100.0).abs() < 0.1,
        "after 1000 pulses expected ~100 ppm, got {}",
        server.freq_ppm
    );
}

/// Regression: the estimator must not drift away from a small constant
/// offset over a long run (an unbounded integrator reached the ±500 ppm
/// clamp after ~83 minutes of a 10 ppm offset).
#[test]
fn servo_estimate_stays_bounded_for_small_constant_offset() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let mut max_seen = 0.0_f64;
    for _ in 0..6_000 {
        server.pps_now(Some(1_000_010));
        max_seen = max_seen.max(server.freq_ppm);
    }
    assert!(
        (server.freq_ppm - 10.0).abs() < 0.01,
        "expected ~10 ppm after 6000 pulses, got {}",
        server.freq_ppm
    );
    assert!(
        max_seen <= 10.0 + 1e-9,
        "estimate overshot the true offset: peak {max_seen}"
    );
}

/// The estimate must track a change in oscillator offset (e.g. thermal drift).
#[test]
fn servo_tracks_frequency_change() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    for _ in 0..1_000 {
        server.pps_now(Some(1_000_020));
    }
    assert!((server.freq_ppm - 20.0).abs() < 0.1);
    for _ in 0..1_000 {
        server.pps_now(Some(999_995));
    }
    assert!(
        (server.freq_ppm + 5.0).abs() < 0.1,
        "expected ~-5 ppm after offset change, got {}",
        server.freq_ppm
    );
}

/// The clamp protects against an absurd first sample (before the outlier
/// guard is armed): a +200 ms interval error would otherwise yield 2000 ppm.
#[test]
fn servo_clamps_freq_ppm_at_max() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    assert!(
        !server.pps_has_sample,
        "outlier guard must still be disarmed"
    );
    server.pps_now(Some(1_200_000));
    assert_eq!(server.freq_ppm, SERVO_MAX_FREQ_PPM);
    assert_eq!(server.pps_glitch_count, 0, "first sample is not an outlier");
}

#[test]
fn ntp_precision_exponent_scales_with_uncertainty() {
    assert_eq!(super::ntp_precision_from_uncertainty_us(1.0), -20);
    assert_eq!(super::ntp_precision_from_uncertainty_us(100.0), -14);
    assert_eq!(super::ntp_precision_from_uncertainty_us(500.0), -11);
    assert_eq!(super::ntp_precision_from_uncertainty_us(2_000_000.0), -4);
}

#[test]
fn discipline_params_precision_reflects_jitter_and_loop_floor() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.pps_jitter_us = 200.0;
    server.pps_has_sample = true;
    let dp = server.discipline_params(true);
    assert_eq!(
        dp.precision, -11,
        "500 µs loop floor dominates 200 µs jitter → 2^-11 s"
    );
}

#[test]
fn discipline_params_precision_poor_when_unsynced() {
    let server = NtpServer::new_for_test().expect("test socket");
    let dp = server.discipline_params(false);
    assert_eq!(dp.precision, -4);
}

#[test]
fn build_response_precision_matches_discipline_params() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 1.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -11,
    };
    let resp = build_response(&[0_u8; NTP_PACKET_LEN], &dp, 0, MODE_SERVER);
    assert_eq!(resp[3], (-11_i8) as u8);
}

#[test]
fn discipline_params_fully_synced_gives_stratum_1_and_base_dispersion() {
    // With the PHI model, locked dispersion is max(jitter, MIN_HW_ACCURACY_US) +
    // PHI × age_since_last_pulse. After the first pulse with no jitter sample
    // yet, this equals MIN_HW_ACCURACY_US (0.1 ms) — well below the old fixed
    // 1 ms — and stratum must be 1.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let dp = server.discipline_params(true);
    assert_eq!(dp.stratum, 1);
    assert_eq!(dp.leap, 0);
    // Dispersion should be in the hardware-floor range (0.1 ms + up to PHI×1s = 0.115 ms).
    assert!(
        dp.root_disp_ms >= 0.09 && dp.root_disp_ms < 0.5,
        "locked dispersion should be in [0.09, 0.5) ms, got {:.3}",
        dp.root_disp_ms
    );
}

#[test]
fn discipline_params_fresh_pps_holds_stratum_1_without_gps_fix() {
    // Holdover design: with fresh PPS and a valid anchor, remain stratum 1
    // even when GPS fix is temporarily lost, so clients keep using us while
    // dispersion is still low.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let dp = server.discipline_params(false);
    assert_eq!(
        dp.stratum, 1,
        "should hold stratum 1 with fresh PPS in holdover"
    );
}

#[test]
fn discipline_params_stale_pps_and_no_gps_fix_gives_stratum_16() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 3_000 * MICROS_PER_SEC);
    let dp = server.discipline_params(false);
    assert_eq!(dp.stratum, 16);
    assert_eq!(dp.leap, 3);
}

#[test]
fn discipline_params_no_pps_no_anchor_gives_stratum_16() {
    let server = NtpServer::new_for_test().expect("test socket");
    let dp = server.discipline_params(true);
    assert_eq!(dp.stratum, 16);
}

#[test]
fn holdover_dispersion_grows_after_pps_loss() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 60 * MICROS_PER_SEC);
    let dp = server.discipline_params(true);
    // Expected disp = BASE_DISP_US + 60 * RATE = 1000 + 30000 = 31000 us = 31 ms.
    assert!(
        (30.9..31.2).contains(&dp.root_disp_ms),
        "expected ~31 ms, got {:.3} ms",
        dp.root_disp_ms
    );
    // Holdover policy: a PPS-locked anchor keeps serving stratum 1 while the
    // modelled dispersion is below HOLDOVER_UNSYNC_US; clients see the growing
    // root dispersion and apply their own distance threshold.
    assert_eq!(dp.stratum, 1, "70 s outage must still serve stratum 1");
    assert_eq!(dp.state, DisciplineState::Holdover);
    assert_eq!(dp.leap, 0, "LI stays normal during holdover");
}

#[test]
fn holdover_serves_stratum_1_until_dispersion_reaches_1s() {
    // Boundary: BASE_DISP_US + n × RATE crosses HOLDOVER_UNSYNC_US at n = 1998.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 1_990 * MICROS_PER_SEC);
    let dp = server.discipline_params(true);
    assert_eq!(dp.stratum, 1, "just below the unsync threshold");
    assert_eq!(dp.state, DisciplineState::Holdover);
    assert!(
        dp.root_disp_ms < 1_000.0,
        "dispersion {:.1} ms must still be below 1 s",
        dp.root_disp_ms
    );
}

#[test]
fn holdover_declares_stratum_16_when_dispersion_exceeds_1s() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    let holdover_secs = 2_010_i64;
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + holdover_secs * MICROS_PER_SEC);
    let dp = server.discipline_params(true);
    assert_eq!(
        dp.stratum, 16,
        "should be stratum 16 when dispersion >= 1 s"
    );
    assert_eq!(dp.state, DisciplineState::Unsync);
    assert_eq!(dp.leap, 3);
}

#[test]
fn holdover_requires_pps_locked_anchor() {
    // An anchor that only ever came from NMEA/RTC (never PPS-disciplined)
    // must not be served as stratum 1, regardless of how fresh it is.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.seed_utc_seconds(1_700_000_000);
    let dp = server.discipline_params(false);
    assert_eq!(dp.stratum, 16);
    assert_eq!(dp.state, DisciplineState::Unsync);

    server.update_gps_utc_seconds(1_700_000_000);
    let dp = server.discipline_params(true);
    assert!(!server.pps_locked);
    assert_eq!(dp.stratum, 16, "NMEA-only anchor is not stratum 1");
    assert_eq!(dp.state, DisciplineState::Unsync);
}

#[test]
fn holdover_stratum_1_restored_after_pps_returns() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 3_000 * MICROS_PER_SEC);
    assert_eq!(server.discipline_params(true).stratum, 16);
    server.pps_now(Some(1_000_000));
    let dp = server.discipline_params(true);
    assert_eq!(
        dp.stratum, 1,
        "stratum should recover to 1 after PPS returns"
    );
}

#[test]
fn freq_correction_does_not_affect_zero_freq_ppm() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    assert_eq!(server.freq_ppm, 0.0);
    // current_ntp_timestamp should not panic or produce obviously wrong output.
    let ts = server.current_ntp_timestamp();
    assert_ne!(ts, 0);
}

// --- Item 2: NTP correctness field tests ---

#[test]
fn last_sync_ntp_ts_set_on_first_pulse() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    assert_eq!(server.last_sync_ntp_ts, 0);
    server.pps_now(None);
    // Reference timestamp is the top of the anchored second (fraction 0).
    let anchor_unix = server.clock_anchor.expect("anchor").unix_seconds;
    assert_eq!(
        server.last_sync_ntp_ts,
        ntp_timestamp_from_unix(anchor_unix, 0),
        "ref ts should be the disciplined time of the PPS edge"
    );
}

#[test]
fn last_sync_ntp_ts_updated_on_subsequent_pulse() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let base = monotonic_us_now() - 5 * MICROS_PER_SEC;
    server.nmea_for_edge(1_700_000_000, base);
    server.pps_at(None, base);
    let first_ref = server.last_sync_ntp_ts;
    server.nmea_for_edge(1_700_000_001, base + MICROS_PER_SEC);
    server.pps_at(Some(1_000_000), base + MICROS_PER_SEC);
    // After the second pulse, ref_ts should advance by approximately 1 NTP second.
    let delta_secs = ((server.last_sync_ntp_ts >> 32) as i64) - ((first_ref >> 32) as i64);
    assert_eq!(delta_secs, 1, "ref_ts should advance by 1 s per PPS pulse");
}

#[test]
fn build_response_uses_last_sync_ts_as_reference_timestamp() {
    // When synced, the reference timestamp in the NTP response should be the
    // last discipline event time (ref_ts), NOT the current receive time.
    let fixed_ref_ts = 0xE9D1_E7B4_0000_0000_u64;
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 0.1,
        ref_ts: fixed_ref_ts,
        current_ts: fixed_ref_ts + 0x1_0000_0000,
        precision: -10,
    };
    let req = [0_u8; NTP_PACKET_LEN];
    // Pass a different receive_ts so we can distinguish ref_ts from it.
    let receive_ts = fixed_ref_ts + 0x5_0000_0000;
    let resp = build_response(&req, &dp, receive_ts, MODE_SERVER);
    let ref_ts_in_resp = u64::from_be_bytes(resp[16..24].try_into().unwrap());
    assert_eq!(
        ref_ts_in_resp, fixed_ref_ts,
        "reference timestamp should be last_sync_ntp_ts, not receive_ts"
    );
}

#[test]
fn build_response_reference_timestamp_zero_when_unsynced() {
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        root_disp_short: 5 << 16,
        root_disp_ms: 5000.0,
        ref_ts: 0xDEAD_BEEF_0000_0000,
        current_ts: 0,
        precision: -10,
    };
    let req = [0_u8; NTP_PACKET_LEN];
    let resp = build_response(&req, &dp, 0x1234, MODE_SERVER);
    let ref_ts_in_resp = u64::from_be_bytes(resp[16..24].try_into().unwrap());
    assert_eq!(
        ref_ts_in_resp, 0,
        "reference timestamp must be 0 when stratum=16"
    );
}

#[test]
fn discipline_params_locked_dispersion_uses_phi_model() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    // Simulate jitter = 200 µs (above MIN_HW_ACCURACY_US = 100 µs).
    server.pps_jitter_us = 200.0;
    server.pps_has_sample = true;
    let dp = server.discipline_params(true);
    // Expected: max(200, 100) + PHI×0 ≈ 0.2 ms (PPS just fired, age≈0).
    // Dispersion should be well below the old fixed 1 ms.
    assert!(
        dp.root_disp_ms < 1.0,
        "locked dispersion should be < 1 ms with PHI model, got {:.3} ms",
        dp.root_disp_ms
    );
    assert!(
        dp.root_disp_ms >= 0.1,
        "locked dispersion should be >= MIN_HW_ACCURACY (0.1 ms), got {:.3} ms",
        dp.root_disp_ms
    );
}

#[test]
fn discipline_params_locked_dispersion_minimum_is_hw_floor() {
    // With no PPS sample yet, dispersion should equal the hardware floor.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    // pps_has_sample is still false after first pulse.
    server.pps_has_sample = false;
    let dp = server.discipline_params(true);
    assert!(
        (dp.root_disp_ms - (MIN_HW_ACCURACY_US as f64 / 1_000.0)).abs() < 0.02,
        "dispersion should be ~MIN_HW_ACCURACY when no jitter sample yet"
    );
}

// --- Item 3: mode-6 correctness field tests ---

#[test]
fn build_mode6_system_status_clksrc_is_gps_when_synced() {
    // RFC 1305 B.2.1: ClkSrc field must be 4 (UHF satellite radio) for a GPS reference.
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 0.1,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    let sys_status = u16::from_be_bytes([resp[4], resp[5]]);
    let clksrc = (sys_status >> 8) & 0x3f;
    assert_eq!(clksrc, 4, "ClkSrc should be 4 (UHF/GPS) per RFC 1305 B.2.1");
    let li = sys_status >> 14;
    assert_eq!(li, 0, "LI should be 0 (no warning) when synced");
}

#[test]
fn build_mode6_system_status_unsynced_has_li_alarm() {
    let dp = DisciplineParams {
        stratum: 16,
        state: DisciplineState::Unsync,
        leap: 3,
        root_disp_short: 5 << 16,
        root_disp_ms: 5000.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    let sys_status = u16::from_be_bytes([resp[4], resp[5]]);
    let li = sys_status >> 14;
    assert_eq!(li, 3, "LI should be 3 (alarm) when unsynced");
}

#[test]
fn build_mode6_readstat_peer_status_sel_is_system_peer_when_synced() {
    // sel=6 in the peer status word causes ntpq to display '*' (system peer).
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 0.1,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 0.0, 0.0, 0.0));
    // READSTAT payload: 2-byte assoc ID + 2-byte peer status.
    let peer_status = u16::from_be_bytes([
        resp[NTP_CONTROL_HEADER_LEN + 2],
        resp[NTP_CONTROL_HEADER_LEN + 3],
    ]);
    let sel = peer_status >> 13;
    assert_eq!(sel, 6, "peer sel should be 6 (system peer) when synced");
}

#[test]
fn build_mode6_readvar_system_includes_reftime_and_frequency() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 0.1,
        ref_ts: 0xE9D1_E7B4_8A3D_71A0,
        current_ts: 0xE9D1_E7B5_0000_0000,
        precision: -10,
    };
    let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let resp = build_mode6_response(&req, 4, &dp, &stats(500, 200.0, 1000.0, 5.321));
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    assert!(
        payload.contains("reftime=0x"),
        "should include reftime in hex"
    );
    assert!(payload.contains("clock=0x"), "should include clock in hex");
    assert!(payload.contains("frequency="), "should include frequency");
    assert!(payload.contains("sys_jitter="), "should include sys_jitter");
    assert!(
        payload.contains("rootdelay=0.000"),
        "root delay should be 0"
    );
}

#[test]
fn build_mode6_readvar_peer_includes_dispersion_and_filter_vars() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 16,
        root_disp_ms: 0.5,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [
        0,
        MODE6_OPCODE_READVAR,
        0,
        0,
        0,
        0,
        0,
        MODE6_ASSOC_ID as u8,
        0,
        0,
        0,
        0,
    ];
    let resp = build_mode6_response(&req, 4, &dp, &stats(250, 100.0, 800.0, 3.0));
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    assert!(payload.contains("dispersion="), "should include dispersion");
    assert!(payload.contains("xleave="), "should include xleave");
    assert!(payload.contains("filtdelay="), "should include filtdelay");
    assert!(payload.contains("filtoffset="), "should include filtoffset");
    assert!(payload.contains("filtdisp="), "should include filtdisp");
}

#[test]
fn ntp_ts_to_mode6_formats_correctly() {
    // 0xE9D1E7B48A3D71A0 → "0xE9D1E7B4.8A3D71A0"
    let ts: u64 = 0xE9D1_E7B4_8A3D_71A0;
    let s = ntp_ts_to_mode6(ts);
    assert_eq!(s, "0xE9D1E7B4.8A3D71A0");
}

#[test]
fn ntp_ts_to_mode6_zero_is_epoch() {
    assert_eq!(ntp_ts_to_mode6(0), "0x00000000.00000000");
}

// --- Item 4: service protection tests (integration) ---
// Unit tests for Acl and RateLimiter live in ntp::protection.
// The tests below cover end-to-end behavior through NtpServer::poll().    // -- KoD packet tests --

#[test]
fn build_kod_response_has_stratum_0_and_rate_refid() {
    let mut req = [0_u8; NTP_PACKET_LEN];
    // Simulate client transmit timestamp in bytes 40-47.
    req[40..48].copy_from_slice(&0x1234_5678_ABCD_EF00_u64.to_be_bytes());
    let resp = build_kod_response(&req, 4, MODE_SERVER);
    // Stratum must be 0 (kiss packet).
    assert_eq!(resp[1], 0, "stratum should be 0");
    // Reference ID must be "RATE".
    assert_eq!(&resp[12..16], b"RATE", "refid should be RATE");
    // LI = 3 (alarm).
    assert_eq!((resp[0] >> 6) & 0x03, 3, "LI should be 3 (alarm)");
    // Mode = 4 (server).
    assert_eq!(resp[0] & 0x07, 4, "mode should be 4");
    // Originate = client's transmit timestamp.
    assert_eq!(
        &resp[24..32],
        &req[40..48],
        "originate should echo client transmit ts"
    );
}

// -- poll() integration tests for service protections --

#[test]
fn poll_sends_kod_when_client_polls_too_fast() {
    // Bind a server socket and configure a client that will poll twice in
    // rapid succession. The second poll should receive a KoD RATE response.
    let mut server = NtpServer::new_for_test().expect("server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    // Build a minimal mode-3 NTP request packet.
    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3; // VN=4, Mode=3

    // First request — should be served normally.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let mut buf = [0_u8; 64];
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_ne!(
        &buf[12..16],
        b"RATE",
        "first response should not be a kiss packet"
    );

    // Second request immediately after (no delay) — should get KoD.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_eq!(&buf[12..16], b"RATE", "second response should be a KoD");
    assert_eq!(buf[1], 0, "KoD stratum must be 0");
    assert_eq!(&buf[12..16], b"RATE", "KoD refid should be RATE");
    assert_eq!(
        server.rate_limited_total, 1,
        "rate_limited counter should increment"
    );
}

#[test]
fn poll_sends_kod_when_symmetric_active_polls_too_fast() {
    let mut server = NtpServer::new_for_test().expect("server");
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | MODE_SYMMETRIC_ACTIVE; // VN=4, Mode=1

    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let mut buf = [0_u8; 64];
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_ne!(
        &buf[12..16],
        b"RATE",
        "first response should not be a kiss packet"
    );
    assert_eq!(
        buf[0] & 0x07,
        MODE_SYMMETRIC_PASSIVE,
        "mode-1 request must be answered in symmetric-passive mode (RFC 5905 §9.2)"
    );

    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_eq!(&buf[12..16], b"RATE", "second response should be a KoD");
    assert_eq!(buf[1], 0, "KoD stratum must be 0");
    assert_eq!(&buf[12..16], b"RATE", "KoD refid should be RATE");
    assert_eq!(
        buf[0] & 0x07,
        MODE_SYMMETRIC_PASSIVE,
        "KoD to a mode-1 request must also use mode 2"
    );
    assert_eq!(server.rate_limited_total, 1);
}

/// RFC 5905 §9.2 Table 10: packets in server (4), symmetric-passive (2),
/// broadcast (5), reserved (0) and private (7) modes carry no request and
/// must be discarded — answering a mode-4 packet is a reflection loop.
#[test]
fn poll_discards_non_request_modes() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();

    for mode in [0_u8, 2, 4, 5, 7] {
        let mut server = NtpServer::new_for_test().expect("server");
        server.update_gps_utc_seconds(1_700_000_000);
        server.pps_now(None);
        let server_addr = server.socket.local_addr().unwrap();

        let mut req = [0_u8; NTP_PACKET_LEN];
        req[0] = (4 << 3) | mode;
        client.send_to(&req, server_addr).unwrap();
        server.poll(true).unwrap();

        let mut buf = [0_u8; 64];
        assert!(
            client.recv_from(&mut buf).is_err(),
            "mode-{mode} packet must not receive a reply"
        );
        assert_eq!(server.served(), 0, "mode-{mode} must not count as served");
        assert_eq!(
            server.rate_limited_total, 0,
            "mode-{mode} must not touch the rate limiter"
        );
    }
}

/// Truncated datagrams must be dropped without a reply: a 47-byte mode-3
/// packet, a 1-byte packet, and an 11-byte mode-6 header are all too short.
#[test]
fn poll_drops_short_packets_without_reply() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let mut server = NtpServer::new_for_test().expect("server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let server_addr = server.socket.local_addr().unwrap();

    let mut short_mode3 = vec![0_u8; NTP_PACKET_LEN - 1];
    short_mode3[0] = (4 << 3) | MODE_CLIENT;
    let one_byte = vec![(4 << 3) | MODE_CLIENT];
    let mut short_mode6 = vec![0_u8; NTP_CONTROL_HEADER_LEN - 1];
    short_mode6[0] = (4 << 3) | MODE_CONTROL;
    short_mode6[1] = MODE6_OPCODE_READSTAT;

    for (label, pkt) in [
        ("47-byte mode-3", short_mode3),
        ("1-byte", one_byte),
        ("11-byte mode-6", short_mode6),
    ] {
        client.send_to(&pkt, server_addr).unwrap();
        server.poll(true).unwrap();
        let mut buf = [0_u8; 64];
        assert!(
            client.recv_from(&mut buf).is_err(),
            "{label} packet must not receive a reply"
        );
    }
    assert_eq!(server.served(), 0);
    assert_eq!(
        server.rate_limited_total, 0,
        "drops happen before the limiter"
    );
}

/// A mode-6 packet with the response bit set is a reply, not a request
/// (RFC 1305 B.1); answering it would allow reflection between servers.
#[test]
fn poll_ignores_mode6_packets_with_response_bit() {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let mut server = NtpServer::new_for_test().expect("server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = make_mode6_req(MODE6_OPCODE_READVAR, 0);
    req[1] |= MODE6_RESPONSE_BIT;
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let mut buf = [0_u8; 512];
    assert!(
        client.recv_from(&mut buf).is_err(),
        "must not reply to a response"
    );
    assert_eq!(server.served(), 0);
    assert_eq!(server.rate_limited_total, 0, "dropped before the limiter");

    // The same packet without the response bit is answered normally.
    req[1] &= !MODE6_RESPONSE_BIT;
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let (n, _) = client
        .recv_from(&mut buf)
        .expect("genuine request is served");
    assert!(n > NTP_CONTROL_HEADER_LEN);
    assert_eq!(buf[1] & MODE6_RESPONSE_BIT, MODE6_RESPONSE_BIT);
    assert_eq!(server.served(), 1);
}

/// Pre-1970 instants cannot be represented relative to the Unix epoch in
/// this code path; they clamp to the NTP era boundary rather than wrapping.
#[test]
fn ntp_timestamp_from_unix_clamps_negative_seconds() {
    assert_eq!(
        ntp_timestamp_from_unix(-1, 0) >> 32,
        NTP_UNIX_EPOCH_OFFSET_SECS
    );
    assert_eq!(
        ntp_timestamp_from_unix(0, 0) >> 32,
        NTP_UNIX_EPOCH_OFFSET_SECS
    );
    assert_eq!(
        ntp_timestamp_from_unix(0, 500_000) & 0xFFFF_FFFF,
        0x8000_0000
    );
}

/// The UI's initial snapshot must describe an unsynchronised server.
#[test]
fn ntp_snapshot_default_is_unsynchronised() {
    let snap = NtpSnapshot::default();
    assert_eq!(snap.stratum, 16);
    assert_eq!(snap.state, DisciplineState::Unsync);
    assert_eq!(snap.served, 0);
    assert!(!snap.proc_delay_has_sample);
}

/// A mode-3 request is answered in mode 4 and a mode-1 request in mode 2,
/// at the packet-builder level, independent of the socket path.
#[test]
fn build_response_reply_mode_follows_request_mode() {
    let dp = synced_dp();
    let req = [0_u8; NTP_PACKET_LEN];
    assert_eq!(
        build_response(&req, &dp, 0, MODE_SERVER)[0] & 0x07,
        MODE_SERVER
    );
    assert_eq!(
        build_response(&req, &dp, 0, MODE_SYMMETRIC_PASSIVE)[0] & 0x07,
        MODE_SYMMETRIC_PASSIVE
    );
    assert_eq!(
        build_kod_response(&req, 4, MODE_SYMMETRIC_PASSIVE)[0] & 0x07,
        MODE_SYMMETRIC_PASSIVE
    );
}

#[test]
fn poll_rate_limits_across_time_request_modes() {
    let mut server = NtpServer::new_for_test().expect("server");
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut mode3 = [0_u8; NTP_PACKET_LEN];
    mode3[0] = (4 << 3) | 3;
    let mut mode1 = [0_u8; NTP_PACKET_LEN];
    mode1[0] = (4 << 3) | 1;

    client.send_to(&mode3, server_addr).unwrap();
    server.poll(true).unwrap();
    let mut buf = [0_u8; 64];
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_ne!(&buf[12..16], b"RATE", "first request must be served");

    client.send_to(&mode1, server_addr).unwrap();
    server.poll(true).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_eq!(
        buf[1], 0,
        "mode-1 after mode-3 should share limiter and get KoD"
    );
    assert_eq!(&buf[12..16], b"RATE");
    assert_eq!(server.rate_limited_total, 1);
}

#[test]
fn poll_drops_packet_from_acl_blocked_source() {
    let mut server = NtpServer::new_for_test().expect("server");
    // Configure a deny-all ACL so even loopback is blocked.
    server.set_acl(Acl::deny_all());

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3; // VN=4, Mode=3

    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();

    // The packet should be silently dropped — no response.
    let mut buf = [0_u8; 64];
    let result = client.recv_from(&mut buf);
    assert!(
        result.is_err(),
        "ACL-blocked packet should not receive a response"
    );
    assert_eq!(
        server.acl_blocked_total, 1,
        "acl_blocked counter should increment"
    );
}

#[test]
fn poll_allows_loopback_with_private_lan_acl() {
    let mut server = NtpServer::new_for_test().expect("server");
    server.set_acl(Acl::private_lan());
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    client.send_to(&client_ntp_request(), server_addr).unwrap();
    server.poll(true).unwrap();

    let mut buf = [0_u8; NTP_PACKET_LEN];
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_eq!(server.acl_blocked_total, 0);
}

#[test]
fn poll_blocks_loopback_outside_configured_cidr() {
    let mut server = NtpServer::new_for_test().expect("server");
    server.set_acl(Acl::from_config("192.168.1.0/24"));

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    client.send_to(&client_ntp_request(), server_addr).unwrap();
    server.poll(true).unwrap();

    let mut buf = [0_u8; NTP_PACKET_LEN];
    assert!(
        client.recv_from(&mut buf).is_err(),
        "127.0.0.1 should be blocked by 192.168.1.0/24 ACL"
    );
    assert_eq!(server.acl_blocked_total, 1);
}

#[test]
fn poll_mode6_rate_limited_on_rapid_poll() {
    let mut server = NtpServer::new_for_test().expect("server");
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_CONTROL_HEADER_LEN];
    req[0] = (4 << 3) | 6; // VN=4, Mode=6
    req[1] = MODE6_OPCODE_READVAR;

    let mut buf = [0_u8; 256];
    client.send_to(&req, server_addr).unwrap();
    server.poll(false).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert!(
        n >= NTP_CONTROL_HEADER_LEN,
        "first mode-6 request should get a response"
    );

    client.send_to(&req, server_addr).unwrap();
    server.poll(false).unwrap();
    assert!(
        client.recv_from(&mut buf).is_err(),
        "rapid mode-6 poll should be silently dropped"
    );
    assert_eq!(
        server.rate_limited_total, 1,
        "mode-6 should increment rate_limited counter"
    );
}

#[test]
fn poll_mode6_allowed_again_after_rate_limit_window() {
    let mut server = NtpServer::new_for_test().expect("server");
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_CONTROL_HEADER_LEN];
    req[0] = (4 << 3) | 6;
    req[1] = MODE6_OPCODE_READVAR;

    client.send_to(&req, server_addr).unwrap();
    server.poll(false).unwrap();
    let mut buf = [0_u8; 256];
    let _ = client.recv_from(&mut buf).unwrap();

    client.send_to(&req, server_addr).unwrap();
    server.poll(false).unwrap();
    assert!(client.recv_from(&mut buf).is_err());

    server
        .rate_limiter
        .subtract_time(protection::MIN_POLL_INTERVAL_US + 1);

    client.send_to(&req, server_addr).unwrap();
    server.poll(false).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert!(
        n >= NTP_CONTROL_HEADER_LEN,
        "mode-6 should be served again after rate-limit window"
    );
}

/// KoD response must echo the client's transmit timestamp in the originate
/// field (bytes 24-31 = client bytes 40-47) per RFC 5905 §7.4.
#[test]
fn poll_kod_response_echoes_client_transmit_timestamp() {
    let mut server = NtpServer::new_for_test().expect("server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3;
    // Write a distinct transmit timestamp into the request.
    let client_xmit: u64 = 0xDEAD_BEEF_CAFE_1234;
    req[40..48].copy_from_slice(&client_xmit.to_be_bytes());
    let mut buf = [0_u8; 64];

    // First request — accepted normally (consume the slot so next is KoD).
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    client.recv_from(&mut buf).unwrap();

    // Second immediate request — should receive KoD RATE.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_eq!(buf[1], 0, "KoD stratum must be 0");
    assert_eq!(&buf[12..16], b"RATE", "KoD refid must be RATE");
    let originate = u64::from_be_bytes(buf[24..32].try_into().unwrap());
    assert_eq!(
        originate, client_xmit,
        "KoD originate must echo client transmit timestamp (RFC 5905 §7.4)"
    );
}

/// After a client is rate-limited, the rate-limited counter must be visible
/// through `ntp_snapshot` so the UI and operator can observe it.
#[test]
fn poll_rate_limited_counter_visible_in_snapshot() {
    let mut server = NtpServer::new_for_test().expect("server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3;
    let mut buf = [0_u8; 64];

    // First request — allowed.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    client.recv_from(&mut buf).unwrap();

    // Second request immediately — triggers KoD + increments counter.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    client.recv_from(&mut buf).unwrap();

    let snap = server.ntp_snapshot(true);
    assert_eq!(
        snap.rate_limited, 1,
        "rate_limited must be 1 in NtpSnapshot"
    );
    // KoD responses do not increment served; only the first (allowed) request does.
    assert_eq!(
        snap.served, 1,
        "served should count only the allowed response"
    );
}

/// After a KoD is sent the rate limiter must NOT update `last_us`, so the
/// client is allowed again once `MIN_POLL_INTERVAL_US` has elapsed since the
/// original accepted request.
#[test]
fn poll_client_allowed_again_after_rate_limit_window() {
    let mut server = NtpServer::new_for_test().expect("server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);

    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let server_addr = server.socket.local_addr().unwrap();

    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3;
    let mut buf = [0_u8; 64];

    // First request — allowed.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    client.recv_from(&mut buf).unwrap();

    // Second request immediately — KoD.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    client.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[12..16], b"RATE", "second response should be KoD");

    // Advance the rate limiter's clock past the min poll interval by
    // winding back every entry's last_us without real sleeps.
    server
        .rate_limiter
        .subtract_time(protection::MIN_POLL_INTERVAL_US + 1);

    // Third request after the window — should be served normally again.
    client.send_to(&req, server_addr).unwrap();
    server.poll(true).unwrap();
    let (n, _) = client.recv_from(&mut buf).unwrap();
    assert_eq!(n, NTP_PACKET_LEN);
    assert_ne!(
        &buf[12..16],
        b"RATE",
        "third response should be a normal NTP reply"
    );
    assert_eq!(
        server.rate_limited_total, 1,
        "only one KoD should have been sent"
    );
}

// -- Leap indicator tests --

#[test]
fn set_leap_indicator_propagates_to_ntp_response() {
    // LI=1 set by the application must appear in NTP response byte 0 bits 7-6.
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 1, // simulating pending_leap=1 flowing through discipline_params
        root_disp_short: 1 << 16,
        root_disp_ms: 0.1,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let req = [0_u8; NTP_PACKET_LEN];
    let resp = build_response(&req, &dp, 0, MODE_SERVER);
    let li = (resp[0] >> 6) & 0x03;
    assert_eq!(li, 1, "LI=1 should propagate to response byte 0 bits 7-6");
}

#[test]
fn set_leap_indicator_clamped_to_2() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.set_leap_indicator(5);
    assert_eq!(server.pending_leap, 2, "values > 2 should be clamped to 2");
}

#[test]
fn set_leap_indicator_0_clears_warning() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.set_leap_indicator(1);
    assert_eq!(server.pending_leap, 1);
    server.set_leap_indicator(0);
    assert_eq!(server.pending_leap, 0);
}

#[test]
fn discipline_params_uses_pending_leap_when_synced() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.set_leap_indicator(1);
    let dp = server.discipline_params(true);
    assert_eq!(
        dp.leap, 1,
        "pending_leap should flow into DisciplineParams.leap"
    );
}

#[test]
fn discipline_params_overrides_pending_leap_with_alarm_when_unsynced() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.set_leap_indicator(1); // would be active if synced
    let dp = server.discipline_params(false); // no anchor → stratum 16
    assert_eq!(
        dp.leap, 3,
        "unsynced server must emit LI=3 regardless of pending_leap"
    );
}

#[test]
fn ntp_snapshot_exposes_leap_indicator() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.set_leap_indicator(2);
    let snap = server.ntp_snapshot(true);
    assert_eq!(snap.leap_indicator, 2);
}

// -- PPS outlier filter tests --

#[test]
fn pps_outlier_rejected_after_servo_converged() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    // Give the servo a good first sample to establish pps_has_sample=true.
    server.pps_now(Some(1_000_100)); // +100 µs error — OK

    let freq_before = server.freq_ppm;
    // Now send a huge phase error that exceeds PPS_OUTLIER_THRESHOLD_US (50 ms).
    server.pps_now(Some(1_100_000)); // +100_000 µs = 100 ms — outlier
    assert_eq!(
        server.pps_glitch_count, 1,
        "outlier should increment pps_glitch_count"
    );
    assert!(
        (server.freq_ppm - freq_before).abs() < 0.001,
        "freq_ppm should not change on outlier rejection"
    );
}

#[test]
fn pps_outlier_not_applied_before_first_sample() {
    // Before pps_has_sample is true (first disciplined pulse), large phase
    // errors are allowed through so the servo can make an initial correction.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    // pps_has_sample is still false; a large interval is still gated by the
    // ±20% interval filter (800k–1200k µs).  Use a mid-range value.
    server.pps_now(Some(1_049_999)); // 49_999 µs error — just under threshold
    assert_eq!(
        server.pps_glitch_count, 0,
        "should not be counted as outlier"
    );
    assert!(
        server.pps_has_sample,
        "servo should accept the first large-ish error"
    );
}

#[test]
fn pps_outlier_does_not_update_last_pps_monotonic() {
    // A rejected outlier must not advance last_pps_monotonic_us; otherwise
    // the holdover timer would be reset by bad pulses.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.pps_now(Some(1_000_100)); // good → sets last_pps_monotonic_us
    let last_good = server.last_pps_monotonic_us;
    server.pps_now(Some(1_100_000)); // outlier
    assert_eq!(
        server.last_pps_monotonic_us, last_good,
        "outlier must not update last_pps_monotonic_us"
    );
}

// -- Stale GPS anchor guard tests --

#[test]
fn first_pps_pulse_skipped_when_gps_time_is_stale() {
    // If update_gps_utc_seconds was not called recently, the first PPS pulse
    // must not anchor the clock to old GPS data.
    let mut server = NtpServer::new_for_test().expect("test socket");
    // Set GPS UTC seconds but do NOT call update_gps_utc_seconds via the
    // normal path — instead manipulate internal state to make GPS appear stale.
    server.last_gps_utc_seconds = Some(1_700_000_000);
    server.last_gps_utc_update_us = Some(monotonic_us_now() - 2 * GPS_STALE_THRESHOLD_US);
    server.pps_now(None);
    assert!(
        server.clock_anchor.is_none(),
        "stale GPS should prevent phase anchor on first PPS pulse"
    );
}

#[test]
fn first_pps_pulse_anchors_with_fresh_gps() {
    // With a recent GPS update the first PPS pulse should establish the anchor.
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000); // fresh
    server.pps_now(None);
    assert!(
        server.clock_anchor.is_some(),
        "fresh GPS should allow phase anchor on first PPS pulse"
    );
}

#[test]
fn glitch_count_exposed_in_ntp_snapshot() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    server.pps_now(Some(1_000_100)); // good
    server.pps_now(Some(1_100_000)); // outlier
    let snap = server.ntp_snapshot(true);
    assert_eq!(snap.pps_glitch_count, 1);
}

// --- Item 6: Timestamp math ---

/// The NTP–Unix epoch offset is exactly the number of seconds from
/// 1900-01-01T00:00:00Z to 1970-01-01T00:00:00Z as specified in RFC 868.
#[test]
fn ntp_epoch_offset_matches_rfc868_definition() {
    // 70 years * 365.25 days/year * 86400 s/day = 2_208_988_800 s
    assert_eq!(NTP_UNIX_EPOCH_OFFSET_SECS, 2_208_988_800_u64);
}

/// With the monotonic clock frozen, the disciplined timestamp is a pure
/// function of the anchor and can be checked bit-exactly: 500 000 µs after
/// the anchor must yield fraction 0x8000_0000, 250 000 µs → 0x4000_0000, and
/// crossing the second boundary must carry into the seconds field.
#[test]
fn ntp_fraction_encodes_exactly_with_frozen_clock() {
    let anchor_us = 10_000 * MICROS_PER_SEC;
    let clock = test_clock::freeze(anchor_us);
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.clock_anchor = Some(ClockAnchor {
        unix_seconds: 1_700_000_000,
        monotonic_us: anchor_us,
    });
    server.freq_ppm = 0.0;
    let ntp_secs = 1_700_000_000 + NTP_UNIX_EPOCH_OFFSET_SECS;

    assert_eq!(server.current_ntp_timestamp(), ntp_secs << 32);

    clock.set(anchor_us + 500_000);
    assert_eq!(
        server.current_ntp_timestamp(),
        (ntp_secs << 32) | 0x8000_0000
    );

    clock.set(anchor_us + 250_000);
    assert_eq!(
        server.current_ntp_timestamp(),
        (ntp_secs << 32) | 0x4000_0000
    );

    clock.set(anchor_us + 1_750_000);
    assert_eq!(
        server.current_ntp_timestamp(),
        ((ntp_secs + 1) << 32) | 0xC000_0000,
        "elapsed 1.75 s must carry one second and leave a 0.75 fraction"
    );

    // One microsecond resolves to 4295 NTP fraction units (2^32 / 1e6).
    clock.set(anchor_us + 1);
    assert_eq!(server.current_ntp_timestamp(), (ntp_secs << 32) | 4294);
}

/// Exact check of the frequency correction: at +100 ppm, 1 000 000 µs of raw
/// elapsed time is 100 µs of oscillator gain, so disciplined time reads
/// 999 900 µs after the anchor (fraction = 0.9999 s), not a full second.
#[test]
fn ntp_frequency_correction_is_exact_with_frozen_clock() {
    let anchor_us = 10_000 * MICROS_PER_SEC;
    let clock = test_clock::freeze(anchor_us + MICROS_PER_SEC);
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.clock_anchor = Some(ClockAnchor {
        unix_seconds: 1_700_000_000,
        monotonic_us: anchor_us,
    });
    server.freq_ppm = 100.0;
    let ts = server.current_ntp_timestamp();
    let ntp_secs = 1_700_000_000 + NTP_UNIX_EPOCH_OFFSET_SECS;
    assert_eq!(
        ts >> 32,
        ntp_secs,
        "must not yet have rolled to the next second"
    );
    let frac_us = (ts & 0xFFFF_FFFF) * 1_000_000 / (1u64 << 32);
    assert_eq!(frac_us, 999_899, "999 900 µs minus one unit of truncation");

    // Negative ppm (slow oscillator) stretches elapsed time instead.
    server.freq_ppm = -100.0;
    let ts = server.current_ntp_timestamp();
    assert_eq!(ts >> 32, ntp_secs + 1);
    let frac_us = (ts & 0xFFFF_FFFF) * 1_000_000 / (1u64 << 32);
    assert_eq!(frac_us, 99, "100 µs into the next second (truncated)");
    drop(clock);
}

/// The mode-6 `clock=` variable must be the disciplined timestamp, so a
/// frozen clock lets us check the rendered hex exactly.
#[test]
fn mode6_clock_variable_matches_disciplined_timestamp_exactly() {
    let anchor_us = 10_000 * MICROS_PER_SEC;
    let _clock = test_clock::freeze(anchor_us + 500_000);
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.clock_anchor = Some(ClockAnchor {
        unix_seconds: 1_700_000_000,
        monotonic_us: anchor_us,
    });
    server.pps_locked = true;
    server.last_pps_monotonic_us = Some(anchor_us);
    server.last_sync_ntp_ts = ntp_timestamp_from_unix(1_700_000_000, 0);
    let dp = server.discipline_params(true);
    assert_eq!(dp.stratum, 1);
    let req = make_mode6_req(MODE6_OPCODE_READVAR, 0);
    let resp = build_mode6_response(&req, 4, &dp, &server.servo_stats());
    let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
    let ntp_secs = (1_700_000_000 + NTP_UNIX_EPOCH_OFFSET_SECS) as u32;
    assert!(
        payload.contains(&format!("reftime=0x{ntp_secs:08X}.00000000,")),
        "{payload}"
    );
    assert!(
        payload.contains(&format!("clock=0x{ntp_secs:08X}.80000000,")),
        "{payload}"
    );
}

/// With a positive `freq_ppm` the disciplined clock should report a
/// smaller elapsed interval than the raw monotonic elapsed.  A large
/// correction (50 000 ppm = 5%) creates a 25 ms difference over 500 ms,
/// which dwarfs any test-executor scheduling overhead.
#[test]
fn ntp_frequency_correction_compensates_fast_clock() {
    let elapsed_us: i64 = 500_000;
    let now_us = monotonic_us_now();
    let anchor = ClockAnchor {
        unix_seconds: 1_700_000_000,
        monotonic_us: now_us - elapsed_us,
    };

    let mut server_uncorrected = NtpServer::new_for_test().expect("test socket");
    server_uncorrected.clock_anchor = Some(anchor);
    server_uncorrected.freq_ppm = 0.0;
    let ts_uncorrected = server_uncorrected.current_ntp_timestamp();

    // Use a large correction so the effect (25 ms) overwhelms any jitter.
    let mut server_fast = NtpServer::new_for_test().expect("test socket");
    server_fast.clock_anchor = Some(anchor);
    server_fast.freq_ppm = 50_000.0; // 5% fast: corrected elapsed ≈ 475 ms
    let ts_fast = server_fast.current_ntp_timestamp();

    assert!(
        ts_fast < ts_uncorrected,
        "positive freq_ppm should reduce elapsed: ts_fast=0x{ts_fast:016X} ts_uncorrected=0x{ts_uncorrected:016X}"
    );
}

/// The NTP seconds field extracted from a disciplined timestamp must match
/// the anchor's Unix epoch plus the NTP–Unix offset for zero elapsed time.
#[test]
fn ntp_timestamp_seconds_field_reflects_anchor_unix_epoch() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    let unix_sec: i64 = 1_700_000_100;
    let now_us = monotonic_us_now();
    // Set anchor to exactly now so elapsed ≈ 0.
    server.clock_anchor = Some(ClockAnchor {
        unix_seconds: unix_sec,
        monotonic_us: now_us,
    });
    server.freq_ppm = 0.0;
    let ts = server.current_ntp_timestamp();
    let ntp_sec = ts >> 32;
    let expected = unix_sec as u64 + NTP_UNIX_EPOCH_OFFSET_SECS;
    assert_eq!(
        ntp_sec, expected,
        "NTP seconds should be unix_seconds + epoch offset"
    );
}

// --- Item 6: Sync state transitions ---

#[test]
fn ntp_snapshot_state_locked_with_gps_fix_and_fresh_pps() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let snap = server.ntp_snapshot(true);
    assert_eq!(snap.state, DisciplineState::Locked);
    assert_eq!(snap.stratum, 1);
}

#[test]
fn ntp_snapshot_state_holdover_when_gps_fix_lost() {
    // Anchor + fresh PPS, but gps_fix = false → Holdover (not fully_synced).
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    let snap = server.ntp_snapshot(false); // GPS fix gone
    assert_eq!(snap.state, DisciplineState::Holdover);
}

#[test]
fn ntp_snapshot_state_holdover_when_pps_becomes_stale() {
    let mut server = NtpServer::new_for_test().expect("test socket");
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 1_000_000);
    let snap = server.ntp_snapshot(true);
    assert_eq!(snap.state, DisciplineState::Holdover);
}

#[test]
fn ntp_snapshot_state_unsync_without_any_anchor() {
    let server = NtpServer::new_for_test().expect("test socket");
    let snap = server.ntp_snapshot(false);
    assert_eq!(snap.state, DisciplineState::Unsync);
}

/// Full state-machine cycle: Unsync → Locked → Holdover (GPS lost) →
/// Holdover (PPS stale) → Locked again (PPS + GPS return).
#[test]
fn sync_state_full_cycle() {
    let mut server = NtpServer::new_for_test().expect("test socket");

    // Initially unsync.
    assert_eq!(server.ntp_snapshot(false).state, DisciplineState::Unsync);

    // Establish GPS + PPS → Locked.
    server.update_gps_utc_seconds(1_700_000_000);
    server.pps_now(None);
    assert_eq!(server.ntp_snapshot(true).state, DisciplineState::Locked);

    // GPS fix lost → Holdover (anchor still valid).
    assert_eq!(server.ntp_snapshot(false).state, DisciplineState::Holdover);

    // GPS fix returns; still has anchor → Locked again.
    assert_eq!(server.ntp_snapshot(true).state, DisciplineState::Locked);

    // PPS becomes stale → Holdover even with GPS fix.
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 1_000_000);
    let snap = server.ntp_snapshot(true);
    assert_eq!(snap.state, DisciplineState::Holdover);
    assert_eq!(snap.stratum, 1, "holdover keeps serving stratum 1");

    // Holdover window exhausted (dispersion ≥ 1 s) → Unsync, stratum 16.
    server.pps_stale_ago(None, HOLDOVER_ENTRY_US + 2_100 * MICROS_PER_SEC);
    let snap = server.ntp_snapshot(true);
    assert_eq!(snap.state, DisciplineState::Unsync);
    assert_eq!(snap.stratum, 16);
    assert_eq!(snap.leap_indicator, 3);

    // PPS returns (new pulse) → Locked.
    server.pps_now(Some(1_000_000));
    assert_eq!(server.ntp_snapshot(true).state, DisciplineState::Locked);
}

// --- Item 6: Mode-6 framing and padding ---

fn make_mode6_req(opcode: u8, assoc: u16) -> Vec<u8> {
    let mut req = vec![0_u8; NTP_CONTROL_HEADER_LEN];
    req[0] = (4 << 3) | 6; // VN=4, mode=6
    req[1] = opcode;
    req[6..8].copy_from_slice(&assoc.to_be_bytes());
    req
}

fn synced_dp() -> DisciplineParams {
    DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 1 << 14,
        root_disp_ms: 0.1,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    }
}

/// All mode-6 responses must be padded to a multiple of 4 bytes.
#[test]
fn build_mode6_response_length_is_multiple_of_4() {
    let req = make_mode6_req(MODE6_OPCODE_READVAR, 0);
    let dp = synced_dp();
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 1.0, 1.0, 0.0));
    assert_eq!(
        resp.len() % 4,
        0,
        "mode-6 response length {} is not 32-bit aligned",
        resp.len()
    );
}

/// Padding bytes appended for 32-bit alignment must all be zero.
#[test]
fn build_mode6_padding_bytes_are_zero() {
    let req = make_mode6_req(MODE6_OPCODE_READVAR, 0);
    let dp = synced_dp();
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 1.0, 1.0, 0.0));
    // The count field (bytes 10-11) tells us how many payload bytes precede padding.
    let count = u16::from_be_bytes([resp[10], resp[11]]) as usize;
    let payload_end = NTP_CONTROL_HEADER_LEN + count;
    for (i, &b) in resp[payload_end..].iter().enumerate() {
        assert_eq!(b, 0, "padding byte at index {i} should be zero");
    }
}

/// Byte 1 of every mode-6 response must have the response bit (0x80) set.
#[test]
fn build_mode6_response_bit_set_in_byte_1() {
    for opcode in [MODE6_OPCODE_READSTAT, MODE6_OPCODE_READVAR] {
        let req = make_mode6_req(opcode, 0);
        let dp = synced_dp();
        let resp = build_mode6_response(&req, 4, &dp, &stats(0, 1.0, 1.0, 0.0));
        assert_ne!(
            resp[1] & 0x80,
            0,
            "response bit not set for opcode {opcode}"
        );
    }
}

/// An unknown association ID must produce a 12-byte header-only response
/// (no payload, no padding).
#[test]
fn build_mode6_unknown_assoc_id_returns_header_only() {
    let req = make_mode6_req(MODE6_OPCODE_READVAR, 99); // assoc 99 not registered
    let dp = synced_dp();
    let resp = build_mode6_response(&req, 4, &dp, &stats(0, 1.0, 1.0, 0.0));
    // count field must be zero (bytes 10-11)
    let count = u16::from_be_bytes([resp[10], resp[11]]);
    assert_eq!(count, 0, "unknown assoc should return zero count");
    assert_eq!(
        resp.len(),
        NTP_CONTROL_HEADER_LEN,
        "unknown assoc should return header-only response"
    );
}

/// The Originate Timestamp field (bytes 24-31) of a mode-3/4 response must
/// be a verbatim copy of the Transmit Timestamp field (bytes 40-47) from
/// the client request, as required by RFC 5905 §7.3.
#[test]
fn build_response_originate_echoes_client_transmit() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 0,
        root_disp_ms: 0.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let mut req = [0_u8; NTP_PACKET_LEN];
    // Write a known transmit timestamp into bytes 40-47 of the request.
    let client_xmit: u64 = 0xDEAD_BEEF_1234_5678;
    req[40..48].copy_from_slice(&client_xmit.to_be_bytes());
    let resp = build_response(&req, &dp, 0, MODE_SERVER);
    let originate = u64::from_be_bytes(resp[24..32].try_into().unwrap());
    assert_eq!(
        originate, client_xmit,
        "originate timestamp must echo client transmit timestamp"
    );
}

/// When a client uses NTPv3 the server must mirror VN=3 in the response.
#[test]
fn build_response_version_mirrors_client_v3() {
    let dp = DisciplineParams {
        stratum: 1,
        state: DisciplineState::Locked,
        leap: 0,
        root_disp_short: 0,
        root_disp_ms: 0.0,
        ref_ts: 0,
        current_ts: 0,
        precision: -10,
    };
    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (3 << 3) | 3; // VN=3, mode=3 (client)
    let resp = build_response(&req, &dp, 0, MODE_SERVER);
    let vn = (resp[0] >> 3) & 0x07;
    assert_eq!(vn, 3, "server should mirror client VN=3");
}

/// VN 1–4 are mirrored; VN 0 and 5–7 are not valid NTP versions and the
/// reply falls back to VN=4 (both in time replies and KoD).
#[test]
fn build_response_clamps_invalid_version_to_v4() {
    let dp = synced_dp();
    for vn in 0_u8..=7 {
        let mut req = [0_u8; NTP_PACKET_LEN];
        req[0] = (vn << 3) | MODE_CLIENT;
        let expected = if (1..=4).contains(&vn) { vn } else { 4 };
        let resp = build_response(&req, &dp, 0, MODE_SERVER);
        assert_eq!((resp[0] >> 3) & 0x07, expected, "time reply for VN={vn}");
        let kod = build_kod_response(&req, vn, MODE_SERVER);
        assert_eq!((kod[0] >> 3) & 0x07, expected, "KoD reply for VN={vn}");
    }
}
