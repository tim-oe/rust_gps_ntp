//! Wi-Fi station credential loading and connection helpers.

use anyhow::{Context, anyhow, bail};
use core::convert::TryInto;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

/// Compile-time Wi-Fi station credentials captured during firmware build.
#[derive(Debug, Clone)]
pub struct WifiCredentials {
    /// SSID string for station connection.
    pub ssid: String,
    /// WPA/WPA2 passphrase (or empty for open network).
    pub pass: String,
}

/// Load required Wi-Fi credentials from compile-time environment variables.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - `Ok(WifiCredentials)` when both `WIFI_SSID` and `WIFI_PASS` are set.
/// - `Err` when either value is missing.
pub fn load_wifi_credentials_from_env() -> anyhow::Result<WifiCredentials> {
    let env_ssid = option_env!("WIFI_SSID")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let env_pass = option_env!("WIFI_PASS")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);

    if let (Some(ssid), Some(pass)) = (env_ssid, env_pass) {
        log::info!("Wi-Fi: STA SSID loaded: {}", ssid);
        return Ok(WifiCredentials { ssid, pass });
    }

    bail!("No Wi-Fi credentials found. Set WIFI_SSID and WIFI_PASS in your shell before flashing.");
}

/// Connect station Wi-Fi and block until the network interface is up.
///
/// # Parameters
/// - `modem`: ESP modem peripheral.
/// - `sys_loop`: System event loop handle.
/// - `nvs`: Default NVS partition handle for Wi-Fi driver state.
/// - `creds`: Wi-Fi credentials to apply.
///
/// # Returns
/// - `Ok(BlockingWifi<EspWifi<'static>>)` when station link is up with DHCP info.
/// - `Err` when setup, connect, or netif readiness fails.
pub fn connect_wifi_sta(
    modem: Modem,
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

    // Disable Wi-Fi modem sleep so the radio stays awake and can respond to
    // incoming NTP UDP packets without waiting for the next DTIM beacon.
    // The default (WIFI_PS_MIN_MODEM) buffers incoming packets at the AP for
    // up to ~100 ms, which dominates NTP response latency.
    // WIFI_PS_NONE = 0 per esp-idf wifi_ps_type_t enum.
    #[cfg(target_os = "espidf")]
    esp_idf_svc::sys::esp!(unsafe {
        esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE)
    })
    .context("failed to disable Wi-Fi power save")?;

    wifi.connect().context("failed to connect Wi-Fi STA")?;
    wifi.wait_netif_up()
        .context("Wi-Fi netif did not come up")?;

    let ip_info = wifi
        .wifi()
        .sta_netif()
        .get_ip_info()
        .context("failed to read DHCP IP info")?;
    log::info!("Wi-Fi: connected; STA IP: {}", ip_info.ip);

    Ok(wifi)
}
