//! TFT display initialization and page rendering helpers.
//!
//! [`DisplayDevice`] owns the shared SPI bus, TFT panel init, and six rotating
//! UI pages with GPS, battery, network, and system-health information.

use anyhow::{Context, anyhow};
use core::sync::atomic::{AtomicBool, Ordering};
// Official font docs (embedded-graphics mono/ascii):
// https://docs.rs/embedded-graphics/latest/embedded_graphics/mono_font/ascii/index.html
use display_interface_spi::SPIInterfaceNoCS;
use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{self, Input, Output, PinDriver};
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::spi::{self, SpiDriver};
use st7789::{BacklightState, Orientation, ST7789};

use crate::battery::BatterySnapshot;
use crate::gps::GpsSnapshot;
use crate::ntp::{DisciplineState, NtpSnapshot};
use crate::pins::PinPool;
use crate::rtc::RtcSnapshot;
use crate::storage::{self, StorageStatus};
use crate::wifi::NetworkSnapshot;

/// Feather SPI clock line shared by TFT and microSD (re-exported from [`storage`]).
pub use storage::{FEATHER_SPI_MISO_PIN, FEATHER_SPI_MOSI_PIN, FEATHER_SPI_SCK_PIN};

/// TFT chip-select on the shared Feather SPI bus.
pub const CS_PIN: i32 = 7;
/// TFT data/command select.
pub const DC_PIN: i32 = 39;
/// TFT hardware reset.
pub const RST_PIN: i32 = 40;
/// TFT backlight PWM/GPIO.
pub const BL_PIN: i32 = 45;
/// TFT power-enable GPIO (active high).
pub const POWER_PIN: i32 = 21;
/// Page-toggle / wake button (active low with pull-up).
pub const BUTTON_PIN: i32 = 0;
/// SPI clock for the TFT device (40 MHz).
pub const SPI_BAUD_MHZ: u32 = 40;

pub const DISPLAY_DEBUG_ALWAYS_ON: bool = false;
pub const DISPLAY_BACKLIGHT_ACTIVE_LOW: bool = false;
pub const DISPLAY_X_OFFSET: i32 = 40;
pub const DISPLAY_Y_OFFSET: i32 = 52;
const DISPLAY_WIDTH: u16 = 240;
const DISPLAY_HEIGHT: u16 = 135;
static BOOT_TEST_DRAWN: AtomicBool = AtomicBool::new(false);

/// Logical panel width passed to ST7789 construction.
pub const PANEL_WIDTH: u16 = DISPLAY_WIDTH;
/// Logical panel height passed to ST7789 construction.
pub const PANEL_HEIGHT: u16 = DISPLAY_HEIGHT;

/// TFT stack: shared SPI bus, panel driver, and page button.
pub struct DisplayDevice {
    driver: &'static SpiDriver<'static>,
    _power: PinDriver<'static, gpio::Gpio21, Output>,
    panel_claimed: bool,
}

impl DisplayDevice {
    const MODULE: &'static str = "display";

    /// Enable TFT power, bring up the shared SPI bus, and deselect TFT CS for SD probing.
    pub fn init<SPI: spi::Spi + spi::SpiAnyPins>(
        pool: &mut PinPool,
        spi_peripheral: impl Peripheral<P = SPI> + 'static,
    ) -> anyhow::Result<Self> {
        let power_gpio = pool
            .take_gpio21(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let sck = pool
            .take_gpio36(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let mosi = pool
            .take_gpio35(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let miso = pool
            .take_gpio37(Self::MODULE)
            .map_err(anyhow::Error::from)?;

        let _power = enable_power(power_gpio)?;
        let driver: &'static SpiDriver<'static> = Box::leak(Box::new(
            SpiDriver::new(
                spi_peripheral,
                sck,
                mosi,
                Some(miso),
                &storage::shared_spi_bus_config(),
            )
            .context("failed to initialize shared SPI bus for TFT and SD card")?,
        ));
        log::info!(
            "Display: shared SPI bus on SCK=GPIO{}, MOSI=GPIO{}, MISO=GPIO{}",
            FEATHER_SPI_SCK_PIN,
            FEATHER_SPI_MOSI_PIN,
            FEATHER_SPI_MISO_PIN
        );
        FreeRtos::delay_ms(50);
        deselect_spi_cs().context("failed to deselect TFT before SD init")?;
        Ok(Self {
            driver,
            _power,
            panel_claimed: false,
        })
    }

    /// Borrow the shared SPI bus for the microSD socket or TFT device driver.
    pub fn spi_driver(&self) -> &'static SpiDriver<'static> {
        self.driver
    }

    /// Wire TFT control pins, construct the ST7789 driver, and run panel init.
    pub fn init_panel(
        &mut self,
        pool: &mut PinPool,
    ) -> anyhow::Result<
        InitializedPanel<
            SPIInterfaceNoCS<
                spi::SpiDeviceDriver<'static, &'static SpiDriver<'static>>,
                PinDriver<'static, gpio::Gpio39, Output>,
            >,
            PinDriver<'static, gpio::Gpio40, Output>,
            PinDriver<'static, gpio::Gpio45, Output>,
        >,
    > {
        let cs = pool.take_gpio7(Self::MODULE).map_err(anyhow::Error::from)?;
        let dc = pool
            .take_gpio39(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let rst = pool
            .take_gpio40(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let bl = pool
            .take_gpio45(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let button_pin = pool.take_gpio0(Self::MODULE).map_err(anyhow::Error::from)?;

        let tft_spi = spi::SpiDeviceDriver::new(
            self.driver,
            Some(cs),
            &spi::config::Config::new().baudrate(SPI_BAUD_MHZ.MHz().into()),
        )
        .context("failed to initialize SPI device for TFT")?;
        let dc = PinDriver::output(dc).context("failed to init TFT DC")?;
        let rst = PinDriver::output(rst).context("failed to init TFT RST")?;
        let backlight = PinDriver::output(bl).context("failed to init TFT backlight")?;
        let button = PinDriver::input(button_pin).context("failed to init page button")?;

        let di = SPIInterfaceNoCS::new(tft_spi, dc);
        let mut display = ST7789::new(di, Some(rst), Some(backlight), PANEL_WIDTH, PANEL_HEIGHT);
        let mut ets = Ets;
        let backlight_on_state = init_display(&mut display, &mut ets)?;
        self.panel_claimed = true;

        Ok(InitializedPanel {
            display,
            button,
            backlight_on_state,
        })
    }

    /// Release all display GPIO claims (SPI bus lines and panel pins when initialized).
    pub fn close(self, pool: &mut PinPool) {
        if self.panel_claimed {
            pool.release(BUTTON_PIN);
            pool.release(CS_PIN);
            pool.release(DC_PIN);
            pool.release(RST_PIN);
            pool.release(BL_PIN);
        }
        pool.release(POWER_PIN);
        pool.release(FEATHER_SPI_SCK_PIN);
        pool.release(FEATHER_SPI_MOSI_PIN);
        pool.release(FEATHER_SPI_MISO_PIN);
    }
}

/// Initialized ST7789 panel, page button, and backlight state for the UI task.
pub struct InitializedPanel<DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin,
    BL: embedded_hal_02::digital::v2::OutputPin,
{
    pub display: ST7789<DI, RST, BL>,
    pub button: PinDriver<'static, gpio::Gpio0, Input>,
    pub backlight_on_state: BacklightState,
}

/// Enable the TFT power rail and wait for the panel to stabilize.
fn enable_power(
    pin: esp_idf_svc::hal::gpio::Gpio21,
) -> anyhow::Result<
    esp_idf_svc::hal::gpio::PinDriver<
        'static,
        esp_idf_svc::hal::gpio::Gpio21,
        esp_idf_svc::hal::gpio::Output,
    >,
> {
    use esp_idf_svc::hal::delay::FreeRtos;
    use esp_idf_svc::hal::gpio::PinDriver;

    let mut power = PinDriver::output(pin).context("failed to init TFT power enable")?;
    power
        .set_high()
        .context("failed to enable TFT power rail")?;
    FreeRtos::delay_ms(10);
    Ok(power)
}

/// Drive the TFT chip-select high before another SPI device uses the shared bus.
fn deselect_spi_cs() -> anyhow::Result<()> {
    use esp_idf_svc::sys::{
        GPIO_MODE_DEF_OUTPUT, gpio_reset_pin, gpio_set_direction, gpio_set_level,
    };

    esp_idf_svc::sys::esp!(unsafe {
        gpio_reset_pin(CS_PIN);
        gpio_set_direction(CS_PIN, GPIO_MODE_DEF_OUTPUT);
        gpio_set_level(CS_PIN, 1)
    })
    .context("failed to deselect TFT SPI CS")
}

/// Application display pages shown by button rotation.
#[derive(Debug, Copy, Clone)]
pub enum Page {
    Time,
    Location,
    Resources,
    Battery,
    Ntp,
    Network,
}

impl Page {
    /// Return the next page in the cyclic page order.
    ///
    /// # Parameters
    /// - `self`: Current page.
    ///
    /// # Returns
    /// - Next page in the six-page rotation.
    pub fn next(self) -> Self {
        match self {
            Self::Time => Self::Location,
            Self::Location => Self::Resources,
            Self::Resources => Self::Battery,
            Self::Battery => Self::Ntp,
            Self::Ntp => Self::Network,
            Self::Network => Self::Time,
        }
    }
}

/// Draw-target wrapper that maps logical coordinates into a fixed panel offset.
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
    /// Build an offset viewport into an underlying ST7789 target.
    ///
    /// # Parameters
    /// - `inner`: Backing ST7789 draw target.
    /// - `x_off`: X-axis pixel offset from panel origin.
    /// - `y_off`: Y-axis pixel offset from panel origin.
    /// - `width`: Logical viewport width.
    /// - `height`: Logical viewport height.
    ///
    /// # Returns
    /// - Configured `OffsetDisplay` wrapper.
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
    /// Report logical viewport size rather than the full backing panel.
    ///
    /// # Parameters
    /// - `self`: Offset viewport instance.
    ///
    /// # Returns
    /// - Logical viewport dimensions as `Size`.
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

    /// Translate incoming pixels by viewport offsets and clip out-of-bounds data.
    ///
    /// # Parameters
    /// - `self`: Offset draw target receiving pixels.
    /// - `pixels`: Pixel iterator in logical viewport coordinates.
    ///
    /// # Returns
    /// - `Ok(())` when drawing succeeds.
    /// - Driver-specific draw error on failure.
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

    /// Clear the logical viewport by drawing into the offset region.
    ///
    /// # Parameters
    /// - `self`: Offset draw target to clear.
    /// - `color`: Fill color for all logical pixels.
    ///
    /// # Returns
    /// - `Ok(())` when drawing succeeds.
    /// - Driver-specific draw error on failure.
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

/// Create the panel viewport and conditionally run the one-time boot test.
///
/// # Parameters
/// - `display`: Mutable ST7789 panel driver.
///
/// # Returns
/// - Offset viewport wrapper used for regular rendering.
pub fn make_panel<'a, DI, RST, BL, PinE>(
    display: &'a mut ST7789<DI, RST, BL>,
) -> OffsetDisplay<'a, DI, RST, BL>
where
    DI: display_interface::WriteOnlyDataCommand,
    RST: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    BL: embedded_hal_02::digital::v2::OutputPin<Error = PinE>,
    ST7789<DI, RST, BL>: DrawTarget<Color = Rgb565, Error = st7789::Error<PinE>>,
    PinE: core::fmt::Debug,
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

/// Return the ST7789 backlight state used to turn the panel off.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Backlight state that corresponds to panel-off behavior.
pub fn backlight_off_state() -> BacklightState {
    if DISPLAY_BACKLIGHT_ACTIVE_LOW {
        BacklightState::On
    } else {
        BacklightState::Off
    }
}

/// Initialize the ST7789 panel orientation and backlight defaults.
///
/// # Parameters
/// - `display`: Mutable ST7789 panel driver.
/// - `ets`: Delay provider required by panel init/backlight APIs.
///
/// # Returns
/// - `Ok(BacklightState)` representing the panel-on state.
/// - `Err` when panel initialization/orientation setup fails.
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

    if let Err(err) = display.set_backlight(backlight_on_state, ets) {
        log::warn!(
            "Display: failed to set backlight state during init: {:?}",
            err
        );
    }
    log::info!(
        "Display: backlight forced on (active_low={})",
        DISPLAY_BACKLIGHT_ACTIVE_LOW
    );

    Ok(backlight_on_state)
}

/// Draw a one-time RGB stripe boot test to validate panel visibility.
///
/// # Parameters
/// - `panel`: Draw target receiving the boot test pattern.
///
/// # Returns
/// - No return value.
fn draw_boot_test<D>(panel: &mut D)
where
    D: DrawTarget<Color = Rgb565>,
    D::Error: core::fmt::Debug,
{
    if let Err(err) = Rectangle::new(Point::new(0, 0), Size::new(240, 45))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(panel)
    {
        log::debug!("Display: boot test red band draw failed: {:?}", err);
    }
    if let Err(err) = Rectangle::new(Point::new(0, 45), Size::new(240, 45))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::GREEN))
        .draw(panel)
    {
        log::debug!("Display: boot test green band draw failed: {:?}", err);
    }
    if let Err(err) = Rectangle::new(Point::new(0, 90), Size::new(240, 45))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLUE))
        .draw(panel)
    {
        log::debug!("Display: boot test blue band draw failed: {:?}", err);
    }
    let boot_style = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(Rgb565::WHITE)
        .build();
    if let Err(err) = Text::new("Display boot test", Point::new(8, 20), boot_style).draw(panel) {
        log::debug!("Display: boot test text draw failed: {:?}", err);
    }
    log::debug!(
        "Display: applying viewport offsets x={} y={}",
        DISPLAY_X_OFFSET,
        DISPLAY_Y_OFFSET
    );
    log::debug!("Display: boot test pattern drawn");
    FreeRtos::delay_ms(800);
}

/// Format bytes using a compact IEC-like unit suffix.
///
/// # Parameters
/// - `bytes`: Raw byte count.
///
/// # Returns
/// - Human-readable storage string (for example `512B`, `2.0K`, `1.5M`).
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

/// Format a signed PPS offset with `us`/`ms` auto-scaling.
///
/// # Parameters
/// - `offset_us`: Signed offset in microseconds.
///
/// # Returns
/// - Formatted string with sign and either `us` or `ms` units.
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

/// Format a PPS jitter or dispersion value with `us`/`ms` auto-scaling (unsigned).
///
/// # Parameters
/// - `value_us`: Unsigned value in microseconds.
///
/// # Returns
/// - Formatted string with either `us` or `ms` units.
fn format_unsigned_us(value_us: u32) -> String {
    if value_us < 1_000 {
        format!("{}us", value_us)
    } else {
        let ms = value_us as f64 / 1_000.0;
        // Strip trailing zeros after decimal.
        let mut s = format!("{:.3}", ms);
        while s.contains('.') && s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        format!("{}ms", s)
    }
}

/// True when GPS time/location pages should show RTC fallback instead of live GPS.
fn gps_use_rtc_fallback(gps: &GpsSnapshot) -> bool {
    !gps.fix || gps.sats == 0
}

/// Resolve date/time strings for display, preferring RTC when GPS is unavailable.
fn display_time_source(gps: &GpsSnapshot, rtc: RtcSnapshot) -> (String, String, bool) {
    if gps_use_rtc_fallback(gps) {
        if let Some(utc) = rtc.utc_unix_seconds {
            if let Some((date, time)) = crate::gps::local_from_utc_unix(utc) {
                return (time, date, true);
            }
            let offset = crate::gps::tz_offset_hours_at_unix(utc).max(gps.tz_offset_hours);
            if let Some((date, time)) = crate::rtc::local_date_time_from_utc(utc, offset) {
                return (time, date, true);
            }
        }
        ("n/a".to_owned(), "n/a".to_owned(), true)
    } else {
        (gps.local_time.clone(), gps.local_date.clone(), false)
    }
}

/// Format the last PPS interval as a signed offset from 1 s.
fn pps_offset_label(pps_delta_us: u32) -> String {
    if pps_delta_us == 0 {
        "PPS: n/a".to_owned()
    } else {
        let offset_us = pps_delta_us as i64 - 1_000_000_i64;
        format!("PPS: {}", format_signed_offset_us(offset_us))
    }
}

/// Local RTC date/time strings for display, or `n/a` when absent or unset.
fn rtc_local_strings(rtc: RtcSnapshot, tz_offset_hours: i8) -> (String, String) {
    if !rtc.detected {
        return ("n/a".to_owned(), "n/a".to_owned());
    }
    if let Some(utc) = rtc.utc_unix_seconds {
        if let Some((date, time)) = crate::gps::local_from_utc_unix(utc) {
            return (time, date);
        }
        let offset = crate::gps::tz_offset_hours_at_unix(utc).max(tz_offset_hours);
        if let Some((date, time)) = crate::rtc::local_date_time_from_utc(utc, offset) {
            return (time, date);
        }
    }
    ("n/a".to_owned(), "n/a".to_owned())
}

/// Draw the selected UI page onto the display target.
///
/// # Parameters
/// - `display`: Draw target for rendering text and backgrounds.
/// - `page`: Selected page to render.
/// - `gps`: Latest GPS snapshot.
/// - `battery`: Latest battery snapshot.
/// - `pps_delta_us`: Last observed PPS interval delta in microseconds.
/// - `ntp`: Latest NTP discipline snapshot.
/// - `storage`: MicroSD mount status, when available.
/// - `rtc`: Latest RTC sample for GPS-loss fallback display.
/// - `network`: Latest STA IP, gateway, and hostname snapshot.
///
/// # Returns
/// - No return value; draw errors are logged.
pub fn draw_page<D>(
    display: &mut D,
    page: Page,
    gps: &GpsSnapshot,
    battery: &BatterySnapshot,
    pps_delta_us: u32,
    ntp: &NtpSnapshot,
    storage: StorageStatus,
    rtc: RtcSnapshot,
    network: &NetworkSnapshot,
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
            let (time, date, rtc_fallback) = display_time_source(gps, rtc);
            line("Page 1/6  TIME".to_owned());
            if rtc_fallback {
                line("RTC fallback".to_owned());
            }
            line(format!("Time:  {}", time));
            line(format!("Date:  {}", date));
            line(format!("TZ:    {:+}h", gps.tz_offset_hours));
            line(pps_offset_label(pps_delta_us));
        }
        Page::Location => {
            line("Page 2/6  LOCATION".to_owned());
            if gps_use_rtc_fallback(gps) {
                let (time, date, _) = display_time_source(gps, rtc);
                line("RTC fallback".to_owned());
                line(format!("Time:  {}", time));
                line(format!("Date:  {}", date));
            } else {
                line(format!("Lat: {:.6}", gps.lat));
                line(format!("Lon: {:.6}", gps.lon));
                match gps.altitude_m {
                    Some(alt) => line(format!("Alt: {:.1} m", alt)),
                    None => line("Alt: n/a".to_owned()),
                }
                line(format!("Sats: {}", gps.sats));
            }
        }
        Page::Resources => {
            let free_heap = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
            let min_heap = unsafe { esp_idf_svc::sys::esp_get_minimum_free_heap_size() };
            line("Page 3/6  RESOURCES".to_owned());
            if storage.mounted {
                line(format!(
                    "SD free: {}",
                    format_human_bytes(storage.free_bytes)
                ));
                line(format!(
                    "SD total: {}",
                    format_human_bytes(storage.total_bytes)
                ));
            } else {
                let storage_part_bytes = unsafe {
                    let p = esp_idf_svc::sys::esp_ota_get_running_partition();
                    if p.is_null() { 0_u64 } else { (*p).size as u64 }
                };
                line(format!("SD: not mounted",));
                line(format!(
                    "App part: {}",
                    format_human_bytes(storage_part_bytes)
                ));
            }
            line(format!(
                "Heap free: {}",
                format_human_bytes(free_heap as u64)
            ));
            line(format!("Heap min: {}", format_human_bytes(min_heap as u64)));
        }
        Page::Battery => {
            let (rtc_time, rtc_date) = rtc_local_strings(rtc, gps.tz_offset_hours);
            line("Page 4/6  BATT & RTC".to_owned());
            line(format!("Voltage: {:.3} V", battery.voltage_v));
            line(format!("Charge:  {:.1} %", battery.percent));
            line(format!("Time:  {}", rtc_time));
            line(format!("Date:  {}", rtc_date));
        }
        Page::Ntp => {
            let state_str = match ntp.state {
                DisciplineState::Locked => "LOCKED",
                DisciplineState::Holdover => "HOLDOVER",
                DisciplineState::Unsync => "UNSYNC",
            };
            let jitter_us = ntp.pps_jitter_us.round() as u32;
            line("Page 5/6  NTP".to_owned());
            line(format!("Str:{} {}", ntp.stratum, state_str));
            line(format!("Freq:{:+.3} ppm", ntp.freq_ppm));
            line(format!(
                "Jit:{}  disp:{:.1}ms",
                format_unsigned_us(jitter_us),
                ntp.root_disp_ms
            ));
            let proc_label = if ntp.proc_delay_has_sample {
                format_unsigned_us(ntp.proc_delay_us.round().max(1.0) as u32)
            } else {
                "n/a".to_owned()
            };
            line(format!(
                "Proc:{}  srv:{} ko:{}",
                proc_label, ntp.served, ntp.rate_limited
            ));
        }
        Page::Network => {
            line("Page 6/6  NETWORK".to_owned());
            line(format!("IP:  {}", network.ip));
            line(format!("GW:  {}", network.gateway));
            line(format!("Host: {}", network.hostname));
            line(format!("SSID: {}", network.ssid));
        }
    }
}
