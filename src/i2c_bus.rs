//! Feather I2C bus shared by battery fuel gauges and the Adalogger RTC.
//!
//! Device modules implement [`I2cDevice`] and receive a [`FeatherI2cBus`] at init.

use anyhow::Context;
use esp_idf_svc::hal::gpio;
use esp_idf_svc::hal::i2c::{self, I2c, I2cDriver};
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::prelude::*;

/// Feather I2C data line (battery gauge + PCF8523 RTC on Adalogger wing).
pub const SDA_PIN: i32 = 42;
/// Feather I2C clock line.
pub const SCL_PIN: i32 = 41;

/// Marker trait for a single-address I2C peripheral on [`FeatherI2cBus`].
pub trait I2cDevice {
    /// 7-bit I2C address.
    const ADDR: u8;
    /// Register access timeout in milliseconds.
    const TIMEOUT_MS: u32 = 50;
}

/// Initialized Feather I2C controller passed to device `init` functions.
pub struct FeatherI2cBus {
    driver: I2cDriver<'static>,
}

impl FeatherI2cBus {
    /// Bring up the shared Feather I2C bus at 100 kHz.
    pub fn init<I2C: I2c>(
        i2c_peripheral: impl Peripheral<P = I2C> + 'static,
        sda: impl gpio::InputPin + gpio::OutputPin + 'static,
        scl: impl gpio::InputPin + gpio::OutputPin + 'static,
    ) -> anyhow::Result<Self> {
        let cfg = i2c::config::Config::new().baudrate(100.kHz().into());
        let driver = I2cDriver::new(i2c_peripheral, sda, scl, &cfg)
            .context("failed to initialize Feather I2C bus")?;
        log::info!(
            "I2C: bus initialized on SDA=GPIO{}, SCL=GPIO{}",
            SDA_PIN,
            SCL_PIN
        );
        Ok(Self { driver })
    }

    /// Write a register payload to a typed I2C device.
    pub fn write<D: I2cDevice>(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.driver
            .write(D::ADDR, data, D::TIMEOUT_MS)
            .map_err(|e| anyhow::anyhow!("I2C write @ 0x{:02X} failed: {e}", D::ADDR))
    }

    /// Write a register address (or prefix) and read a response from a typed device.
    pub fn write_read<D: I2cDevice>(
        &mut self,
        write: &[u8],
        read: &mut [u8],
    ) -> anyhow::Result<()> {
        self.driver
            .write_read(D::ADDR, write, read, D::TIMEOUT_MS)
            .map_err(|e| anyhow::anyhow!("I2C write_read @ 0x{:02X} failed: {e}", D::ADDR))
    }

    /// Write/read against an arbitrary 7-bit address (multi-address probes).
    pub fn write_read_addr(
        &mut self,
        addr: u8,
        write: &[u8],
        read: &mut [u8],
        timeout_ms: u32,
    ) -> anyhow::Result<()> {
        self.driver
            .write_read(addr, write, read, timeout_ms)
            .map_err(|e| anyhow::anyhow!("I2C write_read @ 0x{addr:02X} failed: {e}"))
    }

    /// Write against an arbitrary 7-bit address.
    pub fn write_addr(&mut self, addr: u8, data: &[u8], timeout_ms: u32) -> anyhow::Result<()> {
        self.driver
            .write(addr, data, timeout_ms)
            .map_err(|e| anyhow::anyhow!("I2C write @ 0x{addr:02X} failed: {e}"))
    }
}
