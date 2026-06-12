//! Board-level peripheral routing: take-once HAL blocks, GPIO pool, and init order.
//!
//! [`BoardBoot::boot`] owns the wiring between Feather modules so [`crate::app`]
//! only receives runtime handles for the main service loop.

use anyhow::Context;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::Arc;

use crate::battery::BatteryDevice;
use crate::display::DisplayDevice;
use crate::gps::{self, GpsSnapshot, GpsUart};
use crate::i2c_bus::FeatherI2cBus;
use crate::ntp::NtpServer;
use crate::pins::PinPool;
use crate::pps::PpsDevice;
use crate::rtc::{self, RtcDevice};
use crate::storage::StorageDevice;
use crate::timezone::{self, TimezoneStore};
use crate::ui_task::{UiFeed, UiTaskHandle};
use crate::wifi::{self, WifiCredentials};
#[cfg(esp_idf_comp_mdns_enabled)]
use esp_idf_svc::mdns::EspMdns;

/// Cached timezone and initial GPS snapshot prepared during board boot.
pub struct TimezoneBoot {
    pub gps: GpsSnapshot,
    pub tz_store: Option<TimezoneStore>,
    pub tz_initialized: bool,
    pub current_tz_name: Option<String>,
}

/// Service-loop handles extracted from [`BoardBoot`].
pub struct BoardRuntime {
    pub gps_uart: GpsUart,
    pub pps: PpsDevice,
    pub ui_feed: Arc<UiFeed>,
    pub rtc_present: bool,
    pub boot_rtc_unix: Option<i64>,
    pub timezone: TimezoneBoot,
}

/// Runtime handles for the main NTP/GPS service loop plus keep-alive drivers.
pub struct BoardBoot {
    pub gps_uart: GpsUart,
    pub pps: PpsDevice,
    pub ui_feed: Arc<UiFeed>,
    pub rtc_present: bool,
    pub boot_rtc_unix: Option<i64>,
    pub timezone: TimezoneBoot,
    keepalive: BoardKeepalive,
}

/// Wi-Fi, display bus, storage mount, and UI task — held for the firmware lifetime.
pub struct BoardKeepalive {
    _wifi: esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    #[cfg(esp_idf_comp_mdns_enabled)]
    _mdns: Option<EspMdns<'static>>,
    _display: DisplayDevice,
    _storage: StorageDevice<'static>,
    _ui_task: UiTaskHandle,
}

impl BoardBoot {
    /// Take ESP32 peripherals, connect network services, and bring up Feather hardware.
    ///
    /// Init order: GPS UART and PPS first, display SPI bus, I2C sensors, microSD
    /// probe on the shared SPI bus, then TFT panel and UI task.
    pub fn boot(wifi_creds: &WifiCredentials) -> anyhow::Result<Self> {
        let peripherals = Peripherals::take().context("failed to take ESP32 peripherals")?;
        let mut pin_pool = PinPool::from_board_pins(peripherals.pins);

        let default_nvs =
            EspDefaultNvsPartition::take().context("failed to take default NVS partition")?;
        let nvs_tz = default_nvs.clone();
        let sys_loop = esp_idf_svc::eventloop::EspSystemEventLoop::take()
            .context("failed to take system event loop")?;
        let wifi = wifi::connect_wifi_sta(peripherals.modem, sys_loop, default_nvs, wifi_creds)?;

        let mut gps_uart = GpsUart::init(&mut pin_pool, peripherals.uart1)?;
        // Arm PPS before display/SD init so edges are captured during slow bring-up.
        let pps = PpsDevice::init(&mut pin_pool)?;

        let mut display = DisplayDevice::init(&mut pin_pool, peripherals.spi2)?;

        let mut i2c_bus = FeatherI2cBus::init(&mut pin_pool, peripherals.i2c0)?;
        let battery = BatteryDevice::detect(&mut i2c_bus);
        let rtc = RtcDevice::init(&mut i2c_bus);
        let rtc_present = rtc.present;
        let boot_rtc_unix = rtc.boot_utc;

        let storage = StorageDevice::init(&mut pin_pool, display.spi_driver());
        let storage_status = storage.status;

        let panel = display.init_panel(&mut pin_pool)?;

        let timezone = load_timezone_boot(nvs_tz, boot_rtc_unix);
        gps_uart.set_snapshot(timezone.gps.clone());

        let ui_feed = UiFeed::new(storage_status);
        if rtc_present {
            ui_feed.publish_rtc(rtc::RtcSnapshot {
                detected: true,
                utc_unix_seconds: boot_rtc_unix,
            });
        }
        ui_feed.publish_gps(&timezone.gps);

        let ui_task = UiTaskHandle::spawn(
            Arc::clone(&ui_feed),
            panel.display,
            panel.button,
            i2c_bus,
            battery,
            rtc,
            panel.backlight_on_state,
        )?;

        log::info!("System: booted; Wi-Fi + GPS UART diagnostics mode");

        Ok(Self {
            gps_uart,
            pps,
            ui_feed,
            rtc_present,
            boot_rtc_unix,
            timezone,
            keepalive: BoardKeepalive {
                _wifi: wifi,
                #[cfg(esp_idf_comp_mdns_enabled)]
                _mdns: register_mdns(),
                _display: display,
                _storage: storage,
                _ui_task: ui_task,
            },
        })
    }

    /// Bind NTP, apply compile-time ACL, and seed from boot-time RTC when available.
    pub fn init_ntp_server(&self) -> anyhow::Result<NtpServer> {
        let mut server = NtpServer::bind()?;
        NtpServer::apply_boot_acl(&mut server);
        if let Some(secs) = self.boot_rtc_unix {
            server.seed_utc_seconds(secs);
            log::info!("RTC: seeded NTP anchor from cached time (UTC {secs})");
        }
        Ok(server)
    }

    /// Split service-loop handles from keep-alive drivers.
    pub fn into_runtime(self) -> (BoardRuntime, BoardKeepalive) {
        (
            BoardRuntime {
                gps_uart: self.gps_uart,
                pps: self.pps,
                ui_feed: self.ui_feed,
                rtc_present: self.rtc_present,
                boot_rtc_unix: self.boot_rtc_unix,
                timezone: self.timezone,
            },
            self.keepalive,
        )
    }
}

fn load_timezone_boot(nvs: EspDefaultNvsPartition, boot_rtc_unix: Option<i64>) -> TimezoneBoot {
    let mut gps = GpsSnapshot::default();
    let tz_store = match TimezoneStore::new(nvs) {
        Ok(store) => Some(store),
        Err(err) => {
            log::warn!("GPS: timezone NVS store unavailable: {}", err);
            None
        }
    };
    let mut tz_initialized = false;
    let mut current_tz_name = None;

    let mut cached_tz_name = None;
    if let Some(store) = tz_store.as_ref() {
        match store.load_cached() {
            Ok(Some(tz_name)) => cached_tz_name = Some((tz_name, "NVS")),
            Ok(None) => {}
            Err(err) => log::warn!("GPS: failed to read timezone cache from NVS: {}", err),
        }
    }
    if cached_tz_name.is_none() {
        if let Some(tz_name) = timezone::load_cached_sd() {
            cached_tz_name = Some((tz_name, "SD"));
        }
    }
    if let Some((tz_name, source)) = cached_tz_name {
        if timezone::apply_cached_timezone(&tz_name, source) {
            tz_initialized = true;
            current_tz_name = Some(tz_name);
        }
    }
    if let Some(utc) = boot_rtc_unix {
        gps.tz_offset_hours = gps::tz_offset_hours_at_unix(utc);
    }
    if !tz_initialized && gps.tz_offset_hours == 0 {
        log::warn!(
            "GPS: no cached timezone; RTC display shows UTC until GPS fix + Wi-Fi resolves TZ"
        );
    }

    TimezoneBoot {
        gps,
        tz_store,
        tz_initialized,
        current_tz_name,
    }
}

#[cfg(esp_idf_comp_mdns_enabled)]
fn register_mdns() -> Option<EspMdns<'static>> {
    match EspMdns::take() {
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
    }
}

#[cfg(not(esp_idf_comp_mdns_enabled))]
#[allow(dead_code)]
fn register_mdns() {}
