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
| NTP mode-6 control protocol | [RFC 1305 Appendix B – NTP Control Messages](https://www.rfc-editor.org/rfc/rfc1305#appendix-B) · [ntpq reference](https://www.ntp.org/documentation/4.2.8-series/ntpq/) |
| Mode-6 status words (system, peer, error) | [RFC 1305 B.2 – Status Words](https://www.rfc-editor.org/rfc/rfc1305#appendix-B) (B.2.1 system incl. clock-source codes, B.2.2 peer, B.2.4 error) |
| Mode-6 opcodes | [RFC 1305 B.3 – Commands](https://www.rfc-editor.org/rfc/rfc1305#appendix-B) (READSTAT = 1, READVAR = 2) |
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
├── board.rs        # BoardBoot: Peripherals::take, PinPool, init order, UI spawn
├── pins.rs         # PinPool / PinRegistry: GPIO claim tracking
├── app.rs          # main service loop; GpsConsumer wiring to NTP/UI
├── i2c_bus.rs      # shared Feather I2C bus and I2cDevice trait
├── gps.rs          # GpsUart ingest, GpsConsumer callbacks, NMEA parse, GpsSnapshot
├── pps.rs          # PpsDevice: GPIO ISR capture, PpsEvent polling
├── ntp/
│   ├── mod.rs          # NTPv4 server, clock discipline, mode-6 diagnostics
│   ├── protection.rs   # per-client rate limiter and IP ACL
│   └── cidr_parse.rs   # parse_ipv4_cidr, include!d by protection.rs and build.rs
│   (each host-testable module keeps its tests in <module>/tests.rs)
├── display.rs      # DisplayDevice: ST7789 TFT, shared SPI bus, panel init
├── storage.rs      # StorageDevice: Adalogger microSD on shared SPI
├── rtc.rs          # PCF8523 RTC on Feather I2C
├── battery.rs      # MAX17048 / LC709203 I2C fuel gauge
├── timezone.rs     # IANA timezone resolution, NVS cache, HTTP worker
├── ui_task.rs      # FreeRTOS task for display, button, battery sampling
├── wifi.rs         # Wi-Fi STA connection using esp-idf-svc
├── logging.rs      # esp-idf logging init, boot-test flag
└── sdlog.rs        # mirrors info+ log records to /sdcard/gps-ntp.log
```

### Data Flow

```
BoardBoot::boot()
    │  Peripherals::take → PinPool → module init (ordered: GPS, PPS, display SPI,
    │  I2C, SD mount, TFT panel, UI task)
    ▼
GpsUart::poll(consumer)          [non-blocking UART read, timeout=0]
    │  parse_rmc / parse_gga → GpsSnapshot (held inside GpsUart)
    │  synchronous GpsConsumer callbacks (no queue):
    │    on_snapshot  → UiFeed::publish_gps()
    │    on_fix       → NtpServer::update_gps_utc_seconds()
    │                   + timezone lookup scheduling (via app::GpsDispatch)

GPIO12 rising-edge ISR (PpsDevice)
    │  esp_timer_get_time() → AtomicU64
    ▼
PpsDevice::poll() → PpsEvent
    │  First / Delta { interval_us, edge_us }
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

### `gps` – NMEA Parsing and UART Ingest

**Module configuration:** at boot `GpsUart::init` writes `$PMTK314,0,1,0,1,…*28` (RMC + GGA only, every fix) to the FeatherWing. The default MTK sentence set at 9600 baud can take a full second to transmit, which pushes `RMC` (the only sentence that feeds NTP time) onto the next PPS edge and breaks the sentence↔edge pairing; see the `ntp` integer-second accounting section. The command is idempotent and ignored by non-MTK modules. `GpsUart::discard_backlog()` is called once by `app` immediately before the poll loop so the first parsed sentences are live rather than buffered during bring-up.


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
    pub altitude_m: Option<f32>,  // GGA field 9 (metres MSL)
}
```

**`GpsUart`** (ESP-IDF only) owns UART1 at 9600 baud, line buffering, and the
working [`GpsSnapshot`]. [`GpsUart::poll`] performs a non-blocking read
(`timeout = 0`), assembles NMEA lines, parses RMC/GGA, and invokes registered
consumers immediately:

```rust
pub trait GpsConsumer {
    fn on_snapshot(&mut self, gps: &GpsSnapshot);  // any successful RMC/GGA
    fn on_fix(&mut self, gps: &GpsSnapshot);        // RMC with active fix
}
```

[`crate::app`] implements `GpsConsumer` via `GpsDispatch`, wiring UI publish,
NTP UTC updates, and timezone lookup scheduling. There is no event queue — callbacks
run synchronously in the ingest path for minimum latency.

---

### `pps` – Pulse Per Second Discipline

The MTK3339 GPS module emits a 1 Hz PPS signal wired to GPIO 12. The rising edge is captured in a GPIO ISR via `esp_timer_get_time()` (microsecond resolution, 64-bit monotonic timer).

**ISR-safe atomics**: The edge timestamp is stored in a `portable_atomic::AtomicU64` (not available in the standard library on all ESP32 targets without `portable-atomic`). A companion `AtomicU32` counts pulses.

**Main-loop polling** (`PpsDevice::poll`): Compares the ISR pulse counter against the last-seen count. On a new edge it computes the interval with `wrapping_sub` to handle 64-bit timer rollover safely. The result is one of:

- `PpsEvent::First { edge_us }` – first pulse since boot; no interval yet.
- `PpsEvent::Delta { interval_us, edge_us }` – subsequent pulse with microsecond interval and ISR-captured edge time.

A valid interval is defined as `800_000 µs ≤ interval ≤ 1_200_000 µs` (±20% of 1 second) to reject spurious edges.

---

### `ntp` – NTPv4 Server

**Relevant specs:**
- [RFC 5905 – Network Time Protocol Version 4](https://www.rfc-editor.org/rfc/rfc5905)
- [RFC 5905 §11 – Clock Discipline Procedures](https://www.rfc-editor.org/rfc/rfc5905#section-11)
- [RFC 1305 Appendix B – Mode-6 Control Messages](https://www.rfc-editor.org/rfc/rfc1305#appendix-B)
- [D. L. Mills, "Internet Time Synchronization: the Network Time Protocol", IEEE Trans. Comm. 1991](https://www.eecis.udel.edu/~mills/database/papers/trans.pdf)

#### Clock Discipline and PLL Servo

The clock discipline is a simplified type-2 PLL/FLL hybrid ([RFC 5905 §11](https://www.rfc-editor.org/rfc/rfc5905#section-11), Mills 1991) operating on two inputs:

1. **GPS UTC seconds** (from RMC, ~1 Hz): Establishes the monotonic-to-UTC mapping before PPS lock and, once locked, supplies the integer second that each PPS edge is cross-checked against (see *Integer-second accounting* below).
2. **PPS interval** (from GPIO ISR, 1 Hz): Runs the full servo on each valid pulse.

**Servo update on each PPS pulse** (interval `I` in microseconds):

```
interval_error_us = I − 1_000_000        // µs over one second ≡ ppm

// Frequency estimate: EWMA of the per-second interval error.  The error is a
// direct measurement of the oscillator offset, so it is blended toward the
// measurement rather than integrated (an integrator has no feedback term and
// grows without bound on a constant offset).
freq_ppm += (interval_error_us − freq_ppm) × Ki
freq_ppm  = clamp(freq_ppm, −500, +500)

// Phase: re-anchor at this edge, nudged by a fraction of the interval error
anchor.monotonic_us = pulse_edge_us − (interval_error_us × Kp)
```

Constants: `Kp = 0.1`, `Ki = 0.01` (≈100-pulse time constant). Positive `freq_ppm` means the monotonic oscillator runs fast relative to GPS.

**Integer-second accounting.** The anchor's UTC second is never advanced by a fixed +1 per pulse, because pulses are legitimately skipped: a main-loop stall collapses two ISR edges into one ~2 s interval that the ±20% range filter rejects, and the outlier guard drops single bad edges. On each accepted pulse the servo instead:

```
elapsed_secs = round(freq_corrected(edge_us − anchor.monotonic_us) / 1 s)
predicted    = anchor.unix_seconds + elapsed_secs

// GPS second for this edge, derived from the NMEA *parse time* t_nmea of the
// latest sentence (UTC = N) so the mapping survives late-processed edges and
// dropped sentences:
gps_second   = N + floor((edge_us − t_nmea) / 1 s) + NMEA_PPS_FUDGE_S   // if fresh (< 2 s)

// A disagreement is acted on only when the same delta is seen on two
// consecutive accepted pulses:
anchor.unix_seconds = gps_second if fresh && gps_second ≠ predicted
                                    && pending == gps_second − predicted
                      else predicted
```

The two-pulse confirmation exists because the sentence↔edge pairing is ambiguous whenever the sentence for second *N* is parsed after edge *N+1* has fired: `floor((edge_us − t_nmea) / 1 s)` is −1 and `gps_second` reads one low for exactly that pulse. Without confirmation this produced a −1 step immediately followed by a +1 step. An overnight card log showed the dominant cause was not a loop stall but the module itself: with the MTK3333's default sentence set (GGA, GSA, GSV×n, RMC, VTG) and multi-GNSS satellite counts, the per-second burst at 9600 baud is ~0.9–1.05 s long and `RMC`, near the end of the burst, arrives within ±3 ms of the *next* PPS edge. `gps` therefore sends `PMTK314` at boot to restrict output to RMC + GGA (~150 ms burst) and discards the UART backlog before the first poll so stale buffered sentences cannot anchor the first lock a second low. The first disagreeing pulse is logged at `info` with the "NMEA lead" (`edge_us − t_nmea`; negative means the sentence was parsed after the edge, the stall signature); a confirmed step is logged at `warn` and counted in `anchor_step_count` (exposed in `NtpSnapshot`). A coalesced PPS interval (outside 0.8–1.2 s, i.e. the loop did not run for over a second) is also logged at `warn`.

This is also how a positive leap second is handled: NMEA `23:59:60` fails to parse, the pulse count runs one ahead, and the post-leap sentences pull the anchor back on the second confirming pulse (served time is one second fast for two seconds after the event; clients were already warned through the LI bits).

Before the first PPS pulse the anchor is simply the NMEA second at its parse time (no fudge). That estimate is a few hundred milliseconds late and is served at stratum 16; the fudge describes the *edge*↔sentence relationship and is only applied once an edge timestamp exists.

**Frequency-corrected timestamps**: elapsed monotonic time is scaled before computing sub-second fractions:

```
corrected_elapsed_us = raw_elapsed_us × (1 − freq_ppm / 1_000_000)
```

This removes accumulated oscillator drift between PPS pulses, improving timestamp accuracy during the inter-pulse interval.

#### Holdover State Machine

When PPS pulses stop arriving, the server enters holdover and continues serving time from the last known anchor with growing uncertainty ([RFC 5905 §11.1](https://www.rfc-editor.org/rfc/rfc5905#section-11.1)):

| PPS age | State | Stratum (internal / on wire) | Root dispersion |
|---|---|---|---|
| < 10 s, GPS fix | `Locked` | 1 / 1 | `max(jitter, 0.1 ms) + PHI × age` |
| < 10 s, GPS fix lost | `Holdover` | 1 / 1 | 1 ms |
| 10 s – ~33 min | `Holdover` | 1 / 1 | 1 ms + 0.5 ms/s |
| > ~33 min (disp ≥ 1 s) | `Unsync` | 16 / 0 | capped at 2 s |

- **Entry condition**: holdover requires an anchor that was PPS-disciplined at least once (`pps_locked`). An RTC-seeded or NMEA-only anchor is served at stratum 16 regardless of age.
- **Dispersion growth rate**: 0.5 ms/s (`HOLDOVER_DISP_RATE_US_PER_SEC = 500 µs/s`). After 60 s in holdover the dispersion is ≈31 ms; clients can decide whether to trust it based on their own `maxdist` policy.
- **Stratum demotion threshold**: 1 s of root dispersion (`HOLDOVER_UNSYNC_US`, reached ≈1998 s after holdover entry) triggers `stratum=16` and `LI=11` (alarm), consistent with RFC 5905 §11.1.
- **Recovery**: stratum returns to 1 immediately upon the next valid PPS pulse.

`discipline_params` computes stratum and `DisciplineState` together from one rule, so the UI state and the wire stratum cannot disagree. The `DisciplineState` enum (`Locked` / `Holdover` / `Unsync`) is published via `NtpSnapshot` to the display task.

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

**Reference timestamp** ([RFC 5905 §7.3](https://www.rfc-editor.org/rfc/rfc5905#section-7.3)): The disciplined NTP time of the most recent accepted PPS edge (the top of its UTC second), stored in `last_sync_ntp_ts`. This is the value the RFC describes ("time when the system clock was last set or corrected"). Between PPS pulses the field stays fixed, so clients can observe reference aging as `current_time − reference_timestamp`.

#### Mode Dispatch (`poll`)

Per [RFC 5905 §9.2](https://www.rfc-editor.org/rfc/rfc5905#section-9.2) Table 10 only packets that constitute a request are answered:

- **Mode 3 (client)** → reply mode 4 (server).
- **Mode 1 (symmetric active)** → reply mode 2 (symmetric passive) without mobilising an association, matching ntpd's `nopeer` behaviour.
- **Mode 6 (control)** → mode-6 response (see *Mode-6 Diagnostics*).
- **Modes 0, 2, 4, 5, 7** are discarded silently. Answering a mode-4 packet would let a spoofed source bounce replies between two servers indefinitely.

KoD RATE responses carry the same reply mode as a normal reply would.

#### Packet Building (`build_response`)

Standard mode-3 client → mode-4 server exchange per [RFC 5905 §7.3](https://www.rfc-editor.org/rfc/rfc5905#section-7.3):

| Byte offset | Field | Value (synced) |
|---|---|---|
| 0 | LI/VN/Mode | LI=0, VN mirrored from client, Mode=4 (or 2 for a mode-1 request) |
| 1 | Stratum | 1 (GPS primary reference) or 0 when unsynchronised (RFC 5905 §7.3: internal stratum 16 is transmitted as 0) |
| 2 | Poll | mirrored from client |
| 3 | Precision | log2(seconds) from `max(jitter, 100 µs, loop floor, proc delay)` (typically −11…−13 when synced). RFC 5905 §7.3 defines this as the clock's *resolution*; we deliberately report the coarser *effective* precision (timestamp jitter plus the ~1 ms poll-loop quantum), which is what clients use when weighting this server, so advertising the 1 µs timer resolution would overstate quality |
| 4–7 | Root Delay | 0 (hardware reference, per RFC 5905 §6) |
| 8–11 | Root Dispersion | model-driven: `max(jitter, 100 µs) + PHI × age` |
| 12–15 | Reference ID | `GPS\0` (stratum 1) or `INIT` (unsynchronised; LI=3 distinguishes this from a KoD, whose refid is `RATE`) |
| 16–23 | Reference Timestamp | `last_sync_ntp_ts` (time of last PPS discipline event) |
| 24–31 | Originate Timestamp | client transmit timestamp (echoed) |
| 32–39 | Receive Timestamp | server receive timestamp |
| 40–47 | Transmit Timestamp | filled immediately before send |

#### Mode-6 Diagnostics

**Relevant spec:** [RFC 1305 Appendix B](https://www.rfc-editor.org/rfc/rfc1305#appendix-B)

Mode-6 (control) packets from `ntpq` are answered with the following opcodes:

- **READSTAT (op=1, assoc=0)**: Returns one pseudo-association entry `(assoc id = 1, peer status)`. The header carries the system status word `[LI:2][ClkSrc:6][EvtCode:4][EvtCnt:4]` per RFC 1305 B.2.1, with `ClkSrc = 4` (UHF satellite radio). The peer status word uses `sel = 6` (system peer), which causes `ntpq -p` to display the `*` tally mark.
- **READSTAT (op=1, assoc=1)**: Returns that association's peer status word in the header status field with no payload (RFC 1305 B.3).
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

**Status word encoding** (per RFC 1305 B.2):
```
System status = (LI << 14) | (ClkSrc << 8) | (EvtCode << 4) | EvtCnt
  LI     = pending leap indicator (0/1/2) when stratum=1, 3 (alarm) when stratum=16
  ClkSrc = 4 (UHF/GPS) when synced, 0 (unspecified) when unsynced

Peer status = (Sel << 13) | (Config << 12) | (Reach << 9)
  Sel    = 6 (system peer, '*' in ntpq) when synced, 0 when not
  Config = 1 (always configured)
  Reach  = 1 when synced, 0 when not

Error status = (Code << 8)            // replaces the status word when the E bit is set
  Code 3 = invalid opcode, Code 4 = unknown association id (RFC 1305 B.2.4)
```

The `leap=` text variable is the two-bit LI value in binary (`00`, `01`, `10`, `11`), matching `ntpq`.

**Error handling and hygiene**:

- Unknown opcodes and unknown association ids get an error response: response bit + error bit set, opcode and sequence echoed, error status word as above, no data.
- Packets with the response bit (R) already set are dropped before rate limiting: they are replies, not requests, and answering them would allow reflection between two servers.
- Mode-6 shares the per-client rate limiter with time requests but is silently dropped (no KoD) when limited, since a KoD is a 48-byte time-format packet that `ntpq` would not understand.

#### Jitter and Delay Tracking

Both `pps_jitter_us` and `proc_delay_us` are maintained as exponentially weighted moving averages with coefficient α=0.2 (80% weight on history). The PPS offset is the raw phase error `I − 1_000_000` µs; jitter is the EWMA of its absolute value.

---

### `ntp` – Service Protections

**Relevant specs:**
- [RFC 5905 §7.4 – Kiss-o'-Death](https://www.rfc-editor.org/rfc/rfc5905#section-7.4)
- [RFC 1918 – Private Address Allocation](https://www.rfc-editor.org/rfc/rfc1918)

#### Per-Client Rate Limiter

A fixed-size table of 32 `ClientRecord` entries tracks the last accepted monotonic timestamp per IPv4 source address. On each incoming 48-byte time request (mode 3 or mode 1) or mode-6 control query:

1. Look up the source address in the table (O(32) linear scan).
2. If found and `now − last_us < MIN_POLL_INTERVAL_US` (2 s): send a KoD RATE response for time requests, silently drop mode-6, increment `rate_limited_total`, and skip normal processing.
3. If found and interval is sufficient: update `last_us`, serve normally.
4. If not found: add a new entry (evicting the oldest `last_us` when the table is full), serve normally.

Mode-6 (`ntpq`) queries share the same per-client limiter. When a mode-6
request exceeds the interval the packet is silently dropped (no response) to
avoid amplification; over-limit 48-byte requests receive a KoD RATE response instead.

The limiter is keyed on IPv4 addresses only: IPv6 sources are not tracked and
are never rate limited (the firmware binds an IPv4 socket, so this path is
unreachable in practice but is noted for completeness). The 32-entry table
means that more than 32 distinct clients polling inside one interval can evict
each other and defeat the limit; the ACL is the primary defence against
untrusted sources, and the limiter is a safety net against misbehaving LAN
clients.

#### Kiss-o'-Death (KoD) Response

Per [RFC 5905 §7.4](https://www.rfc-editor.org/rfc/rfc5905#section-7.4), a KoD packet signals the client to stop polling:

| Field | Value |
|---|---|
| LI / Mode | LI=3 (alarm), Mode=4 (server) or 2 for a mode-1 request |
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

To configure the compile-time ACL (via `NTP_ACL_CIDR` in `build.rs`):

```rust
// Applied automatically at boot by BoardBoot::init_ntp_server()
server.apply_boot_acl();
// Empty NTP_ACL_CIDR → RFC 1918 private networks; non-empty → that CIDR only
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
| Time (1/5) | Local time, date, timezone offset, PPS offset |
| Location (2/5) | Lat/lon/alt/sats, or RTC fallback when no fix |
| Resources (3/5) | SD free/total (or flash partition if unmounted), heap free/min |
| Battery (4/5) | Voltage, charge percent, RTC local time/date |
| NTP (5/5) | Discipline state (LOCKED/HOLDOVER/UNSYNC), stratum, freq offset (ppm), jitter, root dispersion, served count |

**Boot test:** On first boot, three horizontal RGB bands (red/green/blue) are drawn to verify panel visibility, followed by an 800 ms delay.

**Shared SPI bus ordering:** The TFT shares SPI2 with the microSD socket. `DisplayDevice::init` attaches the ST7789 as an SPI device (hardware CS on GPIO 7) *before* storage probes the card; `spi_bus_add_device` parks CS inactive, so the TFT stays deselected during the SD probe without any direct GPIO register writes. `init_panel` later takes that device handle and wires DC/RST/backlight.

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

On first GPS fix the device looks up the IANA timezone for the current coordinates. The lookup runs on a background FreeRTOS thread (`tz_lookup`, 12 KB stack) to avoid blocking the 1 ms main loop.

**Providers (tried in order):**

1. [Open-Meteo forecast API](https://open-meteo.com/en/docs) – no API key required; JSON field `timezone` (HTTPS).
2. [GeoNames timezoneJSON](https://secure.geonames.org/) – demo account, rate-limited; JSON field `timezoneId` (HTTPS via `secure.geonames.org`; `api.geonames.org` has a mismatched TLS certificate).

The timezone string is persisted to the ESP-IDF NVS partition (namespace `rust_gps_ntp`, key `local_tz`) so it survives power cycles. The cache is refreshed every 6 hours (`timezone::LOOKUP_REFRESH_US`); on cache miss a retry fires every 5 minutes (`timezone::LOOKUP_RETRY_US`).

---

### `board` – Peripheral Routing

[`BoardBoot::boot`] performs the full hardware bring-up sequence:

1. `Peripherals::take()` and [`PinPool::from_board_pins`]
2. Wi-Fi STA connect and optional mDNS registration
3. Module init in dependency order: `GpsUart` → `PpsDevice` → `DisplayDevice` (SPI bus) → `FeatherI2cBus` → `StorageDevice` (shared SPI) → TFT panel → UI task spawn
4. Timezone cache load and `UiFeed` seeding

Returns [`BoardRuntime`] handles for the main loop plus [`BoardKeepalive`] (Wi-Fi,
display bus, storage mount, UI thread). [`BoardBoot::init_ntp_server`] binds UDP/123,
applies compile-time ACL, and seeds from boot-time RTC when available.

---

### `pins` – GPIO Registry

[`PinPool`] holds unused Feather GPIOs extracted from `Peripherals::take()`.
Each module calls `take_gpioN("module_name")` during init; a second claim for the
same number returns [`PinError::AlreadyInUse`]. Modules release claims in `close()`.

Peripheral controller blocks (`uart1`, `spi2`, `i2c0`) are passed only to the
module that owns each bus; GPIO numbers are never exposed through `app`.

---

### `app` – Main Service Loop

`app::run()` executes:

1. `logging::init()`
2. `BoardBoot::boot(&wifi_creds)` – all hardware and UI task setup
3. `board.init_ntp_server()` – NTP bind, ACL, RTC seed
4. `board.into_runtime()` – split loop handles from keep-alive drivers
5. **Loop (1 ms delay per iteration, requires `CONFIG_FREERTOS_HZ=1000`):**
    - `GpsUart::poll(&mut GpsDispatch)` – UART ingest with direct consumer callbacks
    - Poll `TimezoneWorker` for completed HTTP result
    - `NtpServer::poll` – serve pending UDP requests
    - `PpsDevice::poll` – forward PPS events to `NtpServer` and `UiFeed`
    - Periodic storage status refresh, RTC writeback, boot self-checks

The main loop sleeps 1 ms per iteration (`FreeRtos::delay_ms(1)`). At the default 100 Hz FreeRTOS tick rate this would round up to 10 ms; `sdkconfig.defaults` sets `CONFIG_FREERTOS_HZ=1000` so the sleep is actually 1 ms. GPS sentences arrive at approximately 1 Hz with minimal per-sentence processing time; NTP requests are served non-blocking from a pre-bound UDP socket.

---

### `sdlog` – microSD Log Mirror

`sdlog::install()` replaces `EspLogger::initialize_default()` at boot with a wrapper that forwards every record to the console unchanged and additionally queues `info`-and-above records (regardless of per-tag console overrides) as `"<uptime ms> <LEVEL> <target>: <message>"` lines, the same millisecond stamp the console prints. Once `board` has mounted the card it calls `sdlog::start_writer()`, a priority‑2 thread (below the UI task at 5 and the main task at 10) that drains the queue to `/sdcard/gps-ntp.log` every 5 s, opening and closing the file each time so lines survive a power pull. The file rotates to `gps-ntp.1.log` past 8 MiB. The queue is capped at 16 KiB; overflow is counted and reported as a single `dropped` line on the next flush rather than blocking the logging task. Logging call sites therefore never touch SPI or FAT, which matters because the main loop's PPS/NMEA pairing is sensitive to exactly the kind of stall a synchronous card write would cause. If no card is mounted the writer is not started and the queue simply saturates at its cap. When a card is mounted, `sdlog::start_http_server()` also brings up a read-only `EspHttpServer` on port 80 serving `/log` and `/log/1` as `text/plain`; downloads hold a `CARD_LOCK` that the writer checks with `try_lock`, deferring its flush rather than appending mid-transfer.

---

### `wifi` – Wi-Fi STA

Uses `esp_idf_svc::wifi::EspWifi` to connect to a WPA2 access point. Credentials are injected at compile time via environment variables (`WIFI_SSID`, `WIFI_PASS`) and stored in the firmware binary in plaintext. The SSID (not the password) is logged at boot. See [`docs/setup.md`](setup.md#security-considerations) for deployment guidance.

After the initial blocking connect, the driver is wrapped in `SharedWifi` (`Arc<Mutex<BlockingWifi<EspWifi>>>`) and shared with two consumers: the system-event-loop handler that re-issues a **non-blocking** `EspWifi::connect` on `StaDisconnected`, and the UI task, which reads the STA IP/gateway via `sta_netif().get_ip_info()` for the Network page. Lock holders must never wait on Wi-Fi events while holding the mutex, since the reconnect handler runs on the event task that delivers them.

---

## Pin Map

See [`docs/hardware.md`](hardware.md) for the canonical pin map, assembly notes, and bring-up checklist. GPIO assignments are defined as named constants in each peripheral module and claimed through [`pins::PinPool`] at init.

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
| `gps` | Checksum validation (including non-ASCII rejection without panicking), RMC/GGA parsing (fix flag cleared by a `V` status with empty position), coordinate conversion, timezone DST rules |
| `ntp` | Packet field layout (stratum 0 + LI=3 + `INIT` when unsynchronised), anchor establishment, PPS discipline (frequency-estimate convergence and boundedness, integer-second accounting across missed/rejected pulses, PPS outages, a leap second, and a late-parsed backlog sentence that must not step the anchor), holdover policy (stratum 1 through the 33 min window, 16 after; PPS-locked anchor required), mode dispatch (mode 3→4, 1→2, others discarded; short packets and mode-6 responses dropped), mode-6 responses (READSTAT summary and per-association, READVAR, error status words, two-bit leap text), EWMA smoothing, bit-exact timestamp math under a frozen test clock (epoch offset, fraction encoding, frequency correction, `clock=` text), sync state transitions (Locked/Holdover/Unsync cycle), mode-6 framing and 32-bit alignment, originate-timestamp echo, VN mirroring and clamping |
| `ntp::protection` | ACL CIDR matching, rate-limiter interval boundary, LRU eviction (evicted client re-admitted, survivors still limited) |
| `pps` | Delta computation, u64 wraparound safety, First/Delta event sequencing, coalescing of multiple edges between polls, seqlock consistency under a concurrent writer |
| `rtc` | BCD codec, register encode/decode round trip (PCF8523 register order), range validation, Unix conversions |
| `timezone` | JSON field extraction for Open-Meteo and GeoNames response shapes |
| `battery` | MAX17048 and LC709203 register decoding arithmetic |

Tests live beside their module as `src/<module>/tests.rs` (declared with `#[cfg(test)] mod tests;`) rather than inline, so the coverage gate in `just coverage` can exclude them (`--ignore-filename-regex 'src/main\.rs$|/tests\.rs$'`) and measure only production code. The `ntp` tests freeze the monotonic clock through a thread-local override (`test_clock::freeze`) where exact arithmetic matters; everything else uses the real clock with edge timestamps placed explicitly in the past.

ESP-IDF target modules (`app`, `board`, `display`, `pins`, `storage`, `wifi`, `ui_task`, `logging`) are excluded from host builds via `#[cfg(target_os = "espidf")]`.

### Unsafe code policy

All `unsafe` in the firmware is FFI into ESP-IDF where `esp-idf-svc`/`esp-idf-hal` offer no safe wrapper (e.g. `esp_timer_get_time` for the ISR-shared timebase, `esp_wifi_set_ps`, heap/partition statistics, `subscribe_nonstatic` for the PPS ISR). Where a safe API exists it is used instead (netif IP info, `EspWifi::connect`, SPI device CS). `Cargo.toml` enforces this via `[lints]`: `clippy::undocumented_unsafe_blocks = "deny"` (every block needs a `// SAFETY:` comment), `clippy::multiple_unsafe_ops_per_block = "warn"` (one FFI call per block so each comment justifies exactly one operation), and `rust::unsafe_op_in_unsafe_fn = "warn"`. Run `cargo +esp clippy` to check the ESP-only modules, since host clippy never sees them.

## NTP Client Interoperability

Client-specific compatibility notes — iburst KoD behaviour, `maxdistance`
holdover windows, mode-6 diagnostics, and a 7-step on-device validation
checklist — are documented in [`docs/interop.md`](interop.md).

Clients tested and confirmed compatible: `ntpd` (ISC 4.2.x), `chronyd`
(4.x), `systemd-timesyncd` (systemd ≥ 250), and `ntpsec` (1.x).
