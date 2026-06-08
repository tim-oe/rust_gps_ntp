//! Board GPIO allocation registry for Feather peripheral modules.
//!
//! Each hardware module claims the pins it owns through [`PinPool`]. A second
//! claim for the same GPIO number returns [`PinError::AlreadyInUse`]. Modules
//! call [`PinPool::release`] from their `close` path when dropping drivers.

use esp_idf_svc::hal::gpio::{
    Gpio0, Gpio1, Gpio2, Gpio7, Gpio10, Gpio12, Gpio21, Gpio33, Gpio35, Gpio36, Gpio37, Gpio39,
    Gpio40, Gpio41, Gpio42, Gpio45, Pins,
};

const GPIO_COUNT: usize = 49;

/// GPIO allocation conflict or missing pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    /// The GPIO number is already owned by another module.
    AlreadyInUse { pin: i32, owner: &'static str },
    /// The pin is not present in this board's [`PinPool`].
    NotAvailable { pin: i32 },
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInUse { pin, owner } => {
                write!(f, "GPIO{pin} already allocated by {owner}")
            }
            Self::NotAvailable { pin } => write!(f, "GPIO{pin} not available on this board"),
        }
    }
}

impl std::error::Error for PinError {}

/// Tracks which GPIO numbers are currently allocated and by which module.
#[derive(Debug)]
pub struct PinRegistry {
    owners: [Option<&'static str>; GPIO_COUNT],
}

impl Default for PinRegistry {
    fn default() -> Self {
        Self {
            owners: [None; GPIO_COUNT],
        }
    }
}

impl PinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn claim(&mut self, pin: i32, owner: &'static str) -> Result<(), PinError> {
        let Some(slot) = self.owners.get_mut(pin as usize) else {
            return Err(PinError::NotAvailable { pin });
        };
        if let Some(existing) = *slot {
            return Err(PinError::AlreadyInUse {
                pin,
                owner: existing,
            });
        }
        *slot = Some(owner);
        Ok(())
    }

    pub fn release(&mut self, pin: i32) {
        if let Some(slot) = self.owners.get_mut(pin as usize) {
            *slot = None;
        }
    }

    pub fn owner(&self, pin: i32) -> Option<&'static str> {
        self.owners.get(pin as usize).copied().flatten()
    }
}

/// Unallocated board GPIOs extracted from [`Peripherals::take()`] pin bundle.
pub struct PinPool {
    registry: PinRegistry,
    gpio0: Option<Gpio0>,
    gpio1: Option<Gpio1>,
    gpio2: Option<Gpio2>,
    gpio7: Option<Gpio7>,
    gpio10: Option<Gpio10>,
    gpio12: Option<Gpio12>,
    gpio21: Option<Gpio21>,
    gpio33: Option<Gpio33>,
    gpio35: Option<Gpio35>,
    gpio36: Option<Gpio36>,
    gpio37: Option<Gpio37>,
    gpio39: Option<Gpio39>,
    gpio40: Option<Gpio40>,
    gpio41: Option<Gpio41>,
    gpio42: Option<Gpio42>,
    gpio45: Option<Gpio45>,
}

impl PinPool {
    /// Take ownership of all Feather GPIO lines used by firmware modules.
    pub fn from_board_pins(pins: Pins) -> Self {
        Self {
            registry: PinRegistry::new(),
            gpio0: Some(pins.gpio0),
            gpio1: Some(pins.gpio1),
            gpio2: Some(pins.gpio2),
            gpio7: Some(pins.gpio7),
            gpio10: Some(pins.gpio10),
            gpio12: Some(pins.gpio12),
            gpio21: Some(pins.gpio21),
            gpio33: Some(pins.gpio33),
            gpio35: Some(pins.gpio35),
            gpio36: Some(pins.gpio36),
            gpio37: Some(pins.gpio37),
            gpio39: Some(pins.gpio39),
            gpio40: Some(pins.gpio40),
            gpio41: Some(pins.gpio41),
            gpio42: Some(pins.gpio42),
            gpio45: Some(pins.gpio45),
        }
    }

    pub fn registry(&self) -> &PinRegistry {
        &self.registry
    }

    pub fn release(&mut self, pin: i32) {
        self.registry.release(pin);
    }

    pub fn take_gpio0(&mut self, owner: &'static str) -> Result<Gpio0, PinError> {
        self.registry.claim(0, owner)?;
        match self.gpio0.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(0);
                Err(PinError::NotAvailable { pin: 0 })
            }
        }
    }

    pub fn take_gpio1(&mut self, owner: &'static str) -> Result<Gpio1, PinError> {
        self.registry.claim(1, owner)?;
        match self.gpio1.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(1);
                Err(PinError::NotAvailable { pin: 1 })
            }
        }
    }

    pub fn take_gpio2(&mut self, owner: &'static str) -> Result<Gpio2, PinError> {
        self.registry.claim(2, owner)?;
        match self.gpio2.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(2);
                Err(PinError::NotAvailable { pin: 2 })
            }
        }
    }

    pub fn take_gpio7(&mut self, owner: &'static str) -> Result<Gpio7, PinError> {
        self.registry.claim(7, owner)?;
        match self.gpio7.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(7);
                Err(PinError::NotAvailable { pin: 7 })
            }
        }
    }

    pub fn take_gpio10(&mut self, owner: &'static str) -> Result<Gpio10, PinError> {
        self.registry.claim(10, owner)?;
        match self.gpio10.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(10);
                Err(PinError::NotAvailable { pin: 10 })
            }
        }
    }

    pub fn take_gpio12(&mut self, owner: &'static str) -> Result<Gpio12, PinError> {
        self.registry.claim(12, owner)?;
        match self.gpio12.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(12);
                Err(PinError::NotAvailable { pin: 12 })
            }
        }
    }

    pub fn take_gpio21(&mut self, owner: &'static str) -> Result<Gpio21, PinError> {
        self.registry.claim(21, owner)?;
        match self.gpio21.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(21);
                Err(PinError::NotAvailable { pin: 21 })
            }
        }
    }

    pub fn take_gpio33(&mut self, owner: &'static str) -> Result<Gpio33, PinError> {
        self.registry.claim(33, owner)?;
        match self.gpio33.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(33);
                Err(PinError::NotAvailable { pin: 33 })
            }
        }
    }

    pub fn take_gpio35(&mut self, owner: &'static str) -> Result<Gpio35, PinError> {
        self.registry.claim(35, owner)?;
        match self.gpio35.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(35);
                Err(PinError::NotAvailable { pin: 35 })
            }
        }
    }

    pub fn take_gpio36(&mut self, owner: &'static str) -> Result<Gpio36, PinError> {
        self.registry.claim(36, owner)?;
        match self.gpio36.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(36);
                Err(PinError::NotAvailable { pin: 36 })
            }
        }
    }

    pub fn take_gpio37(&mut self, owner: &'static str) -> Result<Gpio37, PinError> {
        self.registry.claim(37, owner)?;
        match self.gpio37.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(37);
                Err(PinError::NotAvailable { pin: 37 })
            }
        }
    }

    pub fn take_gpio39(&mut self, owner: &'static str) -> Result<Gpio39, PinError> {
        self.registry.claim(39, owner)?;
        match self.gpio39.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(39);
                Err(PinError::NotAvailable { pin: 39 })
            }
        }
    }

    pub fn take_gpio40(&mut self, owner: &'static str) -> Result<Gpio40, PinError> {
        self.registry.claim(40, owner)?;
        match self.gpio40.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(40);
                Err(PinError::NotAvailable { pin: 40 })
            }
        }
    }

    pub fn take_gpio41(&mut self, owner: &'static str) -> Result<Gpio41, PinError> {
        self.registry.claim(41, owner)?;
        match self.gpio41.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(41);
                Err(PinError::NotAvailable { pin: 41 })
            }
        }
    }

    pub fn take_gpio42(&mut self, owner: &'static str) -> Result<Gpio42, PinError> {
        self.registry.claim(42, owner)?;
        match self.gpio42.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(42);
                Err(PinError::NotAvailable { pin: 42 })
            }
        }
    }

    pub fn take_gpio45(&mut self, owner: &'static str) -> Result<Gpio45, PinError> {
        self.registry.claim(45, owner)?;
        match self.gpio45.take() {
            Some(pin) => Ok(pin),
            None => {
                self.registry.release(45);
                Err(PinError::NotAvailable { pin: 45 })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_and_release_pin() {
        let mut registry = PinRegistry::new();
        registry.claim(12, "pps").unwrap();
        assert_eq!(
            registry.claim(12, "display"),
            Err(PinError::AlreadyInUse {
                pin: 12,
                owner: "pps"
            })
        );
        registry.release(12);
        registry.claim(12, "display").unwrap();
    }
}
