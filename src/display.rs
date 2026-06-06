use anyhow::anyhow;
use core::sync::atomic::{AtomicBool, Ordering};
// Official font docs (embedded-graphics mono/ascii):
// https://docs.rs/embedded-graphics/latest/embedded_graphics/mono_font/ascii/index.html
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use st7789::{BacklightState, Orientation, ST7789};

use crate::battery::BatterySnapshot;
use crate::gps::GpsSnapshot;

pub const DISPLAY_DEBUG_ALWAYS_ON: bool = false;
pub const DISPLAY_BACKLIGHT_ACTIVE_LOW: bool = false;
pub const DISPLAY_X_OFFSET: i32 = 40;
pub const DISPLAY_Y_OFFSET: i32 = 52;
const DISPLAY_WIDTH: u16 = 240;
const DISPLAY_HEIGHT: u16 = 135;
static BOOT_TEST_DRAWN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Copy, Clone)]
pub enum Page {
    Time,
    Location,
    Resources,
    Battery,
}

impl Page {
    pub fn next(self) -> Self {
        match self {
            Self::Time => Self::Location,
            Self::Location => Self::Resources,
            Self::Resources => Self::Battery,
            Self::Battery => Self::Time,
        }
    }
}

pub struct OffsetDisplay<'a, DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin,
    BL: embedded_hal_02::digital::v2::OutputPin,
{
    inner: &'a mut ST7789<DI, RST, BL>,
    x_off: u16,
    y_off: u16,
    width: u16,
    height: u16,
}

impl<'a, DI, RST, BL> OffsetDisplay<'a, DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin,
    BL: embedded_hal_02::digital::v2::OutputPin,
{
    fn new(
        inner: &'a mut ST7789<DI, RST, BL>,
        x_off: u16,
        y_off: u16,
        width: u16,
        height: u16,
    ) -> Self {
        Self {
            inner,
            x_off,
            y_off,
            width,
            height,
        }
    }
}

impl<'a, DI, RST, BL> OriginDimensions for OffsetDisplay<'a, DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin,
    BL: embedded_hal_02::digital::v2::OutputPin,
{
    fn size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }
}

impl<'a, DI, RST, BL, PinE> DrawTarget for OffsetDisplay<'a, DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    BL: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    ST7789<DI, RST, BL>: DrawTarget<Color = Rgb565, Error = st7789::Error<PinE>>,
{
    type Color = Rgb565;
    type Error = st7789::Error<PinE>;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let x_off = self.x_off as i32;
        let y_off = self.y_off as i32;
        let width = self.width as i32;
        let height = self.height as i32;

        let mapped = pixels.into_iter().filter_map(move |Pixel(p, c)| {
            if p.x < 0 || p.y < 0 || p.x >= width || p.y >= height {
                None
            } else {
                Some(Pixel(Point::new(p.x + x_off, p.y + y_off), c))
            }
        });

        self.inner.draw_iter(mapped)
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        let x_off = self.x_off as i32;
        let y_off = self.y_off as i32;
        let width = self.width as i32;
        let height = self.height as i32;

        let pixels = (0..height).flat_map(move |y| {
            (0..width).map(move |x| Pixel(Point::new(x + x_off, y + y_off), color))
        });

        self.inner.draw_iter(pixels)
    }
}

pub fn make_panel<'a, DI, RST, BL, PinE>(
    display: &'a mut ST7789<DI, RST, BL>,
) -> OffsetDisplay<'a, DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    BL: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    ST7789<DI, RST, BL>: DrawTarget<Color = Rgb565, Error = st7789::Error<PinE>>,
{
    let mut panel = OffsetDisplay::new(
        display,
        DISPLAY_X_OFFSET as u16,
        DISPLAY_Y_OFFSET as u16,
        DISPLAY_WIDTH,
        DISPLAY_HEIGHT,
    );

    let should_draw_boot_test = crate::logging::display_boot_test_enabled()
        && !BOOT_TEST_DRAWN.swap(true, Ordering::AcqRel);
    if should_draw_boot_test {
        draw_boot_test(&mut panel);
    }

    panel
}

pub fn backlight_off_state() -> BacklightState {
    if DISPLAY_BACKLIGHT_ACTIVE_LOW {
        BacklightState::On
    } else {
        BacklightState::Off
    }
}

pub fn init_display<DI, RST, BL, PinE>(
    display: &mut ST7789<DI, RST, BL>,
    ets: &mut Ets,
) -> anyhow::Result<BacklightState>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    BL: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    PinE: core::fmt::Debug,
{
    display
        .init(ets)
        .map_err(|e| anyhow!("failed to initialize ST7789 display: {:?}", e))?;
    log::info!("Display: ST7789 initialized");

    display
        .set_orientation(Orientation::LandscapeSwapped)
        .map_err(|e| anyhow!("failed to set display orientation: {:?}", e))?;
    log::info!("Display: orientation set to LandscapeSwapped (CP rot=270)");

    let backlight_on_state = if DISPLAY_BACKLIGHT_ACTIVE_LOW {
        BacklightState::Off
    } else {
        BacklightState::On
    };

    let _ = display.set_backlight(backlight_on_state, ets);
    log::info!(
        "Display: backlight forced on (active_low={})",
        DISPLAY_BACKLIGHT_ACTIVE_LOW
    );

    Ok(backlight_on_state)
}

fn draw_boot_test<D>(panel: &mut D)
where
    D: DrawTarget<Color = Rgb565>,
{
    let _ = Rectangle::new(Point::new(0, 0), Size::new(240, 45))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(panel);
    let _ = Rectangle::new(Point::new(0, 45), Size::new(240, 45))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::GREEN))
        .draw(panel);
    let _ = Rectangle::new(Point::new(0, 90), Size::new(240, 45))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLUE))
        .draw(panel);
    let boot_style = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(Rgb565::WHITE)
        .build();
    let _ = Text::new("Display boot test", Point::new(8, 20), boot_style).draw(panel);
    log::debug!(
        "Display: applying viewport offsets x={} y={}",
        DISPLAY_X_OFFSET,
        DISPLAY_Y_OFFSET
    );
    log::debug!("Display: boot test pattern drawn");
    FreeRtos::delay_ms(800);
}

fn format_human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit_idx = 0usize;
    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{}{}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.1}{}", value, UNITS[unit_idx])
    }
}

fn format_signed_offset_us(offset_us: i64) -> String {
    let abs_us = offset_us.unsigned_abs();
    if abs_us < 1_000 {
        format!("{:+}us", offset_us)
    } else {
        let mut ms = format!("{:+.3}", (offset_us as f64) / 1000.0);
        while ms.contains('.') && ms.ends_with('0') {
            ms.pop();
        }
        if ms.ends_with('.') {
            ms.pop();
        }
        format!("{}ms", ms)
    }
}

pub fn draw_page<D>(
    display: &mut D,
    page: Page,
    gps: &GpsSnapshot,
    battery: &BatterySnapshot,
    pps_delta_us: u32,
) where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(Rgb565::WHITE)
        .build();

    if let Err(err) = display.clear(Rgb565::BLACK) {
        log::warn!("Display: clear failed: {:?}", err);
    }

    let mut y = 20;
    let mut line = |text: String| {
        if let Err(err) = Text::new(&text, Point::new(4, y), style).draw(display) {
            log::trace!("Display diag: text draw failed (\"{}\"): {:?}", text, err);
        }
        y += 21;
    };

    match page {
        Page::Time => {
            line("Page 1/4  TIME".to_owned());
            line(format!("Time:  {}", gps.local_time));
            line(format!("Date:  {}", gps.local_date));
            line(format!("TZ:    {:+}h", gps.tz_offset_hours));
        }
        Page::Location => {
            line("Page 2/4  LOCATION".to_owned());
            line(format!("Lat: {:.6}", gps.lat));
            line(format!("Lon: {:.6}", gps.lon));
            line(format!("Sats: {}", gps.sats));
        }
        Page::Resources => {
            let free_heap = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
            let min_heap = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
            // We don't have a filesystem yet, so show app partition size as storage capacity.
            let storage_part_bytes = unsafe {
                let p = esp_idf_svc::sys::esp_ota_get_running_partition();
                if p.is_null() { 0_u64 } else { (*p).size as u64 }
            };
            line("Page 3/4  RESOURCES".to_owned());
            line(format!(
                "Storage part: {}",
                format_human_bytes(storage_part_bytes)
            ));
            line(format!(
                "Heap free: {}",
                format_human_bytes(free_heap as u64)
            ));
            line(format!("Heap min: {}", format_human_bytes(min_heap as u64)));
        }
        Page::Battery => {
            let pps_label = if pps_delta_us == 0 {
                "PPS: n/a".to_owned()
            } else {
                let offset_us = pps_delta_us as i64 - 1_000_000_i64;
                format!("PPS: {}", format_signed_offset_us(offset_us))
            };
            line("Page 4/4  BATTERY".to_owned());
            line(format!("Voltage: {:.3} V", battery.voltage_v));
            line(format!("Charge:  {:.1} %", battery.percent));
            line(pps_label);
        }
    }
}
