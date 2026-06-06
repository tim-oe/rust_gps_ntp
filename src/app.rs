//! Firmware orchestrator: peripheral init and the main service loop.

use anyhow::Context;
use display_interface_spi::SPIInterfaceNoCS;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{self, PinDriver, Pull};
use esp_idf_svc::hal::i2c;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::spi;
use esp_idf_svc::hal::uart::{self, UartDriver};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use st7789::ST7789;

use crate::battery::{self, BatteryMonitor, BatterySnapshot};
use crate::display::{self, Page};
use crate::gps::{self, GpsSnapshot};
use crate::logging;
use crate::ntp;
use crate::pps::{PpsEvent, PpsMonitor, PpsPollState};
use crate::timezone::{TimezoneStore, TimezoneWorker};
use crate::wifi;

pub const GPS_UART_TX_PIN: i32 = 1;
pub const GPS_UART_RX_PIN: i32 = 2;
pub const PPS_GPIO_PIN: i32 = 12;
pub const BOARD_I2C_SDA_PIN: i32 = 42;
pub const BOARD_I2C_SCL_PIN: i32 = 41;

const TZ_LOOKUP_RETRY_US: i64 = 300_000_000;
const TZ_LOOKUP_REFRESH_US: i64 = 21_600_000_000;

/// Initialize peripherals and run the firmware service loop.
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
    let mut button = PinDriver::input(pins.gpio0).context("failed to init page button")?;
    if let Err(err) = button.set_pull(Pull::Up) {
        log::warn!("Display: failed to enable button pull-up: {}", err);
    }

    let di = SPIInterfaceNoCS::new(spi_drv, dc);
    let mut display = ST7789::new(di, Some(rst), Some(backlight), 240, 135);
    let mut ets = Ets;
    let backlight_on_state = display::init_display(&mut display, &mut ets)?;

    let mut rx_buf = [0_u8; 256];
    let mut line_buf = String::new();
    let mut bytes_seen: u64 = 0;
    let mut gps = GpsSnapshot::default();
    let mut battery = BatterySnapshot::default();

    let pps = PpsMonitor::new();
    let pps_edge_us_isr = pps.edge_us();
    let pps_count_isr = pps.count();
    let mut pps_poll = PpsPollState::default();
    let mut pps_delta_us = 0_u32;
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

    let mut last_battery_us = 0_i64;
    let mut last_draw_us = 0_i64;
    let mut last_button_pressed = false;
    let mut screen_on = true;
    let mut current_page = Page::Time;
    let mut last_interaction_us = monotonic_us();
    let mut force_redraw = true;
    let mut rendered_once = false;

    let mut ntp_server = ntp::NtpServer::bind()?;
    let mut tz_store = TimezoneStore::new(default_nvs_tz).ok();
    let mut tz_worker = TimezoneWorker::spawn().ok();
    let mut tz_initialized = false;
    let mut current_tz_name: Option<String> = None;
    let mut last_tz_lookup_us = 0_i64;

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
                    pps_delta_us = delta;
                    log::debug!(
                        "PPS: pulse #{} delta={}us",
                        pps_poll.pulse_count(),
                        pps_delta_us
                    );
                    ntp_server.observe_pps_pulse(Some(pps_delta_us));
                }
            }
            if let Err(err) = pps_pin.enable_interrupt() {
                log::warn!("PPS: failed to re-enable interrupt: {}", err);
            }
        }

        let now_us = monotonic_us();
        if last_battery_us == 0 || (now_us - last_battery_us) >= 5_000_000 {
            if let Some(kind) = battery_monitor {
                match battery::read_battery(&mut i2c_drv, kind) {
                    Ok(reading) => battery = reading,
                    Err(err) => log::debug!("Battery: read failed: {}", err),
                }
            }
            last_battery_us = now_us;
        }

        let button_pressed = !button.is_high();
        if button_pressed && !last_button_pressed {
            if !screen_on {
                screen_on = true;
                if let Err(err) = display.set_backlight(backlight_on_state, &mut ets) {
                    log::warn!("Display: failed to turn backlight on: {:?}", err);
                }
            } else {
                current_page = current_page.next();
            }
            last_interaction_us = now_us;
            force_redraw = true;
        }
        last_button_pressed = button_pressed;

        if !display::DISPLAY_DEBUG_ALWAYS_ON
            && screen_on
            && (now_us - last_interaction_us) >= 15_000_000
        {
            screen_on = false;
            if let Err(err) = display.set_backlight(display::backlight_off_state(), &mut ets) {
                log::warn!("Display: failed to turn backlight off: {:?}", err);
            }
        }

        if screen_on && (force_redraw || (now_us - last_draw_us) >= 5_000_000) {
            let mut panel = display::make_panel(&mut display);
            display::draw_page(&mut panel, current_page, &gps, &battery, pps_delta_us);
            if !rendered_once {
                log::trace!("Display diag: first frame rendered");
                rendered_once = true;
            }
            last_draw_us = now_us;
            force_redraw = false;
        }

        FreeRtos::delay_ms(10);
    }
}

fn monotonic_us() -> i64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}

fn poll_gps_uart(
    gps_uart: &UartDriver<'_>,
    rx_buf: &mut [u8; 256],
    line_buf: &mut String,
    bytes_seen: &mut u64,
    gps: &mut GpsSnapshot,
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
            if gps::parse_rmc(trimmed, gps).is_some() && gps.fix {
                if let Some(utc_unix_seconds) = gps.utc_unix_seconds {
                    ntp_server.update_gps_utc_seconds(utc_unix_seconds);
                }
                if let Some(worker) = tz_worker.as_mut() {
                    maybe_schedule_timezone_lookup(gps, worker, tz_initialized, last_tz_lookup_us);
                }
            }
        } else if trimmed.starts_with("$GNGGA") || trimmed.starts_with("$GPGGA") {
            let _ = gps::parse_gga(trimmed, gps);
        }
    }
}

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
