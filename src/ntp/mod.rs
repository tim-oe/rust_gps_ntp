//! Minimal NTP server with GPS/PPS-backed discipline and mode-6 diagnostics.
//!
//! The server responds to standard client time requests (mode 3/4 flow) and
//! a focused subset of mode-6 control queries used by `ntpq`.
//!
//! Service-protection types (rate limiter, ACL) live in the [`protection`]
//! submodule.

mod protection;
pub use protection::Acl;
use protection::RateLimiter;

use std::net::{SocketAddr, UdpSocket};
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

/// Seconds added to the NMEA UTC value when anchoring to a PPS edge.
///
/// Most GPS modules output the NMEA sentence for second N *after* the PPS
/// pulse for second N fires, so `last_gps_utc_seconds` lags by one second at
/// PPS time.  Set `CONFIG_GPS_NTP_NMEA_PPS_FUDGE_S=0` in `sdkconfig.defaults`
/// for modules that transmit NMEA *before* the PPS edge (uncommon).
const NMEA_PPS_FUDGE_S: i64 = match env!("NMEA_PPS_FUDGE_S").as_bytes() {
    [b'0'] => 0,
    _ => 1,
};

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

// --- RFC 5905 §11.1 correctness-field constants ---
/// Maximum oscillator drift rate per RFC 5905 §11.1 (symbol PHI = 15 ppm).
/// Governs how fast root dispersion accumulates within an NTP polling interval.
const PHI_US_PER_S: i64 = 15;
/// Hardware accuracy floor for GPS + PPS: MTK3339 PPS accuracy is ±10 ns, but
/// ISR capture latency and ESP32 timer jitter make 100 µs a conservative floor.
const MIN_HW_ACCURACY_US: i64 = 100;
/// Worst-case uncertainty from the 1 kHz main-loop poll interval (NTP D/2).
const LOOP_POLL_UNCERTAINTY_US: i64 = 500;
/// The round-trip delay to a hardware reference clock wired directly to a GPIO
/// pin is modelled as zero; any GPS propagation delay is absorbed into precision.
const ROOT_DELAY_MS: f64 = 0.0;

// --- Long-run robustness and leap-second constants ---
/// Phase error magnitude above which a PPS interval is treated as an outlier
/// and rejected without feeding the servo.  Applied only after the servo has
/// converged (i.e. `pps_has_sample = true`).  Protects `freq_ppm` from being
/// corrupted by a single errant PPS edge while still allowing the normal ±20%
/// interval filter to catch gross glitches.
const PPS_OUTLIER_THRESHOLD_US: i64 = 50_000; // 50 ms
/// Maximum age of the last GPS UTC update before the first PPS pulse is
/// considered "stale GPS" and the phase anchor is not set.  Prevents anchoring
/// to GPS data that was cached before a GPS module reset or cold-start.
const GPS_STALE_THRESHOLD_US: i64 = 2_000_000; // 2 s

// --- KoD constant (used by build_kod_response) ---
/// Kiss-o'-Death kiss code for rate limiting (RFC 5905 §7.4).
const KOD_KISS_RATE: &[u8; 4] = b"RATE";

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
    /// Total NTP requests served since boot.
    pub served: u64,
    /// Total Kiss-o'-Death RATE responses sent (rate-limited clients).
    pub rate_limited: u64,
    /// Total packets silently dropped by the ACL.
    pub acl_blocked: u64,
    /// Total PPS phase outliers rejected since boot (large single-pulse errors).
    pub pps_glitch_count: u32,
    /// Current leap indicator value being broadcast in NTP responses.
    /// 0 = no warning, 1 = +1 s at end of day, 2 = −1 s at end of day.
    pub leap_indicator: u8,
    /// Smoothed per-packet NTP processing delay in microseconds (EWMA).
    pub proc_delay_us: f32,
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
            served: 0,
            rate_limited: 0,
            acl_blocked: 0,
            pps_glitch_count: 0,
            leap_indicator: 0,
            proc_delay_us: 0.0,
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
    /// NTP timestamp of the most recent clock discipline event (last PPS pulse).
    /// Used as the Reference Timestamp field per RFC 5905 §7.3.
    ref_ts: u64,
    /// Current NTP timestamp, captured when DisciplineParams was computed.
    /// Used as the system clock value in mode-6 READVAR responses.
    current_ts: u64,
    /// RFC 5905 precision field: log2(seconds) of estimated worst-case error.
    precision: i8,
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
    /// NTP timestamp of the most recent PPS-based clock discipline event.
    /// Returned as the Reference Timestamp in NTP responses (RFC 5905 §7.3).
    last_sync_ntp_ts: u64,
    /// Per-client rate limiter for incoming time and control requests.
    rate_limiter: RateLimiter,
    /// IP allowlist; checked for every incoming packet.
    acl: Acl,
    /// Total Kiss-o'-Death RATE responses sent to rate-limited clients.
    rate_limited_total: u64,
    /// Total packets dropped by the ACL.
    acl_blocked_total: u64,
    /// Leap indicator to broadcast in NTP responses when synced (RFC 5905 §7.3).
    /// 0 = no warning, 1 = last minute has 61 s, 2 = last minute has 59 s.
    /// Set via `set_leap_indicator()`; must be cleared manually after the event.
    pending_leap: u8,
    /// Count of PPS intervals rejected as phase outliers since boot.
    pps_glitch_count: u32,
    /// Monotonic timestamp of the most recent `update_gps_utc_seconds` call.
    /// Used to guard against anchoring the phase to stale GPS data on the first
    /// PPS pulse after a device restart or GPS module reset.
    last_gps_utc_update_us: Option<i64>,
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
            last_sync_ntp_ts: 0,
            rate_limiter: RateLimiter::new(),
            acl: Acl::allow_all(),
            rate_limited_total: 0,
            acl_blocked_total: 0,
            pending_leap: 0,
            pps_glitch_count: 0,
            last_gps_utc_update_us: None,
        })
    }

    /// Replace the ACL used by this server.
    ///
    /// Takes effect immediately on the next call to `poll()`.
    ///
    /// # Example
    /// ```rust,no_run
    /// use rust_gps_ntp::ntp::{Acl, NtpServer};
    ///
    /// let mut server = NtpServer::bind().unwrap();
    /// server.set_acl(Acl::private_lan());
    /// ```
    pub fn set_acl(&mut self, acl: Acl) {
        self.acl = acl;
    }

    /// Set the leap second indicator broadcast in NTP responses (RFC 5905 §7.3).
    ///
    /// | `li` | Meaning |
    /// |---|---|
    /// | 0 | No warning (normal operation) |
    /// | 1 | Last minute of the current UTC day has 61 seconds (+1 leap) |
    /// | 2 | Last minute of the current UTC day has 59 seconds (−1 leap) |
    ///
    /// The indicator is only emitted when the server is synced (stratum 1).
    /// Values greater than 2 are clamped to 2; to clear the warning call
    /// `set_leap_indicator(0)`.
    ///
    /// **The caller is responsible for clearing the indicator after the leap
    /// event has passed** (typically at 00:00:00 UTC on the day after the
    /// event). GPS receivers apply the current UTC offset internally, so the
    /// time output is always correct; the warning exists only so NTP clients
    /// can prepare their own clocks.
    ///
    /// # Example
    /// ```rust,no_run
    /// use rust_gps_ntp::ntp::NtpServer;
    ///
    /// let mut server = NtpServer::bind().unwrap();
    /// // Announce a positive leap second at end of the current UTC day.
    /// server.set_leap_indicator(1);
    /// // … after midnight UTC …
    /// server.set_leap_indicator(0);
    /// ```
    pub fn set_leap_indicator(&mut self, li: u8) {
        self.pending_leap = li.min(2);
    }

    /// Update absolute UTC seconds from GPS, RTC, or another cached time source.
    ///
    /// # Parameters
    /// - `utc_unix_seconds`: UTC seconds since Unix epoch.
    ///
    /// # Returns
    /// - No return value.
    pub fn update_gps_utc_seconds(&mut self, utc_unix_seconds: i64) {
        self.last_gps_utc_seconds = Some(utc_unix_seconds);
        let now_us = monotonic_us_now();
        self.last_gps_utc_update_us = Some(now_us);
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
    /// - `edge_us`: ISR-captured monotonic timestamp of the rising edge.
    ///   Using the ISR timestamp instead of reading the clock at call time
    ///   eliminates task-scheduling latency (~10–100 ms) from the anchor.
    ///
    /// # Returns
    /// - No return value.
    pub fn observe_pps_pulse(&mut self, pps_interval_us: Option<u32>, edge_us: i64) {
        let now_us = edge_us;

        match pps_interval_us {
            None => {
                // First pulse: align anchor to GPS UTC; no servo update yet since
                // there is no interval measurement to derive frequency error from.
                // Guard: only anchor if GPS time is fresh, i.e. the NMEA parser
                // fed us a UTC value recently.  Stale GPS data (from before a
                // module reset or cold-start) must not be used to initialise the
                // clock anchor.
                let gps_fresh = self
                    .last_gps_utc_update_us
                    .map(|t| now_us.saturating_sub(t) < GPS_STALE_THRESHOLD_US)
                    .unwrap_or(false);

                if !gps_fresh {
                    log::debug!(
                        "NTP: PPS first pulse skipped — GPS time not fresh (age > {}s)",
                        GPS_STALE_THRESHOLD_US / MICROS_PER_SEC
                    );
                    return;
                }

                if let Some(gps_utc) = self.last_gps_utc_seconds {
                    self.clock_anchor = Some(ClockAnchor {
                        // The GPS module sends NMEA ~100-200 ms AFTER the PPS
                        // edge.  When PPS fires for second N, last_gps_utc_seconds
                        // still holds N-1 (from the previous NMEA cycle).  Adding
                        // NMEA_PPS_FUDGE_S (from sdkconfig.defaults) aligns the
                        // anchor to the second the PPS pulse is actually marking.
                        unix_seconds: gps_utc + NMEA_PPS_FUDGE_S,
                        monotonic_us: now_us,
                    });
                    self.pps_locked = true;
                    self.last_pps_monotonic_us = Some(now_us);
                    // Record the NTP timestamp of this discipline event as the
                    // reference timestamp returned to NTP clients (RFC 5905 §7.3).
                    self.last_sync_ntp_ts = self.current_ntp_timestamp();
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

                // Outlier guard: after the servo has had at least one sample to
                // establish a jitter baseline, reject any pulse whose phase error
                // exceeds PPS_OUTLIER_THRESHOLD_US (50 ms).  Such errors are far
                // larger than normal oscillator drift and indicate a bad PPS edge,
                // GPS time jump, or transient interference.  Rejecting here keeps
                // freq_ppm and the anchor free of single-sample corruption.
                if self.pps_has_sample
                    && phase_error_us.unsigned_abs() > PPS_OUTLIER_THRESHOLD_US as u64
                {
                    self.pps_glitch_count = self.pps_glitch_count.saturating_add(1);
                    log::warn!(
                        "NTP: PPS outlier rejected phase_err={}us (glitch #{})",
                        phase_error_us,
                        self.pps_glitch_count
                    );
                    return;
                }

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
                        unix_seconds: gps_utc + NMEA_PPS_FUDGE_S,
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
                // Record the NTP timestamp of this discipline event so clients
                // can observe reference timestamp aging (RFC 5905 §7.3).
                self.last_sync_ntp_ts = self.current_ntp_timestamp();
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
    /// **Root dispersion** follows RFC 5905 §11.1: when PPS-locked the base is
    /// `max(jitter, MIN_HW_ACCURACY_US) + PHI × age_since_last_pulse` where PHI
    /// is 15 ppm. During holdover it grows at 0.5 ms/s until stratum=16 is
    /// declared at 1 s.
    ///
    /// **Root delay** is 0 for a hardware-referenced GPS+PPS server (RFC 5905 §6).
    ///
    /// **Reference timestamp** (`ref_ts`) is the NTP timestamp of the last PPS
    /// discipline event, so clients can observe reference aging (RFC 5905 §7.3).
    ///
    /// # Parameters
    /// - `gps_fix`: Whether the GPS module currently reports a valid fix.
    ///
    /// # Returns
    /// - `DisciplineParams` with all correctness fields populated.
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
            // Model-driven dispersion per RFC 5905 §11.1:
            //   disp = max(jitter, hw_accuracy_floor) + PHI × age_since_last_pulse
            // PHI = 15 ppm is the RFC's assumed maximum frequency tolerance.
            // This shrinks to the jitter floor just after a PPS pulse and grows
            // by at most 15 µs before the next one (≈ 1 s interval).
            let hw_accuracy = if self.pps_has_sample {
                (self.pps_jitter_us.round() as i64).max(MIN_HW_ACCURACY_US)
            } else {
                MIN_HW_ACCURACY_US
            };
            let age_us = pps_age_us.min(MICROS_PER_SEC);
            let phi_component = PHI_US_PER_S * age_us / MICROS_PER_SEC;
            hw_accuracy + phi_component
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
        // Use the application-supplied leap indicator when synced.
        // When unsynced (stratum 16) always force LI=3 (alarm) per RFC 5905 §7.3.
        let leap = if stratum == 1 {
            self.pending_leap
        } else {
            3_u8
        };

        // NTP short format: 16.16 fixed-point seconds.
        let root_disp_short = ((disp_us as u64 * (1u64 << 16)) / 1_000_000) as u32;
        let root_disp_ms = disp_us as f64 / 1_000.0;

        let precision = if fully_synced {
            let mut uncertainty_us = if self.pps_has_sample {
                self.pps_jitter_us.max(MIN_HW_ACCURACY_US as f32)
            } else {
                MIN_HW_ACCURACY_US as f32
            };
            uncertainty_us = uncertainty_us.max(LOOP_POLL_UNCERTAINTY_US as f32);
            if self.proc_delay_has_sample {
                uncertainty_us = uncertainty_us.max(self.proc_delay_us);
            }
            ntp_precision_from_uncertainty_us(uncertainty_us as f64)
        } else if has_anchor {
            ntp_precision_from_uncertainty_us(disp_us as f64)
        } else {
            ntp_precision_from_uncertainty_us(MAX_DISP_US as f64)
        };

        DisciplineParams {
            stratum,
            leap,
            root_disp_short,
            root_disp_ms,
            ref_ts: self.last_sync_ntp_ts,
            current_ts: self.current_ntp_timestamp(),
            precision,
        }
    }

    /// Current disciplined UTC seconds when a clock anchor exists.
    ///
    /// Uses the same frequency-corrected elapsed time as [`Self::current_ntp_timestamp`].
    pub fn current_utc_unix_seconds(&self) -> Option<i64> {
        let anchor = self.clock_anchor?;
        let now_us = monotonic_us_now();
        let raw_elapsed_us = now_us.saturating_sub(anchor.monotonic_us);
        let corrected_elapsed_us = if self.freq_ppm == 0.0 {
            raw_elapsed_us
        } else {
            ((raw_elapsed_us as f64) * (1.0 - self.freq_ppm / 1_000_000.0)) as i64
        }
        .max(0);
        let elapsed_seconds = corrected_elapsed_us.div_euclid(MICROS_PER_SEC);
        Some(anchor.unix_seconds.saturating_add(elapsed_seconds))
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
            served: self.served,
            rate_limited: self.rate_limited_total,
            acl_blocked: self.acl_blocked_total,
            pps_glitch_count: self.pps_glitch_count,
            leap_indicator: dp.leap,
            proc_delay_us: if self.proc_delay_has_sample {
                self.proc_delay_us
            } else {
                0.0
            },
        }
    }

    /// Returns `true` when the source may proceed, `false` when rate-limited.
    fn try_rate_limit(&mut self, src_ip_v4: Option<u32>) -> bool {
        let Some(ip) = src_ip_v4 else {
            return true;
        };
        let now_us = monotonic_us_now();
        if self.rate_limiter.check(ip, now_us) {
            true
        } else {
            self.rate_limited_total += 1;
            false
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

                    // RFC 5905 receive timestamp (T2): sample before ACL, rate-limit,
                    // or discipline work so burst traffic does not inflate T2.
                    let receive_ntp_ts = self.current_ntp_timestamp();

                    // Extract IPv4 source address for ACL and rate limiting.
                    // IPv6 sources bypass both (pass through unconditionally).
                    let src_ip_v4: Option<u32> = match &peer {
                        SocketAddr::V4(a) => Some(u32::from(*a.ip())),
                        SocketAddr::V6(_) => None,
                    };

                    // ACL check: silently drop packets from disallowed sources.
                    if let Some(ip) = src_ip_v4
                        && !self.acl.allows(ip)
                    {
                        self.acl_blocked_total += 1;
                        log::debug!("NTP: ACL blocked packet from {}", peer);
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
                            if !self.try_rate_limit(src_ip_v4) {
                                log::debug!("NTP: mode-6 rate limited from {}", peer);
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
                                self.freq_ppm,
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

                            // Rate limit all 48-byte time requests (modes 0–5, 7).
                            // Mode-6 uses the same limiter but drops silently.
                            if !self.try_rate_limit(src_ip_v4) {
                                let mut req48 = [0_u8; NTP_PACKET_LEN];
                                req48.copy_from_slice(&req[..NTP_PACKET_LEN]);
                                let resp = build_kod_response(&req48, version);
                                let _ = self.socket.send_to(&resp, peer);
                                log::debug!("NTP: KoD RATE sent to {} (rate limited)", peer);
                                continue;
                            }

                            let mut req48 = [0_u8; NTP_PACKET_LEN];
                            req48.copy_from_slice(&req[..NTP_PACKET_LEN]);

                            let dp = self.discipline_params(gps_fix);
                            let started_us = monotonic_us_now();
                            let mut resp = build_response(&req48, &dp, receive_ntp_ts);
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

/// Format an NTP 64-bit timestamp as a mode-6 text value: `0xSSSSSSSS.FFFFFFFF`.
///
/// RFC 1305 §3.2 convention for NTP timestamps in control-message text is
/// the hex representation of the 32-bit seconds field followed by the 32-bit
/// fraction field, separated by `.`.  ntpq parses and displays this form.
///
/// # Parameters
/// - `ts`: 64-bit NTP timestamp (seconds in high 32 bits, fraction in low 32 bits).
///
/// # Returns
/// - Formatted timestamp string, e.g. `0xE9D1E7B4.8A3D71A0`.
fn ntp_ts_to_mode6(ts: u64) -> String {
    format!("0x{:08X}.{:08X}", (ts >> 32) as u32, ts as u32)
}

/// Build a mode-6 control response for a request packet.
///
/// # Parameters
/// - `req`: Raw mode-6 request bytes.
/// - `version`: NTP version extracted from request header.
/// - `dp`: Discipline parameters (stratum, leap, dispersion, timestamps).
/// - `pps_offset_us`: Latest PPS-derived phase offset in microseconds.
/// - `pps_jitter_us`: Smoothed jitter estimate in microseconds.
/// - `proc_delay_us`: Smoothed processing delay estimate in microseconds.
/// - `freq_ppm`: Current oscillator frequency offset estimate in ppm.
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
    freq_ppm: f64,
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
            // RFC 1305 §3.2.2 peer status word: [Sel:3][Cfg:1][Auth:1][AuthOK:1][Reach:1][Bcast:1][EvtCode:4][EvtCnt:4]
            // sel=6 (system peer, shown as '*' by ntpq), cfg=1, reach=1.
            // sel=0 (rejected) when unsynced.
            let peer_status: u16 = if synced {
                (6_u16 << 13) | (1 << 12) | (1 << 9) // 0xD200
            } else {
                1 << 12 // 0x1000: configured but not selected
            };
            payload.extend_from_slice(&MODE6_ASSOC_ID.to_be_bytes());
            payload.extend_from_slice(&peer_status.to_be_bytes());
        }
        MODE6_OPCODE_READVAR => {
            let vars = if associd == 0 {
                // System variables (RFC 5905 §7.3 + common ntpq fields).
                // Expanded with frequency, reference timestamp, clock, jitter,
                // and wander so ntpq -c rv gives a complete diagnostic picture.
                if synced {
                    let offset_ms = pps_offset_us as f64 / 1_000.0;
                    let jitter_ms = (pps_jitter_us.max(1.0)) as f64 / 1_000.0;
                    let freq_sign = if freq_ppm >= 0.0 { "+" } else { "" };
                    format!(
                        "stratum={},leap={},precision={},\
                         rootdelay={:.3},rootdisp={:.3},refid=GPS,\
                         reftime={},clock={},\
                         offset={:.3},frequency={}{:.3},\
                         sys_jitter={:.3},clk_jitter={:.3},clk_wander={:.3},\
                         tc=7,mintc=3,peer=1,system=\"rust_gps_ntp\"",
                        dp.stratum,
                        leap_str,
                        dp.precision,
                        ROOT_DELAY_MS,
                        dp.root_disp_ms,
                        ntp_ts_to_mode6(dp.ref_ts),
                        ntp_ts_to_mode6(dp.current_ts),
                        offset_ms,
                        freq_sign,
                        freq_ppm,
                        jitter_ms,
                        jitter_ms,
                        freq_ppm.abs(),
                    )
                } else {
                    format!(
                        "stratum=16,leap=11,precision={},\
                         rootdelay={:.3},rootdisp={:.3},refid=INIT,\
                         reftime={},clock={},\
                         offset=0.000,frequency=+0.000,\
                         sys_jitter=0.000,clk_jitter=0.000,clk_wander=0.000,\
                         tc=7,mintc=3,peer=0,system=\"rust_gps_ntp\"",
                        dp.precision,
                        ROOT_DELAY_MS,
                        dp.root_disp_ms,
                        ntp_ts_to_mode6(dp.ref_ts),
                        ntp_ts_to_mode6(dp.current_ts),
                    )
                }
            } else if associd == MODE6_ASSOC_ID {
                // Peer variables (RFC 5905 §7.3 + ntpq filter columns).
                // filtdelay / filtoffset / filtdisp echo the last measured values;
                // we do not maintain a full 8-sample filter register.
                if synced {
                    let offset_ms = pps_offset_us as f64 / 1_000.0;
                    let jitter_ms = (pps_jitter_us.max(1.0)) as f64 / 1_000.0;
                    let delay_ms = (proc_delay_us.max(1.0)) as f64 / 1_000.0;
                    format!(
                        "srcadr=GPS,srcport=123,refid=GPS,\
                         stratum={},leap={},hmode=3,pmode=4,\
                         hpoll=6,ppoll=6,reach=255,\
                         delay={delay_ms:.3},offset={offset_ms:.3},jitter={jitter_ms:.3},\
                         dispersion={:.3},xleave=0.000,\
                         filtdelay={delay_ms:.3},filtoffset={offset_ms:.3},filtdisp={:.3}",
                        dp.stratum, leap_str, dp.root_disp_ms, jitter_ms,
                    )
                } else {
                    String::from(
                        "srcadr=INIT,srcport=123,refid=INIT,\
                         stratum=16,leap=11,hmode=3,pmode=4,\
                         hpoll=6,ppoll=6,reach=0,\
                         delay=0.001,offset=0.000,jitter=0.000,\
                         dispersion=5000.000,xleave=0.000,\
                         filtdelay=0.001,filtoffset=0.000,filtdisp=5000.000",
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

    // RFC 1305 §3.2.1 system status: [LI:2][ClkSrc:6][EvtCode:4][EvtCnt:4]
    // ClkSrc = 4 = UHF/GPS per RFC 1305 Table F-2.
    let clksrc: u16 = if synced { 4 } else { 0 };
    let sys_status: u16 = ((dp.leap as u16) << 14) | (clksrc << 8);
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

/// RFC 5905 precision exponent from worst-case error in microseconds.
///
/// Returns log2(seconds), clamped to [-20, -4] (−20 ≈ 0.95 µs, −4 ≈ 62 ms).
fn ntp_precision_from_uncertainty_us(uncertainty_us: f64) -> i8 {
    let secs = uncertainty_us.max(1.0) / 1_000_000.0;
    secs.log2().floor().clamp(-20.0, -4.0) as i8
}

/// Build a Kiss-o'-Death (KoD) RATE response for a client that is polling
/// too frequently.
///
/// Per RFC 5905 §7.4 the response has:
/// - LI = 3 (alarm), VN mirrored, Mode = 4 (server)
/// - Stratum = 0 (kiss packet)
/// - Reference ID = "RATE" (RFC 5905 Table 6)
/// - Originate Timestamp = client's Transmit Timestamp (bytes 40–47),
///   allowing the client to validate the response.
/// - All other fields zeroed.
///
/// The client MUST stop polling this server upon receiving KoD RATE and MUST
/// reduce its polling interval before trying again.
///
/// # Parameters
/// - `req`: Client's 48-byte NTP request packet.
/// - `version`: NTP version extracted from the request header.
///
/// # Returns
/// - 48-byte KoD response packet.
fn build_kod_response(req: &[u8; NTP_PACKET_LEN], version: u8) -> [u8; NTP_PACKET_LEN] {
    let mut resp = [0_u8; NTP_PACKET_LEN];
    // LI=3 (alarm), VN mirrored from client, Mode=4 (server)
    resp[0] = (3 << 6) | (version << 3) | 4;
    // Stratum 0 = kiss packet (Reference ID carries kiss code, not a clock refid)
    resp[1] = 0;
    // Reference ID = "RATE" kiss code (RFC 5905 Table 6)
    resp[12..16].copy_from_slice(KOD_KISS_RATE);
    // Originate Timestamp = client's Transmit Timestamp (RFC 5905 §7.4)
    resp[24..32].copy_from_slice(&req[40..48]);
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
    resp[3] = dp.precision as u8;

    // Root delay / root dispersion in NTP short format (16.16).
    write_u32_be(&mut resp[4..8], 0);
    write_u32_be(&mut resp[8..12], dp.root_disp_short);

    // Reference ID:
    if dp.stratum == 1 {
        resp[12..16].copy_from_slice(b"GPS\0");
    } else {
        resp[12..16].copy_from_slice(b"INIT");
    }

    // Reference timestamp: time of the last clock discipline event (RFC 5905 §7.3).
    // Remains fixed between PPS pulses so clients can measure reference aging.
    // Zero when unsynced.
    let reference_ts = if dp.stratum == 1 { dp.ref_ts } else { 0 };
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

/// Ephemeral localhost UDP socket for host tests and Criterion benchmarks.
#[cfg(any(test, feature = "bench"))]
impl NtpServer {
    pub fn new_loopback() -> anyhow::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.set_nonblocking(true)?;
        Ok(Self::from_socket(socket))
    }

    /// Return the bound address of the loopback test/bench socket.
    pub fn loopback_addr(&self) -> std::net::SocketAddr {
        self.socket
            .local_addr()
            .expect("loopback NTP socket address")
    }

    fn from_socket(socket: UdpSocket) -> Self {
        Self {
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
            last_sync_ntp_ts: 0,
            rate_limiter: RateLimiter::new(),
            acl: Acl::allow_all(),
            rate_limited_total: 0,
            acl_blocked_total: 0,
            pending_leap: 0,
            pps_glitch_count: 0,
            last_gps_utc_update_us: None,
        }
    }
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
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
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
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
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
        let resp = build_mode6_response(&req, 4, &dp, 250, 500.0, 1200.0, 0.0);
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
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
        assert_eq!(resp.len(), NTP_CONTROL_HEADER_LEN + 4);
    }

    #[test]
    fn build_mode6_readvar_unsynced_system_vars() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
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
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
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
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, 99, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
        assert_ne!(resp[1] & 0x40, 0);
    }

    #[test]
    fn build_mode6_invalid_version_defaults_to_v4() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 0, &dp, 0, 0.0, 0.0, 0.0);
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
        server.update_gps_utc_seconds(1_700_000_000);
        server.pps_now(None);
        let before = server.clock_anchor.expect("anchor").unix_seconds;
        server.pps_now(Some(1_000_000));
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
        assert!(
            (transmit_us - anchor_unix * MICROS_PER_SEC).abs() < 5_000_000,
            "transmit time should track anchored GPS second {anchor_unix}"
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
        assert_eq!(resp[1], 16);
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

    #[test]
    fn servo_accumulates_freq_over_multiple_pulses() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.pps_now(None);
        // Apply ten identical intervals with +100 us error each.
        for _ in 0..10 {
            server.pps_now(Some(1_000_100));
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
        server.pps_now(None);
        // Extreme interval: should saturate at SERVO_MAX_FREQ_PPM.
        for _ in 0..10_000 {
            server.pps_now(Some(1_200_000));
        }
        assert_eq!(server.freq_ppm, SERVO_MAX_FREQ_PPM);
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
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 1.0,
            ref_ts: 0,
            current_ts: 0,
            precision: -11,
        };
        let resp = build_response(&[0_u8; NTP_PACKET_LEN], &dp, 0);
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
        let holdover_secs = 2_010_i64;
        server.pps_stale_ago(None, HOLDOVER_ENTRY_US + holdover_secs * MICROS_PER_SEC);
        let dp = server.discipline_params(true);
        assert_eq!(
            dp.stratum, 16,
            "should be stratum 16 when dispersion >= 1 s"
        );
        assert_eq!(dp.leap, 3);
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
        assert_ne!(
            server.last_sync_ntp_ts, 0,
            "ref ts should be set after first pulse"
        );
    }

    #[test]
    fn last_sync_ntp_ts_updated_on_subsequent_pulse() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        server.update_gps_utc_seconds(1_700_000_000);
        server.pps_now(None);
        let first_ref = server.last_sync_ntp_ts;
        server.pps_now(Some(1_000_000));
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
        let resp = build_response(&req, &dp, receive_ts);
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
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
            ref_ts: 0xDEAD_BEEF_0000_0000,
            current_ts: 0,
            precision: -10,
        };
        let req = [0_u8; NTP_PACKET_LEN];
        let resp = build_response(&req, &dp, 0x1234);
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
        // RFC 1305 §3.2.1: ClkSrc field must be 4 (UHF/GPS) for a GPS reference.
        let dp = DisciplineParams {
            stratum: 1,
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 0.1,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
        let sys_status = u16::from_be_bytes([resp[4], resp[5]]);
        let clksrc = (sys_status >> 8) & 0x3f;
        assert_eq!(
            clksrc, 4,
            "ClkSrc should be 4 (UHF/GPS) per RFC 1305 Table F-2"
        );
        let li = sys_status >> 14;
        assert_eq!(li, 0, "LI should be 0 (no warning) when synced");
    }

    #[test]
    fn build_mode6_system_status_unsynced_has_li_alarm() {
        let dp = DisciplineParams {
            stratum: 16,
            leap: 3,
            root_disp_short: 5 << 16,
            root_disp_ms: 5000.0,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
        let sys_status = u16::from_be_bytes([resp[4], resp[5]]);
        let li = sys_status >> 14;
        assert_eq!(li, 3, "LI should be 3 (alarm) when unsynced");
    }

    #[test]
    fn build_mode6_readstat_peer_status_sel_is_system_peer_when_synced() {
        // sel=6 in the peer status word causes ntpq to display '*' (system peer).
        let dp = DisciplineParams {
            stratum: 1,
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 0.1,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READSTAT, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 0, 0.0, 0.0, 0.0);
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
            leap: 0,
            root_disp_short: 1 << 16,
            root_disp_ms: 0.1,
            ref_ts: 0xE9D1_E7B4_8A3D_71A0,
            current_ts: 0xE9D1_E7B5_0000_0000,
            precision: -10,
        };
        let req = [0, MODE6_OPCODE_READVAR, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let resp = build_mode6_response(&req, 4, &dp, 500, 200.0, 1000.0, 5.321);
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
        let resp = build_mode6_response(&req, 4, &dp, 250, 100.0, 800.0, 3.0);
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
        let resp = build_kod_response(&req, 4);
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
        assert_ne!(buf[1], 0, "first response should not be a kiss packet");

        // Second request immediately after (no delay) — should get KoD.
        client.send_to(&req, server_addr).unwrap();
        server.poll(true).unwrap();
        let (n, _) = client.recv_from(&mut buf).unwrap();
        assert_eq!(n, NTP_PACKET_LEN);
        assert_eq!(buf[1], 0, "second response should be stratum-0 KoD");
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
        req[0] = (4 << 3) | 1; // VN=4, Mode=1 (symmetric active)

        client.send_to(&req, server_addr).unwrap();
        server.poll(true).unwrap();
        let mut buf = [0_u8; 64];
        let (n, _) = client.recv_from(&mut buf).unwrap();
        assert_eq!(n, NTP_PACKET_LEN);
        assert_ne!(buf[1], 0, "first response should not be a kiss packet");

        client.send_to(&req, server_addr).unwrap();
        server.poll(true).unwrap();
        let (n, _) = client.recv_from(&mut buf).unwrap();
        assert_eq!(n, NTP_PACKET_LEN);
        assert_eq!(buf[1], 0, "second response should be stratum-0 KoD");
        assert_eq!(&buf[12..16], b"RATE", "KoD refid should be RATE");
        assert_eq!(server.rate_limited_total, 1);
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
        assert_ne!(buf[1], 0);

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
        assert_eq!(buf[1], 0, "second response should be KoD");

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
        assert_ne!(buf[1], 0, "third response should be a normal NTP reply");
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
            leap: 1, // simulating pending_leap=1 flowing through discipline_params
            root_disp_short: 1 << 16,
            root_disp_ms: 0.1,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let req = [0_u8; NTP_PACKET_LEN];
        let resp = build_response(&req, &dp, 0);
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

    /// 500 000 µs of elapsed time must produce an NTP fraction near
    /// 0x8000_0000 (half the 32-bit range = half a second).
    /// Real elapsed when `current_ntp_timestamp` runs is ≥ 500 000 µs (time
    /// only moves forward), so we accept the range [500 ms, 600 ms).
    #[test]
    fn ntp_fraction_half_second_encodes_correctly() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        let now_us = monotonic_us_now();
        server.clock_anchor = Some(ClockAnchor {
            unix_seconds: 1_700_000_000,
            monotonic_us: now_us - 500_000,
        });
        server.freq_ppm = 0.0;
        let ts = server.current_ntp_timestamp();
        // Convert the sub-second NTP fraction back to microseconds.
        let frac_us = (ts & 0xFFFF_FFFF) * 1_000_000 / (1u64 << 32);
        assert!(
            (500_000..600_000).contains(&frac_us),
            "expected sub-second in 500–600 ms range, got {frac_us} µs"
        );
    }

    /// 250 000 µs of elapsed time must produce an NTP fraction near
    /// 0x4000_0000 (one quarter of the 32-bit range).
    /// Same timing argument as above: accept [250 ms, 350 ms).
    #[test]
    fn ntp_fraction_quarter_second_encodes_correctly() {
        let mut server = NtpServer::new_for_test().expect("test socket");
        let now_us = monotonic_us_now();
        server.clock_anchor = Some(ClockAnchor {
            unix_seconds: 1_700_000_000,
            monotonic_us: now_us - 250_000,
        });
        server.freq_ppm = 0.0;
        let ts = server.current_ntp_timestamp();
        let frac_us = (ts & 0xFFFF_FFFF) * 1_000_000 / (1u64 << 32);
        assert!(
            (250_000..350_000).contains(&frac_us),
            "expected sub-second in 250–350 ms range, got {frac_us} µs"
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
        assert_eq!(server.ntp_snapshot(true).state, DisciplineState::Holdover);

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
        let resp = build_mode6_response(&req, 4, &dp, 0, 1.0, 1.0, 0.0);
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
        let resp = build_mode6_response(&req, 4, &dp, 0, 1.0, 1.0, 0.0);
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
            let resp = build_mode6_response(&req, 4, &dp, 0, 1.0, 1.0, 0.0);
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
        let resp = build_mode6_response(&req, 4, &dp, 0, 1.0, 1.0, 0.0);
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
        let resp = build_response(&req, &dp, 0);
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
            leap: 0,
            root_disp_short: 0,
            root_disp_ms: 0.0,
            ref_ts: 0,
            current_ts: 0,
            precision: -10,
        };
        let mut req = [0_u8; NTP_PACKET_LEN];
        req[0] = (3 << 3) | 3; // VN=3, mode=3 (client)
        let resp = build_response(&req, &dp, 0);
        let vn = (resp[0] >> 3) & 0x07;
        assert_eq!(vn, 3, "server should mirror client VN=3");
    }
}
