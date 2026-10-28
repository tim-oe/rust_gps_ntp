# ESP32 Rust Setup

This project uses `esp-idf-svc` and is currently configured for the connected
ESP32-S3 board on `/dev/ttyACM0`.

## Official documentation

- Rust install docs: [Rust Installation](https://www.rust-lang.org/tools/install)
- Espressif Rust docs: [The Rust on ESP Book](https://docs.esp-rs.org/book/)

## 1) Install host prerequisites (Linux)

Install native toolchain + Python virtualenv support used by `esp-idf-sys`:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config libssl-dev cmake git \
  python3-venv python3-pip
```

Optional but useful for local development:

```bash
sudo apt-get install -y ripgrep
```

## 2) Install Rust + ESP toolchain

Install Rust if needed, then install Espressif tooling:

```bash
curl https://sh.rustup.rs -sSf | sh
source "$HOME/.cargo/env"
cargo install espup
espup install
source "$HOME/export-esp.sh"
```

Verify Rust + ESP toolchains are installed:

```bash
rustup toolchain list
```

Expected output should include both `stable-...` and `esp`.
Note: your default active toolchain may still be `stable`; this project uses
`cargo +esp ...` for ESP builds/flash operations.

Persist ESP environment setup for new shells:

```bash
cat <<'EOF' >> ~/.bashrc
if [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
if [ -f "$HOME/.cargo/export-esp.sh" ]; then
  . "$HOME/.cargo/export-esp.sh"
fi
EOF
source ~/.bashrc
```

## 3) Install flashing tools

```bash
cargo install espflash cargo-espflash
cargo install ldproxy
```

Install `just` task runner:

```bash
cargo install just
```

Quick verification:

```bash
espup --version
espflash --version
cargo espflash --version
just --version
```

## 4) Confirm serial device

Your board is expected at:

```bash
ls -l /dev/ttyACM0
```

If permissions fail, add your user to `dialout` and re-login:

```bash
sudo usermod -aG dialout "$USER"
```

## 5) Build / flash / monitor

### Build memory

A firmware compile can peak above 4 GB of RAM. Cargo's default is one `rustc` per core, and the ESP-IDF C build defaults to the same, so a 4-core Pi runs several compilers at once. That total is what crosses 4 GB. One release compile of `chrono-tz` (the IANA timezone database) is about 600 MB; it is not the whole spike by itself.

On a 4 GB Pi with no swap, the kernel's OOM killer has nowhere to page memory out, so the board locks instead of slowing down.

The project keeps the compile inside physical RAM in two places:

- `.cargo/config.toml` sets `jobs = 1`, plus `NUM_JOBS` and `CMAKE_BUILD_PARALLEL_LEVEL` for the ESP-IDF C build. A plain `cargo` invocation stays at one compiler. On a Pi, ESP-IDF uses Makefiles and shares Cargo's job count; `NUM_JOBS` covers the Ninja path.
- `just` reads `MemTotal` and raises that floor when the machine can take it: 1 job on a 4 GB Pi, 3 on an 8 GB Pi, otherwise the core count. The same value is exported as `CARGO_BUILD_JOBS`, `NUM_JOBS`, and `CMAKE_BUILD_PARALLEL_LEVEL`.

To use every core when you know you have the RAM and swap:

```bash
CARGO_BUILD_JOBS=4 NUM_JOBS=4 just flash
```

A swap file is still worth having on a 4 GB Pi. One compiler plus a desktop session can spike, and with no swap the board locks instead of paging:

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
grep -q '/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

Set Wi-Fi station credentials in your shell before flashing:

```bash
export WIFI_SSID="your-ssid"
export WIFI_PASS="your-password"
# Optional: named timezone for rule-accurate DST conversion
export LOCAL_TZ="America/Chicago"
```

These env vars are compile-time inputs (captured during build/flash), not runtime
shell variables on the board. Current firmware uses these env-based STA credentials
at boot (credential persistence is disabled for now), while still using default NVS
internally for ESP-IDF Wi-Fi operation.

`LOCAL_TZ` is optional. When set, GPS UTC->local conversion uses that named
timezone (including DST rules). When unset, firmware falls back to geodetermined
offset from longitude.

When `LOCAL_TZ` is unset and Wi-Fi is connected, firmware also attempts to
resolve an IANA timezone from GPS coordinates via an online API and caches the
result in NVS for future boots. Cached timezone is used immediately at boot and
then refreshed periodically from network lookup; if the resolved zone changes,
the new value is applied and persisted.

### Security considerations

**Wi-Fi credentials (compile-time).** `WIFI_SSID` and `WIFI_PASS` are embedded
in the firmware image at build time via `option_env!`. Anyone with access to
the flash binary can recover them. This is acceptable for a single-purpose LAN
appliance but not for devices deployed outside a trusted network. Only the
SSID (never the password) is logged at boot.

**Timezone lookups (HTTPS).** When `LOCAL_TZ` is unset, coordinate lookups use
HTTPS against Open-Meteo and GeoNames (`secure.geonames.org`). The IANA name is
validated with `Tz::from_str` before use; a MITM could influence display local
time only, not NTP discipline.

There are two flash workflows, both always run the coverage gate first:

**First flash or after changing `partitions.csv`** — writes bootloader, partition table, and app:

```bash
just flash          # flash only
just flash-monitor  # flash then attach monitor
```

**Iterative code deployment** — writes app partition only; use after the first full flash:

```bash
just flash-app          # flash only
just flash-app-monitor  # flash then attach monitor
```

All flash recipes use `--release` builds and `partitions.csv`, which configures a larger factory app slot for ESP32-S3.

Other useful tasks:

```bash
just fmt
just lint
just test
just coverage
just ci
just ci-no-esp
```

## 6) Test and coverage reporting

Host tests and coverage run on your Linux host target (not xtensa), which is
what we want for pure-logic modules like GPS parsing.

Install coverage tooling once:

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov
```

Run tests:

```bash
just test
```

Coverage enforces **90%** minimums on lines, functions, and regions:

```bash
just coverage

# fmt + clippy + coverage (same as ci-no-esp)
just ci-no-esp
```

`just flash` and `just flash-monitor` run `just coverage` before the ESP build.

Minimums are defined in `justfile` (`coverage_min_*`, currently **90%**). Host `main.rs` is excluded from coverage totals.

Outputs:

- HTML report: `target/llvm-cov/html/index.html`
- LCOV file: `target/llvm-cov/lcov.info`

## 7) Logging configuration

The project uses `src/logging.rs` to centralize logger initialization and optional
per-module log-level overrides.

Default behavior:

- global ESP-IDF log level is `INFO` via `sdkconfig.defaults`
- ESP-IDF maximum log level is `VERBOSE` so per-module runtime overrides can
  raise a module to `debug`/`trace`
- module logs use the Rust module path (for example `rust_gps_ntp::battery`)

Optional log overrides (either build-time env vars, or keys in `sdkconfig.defaults`):

- `LOG_WIFI_LEVEL`
- `LOG_GPS_LEVEL`
- `LOG_DISPLAY_LEVEL`
- `LOG_BATTERY_LEVEL`
- `LOG_NTP_LEVEL`
- `LOG_PPS_LEVEL` (applies to crate-root/main logs, including PPS loop logs)

Accepted values: `none`, `error`, `warn`, `info`, `debug`, `trace` (`verbose` alias is also accepted).

Example (battery debug only):

```bash
export LOG_BATTERY_LEVEL=debug
just flash-monitor
```

These are compile-time inputs (same pattern as `WIFI_SSID`/`WIFI_PASS`), so
re-flash after changing them.

### microSD log mirror

When a card is mounted, every `info`/`warn`/`error` line also lands in
`/sdcard/gps-ntp.log` (rotating to `gps-ntp.1.log` at 8 MiB), prefixed with the
same boot-relative millisecond stamp the serial console shows. This is the way
to capture overnight or unattended runs without a serial cable: let the device
run, power it down, and read the card on a PC, or fetch it over the LAN:

```bash
curl -o gps-ntp.log  http://gps-ntp/log      # current file
curl -o gps-ntp.1.log http://gps-ntp/log/1   # rotated file, if any
```

`http://gps-ntp/files` lists the card root in a browser with a delete button
per file (top-level files only; directories and the ready marker are never
deletable), for clearing out leftovers from a card's previous life. The same
operations are available from the shell as `just sd-ls`, `just sd-rm <name>`
and `just sd-log`.

The HTTP endpoints are unauthenticated (they only exist when a card is
mounted); treat them as having the same trust boundary as the NTP service,
since the log includes the STA SSID and client IP addresses and anyone on the
LAN can delete card files. Flushes happen
every 5 s, so at most the last few seconds are lost on a power pull. If any
single card call (a flush, a free-space read, or one HTTP chunk) does not
return within 180 seconds, the firmware restarts. A stuck SPI transfer cannot
be cancelled without abandoning the bus lock, so the chip reset is the
recovery.
Debug/trace lines are never written to the card even when a module is raised
to `debug`; use the serial console for those.

Example (`sdkconfig.defaults`):

```text
LOG_GPS_LEVEL=trace
```

Display boot test behavior:

- runs once at startup only when effective display log level is `debug` or `trace/verbose`
- does not run at default `info`

## 8) Verify boot logs

You should see:

- Wi-Fi credential source (`env` update or existing NVS)
- STA connection success
- assigned DHCP address, e.g. `Wi-Fi connected; STA IP: 192.168.1.42`
- boot message indicating Wi-Fi + GPS UART diagnostics mode
- TFT page output with button page-toggle and 15-second auto-blank/wake behavior

## 9) Verify NTP service

After `just flash-monitor`, wait until Wi-Fi is connected and GPS/PPS are active,
then test from another host on the same network:

```bash
ntpdate -q <esp_ip>
ntpq -pnu <esp_ip>
```

Expected healthy behavior:

- `ntpdate -q` returns current date/time (not epoch/1970), with `s1 no-leap`
- `ntpq -pnu` shows one selected source similar to `*GPS`
- `stratum` is `1` once GPS fix + PPS lock are established
- `reach` converges to `377` (octal), indicating stable recent replies
- `offset`/`jitter` are small non-zero values (typically in microseconds)

Example:

```text
remote           refid      st t when poll reach   delay   offset   jitter
===============================================================================
*GPS             .GPS.            1 u    -   64  377      1us      5us      4us
```

If NTP is not healthy:

- If `ntpdate -q` shows 1970/epoch time, GPS UTC has not been accepted yet
- If `ntpq` times out, verify port `123/udp` reachability and that the board is online
- If `stratum` stays `16`, the server is still unsynchronized (no valid GPS fix/PPS lock)
