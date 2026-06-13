set shell := ["bash", "-cu"]

port := "/dev/ttyACM0"
chip := "esp32s3"
esp_export := "$HOME/.cargo/export-esp.sh"
partition_table := "partitions.csv"
host_target := `rustc -vV | sed -n 's/^host: //p'`
DEFAULT_DEVICE := "gps-ntp"

# Host coverage minimums (project target: 90% on host-testable lib code).
# `main.rs` is excluded (host stub); refresh baselines with `just coverage`.
coverage_min_lines := "90"
coverage_min_functions := "90"
coverage_min_regions := "90"

coverage_llvm_cov_run := "cargo llvm-cov --target " + host_target + " --no-report"
coverage_llvm_cov_report := "cargo llvm-cov report --target " + host_target + " --ignore-filename-regex 'src/main\\.rs$'"
coverage_llvm_cov_gate := coverage_llvm_cov_report + " --summary-only --fail-under-lines " + coverage_min_lines + " --fail-under-functions " + coverage_min_functions + " --fail-under-regions " + coverage_min_regions

# List available recipes.
default:
    @just --list

# Auto-format all Rust source files.
fmt:
    cargo fmt --all

# Check formatting without modifying files (used in CI).
fmt-check:
    cargo fmt --all -- --check

# Run Clippy on host target; treats warnings as errors.
lint:
    cargo clippy --target {{host_target}} --all-targets --all-features -- -D warnings

# Run unit tests on host target (ESP modules are excluded via cfg).
test:
    cargo test --target {{host_target}}

# Run tests with coverage; enforces 90% minimums and writes HTML + LCOV reports.
coverage:
    mkdir -p target/llvm-cov
    {{coverage_llvm_cov_run}}
    {{coverage_llvm_cov_gate}}
    {{coverage_llvm_cov_report}} --html
    {{coverage_llvm_cov_report}} --lcov --output-path target/llvm-cov/lcov.info

# Type-check the ESP32 firmware target (requires export-esp.sh).
check:
    source {{esp_export}} && cargo +esp check

# Deploy app partition only — runs full CI gate then flashes app; use after first full flash.
flash-app:
    just ci
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

# Deploy app partition only and attach serial monitor.
flash-app-monitor:
    just ci
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

# Full flash (bootloader + partition table + app); run after partitions.csv changes or first flash.
flash:
    just ci
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

# Full flash and attach serial monitor.
flash-monitor:
    just ci
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

# Attach serial monitor to the connected board without flashing.
monitor:
    source {{esp_export}} && cargo +esp espflash monitor --chip {{chip}} --port {{port}}

# Full firmware CI: fmt-check + lint + gated coverage + ESP target check. Requires export-esp.sh.
ci: fmt-check lint coverage check

# Host-only CI: fmt-check + lint + gated coverage. Works without ESP toolchain.
ci-no-esp: fmt-check lint coverage

# Query the live device and compare its time against well-known reference servers.
# Checks: stratum=1, leap-indicator not unsync, offset within --tolerance ms (default 100).
# Requires the device to be running on the network.
# Note: takes ~60 s (3 samples × 2.5 s gap × 4 hosts) to respect the device rate-limiter.
#
#   just validate-ntp                                    # default device + 3 built-in refs
#   just validate-ntp gps-ntp.localdomain                # explicit hostname
#   just validate-ntp 192.168.1.48                       # by IP
#   just validate-ntp gps-ntp -- --tolerance 10          # tighter 10ms check
#   just validate-ntp gps-ntp -- --ref ntp.ubuntu.com    # add an extra reference server
#   just validate-ntp gps-ntp -- --no-defaults --ref ntp.ubuntu.com  # replace all refs
validate-ntp device=DEFAULT_DEVICE *flags="":
    python3 scripts/validate_ntp.py {{device}} {{flags}}

# Sustained NTP load from this host (single or multi-worker with --bind-ip).
# Respects the device's 2 s per-client rate limiter (default 2.5 s interval).
#
#   just load-test                                    # 5 min @ gps-ntp
#   just load-test 192.168.1.48 -- --duration 60
#   just load-test gps-ntp -- --workers 4 --bind-ip 192.168.1.201 --bind-ip 192.168.1.202
load-test device=DEFAULT_DEVICE *flags="":
    python3 scripts/load_ntp.py {{device}} {{flags}}

# Multi-client load via Docker macvlan (distinct 192.168.1.x per container).
# Requires MACVLAN_PARENT set to your wired LAN interface.
#
#   just load-test-docker CLIENTS=8 DURATION=300 MACVLAN_PARENT=eth0
load-test-docker device=DEFAULT_DEVICE clients="4" duration="300" interval="2.5" macvlan_parent="eth0":
    DEVICE={{device}} DURATION={{duration}} INTERVAL={{interval}} MACVLAN_PARENT={{macvlan_parent}} \
      docker compose -f docker/load/docker-compose.yml --profile macvlan up --scale ntp-client={{clients}}

# Host micro-benchmarks for GPS parse and NTP serve hot paths (Criterion HTML report).
bench:
    cargo bench --target {{host_target}} --features bench
