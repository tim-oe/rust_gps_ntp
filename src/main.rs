#[cfg(target_os = "espidf")]
mod battery;
#[cfg(target_os = "espidf")]
mod display;
#[cfg(target_os = "espidf")]
mod gps;
#[cfg(target_os = "espidf")]
mod wifi;

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use core::sync::atomic::{AtomicU32, Ordering};
    use display::Page;
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
    use std::sync::Arc;

    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let wifi_creds = wifi::load_wifi_credentials_from_env()?;

    let peripherals = Peripherals::take().context("failed to take ESP32 peripherals")?;
    let modem = peripherals.modem;
    let uart1 = peripherals.uart1;
    let i2c0 = peripherals.i2c0;
    let spi2 = peripherals.spi2;
    let pins = peripherals.pins;

    let default_nvs = EspDefaultNvsPartition::take()
        .context("failed to take default NVS partition for Wi-Fi")?;
    let sys_loop = esp_idf_svc::eventloop::EspSystemEventLoop::take()
        .context("failed to take system event loop")?;
    let _wifi = wifi::connect_wifi_sta(modem, sys_loop, default_nvs, &wifi_creds)?;

    const GPS_UART_TX_PIN: i32 = 1;
    const GPS_UART_RX_PIN: i32 = 2;
    const PPS_GPIO_PIN: i32 = 12;
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

    let i2c_cfg = i2c::config::Config::new().baudrate(100.kHz().into());
    let mut i2c_drv = i2c::I2cDriver::new(i2c0, pins.gpio3, pins.gpio4, &i2c_cfg)
        .context("failed to initialize I2C for battery monitor")?;

    let mut tft_power =
        PinDriver::output(pins.gpio21).context("failed to init TFT power enable")?;
    tft_power
        .set_high()
        .context("failed to enable TFT power rail")?;

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
    button.set_pull(Pull::Up).ok();

    let di = SPIInterfaceNoCS::new(spi_drv, dc);
    let mut display = ST7789::new(di, Some(rst), Some(backlight), 240, 135);
    let mut ets = Ets;
    let backlight_on_state = display::init_display(&mut display, &mut ets)?;
    {
        let mut panel = display::make_panel(&mut display);
        display::draw_boot_test(&mut panel);
    }

    let mut rx_buf = [0_u8; 256];
    let mut line_buf = String::new();
    let mut bytes_seen: u64 = 0;
    let mut gps = gps::GpsSnapshot::default();
    let mut battery = battery::BatterySnapshot::default();
    let mut pps_pin =
        PinDriver::input(pins.gpio12).context("failed to initialize PPS input pin")?;
    let pps_edge_us = Arc::new(AtomicU32::new(0));
    let pps_count = Arc::new(AtomicU32::new(0));
    let pps_edge_us_isr = Arc::clone(&pps_edge_us);
    let pps_count_isr = Arc::clone(&pps_count);
    let mut last_logged_pps_count = 0_u32;
    let mut last_logged_pps_us = 0_u32;
    let mut pps_delta_us = 0_u32;
    let mut last_diag_us = 0_i64;
    let mut last_battery_us = 0_i64;
    let mut last_draw_us = 0_i64;
    let mut last_button_pressed = false;
    let mut screen_on = true;
    let mut current_page = Page::Time;
    let mut last_interaction_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
    let mut force_redraw = true;

    log::info!("ESP32 booted; Wi-Fi + GPS UART diagnostics mode");
    log::info!(
        "Listening for raw NMEA on UART1 (9600 baud), TX=GPIO{}, RX=GPIO{}",
        GPS_UART_TX_PIN,
        GPS_UART_RX_PIN
    );
    unsafe {
        pps_pin
            .subscribe_nonstatic(move || {
                let now_us = esp_idf_svc::sys::esp_timer_get_time();
                pps_edge_us_isr.store(now_us as u32, Ordering::Relaxed);
                pps_count_isr.fetch_add(1, Ordering::Relaxed);
            })
            .context("failed to subscribe PPS ISR callback")?;
    }
    pps_pin
        .set_interrupt_type(gpio::InterruptType::PosEdge)
        .context("failed to set PPS interrupt type")?;
    pps_pin
        .enable_interrupt()
        .context("failed to enable PPS interrupt")?;
    log::info!("Monitoring PPS on GPIO{} (rising-edge interrupt)", PPS_GPIO_PIN);

    loop {
        if let Ok(read) = gps_uart.read(&mut rx_buf, 25) {
            if read > 0 {
                bytes_seen += read as u64;

                match core::str::from_utf8(&rx_buf[..read]) {
                    Ok(chunk) => {
                        line_buf.push_str(chunk);

                        while let Some(newline_idx) = line_buf.find('\n') {
                            let line: String = line_buf.drain(..=newline_idx).collect();
                            let trimmed = line.trim();
                            if trimmed.starts_with('$') {
                                if trimmed.starts_with("$GNRMC") || trimmed.starts_with("$GPRMC") {
                                    let _ = gps::parse_rmc(trimmed, &mut gps);
                                } else if trimmed.starts_with("$GNGGA")
                                    || trimmed.starts_with("$GPGGA")
                                {
                                    let _ = gps::parse_gga(trimmed, &mut gps);
                                }
                            }
                        }
                    }
                    Err(_) => {
                        log::warn!("GPS UART received {} non-UTF8 bytes", read);
                    }
                }
            }
        }

        if bytes_seen > 0 && bytes_seen % 512 == 0 {
            log::info!("GPS diagnostics bytes received: {}", bytes_seen);
        }

        let current_pps_count = pps_count.load(Ordering::Relaxed);
        if current_pps_count > last_logged_pps_count {
            let now_us = pps_edge_us.load(Ordering::Relaxed);
            if last_logged_pps_us > 0 {
                pps_delta_us = now_us.wrapping_sub(last_logged_pps_us);
                log::info!("PPS pulse #{} delta={}us", current_pps_count, pps_delta_us);
            } else {
                log::info!("PPS pulse #{} detected", current_pps_count);
            }

            last_logged_pps_count = current_pps_count;
            last_logged_pps_us = now_us;

            if let Err(err) = pps_pin.enable_interrupt() {
                log::warn!("Failed to re-enable PPS interrupt: {}", err);
            }
        }

        let now_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        if last_battery_us == 0 || (now_us - last_battery_us) >= 5_000_000 {
            if let Some(reading) = battery::read_battery(&mut i2c_drv) {
                battery = reading;
            }
            last_battery_us = now_us;
        }

        let button_pressed = !button.is_high();
        if button_pressed && !last_button_pressed {
            if !screen_on {
                screen_on = true;
                let _ = display.set_backlight(backlight_on_state, &mut ets);
            } else {
                current_page = current_page.next();
            }
            last_interaction_us = now_us;
            force_redraw = true;
        }
        last_button_pressed = button_pressed;

        if !display::DISPLAY_DEBUG_ALWAYS_ON && screen_on && (now_us - last_interaction_us) >= 15_000_000
        {
            screen_on = false;
            let _ = display.set_backlight(display::backlight_off_state(), &mut ets);
        }

        if screen_on && (force_redraw || (now_us - last_draw_us) >= 5_000_000) {
            let mut panel = display::make_panel(&mut display);
            display::draw_page(
                &mut panel,
                current_page,
                &gps,
                &battery,
                pps_delta_us,
                current_pps_count,
                bytes_seen,
            );
            last_draw_us = now_us;
            force_redraw = false;
        }

        if last_diag_us == 0 || (now_us - last_diag_us) >= 10_000_000 {
            let free_heap = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
            let min_free_heap = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
            let largest_block = unsafe {
                esp_idf_svc::sys::heap_caps_get_largest_free_block(
                    esp_idf_svc::sys::MALLOC_CAP_8BIT as u32,
                )
            };
            let _ = (free_heap, min_free_heap, largest_block, current_pps_count, bytes_seen);
            last_diag_us = now_us;
        }

        FreeRtos::delay_ms(10);
    }
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("This project targets ESP32 with ESP-IDF (run through cargo espflash).");
}
