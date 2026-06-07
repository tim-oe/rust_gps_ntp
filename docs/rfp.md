# GPS-Disciplined NTP — Project Record

## Goal

Build a local stratum-1 NTP source on ESP32 by disciplining system time using
GPS NMEA + PPS.

## Scope

- **Platform**: ESP32 Feather board (ESP32-S3 target)
- **Time source**: Ultimate GPS FeatherWing (MTK3339) — NMEA over UART + PPS GPIO
- **Network**: Wi-Fi + UDP NTP service on port 123

---

## Milestones

### M1 — Platform bring-up

- [x] Boot firmware on ESP32 and emit periodic logs
- [x] Validate flash and monitor flow on `/dev/ttyACM0`

### M2 — GPS serial ingest

- [x] Configure UART at 9600 baud
- [x] Read NMEA sentences
- [x] Parse `RMC` and `GGA` for UTC time, date, and satellite count
- [x] Validate NMEA checksum before parse
- [x] Track GPS fix validity

### M3 — PPS discipline

- [x] Wire PPS to interrupt-capable GPIO (`GPIO12`)
- [x] Capture microsecond timer on each rising edge (`AtomicU64`)
- [x] Align parsed UTC second with PPS edge

### M4 — NTP server

- [x] Bind UDP socket on port 123
- [x] Build NTPv4 response packets with stratum/reference fields
- [x] Populate transmit timestamps from disciplined clock
- [x] Mode-6 diagnostics for `ntpq`
- [x] PLL/FLL-style servo for phase + frequency correction using PPS as phase
  reference and GPS UTC for second labeling
- [x] Holdover state on GPS/PPS loss: serve from oscillator with growing root
  dispersion, stratum demotion when uncertainty exceeds 1 s
- [x] Model-driven root delay (0, hardware reference) and root dispersion
  (`max(jitter, 100 µs) + PHI×age` — RFC 5905 §11.1, PHI = 15 ppm)
- [x] Reference timestamp set to last PPS discipline event (RFC 5905 §7.3)
- [x] Mode-6 correctness: `ClkSrc=4` (GPS/RFC 1305 Table F-2), peer `sel=6`,
  expanded `READVAR` system and peer variables

### M5 — Robustness and service protection

- [x] Per-client rate limiting (32-entry LRU table, 2 s min poll interval)
  with Kiss-o'-Death RATE responses for fast-polling clients (RFC 5905 §7.4);
  mode-6 queries exempt
- [x] IP ACL allowlist (`Acl::allow_all` / `deny_all` / `private_lan`);
  `private_lan` covers `127/8`, `10/8`, `172.16/12`, `192.168/16`
- [x] Leap indicator API (`set_leap_indicator(0–2)`) with RFC 5905 §7.3
  semantics; auto-forced to LI=3 when unsynced
- [x] PPS phase-outlier filter: reject phase errors > 50 ms after servo
  convergence; increment `pps_glitch_count`, do not advance holdover timer
- [x] Stale GPS anchor guard: first PPS pulse ignored if last GPS UTC update
  is > 2 s old, preventing cold-start anchoring to stale data
- [x] Service counters (`served`, `rate_limited`, `acl_blocked`,
  `pps_glitch_count`, `leap_indicator`) exposed in `NtpSnapshot`
- [x] Boot-time self-check logs for UART/PPS/network state

### M6 — Testing and interoperability

- [x] 121 host unit tests across all pure modules (`just test`)
  - Timestamp math: RFC 868 epoch offset, NTP fraction encoding, frequency
    correction
  - Sync state machine: Locked → Holdover → Unsync full-cycle transitions
  - Holdover: dispersion growth rate, stratum demotion, PPS recovery
  - Mode-6 framing: 32-bit alignment, zero padding, response bit, originate
    echo (RFC 5905 §7.3), VN mirror
  - Service protection: ACL CIDR matching, rate-limiter LRU eviction, KoD
    on rapid poll, KoD originate echo (RFC 5905 §7.4), rate-limited counter
    snapshot, retry-after-backoff, poll-level integration
- [x] Interop compatibility notes (`docs/interop.md`) for `ntpd`, `chronyd`,
  `systemd-timesyncd`, and `ntpsec` including iburst KoD interaction,
  `maxdistance` holdover windows, and a 7-step on-device validation checklist
- [x] mDNS registration (`gps-ntp.local`, `_ntp._udp` port 123) for zero-config
  device discovery (`CONFIG_MDNS_ENABLED=y` in `sdkconfig.defaults`);
  resolve with `ping gps-ntp.local`, `dns-sd -G v4 gps-ntp.local` (macOS),
  or `resolvectl query gps-ntp.local` (Linux systemd)

---

## Data model

| Type | Role |
|------|------|
| `GpsSnapshot` | Fix status, satellite count, local time estimate, UTC epoch |
| `PpsMonitor` | Last edge instant (`u64` monotonic µs) and pulse count |
| `NtpServer` / `ClockAnchor` | Monotonic-to-UTC conversion for NTP timestamps |
| `NtpSnapshot` | Public metrics snapshot conveyed to the UI task each second |
| `Acl` | Fixed-capacity CIDR allowlist (lives in `ntp::protection`) |

---

## Test plan

- Compare offset against a trusted LAN NTP server (target < 1 ms steady-state)
- Verify second rollover aligns exactly with PPS rising edge
- Exercise no-fix and fix-recovery (holdover → locked) scenarios
- Confirm multiple clients can query repeatedly without drift jumps
- Host unit tests: `just test`
- On-device integration: `just flash-monitor`, then `ntpq -pnu <ip>` and
  `ntpq -c "rv 0" <ip>`; see `docs/interop.md` for the full checklist

---

## ESP32 feasibility notes

All milestones are feasible on ESP32-class hardware for a LAN stratum-1
appliance. Key engineering trade-offs:

- CPU/memory budget requires simpler algorithms than full desktop `ntpd`; fixed-
  capacity arrays replace dynamic allocations throughout.
- Mode-6 coverage is intentionally minimal; depth can be increased if needed.
- Cryptographic NTP authentication (NTS) is feasible in principle but heavy for
  this class of device and unnecessary on a trusted LAN.
- Leap-second quality depends on out-of-band sources (IERS Bulletin C); the
  MTK3339 NMEA output carries no explicit leap-warning field.
