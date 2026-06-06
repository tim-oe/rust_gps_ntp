//! Firmware orchestrator: peripheral init, UI task spawn, and the main service loop.
//!
//! The main loop handles GPS ingest, PPS discipline, NTP polling, and timezone
//! coordination. Display, button, and battery sampling run on [`crate::ui_task`].

use anyhow::Context;
use display_interface_spi::SPIInterfaceNoCS;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{self, PinDriver};
use esp_idf_svc::hal::i2c;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::spi;
use esp_idf_svc::hal::uart::{self, UartDriver};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use st7789::ST7789;
use std::sync::Arc;

use crate::battery::{self, BatteryMonitor};
use crate::display;
use crate::gps::{self, GpsSnapshot};
use crate::logging;
use crate::ntp;
use crate::pps::{PpsEvent, PpsMonitor, PpsPollState};
use crate::timezone::{TimezoneStore, TimezoneWorker};
use crate::ui_task::{UiFeed, UiTaskHandle};
use crate::wifi;

/// GPS module UART TX pin used for NMEA output from the FeatherWing.
pub const GPS_UART_TX_PIN: i32 = 1;
/// GPS module UART RX pin used for NMEA input to the ESP32.
pub const GPS_UART_RX_PIN: i32 = 2;
/// PPS input pin monitored with a rising-edge GPIO interrupt.
pub const PPS_GPIO_PIN: i32 = 12;
/// Board I2C SDA pin for the battery fuel gauge.
pub const BOARD_I2C_SDA_PIN: i32 = 42;
/// Board I2C SCL pin for the battery fuel gauge.
pub const BOARD_I2C_SCL_PIN: i32 = 41;

const TZ_LOOKUP_RETRY_US: i64 = 300_000_000;
const TZ_LOOKUP_REFRESH_US: i64 = 21_600_000_000;

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

    let peripherals = Peripherals::take().context("failed to take ESP32 peripherals")?;
    let modem = peripherals.modem;
    let uart1 = peripherals.uart1;
    let i2c0 = peripherals.i2c0;
    let spi2 = peripherals.spi2;
    let pins = peripherals.pins;

    let default_nvs =
        EspDefaultNvsPartition::take().context("failed to take default NVS partition for Wi-Fi")?;
    let default_nvs_tz = default_nvs.clone();
    let sys_loop = esp_idf_svc::eventloop::EspSystemEventLoop::take()
        .context("failed to take system event loop")?;
    let _wifi = wifi::connect_wifi_sta(modem, sys_loop, default_nvs, &wifi_creds)?;

    let uart_cfg = uart::config::Config::default().baudrate(Hertz(9_600));
    let gps_uart = UartDriver::new(
        uart1,
        pins.gpio1,
        pins.gpio2,
        Option::<gpio::Gpio0>::None,
        Option::<gpio::Gpio1>::None,
        &uart_cfg,
    )
    .context("failed to initialize GPS UART diagnostics")?;

    let mut tft_power =
        PinDriver::output(pins.gpio21).context("failed to init TFT power enable")?;
    tft_power
        .set_high()
        .context("failed to enable TFT power rail")?;
    FreeRtos::delay_ms(10);

    let i2c_cfg = i2c::config::Config::new().baudrate(100.kHz().into());
    let mut i2c_drv = i2c::I2cDriver::new(i2c0, pins.gpio42, pins.gpio41, &i2c_cfg)
        .context("failed to initialize I2C for battery monitor")?;
    log::info!(
        "Battery I2C bus initialized on SDA=GPIO{}, SCL=GPIO{}",
        BOARD_I2C_SDA_PIN,
        BOARD_I2C_SCL_PIN
    );
    let battery_monitor = battery::detect_monitor(&mut i2c_drv);
    match battery_monitor {
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

    let spi_drv = spi::SpiDeviceDriver::new_single(
        spi2,
        pins.gpio36,
        pins.gpio35,
        None::<gpio::Gpio37>,
        Some(pins.gpio7),
        &spi::config::DriverConfig::new(),
        &spi::config::Config::new().baudrate(40.MHz().into()),
    )
    .context("failed to initialize SPI for TFT")?;
    let dc = PinDriver::output(pins.gpio39).context("failed to init TFT DC")?;
    let rst = PinDriver::output(pins.gpio40).context("failed to init TFT RST")?;
    let backlight = PinDriver::output(pins.gpio45).context("failed to init TFT backlight")?;
    let button = PinDriver::input(pins.gpio0).context("failed to init page button")?;

    let di = SPIInterfaceNoCS::new(spi_drv, dc);
    let mut display = ST7789::new(di, Some(rst), Some(backlight), 240, 135);
    let mut ets = Ets;
    let backlight_on_state = display::init_display(&mut display, &mut ets)?;

    let ui_feed = UiFeed::new();
    let _ui_task = UiTaskHandle::spawn(
        Arc::clone(&ui_feed),
        display,
        button,
        i2c_drv,
        battery_monitor,
        backlight_on_state,
    )?;

    let mut rx_buf = [0_u8; 256];
    let mut line_buf = String::new();
    let mut bytes_seen: u64 = 0;
    let mut gps = GpsSnapshot::default();

    let pps = PpsMonitor::new();
    let pps_edge_us_isr = pps.edge_us();
    let pps_count_isr = pps.count();
    let mut pps_poll = PpsPollState::default();
    let mut pps_pin =
        PinDriver::input(pins.gpio12).context("failed to initialize PPS input pin")?;
    unsafe {
        pps_pin
            .subscribe_nonstatic(move || {
                let now_us = esp_idf_svc::sys::esp_timer_get_time() as u64;
                PpsMonitor::record_edge(&pps_edge_us_isr, &pps_count_isr, now_us);
            })
            .context("failed to subscribe PPS ISR callback")?;
    }
    pps_pin
        .set_interrupt_type(gpio::InterruptType::PosEdge)
        .context("failed to set PPS interrupt type")?;
    pps_pin
        .enable_interrupt()
        .context("failed to enable PPS interrupt")?;
    log::info!(
        "PPS: monitoring GPIO{} (rising-edge interrupt)",
        PPS_GPIO_PIN
    );

    let mut ntp_server = ntp::NtpServer::bind()?;
    let mut tz_store = TimezoneStore::new(default_nvs_tz).ok();
    let mut tz_worker = TimezoneWorker::spawn().ok();
    let mut tz_initialized = false;
    let mut current_tz_name: Option<String> = None;
    let mut last_tz_lookup_us = 0_i64;
    let mut last_ntp_publish_us = 0_i64;

    if let Some(store) = tz_store.as_ref() {
        match store.load_cached() {
            Ok(Some(tz_name)) if gps::set_runtime_timezone(&tz_name) => {
                tz_initialized = true;
                current_tz_name = Some(tz_name.clone());
                log::info!("GPS: loaded cached timezone {}", tz_name);
            }
            Ok(Some(tz_name)) => {
                log::warn!(
                    "GPS: cached timezone '{}' is invalid; will refresh",
                    tz_name
                );
            }
            Ok(None) => {}
            Err(err) => {
                log::warn!("GPS: failed to read timezone cache: {}", err);
            }
        }
    }

    log::info!("NTP: listening on UDP/123");
    log::info!("System: booted; Wi-Fi + GPS UART diagnostics mode");
    log::info!(
        "Listening for raw NMEA on UART1 (9600 baud), TX=GPIO{}, RX=GPIO{}",
        GPS_UART_TX_PIN,
        GPS_UART_RX_PIN
    );

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
            }
        }

        if let Err(err) = ntp_server.poll(gps.fix) {
            log::warn!("NTP: poll failed: {}", err);
        }

        if bytes_seen > 0 && bytes_seen % 512 == 0 {
            log::debug!("GPS: diagnostics bytes received={}", bytes_seen);
        }

        if let Some(event) = pps.poll(&mut pps_poll) {
            match event {
                PpsEvent::First => {
                    log::debug!("PPS: pulse #{} detected", pps_poll.pulse_count());
                    ntp_server.observe_pps_pulse(None);
                }
                PpsEvent::Delta(delta) => {
                    ui_feed.publish_pps_delta(delta);
                    log::debug!("PPS: pulse #{} delta={}us", pps_poll.pulse_count(), delta);
                    ntp_server.observe_pps_pulse(Some(delta));
                }
            }
            // Publish fresh discipline metrics whenever PPS fires (every ~1 s when locked).
            ui_feed.publish_ntp(ntp_server.ntp_snapshot(gps.fix));
            last_ntp_publish_us = monotonic_us();
            if let Err(err) = pps_pin.enable_interrupt() {
                log::warn!("PPS: failed to re-enable interrupt: {}", err);
            }
        }

        // During holdover the dispersion grows with time; refresh the display
        // snapshot every second so the UI reflects current uncertainty.
        let now_us = monotonic_us();
        if (now_us - last_ntp_publish_us) >= 1_000_000 {
            ui_feed.publish_ntp(ntp_server.ntp_snapshot(gps.fix));
            last_ntp_publish_us = now_us;
        }

        FreeRtos::delay_ms(10);
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
    gps_uart: &UartDriver<'_>,
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
    let Ok(read) = gps_uart.read(rx_buf, 25) else {
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
    let lookup_interval_us = if *tz_initialized {
        TZ_LOOKUP_REFRESH_US
    } else {
        TZ_LOOKUP_RETRY_US
    };
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
                    if let Some(store) = tz_store {
                        if let Err(err) = store.save(&tz_name) {
                            log::warn!("GPS: failed to persist timezone '{}': {}", tz_name, err);
                        }
                    }
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
