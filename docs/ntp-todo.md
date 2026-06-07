# NTP Outstanding Work

This document tracks remaining NTP work after the current functional baseline:

- GPS-backed NTP timestamps
- PPS-aware discipline path
- basic mode-6 support for `ntpq -pnu`
- per-module logging (including `LOG_NTP_LEVEL`)

## Priority roadmap

## [x] 1) Discipline servo and holdover (highest impact)

- Implement a small PLL/FLL-style servo for phase + frequency correction.
- Use PPS as the phase reference, and GPS UTC as second labeling.
- Add holdover state when PPS/GPS are lost:
  - continue serving with growing uncertainty,
  - increase root dispersion over elapsed holdover time,
  - transition stratum/leap state when uncertainty exceeds thresholds.

Why: this gives the biggest real-world stability improvement.

ESP32 feasibility: **Yes**.

## [x] 2) Improve NTP correctness fields

- Replace synthetic values with measured/model-driven values:
  - root delay: set to 0 per RFC 5905 §6 (hardware reference, zero round-trip delay to PPS pin).
  - root dispersion: model-driven as `max(jitter, MIN_HW_ACCURACY_US) + PHI × pps_age`
    where `PHI = 15 ppm` (RFC 5905 §11.1 maximum clock drift assumption) and
    `MIN_HW_ACCURACY_US = 100 µs` (GPS+PPS ISR capture floor).
    Locked dispersion is now 100–300 µs vs. the old fixed 1 ms.
  - reference timestamp aging: stored as `last_sync_ntp_ts`, the NTP timestamp of the
    most recent PPS discipline event, per RFC 5905 §7.3. Stays fixed between pulses so
    clients can observe reference aging as `current_time − reference_timestamp`.
- Stratum/leap transitions remain consistent with the holdover state machine from item 1.

Why: improves standards compliance and client trust decisions.

ESP32 feasibility: **Yes**.

## [x] 3) Expand mode-6 control support

- READVAR assoc=0 (system vars) expanded with `reftime`, `clock`, `offset`,
  `frequency`, `sys_jitter`, `clk_jitter`, `clk_wander`, `tc`, `mintc`.
- READVAR assoc=1 (peer vars) expanded with `dispersion`, `xleave`,
  `filtdelay`, `filtoffset`, `filtdisp`.
- System status word corrected: `ClkSrc = 4` (UHF/GPS) per RFC 1305 Table F-2
  instead of the previous synthetic `0x0604`.
- Peer status word corrected: `sel = 6` (system peer) causes ntpq to display `*`.
- NTP timestamp helper `ntp_ts_to_mode6` formats 64-bit NTP timestamps as
  `0xSSSSSSSS.FFFFFFFF` per RFC 1305 §3.2 text-protocol convention.

Why: better observability and debugging from standard NTP tools.

ESP32 feasibility: **Yes**, with scope control to avoid unnecessary complexity.

## [x] 4) Add NTP service protections

- **Per-client rate limiting**: a fixed-capacity table (32 entries) tracks the
  last accepted request timestamp per IPv4 address. Clients polling faster than
  `MIN_POLL_INTERVAL_US = 2 s` receive a Kiss-o'-Death RATE response and are
  counted in `rate_limited_total`.
- **Kiss-o'-Death responses** (RFC 5905 §7.4): KoD RATE packets have
  stratum=0, refid=`RATE`, and echo the client's transmit timestamp as the
  originate timestamp. Mode-6 (ntpq) queries are exempt from rate limiting.
- **ACL allowlist** (`Acl` type): fixed-capacity (8 CIDR entries). Presets:
  - `Acl::allow_all()` — default, no restrictions.
  - `Acl::deny_all()` — block everything, build explicit list with `add_ipv4_cidr`.
  - `Acl::private_lan()` — allow `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`,
    `192.168.0.0/16`. Recommended for trusted LAN deployments.
  - ACL-blocked packets are silently dropped and counted in `acl_blocked_total`.
- Counters (`served`, `rate_limited`, `acl_blocked`) exposed in `NtpSnapshot`
  for display and diagnostics.

Why: protects limited CPU/network resources on embedded hardware.

ESP32 feasibility: **Yes**.

## [x] 5) Leap-second and long-run edge cases

- **Explicit leap indicator infrastructure** (`set_leap_indicator(li: u8)`):
  - Values 0–2 per RFC 5905 §7.3 (0=normal, 1=+1s at end of day, 2=−1s).
  - Emitted in every NTP response when the server is synced (stratum 1).
  - Automatically forced to LI=3 (alarm) when unsynced regardless of setting.
  - Clamped to 2; caller must clear with `set_leap_indicator(0)` after the event.
  - GPS receivers (MTK3339) apply the GPS-UTC offset internally; standard NMEA
    sentences carry no explicit leap-warning field.  The API exists for
    integration with out-of-band sources (almanac, IERS bulletin, etc.).

- **Robust behavior improvements**:
  - **PPS phase-outlier filter**: after the servo converges (`pps_has_sample=true`),
    any PPS interval whose phase error exceeds `PPS_OUTLIER_THRESHOLD_US = 50 ms`
    is rejected.  `pps_glitch_count` is incremented and `freq_ppm` is not updated,
    preventing a single errant PPS edge from corrupting the frequency estimate.
    `last_pps_monotonic_us` is also not advanced, so the holdover timer is not reset
    by bad pulses.
  - **Stale GPS anchor guard**: the first PPS pulse only establishes the clock
    anchor if `update_gps_utc_seconds()` was called within `GPS_STALE_THRESHOLD_US
    = 2 s`.  Prevents anchoring to GPS data cached before a module reset or
    cold-start.  This is a no-op in normal operation where GPS RMC sentences
    arrive at ~1 Hz just before each PPS edge.
  - **Monotonic counter safety**: all elapsed-time arithmetic uses `saturating_sub`
    (wrapping subtraction on the `i64` timer would need 292,000 years to fire).
    The 64-bit `esp_timer_get_time()` returns `int64_t` and poses no overflow risk
    for any realistic deployment lifetime.

- `pps_glitch_count` and `leap_indicator` are exposed via `NtpSnapshot` for
  display and diagnostics.

Why: long-term correctness and resilience.

ESP32 feasibility: **Mostly yes**; leap-quality depends on the upstream GPS data quality.

## [ ] 6) Testing and interoperability

- Add unit tests for:
  - timestamp math,
  - sync state transitions,
  - holdover behavior,
  - mode-6 packet framing/padding.
- Add interop validation notes for:
  - `ntpd`,
  - `chronyd`,
  - `systemd-timesyncd`,
  - `ntpsec` tools.

Why: prevents regressions and validates behavior across clients.

ESP32 feasibility: **Yes** (host-side tests + on-device integration checks).

## Feasibility summary for ESP32

All major outstanding items are feasible on ESP32-class hardware for a LAN stratum-1 appliance.

Primary constraints are not capability blockers, but engineering trade-offs:

- CPU/memory budget requires simpler algorithms than full desktop `ntpd`.
- mode-6 should stay intentionally minimal unless more coverage is required.
- cryptographic NTP authentication/NTS is possible in principle but can be heavy for this class of device and is usually unnecessary on a trusted LAN.

## Suggested next implementation order

1. Servo + holdover state machine.
2. Root delay/dispersion modeling tied to sync uncertainty.
3. Mode-6 variable/status expansion.
4. Rate limiting + optional ACL.
5. Test suite expansion and interop matrix documentation.
