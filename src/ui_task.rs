//! Display, button, and battery sampling on a dedicated FreeRTOS task.
//!
//! SPI drawing and I2C battery reads run here so the main service loop can
//! focus on GPS ingest, PPS discipline, and NTP polling.

use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use anyhow::Context;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{self, Input, PinDriver, Pull};
use portable_atomic::{AtomicI64, AtomicU32};
use st7789::{BacklightState, ST7789};
use std::sync::atomic::Ordering;

use crate::battery::{self, BatteryDevice, BatterySnapshot};
use crate::display::{self, Page};
use crate::gps::GpsSnapshot;
use crate::i2c_bus::FeatherI2cBus;
use crate::ntp::NtpSnapshot;
use crate::rtc::{self, RtcSnapshot};
use crate::storage::StorageStatus;
use crate::wifi;

const UI_TASK_STACK_BYTES: usize = 16_384;
const BATTERY_SAMPLE_US: i64 = 5_000_000;
const RTC_SAMPLE_US: i64 = 1_000_000;
const DRAW_INTERVAL_US: i64 = 5_000_000;
const SCREEN_BLANK_US: i64 = 15_000_000;
const UI_LOOP_DELAY_MS: u32 = 10;

/// Shared GPS, PPS, NTP discipline, RTC, and storage samples written by tasks.
pub struct UiFeed {
    gps: RwLock<GpsSnapshot>,
    pps_delta_us: AtomicU32,
    ntp: RwLock<NtpSnapshot>,
    rtc: RwLock<RtcSnapshot>,
    storage: RwLock<StorageStatus>,
    rtc_write_pending: AtomicI64,
}

impl UiFeed {
    /// Create a new feed with an empty GPS snapshot, zero PPS delta, and default NTP state.
    ///
    /// # Parameters
    /// - None.
    ///
    /// # Returns
    /// - `Arc<UiFeed>` ready to share between the main loop and UI task.
    pub fn new(storage: StorageStatus) -> Arc<Self> {
        Arc::new(Self {
            gps: RwLock::new(GpsSnapshot::default()),
            pps_delta_us: AtomicU32::new(0),
            ntp: RwLock::new(NtpSnapshot::default()),
            rtc: RwLock::new(RtcSnapshot::default()),
            storage: RwLock::new(storage),
            rtc_write_pending: AtomicI64::new(0),
        })
    }

    /// Publish the latest GPS snapshot for UI rendering.
    ///
    /// # Parameters
    /// - `gps`: Current GPS snapshot to copy into shared feed state.
    ///
    /// # Returns
    /// - No return value. Silently no-ops if the feed lock is poisoned.
    pub fn publish_gps(&self, gps: &GpsSnapshot) {
        if let Ok(mut guard) = self.gps.write() {
            *guard = gps.clone();
        }
    }

    /// Publish the latest PPS pulse interval for UI rendering.
    ///
    /// # Parameters
    /// - `delta_us`: Interval between consecutive PPS edges in microseconds.
    ///
    /// # Returns
    /// - No return value.
    pub fn publish_pps_delta(&self, delta_us: u32) {
        self.pps_delta_us.store(delta_us, Ordering::Relaxed);
    }

    /// Publish the latest NTP discipline snapshot for UI rendering.
    ///
    /// # Parameters
    /// - `snap`: Current discipline snapshot from `NtpServer::ntp_snapshot`.
    ///
    /// # Returns
    /// - No return value. Silently no-ops if the feed lock is poisoned.
    pub fn publish_ntp(&self, snap: NtpSnapshot) {
        if let Ok(mut guard) = self.ntp.write() {
            *guard = snap;
        }
    }

    /// Publish the latest cached RTC sample (for example after a boot-time read).
    pub fn publish_rtc(&self, snap: RtcSnapshot) {
        if let Ok(mut guard) = self.rtc.write() {
            *guard = snap;
        }
    }

    /// Read the latest cached RTC sample for GPS-loss time fallback.
    pub fn rtc(&self) -> RtcSnapshot {
        self.rtc.read().map(|guard| *guard).unwrap_or_default()
    }

    /// Queue a disciplined UTC write to the PCF8523 (handled by the UI task).
    pub fn request_rtc_write(&self, utc_unix_seconds: i64) {
        self.rtc_write_pending
            .store(utc_unix_seconds, Ordering::Relaxed);
    }

    /// Read cached microSD storage status for the Resources page.
    pub fn storage(&self) -> StorageStatus {
        self.storage.read().map(|guard| *guard).unwrap_or_default()
    }

    /// Update storage status after mount or periodic refresh.
    pub fn publish_storage(&self, status: StorageStatus) {
        if let Ok(mut guard) = self.storage.write() {
            *guard = status;
        }
    }

    /// Read a consistent GPS snapshot, PPS delta, NTP snapshot, and storage for one draw pass.
    ///
    /// # Parameters
    /// - `self`: Shared feed state.
    ///
    /// # Returns
    /// - Tuple of cloned GPS snapshot, PPS delta in microseconds, NTP snapshot, storage, and RTC.
    fn snapshot(&self) -> (GpsSnapshot, u32, NtpSnapshot, StorageStatus, RtcSnapshot) {
        let gps = self
            .gps
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let pps_delta_us = self.pps_delta_us.load(Ordering::Relaxed);
        let ntp = self.ntp.read().map(|guard| *guard).unwrap_or_default();
        let storage = self.storage();
        let rtc = self.rtc();
        (gps, pps_delta_us, ntp, storage, rtc)
    }
}

/// Join handle that keeps the UI worker thread alive for the firmware lifetime.
pub struct UiTaskHandle {
    _thread: JoinHandle<()>,
}

impl UiTaskHandle {
    /// Spawn the UI task with exclusive ownership of display and input hardware.
    ///
    /// # Parameters
    /// - `feed`: Shared feed updated by the main service loop.
    /// - `display`: Initialized ST7789 panel driver (moved into the UI task).
    /// - `button`: Page/wake button on `GPIO0` (moved into the UI task).
    /// - `i2c_bus`: Shared Feather I2C bus for battery and RTC access (moved into the UI task).
    /// - `battery`: Detected fuel gauge, if any, for periodic sampling.
    /// - `rtc`: Boot-time RTC probe result.
    /// - `backlight_on_state`: Backlight level that represents the panel-on state.
    ///
    /// # Returns
    /// - `Ok(UiTaskHandle)` when the UI thread starts successfully.
    /// - `Err` when pull-up setup or thread spawn fails.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn<DI, RST, BL, PinE>(
        feed: Arc<UiFeed>,
        mut display: ST7789<DI, RST, BL>,
        mut button: PinDriver<'static, gpio::Gpio0, Input>,
        mut i2c_bus: FeatherI2cBus,
        battery: BatteryDevice,
        rtc: rtc::RtcDevice,
        backlight_on_state: BacklightState,
    ) -> anyhow::Result<Self>
    where
        DI: display_interface::WriteOnlyDataCommand + Send + 'static,
        RST: embedded_hal_02::digital::v2::OutputPin<Error = PinE> + Send + 'static,
        BL: embedded_hal_02::digital::v2::OutputPin<Error = PinE> + Send + 'static,
        PinE: core::fmt::Debug + Send + 'static,
        ST7789<DI, RST, BL>: embedded_graphics::prelude::DrawTarget<
                Color = embedded_graphics::pixelcolor::Rgb565,
                Error = st7789::Error<PinE>,
            > + Send
            + 'static,
    {
        if let Err(err) = button.set_pull(Pull::Up) {
            log::warn!("Display: failed to enable button pull-up: {}", err);
        }

        // Run below the main NTP/GPS loop (priority 10) so display work never
        // delays time-critical NTP packet processing.
        #[cfg(target_os = "espidf")]
        esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration {
            priority: 5,
            ..Default::default()
        }
        .set()
        .context("failed to configure UI task priority")?;

        let thread = thread::Builder::new()
            .name("ui_task".into())
            .stack_size(UI_TASK_STACK_BYTES)
            .spawn(move || {
                ui_task_main(
                    feed,
                    &mut display,
                    &mut button,
                    &mut i2c_bus,
                    battery,
                    rtc,
                    backlight_on_state,
                );
            })
            .context("failed to spawn UI task")?;

        // Restore default spawn configuration so subsequent thread::spawn
        // calls elsewhere are not inadvertently affected.
        #[cfg(target_os = "espidf")]
        esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration::default()
            .set()
            .context("failed to restore thread spawn configuration")?;

        log::info!(
            "Display: UI task started (stack {} bytes)",
            UI_TASK_STACK_BYTES
        );
        Ok(Self { _thread: thread })
    }
}

/// UI task body: sample battery, handle button input, and redraw the TFT.
///
/// # Parameters
/// - `feed`: Shared GPS/PPS feed written by the main loop.
/// - `display`: ST7789 panel used for SPI drawing.
/// - `button`: Active-low page button with internal pull-up.
/// - `i2c_bus`: Shared Feather I2C bus for battery and RTC register access.
/// - `battery`: Detected fuel gauge, if any.
/// - `rtc`: Boot-time RTC probe result.
/// - `backlight_on_state`: Backlight level representing panel-on.
///
/// # Returns
/// - Never returns under normal operation (infinite UI loop).
fn ui_task_main<DI, RST, BL, PinE>(
    feed: Arc<UiFeed>,
    display: &mut ST7789<DI, RST, BL>,
    button: &mut PinDriver<'static, gpio::Gpio0, Input>,
    i2c_bus: &mut FeatherI2cBus,
    battery: BatteryDevice,
    rtc: rtc::RtcDevice,
    backlight_on_state: BacklightState,
) where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    BL: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    PinE: core::fmt::Debug,
    ST7789<DI, RST, BL>: embedded_graphics::prelude::DrawTarget<
            Color = embedded_graphics::pixelcolor::Rgb565,
            Error = st7789::Error<PinE>,
        >,
{
    let mut ets = Ets;
    let mut battery_sample = BatterySnapshot::default();
    let mut last_battery_us = 0_i64;
    let mut last_rtc_us = 0_i64;
    let mut last_draw_us = 0_i64;
    let mut last_button_pressed = false;
    let mut screen_on = true;
    let mut current_page = Page::Time;
    let mut last_interaction_us = monotonic_us();
    let mut force_redraw = true;
    let mut rendered_once = false;

    loop {
        let now_us = monotonic_us();

        if last_battery_us == 0 || (now_us - last_battery_us) >= BATTERY_SAMPLE_US {
            if let Some(kind) = battery.monitor {
                match battery::read_battery(i2c_bus, kind) {
                    Ok(reading) => battery_sample = reading,
                    Err(err) => log::debug!("Battery: read failed: {}", err),
                }
            }
            last_battery_us = now_us;
        }

        if rtc.present {
            let pending_write = feed.rtc_write_pending.swap(0, Ordering::Relaxed);
            if pending_write > 0 {
                match rtc::write_unix_seconds(i2c_bus, pending_write) {
                    Ok(()) => log::debug!("RTC: wrote UTC {pending_write} from discipline"),
                    Err(err) => log::warn!("RTC: write failed: {err}"),
                }
            }

            if last_rtc_us == 0 || (now_us - last_rtc_us) >= RTC_SAMPLE_US {
                let snap = match rtc::read_unix_seconds(i2c_bus) {
                    Ok(secs) => RtcSnapshot {
                        detected: true,
                        utc_unix_seconds: Some(secs),
                    },
                    Err(err) => {
                        log::debug!("RTC: read failed: {err}");
                        RtcSnapshot {
                            detected: true,
                            utc_unix_seconds: None,
                        }
                    }
                };
                if let Ok(mut guard) = feed.rtc.write() {
                    *guard = snap;
                }
                last_rtc_us = now_us;
            }
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
            && (now_us - last_interaction_us) >= SCREEN_BLANK_US
        {
            screen_on = false;
            if let Err(err) = display.set_backlight(display::backlight_off_state(), &mut ets) {
                log::warn!("Display: failed to turn backlight off: {:?}", err);
            }
        }

        if screen_on && (force_redraw || (now_us - last_draw_us) >= DRAW_INTERVAL_US) {
            let (gps, pps_delta_us, ntp, storage, rtc) = feed.snapshot();
            let network = wifi::read_network_snapshot();
            let mut panel = display::make_panel(display);
            display::draw_page(
                &mut panel,
                current_page,
                &gps,
                &battery_sample,
                pps_delta_us,
                &ntp,
                storage,
                rtc,
                &network,
            );
            if !rendered_once {
                log::trace!("Display diag: first frame rendered");
                rendered_once = true;
            }
            last_draw_us = now_us;
            force_redraw = false;
        }

        FreeRtos::delay_ms(UI_LOOP_DELAY_MS);
    }
}

/// Read monotonic time from the ESP high-resolution timer.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Monotonic timestamp in microseconds since boot.
fn monotonic_us() -> i64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}
