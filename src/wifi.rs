//! Wi-Fi station credential loading and connection helpers.

use anyhow::{Context, anyhow, bail};
use core::convert::TryInto;
use embedded_svc::ipv4::Ipv4Addr;
use esp_idf_svc::eventloop::{EspSystemEventLoop, EspSystemSubscription};
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi, WifiEvent,
};

/// STA IP, gateway, hostname, and SSID for the Network display page.
#[derive(Debug, Clone)]
pub struct NetworkSnapshot {
    /// DHCP-assigned station IPv4 address.
    pub ip: String,
    /// Default gateway IPv4 address.
    pub gateway: String,
    /// Compile-time hostname from `CONFIG_LWIP_LOCAL_HOSTNAME`.
    pub hostname: String,
    /// Compile-time SSID from `WIFI_SSID`.
    pub ssid: String,
}

impl Default for NetworkSnapshot {
    fn default() -> Self {
        Self {
            ip: "n/a".to_owned(),
            gateway: "n/a".to_owned(),
            hostname: env!("DEVICE_HOSTNAME").to_owned(),
            ssid: compile_time_ssid(),
        }
    }
}

fn compile_time_ssid() -> String {
    option_env!("WIFI_SSID")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("n/a")
        .to_owned()
}

/// Read the current STA interface addresses from the ESP-IDF netif layer.
pub fn read_network_snapshot() -> NetworkSnapshot {
    let mut snap = NetworkSnapshot::default();

    #[cfg(target_os = "espidf")]
    unsafe {
        use esp_idf_svc::sys::{
            esp_netif_get_handle_from_ifkey, esp_netif_get_ip_info, esp_netif_ip_info_t,
        };

        let netif = esp_netif_get_handle_from_ifkey(c"WIFI_STA_DEF".as_ptr());
        if netif.is_null() {
            return snap;
        }
        let mut ip_info = esp_netif_ip_info_t::default();
        if esp_netif_get_ip_info(netif, &mut ip_info) != 0 {
            return snap;
        }
        snap.ip = Ipv4Addr::from(ip_info.ip.addr.to_le_bytes()).to_string();
        snap.gateway = Ipv4Addr::from(ip_info.gw.addr.to_le_bytes()).to_string();
    }

    snap
}

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
/// - `Ok((BlockingWifi<EspWifi<'static>>, EspSystemSubscription))` when the station
///   link is up with DHCP info. The subscription drives automatic reconnect and
///   MUST be kept alive for the firmware lifetime.
/// - `Err` when setup, connect, or netif readiness fails.
pub fn connect_wifi_sta(
    modem: Modem,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    creds: &WifiCredentials,
) -> anyhow::Result<(
    BlockingWifi<EspWifi<'static>>,
    EspSystemSubscription<'static>,
)> {
    // Clone the loop before it is moved into the driver so the reconnect handler
    // can subscribe to the same event source after the initial connection is up.
    let reconnect_loop = sys_loop.clone();
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

    // Subscribe only after the initial connection succeeds so the handler does
    // not race the blocking connect sequence during boot.
    let reconnect = subscribe_wifi_reconnect(&reconnect_loop)?;

    Ok((wifi, reconnect))
}

/// Subscribe to Wi-Fi STA disconnect events and re-initiate association.
///
/// The firmware otherwise connects once at boot; without this, any AP drop,
/// roam, or beacon timeout leaves the station disconnected with IP `0.0.0.0`
/// (unreachable to all clients) until a manual reset. `esp_netif` restarts the
/// DHCP client automatically on reassociation, so re-acquiring the lease needs
/// no extra call here.
///
/// # Parameters
/// - `sys_loop`: System event loop the Wi-Fi driver posts events on.
///
/// # Returns
/// - `Ok(EspSystemSubscription)` that MUST be kept alive for the handler to run;
///   dropping it unsubscribes and disables automatic reconnect.
/// - `Err` when the event subscription cannot be registered.
fn subscribe_wifi_reconnect(
    sys_loop: &EspSystemEventLoop,
) -> anyhow::Result<EspSystemSubscription<'static>> {
    sys_loop
        .subscribe::<WifiEvent, _>(|event| {
            if let WifiEvent::StaDisconnected(_) = event {
                log::warn!("Wi-Fi: STA disconnected; re-initiating association");
                let err = unsafe { esp_idf_svc::sys::esp_wifi_connect() };
                if err != esp_idf_svc::sys::ESP_OK as i32 {
                    log::warn!("Wi-Fi: esp_wifi_connect() failed (err={err})");
                }
            }
        })
        .context("failed to subscribe to Wi-Fi reconnect events")
}
