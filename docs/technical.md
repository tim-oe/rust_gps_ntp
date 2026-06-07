# Technical Implementation

GPS-disciplined Stratum-1 NTP server firmware written in Rust for the ESP32-S3 platform.

## External Specifications and References

| Topic | Resource |
|---|---|
| NMEA 0183 sentence format | [NMEA 0183 Standard (NMEA.org)](https://www.nmea.org/content/STANDARDS/NMEA_0183_Standard) · [NMEA Sentence Reference (gpsd)](https://gpsd.gitlab.io/gpsd/NMEA.html) |
| RMC sentence | [GPRMC field layout (gpsd)](https://gpsd.gitlab.io/gpsd/NMEA.html#_rmc_recommended_minimum_navigation_information) |
| GGA sentence | [GPGGA field layout (gpsd)](https://gpsd.gitlab.io/gpsd/NMEA.html#_gga_global_positioning_system_fix_data) |
| NTPv4 protocol | [RFC 5905 – Network Time Protocol Version 4](https://www.rfc-editor.org/rfc/rfc5905) |
| NTPv4 clock discipline algorithm | [RFC 5905 §11 – Poll Process and Clock Discipline](https://www.rfc-editor.org/rfc/rfc5905#section-11) |
| NTPv4 data types (timestamp format) | [RFC 5905 §6 – Data Types](https://www.rfc-editor.org/rfc/rfc5905#section-6) |
| NTPv4 on-wire packet format | [RFC 5905 §7.3 – NTP Extension Fields](https://www.rfc-editor.org/rfc/rfc5905#section-7.3) |
| Root dispersion and PHI model | [RFC 5905 §11.1 – Clock Discipline Procedures](https://www.rfc-editor.org/rfc/rfc5905#section-11.1) |
| Root delay for primary reference | [RFC 5905 §6 – Data Types (rootdelay = 0 for hardware reference)](https://www.rfc-editor.org/rfc/rfc5905#section-6) |
| Reference Timestamp semantics | [RFC 5905 §7.3 – Packet Header Variables](https://www.rfc-editor.org/rfc/rfc5905#section-7.3) |
| Leap indicator and stratum encoding | [RFC 5905 §7.3 – Packet Header Variables](https://www.rfc-editor.org/rfc/rfc5905#section-7.3) |
| NTP mode-6 control protocol | [RFC 1305 §3.2 – Control Messages](https://www.rfc-editor.org/rfc/rfc1305#section-3.2) · [ntpq reference](https://www.ntp.org/documentation/4.2.8-series/ntpq/) |
| Mode-6 status word and variable encoding | [RFC 1305 §3.2.1-3.2.2 – System/Peer Status Words and Variable Sets](https://www.rfc-editor.org/rfc/rfc1305#section-3.2) |
| Clock source codes (ClkSrc field) | [RFC 1305 Table F-2 – Clock Source Codes (4 = UHF/GPS)](https://www.rfc-editor.org/rfc/rfc1305#appendix-F) |
| Kiss-o'-Death (KoD) responses | [RFC 5905 §7.4 – Kiss-o'-Death (KoD) Packet](https://www.rfc-editor.org/rfc/rfc5905#section-7.4) |
| RFC 1918 private address ranges | [RFC 1918 – Address Allocation for Private Internets](https://www.rfc-editor.org/rfc/rfc1918) |
| Leap indicator semantics | [RFC 5905 §7.3 – Packet Header Variables (LI field)](https://www.rfc-editor.org/rfc/rfc5905#section-7.3) |
| GPS-UTC leap second offset | [IERS Earth Orientation Centre – Bulletin C (leap second announcements)](https://www.iers.org/IERS/EN/Publications/Bulletins/bulletins.html) |
| PLL/FLL clock servo theory | [D. L. Mills, "Internet Time Synchronization: the Network Time Protocol", IEEE Trans. Comm. 1991](https://www.eecis.udel.edu/~mills/database/papers/trans.pdf) |
| NTP clock filter and combine algorithms | [D. L. Mills, "Improved Algorithms for Synchronizing Computer Network Clocks", IEEE/ACM Trans. Netw. 1995](https://www.eecis.udel.edu/~mills/database/papers/tune2.pdf) |
| NTP project clock discipline notes | [NTP 4.2.8 Clock Discipline](https://www.ntp.org/documentation/4.2.8-series/discipline/) |
| Holdover / frequency stability characterization | [NIST Technical Note 1337 – Characterization of Clocks and Oscillators](https://tf.nist.gov/general/pdf/868.pdf) |
| GPS PPS signal specification | [NMEA 0183 §8 / MTK3339 datasheet](https://cdn-shop.adafruit.com/datasheets/GlobalTop-FGPMMOPA6H-Datasheet-V0A.pdf) |
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
NtpServer::observe_pps_pulse()
    │  First pulse: align anchor to GPS UTC
    │  Subsequent pulses: PLL servo update
    │    - proportional path: nudge anchor.monotonic_us by Kp × phase_error
    │    - integral path:     freq_ppm += Ki × phase_error
    └──► UiFeed::publish_ntp(ntp_snapshot)     (discipline metrics for display)

UDP port 123 (nonblocking)
    │  NTP client requests (mode 3/4) or ntpq control queries (mode 6)
    ▼
NtpServer::poll()
    │  discipline_params() → DisciplineParams { stratum, leap, root_disp_short }
    │  current_ntp_timestamp() → freq-corrected NTP timestamp
    │  build_response() / build_mode6_response()
    └──► UDP send to client

Main loop (every 1 s when no PPS, or on each PPS event)
    └──► UiFeed::publish_ntp(ntp_snapshot)     (keeps holdover dispersion live on display)
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

**Relevant specs:**
- [RFC 5905 – Network Time Protocol Version 4](https://www.rfc-editor.org/rfc/rfc5905)
- [RFC 5905 §11 – Clock Discipline Procedures](https://www.rfc-editor.org/rfc/rfc5905#section-11)
- [RFC 1305 §3.2 – Mode-6 Control Messages](https://www.rfc-editor.org/rfc/rfc1305#section-3.2)
- [D. L. Mills, "Internet Time Synchronization: the Network Time Protocol", IEEE Trans. Comm. 1991](https://www.eecis.udel.edu/~mills/database/papers/trans.pdf)

#### Clock Discipline and PLL Servo

The clock discipline is a simplified type-2 PLL/FLL hybrid ([RFC 5905 §11](https://www.rfc-editor.org/rfc/rfc5905#section-11), Mills 1991) operating on two inputs:

1. **GPS UTC seconds** (from RMC, ~1 Hz): Sets or re-anchors the monotonic-to-UTC mapping. Re-anchoring only occurs when the predicted UTC diverges by more than 1 second, avoiding unnecessary jumps.
2. **PPS interval** (from GPIO ISR, 1 Hz): Runs the full servo on each valid pulse.

**Servo update on each PPS pulse** (interval `I` in microseconds):

```
phase_error_us = I − 1_000_000

// Proportional path: nudge the anchor's monotonic reference
anchor.monotonic_us = pulse_edge_us − (phase_error_us × Kp)

// Integral path: accumulate oscillator frequency estimate
freq_ppm += phase_error_us × Ki
freq_ppm = clamp(freq_ppm, −500, +500)
```

Constants: `Kp = 0.1`, `Ki = 0.01`. Positive `freq_ppm` means the monotonic oscillator runs fast relative to GPS.

**Frequency-corrected timestamps**: elapsed monotonic time is scaled before computing sub-second fractions:

```
corrected_elapsed_us = raw_elapsed_us × (1 − freq_ppm / 1_000_000)
```

This removes accumulated oscillator drift between PPS pulses, improving timestamp accuracy during the inter-pulse interval.

#### Holdover State Machine

When PPS pulses stop arriving, the server enters holdover and continues serving time from the last known anchor with growing uncertainty ([RFC 5905 §11.1](https://www.rfc-editor.org/rfc/rfc5905#section-11.1)):

| PPS age | State | Stratum | Root dispersion |
|---|---|---|---|
| < 10 s | `Locked` | 1 | `max(jitter, 0.1 ms) + PHI × age` |
| 10 s – ~33 min | `Holdover` | 1 | 1 ms + 0.5 ms/s |
| > ~33 min (disp ≥ 1 s) | `Unsync` | 16 | capped at 2 s |

- **Dispersion growth rate**: 0.5 ms/s (`HOLDOVER_DISP_RATE_US_PER_SEC = 500 µs/s`). After 60 s in holdover the dispersion is ≈31 ms; clients can decide whether to trust it based on their own `maxdist` policy.
- **Stratum demotion threshold**: 1 s of root dispersion triggers `stratum=16` and `LI=11` (alarm), consistent with RFC 5905 §11.1.
- **Recovery**: stratum returns to 1 immediately upon the next valid PPS pulse.

The `DisciplineState` enum (`Locked` / `Holdover` / `Unsync`) is published via `NtpSnapshot` to the display task.

#### NTP Timestamp Format

Per [RFC 5905 §6](https://www.rfc-editor.org/rfc/rfc5905#section-6): 32-bit seconds since 1900-01-01 followed by 32-bit sub-second fraction.

```
ntp_seconds  = anchor.unix_seconds + corrected_elapsed_seconds + 2_208_988_800
ntp_fraction = corrected_remainder_us × (2³² / 1_000_000)
ntp_timestamp = (ntp_seconds << 32) | ntp_fraction
```

#### NTP Correctness Fields

**Root delay** ([RFC 5905 §6](https://www.rfc-editor.org/rfc/rfc5905#section-6)): Set to 0 in all states. For a GPS+PPS primary reference the hardware reference clock is wired directly to a GPIO pin, so the round-trip delay to the reference is modelled as zero. This is consistent with how desktop GPS-disciplined NTP implementations treat the PPS reference.

**Root dispersion** ([RFC 5905 §11.1](https://www.rfc-editor.org/rfc/rfc5905#section-11.1)): Model-driven rather than a fixed constant:

```
When PPS-locked:
  disp = max(pps_jitter_us, MIN_HW_ACCURACY_US)   ← hardware accuracy floor
        + PHI × pps_age_us                         ← drift accumulation

Where:
  MIN_HW_ACCURACY_US = 100 µs   (GPS+PPS measurement latency floor)
  PHI                = 15 ppm   (RFC 5905 maximum frequency tolerance)
  pps_age_us         = time since last PPS pulse (capped at 1 s)
```

This gives a dispersion that shrinks to the jitter floor just after each PPS pulse and grows by at most 15 µs before the next one. A typical locked value is 100–300 µs (vs. the old fixed 1 ms).

**Reference timestamp** ([RFC 5905 §7.3](https://www.rfc-editor.org/rfc/rfc5905#section-7.3)): The NTP timestamp of the most recent PPS discipline event, stored in `last_sync_ntp_ts`. This is the correct value per the RFC ("time when the system clock was last set or corrected"). Between PPS pulses the field stays fixed, so clients can observe reference aging as `current_time − reference_timestamp`. Previously this was incorrectly set to the current receive time on every request.

#### Packet Building (`build_response`)

Standard mode-3 client → mode-4 server exchange per [RFC 5905 §7.3](https://www.rfc-editor.org/rfc/rfc5905#section-7.3):

| Byte offset | Field | Value (synced) |
|---|---|---|
| 0 | LI/VN/Mode | LI=0, VN mirrored from client, Mode=4 |
| 1 | Stratum | 1 (GPS primary reference) or 16 (unsync) |
| 2 | Poll | mirrored from client |
| 3 | Precision | log2(seconds) from `max(jitter, 100 µs, loop floor, proc delay)` (typically −11…−13 when synced) |
| 4–7 | Root Delay | 0 (hardware reference, per RFC 5905 §6) |
| 8–11 | Root Dispersion | model-driven: `max(jitter, 100 µs) + PHI × age` |
| 12–15 | Reference ID | `GPS\0` (stratum 1) or `INIT` (stratum 16) |
| 16–23 | Reference Timestamp | `last_sync_ntp_ts` (time of last PPS discipline event) |
| 24–31 | Originate Timestamp | client transmit timestamp (echoed) |
| 32–39 | Receive Timestamp | server receive timestamp |
| 40–47 | Transmit Timestamp | filled immediately before send |

#### Mode-6 Diagnostics

**Relevant spec:** [RFC 1305 §3.2](https://www.rfc-editor.org/rfc/rfc1305#section-3.2)

Mode-6 (control) packets from `ntpq` are answered with the following opcodes:

- **READSTAT (op=1)**: Returns one pseudo-association entry. The system status word encodes `[LI:2][ClkSrc:6][EvtCode:4][EvtCnt:4]` per RFC 1305 §3.2.1, with `ClkSrc = 4` (UHF/GPS, per RFC 1305 Table F-2). The peer status word uses `sel = 6` (system peer), which causes `ntpq -p` to display the `*` tally mark.
- **READVAR (op=2, assoc=0)**: System variables. When synced, includes:

  | Variable | Description |
  |---|---|
  | `stratum`, `leap`, `precision` | RFC 5905 §7.3 fields |
  | `rootdelay` | 0 (hardware reference; RFC 5905 §6) |
  | `rootdisp` | model-driven dispersion in ms |
  | `refid` | `GPS` (synced) or `INIT` (unsynced) |
  | `reftime` | `0xSSSSSSSS.FFFFFFFF` — last discipline event timestamp |
  | `clock` | `0xSSSSSSSS.FFFFFFFF` — current system NTP timestamp |
  | `offset` | PPS phase offset in ms |
  | `frequency` | oscillator offset in ppm (±sign) |
  | `sys_jitter` | smoothed PPS jitter in ms |
  | `clk_jitter` | same as `sys_jitter` (PPS-derived) |
  | `clk_wander` | `\|freq_ppm\|` (approximation of frequency wander) |
  | `tc`, `mintc` | servo time constant (7) and minimum (3) |
  | `peer`, `system` | association ID and firmware string |

- **READVAR (op=2, assoc=1)**: Peer variables. When synced, includes all standard columns `ntpq -p` uses plus:

  | Variable | Description |
  |---|---|
  | `delay`, `offset`, `jitter` | processing delay, PPS offset, jitter (ms) |
  | `dispersion` | root dispersion in ms |
  | `xleave` | interleave delay (always 0.000) |
  | `filtdelay`, `filtoffset`, `filtdisp` | last-sample filter values (no 8-entry ring buffer maintained) |

**Status word encoding** (per RFC 1305 §3.2.1):
```
System status = (LI << 14) | (ClkSrc << 8) | (EvtCode << 4) | EvtCnt
  LI     = 0 (no warning) when stratum=1, 3 (alarm) when stratum=16
  ClkSrc = 4 (UHF/GPS) when synced, 0 (unspecified) when unsynced

Peer status = (Sel << 13) | (Config << 12) | (Reach << 9)
  Sel    = 6 (system peer, '*' in ntpq) when synced, 0 when not
  Config = 1 (always configured)
  Reach  = 1 when synced, 0 when not
```

#### Jitter and Delay Tracking

Both `pps_jitter_us` and `proc_delay_us` are maintained as exponentially weighted moving averages with coefficient α=0.2 (80% weight on history). The PPS offset is the raw phase error `I − 1_000_000` µs; jitter is the EWMA of its absolute value.

---

### `ntp` – Service Protections

**Relevant specs:**
- [RFC 5905 §7.4 – Kiss-o'-Death](https://www.rfc-editor.org/rfc/rfc5905#section-7.4)
- [RFC 1918 – Private Address Allocation](https://www.rfc-editor.org/rfc/rfc1918)

#### Per-Client Rate Limiter

A fixed-size table of 32 `ClientRecord` entries tracks the last accepted monotonic timestamp per IPv4 source address. On each incoming 48-byte time request (modes 0–5, 7) or mode-6 control query:

1. Look up the source address in the table (O(32) linear scan).
2. If found and `now − last_us < MIN_POLL_INTERVAL_US` (2 s): send a KoD RATE response for 48-byte modes, silently drop mode-6, increment `rate_limited_total`, and skip normal processing.
3. If found and interval is sufficient: update `last_us`, serve normally.
4. If not found: add a new entry (evicting the oldest `last_us` when the table is full), serve normally.

Mode-6 (`ntpq`) queries share the same per-client limiter. When a mode-6
request exceeds the interval the packet is silently dropped (no response) to
avoid amplification; over-limit 48-byte requests receive a KoD RATE response instead.

#### Kiss-o'-Death (KoD) Response

Per [RFC 5905 §7.4](https://www.rfc-editor.org/rfc/rfc5905#section-7.4), a KoD packet signals the client to stop polling:

| Field | Value |
|---|---|
| LI / Mode | LI=3 (alarm), Mode=4 (server) |
| Stratum | 0 (kiss packet) |
| Reference ID | `RATE` (RFC 5905 Table 6 kiss code) |
| Originate Timestamp | client's Transmit Timestamp (bytes 40–47) |
| All other fields | 0 |

The client MUST stop polling and MUST reduce its polling interval upon receiving KoD RATE.

#### IP ACL Allowlist (`Acl`)

A fixed-capacity (8-entry) CIDR allowlist controls which sources can receive responses. All packets from unlisted sources are silently dropped and counted in `acl_blocked_total`.

| Factory method | Behaviour |
|---|---|
| `Acl::allow_all()` | Permit every IPv4 source (default) |
| `Acl::deny_all()` | Block all; add entries with `add_ipv4_cidr` |
| `Acl::private_lan()` | Allow RFC 1918 + loopback only (recommended for LAN deployment) |

To configure a private-LAN ACL from `app.rs`:

```rust
ntp_server.set_acl(Acl::private_lan());
```

IPv6 sources always bypass the ACL (pass through unconditionally).

#### Diagnostics Counters

All three counters (`served`, `rate_limited`, `acl_blocked`) are exposed in `NtpSnapshot` and published to the UI task each second.

---

### `ntp` – Leap Second and Long-Run Robustness

**Relevant specs:**
- [RFC 5905 §7.3 – Leap Indicator](https://www.rfc-editor.org/rfc/rfc5905#section-7.3)
- [IERS Bulletin C – Leap Second Announcements](https://www.iers.org/IERS/EN/Publications/Bulletins/bulletins.html)

#### Leap Indicator

Per [RFC 5905 §7.3](https://www.rfc-editor.org/rfc/rfc5905#section-7.3), the two LI bits in every NTP response indicate an imminent leap second:

| Value | Meaning |
|---|---|
| `0` | No warning (normal) |
| `1` | Last minute of the current UTC day has 61 seconds (+1 leap) |
| `2` | Last minute of the current UTC day has 59 seconds (−1 leap) |
| `3` | Alarm — clock is unsynchronised |

**GPS leap handling**: The MTK3339 GPS receiver applies the GPS-UTC offset (taken from the navigation message subframe 4 page 18) internally before outputting NMEA UTC time. Standard NMEA sentences (`$GPRMC`, `$GPGGA`) carry no explicit leap-second warning field. `LI = 0` when synced is therefore correct: the time output is already leap-corrected.

**`set_leap_indicator(li: u8)` API**: Allows the application to set `LI = 1` or `LI = 2` from an external source (IERS Bulletin C, almanac data, or proprietary GPS sentences):
- Clamped to 0–2; values > 2 are clamped to 2.
- When stratum = 16 (unsync), LI is always forced to 3 regardless of this setting.
- The application must call `set_leap_indicator(0)` after the event (e.g., at 00:00:00 UTC).

#### PPS Phase-Outlier Filter

After the servo has produced at least one jitter sample (`pps_has_sample = true`), any PPS pulse whose phase error exceeds `PPS_OUTLIER_THRESHOLD_US = 50 ms` is rejected:

- `freq_ppm` is not updated (servo integrity preserved).
- `last_pps_monotonic_us` is not advanced (holdover timer is not reset by bad pulses).
- `pps_glitch_count` is incremented and included in `NtpSnapshot`.

The outlier guard is inactive on the first disciplined pulse, because the servo may legitimately need a large initial correction. The ±20% interval filter remains active for all pulses regardless.

**Rationale**: A single errant PPS edge caused by cable transients, GPS time-of-week rollover, or satellite-constellation changes should not corrupt the frequency estimate learned over many good pulses.

#### Stale GPS Anchor Guard

The first PPS pulse (`observe_pps_pulse(None)`) only establishes the clock anchor if `update_gps_utc_seconds()` was called within `GPS_STALE_THRESHOLD_US = 2 s`. This prevents anchoring to GPS data that was cached before a GPS module reset, cold-start, or initial fix acquisition:

```
gps_fresh = (now_us − last_gps_utc_update_us) < GPS_STALE_THRESHOLD_US
If !gps_fresh → log warning, skip anchor, return.
```

In normal operation (GPS outputs RMC at ~1 Hz, PPS fires ~1 s later) the guard is transparent.

#### Monotonic Counter Safety

`esp_timer_get_time()` returns a signed 64-bit microsecond counter starting at 0 on boot. Overflow would require ~292,000 years of continuous operation — not a practical risk. All elapsed-time computations use `saturating_sub` to remain well-defined even if the system clock is manipulated in tests.

---

### `display` – TFT UI

**Driver:** `st7789` crate over SPI2 at 40 MHz, 240×135 panel with a fixed pixel offset (x=40, y=52) applied via the `OffsetDisplay` wrapper that translates logical coordinates into the physical panel memory window.

**Rendering:** `embedded-graphics` `Text` primitives using `FONT_10X20` (monospace, 10×20 px). The display refreshes on each `ui_task` iteration; `draw_page` clears to black then renders lines at 21-pixel vertical spacing.

**Pages (cycled by button on GPIO 0):**

| Page | Content |
|---|---|
| Time (1/5) | Local time, date, timezone offset |
| Location (2/5) | Latitude, longitude, satellite count |
| Resources (3/5) | Flash partition size, heap free, heap minimum |
| Battery (4/5) | Voltage, charge percent, PPS offset |
| NTP (5/5) | Discipline state (LOCKED/HOLDOVER/UNSYNC), stratum, freq offset (ppm), PPS phase offset, jitter, root dispersion |

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

1. [Open-Meteo forecast API](https://open-meteo.com/en/docs) – no API key required; JSON field `timezone` (HTTPS).
2. [GeoNames timezoneJSON](https://secure.geonames.org/) – demo account, rate-limited; JSON field `timezoneId` (HTTPS via `secure.geonames.org`; `api.geonames.org` has a mismatched TLS certificate).

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

Uses `esp_idf_svc::wifi::EspWifi` to connect to a WPA2 access point. Credentials are injected at compile time via environment variables (`WIFI_SSID`, `WIFI_PASS`) and stored in the firmware binary in plaintext. The SSID (not the password) is logged at boot. See [`docs/setup.md`](setup.md#security-considerations) for deployment guidance.

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
| `ntp` | Packet field layout, anchor establishment, PPS discipline, mode-6 responses, EWMA smoothing, timestamp math (epoch offset, fraction encoding, frequency correction), sync state transitions (Locked/Holdover/Unsync cycle), mode-6 framing and 32-bit alignment, originate-timestamp echo, VN mirroring |
| `pps` | Delta computation, u64 wraparound safety, First/Delta event sequencing |
| `timezone` | JSON field extraction for Open-Meteo and GeoNames response shapes |
| `battery` | MAX17048 and LC709203 register decoding arithmetic |

ESP-IDF target modules (`app`, `display`, `wifi`, `ui_task`, `logging`) are excluded from host builds via `#[cfg(target_os = "espidf")]`.

## NTP Client Interoperability

Client-specific compatibility notes — iburst KoD behaviour, `maxdistance`
holdover windows, mode-6 diagnostics, and a 6-step on-device validation
checklist — are documented in [`docs/interop.md`](interop.md).

Clients tested and confirmed compatible: `ntpd` (ISC 4.2.x), `chronyd`
(4.x), `systemd-timesyncd` (systemd ≥ 250), and `ntpsec` (1.x).
