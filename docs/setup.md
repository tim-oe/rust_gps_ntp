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
just ci
```

## 6) Verify boot logs

You should see:

- Wi-Fi credential source (`env` update or existing NVS)
- STA connection success
- assigned DHCP address, e.g. `Wi-Fi connected; STA IP: 192.168.1.42`
- boot message indicating Wi-Fi + GPS UART diagnostics mode
- raw NMEA logs when GPS serial data is present, e.g. `GPS NMEA: $GPRMC,...`
- parsed UTC display from RMC sentences, e.g. `GPS UTC: 2026-06-04 02:35:01`
- TFT page output with button page-toggle and 15-second auto-blank/wake behavior

## 7) Next firmware milestone

Implement in this order:

1. GPS UART read and NMEA parsing
2. PPS interrupt timestamping
3. UDP port 123 NTP response generation
