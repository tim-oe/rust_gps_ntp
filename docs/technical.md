# Technical Implementation

GPS-disciplined Stratum-1 NTP server firmware written in Rust for the ESP32-S3 platform.

## External Specifications and References

| Topic | Resource |
|---|---|
| NMEA 0183 sentence format | [NMEA 0183 Standard (NMEA.org)](https://www.nmea.org/content/STANDARDS/NMEA_0183_Standard) · [NMEA Sentence Reference (gpsd)](https://gpsd.gitlab.io/gpsd/NMEA.html) |
| RMC sentence | [GPRMC field layout (gpsd)](https://gpsd.gitlab.io/gpsd/NMEA.html#_rmc_recommended_minimum_navigation_information) |
| GGA sentence | [GPGGA field layout (gpsd)](https://gpsd.gitlab.io/gpsd/NMEA.html#_gga_global_positioning_system_fix_data) |
| NTPv4 protocol | [RFC 5905 – Network Time Protocol Version 4](https://www.rfc-editor.org/rfc/rfc5905) |
| NTP mode-6 control | [RFC 1305 §3.2 – Control Messages](https://www.rfc-editor.org/rfc/rfc1305#section-3.2) · [ntpq reference](https://www.ntp.org/documentation/4.2.8-series/ntpq/) |
| NTP timestamp format | [RFC 5905 §6 – Data Types](https://www.rfc-editor.org/rfc/rfc5905#section-6) |
| ESP32-S3 technical reference | [ESP32-S3 Technical Reference Manual (Espressif)](https://www.espressif.com/sites/default/files/documentation/esp32-s3_technical_reference_manual_en.pdf) |
| esp-idf-svc (ESP-IDF Rust bindings) | [esp-idf-svc on crates.io](https://crates.io/crates/esp-idf-svc) · [GitHub](https://github.com/esp-rs/esp-idf-svc) · [API docs](https://docs.rs/esp-idf-svc) |
| esp-idf-sys (low-level C bindings) | [esp-idf-sys on crates.io](https://crates.io/crates/esp-idf-sys) · [API docs](https://docs.rs/esp-idf-sys) |
| Rust on ESP32 book | [The Rust on ESP Book (esp-rs)](https://docs.esp-rs.org/book/) |
| embuild (build system integration) | [embuild on crates.io](https://crates.io/crates/embuild) |
| embedded-graphics display library | [embedded-graphics on crates.io](https://crates.io/crates/embedded-graphics) · [API docs](https://docs.rs/embedded-graphics) |
| st7789 TFT driver | [st7789 on crates.io](https://crates.io/crates/st7789) · [API docs](https://docs.rs/st7789) |
| MTK3339 GPS module (Ultimate GPS FeatherWing) | [Adafruit Ultimate GPS FeatherWing guide](https://learn.adafruit.com/adafruit-ultimate-gps-featherwing) · [MTK3339 datasheet](https://cdn-shop.adafruit.com/datasheets/GlobalTop-FGPMMOPA6H-Datasheet-V0A.pdf) |
| MAX17048 battery fuel gauge | [MAX17048 datasheet (Maxim/Analog Devices)](https://www.analog.com/en/products/max17048.html) |
| LC709203 battery fuel gauge | [LC709203F datasheet (ON Semiconductor)](https://www.onsemi.com/products/power-management/battery-management/battery-fuel-gauges/lc709203f) |
| chrono date/time library | [chrono on crates.io](https://crates.io/crates/chrono) · [API docs](https://docs.rs/chrono) |
| chrono-tz timezone database | [chrono-tz on crates.io](https://crates.io/crates/chrono-tz) · [API docs](https://docs.rs/chrono-tz) |
| portable-atomic (ISR-safe atomics) | [portable-atomic on crates.io](https://crates.io/crates/portable-atomic) · [API docs](https://docs.rs/portable-atomic) |
| Open-Meteo timezone API | [Open-Meteo Forecast API docs](https://open-meteo.com/en/docs) |

---

## Architecture Overview

The firmware is structured as a Rust library (`lib.rs`) that is conditionally compiled for two build targets:

- **`target_os = "espidf"`** – full firmware build with all peripheral drivers, display, Wi-Fi, NTP server, and GPS ingest.
- **Host** – a subset of pure modules (`gps`, `ntp`, `pps`, `timezone`, `battery`) compiled for native targets to enable fast unit testing with `cargo test` / `just test`.

```
src/
├── lib.rs          # crate root; module declarations and cfg-gated exports
├── main.rs         # firmware entry point; calls app::run()
├── app.rs          # peripheral init, main service loop, task coordination
├── gps.rs          # NMEA sentence parsing, GpsSnapshot
├── pps.rs          # GPIO ISR capture, PPS interval tracking
├── ntp.rs          # NTPv4 server, clock discipline, mode-6 diagnostics
├── display.rs      # ST7789 TFT rendering, page layout
├── battery.rs      # MAX17048 / LC709203 I2C fuel gauge
├── timezone.rs     # IANA timezone resolution, NVS cache, HTTP worker
├── ui_task.rs      # FreeRTOS task for display, button, battery sampling
├── wifi.rs         # Wi-Fi STA connection using esp-idf-svc
└── logging.rs      # esp-idf logging init, boot-test flag
```

### Data Flow

```
GPS UART (9600 baud)
    │  NMEA sentences ($GPRMC, $GPGGA, $GNRMC, $GNGGA)
    ▼
gps::parse_rmc / parse_gga
    │  GpsSnapshot { utc_unix_seconds, lat, lon, fix, sats, local_time, local_date }
    ├──► NtpServer::update_gps_utc_seconds()   (sets ClockAnchor)
    └──► UiFeed::publish_gps()                 (shared Arc, read by ui_task)

GPIO12 rising-edge ISR
    │  esp_timer_get_time() → AtomicU64
    ▼
PpsMonitor::record_edge()
    │  polled each loop iteration by PpsMonitor::poll()
    ▼
NtpServer::observe_pps_pulse()   (advances ClockAnchor by 1 s per valid pulse)

UDP port 123 (nonblocking)
    │  NTP client requests (mode 3/4) or ntpq control queries (mode 6)
    ▼
NtpServer::poll()
    │  build_response() / build_mode6_response()
    └──► UDP send to client
```

---

## Module Details

### `gps` – NMEA Parsing

**Relevant spec:** [NMEA 0183 §6](https://gpsd.gitlab.io/gpsd/NMEA.html)

The module handles two sentence types:

| Sentence | Used fields |
|---|---|
| `$GPRMC` / `$GNRMC` | UTC time (`hhmmss`), date (`ddmmyy`), fix status (`A`/`V`), latitude, longitude |
| `$GPGGA` / `$GNGGA` | Satellite count |

**Checksum validation** (`nmea_checksum_valid`): XOR of all bytes between `$` and `*` must equal the two-hex-digit suffix. Sentences failing validation are silently dropped.

**Coordinate conversion** (`nmea_to_decimal`): NMEA encodes latitude as `ddmm.mmmm` and longitude as `dddmm.mmmm`. Conversion to decimal degrees: `degrees + minutes/60`, negated for `S` and `W` hemispheres.

**Timezone resolution** (`local_datetime_from_utc`): If a runtime IANA timezone is configured (via `set_runtime_timezone` or the `LOCAL_TZ` build-time env var), `chrono-tz` applies proper DST rules. Without a configured timezone the UTC offset is estimated as `round(longitude / 15)`.

**`GpsSnapshot`** – lightweight value type cloned into both the NTP and display paths:

```rust
pub struct GpsSnapshot {
    pub local_date: String,       // "YYYY-MM-DD" local
    pub local_time: String,       // "HH:MM:SS" local
    pub tz_offset_hours: i8,
    pub utc_unix_seconds: Option<i64>,
    pub lat: f32,
    pub lon: f32,
    pub fix: bool,                // RMC status == "A"
    pub sats: u8,                 // from GGA field[7]
}
```

---

### `pps` – Pulse Per Second Discipline

The MTK3339 GPS module emits a 1 Hz PPS signal wired to GPIO 12. The rising edge is captured in a GPIO ISR via `esp_timer_get_time()` (microsecond resolution, 64-bit monotonic timer).

**ISR-safe atomics**: The edge timestamp is stored in a `portable_atomic::AtomicU64` (not available in the standard library on all ESP32 targets without `portable-atomic`). A companion `AtomicU32` counts pulses.

**Main-loop polling** (`PpsMonitor::poll`): Compares the ISR pulse counter against the last-seen count. On a new edge it computes the interval with `wrapping_sub` to handle 64-bit timer rollover safely. The result is one of:

- `PpsEvent::First` – first pulse since boot; no interval yet.
- `PpsEvent::Delta(u32)` – subsequent pulse with microsecond interval.

A valid interval is defined as `800_000 µs ≤ interval ≤ 1_200_000 µs` (±20% of 1 second) to reject spurious edges.

---

### `ntp` – NTPv4 Server

**Relevant spec:** [RFC 5905 – Network Time Protocol Version 4](https://www.rfc-editor.org/rfc/rfc5905)

#### Clock Discipline

Two inputs feed the `ClockAnchor`:

1. **GPS UTC seconds** (from RMC, ~1 Hz): Sets or re-anchors the monotonic-to-UTC mapping. Re-anchoring only occurs when the predicted UTC from the current anchor diverges from GPS by more than 1 second, avoiding unnecessary jumps.
2. **PPS interval** (from GPIO ISR, 1 Hz): The first pulse aligns the anchor to the latest GPS UTC second. Subsequent valid pulses increment `anchor.unix_seconds` by 1 and update `anchor.monotonic_us` to the pulse edge.

Current NTP timestamp is computed from the anchor:

```
ntp_seconds = anchor.unix_seconds + elapsed_seconds + NTP_UNIX_EPOCH_OFFSET (2208988800)
ntp_fraction = elapsed_remainder_us * (2^32 / 1_000_000)
ntp_timestamp = (ntp_seconds << 32) | ntp_fraction
```

The 64-bit NTP timestamp format is defined in [RFC 5905 §6](https://www.rfc-editor.org/rfc/rfc5905#section-6): 32 bits of seconds since 1900-01-01 followed by 32 bits of sub-second fraction.

#### Packet Building (`build_response`)

Standard mode-3 client → mode-4 server exchange per [RFC 5905 §7.3](https://www.rfc-editor.org/rfc/rfc5905#section-7.3):

| Byte offset | Field | Value (synced) |
|---|---|---|
| 0 | LI/VN/Mode | LI=0, VN mirrored from client, Mode=4 |
| 1 | Stratum | 1 (GPS primary reference) |
| 2 | Poll | mirrored from client |
| 3 | Precision | −20 (≈1 µs) |
| 4–7 | Root Delay | 0 |
| 8–11 | Root Dispersion | 1/65536 s (synced) or 5/65536 s (unsynced) |
| 12–15 | Reference ID | `GPS\0` (synced) or `INIT` (unsynced) |
| 16–23 | Reference Timestamp | server receive timestamp |
| 24–31 | Originate Timestamp | client transmit timestamp (echoed) |
| 32–39 | Receive Timestamp | server receive timestamp |
| 40–47 | Transmit Timestamp | filled immediately before send |

When GPS fix or PPS lock is absent, stratum is set to 16 (unsynchronised) and LI to `11` (alarm).

#### Mode-6 Diagnostics

Mode-6 (control) packets from `ntpq` are answered with a minimal subset of opcodes:

- **READSTAT (op=1)**: Returns one pseudo-association entry so `ntpq -p` can proceed.
- **READVAR (op=2, assoc=0)**: System variables (`stratum`, `refid`, `precision`, `rootdelay`, `rootdisp`).
- **READVAR (op=2, assoc=1)**: Peer variables including live PPS offset, jitter, and processing delay (EWMA smoothed, α=0.2).

**Reference:** [RFC 1305 §3.2](https://www.rfc-editor.org/rfc/rfc1305#section-3.2)

#### Jitter and Delay Tracking

Both `pps_jitter_us` and `proc_delay_us` are maintained as exponentially weighted moving averages with coefficient α=0.2 (80% weight on history). The PPS offset is the deviation of the measured 1-second interval from 1,000,000 µs.

---

### `display` – TFT UI

**Driver:** `st7789` crate over SPI2 at 40 MHz, 240×135 panel with a fixed pixel offset (x=40, y=52) applied via the `OffsetDisplay` wrapper that translates logical coordinates into the physical panel memory window.

**Rendering:** `embedded-graphics` `Text` primitives using `FONT_10X20` (monospace, 10×20 px). The display refreshes on each `ui_task` iteration; `draw_page` clears to black then renders lines at 21-pixel vertical spacing.

**Pages (cycled by button on GPIO 0):**

| Page | Content |
|---|---|
| Time | Local time, date, timezone offset |
| Location | Latitude, longitude, satellite count |
| Resources | Flash partition size, heap free, heap minimum |
| Battery | Voltage, charge percent, PPS offset |

**Boot test:** On first boot, three horizontal RGB bands (red/green/blue) are drawn to verify panel visibility, followed by an 800 ms delay.

---

### `battery` – Fuel Gauge

I2C bus on GPIO 42 (SDA) / GPIO 41 (SCL) at 100 kHz. Two gauge ICs are supported and auto-detected at boot:

**MAX17048** at I2C `0x36` ([datasheet](https://www.analog.com/en/products/max17048.html)):

| Register | Address | Decode |
|---|---|---|
| VCELL | `0x02` | `u16_be * 78.125 µV` → volts |
| SOC | `0x04` | `byte[0] + byte[1]/256` → percent |

**LC709203** at I2C `0x0B` ([datasheet](https://www.onsemi.com/products/power-management/battery-management/battery-fuel-gauges/lc709203f)):

| Register | Address | Decode |
|---|---|---|
| VCELL | `0x09` | `u16_le` → millivolts |
| RSOC | `0x0D` | `u16_le` → percent |

---

### `timezone` – IANA Timezone Resolution

On first GPS fix the device looks up the IANA timezone for the current coordinates. The lookup runs on a background FreeRTOS thread (`tz_lookup`, 12 KB stack) to avoid blocking the 10 ms main loop.

**Providers (tried in order):**

1. [Open-Meteo forecast API](https://open-meteo.com/en/docs) – no API key required; JSON field `timezone`.
2. [GeoNames timezoneJSON](http://api.geonames.org/timezoneJSON) – demo account, rate-limited; JSON field `timezoneId`.

The timezone string is persisted to the ESP-IDF NVS partition (namespace `rust_gps_ntp`, key `local_tz`) so it survives power cycles. The cache is refreshed every 6 hours (`TZ_LOOKUP_REFRESH_US = 21_600_000_000 µs`); on cache miss a retry fires every 5 minutes (`TZ_LOOKUP_RETRY_US = 300_000_000 µs`).

---

### `app` – Main Service Loop

`app::run()` executes the firmware orchestration sequence:

1. `logging::init()` – configure esp-idf log level.
2. Load Wi-Fi credentials from build-time environment, connect STA.
3. Initialize UART1 at 9600 baud (TX=GPIO1, RX=GPIO2) for GPS NMEA.
4. Initialize I2C0 for battery monitor.
5. Initialize SPI2 for ST7789 TFT, power rail on GPIO 21.
6. Spawn `UiTaskHandle` (FreeRTOS task) with shared `UiFeed` arc.
7. Subscribe GPIO12 rising-edge ISR for PPS capture.
8. Bind `NtpServer` on UDP/123.
9. Load cached timezone from NVS.
10. **Loop (10 ms delay per iteration):**
    - `poll_gps_uart` – read UART bytes, accumulate lines, parse RMC/GGA.
    - Poll `TimezoneWorker` for completed HTTP result.
    - `NtpServer::poll` – serve pending UDP requests.
    - `PpsMonitor::poll` – forward new PPS events to `NtpServer` and `UiFeed`.

The main loop iteration budget is 10 ms (`FreeRtos::delay_ms(10)`). GPS sentences arrive at approximately 1 Hz with minimal per-sentence processing time; NTP requests are served non-blocking from a pre-bound UDP socket.

---

### `wifi` – Wi-Fi STA

Uses `esp_idf_svc::wifi::EspWifi` to connect to a WPA2 access point. Credentials are injected at compile time via environment variables (`WIFI_SSID`, `WIFI_PASS`). The connection is established before any GPS or NTP activity begins; Wi-Fi is required for NTP clients to reach the server and for timezone HTTP lookups.

---

## Pin Map

See [`docs/hardware.md`](hardware.md) for the canonical pin map, assembly notes, and bring-up checklist. The GPIO assignments referenced in the code are defined as named constants in `src/app.rs` (`GPS_UART_TX_PIN`, `GPS_UART_RX_PIN`, `PPS_GPIO_PIN`, `BOARD_I2C_SDA_PIN`, `BOARD_I2C_SCL_PIN`).

---

## Key Cargo Dependencies

| Crate | Version | Purpose |
|---|---|---|
| [`anyhow`](https://crates.io/crates/anyhow) | 1 | Ergonomic error propagation |
| [`chrono`](https://crates.io/crates/chrono) | 0.4 | UTC date/time arithmetic |
| [`chrono-tz`](https://crates.io/crates/chrono-tz) | 0.10 | IANA timezone DST rules (TZDATA embedded) |
| [`log`](https://crates.io/crates/log) | 0.4 | Logging facade |
| [`portable-atomic`](https://crates.io/crates/portable-atomic) | 1 | `AtomicU64` safe in ISR context on ESP32 |
| [`esp-idf-svc`](https://crates.io/crates/esp-idf-svc) | 0.51 | High-level ESP-IDF bindings (Wi-Fi, GPIO, UART, I2C, SPI, NVS, HTTP) |
| [`embedded-graphics`](https://crates.io/crates/embedded-graphics) | 0.7 | 2-D drawing primitives and text rendering |
| [`st7789`](https://crates.io/crates/st7789) | 0.7 | ST7789 TFT panel driver |
| [`display-interface-spi`](https://crates.io/crates/display-interface-spi) | 0.4 | SPI transport for `st7789` |
| [`embuild`](https://crates.io/crates/embuild) | 0.33 | `build.rs` integration for ESP-IDF cmake |
| [`serial_test`](https://crates.io/crates/serial_test) | 3 | Serialize tests sharing global timezone state |

---

## Testing

Host unit tests (`cargo test` or `just test`) cover pure modules:

| Module | What is tested |
|---|---|
| `gps` | Checksum validation, RMC/GGA parsing, coordinate conversion, timezone DST rules |
| `ntp` | Packet field layout, anchor establishment, PPS discipline, mode-6 responses, EWMA smoothing |
| `pps` | Delta computation, u64 wraparound safety, First/Delta event sequencing |
| `timezone` | JSON field extraction for Open-Meteo and GeoNames response shapes |
| `battery` | MAX17048 and LC709203 register decoding arithmetic |

ESP-IDF target modules (`app`, `display`, `wifi`, `ui_task`, `logging`) are excluded from host builds via `#[cfg(target_os = "espidf")]`.
