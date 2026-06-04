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

- Boot firmware on ESP32 and emit periodic logs
- Validate flash and monitor flow on `/dev/ttyACM0`

### M2 - GPS serial ingest

- [x] Configure UART at 9600 baud
- [x] Read NMEA sentences
- [x] Parse `RMC` and `ZDA` for UTC time and date
- [x] Track GPS fix validity

### M3 - PPS discipline

- Wire PPS to interrupt-capable GPIO
- Capture microsecond timer on each rising edge
- Align parsed UTC second with PPS edge

### M4 - NTP server

- Bind UDP socket on port 123
- Build NTPv4 response packets
- Set stratum/reference fields to GPS source
- Populate transmit timestamps from disciplined clock

### M5 - robustness

- Holdover behavior when GPS fix is lost
- Basic metrics (fix age, PPS age, clients served)
- Boot-time self-check logs for UART/PPS/network state

## Data model (planned)

- `GpsState`: fix status, satellites, last parsed UTC, last NMEA update
- `PpsState`: last edge instant and quality flags
- `DisciplinedClock`: conversion between monotonic ticks and UTC/NTP time

## Test plan

- Compare output against trusted LAN NTP server
- Verify second rollover exactly on PPS pulses
- Exercise no-fix and fix-recovery scenarios
- Confirm multiple clients can query repeatedly without drift jumps
