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
if [ -f "$HOME/export-esp.sh" ]; then
  . "$HOME/export-esp.sh"
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

Set Wi-Fi station credentials in your shell before flashing:

```bash
export WIFI_SSID="your-ssid"
export WIFI_PASS="your-password"
```

These env vars are compile-time inputs (captured during build/flash), not runtime
shell variables on the board. Current firmware uses these env-based STA credentials
at boot (credential persistence is disabled for now), while still using default NVS
internally for ESP-IDF Wi-Fi operation.

From repo root:

```bash
just flash-monitor
```

`just flash` and `just flash-monitor` use:

- `--release` builds (smaller binary)
- custom `partitions.csv` with a larger factory app slot for ESP32-S3

Alternative (uses runner from `.cargo/config.toml`):

```bash
cargo +esp run
```

Other useful tasks:

```bash
just fmt
just lint
just test
just test-host
just coverage
just coverage-html
just coverage-lcov
just ci-esp
just ci
```

Note: `just flash` and `just flash-monitor` now run an ESP compile check
(`just check-esp`) before flashing.

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
just test-host
```

Generate coverage reports:

```bash
# terminal summary
just coverage

# HTML report (open target/llvm-cov/html/index.html)
just coverage-html

# LCOV file for CI tools (Codecov/Sonar/etc.)
just coverage-lcov
```

Outputs:

- HTML report directory: `target/llvm-cov/html/`
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
