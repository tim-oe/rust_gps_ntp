//! Firmware orchestrator: peripheral init, UI task spawn, and the main service loop.
//!
//! The main loop handles GPS ingest, PPS discipline, NTP polling, and timezone
//! coordination. Display, button, and battery sampling run on [`crate::ui_task`].
//! Hardware bring-up is handled by [`crate::board::BoardBoot`].
//!
//! # Performance architecture
//!
//! Several choices were made specifically to keep NTP response latency low:
//!
//! * **Wi-Fi power save disabled** (`WIFI_PS_NONE` in [`crate::wifi`]): the default
//!   `WIFI_PS_MIN_MODEM` mode buffers incoming UDP packets at the AP for up to ~100 ms
//!   (one DTIM interval), dominating NTP round-trip time.
//!
//! * **Non-blocking UART reads** (`timeout = 0` in [`poll_gps_uart`]): a blocking read
//!   with even a 25-tick timeout stalls the loop for up to 250 ms, queueing NTP
//!   packets and adding the same latency as power save.
//!
//! * **1 ms loop sleep** (`FreeRtos::delay_ms(1)`): reduces the worst-case time
//!   between a UDP packet arriving and `poll()` processing it.  Combined with
//!   `CONFIG_FREERTOS_HZ=1000` (1 kHz tick rate), this cuts the D/2 NTP
//!   4-timestamp bias from ~5 ms to ~0.5 ms.
//!
//! * **ISR-captured PPS timestamp** (`edge_us` from [`crate::pps`]): the PPS edge
//!   time is recorded in the GPIO ISR and passed directly to the clock anchor,
//!   bypassing the ~10–100 ms task-scheduling delay that would accumulate if the
//!   clock were read at the point `poll()` processes the event.
//!
//! * **Task priorities** (`CONFIG_ESP_MAIN_TASK_PRIO=10` in `sdkconfig.defaults`):
//!   the ESP-IDF main task defaults to priority 1, below the default pthread
//!   priority of 5 used by `std::thread`.  The UI task (priority 5) and timezone
//!   worker (priority 2) are explicitly set below the NTP loop so display work
//!   never preempts time-critical packet processing.

use esp_idf_svc::hal::delay::FreeRtos;

use crate::board::BoardBoot;
use crate::gps::{self, GpsSnapshot, GpsUart};
use crate::logging;
use crate::ntp::{self, DisciplineState};
use crate::pps::{self, PpsEvent};
use crate::rtc;
use crate::storage::{self, StorageStatus};
use crate::timezone::{self, TimezoneStore, TimezoneWorker};
use crate::ui_task::UiFeed;
use crate::wifi;

/// Initialize peripherals, spawn the UI task, and run the main service loop.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - `Ok(())` only if the main loop exits cleanly (normally it runs forever).
/// - `Err` when peripheral init, UI task spawn, or NTP bind fails.
pub fn run() -> anyhow::Result<()> {
    logging::init();
    let wifi_creds = wifi::load_wifi_credentials_from_env()?;
    let board = BoardBoot::boot(&wifi_creds)?;
    let mut ntp_server = board.init_ntp_server()?;
    let (runtime, _keepalive) = board.into_runtime();

    let gps_uart = runtime.gps_uart;
    let mut pps = runtime.pps;
    let ui_feed = runtime.ui_feed;
    let rtc_present = runtime.rtc_present;
    let boot_rtc_unix = runtime.boot_rtc_unix;
    let mut gps = runtime.timezone.gps;
    let mut tz_store = runtime.timezone.tz_store;
    let mut tz_initialized = runtime.timezone.tz_initialized;
    let mut current_tz_name = runtime.timezone.current_tz_name;

    let mut rx_buf = [0_u8; 256];
    let mut line_buf = String::new();
    let mut bytes_seen: u64 = 0;

    let mut tz_worker = TimezoneWorker::spawn().ok();
    let mut last_tz_lookup_us = 0_i64;
    let mut last_ntp_publish_us = 0_i64;
    let mut last_ntp_served = 0_u64;
    let mut last_rtc_fallback_us = 0_i64;
    let mut last_rtc_write_us = 0_i64;
    let mut last_rtc_utc = boot_rtc_unix;
    let mut last_storage_refresh_us = 0_i64;

    // --- Self-check state: track first-event milestones and emit timeout warnings. ---
    let boot_us = monotonic_us();
    let mut first_nmea_logged = false;
    let mut first_fix_logged = false;
    let mut first_pps_logged = false;
    let mut first_ntp_client_logged = false;
    let mut warn_no_nmea_done = false;
    let mut warn_no_pps_done = false;
    let mut rtc_seeded_from_gps = boot_rtc_unix.is_some();

    loop {
        poll_gps_uart(
            &gps_uart,
            &mut rx_buf,
            &mut line_buf,
            &mut bytes_seen,
            &mut gps,
            &ui_feed,
            &mut ntp_server,
            tz_worker.as_mut(),
            &mut tz_initialized,
            &mut last_tz_lookup_us,
        );

        if let Some(worker) = tz_worker.as_mut() {
            if let Some(result) = worker.poll() {
                apply_timezone_lookup_result(
                    result,
                    gps.lat,
                    gps.lon,
                    tz_store.as_mut(),
                    &mut tz_initialized,
                    &mut current_tz_name,
                );
                if tz_initialized {
                    let utc = gps
                        .utc_unix_seconds
                        .or_else(|| ui_feed.rtc().utc_unix_seconds);
                    if let Some(utc) = utc {
                        gps.tz_offset_hours = gps::tz_offset_hours_at_unix(utc);
                        ui_feed.publish_gps(&gps);
                    }
                }
            }
        }

        if let Err(err) = ntp_server.poll(gps.fix) {
            log::warn!("NTP: poll failed: {}", err);
        } else {
            let served = ntp_server.served();
            if served > last_ntp_served {
                ui_feed.publish_ntp(ntp_server.ntp_snapshot(gps.fix));
                last_ntp_publish_us = monotonic_us();
                last_ntp_served = served;
            }
        }

        maybe_apply_rtc_fallback(
            &ui_feed,
            &mut ntp_server,
            gps.fix,
            &mut last_rtc_fallback_us,
            &mut last_rtc_utc,
        );

        let mut now_us = monotonic_us();
        maybe_writeback_rtc(
            &ui_feed,
            &ntp_server,
            gps.fix,
            now_us,
            &mut last_rtc_write_us,
        );

        if ui_feed.storage().mounted
            && (last_storage_refresh_us == 0
                || (now_us - last_storage_refresh_us) >= storage::STATUS_REFRESH_US)
        {
            ui_feed.publish_storage(StorageStatus::refresh(
                storage::MOUNT_POINT,
                ui_feed.storage(),
            ));
            last_storage_refresh_us = now_us;
        }

        if bytes_seen > 0 && bytes_seen % 512 == 0 {
            log::debug!("GPS: diagnostics bytes received={}", bytes_seen);
        }

        if let Some(event) = pps.poll() {
            match event {
                PpsEvent::First { edge_us } => {
                    first_pps_logged = true;
                    log::info!(
                        "PPS: first pulse received (+{}s)",
                        (monotonic_us() - boot_us) / 1_000_000
                    );
                    ntp_server.observe_pps_pulse(None, edge_us);
                }
                PpsEvent::Delta {
                    interval_us,
                    edge_us,
                } => {
                    ui_feed.publish_pps_delta(interval_us);
                    log::debug!("PPS: pulse #{} delta={}us", pps.pulse_count(), interval_us);
                    ntp_server.observe_pps_pulse(Some(interval_us), edge_us);
                }
            }
            // Publish fresh discipline metrics whenever PPS fires (every ~1 s when locked).
            ui_feed.publish_ntp(ntp_server.ntp_snapshot(gps.fix));
            last_ntp_publish_us = monotonic_us();
            if let Err(err) = pps.reenable_interrupt() {
                log::warn!("PPS: failed to re-enable interrupt: {}", err);
            }
        }

        // During holdover the dispersion grows with time; refresh the display
        // snapshot every second so the UI reflects current uncertainty.
        now_us = monotonic_us();
        if (now_us - last_ntp_publish_us) >= 1_000_000 {
            ui_feed.publish_ntp(ntp_server.ntp_snapshot(gps.fix));
            last_ntp_publish_us = now_us;
        }

        // 1 ms sleep keeps the loop at ~1 kHz: fast enough to respond to NTP
        // requests within ~0.5 ms on average (D/2 bias in 4-timestamp offset),
        // while still yielding to lower-priority tasks and keeping GPS UART
        // drained (9600 baud delivers ~1 byte/ms so 1 ms reads are sufficient).
        FreeRtos::delay_ms(1);

        // --- Boot self-checks: log key lifecycle milestones once, warn on stalls. ---
        let elapsed_s = (monotonic_us() - boot_us) / 1_000_000;

        if !first_nmea_logged && bytes_seen > 0 {
            first_nmea_logged = true;
            log::info!("GPS UART: first NMEA data received (+{}s)", elapsed_s);
        }

        if !first_fix_logged && gps.fix {
            first_fix_logged = true;
            log::info!(
                "GPS: first fix acquired — sats={} lat={:.5} lon={:.5} (+{}s)",
                gps.sats,
                gps.lat,
                gps.lon,
                elapsed_s
            );
            if rtc_present && !rtc_seeded_from_gps {
                if let Some(utc) = gps.utc_unix_seconds {
                    ui_feed.request_rtc_write(utc);
                    rtc_seeded_from_gps = true;
                    log::info!("RTC: queued initial time set from GPS fix (UTC {utc})");
                }
            }
        }

        if !first_ntp_client_logged {
            let snap = ntp_server.ntp_snapshot(gps.fix);
            if snap.served > 0 {
                first_ntp_client_logged = true;
                log::info!("NTP: first client request served (+{}s)", elapsed_s);
            }
        }

        // Warn once if expected events don't arrive within the expected window.
        if !warn_no_nmea_done && !first_nmea_logged && elapsed_s >= 10 {
            warn_no_nmea_done = true;
            log::warn!(
                "GPS UART: no data in {}s — check GPS module power and wiring \
                 (UART1 TX=GPIO{} RX=GPIO{})",
                elapsed_s,
                gps::UART_TX_PIN,
                gps::UART_RX_PIN
            );
        }

        if !warn_no_pps_done && first_nmea_logged && !first_pps_logged && elapsed_s >= 30 {
            warn_no_pps_done = true;
            log::warn!(
                "PPS: no pulse in {}s since boot — check PPS pin wiring (GPIO{})",
                elapsed_s,
                pps::GPIO_PIN
            );
        }
    }
}

/// Feed RTC-cached UTC into the NTP anchor when GPS fix is unavailable.
fn maybe_apply_rtc_fallback(
    ui_feed: &UiFeed,
    ntp_server: &mut ntp::NtpServer,
    gps_fix: bool,
    last_rtc_fallback_us: &mut i64,
    last_rtc_utc: &mut Option<i64>,
) {
    if gps_fix {
        return;
    }

    let rtc = ui_feed.rtc();
    if !rtc.detected {
        return;
    }
    let Some(secs) = rtc.utc_unix_seconds else {
        return;
    };

    let now_us = monotonic_us();
    let due =
        *last_rtc_fallback_us == 0 || (now_us - *last_rtc_fallback_us) >= rtc::FALLBACK_INTERVAL_US;
    if !due {
        return;
    }
    if last_rtc_utc != &Some(secs) {
        log::debug!("RTC: feeding cached UTC {secs} (GPS fix lost)");
    }
    ntp_server.seed_utc_seconds(secs);
    *last_rtc_utc = Some(secs);
    *last_rtc_fallback_us = now_us;
}

/// Queue a PCF8523 write when GPS is locked and discipline has a valid anchor.
fn maybe_writeback_rtc(
    ui_feed: &UiFeed,
    ntp_server: &ntp::NtpServer,
    gps_fix: bool,
    now_us: i64,
    last_rtc_write_us: &mut i64,
) {
    if !gps_fix {
        return;
    }
    if !matches!(
        ntp_server.ntp_snapshot(gps_fix).state,
        DisciplineState::Locked
    ) {
        return;
    }
    if *last_rtc_write_us != 0 && (now_us - *last_rtc_write_us) < rtc::WRITEBACK_INTERVAL_US {
        return;
    }
    if let Some(secs) = ntp_server.current_utc_unix_seconds() {
        ui_feed.request_rtc_write(secs);
        *last_rtc_write_us = now_us;
    }
}

/// Read monotonic time from the ESP high-resolution timer.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Monotonic timestamp in microseconds since boot.
fn monotonic_us() -> i64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}

/// Read and parse available GPS UART bytes, updating shared state and NTP inputs.
///
/// # Parameters
/// - `gps_uart`: GPS NMEA UART driver.
/// - `rx_buf`: Scratch buffer for UART reads.
/// - `line_buf`: Accumulator for partial NMEA lines spanning reads.
/// - `bytes_seen`: Running total of UART bytes received (diagnostics).
/// - `gps`: Mutable GPS snapshot updated by parsed sentences.
/// - `ui_feed`: Shared feed published to the UI task after successful parses.
/// - `ntp_server`: NTP server receiving UTC updates from valid RMC fixes.
/// - `tz_worker`: Optional background timezone lookup worker.
/// - `tz_initialized`: Whether a valid runtime timezone is already configured.
/// - `last_tz_lookup_us`: Monotonic timestamp of the last timezone lookup request.
///
/// # Returns
/// - No return value.
fn poll_gps_uart(
    gps_uart: &GpsUart,
    rx_buf: &mut [u8; 256],
    line_buf: &mut String,
    bytes_seen: &mut u64,
    gps: &mut GpsSnapshot,
    ui_feed: &UiFeed,
    ntp_server: &mut ntp::NtpServer,
    mut tz_worker: Option<&mut TimezoneWorker>,
    tz_initialized: &mut bool,
    last_tz_lookup_us: &mut i64,
) {
    // timeout=0: non-blocking read.  A blocking timeout (e.g. 25 ticks at
    // 100 Hz = 250 ms) stalls the loop and queues NTP packets for the same
    // duration, adding it directly to NTP round-trip time.  At 9600 baud GPS
    // delivers ~1 byte/ms, so a 1 ms loop drains the UART buffer adequately.
    let Ok(read) = gps_uart.read(rx_buf, 0) else {
        return;
    };
    if read == 0 {
        return;
    }

    *bytes_seen += read as u64;
    let Ok(chunk) = core::str::from_utf8(&rx_buf[..read]) else {
        log::info!("GPS: UART received {} non-UTF8 bytes", read);
        return;
    };

    line_buf.push_str(chunk);
    let mut pending_line = String::new();
    while let Some(newline_idx) = line_buf.find('\n') {
        pending_line.clear();
        pending_line.push_str(line_buf[..newline_idx].trim_end_matches('\r').trim());
        line_buf.drain(..=newline_idx);
        let trimmed = pending_line.as_str();

        if !trimmed.starts_with('$') {
            continue;
        }

        if trimmed.starts_with("$GNRMC") || trimmed.starts_with("$GPRMC") {
            if gps::parse_rmc(trimmed, gps).is_some() {
                ui_feed.publish_gps(gps);
                if gps.fix {
                    if let Some(utc_unix_seconds) = gps.utc_unix_seconds {
                        ntp_server.update_gps_utc_seconds(utc_unix_seconds);
                    }
                    if let Some(worker) = tz_worker.as_mut() {
                        maybe_schedule_timezone_lookup(
                            gps,
                            worker,
                            tz_initialized,
                            last_tz_lookup_us,
                        );
                    }
                }
            }
        } else if trimmed.starts_with("$GNGGA") || trimmed.starts_with("$GPGGA") {
            if gps::parse_gga(trimmed, gps).is_some() {
                ui_feed.publish_gps(gps);
            }
        }
    }
}

/// Queue a timezone lookup when the refresh interval has elapsed.
///
/// # Parameters
/// - `gps`: GPS snapshot supplying latitude and longitude for lookup.
/// - `worker`: Background timezone worker receiving coordinate requests.
/// - `tz_initialized`: Whether a valid timezone is already active.
/// - `last_tz_lookup_us`: Updated when a new lookup request is queued.
///
/// # Returns
/// - No return value.
fn maybe_schedule_timezone_lookup(
    gps: &GpsSnapshot,
    worker: &mut TimezoneWorker,
    tz_initialized: &bool,
    last_tz_lookup_us: &mut i64,
) {
    let now_us = monotonic_us();
    let lookup_interval_us = timezone::lookup_interval_us(*tz_initialized);
    let should_lookup =
        *last_tz_lookup_us == 0 || (now_us - *last_tz_lookup_us) >= lookup_interval_us;
    if should_lookup && !worker.is_pending() && worker.try_request(gps.lat, gps.lon) {
        *last_tz_lookup_us = now_us;
    }
}

/// Apply a completed timezone lookup result to runtime and NVS state.
///
/// # Parameters
/// - `result`: Worker result containing an IANA timezone name, empty, or error.
/// - `lat`: Latitude logged when lookup returns no timezone.
/// - `lon`: Longitude logged when lookup returns no timezone.
/// - `tz_store`: Optional NVS store for persisting resolved timezone names.
/// - `tz_initialized`: Set to `true` when a valid timezone is applied.
/// - `current_tz_name`: Updated to the active IANA timezone name.
///
/// # Returns
/// - No return value.
fn apply_timezone_lookup_result(
    result: anyhow::Result<Option<String>>,
    lat: f32,
    lon: f32,
    tz_store: Option<&mut TimezoneStore>,
    tz_initialized: &mut bool,
    current_tz_name: &mut Option<String>,
) {
    match result {
        Ok(Some(tz_name)) => {
            if gps::set_runtime_timezone(&tz_name) {
                let changed = current_tz_name.as_deref() != Some(tz_name.as_str());
                *tz_initialized = true;
                if changed {
                    if let Some(old_tz) = current_tz_name.as_ref() {
                        log::info!("GPS: timezone updated from {} to {}", old_tz, tz_name);
                    } else {
                        log::info!("GPS: timezone resolved from coordinates: {}", tz_name);
                    }
                    timezone::persist_cached(&tz_name, tz_store);
                }
                *current_tz_name = Some(tz_name);
            } else {
                log::warn!("GPS: timezone lookup returned invalid value '{}'", tz_name);
            }
        }
        Ok(None) => {
            log::warn!(
                "GPS: timezone lookup returned no timezone for coords ({:.6}, {:.6})",
                lat,
                lon
            );
        }
        Err(err) => {
            log::warn!("GPS: timezone lookup failed: {}", err);
        }
    }
}
