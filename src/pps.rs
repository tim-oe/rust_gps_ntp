//! PPS pulse capture and interval tracking.
//!
//! Stores monotonic edge timestamps as `u64` microseconds to avoid wraparound
//! issues from truncating ESP timer values to `u32`. On ESP-IDF, [`PpsDevice`]
//! owns the GPIO pin (claimed from [`crate::pins::PinPool`]) and wraps
//! [`PpsMonitor`] polling for the main loop.

use core::sync::atomic::{AtomicU32, Ordering, fence};
use portable_atomic::AtomicU64;
use std::sync::Arc;

#[cfg(target_os = "espidf")]
use anyhow::Context;
#[cfg(target_os = "espidf")]
use esp_idf_svc::hal::gpio::{self, Input, InterruptType, PinDriver};

/// PPS input GPIO monitored with a rising-edge interrupt.
#[cfg(target_os = "espidf")]
pub const GPIO_PIN: i32 = 12;

/// Atomic state shared between the PPS GPIO ISR and the main service loop.
///
/// The pair is published with a seqlock: the ISR increments `seq` to an odd
/// value, stores the edge timestamp, then increments `seq` again to an even
/// value.  `seq / 2` is therefore the pulse count, and a reader that observes
/// the same even `seq` before and after loading `edge_us` knows the timestamp
/// belongs to exactly that pulse.
pub struct PpsMonitor {
    edge_us: Arc<AtomicU64>,
    seq: Arc<AtomicU32>,
}

/// Last pulse observed by the main loop when computing PPS intervals.
#[derive(Debug, Clone, Copy, Default)]
pub struct PpsPollState {
    last_count: u32,
    last_edge_us: u64,
}

/// Result of polling [`PpsMonitor`] for a newly captured PPS edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpsEvent {
    /// First pulse since boot; no interval available yet.
    /// `edge_us` is the ISR-captured monotonic timestamp of the edge.
    First { edge_us: i64 },
    /// Subsequent pulse with interval since previous edge (microseconds).
    /// `edge_us` is the ISR-captured monotonic timestamp of this edge.
    Delta { interval_us: u32, edge_us: i64 },
}

impl PpsPollState {
    /// Return the pulse count recorded at the last successful poll.
    ///
    /// # Parameters
    /// - `self`: Poll state updated by the most recent [`PpsDevice::poll`] call.
    ///
    /// # Returns
    /// - Total PPS pulse count last observed by the main loop.
    pub fn pulse_count(&self) -> u32 {
        self.last_count
    }
}

impl Default for PpsMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl PpsMonitor {
    /// Create a monitor with zeroed edge timestamp and pulse count.
    ///
    /// # Parameters
    /// - None.
    ///
    /// # Returns
    /// - New [`PpsMonitor`] ready for ISR registration and polling.
    pub fn new() -> Self {
        Self {
            edge_us: Arc::new(AtomicU64::new(0)),
            seq: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Clone the edge timestamp handle for GPIO ISR registration.
    ///
    /// # Parameters
    /// - `self`: Monitor providing shared atomic state.
    ///
    /// # Returns
    /// - `Arc<AtomicU64>` storing the latest PPS edge timestamp in microseconds.
    pub fn edge_us(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.edge_us)
    }

    /// Clone the sequence counter handle for GPIO ISR registration.
    ///
    /// # Parameters
    /// - `self`: Monitor providing shared atomic state.
    ///
    /// # Returns
    /// - `Arc<AtomicU32>` advanced by two on each PPS rising edge (odd while
    ///   the ISR is mid-update); `seq / 2` is the pulse count.
    pub fn seq(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.seq)
    }

    /// Record a rising edge from interrupt context.
    ///
    /// Seqlock writer side: bump `seq` to odd (`AcqRel` so the timestamp store
    /// cannot be hoisted above it), store the timestamp, then bump `seq` to
    /// even with `Release` so a reader that sees the even value also sees the
    /// timestamp.  The ISR is the only writer, so no CAS loop is needed.
    ///
    /// # Parameters
    /// - `edge_us`: Shared atomic storing the latest edge timestamp.
    /// - `seq`: Shared sequence counter (see [`PpsMonitor`]).
    /// - `now_us`: Monotonic timestamp in microseconds for the current edge.
    ///
    /// # Returns
    /// - No return value.
    pub fn record_edge(edge_us: &AtomicU64, seq: &AtomicU32, now_us: u64) {
        seq.fetch_add(1, Ordering::AcqRel);
        edge_us.store(now_us, Ordering::Relaxed);
        seq.fetch_add(1, Ordering::Release);
    }

    /// Seqlock reader side: return a consistent `(pulse_count, edge_us)` pair.
    ///
    /// Retries while the ISR is mid-update (odd `seq`) or if `seq` changed
    /// across the timestamp load.  On target the ISR cannot be pre-empted by
    /// the polling task, so a retry costs at most one ISR duration.
    fn snapshot(&self) -> (u32, u64) {
        loop {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                core::hint::spin_loop();
                continue;
            }
            let edge = self.edge_us.load(Ordering::Relaxed);
            // Order the timestamp load before the confirming sequence load.
            fence(Ordering::Acquire);
            let s2 = self.seq.load(Ordering::Relaxed);
            if s1 == s2 {
                return (s1 >> 1, edge);
            }
        }
    }

    /// Poll for a new pulse and compute the interval since the previous edge.
    ///
    /// # Parameters
    /// - `self`: Monitor holding ISR-updated atomic state.
    /// - `state`: Mutable poll cursor updated when a new pulse is observed.
    ///
    /// # Returns
    /// - `Some(PpsEvent::First { edge_us })` on the first observed pulse after boot.
    /// - `Some(PpsEvent::Delta { interval_us, edge_us })` when a later pulse arrives.
    /// - `None` when no new pulse has occurred since the last poll.
    ///
    /// `edge_us` is the ISR-captured monotonic timestamp of the rising edge.
    /// Callers should use it as the authoritative edge time rather than reading
    /// the clock again to avoid task-scheduling latency (~10–100 ms) inflating
    /// the apparent offset.
    pub fn poll(&self, state: &mut PpsPollState) -> Option<PpsEvent> {
        let (current_count, now_us) = self.snapshot();
        if current_count <= state.last_count {
            return None;
        }
        let edge_us = now_us as i64;
        let event = if state.last_edge_us > 0 {
            PpsEvent::Delta {
                interval_us: pps_delta_us(now_us, state.last_edge_us),
                edge_us,
            }
        } else {
            PpsEvent::First { edge_us }
        };

        state.last_count = current_count;
        state.last_edge_us = now_us;
        Some(event)
    }
}

/// Configure a GPIO input for PPS rising-edge capture via ISR.
#[cfg(target_os = "espidf")]
fn configure_interrupt<P>(
    pin: &mut PinDriver<'static, P, Input>,
    monitor: &PpsMonitor,
) -> anyhow::Result<()>
where
    P: gpio::InputPin,
{
    let edge_us = monitor.edge_us();
    let seq = monitor.seq();
    let on_edge = move || {
        // SAFETY: argument-free, ISR-safe read of the esp_timer counter; this
        // is the same timebase the NTP task samples via `monotonic_us_now`.
        let now_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } as u64;
        PpsMonitor::record_edge(&edge_us, &seq, now_us);
    };
    // SAFETY: `subscribe_nonstatic` requires the closure to outlive the
    // interrupt subscription.  The closure owns `Arc` clones of the shared
    // atomics (no borrowed data), and the `PinDriver` it is attached to is
    // stored in `PpsDevice` for the life of the program, so the subscription
    // is never outlived.  The closure body only performs atomic stores and an
    // FFI timer read, both of which are ISR-safe.
    unsafe { pin.subscribe_nonstatic(on_edge) }.context("failed to subscribe PPS ISR callback")?;
    pin.set_interrupt_type(InterruptType::PosEdge)
        .context("failed to set PPS interrupt type")?;
    pin.enable_interrupt()
        .context("failed to enable PPS interrupt")?;
    log::info!("PPS: monitoring GPIO{GPIO_PIN} (rising-edge interrupt)");
    Ok(())
}

/// PPS input pin, ISR state, and poll cursor owned by the PPS module.
#[cfg(target_os = "espidf")]
pub struct PpsDevice {
    pin: PinDriver<'static, gpio::Gpio12, Input>,
    monitor: PpsMonitor,
    poll_state: PpsPollState,
}

#[cfg(target_os = "espidf")]
impl PpsDevice {
    const MODULE: &'static str = "pps";

    /// Claim the PPS GPIO and arm the rising-edge interrupt handler.
    pub fn init(pool: &mut crate::pins::PinPool) -> anyhow::Result<Self> {
        let pin_gpio = pool
            .take_gpio12(Self::MODULE)
            .map_err(anyhow::Error::from)?;
        let mut pin = PinDriver::input(pin_gpio).context("failed to initialize PPS input pin")?;
        let monitor = PpsMonitor::new();
        configure_interrupt(&mut pin, &monitor)?;
        Ok(Self {
            pin,
            monitor,
            poll_state: PpsPollState::default(),
        })
    }

    /// Poll for a newly captured PPS edge.
    pub fn poll(&mut self) -> Option<PpsEvent> {
        self.monitor.poll(&mut self.poll_state)
    }

    /// Pulse count from the last successful [`Self::poll`].
    pub fn pulse_count(&self) -> u32 {
        self.poll_state.pulse_count()
    }

    /// Re-enable the GPIO interrupt after the ISR fires (ESP-IDF auto-disables it).
    pub fn reenable_interrupt(&mut self) -> anyhow::Result<()> {
        self.pin
            .enable_interrupt()
            .context("failed to re-enable PPS interrupt")
    }

    /// Release the PPS GPIO claim.
    pub fn close(self, pool: &mut crate::pins::PinPool) {
        pool.release(GPIO_PIN);
    }
}

/// Compute the microsecond interval between consecutive PPS edges.
///
/// # Parameters
/// - `current_us`: Monotonic timestamp of the latest edge.
/// - `previous_us`: Monotonic timestamp of the previous edge.
///
/// # Returns
/// - Interval in microseconds, safe across `u64` timer wraparound.
pub fn pps_delta_us(current_us: u64, previous_us: u64) -> u32 {
    current_us.wrapping_sub(previous_us) as u32
}

#[cfg(test)]
mod tests;
