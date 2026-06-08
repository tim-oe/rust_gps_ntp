//! PPS pulse capture and interval tracking.
//!
//! Stores monotonic edge timestamps as `u64` microseconds to avoid wraparound
//! issues from truncating ESP timer values to `u32`.

use core::sync::atomic::{AtomicU32, Ordering};
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
pub struct PpsMonitor {
    edge_us: Arc<AtomicU64>,
    count: Arc<AtomicU32>,
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
    /// - `self`: Poll state updated by the most recent [`PpsMonitor::poll`] call.
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
            count: Arc::new(AtomicU32::new(0)),
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

    /// Clone the pulse counter handle for GPIO ISR registration.
    ///
    /// # Parameters
    /// - `self`: Monitor providing shared atomic state.
    ///
    /// # Returns
    /// - `Arc<AtomicU32>` incremented on each PPS rising edge.
    pub fn count(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.count)
    }

    /// Record a rising edge from interrupt context.
    ///
    /// # Parameters
    /// - `edge_us`: Shared atomic storing the latest edge timestamp.
    /// - `count`: Shared atomic incremented once per pulse.
    /// - `now_us`: Monotonic timestamp in microseconds for the current edge.
    ///
    /// # Returns
    /// - No return value.
    pub fn record_edge(edge_us: &AtomicU64, count: &AtomicU32, now_us: u64) {
        edge_us.store(now_us, Ordering::Relaxed);
        count.fetch_add(1, Ordering::Relaxed);
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
        let current_count = self.count.load(Ordering::Relaxed);
        if current_count <= state.last_count {
            return None;
        }

        let now_us = self.edge_us.load(Ordering::Relaxed);
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
pub fn configure_interrupt<P>(
    pin: &mut PinDriver<'static, P, Input>,
    monitor: &PpsMonitor,
) -> anyhow::Result<()>
where
    P: gpio::InputPin,
{
    let edge_us = monitor.edge_us();
    let count = monitor.count();
    unsafe {
        pin.subscribe_nonstatic(move || {
            let now_us = esp_idf_svc::sys::esp_timer_get_time() as u64;
            PpsMonitor::record_edge(&edge_us, &count, now_us);
        })
        .context("failed to subscribe PPS ISR callback")?;
    }
    pin.set_interrupt_type(InterruptType::PosEdge)
        .context("failed to set PPS interrupt type")?;
    pin.enable_interrupt()
        .context("failed to enable PPS interrupt")?;
    log::info!("PPS: monitoring GPIO{GPIO_PIN} (rising-edge interrupt)");
    Ok(())
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
mod tests {
    use super::*;
    use core::sync::atomic::Ordering;

    #[test]
    fn pps_monitor_default_matches_new() {
        assert_eq!(PpsMonitor::default().count().load(Ordering::Relaxed), 0);
    }

    #[test]
    fn pps_delta_one_second_apart() {
        assert_eq!(pps_delta_us(2_000_000, 1_000_000), 1_000_000);
    }

    #[test]
    fn pps_delta_survives_u64_wraparound() {
        let prev = u64::MAX - 999_999;
        let curr = 0;
        assert_eq!(pps_delta_us(curr, prev), 1_000_000);
    }

    #[test]
    fn poll_emits_first_then_delta() {
        let monitor = PpsMonitor::new();
        let edge = monitor.edge_us();
        let count = monitor.count();
        let mut state = PpsPollState::default();

        PpsMonitor::record_edge(&edge, &count, 1_000_000);
        assert_eq!(
            monitor.poll(&mut state),
            Some(PpsEvent::First { edge_us: 1_000_000 })
        );

        PpsMonitor::record_edge(&edge, &count, 2_001_000);
        assert_eq!(
            monitor.poll(&mut state),
            Some(PpsEvent::Delta {
                interval_us: 1_001_000,
                edge_us: 2_001_000
            })
        );
        assert_eq!(state.pulse_count(), 2);
    }
}
