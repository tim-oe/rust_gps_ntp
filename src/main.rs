#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    use chrono::{Duration as ChronoDuration, NaiveDate, NaiveDateTime, NaiveTime};
    use core::convert::TryInto;
    use core::sync::atomic::{AtomicU32, Ordering};
    use anyhow::{anyhow, bail, Context};
    use display_interface_spi::SPIInterfaceNoCS;
    use embedded_graphics::mono_font::ascii::FONT_6X10;
    use embedded_graphics::mono_font::MonoTextStyleBuilder;
    use embedded_graphics::pixelcolor::Rgb565;
    use embedded_graphics::prelude::*;
    use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
    use embedded_graphics::text::Text;
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::delay::{Ets, FreeRtos};
    use esp_idf_svc::hal::gpio::{self, PinDriver, Pull};
    use esp_idf_svc::hal::i2c;
    use esp_idf_svc::hal::peripherals::Peripherals;
    use esp_idf_svc::hal::prelude::*;
    use esp_idf_svc::hal::spi;
    use esp_idf_svc::hal::uart::{self, UartDriver};
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_svc::wifi::{
        AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
    };
    use st7789::{Orientation, ST7789};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct WifiCredentials {
        ssid: String,
        pass: String,
    }

    #[derive(Debug, Clone, Default)]
    struct GpsSnapshot {
        utc_date: String,
        utc_time: String,
        local_time: String,
        lat: f32,
        lon: f32,
        fix: bool,
        sats: u8,
    }

    #[derive(Debug, Clone, Default)]
    struct BatterySnapshot {
        voltage_v: f32,
        percent: f32,
    }

    #[derive(Debug, Copy, Clone)]
    enum Page {
        Time,
        Location,
        Resources,
        Battery,
    }

    impl Page {
        fn next(self) -> Self {
            match self {
                Self::Time => Self::Location,
                Self::Location => Self::Resources,
                Self::Resources => Self::Battery,
                Self::Battery => Self::Time,
            }
        }
    }

    fn parse_hhmmss(raw: &str) -> Option<&str> {
        if raw.len() < 6 || !raw.is_ascii() {
            return None;
        }
        Some(&raw[..6])
    }

    fn format_hhmmss(raw6: &str) -> String {
        format!("{}:{}:{}", &raw6[0..2], &raw6[2..4], &raw6[4..6])
    }

    fn parse_ddmmyy(raw: &str) -> Option<&str> {
        if raw.len() < 6 || !raw.is_ascii() {
            return None;
        }
        Some(&raw[..6])
    }

    fn format_ddmmyy(raw6: &str) -> String {
        format!("20{}-{}-{}", &raw6[4..6], &raw6[2..4], &raw6[0..2])
    }

    fn nmea_to_decimal(value: &str, dir: &str) -> Option<f32> {
        let raw: f32 = value.parse().ok()?;
        let degrees = (raw / 100.0).floor();
        let minutes = raw - (degrees * 100.0);
        let mut decimal = degrees + (minutes / 60.0);
        if dir == "S" || dir == "W" {
            decimal = -decimal;
        }
        Some(decimal)
    }

    fn local_time_from_utc(utc_date: &str, utc_time: &str, lon: f32) -> Option<String> {
        let tz_offset_h = (lon / 15.0).round() as i64;
        let ddmmyy = parse_ddmmyy(utc_date)?;
        let hhmmss = parse_hhmmss(utc_time)?;

        let day: u32 = ddmmyy[0..2].parse().ok()?;
        let month: u32 = ddmmyy[2..4].parse().ok()?;
        let year: i32 = 2000 + ddmmyy[4..6].parse::<i32>().ok()?;
        let hour: u32 = hhmmss[0..2].parse().ok()?;
        let minute: u32 = hhmmss[2..4].parse().ok()?;
        let second: u32 = hhmmss[4..6].parse().ok()?;

        let date = NaiveDate::from_ymd_opt(year, month, day)?;
        let time = NaiveTime::from_hms_opt(hour, minute, second)?;
        let dt = NaiveDateTime::new(date, time) + ChronoDuration::hours(tz_offset_h);

        Some(format!(
            "{} ({:+}h)",
            dt.time().format("%H:%M:%S"),
            tz_offset_h
        ))
    }

    fn parse_rmc(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
        let fields: Vec<&str> = sentence.split(',').collect();
        if fields.len() < 10 {
            return None;
        }

        let time = parse_hhmmss(fields[1])?;
        let status = fields[2];
        let date = parse_ddmmyy(fields[9])?;
        let lat = nmea_to_decimal(fields[3], fields[4])?;
        let lon = nmea_to_decimal(fields[5], fields[6])?;

        gps.utc_date = format_ddmmyy(date);
        gps.utc_time = format_hhmmss(time);
        gps.local_time = local_time_from_utc(date, time, lon).unwrap_or_else(|| "n/a".to_owned());
        gps.lat = lat;
        gps.lon = lon;
        gps.fix = status == "A";

        Some(())
    }

    fn parse_gga(sentence: &str, gps: &mut GpsSnapshot) -> Option<()> {
        let fields: Vec<&str> = sentence.split(',').collect();
        if fields.len() < 8 {
            return None;
        }
        gps.sats = fields[7].parse::<u8>().ok()?;
        Some(())
    }

    fn read_battery(i2c: &mut i2c::I2cDriver<'_>) -> Option<BatterySnapshot> {
        const MAX17048_ADDR: u8 = 0x36;
        const REG_VCELL: u8 = 0x02;
        const REG_SOC: u8 = 0x04;

        let mut vcell = [0_u8; 2];
        let mut soc = [0_u8; 2];
        i2c.write_read(MAX17048_ADDR, &[REG_VCELL], &mut vcell, 50).ok()?;
        i2c.write_read(MAX17048_ADDR, &[REG_SOC], &mut soc, 50).ok()?;

        let vraw = u16::from_be_bytes(vcell);
        let voltage_v = (vraw as f32) * 78.125e-6;
        let percent = (soc[0] as f32) + ((soc[1] as f32) / 256.0);

        Some(BatterySnapshot { voltage_v, percent })
    }

    fn draw_page<D>(display: &mut D, page: Page, gps: &GpsSnapshot, battery: &BatterySnapshot, pps_delta_us: u32, pps_count: u32, bytes_seen: u64)
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let style = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(Rgb565::WHITE)
            .build();

        let _ = Rectangle::new(Point::new(0, 0), Size::new(240, 135))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
            .draw(display);

        let mut y = 14_i32;
        let mut line = |text: String| {
            let _ = Text::new(&text, Point::new(4, y), style).draw(display);
            y += 12;
        };

        match page {
            Page::Time => {
                line("Page 1/4  TIME".to_owned());
                line(format!("UTC:   {} {}", gps.utc_date, gps.utc_time));
                line(format!("Local: {}", gps.local_time));
                line(format!("Fix:   {}", if gps.fix { "yes" } else { "no" }));
                line(format!("Lat:   {:.5}", gps.lat));
                line(format!("Lon:   {:.5}", gps.lon));
            }
            Page::Location => {
                line("Page 2/4  LOCATION".to_owned());
                line(format!("Lat: {:.6}", gps.lat));
                line(format!("Lon: {:.6}", gps.lon));
                line(format!("Sats: {}", gps.sats));
                line(format!("PPS count: {}", pps_count));
                line(format!("PPS offset: {}us", pps_delta_us));
            }
            Page::Resources => {
                let free_heap = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
                let min_heap = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
                let largest = unsafe {
                    esp_idf_svc::sys::heap_caps_get_largest_free_block(
                        esp_idf_svc::sys::MALLOC_CAP_8BIT as u32,
                    )
                };
                let cpu_mhz = unsafe { esp_idf_svc::sys::esp_clk_cpu_freq() / 1_000_000 };
                let part_size_kb = unsafe {
                    let p = esp_idf_svc::sys::esp_ota_get_running_partition();
                    if p.is_null() {
                        0
                    } else {
                        ((*p).size / 1024) as u32
                    }
                };

                line("Page 3/4  RESOURCES".to_owned());
                line(format!("Storage(part): {} KB", part_size_kb));
                line(format!("Heap free: {} B", free_heap));
                line(format!("Heap min:  {} B", min_heap));
                line(format!("Heap block: {} B", largest));
                line(format!("CPU freq: {} MHz", cpu_mhz));
                line(format!("GPS bytes: {}", bytes_seen));
            }
            Page::Battery => {
                line("Page 4/4  BATTERY".to_owned());
                line("MAX17048 over I2C".to_owned());
                line(format!("Voltage: {:.3} V", battery.voltage_v));
                line(format!("Charge:  {:.1} %", battery.percent));
                line(format!("PPS last: {} us", pps_delta_us));
                line(format!("UTC: {}", gps.utc_time));
            }
        }
    }

    fn load_wifi_credentials_from_env() -> anyhow::Result<WifiCredentials> {
        let env_ssid = option_env!("WIFI_SSID")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        let env_pass = option_env!("WIFI_PASS")
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);

        if let (Some(ssid), Some(pass)) = (env_ssid, env_pass) {
            log::info!("Wi-Fi STA SSID loaded: {}", ssid);
            return Ok(WifiCredentials { ssid, pass });
        }

        bail!(
            "No Wi-Fi credentials found. Set WIFI_SSID and WIFI_PASS in your shell before flashing."
        );
    }

    fn connect_wifi_sta(
        modem: esp_idf_svc::hal::modem::Modem,
        sys_loop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        creds: &WifiCredentials,
    ) -> anyhow::Result<BlockingWifi<EspWifi<'static>>> {
        let mut wifi = BlockingWifi::wrap(
            EspWifi::new(modem, sys_loop.clone(), Some(nvs)).context("failed to create EspWifi")?,
            sys_loop,
        )
        .context("failed to wrap BlockingWifi")?;

        let auth_method = if creds.pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        };

        let cfg = Configuration::Client(ClientConfiguration {
            ssid: creds
                .ssid
                .as_str()
                .try_into()
                .map_err(|_| anyhow!("SSID too long for ESP-IDF client config"))?,
            password: creds
                .pass
                .as_str()
                .try_into()
                .map_err(|_| anyhow!("password too long for ESP-IDF client config"))?,
            auth_method,
            ..Default::default()
        });

        wifi.set_configuration(&cfg)
            .context("failed to set Wi-Fi STA configuration")?;

        wifi.start().context("failed to start Wi-Fi driver")?;
        wifi.connect().context("failed to connect Wi-Fi STA")?;
        wifi.wait_netif_up()
            .context("Wi-Fi netif did not come up")?;

        let ip_info = wifi
            .wifi()
            .sta_netif()
            .get_ip_info()
            .context("failed to read DHCP IP info")?;
        log::info!("Wi-Fi connected; STA IP: {}", ip_info.ip);

        Ok(wifi)
    }

    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let wifi_creds = load_wifi_credentials_from_env()?;

    let peripherals = Peripherals::take().context("failed to take ESP32 peripherals")?;
    let modem = peripherals.modem;
    let uart1 = peripherals.uart1;
    let i2c0 = peripherals.i2c0;
    let spi2 = peripherals.spi2;
    let pins = peripherals.pins;

    let default_nvs = EspDefaultNvsPartition::take()
        .context("failed to take default NVS partition for Wi-Fi")?;
    let sys_loop = EspSystemEventLoop::take().context("failed to take system event loop")?;
    let _wifi = connect_wifi_sta(modem, sys_loop, default_nvs, &wifi_creds)?;

    // GPS FeatherWing default UART speed is 9600 baud.
    // Current board pin mapping under test: Feather TX/RX -> GPIO1/GPIO2.
    const GPS_UART_TX_PIN: i32 = 1;
    const GPS_UART_RX_PIN: i32 = 2;
    const PPS_GPIO_PIN: i32 = 13;
    let uart_cfg = uart::config::Config::default().baudrate(Hertz(9_600));
    let gps_uart = UartDriver::new(
        uart1,
        pins.gpio1, // TX -> GPS RX
        pins.gpio2, // RX <- GPS TX
        Option::<gpio::Gpio0>::None,
        Option::<gpio::Gpio1>::None,
        &uart_cfg,
    )
    .context("failed to initialize GPS UART diagnostics")?;
    let i2c_cfg = i2c::config::Config::new().baudrate(100.kHz().into());
    let mut i2c_drv = i2c::I2cDriver::new(i2c0, pins.gpio3, pins.gpio4, &i2c_cfg)
        .context("failed to initialize I2C for battery monitor")?;
    let spi_drv = spi::SpiDeviceDriver::new_single(
        spi2,
        pins.gpio36,            // TFT SCK
        pins.gpio35,            // TFT MOSI
        None::<gpio::Gpio37>,   // TFT MISO unused
        Some(pins.gpio7),       // TFT CS
        &spi::config::DriverConfig::new(),
        &spi::config::Config::new().baudrate(40.MHz().into()),
    )
    .context("failed to initialize SPI for TFT")?;
    let dc = PinDriver::output(pins.gpio39).context("failed to init TFT DC")?;
    let rst = PinDriver::output(pins.gpio40).context("failed to init TFT RST")?;
    let mut backlight = PinDriver::output(pins.gpio45).context("failed to init TFT backlight")?;
    let mut button = PinDriver::input(pins.gpio0).context("failed to init page button")?;
    button.set_pull(Pull::Up).ok();

    let di = SPIInterfaceNoCS::new(spi_drv, dc);
    let mut display = ST7789::new(di, Some(rst), 240, 135);
    let mut ets = Ets;
    display.init(&mut ets).context("failed to initialize ST7789 display")?;
    display
        .set_orientation(Orientation::Landscape)
        .context("failed to set display orientation")?;
    backlight.set_high().ok();

    let mut rx_buf = [0_u8; 256];
    let mut line_buf = String::new();
    let mut bytes_seen: u64 = 0;
    let mut gps = GpsSnapshot::default();
    let mut battery = BatterySnapshot::default();
    let mut pps_pin =
        PinDriver::input(pins.gpio13).context("failed to initialize PPS input pin")?;
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
                                log::info!("GPS NMEA: {}", trimmed);
                                if trimmed.starts_with("$GNRMC") || trimmed.starts_with("$GPRMC") {
                                    let _ = parse_rmc(trimmed, &mut gps);
                                    if !gps.utc_time.is_empty() {
                                        log::info!("GPS UTC: {} {}", gps.utc_date, gps.utc_time);
                                    }
                                } else if trimmed.starts_with("$GNGGA")
                                    || trimmed.starts_with("$GPGGA")
                                {
                                    let _ = parse_gga(trimmed, &mut gps);
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
                let delta = now_us.wrapping_sub(last_logged_pps_us);
                pps_delta_us = delta;
                log::info!("PPS pulse #{} delta={}us", current_pps_count, delta);
            } else {
                log::info!("PPS pulse #{} detected", current_pps_count);
            }

            last_logged_pps_count = current_pps_count;
            last_logged_pps_us = now_us;

            // This HAL disables the GPIO interrupt after each event; re-enable from task context.
            if let Err(err) = pps_pin.enable_interrupt() {
                log::warn!("Failed to re-enable PPS interrupt: {}", err);
            }
        }

        let now_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        if last_battery_us == 0 || (now_us - last_battery_us) >= 5_000_000 {
            if let Some(reading) = read_battery(&mut i2c_drv) {
                battery = reading;
            }
            last_battery_us = now_us;
        }

        let button_pressed = !button.is_high();
        if button_pressed && !last_button_pressed {
            if !screen_on {
                screen_on = true;
                backlight.set_high().ok();
            } else {
                current_page = current_page.next();
            }
            last_interaction_us = now_us;
            force_redraw = true;
        }
        last_button_pressed = button_pressed;

        if screen_on && (now_us - last_interaction_us) >= 15_000_000 {
            screen_on = false;
            backlight.set_low().ok();
        }

        if screen_on && (force_redraw || (now_us - last_draw_us) >= 1_000_000) {
            draw_page(
                &mut display,
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

            log::info!(
                "RES diag: free_heap={}B min_free_heap={}B largest_8bit_block={}B pps_count={} gps_bytes={}",
                free_heap,
                min_free_heap,
                largest_block,
                current_pps_count,
                bytes_seen
            );

            last_diag_us = now_us;
        }

        FreeRtos::delay_ms(10);
    }
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("This project targets ESP32 with ESP-IDF (run through cargo espflash).");
}
