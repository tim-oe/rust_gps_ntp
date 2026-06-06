//! Display, button, and battery sampling on a dedicated FreeRTOS task.
//!
//! SPI drawing and I2C battery reads run here so the main service loop can
//! focus on GPS ingest, PPS discipline, and NTP polling.

use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use anyhow::Context;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{self, Input, PinDriver, Pull};
use esp_idf_svc::hal::i2c;
use portable_atomic::AtomicU32;
use st7789::{BacklightState, ST7789};
use std::sync::atomic::Ordering;

use crate::battery::{self, BatteryMonitor, BatterySnapshot};
use crate::display::{self, Page};
use crate::gps::GpsSnapshot;

const UI_TASK_STACK_BYTES: usize = 16_384;
const BATTERY_SAMPLE_US: i64 = 5_000_000;
const DRAW_INTERVAL_US: i64 = 5_000_000;
const SCREEN_BLANK_US: i64 = 15_000_000;
const UI_LOOP_DELAY_MS: u32 = 10;

/// GPS and PPS values produced by the main loop for UI rendering.
pub struct UiFeed {
    gps: RwLock<GpsSnapshot>,
    pps_delta_us: AtomicU32,
}

impl UiFeed {
    /// Create feed state with empty GPS and zero PPS delta.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            gps: RwLock::new(GpsSnapshot::default()),
            pps_delta_us: AtomicU32::new(0),
        })
    }

    /// Publish the latest GPS snapshot for the UI task.
    pub fn publish_gps(&self, gps: &GpsSnapshot) {
        if let Ok(mut guard) = self.gps.write() {
            *guard = gps.clone();
        }
    }

    /// Publish the latest PPS interval delta in microseconds.
    pub fn publish_pps_delta(&self, delta_us: u32) {
        self.pps_delta_us.store(delta_us, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (GpsSnapshot, u32) {
        let gps = self
            .gps
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let pps_delta_us = self.pps_delta_us.load(Ordering::Relaxed);
        (gps, pps_delta_us)
    }
}

/// Handle keeping the UI worker thread alive for the lifetime of the firmware.
pub struct UiTaskHandle {
    _thread: JoinHandle<()>,
}

impl UiTaskHandle {
    /// Spawn the UI task with exclusive ownership of display and button hardware.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn<DI, RST, BL, PinE>(
        feed: Arc<UiFeed>,
        mut display: ST7789<DI, RST, BL>,
        mut button: PinDriver<'static, gpio::Gpio0, Input>,
        mut i2c_drv: i2c::I2cDriver<'static>,
        battery_monitor: Option<BatteryMonitor>,
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

        let thread = thread::Builder::new()
            .name("ui_task".into())
            .stack_size(UI_TASK_STACK_BYTES)
            .spawn(move || {
                ui_task_main(
                    feed,
                    &mut display,
                    &mut button,
                    &mut i2c_drv,
                    battery_monitor,
                    backlight_on_state,
                );
            })
            .context("failed to spawn UI task")?;

        log::info!(
            "Display: UI task started (stack {} bytes)",
            UI_TASK_STACK_BYTES
        );
        Ok(Self { _thread: thread })
    }
}

fn ui_task_main<DI, RST, BL, PinE>(
    feed: Arc<UiFeed>,
    display: &mut ST7789<DI, RST, BL>,
    button: &mut PinDriver<'static, gpio::Gpio0, Input>,
    i2c_drv: &mut i2c::I2cDriver<'static>,
    battery_monitor: Option<BatteryMonitor>,
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
    let mut battery = BatterySnapshot::default();
    let mut last_battery_us = 0_i64;
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
            if let Some(kind) = battery_monitor {
                match battery::read_battery(i2c_drv, kind) {
                    Ok(reading) => battery = reading,
                    Err(err) => log::debug!("Battery: read failed: {}", err),
                }
            }
            last_battery_us = now_us;
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
            let (gps, pps_delta_us) = feed.snapshot();
            let mut panel = display::make_panel(display);
            display::draw_page(&mut panel, current_page, &gps, &battery, pps_delta_us);
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

fn monotonic_us() -> i64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}
