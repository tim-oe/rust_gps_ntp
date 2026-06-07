//! MicroSD storage on the Adafruit Adalogger FeatherWing (SPI + FAT VFS).
//!
//! The SD socket shares the Feather SPI bus with the TFT display; both devices
//! use separate chip-select lines and borrow the same [`SpiDriver`] instance.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use esp_idf_svc::fs::fatfs::Fatfs;
use esp_idf_svc::hal::gpio::{self, AnyIOPin};
use esp_idf_svc::hal::sd::{SdCardDriver, config::Configuration, spi::SdSpiHostDriver};
use esp_idf_svc::hal::spi::{Dma, SpiDriver, config::DriverConfig};
use esp_idf_svc::io::vfs::MountedFatfs;

/// VFS mount point for the Adalogger microSD card.
pub const MOUNT_POINT: &str = "/sdcard";

/// Marker file written at mount to confirm read/write access.
const READY_MARKER: &str = "/sdcard/.rust_gps_ntp_ready";

/// Adalogger SD card chip-select on ESP32-S3 TFT Feather.
///
/// On this board the Adalogger wing CS trace lands on **GPIO10** (not GPIO33,
/// which is the onboard NeoPixel data line). GPIO33 is kept as a fallback for
/// rewired wings and classic ESP32 Feathers.
///
/// See [Adalogger pinouts](https://learn.adafruit.com/adafruit-adalogger-featherwing/pinouts).
pub const ADALOGGER_SD_CS_PIN: i32 = 10;
/// Fallback SD CS (classic ESP32 Adalogger default / NeoPixel line on S3 TFT).
pub const ADALOGGER_SD_CS_ALT_PIN: i32 = 33;

/// Feather SPI clock line shared by TFT and SD card.
pub const FEATHER_SPI_SCK_PIN: i32 = 36;
/// Feather SPI MOSI line shared by TFT and SD card.
pub const FEATHER_SPI_MOSI_PIN: i32 = 35;
/// Feather SPI MISO line from the SD card socket.
pub const FEATHER_SPI_MISO_PIN: i32 = 37;

/// Mount-time status surfaced on the Resources display page.
#[derive(Debug, Clone, Copy, Default)]
pub struct StorageStatus {
    pub mounted: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

type SdHost<'d> = SdSpiHostDriver<'d, &'d SpiDriver<'d>>;
type SdCard<'d> = SdCardDriver<SdHost<'d>>;
type SdFatfs<'d> = Fatfs<SdCard<'d>>;

/// Keeps the SD host, card driver, and FAT VFS mount alive for the firmware lifetime.
pub struct StorageMount<'d> {
    _mounted: MountedFatfs<SdFatfs<'d>>,
    pub status: StorageStatus,
}

impl StorageStatus {
    /// Read mount state and FAT free/total bytes from the VFS layer.
    pub fn refresh(path: &str, previous: StorageStatus) -> Self {
        if !Path::new(path).exists() {
            return StorageStatus::default();
        }
        match vfs_fat_usage(path) {
            Some((total_bytes, free_bytes)) => StorageStatus {
                mounted: true,
                total_bytes,
                free_bytes,
            },
            None => StorageStatus {
                mounted: true,
                total_bytes: previous.total_bytes,
                free_bytes: previous.free_bytes,
            },
        }
    }
}

fn vfs_fat_usage(path: &str) -> Option<(u64, u64)> {
    use std::ffi::CString;

    let c_path = CString::new(path).ok()?;
    let mut total_bytes = 0_u64;
    let mut free_bytes = 0_u64;
    let rc = unsafe {
        esp_idf_svc::sys::esp_vfs_fat_info(c_path.as_ptr(), &mut total_bytes, &mut free_bytes)
    };
    if rc == esp_idf_svc::sys::ESP_OK as i32 {
        Some((total_bytes, free_bytes))
    } else {
        log::debug!("Storage: esp_vfs_fat_info({path}) failed rc={rc}");
        None
    }
}

fn card_capacity_bytes(card: &esp_idf_svc::sys::sdmmc_card_t) -> u64 {
    (card.csd.capacity as u64).saturating_mul(card.csd.sector_size as u64)
}

/// Attempt to mount the Adalogger microSD card on the shared SPI bus.
///
/// Call with TFT CS (`GPIO7`) already driven high so the display does not
/// respond to SD bus traffic. Returns `(status, mount_handle)`.
pub fn try_mount<'d, CS>(
    spi: &'d SpiDriver<'d>,
    cs: CS,
    cs_pin: i32,
) -> (StorageStatus, Option<StorageMount<'d>>)
where
    CS: gpio::OutputPin + 'd,
{
    log::info!("Storage: probing microSD on CS=GPIO{cs_pin}");

    let mut sd_config = Configuration::new();
    sd_config.speed_khz = 400;
    sd_config.command_timeout_ms = 5_000;

    let sd_host = match SdSpiHostDriver::new(
        spi,
        Some(cs),
        AnyIOPin::none(),
        AnyIOPin::none(),
        AnyIOPin::none(),
        None,
    ) {
        Ok(host) => host,
        Err(err) => {
            log::warn!("Storage: SD SPI host init failed: {err}");
            return (StorageStatus::default(), None);
        }
    };

    let sd_card = match SdCardDriver::new_spi(sd_host, &sd_config) {
        Ok(card) => card,
        Err(err) => {
            log::warn!("Storage: SD card init failed on GPIO{cs_pin}: {err}");
            return (StorageStatus::default(), None);
        }
    };

    let total_bytes = card_capacity_bytes(sd_card.card());
    let fatfs = match Fatfs::new_sdcard(0, sd_card) {
        Ok(fs) => fs,
        Err(err) => {
            log::warn!("Storage: FAT layer registration failed: {err}");
            return (StorageStatus::default(), None);
        }
    };

    let mounted = match MountedFatfs::mount(fatfs, MOUNT_POINT, 4) {
        Ok(mount) => mount,
        Err(err) => {
            log::warn!("Storage: VFS mount at {MOUNT_POINT} failed: {err}");
            return (StorageStatus::default(), None);
        }
    };

    if !write_ready_marker() {
        log::warn!("Storage: mounted but marker write failed");
    }

    let mut status = StorageStatus {
        mounted: true,
        total_bytes,
        free_bytes: 0,
    };
    if let Some((fs_total, fs_free)) = vfs_fat_usage(MOUNT_POINT) {
        status.total_bytes = fs_total;
        status.free_bytes = fs_free;
    }
    log::info!(
        "Storage: microSD mounted at {MOUNT_POINT} on GPIO{cs_pin} (free={}, total={})",
        status.free_bytes,
        status.total_bytes
    );

    (
        status,
        Some(StorageMount {
            _mounted: mounted,
            status,
        }),
    )
}

fn write_ready_marker() -> bool {
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(READY_MARKER)
    {
        Ok(mut file) => file.write_all(b"ok").is_ok(),
        Err(err) => {
            log::debug!("Storage: marker write failed: {err}");
            false
        }
    }
}

/// Return `true` when the SD card VFS is mounted and the ready marker exists.
pub fn is_ready() -> bool {
    Path::new(MOUNT_POINT).exists() && Path::new(READY_MARKER).exists()
}

/// Recommended SPI bus configuration for the shared TFT + SD bus.
pub fn shared_spi_bus_config() -> DriverConfig {
    DriverConfig::new().dma(Dma::Auto(4096))
}
