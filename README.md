# rust_gps_ntp

Rust firmware for a GPS-disciplined NTP server on:

- Adafruit ESP32-S3 TFT Feather (default build target)
- Adafruit Ultimate GPS FeatherWing

This repository includes project setup, hardware notes, and a flash workflow
for your connected board at `/dev/ttyACM0`.

## Requirements

- Follow `docs/setup.md` for Rust + Espressif toolchain installation and flashing tools.

## Current Status

- ESP-IDF Rust project scaffold is in place (`build.rs`, target config, deps)
- Default target/chip is configured for ESP32-S3 (`xtensa-esp32s3-espidf`)
- M2: UART ingest + NMEA parse (`RMC`/`GGA`) with checksum validation + fix tracking
- Wi-Fi STA credentials load from build env (`WIFI_SSID`/`WIFI_PASS`)
- Boot initializes STA and logs acquired DHCP IP
- TFT display pages with button paging + 15s auto-blank
- Display pages: time/local estimate, position/satellites/PPS, resources, battery
- PPS discipline on `GPIO12` (rising-edge interrupt)
- NTP UDP/123 responder with GPS/PPS-backed timestamps and mode-6 diagnostics
- IANA timezone lookup (background worker) with NVS cache

## Rust code organization

Logic lives in the library crate (`src/lib.rs`); `src/main.rs` is a thin ESP-IDF
entrypoint. Modules include `gps`, `pps`, `ntp`, `display`, `battery`, `wifi`,
`timezone`, `logging`, and `app` (main loop orchestrator).

## Repo Layout

- `src/main.rs` - ESP32 firmware entrypoint
- `src/lib.rs` - shared modules and host-testable logic
- `.cargo/config.toml` - default target and flash runner
- `justfile` - development, CI, and flash commands
- `partitions.csv` - custom partition table with larger app partition
- `docs/setup.md` - one-time toolchain install and build/flash commands
- `docs/hardware.md` - board pairing, wiring, and bring-up checklist
- `docs/rfp.md` - implementation roadmap for GPS-disciplined NTP

## Quick Start

1. Follow `docs/setup.md`.
2. Confirm board appears at `/dev/ttyACM0`.
3. Flash and monitor:

```bash
just flash-monitor
```

## Development

Host checks (format, clippy, unit tests):

```bash
just ci
```

Full firmware check (includes ESP target):

```bash
just ci-esp
```
