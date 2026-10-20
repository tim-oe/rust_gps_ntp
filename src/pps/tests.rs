use super::*;
use core::sync::atomic::Ordering;

#[test]
fn pps_monitor_default_matches_new() {
    assert_eq!(PpsMonitor::default().seq().load(Ordering::Relaxed), 0);
}

#[test]
fn record_edge_advances_sequence_by_two_and_leaves_it_even() {
    let monitor = PpsMonitor::new();
    PpsMonitor::record_edge(&monitor.edge_us(), &monitor.seq(), 42);
    assert_eq!(monitor.seq().load(Ordering::Relaxed), 2);
    assert_eq!(monitor.edge_us().load(Ordering::Relaxed), 42);
}

/// A reader must not return while the writer is mid-update (odd sequence).
/// Simulate the ISR having done only its first increment, then complete the
/// write from another thread and check the poll returns the finished value.
#[test]
fn poll_waits_out_an_in_progress_write() {
    use std::thread;
    use std::time::Duration;

    let monitor = PpsMonitor::new();
    let edge = monitor.edge_us();
    let seq = monitor.seq();
    // Writer step 1 of 3: mark in-progress.
    seq.fetch_add(1, Ordering::AcqRel);
    let finisher = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        // Writer steps 2–3: publish the timestamp and mark complete.
        edge.store(7_000_000, Ordering::Relaxed);
        seq.fetch_add(1, Ordering::Release);
    });
    let mut state = PpsPollState::default();
    assert_eq!(
        monitor.poll(&mut state),
        Some(PpsEvent::First { edge_us: 7_000_000 })
    );
    finisher.join().expect("finisher");
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
    let seq = monitor.seq();
    let mut state = PpsPollState::default();

    PpsMonitor::record_edge(&edge, &seq, 1_000_000);
    assert_eq!(
        monitor.poll(&mut state),
        Some(PpsEvent::First { edge_us: 1_000_000 })
    );

    PpsMonitor::record_edge(&edge, &seq, 2_001_000);
    assert_eq!(
        monitor.poll(&mut state),
        Some(PpsEvent::Delta {
            interval_us: 1_001_000,
            edge_us: 2_001_000
        })
    );
    assert_eq!(state.pulse_count(), 2);
}

#[test]
fn poll_returns_none_without_new_edge() {
    let monitor = PpsMonitor::new();
    let mut state = PpsPollState::default();
    assert_eq!(monitor.poll(&mut state), None);

    PpsMonitor::record_edge(&monitor.edge_us(), &monitor.seq(), 5_000_000);
    assert!(monitor.poll(&mut state).is_some());
    assert_eq!(monitor.poll(&mut state), None, "same edge must not repeat");
    assert_eq!(state.pulse_count(), 1);
}

/// If the main loop stalls and two edges land between polls, the poll must
/// report the latest edge with the interval measured from the previous
/// *observed* edge, and the cursor must jump to the current count so the
/// missed pulse is not replayed later.
#[test]
fn poll_coalesces_multiple_edges_between_polls() {
    let monitor = PpsMonitor::new();
    let edge = monitor.edge_us();
    let seq = monitor.seq();
    let mut state = PpsPollState::default();

    PpsMonitor::record_edge(&edge, &seq, 1_000_000);
    assert_eq!(
        monitor.poll(&mut state),
        Some(PpsEvent::First { edge_us: 1_000_000 })
    );

    PpsMonitor::record_edge(&edge, &seq, 2_000_000);
    PpsMonitor::record_edge(&edge, &seq, 3_000_000);
    assert_eq!(
        monitor.poll(&mut state),
        Some(PpsEvent::Delta {
            interval_us: 2_000_000,
            edge_us: 3_000_000
        })
    );
    assert_eq!(state.pulse_count(), 3);
    assert_eq!(monitor.poll(&mut state), None);
}

/// Cross-thread check of the count/timestamp pairing: a producer publishes
/// strictly increasing timestamps; the consumer must never observe a
/// timestamp older than one already seen, which would indicate the count and
/// edge reads were torn.
#[test]
fn poll_never_observes_stale_edge_under_concurrent_updates() {
    use std::sync::atomic::AtomicBool;
    use std::thread;

    let monitor = PpsMonitor::new();
    let edge = monitor.edge_us();
    let seq = monitor.seq();
    let stop = Arc::new(AtomicBool::new(false));
    let producer_stop = Arc::clone(&stop);
    let producer = thread::spawn(move || {
        let mut t = 1_000_000_u64;
        while !producer_stop.load(Ordering::Relaxed) {
            PpsMonitor::record_edge(&edge, &seq, t);
            t += 1_000;
        }
        t
    });

    let mut state = PpsPollState::default();
    let mut last_seen = 0_u64;
    let mut observed = 0_u32;
    while observed < 2_000 {
        if let Some(ev) = monitor.poll(&mut state) {
            let edge_us = match ev {
                PpsEvent::First { edge_us } | PpsEvent::Delta { edge_us, .. } => edge_us as u64,
            };
            assert!(
                edge_us > last_seen,
                "edge went backwards: {last_seen} -> {edge_us}"
            );
            last_seen = edge_us;
            observed += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    let final_t = producer.join().expect("producer");
    assert!(last_seen < final_t);
}
