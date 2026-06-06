//! PPS pulse capture and interval tracking.
//!
//! Stores monotonic edge timestamps as `u64` microseconds to avoid wraparound
//! issues from truncating ESP timer values to `u32`.

use core::sync::atomic::{AtomicU32, Ordering};
use portable_atomic::AtomicU64;
use std::sync::Arc;

/// Shared handles updated from the GPIO ISR.
pub struct PpsMonitor {
    edge_us: Arc<AtomicU64>,
    count: Arc<AtomicU32>,
}

/// Tracks the last observed pulse for delta computation in the main loop.
#[derive(Debug, Clone, Copy, Default)]
pub struct PpsPollState {
    last_count: u32,
    last_edge_us: u64,
}

/// Result of polling the PPS monitor for a new pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpsEvent {
    /// First pulse since boot; no interval available yet.
    First,
    /// Subsequent pulse with interval since previous edge (microseconds).
    Delta(u32),
}

impl PpsPollState {
    /// Pulse count observed by the monitor at the last poll.
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
    /// Create a new monitor with zeroed atomic state.
    pub fn new() -> Self {
        Self {
            edge_us: Arc::new(AtomicU64::new(0)),
            count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Clone the edge timestamp handle for ISR registration.
    pub fn edge_us(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.edge_us)
    }

    /// Clone the pulse counter handle for ISR registration.
    pub fn count(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.count)
    }

    /// Record a rising edge from interrupt context.
    pub fn record_edge(edge_us: &AtomicU64, count: &AtomicU32, now_us: u64) {
        edge_us.store(now_us, Ordering::Relaxed);
        count.fetch_add(1, Ordering::Relaxed);
    }

    /// Poll for a new pulse and compute the interval since the previous edge.
    pub fn poll(&self, state: &mut PpsPollState) -> Option<PpsEvent> {
        let current_count = self.count.load(Ordering::Relaxed);
        if current_count <= state.last_count {
            return None;
        }

        let now_us = self.edge_us.load(Ordering::Relaxed);
        let event = if state.last_edge_us > 0 {
            PpsEvent::Delta(pps_delta_us(now_us, state.last_edge_us))
        } else {
            PpsEvent::First
        };

        state.last_count = current_count;
        state.last_edge_us = now_us;
        Some(event)
    }
}

/// Compute microsecond delta between consecutive PPS edges with `u64` wrap safety.
pub fn pps_delta_us(current_us: u64, previous_us: u64) -> u32 {
    current_us.wrapping_sub(previous_us) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(monitor.poll(&mut state), Some(PpsEvent::First));

        PpsMonitor::record_edge(&edge, &count, 2_001_000);
        assert_eq!(monitor.poll(&mut state), Some(PpsEvent::Delta(1_001_000)));
    }
}
