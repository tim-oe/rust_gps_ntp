# GPS-Disciplined NTP Roadmap

## Goal

Build a local Stratum-1-style NTP source on ESP32 by disciplining system time
using GPS NMEA + PPS.

## Scope

- Platform: ESP32 Feather board (currently configured as ESP32-S3 target)
- Time source: Ultimate GPS FeatherWing (MTK3339)
- Network: Wi-Fi + UDP NTP service on port 123

## Milestones

### M1 - Platform bring-up

- [x] Boot firmware on ESP32 and emit periodic logs
- [x] Validate flash and monitor flow on `/dev/ttyACM0`

### M2 - GPS serial ingest

- [x] Configure UART at 9600 baud
- [x] Read NMEA sentences
- [x] Parse `RMC` and `GGA` for UTC time, date, and satellite count
- [x] Validate NMEA checksum before parse
- [x] Track GPS fix validity

### M3 - PPS discipline

- [x] Wire PPS to interrupt-capable GPIO (`GPIO12`)
- [x] Capture microsecond timer on each rising edge (`AtomicU64`)
- [x] Align parsed UTC second with PPS edge

### M4 - NTP server

- [x] Bind UDP socket on port 123
- [x] Build NTPv4 response packets
- [x] Set stratum/reference fields to GPS source
- [x] Populate transmit timestamps from disciplined clock
- [x] Mode-6 diagnostics for `ntpq`
- [x] Model-driven root delay and root dispersion (PHI×age + jitter floor)
- [x] Reference timestamp set to last PPS discipline event (RFC 5905 §7.3)
- [x] Mode-6 correctness: fix ClkSrc=4 (GPS), peer sel=6, expanded READVAR variables

### M5 - robustness

- [x] Holdover behavior when GPS fix is lost (PLL/FLL servo + growing root dispersion, stratum demotion at 1 s)
- [x] Per-client rate limiting with Kiss-o'-Death RATE responses (RFC 5905 §7.4)
- [x] IP ACL allowlist (`Acl::allow_all` / `deny_all` / `private_lan`)
- [x] Leap indicator API (`set_leap_indicator`) with RFC 5905 §7.3 semantics
- [x] PPS phase-outlier filter (reject phase errors > 50 ms after servo convergence)
- [x] Stale GPS anchor guard (prevent anchoring to GPS data > 2 s old)
- [ ] Basic metrics (fix age, PPS age, clients served)
- [ ] Boot-time self-check logs for UART/PPS/network state

## Data model

- `GpsSnapshot`: fix status, satellites, local time estimate, UTC epoch
- `PpsMonitor`: last edge instant (`u64` monotonic us) and pulse count
- `NtpServer` / `ClockAnchor`: monotonic-to-UTC conversion for NTP timestamps

## Test plan

- Compare output against trusted LAN NTP server
- Verify second rollover exactly on PPS pulses
- Exercise no-fix and fix-recovery scenarios
- Confirm multiple clients can query repeatedly without drift jumps
- Host unit tests: `just test` (GPS, PPS delta, NTP packet builders, timezone JSON, battery decode)
