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
/// Frequency-estimate smoothing weight.  Each PPS interval error (µs over 1 s,
/// i.e. ppm) is blended into `freq_ppm` as an exponentially weighted moving
/// average with this weight, giving a ~100-pulse time constant.  The estimate
/// converges to the measured offset rather than integrating it without bound.
const SERVO_KI: f64 = 0.01;
/// Client-mode request (RFC 5905 §7.3 mode 3); answered with mode 4.
const MODE_CLIENT: u8 = 3;
/// Symmetric-active request (mode 1); answered with mode 2 without mobilising
/// an association, matching ntpd's `nopeer` behaviour (RFC 5905 §9.2).
const MODE_SYMMETRIC_ACTIVE: u8 = 1;
/// Symmetric-passive reply mode for mode-1 requests.
const MODE_SYMMETRIC_PASSIVE: u8 = 2;
/// Server reply mode for mode-3 requests.
const MODE_SERVER: u8 = 4;
/// Control (ntpq) mode.
const MODE_CONTROL: u8 = 6;
/// Internal stratum value meaning "unsynchronised" (RFC 5905 §7.3 MAXSTRAT).
/// Transmitted packets map this to 0 per the same section.
const STRATUM_UNSYNC: u8 = 16;

// --- Mode-6 (RFC 1305 Appendix B) framing constants ---
/// Response bit in the second header byte.
const MODE6_RESPONSE_BIT: u8 = 0x80;
/// Error bit in the second header byte.
const MODE6_ERROR_BIT: u8 = 0x40;
/// Error status code: invalid opcode (RFC 1305 B.2.4).
const MODE6_ERR_BAD_OPCODE: u16 = 3;
/// Error status code: unknown association identifier (RFC 1305 B.2.4).
const MODE6_ERR_BAD_ASSOC: u16 = 4;
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
///
/// With [`BASE_DISP_US`] + [`HOLDOVER_DISP_RATE_US_PER_SEC`] this is reached
/// (1 s − 1 ms) / 0.5 ms/s ≈ 1998 s after holdover entry, i.e. ≈ 33 min
/// after the last PPS pulse.
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

#[derive(Clone, Copy, Debug)]
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
#[derive(Clone, Copy, Debug)]
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
    /// Total integer-second corrections applied from GPS UTC when the PPS
    /// pulse count disagreed with the NMEA time (missed pulses, leap seconds).
    pub anchor_step_count: u32,
    /// Current leap indicator value being broadcast in NTP responses.
    /// 0 = no warning, 1 = +1 s at end of day, 2 = −1 s at end of day.
    pub leap_indicator: u8,
    /// Smoothed per-packet NTP processing delay in microseconds (EWMA).
    pub proc_delay_us: f32,
    /// True after at least one served request has been timed.
    pub proc_delay_has_sample: bool,
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
            anchor_step_count: 0,
            leap_indicator: 0,
            proc_delay_us: 0.0,
            proc_delay_has_sample: false,
        }
    }
}

/// Discipline state derived from the PPS servo and holdover logic.
///
/// Used to populate stratum, leap indicator, and root dispersion fields in
/// both standard NTP responses and mode-6 control responses.
#[derive(Clone, Copy, Debug)]
struct DisciplineParams {
    /// Internal stratum: 1 when serving, [`STRATUM_UNSYNC`] otherwise.
    stratum: u8,
    /// Locked / Holdover / Unsync classification matching `stratum`.
    state: DisciplineState,
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

/// Servo diagnostics reported through mode-6 READVAR.
#[derive(Clone, Copy, Debug, Default)]
struct ServoStats {
    /// Latest PPS-derived phase offset in microseconds.
    pps_offset_us: i32,
    /// Smoothed PPS jitter estimate in microseconds.
    pps_jitter_us: f32,
    /// Smoothed per-packet processing delay in microseconds.
    proc_delay_us: f32,
    /// Current oscillator frequency offset estimate in ppm.
    freq_ppm: f64,
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
    /// Count of integer-second anchor corrections taken from GPS UTC.
    anchor_step_count: u32,
    /// GPS−predicted second disagreement seen on the previous accepted pulse,
    /// awaiting confirmation by the next one before the anchor is stepped.
    pending_gps_step: Option<i64>,
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

        log::info!("NTP: listening on UDP/123");

        Ok(Self::from_socket(socket))
    }

    /// Wrap an already-bound nonblocking socket in a fresh, unsynchronised server.
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
            anchor_step_count: 0,
            pending_gps_step: None,
            last_gps_utc_update_us: None,
        }
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

    /// Apply compile-time `NTP_ACL_CIDR` and log the effective restriction.
    pub fn apply_boot_acl(&mut self) {
        let acl_cidr = env!("NTP_ACL_CIDR");
        self.set_acl(Acl::from_config(acl_cidr));
        if acl_cidr.is_empty() {
            log::info!("NTP: ACL restricted to RFC 1918 private networks");
        } else {
            log::info!("NTP: ACL restricted to {acl_cidr}");
        }
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

    /// Seed coarse UTC from the RTC before GPS lock.
    ///
    /// Only establishes an anchor when none exists yet.  Does **not** touch
    /// [`Self::last_gps_utc_seconds`] or [`Self::last_gps_utc_update_us`] so a
    /// stale RTC value cannot satisfy the PPS "GPS fresh" guard or masquerade as
    /// a GPS fix.  No-op while PPS locked.
    pub fn seed_utc_seconds(&mut self, utc_unix_seconds: i64) {
        if self.pps_locked || self.clock_anchor.is_some() {
            return;
        }
        self.clock_anchor = Some(ClockAnchor {
            unix_seconds: utc_unix_seconds,
            monotonic_us: monotonic_us_now(),
        });
    }

    /// Update the latest GPS NMEA UTC second and anchor the clock pre-PPS.
    ///
    /// Before PPS lock the anchor is the NMEA second at its parse time.  The
    /// sentence arrives a few hundred milliseconds after the second it names,
    /// so this is a sub-second estimate (served at stratum 16); the exact
    /// edge alignment, including [`NMEA_PPS_FUDGE_S`], happens on the first PPS
    /// pulse.  Always overwrites any RTC bootstrap anchor.  Once PPS discipline
    /// is active only the NMEA record is refreshed; the anchor is then
    /// maintained by [`Self::observe_pps_pulse`], which cross-checks it against
    /// this record on every pulse.
    pub fn update_gps_utc_seconds(&mut self, utc_unix_seconds: i64) {
        self.last_gps_utc_seconds = Some(utc_unix_seconds);
        let now_us = monotonic_us_now();
        self.last_gps_utc_update_us = Some(now_us);
        if self.pps_locked {
            return;
        }
        self.clock_anchor = Some(ClockAnchor {
            unix_seconds: utc_unix_seconds,
            monotonic_us: now_us,
        });
    }

    fn gps_time_fresh(&self, edge_us: i64) -> bool {
        self.last_gps_utc_update_us
            .map(|t| edge_us.saturating_sub(t) < GPS_STALE_THRESHOLD_US)
            .unwrap_or(false)
    }

    /// GPS-derived UTC second marked by a PPS edge at `edge_us`, when GPS is fresh.
    ///
    /// The NMEA sentence carrying UTC second `N` is parsed at `t_nmea`, which on
    /// most modules is 100–900 ms *after* the PPS edge that marks `N`.  An edge
    /// at `edge_us` is therefore `floor((edge_us − t_nmea) / 1 s)` whole seconds
    /// past that reference edge, plus [`NMEA_PPS_FUDGE_S`] to account for which
    /// side of the sentence the edge lies on.  Anchoring on the parse time rather
    /// than assuming "latest NMEA = previous second" keeps the mapping correct
    /// when the main loop processes an edge late, after a newer sentence has
    /// already been parsed, and when the sentence for one second was dropped.
    fn gps_second_at_edge(&self, edge_us: i64) -> Option<i64> {
        if !self.gps_time_fresh(edge_us) {
            return None;
        }
        let gps_utc = self.last_gps_utc_seconds?;
        let t_nmea = self.last_gps_utc_update_us?;
        let whole_secs = edge_us.saturating_sub(t_nmea).div_euclid(MICROS_PER_SEC);
        Some(
            gps_utc
                .saturating_add(whole_secs)
                .saturating_add(NMEA_PPS_FUDGE_S),
        )
    }

    /// Establish (or re-establish) PPS lock with the anchor at `edge_us`.
    fn lock_anchor(&mut self, unix_seconds: i64, edge_us: i64) {
        self.clock_anchor = Some(ClockAnchor {
            unix_seconds,
            monotonic_us: edge_us,
        });
        self.pps_locked = true;
        self.pending_gps_step = None;
        self.last_pps_monotonic_us = Some(edge_us);
        // Reference timestamp (RFC 5905 §7.3): the disciplined time of this
        // edge, i.e. exactly the top of `unix_seconds`.
        self.last_sync_ntp_ts = ntp_timestamp_from_unix(unix_seconds, 0);
    }

    /// Scale raw monotonic elapsed microseconds by the frequency estimate.
    ///
    /// Positive `freq_ppm` means the monotonic clock runs fast, so the elapsed
    /// interval is shrunk to recover true elapsed microseconds.
    fn corrected_elapsed_us(&self, raw_elapsed_us: i64) -> i64 {
        if self.freq_ppm == 0.0 {
            raw_elapsed_us
        } else {
            ((raw_elapsed_us as f64) * (1.0 - self.freq_ppm / 1_000_000.0)) as i64
        }
        .max(0)
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
                let Some(unix_seconds) = self.gps_second_at_edge(now_us) else {
                    log::debug!(
                        "NTP: PPS first pulse skipped — GPS time not fresh (age > {}s)",
                        GPS_STALE_THRESHOLD_US / MICROS_PER_SEC
                    );
                    return;
                };
                self.lock_anchor(unix_seconds, now_us);
                log::debug!("NTP: PPS first pulse, aligned to GPS UTC {unix_seconds}");
            }
            Some(interval) => {
                if !(800_000..=1_200_000).contains(&interval) {
                    // Two or more ISR edges coalesced into one event: the main
                    // loop did not run for over a second.  Not an error for the
                    // clock (the next pulse re-derives the second from elapsed
                    // time) but worth surfacing, since the same stall delays
                    // NMEA parsing and can mis-pair a sentence with an edge.
                    log::warn!("NTP: PPS interval {interval}us out of range; pulse(s) missed");
                    return;
                }

                // First lock may arrive on the Delta path when the initial pulse
                // was skipped (no GPS fix yet) or when pulses accumulated during
                // boot before the main loop ran.  Align to GPS+PPS fudge — do not
                // increment an RTC bootstrap anchor, which lacks NMEA_PPS_FUDGE_S.
                let Some(anchor) = self.clock_anchor.filter(|_| self.pps_locked) else {
                    let Some(unix_seconds) = self.gps_second_at_edge(now_us) else {
                        return;
                    };
                    self.lock_anchor(unix_seconds, now_us);
                    log::debug!("NTP: PPS delta first lock, aligned to GPS UTC {unix_seconds}");
                    return;
                };

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

                // Frequency estimate: the interval error over one second *is* the
                // oscillator offset in ppm, so blend each sample into an EWMA.
                // (A plain integrator would have no feedback term and diverge.)
                self.freq_ppm = (self.freq_ppm
                    + (phase_error_us as f64 - self.freq_ppm) * SERVO_KI)
                    .clamp(-SERVO_MAX_FREQ_PPM, SERVO_MAX_FREQ_PPM);

                // Integer-second accounting.  Never assume exactly one second has
                // passed since the previous accepted pulse: pulses are missed when
                // the main loop stalls (two ISR edges collapse into one ~2 s
                // interval that the range filter rejects) or when the outlier
                // guard drops them.  Count whole seconds from the frequency-
                // corrected monotonic elapsed time, then cross-check against the
                // GPS UTC second for this edge when NMEA is fresh.  GPS wins on
                // *persistent* disagreement (leap seconds, GPS time steps): a
                // single disagreeing pulse is not acted on, because the
                // sentence↔edge pairing is ambiguous when the main loop stalled
                // and parsed a backlog sentence after the next edge had already
                // fired (the pairing then reads one second low for exactly one
                // pulse).  Requiring the same delta on two consecutive accepted
                // pulses rejects that transient while still following a real
                // step within two seconds.
                let raw_elapsed_us = now_us.saturating_sub(anchor.monotonic_us);
                let elapsed_secs = (self.corrected_elapsed_us(raw_elapsed_us) + MICROS_PER_SEC / 2)
                    .div_euclid(MICROS_PER_SEC);
                let predicted = anchor.unix_seconds.saturating_add(elapsed_secs);
                let unix_seconds = match self.gps_second_at_edge(now_us) {
                    Some(gps) if gps != predicted => {
                        let delta = gps - predicted;
                        if self.pending_gps_step == Some(delta) {
                            self.pending_gps_step = None;
                            self.anchor_step_count = self.anchor_step_count.saturating_add(1);
                            log::warn!(
                                "NTP: PPS second corrected from GPS: predicted {predicted}, GPS {gps} ({delta:+}s, step #{})",
                                self.anchor_step_count
                            );
                            gps
                        } else {
                            self.pending_gps_step = Some(delta);
                            // Negative "NMEA lead" means the latest sentence was
                            // parsed after this edge fired: the signature of a
                            // late-parsed backlog sentence rather than a real step.
                            let nmea_lead_ms = self
                                .last_gps_utc_update_us
                                .map(|t| (now_us - t) / 1_000)
                                .unwrap_or(0);
                            log::info!(
                                "NTP: GPS/PPS second disagree (predicted {predicted}, GPS {gps}, {delta:+}s, NMEA lead {nmea_lead_ms} ms); awaiting confirmation"
                            );
                            predicted
                        }
                    }
                    _ => {
                        self.pending_gps_step = None;
                        predicted
                    }
                };

                // Proportional path: nudge the anchor's monotonic reference to
                // correct the phase by a fraction of the observed error.
                // Subtracting the correction shifts the apparent second boundary
                // earlier when the clock is fast, and later when it is slow.
                let phase_correction_us = (phase_error_us as f64 * SERVO_KP) as i64;
                self.clock_anchor = Some(ClockAnchor {
                    unix_seconds,
                    monotonic_us: now_us.saturating_sub(phase_correction_us),
                });

                // Update PPS diagnostics used by mode-6 and ntpq display.
                let sample_jitter = phase_error_us.unsigned_abs() as f32;
                self.pps_offset_us = phase_error_us as i32;
                if self.pps_has_sample {
                    self.pps_jitter_us = self.pps_jitter_us * 0.8 + sample_jitter * 0.2;
                } else {
                    self.pps_jitter_us = sample_jitter;
                    self.pps_has_sample = true;
                }

                self.last_pps_monotonic_us = Some(now_us);
                // Reference timestamp = disciplined time of this edge, so clients
                // can observe reference aging between pulses (RFC 5905 §7.3).
                self.last_sync_ntp_ts = ntp_timestamp_from_unix(unix_seconds, 0);
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

        // Stratum 1 requires live GPS+PPS, or holdover from a PPS-disciplined
        // anchor while the modelled root dispersion stays below
        // HOLDOVER_UNSYNC_US (≈33 min at HOLDOVER_DISP_RATE_US_PER_SEC).
        // Clients apply their own `maxdistance` policy to the growing
        // dispersion.  An RTC/GPS bootstrap anchor that was never PPS-locked
        // stays at stratum 16.
        let holdover_ok = self.pps_locked && has_anchor && disp_us < HOLDOVER_UNSYNC_US;
        let stratum = if fully_synced || holdover_ok {
            1_u8
        } else {
            STRATUM_UNSYNC
        };
        let state = if fully_synced {
            DisciplineState::Locked
        } else if stratum == 1 {
            DisciplineState::Holdover
        } else {
            DisciplineState::Unsync
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
            state,
            leap,
            root_disp_short,
            root_disp_ms,
            ref_ts: self.last_sync_ntp_ts,
            current_ts: self.current_ntp_timestamp(),
            precision,
        }
    }

    /// Servo diagnostics for mode-6 READVAR output.
    fn servo_stats(&self) -> ServoStats {
        ServoStats {
            pps_offset_us: self.pps_offset_us,
            pps_jitter_us: self.pps_jitter_us,
            proc_delay_us: self.proc_delay_us,
            freq_ppm: self.freq_ppm,
        }
    }

    /// Current disciplined UTC seconds when a clock anchor exists.
    ///
    /// Uses the same frequency-corrected elapsed time as [`Self::current_ntp_timestamp`].
    pub fn current_utc_unix_seconds(&self) -> Option<i64> {
        let anchor = self.clock_anchor?;
        let now_us = monotonic_us_now();
        let raw_elapsed_us = now_us.saturating_sub(anchor.monotonic_us);
        let elapsed_seconds = self
            .corrected_elapsed_us(raw_elapsed_us)
            .div_euclid(MICROS_PER_SEC);
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

        NtpSnapshot {
            stratum: dp.stratum,
            state: dp.state,
            freq_ppm: self.freq_ppm,
            pps_offset_us: self.pps_offset_us,
            pps_jitter_us: self.pps_jitter_us,
            root_disp_ms: dp.root_disp_ms,
            served: self.served,
            rate_limited: self.rate_limited_total,
            acl_blocked: self.acl_blocked_total,
            pps_glitch_count: self.pps_glitch_count,
            anchor_step_count: self.anchor_step_count,
            leap_indicator: dp.leap,
            proc_delay_us: if self.proc_delay_has_sample {
                self.proc_delay_us
            } else {
                0.0
            },
            proc_delay_has_sample: self.proc_delay_has_sample,
        }
    }

    /// Total NTP requests served since boot (time and mode-6).
    pub fn served(&self) -> u64 {
        self.served
    }

    /// Update the smoothed per-packet processing delay estimate.
    fn record_proc_delay(&mut self, started_us: i64) {
        let finished_us = monotonic_us_now();
        let sample_us = (finished_us.saturating_sub(started_us)).max(1) as f32;
        if self.proc_delay_has_sample {
            self.proc_delay_us = self.proc_delay_us * 0.8 + sample_us * 0.2;
        } else {
            self.proc_delay_us = sample_us;
            self.proc_delay_has_sample = true;
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
                        MODE_CONTROL => {
                            if len < NTP_CONTROL_HEADER_LEN {
                                log::debug!(
                                    "NTP: ignoring short mode-6 request ({} bytes) from {}",
                                    len,
                                    peer
                                );
                                continue;
                            }
                            // A set response bit marks a reply, not a request
                            // (RFC 1305 B.1).  Answering it would let a spoofed
                            // packet bounce traffic between two servers.
                            if req[1] & MODE6_RESPONSE_BIT != 0 {
                                log::debug!("NTP: ignoring mode-6 response packet from {}", peer);
                                continue;
                            }
                            if !self.try_rate_limit(src_ip_v4) {
                                log::debug!("NTP: mode-6 rate limited from {}", peer);
                                continue;
                            }
                            let dp = self.discipline_params(gps_fix);
                            let stats = self.servo_stats();
                            let started_us = monotonic_us_now();
                            let resp = build_mode6_response(&req[..len], version, &dp, &stats);
                            self.socket.send_to(&resp, peer).with_context(|| {
                                format!("failed to send mode-6 response to {}", peer)
                            })?;
                            self.record_proc_delay(started_us);
                        }
                        MODE_CLIENT | MODE_SYMMETRIC_ACTIVE => {
                            if len < NTP_PACKET_LEN {
                                log::debug!(
                                    "NTP: ignoring short time request ({} bytes) from {}",
                                    len,
                                    peer
                                );
                                continue;
                            }

                            // RFC 5905 §9.2: a client (mode 3) request is answered
                            // in server mode (4); a symmetric-active (mode 1) request
                            // is answered in symmetric-passive mode (2).
                            let reply_mode = if mode == MODE_CLIENT {
                                MODE_SERVER
                            } else {
                                MODE_SYMMETRIC_PASSIVE
                            };

                            let req48: &[u8; NTP_PACKET_LEN] = req[..NTP_PACKET_LEN]
                                .try_into()
                                .expect("slice is exactly NTP_PACKET_LEN bytes");

                            // Rate limit time requests; mode-6 uses the same limiter
                            // but drops silently.
                            if !self.try_rate_limit(src_ip_v4) {
                                let resp = build_kod_response(req48, version, reply_mode);
                                let _ = self.socket.send_to(&resp, peer);
                                log::debug!("NTP: KoD RATE sent to {} (rate limited)", peer);
                                continue;
                            }

                            let dp = self.discipline_params(gps_fix);
                            let started_us = monotonic_us_now();
                            let mut resp = build_response(req48, &dp, receive_ntp_ts, reply_mode);
                            let transmit_ts = self.current_ntp_timestamp();
                            write_u64_be(&mut resp[40..48], transmit_ts);

                            self.socket.send_to(&resp, peer).with_context(|| {
                                format!("failed to send NTP response to {}", peer)
                            })?;
                            self.record_proc_delay(started_us);
                        }
                        _ => {
                            // RFC 5905 §9.2 Table 10: server (4), symmetric-passive
                            // (2), broadcast (5), reserved (0) and private (7)
                            // packets with no association are discarded.  Answering
                            // them would let a spoofed mode-4 packet bounce replies
                            // between two servers indefinitely.
                            log::debug!("NTP: discarding mode-{} packet from {}", mode, peer);
                            continue;
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
/// RFC 1305 Appendix B convention for NTP timestamps in control-message text
/// is the hex representation of the 32-bit seconds field followed by the 32-bit
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
/// Framing follows RFC 1305 Appendix B (and the ntpq implementation that
/// remains the de-facto reference): the response bit is set, the opcode and
/// sequence are echoed, and errors are signalled with the error bit plus an
/// error status word (`code << 8`, RFC 1305 B.2.4) in place of the system
/// status word.
///
/// # Parameters
/// - `req`: Raw mode-6 request bytes (at least `NTP_CONTROL_HEADER_LEN`).
/// - `version`: NTP version extracted from request header.
/// - `dp`: Discipline parameters (stratum, leap, dispersion, timestamps).
/// - `stats`: Servo diagnostics (offset, jitter, processing delay, frequency).
///
/// # Returns
/// - Serialized mode-6 response bytes ready to send.
fn build_mode6_response(
    req: &[u8],
    version: u8,
    dp: &DisciplineParams,
    stats: &ServoStats,
) -> Vec<u8> {
    let mut resp = vec![0_u8; NTP_CONTROL_HEADER_LEN];
    let vn = reply_version(version);
    let opcode = req[1] & 0x1f;
    let associd = u16::from_be_bytes([req[6], req[7]]);
    let mut payload: Vec<u8> = Vec::new();
    // `Some(code)` selects an error response with that RFC 1305 B.2.4 code.
    let mut error: Option<u16> = None;

    let synced = dp.stratum == 1;
    // Mode-6 text renders the leap indicator as its 2-bit binary value.
    let leap_str = format!("{:02b}", dp.leap & 0x03);

    // RFC 1305 B.2.2 peer status word:
    // [Sel:3][Cfg:1][Auth:1][AuthOK:1][Reach:1][Bcast:1][EvtCode:4][EvtCnt:4]
    // sel=6 (system peer, shown as '*' by ntpq), cfg=1, reach=1 when synced;
    // sel=0 (rejected), cfg=1 when unsynced.
    let peer_status: u16 = if synced {
        (6_u16 << 13) | (1 << 12) | (1 << 9) // 0xD200
    } else {
        1 << 12 // 0x1000: configured but not selected
    };

    // LI=0, VN from request, Mode=6 (control)
    resp[0] = (vn << 3) | MODE_CONTROL;
    match opcode {
        MODE6_OPCODE_READSTAT => {
            if associd == 0 {
                // Summary: one (assoc id, peer status) pair per association.
                payload.extend_from_slice(&MODE6_ASSOC_ID.to_be_bytes());
                payload.extend_from_slice(&peer_status.to_be_bytes());
            } else if associd != MODE6_ASSOC_ID {
                error = Some(MODE6_ERR_BAD_ASSOC);
            }
            // For our single association the peer status word is returned in
            // the header status field (see below) with no payload.
        }
        MODE6_OPCODE_READVAR => {
            let vars = if associd == 0 {
                // System variables (RFC 5905 §7.3 + common ntpq fields).
                // Expanded with frequency, reference timestamp, clock, jitter,
                // and wander so ntpq -c rv gives a complete diagnostic picture.
                if synced {
                    let offset_ms = stats.pps_offset_us as f64 / 1_000.0;
                    let jitter_ms = (stats.pps_jitter_us.max(1.0)) as f64 / 1_000.0;
                    let freq_ppm = stats.freq_ppm;
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
                    let offset_ms = stats.pps_offset_us as f64 / 1_000.0;
                    let jitter_ms = (stats.pps_jitter_us.max(1.0)) as f64 / 1_000.0;
                    let delay_ms = (stats.proc_delay_us.max(1.0)) as f64 / 1_000.0;
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
                error = Some(MODE6_ERR_BAD_ASSOC);
                String::new()
            };
            payload.extend_from_slice(vars.as_bytes());
        }
        _ => {
            // Unsupported opcode: reply with a mode-6 error response.
            error = Some(MODE6_ERR_BAD_OPCODE);
        }
    }

    // Response bit set, error bit on failure, opcode echoed from request.
    let error_bit = if error.is_some() { MODE6_ERROR_BIT } else { 0 };
    resp[1] = MODE6_RESPONSE_BIT | error_bit | opcode;
    // Sequence from request.
    resp[2] = req[2];
    resp[3] = req[3];

    // Status word (RFC 1305 B.2):
    // - error response:            error status word  [Code:8][Reserved:8]
    // - READSTAT for one assoc:    that peer's status word
    // - otherwise, system status:  [LI:2][ClkSrc:6][EvtCode:4][EvtCnt:4]
    //   ClkSrc = 4 = UHF (satellite) radio per RFC 1305 B.2.1.
    let status: u16 = if let Some(code) = error {
        code << 8
    } else if opcode == MODE6_OPCODE_READSTAT && associd != 0 {
        peer_status
    } else {
        let clksrc: u16 = if synced { 4 } else { 0 };
        ((dp.leap as u16) << 14) | (clksrc << 8)
    };
    resp[4..6].copy_from_slice(&status.to_be_bytes());
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
/// - LI = 3 (alarm), VN mirrored, Mode = reply mode for the request (4 for a
///   client request, 2 for symmetric active)
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
/// - `reply_mode`: Mode field for the reply (`MODE_SERVER` or `MODE_SYMMETRIC_PASSIVE`).
///
/// # Returns
/// - 48-byte KoD response packet.
fn build_kod_response(
    req: &[u8; NTP_PACKET_LEN],
    version: u8,
    reply_mode: u8,
) -> [u8; NTP_PACKET_LEN] {
    let mut resp = [0_u8; NTP_PACKET_LEN];
    // LI=3 (alarm), VN mirrored from client, Mode per request type
    resp[0] = (3 << 6) | (reply_version(version) << 3) | (reply_mode & 0x07);
    // Stratum 0 = kiss packet (Reference ID carries kiss code, not a clock refid)
    resp[1] = 0;
    // Reference ID = "RATE" kiss code (RFC 5905 Table 6)
    resp[12..16].copy_from_slice(KOD_KISS_RATE);
    // Originate Timestamp = client's Transmit Timestamp (RFC 5905 §7.4)
    resp[24..32].copy_from_slice(&req[40..48]);
    resp
}

/// Version number to place in a reply to a request carrying `request_vn`.
///
/// RFC 5905 §7.3 defines versions 1–4; those are mirrored so the client can
/// match the reply.  Anything else (0, or 5–7 from a future or corrupt
/// packet) is answered as version 4.
fn reply_version(request_vn: u8) -> u8 {
    if (1..=4).contains(&request_vn) {
        request_vn
    } else {
        4
    }
}

/// Build a standard 48-byte NTP server response template.
///
/// # Parameters
/// - `req`: Parsed 48-byte client request.
/// - `dp`: Discipline parameters (stratum, leap, dispersion).
/// - `receive_ts`: Server receive timestamp in NTP 64-bit format.
/// - `reply_mode`: Mode field for the reply (`MODE_SERVER` for mode-3 requests,
///   `MODE_SYMMETRIC_PASSIVE` for mode-1 requests).
///
/// # Returns
/// - 48-byte response packet with originate/receive/reference fields populated.
fn build_response(
    req: &[u8; NTP_PACKET_LEN],
    dp: &DisciplineParams,
    receive_ts: u64,
    reply_mode: u8,
) -> [u8; NTP_PACKET_LEN] {
    let mut resp = [0_u8; NTP_PACKET_LEN];
    let version = reply_version((req[0] >> 3) & 0x07);
    resp[0] = (dp.leap << 6) | (version << 3) | (reply_mode & 0x07);
    // RFC 5905 §7.3: the internal "unsynchronised" stratum (16) is transmitted
    // as 0.  Clients treat 0 and 16 alike (both invalid), but 0 is the value
    // the spec puts on the wire.
    resp[1] = if dp.stratum >= STRATUM_UNSYNC {
        0
    } else {
        dp.stratum
    };
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
            let corrected_elapsed_us = self.corrected_elapsed_us(raw_elapsed_us);

            let elapsed_seconds = corrected_elapsed_us.div_euclid(MICROS_PER_SEC);
            let rem_us = corrected_elapsed_us.rem_euclid(MICROS_PER_SEC) as u64;

            let unix_seconds = anchor.unix_seconds.saturating_add(elapsed_seconds);
            ntp_timestamp_from_unix(unix_seconds, rem_us)
        } else {
            ntp_timestamp_now()
        }
    }
}

/// Pack Unix seconds plus a sub-second microsecond remainder into an NTP
/// 64-bit timestamp (RFC 5905 §6).  Pre-1970 instants clamp to the Unix epoch.
fn ntp_timestamp_from_unix(unix_seconds: i64, rem_us: u64) -> u64 {
    let ntp_seconds = if unix_seconds >= 0 {
        (unix_seconds as u64).saturating_add(NTP_UNIX_EPOCH_OFFSET_SECS)
    } else {
        NTP_UNIX_EPOCH_OFFSET_SECS
    };
    let ntp_fraction = ((rem_us as u128) * NTP_FRAC_SCALE / 1_000_000u128) as u64;
    (ntp_seconds << 32) | ntp_fraction
}

/// Return monotonic microseconds since an arbitrary boot-time origin.
///
/// On ESP-IDF this is `esp_timer_get_time()`.  On the host it is a
/// process-local [`std::time::Instant`] origin so the value is monotonic and
/// unaffected by wall-clock steps (which would otherwise corrupt the
/// PPS-interval and holdover arithmetic that depends on this clock).
fn monotonic_us_now() -> i64 {
    #[cfg(target_os = "espidf")]
    {
        // SAFETY: `esp_timer_get_time` is a plain FFI read of the 64-bit
        // high-resolution timer with no preconditions; it is safe to call from
        // any task context.
        return unsafe { sys::esp_timer_get_time() };
    }
    #[cfg(not(target_os = "espidf"))]
    {
        #[cfg(test)]
        if let Some(fixed) = test_clock::override_us() {
            return fixed;
        }
        use std::sync::OnceLock;
        use std::time::Instant;
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let origin = *ORIGIN.get_or_init(Instant::now);
        // Offset so the clock never reads near zero: several helpers subtract
        // multi-second intervals from "now" and must stay in positive range.
        const HOST_CLOCK_OFFSET_US: i64 = 1_000 * MICROS_PER_SEC;
        HOST_CLOCK_OFFSET_US + origin.elapsed().as_micros() as i64
    }
}

/// Test-only override of [`monotonic_us_now`] so timing arithmetic can be
/// checked exactly instead of within a tolerance window.
///
/// The override is thread-local because `cargo test` runs tests concurrently.
#[cfg(all(test, not(target_os = "espidf")))]
mod test_clock {
    use std::cell::Cell;

    thread_local! {
        static OVERRIDE_US: Cell<Option<i64>> = const { Cell::new(None) };
    }

    /// Current override for this thread, if any.
    pub(super) fn override_us() -> Option<i64> {
        OVERRIDE_US.with(Cell::get)
    }

    /// Pin the monotonic clock to `now_us` for the calling thread until the
    /// returned guard is dropped.
    pub(super) fn freeze(now_us: i64) -> FrozenClock {
        OVERRIDE_US.with(|c| c.set(Some(now_us)));
        FrozenClock
    }

    /// Guard that restores the real clock when dropped.
    pub(super) struct FrozenClock;

    impl FrozenClock {
        /// Move the frozen clock to a new instant.
        pub(super) fn set(&self, now_us: i64) {
            OVERRIDE_US.with(|c| c.set(Some(now_us)));
        }
    }

    impl Drop for FrozenClock {
        fn drop(&mut self) {
            OVERRIDE_US.with(|c| c.set(None));
        }
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
}

#[cfg(test)]
mod tests;
