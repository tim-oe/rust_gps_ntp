//! Firmware orchestrator: peripheral init, UI task spawn, and the main service loop.
//!
//! The main loop handles GPS ingest, PPS discipline, NTP polling, and timezone
//! coordination. Display, button, and battery sampling run on [`crate::ui_task`].
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
use crate::ntp::{self, DisciplineState};
use crate::pps::{PpsEvent, PpsMonitor, PpsPollState};
use crate::rtc;
use crate::storage::{self, StorageStatus};
use crate::timezone::{TimezoneStore, TimezoneWorker};
use crate::ui_task::{UiFeed, UiTaskHandle};
use crate::wifi;
#[cfg(esp_idf_comp_mdns_enabled)]
use esp_idf_svc::mdns::EspMdns;
/// GPS module UART TX pin used for NMEA output from the FeatherWing.
pub const GPS_UART_TX_PIN: i32 = 1;
/// GPS module UART RX pin used for NMEA input to the ESP32.
pub const GPS_UART_RX_PIN: i32 = 2;
/// PPS input pin monitored with a rising-edge GPIO interrupt.
pub const PPS_GPIO_PIN: i32 = 12;
/// Board I2C SDA pin for the battery fuel gauge.
pub const BOARD_I2C_SDA_PIN: i32 = 42;
/// Board I2C SCL pin for fuel gauge and Adalogger RTC.
pub const BOARD_I2C_SCL_PIN: i32 = 41;
/// TFT chip-select on the shared Feather SPI bus.
pub const TFT_CS_PIN: i32 = 7;
/// PCF8523 RTC on the Adalogger FeatherWing (shared Feather I2C bus).
pub const ADALOGGER_RTC_I2C_ADDR: u8 = rtc::PCF8523_ADDR;

const TZ_LOOKUP_RETRY_US: i64 = 300_000_000;
const TZ_LOOKUP_REFRESH_US: i64 = 21_600_000_000;
const RTC_WRITEBACK_US: i64 = 60_000_000;
const RTC_FALLBACK_INTERVAL_US: i64 = 1_000_000;
const STORAGE_REFRESH_US: i64 = 60_000_000;

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

    // Register the device on the LAN as <DEVICE_HOSTNAME>.local with an _ntp._udp
    // service record so clients and scripts can find it without reading the serial log.
    // The hostname is read from CONFIG_LWIP_LOCAL_HOSTNAME in sdkconfig.defaults at
    // compile time via build.rs; change it there to rename the device.
    // Requires CONFIG_MDNS_ENABLED=y in sdkconfig.defaults (see sdkconfig.defaults).
    #[cfg(esp_idf_comp_mdns_enabled)]
    let _mdns = match EspMdns::take() {
        Ok(mut mdns) => {
            let hostname_ok = mdns.set_hostname(env!("DEVICE_HOSTNAME")).is_ok();
            let instance_ok = mdns.set_instance_name("GPS+PPS NTP Server").is_ok();
            let service_ok = mdns
                .add_service(None, "_ntp", "_udp", 123, &[("stratum", "1")])
                .is_ok();
            if hostname_ok && instance_ok && service_ok {
                log::info!(
                    "mDNS: registered as {}.local (_ntp._udp port 123)",
                    env!("DEVICE_HOSTNAME")
                );
            } else {
                log::warn!(
                    "mDNS: partial registration failure (hostname={hostname_ok} instance={instance_ok} service={service_ok})"
                );
            }
            Some(mdns)
        }
        Err(err) => {
            log::warn!("mDNS: failed to acquire singleton: {}", err);
            None
        }
    };

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
        .context("failed to initialize I2C for battery monitor and RTC")?;
    log::info!(
        "I2C bus initialized on SDA=GPIO{}, SCL=GPIO{}",
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

    let rtc_present = rtc::detect(&mut i2c_drv);
    let boot_rtc_unix = if rtc_present {
        if let Err(err) = rtc::init(&mut i2c_drv) {
            log::warn!("RTC: PCF8523 init failed: {err}");
        }
        match rtc::read_unix_seconds(&mut i2c_drv) {
            Ok(secs) => {
                log::info!(
                    "RTC: PCF8523 @ 0x{:02X} reads UTC {secs}",
                    ADALOGGER_RTC_I2C_ADDR
                );
                Some(secs)
            }
            Err(err) => {
                log::warn!(
                    "RTC: PCF8523 present @ 0x{:02X} but time unset/invalid ({err}); waiting for GPS",
                    ADALOGGER_RTC_I2C_ADDR
                );
                None
            }
        }
    } else {
        log::warn!(
            "RTC: no PCF8523 response @ 0x{:02X} — check Adalogger stack and CR1220",
            ADALOGGER_RTC_I2C_ADDR
        );
        None
    };

    // Leak the SPI bus driver so TFT and SD borrows satisfy the UI task `'static` bound.
    let spi_driver: &'static spi::SpiDriver<'static> = Box::leak(Box::new(
        spi::SpiDriver::new(
            spi2,
            pins.gpio36,
            pins.gpio35,
            Some(pins.gpio37),
            &storage::shared_spi_bus_config(),
        )
        .context("failed to initialize shared SPI bus for TFT and SD card")?,
    ));

    FreeRtos::delay_ms(50);

    // Hold TFT deselected while the SD card owns the shared Feather SPI bus.
    deselect_spi_cs(TFT_CS_PIN).context("failed to deselect TFT before SD init")?;

    let (mut storage_status, mut storage_mount) =
        storage::try_mount(spi_driver, pins.gpio10, storage::ADALOGGER_SD_CS_PIN);
    if !storage_status.mounted {
        log::info!(
            "Storage: retrying on GPIO{} (alternate Adalogger CS)",
            storage::ADALOGGER_SD_CS_ALT_PIN
        );
        (storage_status, storage_mount) =
            storage::try_mount(spi_driver, pins.gpio33, storage::ADALOGGER_SD_CS_ALT_PIN);
    }
    if !storage_status.mounted {
        log::warn!(
            "Storage: microSD unavailable — insert a FAT32 card; tried CS GPIO{} and GPIO{}",
            storage::ADALOGGER_SD_CS_PIN,
            storage::ADALOGGER_SD_CS_ALT_PIN
        );
    }
    let _storage_mount = storage_mount;

    let tft_spi = spi::SpiDeviceDriver::new(
        spi_driver,
        Some(pins.gpio7),
        &spi::config::Config::new().baudrate(40.MHz().into()),
    )
    .context("failed to initialize SPI device for TFT")?;
    let dc = PinDriver::output(pins.gpio39).context("failed to init TFT DC")?;
    let rst = PinDriver::output(pins.gpio40).context("failed to init TFT RST")?;
    let backlight = PinDriver::output(pins.gpio45).context("failed to init TFT backlight")?;
    let button = PinDriver::input(pins.gpio0).context("failed to init page button")?;

    let di = SPIInterfaceNoCS::new(tft_spi, dc);
    let mut display = ST7789::new(di, Some(rst), Some(backlight), 240, 135);
    let mut ets = Ets;
    let backlight_on_state = display::init_display(&mut display, &mut ets)?;

    let ui_feed = UiFeed::new(storage_status);
    if rtc_present {
        ui_feed.publish_rtc(rtc::RtcSnapshot {
            detected: true,
            utc_unix_seconds: boot_rtc_unix,
        });
    }
    let _ui_task = UiTaskHandle::spawn(
        Arc::clone(&ui_feed),
        display,
        button,
        i2c_drv,
        battery_monitor,
        rtc_present,
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
    let acl_cidr = env!("NTP_ACL_CIDR");
    ntp_server.set_acl(ntp::Acl::from_config(acl_cidr));
    if acl_cidr.is_empty() {
        log::info!("NTP: ACL restricted to RFC 1918 private networks");
    } else {
        log::info!("NTP: ACL restricted to {acl_cidr}");
    }

    if let Some(secs) = boot_rtc_unix {
        ntp_server.update_gps_utc_seconds(secs);
        log::info!("RTC: seeded NTP anchor from cached time (UTC {secs})");
    }

    let mut tz_store = TimezoneStore::new(default_nvs_tz).ok();
    let mut tz_worker = TimezoneWorker::spawn().ok();
    let mut tz_initialized = false;
    let mut current_tz_name: Option<String> = None;
    let mut last_tz_lookup_us = 0_i64;
    let mut last_ntp_publish_us = 0_i64;
    let mut last_ntp_served = 0_u64;
    let mut last_rtc_fallback_us = 0_i64;
    let mut last_rtc_write_us = 0_i64;
    let mut last_rtc_utc = boot_rtc_unix;
    let mut last_storage_refresh_us = 0_i64;

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

        if storage_status.mounted
            && (last_storage_refresh_us == 0
                || (now_us - last_storage_refresh_us) >= STORAGE_REFRESH_US)
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

        if let Some(event) = pps.poll(&mut pps_poll) {
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
                    log::debug!(
                        "PPS: pulse #{} delta={}us",
                        pps_poll.pulse_count(),
                        interval_us
                    );
                    ntp_server.observe_pps_pulse(Some(interval_us), edge_us);
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
                GPS_UART_TX_PIN,
                GPS_UART_RX_PIN
            );
        }

        if !warn_no_pps_done && first_nmea_logged && !first_pps_logged && elapsed_s >= 30 {
            warn_no_pps_done = true;
            log::warn!(
                "PPS: no pulse in {}s since boot — check PPS pin wiring (GPIO{})",
                elapsed_s,
                PPS_GPIO_PIN
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
        *last_rtc_fallback_us == 0 || (now_us - *last_rtc_fallback_us) >= RTC_FALLBACK_INTERVAL_US;
    if !due {
        return;
    }
    if last_rtc_utc != &Some(secs) {
        log::debug!("RTC: feeding cached UTC {secs} (GPS fix lost)");
    }
    ntp_server.update_gps_utc_seconds(secs);
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
    if *last_rtc_write_us != 0 && (now_us - *last_rtc_write_us) < RTC_WRITEBACK_US {
        return;
    }
    if let Some(secs) = ntp_server.current_utc_unix_seconds() {
        ui_feed.request_rtc_write(secs);
        *last_rtc_write_us = now_us;
    }
}

/// Drive a SPI chip-select GPIO high before another device uses the shared bus.
fn deselect_spi_cs(pin: i32) -> anyhow::Result<()> {
    use esp_idf_svc::sys::{
        GPIO_MODE_DEF_OUTPUT, gpio_reset_pin, gpio_set_direction, gpio_set_level,
    };

    esp_idf_svc::sys::esp!(unsafe {
        gpio_reset_pin(pin);
        gpio_set_direction(pin, GPIO_MODE_DEF_OUTPUT);
        gpio_set_level(pin, 1)
    })
    .context("failed to configure SPI CS GPIO")
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
