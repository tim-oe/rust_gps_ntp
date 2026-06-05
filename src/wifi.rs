use anyhow::{anyhow, bail, Context};
use core::convert::TryInto;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};

#[derive(Debug, Clone)]
pub struct WifiCredentials {
    pub ssid: String,
    pub pass: String,
}

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
        log::info!("Wi-Fi STA SSID loaded: {}", ssid);
        return Ok(WifiCredentials { ssid, pass });
    }

    bail!("No Wi-Fi credentials found. Set WIFI_SSID and WIFI_PASS in your shell before flashing.");
}

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
