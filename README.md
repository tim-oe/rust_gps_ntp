# rust_gps_ntp

Rust firmware scaffold for a GPS-disciplined NTP server on:

- Adafruit ESP32-S2 TFT Feather
- Adafruit Ultimate GPS FeatherWing

This repository now includes project setup, hardware notes, and a flash workflow
for your connected board at `/dev/ttyACM0`.

## Requirements

- Follow `docs/setup.md` for Rust + Espressif toolchain installation and flashing tools.

## Current Status

- ESP-IDF Rust project scaffold is in place (`build.rs`, target config, deps)
- Default target/chip is configured for ESP32-S3 (`xtensa-esp32s3-espidf`)
- M2 implemented: UART ingest + NMEA parse (`RMC`/`ZDA`) + fix tracking
- Wi-Fi STA credentials currently load from build env (`WIFI_SSID`/`WIFI_PASS`)
- Boot now initializes STA and logs acquired DHCP IP
- Runtime now includes TFT display pages with button paging + 15s auto-blank
- Display pages: time/local estimate, position/satellites/PPS, resources, MAX17048 battery
- PPS diagnostics are wired to interrupt capture on `GPIO13`
- NTP responder is still pending

## Rust code organization note

Rust is not class-based OOP in the Java/C# sense. The common project style is:

- `struct` for data/state
- `impl` blocks for behavior/methods
- modules (`mod`) to split features
- enums/traits for extensibility

This project follows that style in `src/main.rs` for now and can be split into
`src/gps.rs`, `src/pps.rs`, and `src/ntp.rs` as milestones progress.

## Repo Layout

- `src/main.rs` - ESP32 firmware entrypoint scaffold
- `.cargo/config.toml` - default target and flash runner
- `justfile` - common development, CI, and flash commands
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
