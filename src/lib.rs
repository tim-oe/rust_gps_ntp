//! Host-testable library surface and ESP-IDF firmware modules.
//!
//! Core logic lives here; [`board::BoardBoot`] brings up peripherals and
//! [`app`] runs the main service loop. Display work runs on [`ui_task`].
//! Host builds exercise pure modules such as [`gps`], [`ntp`], [`pps`],
//! [`timezone`], and [`battery`].

pub mod battery;
pub mod gps;
pub mod ntp;
pub mod pps;
pub mod rtc;
pub mod timezone;

#[cfg(target_os = "espidf")]
pub mod app;
#[cfg(target_os = "espidf")]
pub mod board;
#[cfg(target_os = "espidf")]
pub mod display;
#[cfg(target_os = "espidf")]
pub mod i2c_bus;
#[cfg(target_os = "espidf")]
pub mod logging;
#[cfg(target_os = "espidf")]
pub mod pins;
#[cfg(target_os = "espidf")]
pub mod storage;
#[cfg(target_os = "espidf")]
pub mod ui_task;
#[cfg(target_os = "espidf")]
pub mod wifi;
