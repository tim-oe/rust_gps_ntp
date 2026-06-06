# NTP Outstanding Work

This document tracks remaining NTP work after the current functional baseline:

- GPS-backed NTP timestamps
- PPS-aware discipline path
- basic mode-6 support for `ntpq -pnu`
- per-module logging (including `LOG_NTP_LEVEL`)

## Priority roadmap

## 1) Discipline servo and holdover (highest impact)

- Implement a small PLL/FLL-style servo for phase + frequency correction.
- Use PPS as the phase reference, and GPS UTC as second labeling.
- Add holdover state when PPS/GPS are lost:
  - continue serving with growing uncertainty,
  - increase root dispersion over elapsed holdover time,
  - transition stratum/leap state when uncertainty exceeds thresholds.

Why: this gives the biggest real-world stability improvement.

ESP32 feasibility: **Yes**.

## 2) Improve NTP correctness fields

- Replace synthetic values with measured/model-driven values:
  - root delay,
  - root dispersion,
  - reference timestamp aging behavior.
- Keep stratum/leap transitions consistent with sync state machine.

Why: improves standards compliance and client trust decisions.

ESP32 feasibility: **Yes**.

## 3) Expand mode-6 control support

- Flesh out `READVAR`/`READSTAT` variable coverage.
- Improve association/system status encoding for better `ntpq` output.
- Optionally support additional mode-6 opcodes used by common tooling.

Why: better observability and debugging from standard NTP tools.

ESP32 feasibility: **Yes**, with scope control to avoid unnecessary complexity.

## 4) Add NTP service protections

- Per-client rate limiting.
- Optional Kiss-o'-Death responses for abusive polling patterns.
- Simple ACL/allowlist option for trusted LAN deployments.

Why: protects limited CPU/network resources on embedded hardware.

ESP32 feasibility: **Yes**.

## 5) Leap-second and long-run edge cases

- Explicit leap indicator handling from GPS data path.
- Robust behavior across:
  - GPS outage,
  - PPS glitches,
  - long uptime and monotonic counter edge conditions.

Why: long-term correctness and resilience.

ESP32 feasibility: **Mostly yes**; leap-quality depends on the upstream GPS data quality.

## 6) Testing and interoperability

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
