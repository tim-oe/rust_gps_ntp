//! Host-testable library surface and ESP-IDF firmware modules.

pub mod battery;
pub mod gps;
pub mod ntp;
pub mod pps;
pub mod timezone;

#[cfg(target_os = "espidf")]
pub mod app;
#[cfg(target_os = "espidf")]
pub mod display;
#[cfg(target_os = "espidf")]
pub mod logging;
#[cfg(target_os = "espidf")]
pub mod ui_task;
#[cfg(target_os = "espidf")]
pub mod wifi;
