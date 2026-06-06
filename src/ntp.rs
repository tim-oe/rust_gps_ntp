//! Minimal NTP server with GPS/PPS-backed discipline and mode-6 diagnostics.
//!
//! The server responds to standard client time requests (mode 3/4 flow) and
//! a focused subset of mode-6 control queries used by `ntpq`.

use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
#[cfg(target_os = "espidf")]
use esp_idf_svc::sys;

const NTP_PORT: u16 = 123;
const NTP_PACKET_LEN: usize = 48;
const NTP_MAX_PACKET_LEN: usize = 512;
const NTP_CONTROL_HEADER_LEN: usize = 12;
const NTP_UNIX_EPOCH_OFFSET_SECS: u64 = 2_208_988_800;
const NTP_FRAC_SCALE: u128 = 1u128 << 32;
const MICROS_PER_SEC: i64 = 1_000_000;
const MODE6_OPCODE_READSTAT: u8 = 1;
const MODE6_OPCODE_READVAR: u8 = 2;
const MODE6_ASSOC_ID: u16 = 1;

// --- Discipline servo parameters ---
/// Proportional gain: fraction of phase error applied to the anchor per pulse.
const SERVO_KP: f64 = 0.1;
/// Integral gain: fraction of phase error fed into frequency learning per pulse.
const SERVO_KI: f64 = 0.01;
/// Clamp on estimated frequency offset to prevent runaway corrections (ppm).
const SERVO_MAX_FREQ_PPM: f64 = 500.0;

// --- Holdover and dispersion thresholds ---
/// Microseconds without a valid PPS pulse before holdover dispersion begins growing.
const HOLDOVER_ENTRY_US: i64 = 10_000_000;
/// Root dispersion growth rate in microseconds per second during holdover.
const HOLDOVER_DISP_RATE_US_PER_SEC: i64 = 500;
/// Base root dispersion when PPS-locked, in microseconds (1 ms).
const BASE_DISP_US: i64 = 1_000;
/// Maximum root dispersion cap, in microseconds (2 s).
const MAX_DISP_US: i64 = 2_000_000;
/// Dispersion threshold above which stratum=16 and leap=unsync are declared (1 s).
const HOLDOVER_UNSYNC_US: i64 = 1_000_000;

#[derive(Clone, Copy)]
struct ClockAnchor {
    /// Unix epoch seconds at the anchor instant.
    unix_seconds: i64,
    /// Monotonic microseconds when the anchor was captured.
    monotonic_us: i64,
}

/// High-level discipline state for display and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisciplineState {
    /// GPS fix present, PPS fresh, and anchor established.
    Locked,
    /// PPS or GPS lost; serving from oscillator with growing uncertainty.
    Holdover,
    /// No anchor; cannot produce disciplined timestamps.
    Unsync,
}

/// Snapshot of NTP discipline metrics for the display and UI task.
#[derive(Clone, Copy)]
pub struct NtpSnapshot {
    pub stratum: u8,
    pub state: DisciplineState,
    /// Estimated oscillator frequency offset in ppm (positive = fast).
    pub freq_ppm: f64,
    /// Latest PPS phase error in microseconds.
    pub pps_offset_us: i32,
    /// Smoothed PPS jitter estimate in microseconds.
    pub pps_jitter_us: f32,
    /// Current root dispersion in milliseconds.
    pub root_disp_ms: f64,
}

impl Default for NtpSnapshot {
    fn default() -> Self {
        Self {
            stratum: 16,
            state: DisciplineState::Unsync,
            freq_ppm: 0.0,
            pps_offset_us: 0,
            pps_jitter_us: 0.0,
            root_disp_ms: 5_000.0,
        }
    }
}

/// Discipline state derived from the PPS servo and holdover logic.
///
/// Used to populate stratum, leap indicator, and root dispersion fields in
/// both standard NTP responses and mode-6 control responses.
struct DisciplineParams {
    stratum: u8,
    leap: u8,
    /// Root dispersion in NTP short format (16.16 fixed-point seconds).
    root_disp_short: u32,
    /// Root dispersion in milliseconds, for mode-6 text variable output.
    root_disp_ms: f64,
}

/// Stateful NTP server engine and discipline metrics.
pub struct NtpServer {
    socket: UdpSocket,
    served: u64,
    clock_anchor: Option<ClockAnchor>,
    last_gps_utc_seconds: Option<i64>,
    pps_locked: bool,
    pps_offset_us: i32,
    pps_jitter_us: f32,
    pps_has_sample: bool,
    proc_delay_us: f32,
    proc_delay_has_sample: bool,
    /// Estimated oscillator frequency offset in parts per million.
    /// Positive means the local monotonic clock runs fast relative to GPS seconds.
    freq_ppm: f64,
    /// Monotonic timestamp of the last accepted PPS pulse.
    last_pps_monotonic_us: Option<i64>,
}

impl NtpServer {
    /// Bind a nonblocking UDP socket on NTP port 123.
    ///
    /// # Parameters
    /// - None.
    ///
    /// # Returns
    /// - `Ok(NtpServer)` when socket bind and nonblocking setup succeed.
    /// - `Err` when socket initialization fails.
    pub fn bind() -> anyhow::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", NTP_PORT))
            .context("failed to bind UDP socket on port 123")?;
        socket
            .set_nonblocking(true)
            .context("failed to set NTP socket nonblocking")?;

        Ok(Self {
            socket,
            served: 0,
            clock_anchor: None,
            last_gps_utc_seconds: None,
            pps_locked: false,
            pps_offset_us: 0,
            pps_jitter_us: 0.0,
            pps_has_sample: false,
            proc_delay_us: 0.0,
            proc_delay_has_sample: false,
            freq_ppm: 0.0,
            last_pps_monotonic_us: None,
        })
    }

    /// Update absolute UTC seconds from a parsed GPS RMC sample.
    ///
    /// # Parameters
    /// - `utc_unix_seconds`: UTC seconds since Unix epoch from GPS.
    ///
    /// # Returns
    /// - No return value.
    pub fn update_gps_utc_seconds(&mut self, utc_unix_seconds: i64) {
        self.last_gps_utc_seconds = Some(utc_unix_seconds);
        let now_us = monotonic_us_now();
        match self.clock_anchor {
            Some(anchor) => {
                let elapsed_sec = now_us
                    .saturating_sub(anchor.monotonic_us)
                    .div_euclid(MICROS_PER_SEC);
                let predicted = anchor.unix_seconds.saturating_add(elapsed_sec);
                if predicted.saturating_sub(utc_unix_seconds).abs() > 1 {
                    self.clock_anchor = Some(ClockAnchor {
                        unix_seconds: utc_unix_seconds,
                        monotonic_us: now_us,
                    });
                    log::debug!("NTP: UTC re-anchor from GPS");
                }
            }
            None => {
                self.clock_anchor = Some(ClockAnchor {
                    unix_seconds: utc_unix_seconds,
                    monotonic_us: now_us,
                });
            }
        }
    }

    /// Feed a PPS pulse event into the discipline servo.
    ///
    /// Pass `None` for the first observed pulse to align the anchor to GPS UTC
    /// without a frequency update. Pass `Some(interval_us)` for subsequent
    /// pulses to run the full PLL servo (proportional phase + integral frequency).
    ///
    /// # Parameters
    /// - `pps_interval_us`: `None` for first pulse alignment, or the measured
    ///   interval in microseconds between the previous and current PPS edge.
    ///
    /// # Returns
    /// - No return value.
    pub fn observe_pps_pulse(&mut self, pps_interval_us: Option<u32>) {
        let now_us = monotonic_us_now();

        match pps_interval_us {
            None => {
                // First pulse: align anchor to GPS UTC; no servo update yet since
                // there is no interval measurement to derive frequency error from.
                if let Some(gps_utc) = self.last_gps_utc_seconds {
                    self.clock_anchor = Some(ClockAnchor {
                        unix_seconds: gps_utc,
                        monotonic_us: now_us,
                    });
                    self.pps_locked = true;
                    self.last_pps_monotonic_us = Some(now_us);
                    log::debug!("NTP: PPS first pulse, aligned to GPS UTC {}", gps_utc);
                }
            }
            Some(interval) => {
                if !(800_000..=1_200_000).contains(&interval) {
                    return;
                }

                // Phase error in microseconds: positive means our clock runs fast
                // (the measured interval exceeded the ideal 1 000 000 us).
                let phase_error_us = interval as i64 - 1_000_000;

                // Integral path: accumulate frequency estimate.
                // A phase error of +1 us/s equals +1 ppm.
                self.freq_ppm = (self.freq_ppm + phase_error_us as f64 * SERVO_KI)
                    .clamp(-SERVO_MAX_FREQ_PPM, SERVO_MAX_FREQ_PPM);

                // Proportional path: nudge the anchor's monotonic reference to
                // correct the phase by a fraction of the observed error.
                if let Some(anchor) = &mut self.clock_anchor {
                    let phase_correction_us = (phase_error_us as f64 * SERVO_KP) as i64;
                    anchor.unix_seconds = anchor.unix_seconds.saturating_add(1);
                    // Subtracting the correction shifts the apparent second boundary
                    // earlier when the clock is fast, and later when it is slow.
                    anchor.monotonic_us = now_us.saturating_sub(phase_correction_us);
                } else if let Some(gps_utc) = self.last_gps_utc_seconds {
                    self.clock_anchor = Some(ClockAnchor {
                        unix_seconds: gps_utc,
                        monotonic_us: now_us,
                    });
                }

                // Update PPS diagnostics used by mode-6 and ntpq display.
                let sample_jitter = phase_error_us.unsigned_abs() as f32;
                self.pps_offset_us = phase_error_us as i32;
                if self.pps_has_sample {
                    self.pps_jitter_us = self.pps_jitter_us * 0.8 + sample_jitter * 0.2;
                } else {
                    self.pps_jitter_us = sample_jitter;
                    self.pps_has_sample = true;
                }

                self.pps_locked = true;
                self.last_pps_monotonic_us = Some(now_us);
                log::debug!(
                    "NTP: PPS servo phase_err={}us freq={:.3}ppm",
                    phase_error_us,
                    self.freq_ppm
                );
            }
        }
    }

    /// Compute discipline parameters from current servo and holdover state.
    ///
    /// Stratum and leap indicator follow GPS+PPS sync status. Root dispersion
    /// starts at 1 ms when locked and grows at 0.5 ms/s during holdover.
    /// When dispersion exceeds 1 s the server declares stratum=16 / leap=unsync.
    ///
    /// # Parameters
    /// - `gps_fix`: Whether the GPS module currently reports a valid fix.
    ///
    /// # Returns
    /// - `DisciplineParams` with stratum, leap, and dispersion fields.
    fn discipline_params(&self, gps_fix: bool) -> DisciplineParams {
        let now_us = monotonic_us_now();

        let pps_age_us = self
            .last_pps_monotonic_us
            .map(|t| now_us.saturating_sub(t))
            .unwrap_or(i64::MAX);
        let pps_fresh = pps_age_us < HOLDOVER_ENTRY_US;
        let has_anchor = self.clock_anchor.is_some();

        let fully_synced = gps_fix && pps_fresh && has_anchor;

        let disp_us = if fully_synced {
            BASE_DISP_US
        } else if has_anchor {
            // Holdover: dispersion grows linearly after HOLDOVER_ENTRY_US elapses.
            let holdover_us = pps_age_us.saturating_sub(HOLDOVER_ENTRY_US).max(0);
            let holdover_secs = holdover_us.div_euclid(MICROS_PER_SEC);
            let growth = holdover_secs.saturating_mul(HOLDOVER_DISP_RATE_US_PER_SEC);
            (BASE_DISP_US + growth).min(MAX_DISP_US)
        } else {
            MAX_DISP_US
        };

        // Declare stratum 1 as long as uncertainty is below the unsync threshold,
        // even without a current GPS fix (holdover), so clients keep using us.
        let stratum = if fully_synced || (has_anchor && disp_us < HOLDOVER_UNSYNC_US) {
            1_u8
        } else {
            16_u8
        };
        let leap = if stratum == 1 { 0_u8 } else { 3_u8 };

        // NTP short format: 16.16 fixed-point seconds.
        let root_disp_short = ((disp_us as u64 * (1u64 << 16)) / 1_000_000) as u32;
        let root_disp_ms = disp_us as f64 / 1_000.0;

        DisciplineParams {
            stratum,
            leap,
            root_disp_short,
            root_disp_ms,
        }
    }

    /// Build a public discipline snapshot for the UI and display task.
    ///
    /// # Parameters
    /// - `gps_fix`: Whether the GPS module currently reports a valid fix.
    ///
    /// # Returns
    /// - `NtpSnapshot` with current servo state, frequency, and dispersion.
    pub fn ntp_snapshot(&self, gps_fix: bool) -> NtpSnapshot {
        let dp = self.discipline_params(gps_fix);
        let now_us = monotonic_us_now();
        let pps_age_us = self
            .last_pps_monotonic_us
            .map(|t| now_us.saturating_sub(t))
            .unwrap_or(i64::MAX);
        let pps_fresh = pps_age_us < HOLDOVER_ENTRY_US;
        let has_anchor = self.clock_anchor.is_some();
        let fully_synced = gps_fix && pps_fresh && has_anchor;

        let state = if fully_synced {
            DisciplineState::Locked
        } else if has_anchor {
            DisciplineState::Holdover
        } else {
            DisciplineState::Unsync
        };

        NtpSnapshot {
            stratum: dp.stratum,
            state,
            freq_ppm: self.freq_ppm,
            pps_offset_us: self.pps_offset_us,
            pps_jitter_us: self.pps_jitter_us,
            root_disp_ms: dp.root_disp_ms,
        }
    }

    /// Poll and serve all immediately available NTP packets.
    ///
    /// # Parameters
    /// - `gps_fix`: Current GPS fix status used to determine sync state.
    ///
    /// # Returns
    /// - `Ok(())` when no more packets are pending or all served successfully.
    /// - `Err` on socket receive/send failures.
    pub fn poll(&mut self, gps_fix: bool) -> anyhow::Result<()> {
        let mut req = [0_u8; NTP_MAX_PACKET_LEN];

        loop {
            match self.socket.recv_from(&mut req) {
                Ok((len, peer)) => {
                    if len == 0 {
                        continue;
                    }

                    let mode = req[0] & 0x07;
                    let version = (req[0] >> 3) & 0x07;
                    match mode {
                        6 => {
                            if len < NTP_CONTROL_HEADER_LEN {
                                log::debug!(
                                    "NTP: ignoring short mode-6 request ({} bytes) from {}",
                                    len,
                                    peer
                                );
                                continue;
                            }
                            let dp = self.discipline_params(gps_fix);
                            let resp = build_mode6_response(
                                &req[..len],
                                version,
                                &dp,
                                self.pps_offset_us,
                                self.pps_jitter_us,
                                self.proc_delay_us,
                            );
                            self.socket.send_to(&resp, peer).with_context(|| {
                                format!("failed to send mode-6 response to {}", peer)
                            })?;
                        }
                        _ => {
                            if len < NTP_PACKET_LEN {
                                log::debug!(
                                    "NTP: ignoring short time request ({} bytes) from {}",
                                    len,
                                    peer
                                );
                                continue;
                            }

                            let mut req48 = [0_u8; NTP_PACKET_LEN];
                            req48.copy_from_slice(&req[..NTP_PACKET_LEN]);

                            let dp = self.discipline_params(gps_fix);
                            let started_us = monotonic_us_now();
                            let now_ntp = self.current_ntp_timestamp();
                            let mut resp = build_response(&req48, &dp, now_ntp);
                            let transmit_ts = self.current_ntp_timestamp();
                            write_u64_be(&mut resp[40..48], transmit_ts);

                            self.socket.send_to(&resp, peer).with_context(|| {
                                format!("failed to send NTP response to {}", peer)
                            })?;
                            let finished_us = monotonic_us_now();
                            let sample_us = (finished_us.saturating_sub(started_us)).max(1) as f32;
                            if self.proc_delay_has_sample {
                                self.proc_delay_us = self.proc_delay_us * 0.8 + sample_us * 0.2;
                            } else {
                                self.proc_delay_us = sample_us;
                                self.proc_delay_has_sample = true;
                            }
                        }
                    }

                    self.served += 1;

                    if self.served.is_multiple_of(64) {
                        log::debug!("NTP: served {} requests", self.served);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(err).context("NTP recv_from failed"),
            }
        }
    }
}

/// Build a mode-6 control response for a request packet.
///
/// # Parameters
/// - `req`: Raw mode-6 request bytes.
/// - `version`: NTP version extracted from request header.
/// - `dp`: Discipline parameters (stratum, leap, dispersion).
/// - `pps_offset_us`: Latest PPS-derived offset in microseconds.
/// - `pps_jitter_us`: Smoothed jitter estimate in microseconds.
/// - `proc_delay_us`: Smoothed processing delay estimate in microseconds.
///
/// # Returns
/// - Serialized mode-6 response bytes ready to send.
fn build_mode6_response(
    req: &[u8],
    version: u8,
    dp: &DisciplineParams,
    pps_offset_us: i32,
    pps_jitter_us: f32,
    proc_delay_us: f32,
) -> Vec<u8> {
    let mut resp = vec![0_u8; NTP_CONTROL_HEADER_LEN];
    let vn = if (1..=4).contains(&version) {
        version
    } else {
        4
    };
    let opcode = req[1] & 0x1f;
    let associd = u16::from_be_bytes([req[6], req[7]]);
    let mut payload: Vec<u8> = Vec::new();
    let mut error = false;

    let synced = dp.stratum == 1;
    // NTP mode-6 displays leap as its 2-bit binary value written as two digits.
    let leap_str = if dp.leap == 0 { "00" } else { "11" };

    // LI=0, VN from request, Mode=6 (control)
    resp[0] = (vn << 3) | 6;
    match opcode {
        MODE6_OPCODE_READSTAT => {
            // Return one pseudo-association so `ntpq -p` can continue querying it.
            let peer_status = if synced { 0x9624_u16 } else { 0x16_u16 };
            payload.extend_from_slice(&MODE6_ASSOC_ID.to_be_bytes());
            payload.extend_from_slice(&peer_status.to_be_bytes());
        }
        MODE6_OPCODE_READVAR => {
            let vars = if associd == 0 {
                if synced {
                    format!(
                        "stratum={},leap={},precision=-20,rootdelay=0.000,rootdisp={:.3},refid=GPS,peer=1,system=\"rust_gps_ntp\"",
                        dp.stratum, leap_str, dp.root_disp_ms
                    )
                } else {
                    format!(
                        "stratum=16,leap=11,precision=-20,rootdelay=0.000,rootdisp={:.3},refid=INIT,peer=0,system=\"rust_gps_ntp\"",
                        dp.root_disp_ms
                    )
                }
            } else if associd == MODE6_ASSOC_ID {
                if synced {
                    let offset_ms = (pps_offset_us as f32) / 1_000.0;
                    let jitter_ms = (pps_jitter_us.max(1.0)) / 1_000.0;
                    let delay_ms = (proc_delay_us.max(1.0)) / 1_000.0;
                    format!(
                        "srcadr=GPS,srcport=123,refid=GPS,stratum={},leap={},hmode=3,pmode=4,hpoll=6,ppoll=6,reach=255,delay={delay_ms:.3},offset={offset_ms:.3},jitter={jitter_ms:.3}",
                        dp.stratum, leap_str
                    )
                } else {
                    String::from(
                        "srcadr=INIT,srcport=123,refid=INIT,stratum=16,leap=11,hmode=3,pmode=4,hpoll=6,ppoll=6,reach=0,delay=0.001,offset=0.000,jitter=0.000",
                    )
                }
            } else {
                String::new()
            };
            payload.extend_from_slice(vars.as_bytes());
        }
        _ => {
            // Unsupported opcode: reply with a mode-6 error response.
            error = true;
        }
    }

    // Response bit set, keep opcode from request.
    resp[1] = 0x80 | ((error as u8) << 6) | opcode;
    // Sequence from request.
    resp[2] = req[2];
    resp[3] = req[3];

    // status
    let sys_status = if synced { 0x0604_u16 } else { 0x0_u16 };
    resp[4..6].copy_from_slice(&sys_status.to_be_bytes());
    // associd (echo request association)
    resp[6..8].copy_from_slice(&associd.to_be_bytes());
    // offset=0 (single-fragment response)
    resp[8..10].copy_from_slice(&0_u16.to_be_bytes());
    // count
    let count = payload.len().min(u16::MAX as usize) as u16;
    resp[10..12].copy_from_slice(&count.to_be_bytes());

    if count > 0 {
        resp.extend_from_slice(&payload[..count as usize]);
        // Mode-6 control payloads are 32-bit aligned on the wire.
        while !resp.len().is_multiple_of(4) {
            resp.push(0);
        }
    }
    resp
}

/// Build a standard 48-byte NTP server response template.
///
/// # Parameters
/// - `req`: Parsed 48-byte client request.
/// - `dp`: Discipline parameters (stratum, leap, dispersion).
/// - `receive_ts`: Server receive timestamp in NTP 64-bit format.
///
/// # Returns
/// - 48-byte response packet with originate/receive/reference fields populated.
fn build_response(
    req: &[u8; NTP_PACKET_LEN],
    dp: &DisciplineParams,
    receive_ts: u64,
) -> [u8; NTP_PACKET_LEN] {
    let mut resp = [0_u8; NTP_PACKET_LEN];
    let client_vn = (req[0] >> 3) & 0x07;
    let version = if (1..=4).contains(&client_vn) {
        client_vn
    } else {
        4
    };
    resp[0] = (dp.leap << 6) | (version << 3) | 4; // server mode
    resp[1] = dp.stratum;
    resp[2] = req[2]; // copy poll interval from client
    resp[3] = (-20_i8) as u8; // precision ~1us

    // Root delay / root dispersion in NTP short format (16.16).
    write_u32_be(&mut resp[4..8], 0);
    write_u32_be(&mut resp[8..12], dp.root_disp_short);

    // Reference ID:
    if dp.stratum == 1 {
        resp[12..16].copy_from_slice(b"GPS\0");
    } else {
        resp[12..16].copy_from_slice(b"INIT");
    }

    let reference_ts = if dp.stratum == 1 { receive_ts } else { 0 };
    write_u64_be(&mut resp[16..24], reference_ts);

    // Originate timestamp = client's transmit timestamp.
    resp[24..32].copy_from_slice(&req[40..48]);
    // Receive timestamp set by server when request was read.
    write_u64_be(&mut resp[32..40], receive_ts);
    // Transmit timestamp is filled just before send.
    resp
}

/// Compute current NTP timestamp directly from system wall clock.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Current NTP timestamp (`seconds.fraction` packed into `u64`).
fn ntp_timestamp_now() -> u64 {
    let unix = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(v) => v,
        Err(_) => Duration::from_secs(0),
    };
    let seconds = unix.as_secs().saturating_add(NTP_UNIX_EPOCH_OFFSET_SECS);
    let fraction = ((unix.subsec_nanos() as u128) * NTP_FRAC_SCALE / 1_000_000_000u128) as u64;
    (seconds << 32) | fraction
}

impl NtpServer {
    /// Compute the current NTP timestamp from the disciplined anchor clock.
    ///
    /// Elapsed monotonic time is scaled by the frequency correction factor so
    /// that oscillator drift accumulated since the last PPS pulse is removed
    /// before computing the fractional second.
    ///
    /// # Parameters
    /// - `self`: NTP server state containing discipline anchor.
    ///
    /// # Returns
    /// - Current disciplined NTP timestamp.
    fn current_ntp_timestamp(&self) -> u64 {
        if let Some(anchor) = self.clock_anchor {
            let now_us = monotonic_us_now();
            let raw_elapsed_us = now_us.saturating_sub(anchor.monotonic_us);

            // Scale elapsed time by (1 - freq_ppm/1e6) to compensate for oscillator
            // drift. Positive freq_ppm means the monotonic clock runs fast, so we
            // shrink the elapsed interval to recover real-time microseconds.
            let corrected_elapsed_us = if self.freq_ppm == 0.0 {
                raw_elapsed_us
            } else {
                ((raw_elapsed_us as f64) * (1.0 - self.freq_ppm / 1_000_000.0)) as i64
            }
            .max(0);

            let elapsed_seconds = corrected_elapsed_us.div_euclid(MICROS_PER_SEC);
            let rem_us = corrected_elapsed_us.rem_euclid(MICROS_PER_SEC) as u64;

            let unix_seconds = anchor.unix_seconds.saturating_add(elapsed_seconds);
            let ntp_seconds = if unix_seconds >= 0 {
                (unix_seconds as u64).saturating_add(NTP_UNIX_EPOCH_OFFSET_SECS)
            } else {
                NTP_UNIX_EPOCH_OFFSET_SECS
            };
            let ntp_fraction = ((rem_us as u128) * NTP_FRAC_SCALE / 1_000_000u128) as u64;
            (ntp_seconds << 32) | ntp_fraction
        } else {
            ntp_timestamp_now()
        }
    }
}

/// Return monotonic microseconds from ESP-IDF high-resolution timer.
fn monotonic_us_now() -> i64 {
    #[cfg(target_os = "espidf")]
    {
        return unsafe { sys::esp_timer_get_time() };
    }
    #[cfg(not(target_os = "espidf"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64
    }
}

/// Write a big-endian `u32` into an output byte slice.
///
/// # Parameters
/// - `dst`: Destination slice (must be length 4).
/// - `value`: Integer value to encode.
///
/// # Returns
/// - No return value.
fn write_u32_be(dst: &mut [u8], value: u32) {
    dst.copy_from_slice(&value.to_be_bytes());
}

/// Write a big-endian `u64` into an output byte slice.
///
/// # Parameters
/// - `dst`: Destination slice (must be length 8).
/// - `value`: Integer value to encode.
///
/// # Returns
/// - No return value.
fn write_u64_be(dst: &mut [u8], value: u64) {
    dst.copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_response_sets_server_mode_and_stratum_when_synced() {
        let dp = DisciplineParams {
            stratum: 1,
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 1.0,
        };
        let req = [0_u8; NTP_PACKET_LEN];
        let resp = build_response(&req, &dp, 0x0123_0000_0000_0000);
        assert_eq!(resp[0] & 0x07, 4);
        assert_eq!(resp[1], 1);
        assert_eq!(&resp[12..16], b"GPS\0");
    }

    #[test]
    fn build_response_marks_unsynced_with_init_refid() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
        };
        let req = [0_u8; NTP_PACKET_LEN];
        let resp = build_response(&req, &dp, 0);
        assert_eq!(resp[1], 16);
        assert_eq!(&resp[12..16], b"INIT");
    }

    #[test]
    fn build_mode6_readvar_includes_gps_peer_when_synced() {
        let dp = DisciplineParams {
            stratum: 1,
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 1.0,
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
        let resp = build_mode6_response(&req, 4, &dp, 250, 500.0, 1200.0);
        let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
        assert!(payload.contains("refid=GPS"));
        assert!(payload.contains("stratum=1"));
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
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 1.0,
        };
        let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0);
        assert_eq!(resp.len(), NTP_CONTROL_HEADER_LEN + 4);
    }

    #[test]
    fn build_mode6_readvar_unsynced_system_vars() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
        };
        let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0);
        let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
        assert!(payload.contains("stratum=16"));
        assert!(payload.contains("refid=INIT"));
    }

    #[test]
    fn build_mode6_readvar_unsynced_peer_vars() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
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
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0);
        let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
        assert!(payload.contains("srcadr=INIT"));
    }

    #[test]
    fn build_mode6_unsupported_opcode_sets_error_bit() {
        let dp = DisciplineParams {
            stratum: 1,
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 1.0,
        };
        let req = [0, 99, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0);
        assert_ne!(resp[1] & 0x40, 0);
    }

    #[test]
    fn build_mode6_invalid_version_defaults_to_v4() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
        };
        let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 0, &dp, 0, 0.0, 0.0);
        assert_eq!((resp[0] >> 3) & 0x07, 4);
    }

    impl NtpServer {
        fn new_for_test() -> anyhow::Result<Self> {
            let socket = UdpSocket::bind("127.0.0.1:0")?;
            socket.set_nonblocking(true)?;
            Ok(Self {
                socket,
                served: 0,
                clock_anchor: None,
                last_gps_utc_seconds: None,
                pps_locked: false,
                pps_offset_us: 0,
                pps_jitter_us: 0.0,
                pps_has_sample: false,
                proc_delay_us: 0.0,
                proc_delay_has_sample: false,
                freq_ppm: 0.0,
                last_pps_monotonic_us: None,
            })
        }
    }

    fn client_ntp_request() -> [u8; NTP_PACKET_LEN] {
        let mut req = [0_u8; NTP_PACKET_LEN];
        req[0] = (4 << 3) | 3;
        req
    }

    #[test]
    fn update_gps_utc_seconds_establishes_anchor() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        assert!(server.clock_anchor.is_some());
        assert_eq!(server.last_gps_utc_seconds, Some(1_700_000_000));
    }

    #[test]
    fn update_gps_utc_seconds_reanchors_on_large_drift() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.update_gps_utc_seconds(1_700_000_010);
        let anchor = server.clock_anchor.expect("anchor");
        assert_eq!(anchor.unix_seconds, 1_700_000_010);
    }

    #[test]
    fn observe_pps_pulse_first_pulse_locks_to_gps() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        assert!(server.pps_locked);
        assert!(server.last_pps_monotonic_us.is_some());
    }

    #[test]
    fn observe_pps_pulse_valid_interval_advances_anchor() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        let before = server.clock_anchor.expect("anchor").unix_seconds;
        server.observe_pps_pulse(Some(1_000_000));
        assert_eq!(
            server.clock_anchor.expect("anchor").unix_seconds,
            before + 1
        );
        assert!(server.pps_has_sample);
    }

    #[test]
    fn observe_pps_pulse_ignores_invalid_interval() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(Some(2_000_000));
        assert!(!server.pps_has_sample);
    }

    #[test]
    fn poll_serves_client_time_request() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        let addr = server.socket.local_addr().expect("local addr");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);

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
        assert_eq!(resp[1], 16);
    }

    #[test]
    fn poll_smoothes_processing_delay_after_second_request() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        let addr = server.socket.local_addr().expect("local addr");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);

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
    }

    #[test]
    fn observe_pps_pulse_smoothes_jitter_on_second_valid_interval() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        server.observe_pps_pulse(Some(1_000_250));
        server.observe_pps_pulse(Some(1_000_500));
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
        server.observe_pps_pulse(None);
        // Interval longer than 1s: monotonic is fast relative to GPS.
        server.observe_pps_pulse(Some(1_000_500));
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
        server.observe_pps_pulse(None);
        // Interval shorter than 1s: monotonic is slow relative to GPS.
        server.observe_pps_pulse(Some(999_500));
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

    #[test]
    fn servo_accumulates_freq_over_multiple_pulses() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        // Apply ten identical intervals with +100 us error each.
        for _ in 0..10 {
            server.observe_pps_pulse(Some(1_000_100));
        }
        // Expected after 10 pulses: 10 * 100 * 0.01 = 10.0 ppm
        assert!(
            (server.freq_ppm - 10.0).abs() < 0.01,
            "expected ~10.0 ppm, got {}",
            server.freq_ppm
        );
    }

    #[test]
    fn servo_clamps_freq_ppm_at_max() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        // Extreme interval: should saturate at SERVO_MAX_FREQ_PPM.
        for _ in 0..10_000 {
            server.observe_pps_pulse(Some(1_200_000));
        }
        assert_eq!(server.freq_ppm, SERVO_MAX_FREQ_PPM);
    }

    #[test]
    fn discipline_params_fully_synced_gives_stratum_1_and_base_dispersion() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        let dp = server.discipline_params(true);
        assert_eq!(dp.stratum, 1);
        assert_eq!(dp.leap, 0);
        let expected_short = ((BASE_DISP_US as u64 * (1u64 << 16)) / 1_000_000) as u32;
        assert_eq!(dp.root_disp_short, expected_short);
        assert!((dp.root_disp_ms - 1.0).abs() < 0.01);
    }

    #[test]
    fn discipline_params_fresh_pps_holds_stratum_1_without_gps_fix() {
        // Holdover design: with fresh PPS and a valid anchor, remain stratum 1
        // even when GPS fix is temporarily lost, so clients keep using us while
        // dispersion is still low.
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        let dp = server.discipline_params(false);
        assert_eq!(dp.stratum, 1, "should hold stratum 1 with fresh PPS in holdover");
    }

    #[test]
    fn discipline_params_stale_pps_and_no_gps_fix_gives_stratum_16() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        // Simulate deep holdover (PPS stale beyond unsync threshold).
        let past_us = monotonic_us_now() - (HOLDOVER_ENTRY_US + 3_000 * MICROS_PER_SEC);
        server.last_pps_monotonic_us = Some(past_us);
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
        server.observe_pps_pulse(None);
        // Simulate PPS that arrived 60 s into holdover (10s entry + 60s growth).
        let past_us =
            monotonic_us_now() - (HOLDOVER_ENTRY_US + 60 * MICROS_PER_SEC);
        server.last_pps_monotonic_us = Some(past_us);
        let dp = server.discipline_params(true);
        // Expected disp = BASE_DISP_US + 60 * RATE = 1000 + 30000 = 31000 us = 31 ms.
        assert!(
            dp.root_disp_ms > 1.0,
            "dispersion should exceed base 1 ms in holdover"
        );
        assert!(
            dp.root_disp_ms > 29.0,
            "expected ~31 ms, got {:.1} ms",
            dp.root_disp_ms
        );
        // Still stratum 1 since 31 ms < 1000 ms threshold.
        assert_eq!(dp.stratum, 1, "should remain stratum 1 below 1 s threshold");
    }

    #[test]
    fn holdover_declares_stratum_16_when_dispersion_exceeds_1s() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        // Need dispersion >= HOLDOVER_UNSYNC_US (1_000_000 us):
        // BASE + secs * RATE >= 1_000_000  →  secs >= (1_000_000 - 1_000) / 500 = 1998 s
        let holdover_secs = 2_010_i64;
        let past_us = monotonic_us_now()
            - (HOLDOVER_ENTRY_US + holdover_secs * MICROS_PER_SEC);
        server.last_pps_monotonic_us = Some(past_us);
        let dp = server.discipline_params(true);
        assert_eq!(dp.stratum, 16, "should be stratum 16 when dispersion >= 1 s");
        assert_eq!(dp.leap, 3);
    }

    #[test]
    fn holdover_stratum_1_restored_after_pps_returns() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        // Force into deep holdover.
        let past_us = monotonic_us_now()
            - (HOLDOVER_ENTRY_US + 3_000 * MICROS_PER_SEC);
        server.last_pps_monotonic_us = Some(past_us);
        assert_eq!(server.discipline_params(true).stratum, 16);
        // Simulate PPS returning.
        server.observe_pps_pulse(Some(1_000_000));
        let dp = server.discipline_params(true);
        assert_eq!(dp.stratum, 1, "stratum should recover to 1 after PPS returns");
    }

    #[test]
    fn freq_correction_does_not_affect_zero_freq_ppm() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None);
        assert_eq!(server.freq_ppm, 0.0);
        // current_ntp_timestamp should not panic or produce obviously wrong output.
        let ts = server.current_ntp_timestamp();
        assert_ne!(ts, 0);
    }
}
