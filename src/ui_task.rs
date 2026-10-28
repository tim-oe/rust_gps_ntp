//! Display, button, and battery sampling on dedicated FreeRTOS tasks.
//!
//! The button task only reads GPIO and drives the backlight. SPI drawing and
//! I2C run on a lower-priority panel task. SD filesystem calls run on `sd_io`,
//! below a watchdog that restarts the chip when one does not return. The TFT
//! driver busy-waits inside `spi_device_polling_transmit` with no timeout, so
//! drawing must not share a task with the button. If the panel task stops
//! looping, the button task restarts the chip even while the backlight is off.

use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use anyhow::Context;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{self, Input, InterruptType, Output, PinDriver, Pull};
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use portable_atomic::{AtomicI64, AtomicU32};
use st7789::ST7789;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::battery::{self, BatteryDevice, BatterySnapshot};
use crate::display::{self, Page};
use crate::gps::GpsSnapshot;
use crate::i2c_bus::FeatherI2cBus;
use crate::ntp::NtpSnapshot;
use crate::rtc::{self, RtcSnapshot};
use crate::storage::{self, StorageStatus};
use crate::wifi;

const BUTTON_TASK_STACK_BYTES: usize = 8_192;
const PANEL_TASK_STACK_BYTES: usize = 16_384;
/// Above the panel task and the SD writer, below the main NTP loop (10).
const BUTTON_TASK_PRIORITY: u8 = 6;
/// Below the button task so a polling SPI transfer cannot starve presses.
const PANEL_TASK_PRIORITY: u8 = 4;
const BATTERY_SAMPLE_US: i64 = 5_000_000;
const RTC_SAMPLE_US: i64 = 1_000_000;
const DRAW_INTERVAL_US: i64 = 5_000_000;
const SCREEN_BLANK_US: i64 = 15_000_000;
const BUTTON_DEBOUNCE_US: i64 = 200_000;
const PULLUP_REFRESH_US: i64 = 1_000_000;
/// The panel task must lap its loop within this time. A stuck SD call is
/// restarted by `sd_wd` and does not share this task. Blanking the backlight
/// must not cancel the deadline: a draw that hangs as the screen turns off
/// used to be forgotten, so the last frame (including a stale charge reading)
/// stayed up until power was pulled.
const PANEL_LOOP_STALL_US: i64 = 5_000_000;
/// Re-apply GPIO0's input mode after this long held low. A USB charger can
/// make the Serial/JTAG PHY clear the pin mux, so the boot button stays low
/// and later presses never register until VBUS drops.
const BUTTON_HELD_RECOVER_US: i64 = 500_000;
const STUCK_LOW_US: i64 = 2_000_000;
const BUTTON_LOOP_DELAY_MS: u32 = 10;
const PANEL_LOOP_DELAY_MS: u32 = 20;

/// Falling edges on the page button. The ISR only bumps this counter.
static BUTTON_EDGES: AtomicU32 = AtomicU32::new(0);

/// Last time the panel task reached the top of its loop.
static PANEL_LOOP_US: AtomicI64 = AtomicI64::new(0);

/// Shared page / backlight / redraw generation between the button and panel tasks.
struct UiControl {
    page: AtomicU8,
    screen_on: AtomicBool,
    render_req: AtomicU32,
}

/// Shared GPS, PPS, NTP discipline, RTC, and storage samples written by tasks.
pub struct UiFeed {
    gps: RwLock<GpsSnapshot>,
    pps_delta_us: AtomicU32,
    ntp: RwLock<NtpSnapshot>,
    rtc: RwLock<RtcSnapshot>,
    storage: RwLock<StorageStatus>,
    battery: RwLock<BatterySnapshot>,
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
            battery: RwLock::new(BatterySnapshot::default()),
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

    /// Latest battery sample, or the default when the lock is poisoned.
    pub fn battery(&self) -> BatterySnapshot {
        self.battery.read().map(|guard| *guard).unwrap_or_default()
    }

    /// Publish a battery sample from the panel task.
    pub fn publish_battery(&self, sample: BatterySnapshot) {
        if let Ok(mut guard) = self.battery.write() {
            *guard = sample;
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

/// Join handles that keep the button and panel threads alive for the firmware lifetime.
pub struct UiTaskHandle {
    _panel: JoinHandle<()>,
    _button: JoinHandle<()>,
}

impl UiTaskHandle {
    /// Spawn the button and panel tasks.
    ///
    /// The button task owns the GPIO and the backlight. The panel task owns
    /// the ST7789, the I2C bus, and SD free-space refresh. See the module
    /// docs for why those must not share a task.
    ///
    /// # Parameters
    /// - `feed`: Shared feed updated by the main service loop and the panel task.
    /// - `display`: Initialized ST7789 panel driver (moved into the panel task).
    /// - `button`: Page/wake button on `GPIO0` (moved into the button task).
    /// - `backlight`: TFT backlight GPIO (moved into the button task).
    /// - `i2c_bus`: Shared Feather I2C bus for battery and RTC access (moved into the panel task).
    /// - `battery`: Detected fuel gauge, if any, for periodic sampling.
    /// - `rtc`: Boot-time RTC probe result.
    /// - `network`: Shared Wi-Fi handle for reading the STA address on the Network page.
    ///
    /// # Returns
    /// - `Ok(UiTaskHandle)` when both threads start.
    /// - `Err` when interrupt setup or thread spawn fails.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn<DI, RST, BL, PinE>(
        feed: Arc<UiFeed>,
        mut display: ST7789<DI, RST, BL>,
        mut button: PinDriver<'static, gpio::Gpio0, Input>,
        mut backlight: PinDriver<'static, gpio::Gpio45, Output>,
        mut i2c_bus: FeatherI2cBus,
        battery: BatteryDevice,
        rtc: rtc::RtcDevice,
        network: wifi::SharedWifi,
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
        // A wall charger is not a USB host. The Serial/JTAG PHY can still see
        // D+/D- shorted and reset the IO mux, which drops GPIO0's input mode
        // so the page button reads as held until the cable is unplugged.
        disarm_usb_iomux_reset();
        arm_button_interrupt(&mut button)?;
        PANEL_LOOP_US.store(monotonic_us(), Ordering::Relaxed);

        let control = Arc::new(UiControl {
            page: AtomicU8::new(Page::Time.index()),
            screen_on: AtomicBool::new(true),
            render_req: AtomicU32::new(1),
        });
        let panel_feed = Arc::clone(&feed);
        let panel_control = Arc::clone(&control);

        // Panel first, at the lower priority, so its first frame cannot
        // preempt the button task once that task exists.
        let panel = spawn_with_priority(
            PANEL_TASK_PRIORITY,
            "ui_panel",
            PANEL_TASK_STACK_BYTES,
            move || {
                panel_task_main(
                    panel_feed,
                    &mut display,
                    &mut i2c_bus,
                    battery,
                    rtc,
                    &network,
                    &panel_control,
                );
            },
        )?;
        let button_thread = spawn_with_priority(
            BUTTON_TASK_PRIORITY,
            "ui_button",
            BUTTON_TASK_STACK_BYTES,
            move || {
                button_task_main(&mut button, &mut backlight, &control);
            },
        )?;

        log::info!(
            "Display: button task priority {BUTTON_TASK_PRIORITY}, panel task priority {PANEL_TASK_PRIORITY}"
        );
        Ok(Self {
            _panel: panel,
            _button: button_thread,
        })
    }
}

/// Spawn `body` at `priority`, then restore the default spawn configuration.
fn spawn_with_priority(
    priority: u8,
    name: &str,
    stack: usize,
    body: impl FnOnce() + Send + 'static,
) -> anyhow::Result<JoinHandle<()>> {
    ThreadSpawnConfiguration {
        priority,
        ..Default::default()
    }
    .set()
    .with_context(|| format!("failed to configure {name} priority"))?;

    let spawned = thread::Builder::new()
        .name(name.into())
        .stack_size(stack)
        .spawn(body);

    ThreadSpawnConfiguration::default()
        .set()
        .context("failed to restore thread spawn configuration")?;

    spawned.with_context(|| format!("failed to spawn {name}"))
}

/// Latch falling edges on GPIO0. The handler is re-armed from the button task.
fn arm_button_interrupt(button: &mut PinDriver<'static, gpio::Gpio0, Input>) -> anyhow::Result<()> {
    let on_edge = || {
        BUTTON_EDGES.fetch_add(1, Ordering::Relaxed);
    };
    // SAFETY: the closure only increments a static atomic, which is ISR-safe,
    // and it borrows nothing. The `PinDriver` is moved into the button task
    // and held for the process lifetime, so this subscription is never dropped.
    unsafe { button.subscribe_nonstatic(on_edge) }.context("failed to subscribe button ISR")?;
    button
        .set_interrupt_type(InterruptType::NegEdge)
        .context("failed to set button interrupt type")?;
    button
        .enable_interrupt()
        .context("failed to enable button interrupt")?;
    Ok(())
}

/// Button task: edges, backlight, and blanking. Never touches SPI or I2C.
fn button_task_main(
    button: &mut PinDriver<'static, gpio::Gpio0, Input>,
    backlight: &mut PinDriver<'static, gpio::Gpio45, Output>,
    control: &UiControl,
) {
    let mut last_edges = BUTTON_EDGES.load(Ordering::Relaxed);
    let mut last_level_pressed = false;
    let mut last_action_us = 0_i64;
    let mut last_interaction_us = monotonic_us();
    let mut last_redraw_req_us = last_interaction_us;
    let mut last_pullup_us = last_interaction_us;
    let mut low_since = 0_i64;
    let mut stuck_logged = false;
    let mut last_recover_us = 0_i64;
    let mut restart_blocked_logged = false;

    loop {
        let now_us = monotonic_us();

        let edges = BUTTON_EDGES.load(Ordering::Relaxed);
        let edge_press = edges != last_edges;
        if edge_press {
            last_edges = edges;
            if let Err(err) = button.enable_interrupt() {
                log::warn!("Display: failed to re-arm button interrupt: {err}");
            }
        }

        let level_pressed = !button.is_high();
        let level_press = level_pressed && !last_level_pressed;
        last_level_pressed = level_pressed;

        if level_pressed {
            if low_since == 0 {
                low_since = now_us;
            } else if now_us.saturating_sub(low_since) >= BUTTON_HELD_RECOVER_US
                && now_us.saturating_sub(last_recover_us) >= PULLUP_REFRESH_US
            {
                // The pin should already be an input with a pull-up. Doing it
                // again repairs a USB PHY IO-mux reset without waiting for the
                // cable to be unplugged.
                restore_button_input(button);
                // Reconfiguring the pin can post a stale edge. Consume it so
                // the repair itself does not turn the page.
                last_edges = BUTTON_EDGES.load(Ordering::Relaxed);
                last_recover_us = now_us;
            }
            if !stuck_logged && now_us.saturating_sub(low_since) >= STUCK_LOW_US {
                log::warn!(
                    "Display: button line held low; waiting for release before the next press"
                );
                stuck_logged = true;
            }
        } else {
            low_since = 0;
            stuck_logged = false;
        }

        if (edge_press || level_press)
            && now_us.saturating_sub(last_action_us) >= BUTTON_DEBOUNCE_US
        {
            last_action_us = now_us;
            last_interaction_us = now_us;
            last_redraw_req_us = now_us;
            if !control.screen_on.load(Ordering::Relaxed) {
                control.screen_on.store(true, Ordering::Relaxed);
                if let Err(err) = display::set_backlight_pin(backlight, true) {
                    log::warn!("Display: failed to turn backlight on: {err}");
                }
            } else {
                let page = Page::from_index(control.page.load(Ordering::Relaxed)).next();
                control.page.store(page.index(), Ordering::Relaxed);
                log::debug!("Display: page {}", page.index() + 1);
            }
            control.render_req.fetch_add(1, Ordering::Relaxed);
        }

        let screen_on = control.screen_on.load(Ordering::Relaxed);
        if !display::DISPLAY_DEBUG_ALWAYS_ON
            && screen_on
            && now_us.saturating_sub(last_interaction_us) >= SCREEN_BLANK_US
        {
            control.screen_on.store(false, Ordering::Relaxed);
            if let Err(err) = display::set_backlight_pin(backlight, false) {
                log::warn!("Display: failed to turn backlight off: {err}");
            }
        }

        if control.screen_on.load(Ordering::Relaxed)
            && now_us.saturating_sub(last_redraw_req_us) >= DRAW_INTERVAL_US
        {
            control.render_req.fetch_add(1, Ordering::Relaxed);
            last_redraw_req_us = now_us;
        }

        if now_us.saturating_sub(last_pullup_us) >= PULLUP_REFRESH_US {
            if let Err(err) = button.set_pull(Pull::Up) {
                log::warn!("Display: failed to refresh button pull-up: {err}");
            }
            last_pullup_us = now_us;
        }

        let panel_at = PANEL_LOOP_US.load(Ordering::Relaxed);
        if panel_at != 0 && now_us.saturating_sub(panel_at) >= PANEL_LOOP_STALL_US {
            // GPIO0 low at reset selects the ROM download bootloader. A panel
            // stall that coincides with a held (or USB-stuck) button must wait
            // until the line is high, or the restart would not come back.
            if button.is_high() {
                restart_stalled_panel();
            } else if !restart_blocked_logged {
                log::error!(
                    "Display: panel task stalled while GPIO0 is low; holding restart so the chip does not enter download mode"
                );
                restart_blocked_logged = true;
            }
        } else {
            restart_blocked_logged = false;
        }

        FreeRtos::delay_ms(BUTTON_LOOP_DELAY_MS);
    }
}

/// Panel task: I2C sensors, SD free-space, and SPI drawing.
#[allow(clippy::too_many_arguments)]
fn panel_task_main<DI, RST, BL, PinE>(
    feed: Arc<UiFeed>,
    display: &mut ST7789<DI, RST, BL>,
    i2c_bus: &mut FeatherI2cBus,
    battery: BatteryDevice,
    rtc: rtc::RtcDevice,
    network: &wifi::SharedWifi,
    control: &UiControl,
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
    let mut last_battery_us = 0_i64;
    let mut last_rtc_us = 0_i64;
    let mut last_storage_us = 0_i64;
    let mut last_drawn = 0_u32;
    let mut rendered_once = false;

    loop {
        let now_us = monotonic_us();
        PANEL_LOOP_US.store(now_us, Ordering::Relaxed);
        let req = control.render_req.load(Ordering::Relaxed);
        if control.screen_on.load(Ordering::Relaxed) && req != last_drawn {
            let page = Page::from_index(control.page.load(Ordering::Relaxed));
            let (gps, pps_delta_us, ntp, storage, rtc_snap) = feed.snapshot();
            let battery_sample = feed.battery();
            let network_snap = wifi::read_network_snapshot(network);
            let mut panel = display::make_panel(display);
            display::draw_page(
                &mut panel,
                page,
                &gps,
                &battery_sample,
                pps_delta_us,
                &ntp,
                storage,
                rtc_snap,
                &network_snap,
            );
            if !rendered_once {
                log::trace!("Display diag: first frame rendered");
                rendered_once = true;
            }
            // A press during the draw changes the page after we sampled it.
            // Leave `last_drawn` behind so the next loop paints the new page.
            let page_now = Page::from_index(control.page.load(Ordering::Relaxed));
            let req_now = control.render_req.load(Ordering::Relaxed);
            if page_now == page && req_now == req {
                last_drawn = req;
            }
        }

        // A press that arrived during a sensor read is picked up on the next
        // loop. Free-space reads run on the SD worker; the panel only posts
        // the request and copies a finished sample.
        let frame_pending = control.screen_on.load(Ordering::Relaxed)
            && control.render_req.load(Ordering::Relaxed) != last_drawn;
        if !frame_pending {
            sample_sensors(
                &feed,
                i2c_bus,
                battery,
                rtc,
                now_us,
                &mut last_battery_us,
                &mut last_rtc_us,
            );
            if feed.storage().mounted
                && (last_storage_us == 0
                    || now_us.saturating_sub(last_storage_us) >= storage::STATUS_REFRESH_US)
            {
                storage::request_usage_refresh();
                last_storage_us = now_us;
            }
        }
        if let Some(status) = storage::take_usage() {
            feed.publish_storage(status);
        }

        FreeRtos::delay_ms(PANEL_LOOP_DELAY_MS);
    }
}

/// Battery sample and RTC read/write. Failures are logged and retried next pass.
fn sample_sensors(
    feed: &UiFeed,
    i2c_bus: &mut FeatherI2cBus,
    battery: BatteryDevice,
    rtc: rtc::RtcDevice,
    now_us: i64,
    last_battery_us: &mut i64,
    last_rtc_us: &mut i64,
) {
    if *last_battery_us == 0 || now_us.saturating_sub(*last_battery_us) >= BATTERY_SAMPLE_US {
        if let Some(kind) = battery.monitor {
            match battery::read_battery(i2c_bus, kind) {
                Ok(reading) => feed.publish_battery(reading),
                Err(err) => log::debug!("Battery: read failed: {}", err),
            }
        }
        *last_battery_us = now_us;
    }

    if !rtc.present {
        return;
    }

    let pending_write = feed.rtc_write_pending.swap(0, Ordering::Relaxed);
    if pending_write > 0 {
        match rtc::write_unix_seconds(i2c_bus, pending_write) {
            Ok(()) => log::debug!("RTC: wrote UTC {pending_write} from discipline"),
            Err(err) => log::warn!("RTC: write failed: {err}"),
        }
    }

    if *last_rtc_us != 0 && now_us.saturating_sub(*last_rtc_us) < RTC_SAMPLE_US {
        return;
    }
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
    // `try_write` so a reader on the main task cannot park this task. The
    // sample is retried on the next loop when the lock is busy.
    if let Ok(mut guard) = feed.rtc.try_write() {
        *guard = snap;
        *last_rtc_us = now_us;
    }
}

/// Stop a USB Serial/JTAG connect from resetting the IO mux.
///
/// A dedicated charger shorts D+ and D-. The PHY can treat that as a download
/// event and return GPIO0 (and the other app pins) to their reset state while
/// the firmware is running. Bit 18 of `RTC_CNTL_USB_CONF_REG` is the ESP32-S3
/// control that ignores that mux reset. USB serial itself keeps working, so a
/// host can still open the console.
fn disarm_usb_iomux_reset() {
    const RTC_CNTL_USB_CONF_REG: *mut u32 = 0x6000_8120 as *mut u32;
    const IO_MUX_RESET_DISABLE: u32 = 1 << 18;
    // SAFETY: `0x60008120` is `RTC_CNTL_USB_CONF_REG` on ESP32-S3
    // (`DR_REG_RTCCNTL_BASE + 0x120`). One aligned volatile read of that
    // memory-mapped register.
    let current = unsafe { RTC_CNTL_USB_CONF_REG.read_volatile() };
    // SAFETY: same register. OR-ing bit 18 only disables the USB IO-mux reset.
    unsafe { RTC_CNTL_USB_CONF_REG.write_volatile(current | IO_MUX_RESET_DISABLE) };
}

/// Put GPIO0 back into pulled-up input mode and re-arm its falling-edge ISR.
fn restore_button_input(button: &mut PinDriver<'static, gpio::Gpio0, Input>) {
    if let Err(err) = button.set_pull(Pull::Up) {
        log::warn!("Display: failed to refresh button pull-up: {err}");
    }
    // SAFETY: this task owns GPIO0 for the process lifetime. Selecting input
    // mode again undoes a USB IO-mux reset without releasing the driver.
    let rc = unsafe {
        esp_idf_svc::sys::gpio_set_direction(
            esp_idf_svc::sys::gpio_num_t_GPIO_NUM_0,
            esp_idf_svc::sys::gpio_mode_t_GPIO_MODE_INPUT,
        )
    };
    if rc != esp_idf_svc::sys::ESP_OK {
        log::warn!("Display: failed to restore button input mode: {rc}");
    }
    if let Err(err) = button.set_interrupt_type(InterruptType::NegEdge) {
        log::warn!("Display: failed to restore button edge type: {err}");
    }
    if let Err(err) = button.enable_interrupt() {
        log::warn!("Display: failed to re-arm button interrupt: {err}");
    }
}

/// The panel task stopped looping. SPI polling waits forever when the
/// peripheral never sets its done bit, so the only recovery is a restart.
fn restart_stalled_panel() -> ! {
    log::error!("Display: panel task stopped responding; restarting");
    // SAFETY: `esp_restart` takes no arguments and does not return. Calling it
    // from a task is the supported way to reset after the SPI driver has
    // busy-waited past any useful timeout.
    unsafe { esp_idf_svc::sys::esp_restart() }
}

/// Read monotonic time from the ESP high-resolution timer.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Monotonic timestamp in microseconds since boot.
fn monotonic_us() -> i64 {
    // SAFETY: plain FFI read of the 64-bit high-resolution timer; no
    // preconditions and safe from any task context.
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}
