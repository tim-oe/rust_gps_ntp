#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    use core::convert::TryInto;
    use anyhow::{Context, anyhow, bail};
    use esp_idf_svc::eventloop::EspSystemEventLoop;
    use esp_idf_svc::hal::delay::FreeRtos;
    use esp_idf_svc::hal::gpio;
    use esp_idf_svc::hal::peripherals::Peripherals;
    use esp_idf_svc::hal::prelude::*;
    use esp_idf_svc::hal::uart::{self, UartDriver};
    use esp_idf_svc::nvs::EspDefaultNvsPartition;
    use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

    #[derive(Debug, Clone)]
    struct WifiCredentials {
        ssid: String,
        pass: String,
    }

    fn parse_hhmmss(raw: &str) -> Option<String> {
        if raw.len() < 6 {
            return None;
        }
        Some(format!("{}:{}:{}", &raw[0..2], &raw[2..4], &raw[4..6]))
    }

    fn parse_rmc_utc(sentence: &str) -> Option<String> {
        let fields: Vec<&str> = sentence.split(',').collect();
        if fields.len() < 10 {
            return None;
        }

        let time = parse_hhmmss(fields[1])?;
        let status = fields[2];
        let date = fields[9];
        if date.len() < 6 {
            return None;
        }

        let day = &date[0..2];
        let month = &date[2..4];
        let year = &date[4..6];
        let year_full = format!("20{year}");
        let fix_word = if status == "A" { "fix" } else { "no-fix" };

        Some(format!("{year_full}-{month}-{day}T{time}Z ({fix_word})"))
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
    let pins = peripherals.pins;

    let default_nvs = EspDefaultNvsPartition::take()
        .context("failed to take default NVS partition for Wi-Fi")?;
    let sys_loop = EspSystemEventLoop::take().context("failed to take system event loop")?;
    let _wifi = connect_wifi_sta(modem, sys_loop, default_nvs, &wifi_creds)?;

    // GPS FeatherWing default UART speed is 9600 baud.
    // Current board pin mapping under test: Feather TX/RX -> GPIO1/GPIO2.
    const GPS_UART_TX_PIN: i32 = 1;
    const GPS_UART_RX_PIN: i32 = 2;
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

    let mut rx_buf = [0_u8; 256];
    let mut line_buf = String::new();
    let mut bytes_seen: u64 = 0;

    log::info!("ESP32 booted; Wi-Fi + GPS UART diagnostics mode");
    log::info!(
        "Listening for raw NMEA on UART1 (9600 baud), TX=GPIO{}, RX=GPIO{}",
        GPS_UART_TX_PIN,
        GPS_UART_RX_PIN
    );

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
                                if trimmed.starts_with("$GNRMC")
                                    || trimmed.starts_with("$GPRMC")
                                {
                                    if let Some(utc) = parse_rmc_utc(trimmed) {
                                        log::info!("GPS UTC: {}", utc);
                                    }
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

        FreeRtos::delay_ms(100);
    }
}

#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("This project targets ESP32 with ESP-IDF (run through cargo espflash).");
}
