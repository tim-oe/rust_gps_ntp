//! MicroSD storage on the Adafruit Adalogger FeatherWing (SPI + FAT VFS).
//!
//! The SD socket shares the Feather SPI bus with the TFT display; both devices
//! use separate chip-select lines and borrow the same [`SpiDriver`] instance.
//! [`StorageDevice::init`] claims the SD CS GPIO from [`crate::pins::PinPool`].
//!
//! Every filesystem call after the guard starts runs on the `sd_io` thread.
//! `spi_device_polling_transmit` busy-waits with `portMAX_DELAY`, and that wait
//! cannot be cancelled without leaving the SPI bus lock, chip-select, and the
//! FatFS mutex held. The `sd_wd` thread sits above `sd_io` and restarts the
//! chip when a call stays inside that wait longer than [`SD_CALL_TIMEOUT_US`].

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context;
use esp_idf_svc::fs::fatfs::Fatfs;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{self, AnyIOPin};
use esp_idf_svc::hal::sd::{SdCardDriver, config::Configuration, spi::SdSpiHostDriver};
use esp_idf_svc::hal::spi::{Dma, SpiDriver, config::DriverConfig};
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use esp_idf_svc::io::vfs::MountedFatfs;
use portable_atomic::AtomicI64;

use crate::pins::PinPool;

/// VFS mount point for the Adalogger microSD card.
pub const MOUNT_POINT: &str = "/sdcard";

/// How often the panel asks the SD worker to reread FAT free/total bytes.
pub const STATUS_REFRESH_US: i64 = 60_000_000;

/// One SD filesystem call must return within this long.
///
/// A full FAT free-space walk at the 400 kHz card clock still finishes inside
/// this window on the cards this board uses. A transfer that never completes
/// does not, and the watchdog then restarts the chip.
pub const SD_CALL_TIMEOUT_US: i64 = 180_000_000;

/// Below the watchdog. Low enough that a polling SPI transfer cannot starve
/// the panel (4), the button (6), or the main loop (10).
const SD_WORKER_PRIORITY: u8 = 2;
/// Above the worker so a tick preempts `spi_device_polling_end`. Below the
/// panel, so a card call does not delay drawing once the bus is free.
const SD_WATCHDOG_PRIORITY: u8 = 3;
const SD_THREAD_STACK: usize = 16 * 1024;
const SD_WATCHDOG_STACK: usize = 8 * 1024;
const SD_WATCHDOG_POLL_MS: u32 = 250;

/// Marker file written at mount to confirm read/write access.
pub const READY_MARKER: &str = "/sdcard/.rust_gps_ntp_ready";

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

// SAFETY: `MountedFatfs` holds a `*mut FATFS` that the VFS owns for the life of
// the registration. This handle only keeps that registration from being
// dropped. The mount thread joins before the handle is used on the caller,
// and later filesystem calls go through `sd_io` rather than through this
// pointer. Moving the handle across that join does not alias it.
unsafe impl<'d> Send for StorageMount<'d> {}

/// `esp_timer` timestamp of the SD call in progress. `0` means idle.
static SD_CALL_STARTED_US: AtomicI64 = AtomicI64::new(0);
static SD_CALL_LABEL: Mutex<&str> = Mutex::new("sd");
static SD_CARD_READY: AtomicBool = AtomicBool::new(false);
static SD_USAGE_REFRESH: AtomicBool = AtomicBool::new(false);
static SD_LAST_USAGE: Mutex<StorageStatus> = Mutex::new(StorageStatus {
    mounted: false,
    total_bytes: 0,
    free_bytes: 0,
});
static SD_PENDING_USAGE: Mutex<Option<StorageStatus>> = Mutex::new(None);
static SD_JOBS: OnceLock<mpsc::Sender<SdJob>> = OnceLock::new();
/// FreeRTOS task handle of `sd_io`. Nested `call`s on that task run inline.
static SD_WORKER_TASK: AtomicUsize = AtomicUsize::new(0);
/// Outermost [`SdDeadline`] clears [`SD_CALL_STARTED_US`]. Mount and `sd_io`
/// never hold a deadline at the same time.
static SD_CALL_DEPTH: AtomicU32 = AtomicU32::new(0);

struct SdJob {
    label: &'static str,
    run: Box<dyn FnOnce() + Send>,
}

/// Arms [`SD_CALL_STARTED_US`] and clears it when the outermost call returns.
struct SdDeadline;

impl SdDeadline {
    fn arm(label: &'static str) -> Self {
        let outermost = SD_CALL_DEPTH.fetch_add(1, Ordering::AcqRel) == 0;
        if outermost {
            *SD_CALL_LABEL.lock().unwrap_or_else(|err| err.into_inner()) = label;
        }
        // `0` is the idle sentinel, including at the instant after boot.
        let now = monotonic_us().max(1);
        SD_CALL_STARTED_US.store(now, Ordering::Release);
        Self
    }
}

impl Drop for SdDeadline {
    fn drop(&mut self) {
        if SD_CALL_DEPTH.fetch_sub(1, Ordering::AcqRel) <= 1 {
            SD_CALL_STARTED_US.store(0, Ordering::Release);
        }
    }
}

struct SdGuard {
    _worker: JoinHandle<()>,
    _watchdog: JoinHandle<()>,
}

/// Run `op` on the SD worker and wait for it to finish.
///
/// The watchdog restarts the chip when `op` stays inside the driver longer
/// than [`SD_CALL_TIMEOUT_US`]. A call made from the SD thread itself runs
/// inline so a nested filesystem operation cannot deadlock the worker.
pub fn call<T: Send + 'static>(label: &'static str, op: impl FnOnce() -> T + Send + 'static) -> T {
    if on_sd_worker() {
        let _deadline = SdDeadline::arm(label);
        return op();
    }
    let Some(tx) = SD_JOBS.get() else {
        restart_stuck_sd("sd worker missing");
    };
    let (done_tx, done_rx) = mpsc::channel();
    let job = SdJob {
        label,
        run: Box::new(move || {
            let _ = done_tx.send(op());
        }),
    };
    if tx.send(job).is_err() {
        restart_stuck_sd("sd worker stopped");
    }
    done_rx.recv().unwrap_or_else(|_| restart_stuck_sd(label))
}

/// Ask the SD worker to reread free space. The panel copies the result later
/// with [`take_usage`], so a slow or stuck card does not block drawing.
pub fn request_usage_refresh() {
    SD_USAGE_REFRESH.store(true, Ordering::Release);
}

/// Take a free-space sample the worker has finished since the last call.
pub fn take_usage() -> Option<StorageStatus> {
    SD_PENDING_USAGE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
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
    // SAFETY: `c_path` is a valid NUL-terminated path that outlives the call,
    // and both out-parameters are live `u64`s matching the FFI signature.
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

/// Mounted microSD card (or unmounted probe result) with CS GPIO tracking.
pub struct StorageDevice<'d> {
    _mount: Option<StorageMount<'d>>,
    pub status: StorageStatus,
    cs_pin: Option<i32>,
    _guard: Option<SdGuard>,
}

impl<'d> StorageDevice<'d> {
    const MODULE: &'static str = "storage";

    /// Probe and mount the Adalogger microSD card on the shared SPI bus.
    ///
    /// TFT CS must already be deselected on the shared bus. Tries the primary
    /// CS pin first, then the alternate wing wiring.
    pub fn init(pool: &mut PinPool, spi: &'d SpiDriver<'d>) -> Self {
        let guard = match start_guard() {
            Ok(guard) => Some(guard),
            Err(err) => {
                log::error!("Storage: SD watchdog failed to start: {err:#}");
                return Self {
                    _mount: None,
                    status: StorageStatus::default(),
                    cs_pin: None,
                    _guard: None,
                };
            }
        };

        if let Ok(cs) = pool.take_gpio10(Self::MODULE) {
            let (status, mount) =
                run_on_sd_thread("sd mount", || try_mount(spi, cs, ADALOGGER_SD_CS_PIN));
            if status.mounted {
                remember_usage(status);
                return Self {
                    _mount: mount,
                    status,
                    cs_pin: Some(ADALOGGER_SD_CS_PIN),
                    _guard: guard,
                };
            }
            pool.release(ADALOGGER_SD_CS_PIN);
        }

        log::info!(
            "Storage: retrying on GPIO{} (alternate Adalogger CS)",
            ADALOGGER_SD_CS_ALT_PIN
        );
        if let Ok(cs) = pool.take_gpio33(Self::MODULE) {
            let (status, mount) =
                run_on_sd_thread("sd mount", || try_mount(spi, cs, ADALOGGER_SD_CS_ALT_PIN));
            if status.mounted {
                remember_usage(status);
                return Self {
                    _mount: mount,
                    status,
                    cs_pin: Some(ADALOGGER_SD_CS_ALT_PIN),
                    _guard: guard,
                };
            }
            pool.release(ADALOGGER_SD_CS_ALT_PIN);
        }

        log::warn!(
            "Storage: microSD unavailable — insert a FAT32 card; tried CS GPIO{} and GPIO{}",
            ADALOGGER_SD_CS_PIN,
            ADALOGGER_SD_CS_ALT_PIN
        );
        Self {
            _mount: None,
            status: StorageStatus::default(),
            cs_pin: None,
            _guard: guard,
        }
    }

    /// Release the SD chip-select GPIO claim when a card was mounted.
    pub fn close(self, pool: &mut PinPool) {
        if let Some(pin) = self.cs_pin {
            pool.release(pin);
        }
    }
}

/// Attempt to mount the Adalogger microSD card on the shared SPI bus.
///
/// Call with TFT CS (`GPIO7`) already driven high so the display does not
/// respond to SD bus traffic. Returns `(status, mount_handle)`.
fn try_mount<'d, CS>(
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

    if write_ready_marker() {
        SD_CARD_READY.store(true, Ordering::Release);
    } else {
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

/// Return `true` when the card mounted and the ready marker was written.
///
/// This is a flag set at mount. Checking the directory from an arbitrary task
/// would enter the same SPI wait the watchdog exists to bound.
pub fn is_ready() -> bool {
    SD_CARD_READY.load(Ordering::Acquire)
}

/// Recommended SPI bus configuration for the shared TFT + SD bus.
pub fn shared_spi_bus_config() -> DriverConfig {
    DriverConfig::new().dma(Dma::Auto(4096))
}

fn start_guard() -> anyhow::Result<SdGuard> {
    let (tx, rx) = mpsc::channel();
    SD_JOBS
        .set(tx)
        .map_err(|_| anyhow::anyhow!("SD worker already started"))?;

    let worker = spawn_sd_thread("sd_io", SD_WORKER_PRIORITY, SD_THREAD_STACK, move || {
        SD_WORKER_TASK.store(this_task(), Ordering::Release);
        worker_main(rx);
    })?;
    let watchdog = spawn_sd_thread(
        "sd_wd",
        SD_WATCHDOG_PRIORITY,
        SD_WATCHDOG_STACK,
        monitor_main,
    )?;
    log::info!(
        "Storage: SD calls on sd_io (priority {SD_WORKER_PRIORITY}); restart after {} s",
        SD_CALL_TIMEOUT_US / 1_000_000
    );
    Ok(SdGuard {
        _worker: worker,
        _watchdog: watchdog,
    })
}

fn spawn_sd_thread(
    name: &'static str,
    priority: u8,
    stack: usize,
    body: impl FnOnce() + Send + 'static,
) -> anyhow::Result<JoinHandle<()>> {
    ThreadSpawnConfiguration {
        priority,
        stack_size: stack,
        inherit: false,
        ..Default::default()
    }
    .set()
    .with_context(|| format!("failed to configure {name}"))?;

    let spawned = thread::Builder::new()
        .name(name.into())
        .stack_size(stack)
        .spawn(body);

    ThreadSpawnConfiguration::default()
        .set()
        .context("failed to restore thread spawn configuration")?;

    spawned.with_context(|| format!("failed to spawn {name}"))
}

/// Mount borrows the shared SPI driver, so it cannot be queued as a `'static`
/// job. It still runs below the watchdog, which is already up.
fn run_on_sd_thread<T: Send>(label: &'static str, op: impl FnOnce() -> T + Send) -> T {
    ThreadSpawnConfiguration {
        priority: SD_WORKER_PRIORITY,
        stack_size: SD_THREAD_STACK,
        inherit: false,
        ..Default::default()
    }
    .set()
    .unwrap_or_else(|err| restart_stuck_sd_config(label, err));

    let value = thread::scope(|scope| {
        scope
            .spawn(|| {
                let _deadline = SdDeadline::arm(label);
                op()
            })
            .join()
            .unwrap_or_else(|_| restart_stuck_sd(label))
    });

    if let Err(err) = ThreadSpawnConfiguration::default().set() {
        log::warn!("Storage: failed to restore thread spawn configuration: {err}");
    }
    value
}

fn worker_main(rx: mpsc::Receiver<SdJob>) {
    loop {
        if SD_USAGE_REFRESH.swap(false, Ordering::AcqRel) && is_ready() {
            let previous = *SD_LAST_USAGE.lock().unwrap_or_else(|err| err.into_inner());
            let status = {
                let _deadline = SdDeadline::arm("sd space");
                StorageStatus::refresh(MOUNT_POINT, previous)
            };
            remember_usage(status);
            *SD_PENDING_USAGE
                .lock()
                .unwrap_or_else(|err| err.into_inner()) = Some(status);
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(job) => {
                let _deadline = SdDeadline::arm(job.label);
                (job.run)();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn monitor_main() {
    loop {
        FreeRtos::delay_ms(SD_WATCHDOG_POLL_MS);
        let started = SD_CALL_STARTED_US.load(Ordering::Acquire);
        if started == 0 {
            continue;
        }
        let elapsed = monotonic_us().saturating_sub(started);
        if elapsed < SD_CALL_TIMEOUT_US {
            continue;
        }
        let label = *SD_CALL_LABEL.lock().unwrap_or_else(|err| err.into_inner());
        log::error!(
            "Storage: {label} still running after {} s; restarting",
            elapsed / 1_000_000
        );
        restart_stuck_sd(label);
    }
}

fn remember_usage(status: StorageStatus) {
    *SD_LAST_USAGE.lock().unwrap_or_else(|err| err.into_inner()) = status;
}

fn on_sd_worker() -> bool {
    let worker = SD_WORKER_TASK.load(Ordering::Acquire);
    worker != 0 && worker == this_task()
}

fn this_task() -> usize {
    // SAFETY: `xTaskGetCurrentTaskHandle` returns the calling task. Callers
    // run only after the scheduler has started.
    unsafe { esp_idf_svc::sys::xTaskGetCurrentTaskHandle() as usize }
}

fn monotonic_us() -> i64 {
    // SAFETY: plain FFI read of the 64-bit high-resolution timer; no
    // preconditions and safe from any task context.
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}

fn restart_stuck_sd(label: &str) -> ! {
    log::error!("Storage: '{label}' cannot be cancelled; restarting");
    // SAFETY: `esp_restart` takes no arguments and does not return. The
    // monitor calls it without holding the SPI bus or the FatFS mutex. Those
    // stay with the stuck `sd_io` task, which a task delete would leak; a
    // chip reset clears the peripheral, DMA, chip-select, and the filesystem
    // lock together.
    unsafe { esp_idf_svc::sys::esp_restart() }
}

fn restart_stuck_sd_config(label: &str, err: esp_idf_svc::sys::EspError) -> ! {
    log::error!("Storage: failed to configure {label} thread: {err}");
    restart_stuck_sd(label);
}
