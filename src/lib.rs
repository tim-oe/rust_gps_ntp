//! Host-testable library surface and ESP-IDF firmware modules.
//!
//! Core logic lives here; [`app`] wires peripherals and spawns [`ui_task`] on
//! ESP-IDF targets while host builds exercise pure modules such as [`gps`],
//! [`ntp`], [`pps`], [`timezone`], and [`battery`].

pub mod battery;
pub mod gps;
pub mod ntp;
pub mod pps;
pub mod rtc;
pub mod timezone;

#[cfg(target_os = "espidf")]
pub mod app;
#[cfg(target_os = "espidf")]
pub mod display;
#[cfg(target_os = "espidf")]
pub mod logging;
#[cfg(target_os = "espidf")]
pub mod storage;
#[cfg(target_os = "espidf")]
pub mod ui_task;
#[cfg(target_os = "espidf")]
pub mod wifi;
