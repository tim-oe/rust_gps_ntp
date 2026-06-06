set shell := ["bash", "-cu"]

port := "/dev/ttyACM0"
chip := "esp32s3"
esp_export := "$HOME/export-esp.sh"
partition_table := "partitions.csv"
host_target := `rustc -vV | sed -n 's/^host: //p'`

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

# Deploy app partition only — runs coverage gate then flashes app; use after first full flash.
flash-app:
    just coverage
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

# Deploy app partition only and attach serial monitor.
flash-app-monitor:
    just coverage
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

# Full flash (bootloader + partition table + app); run after partitions.csv changes or first flash.
flash:
    just coverage
    just check
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

# Full flash and attach serial monitor.
flash-monitor:
    just coverage
    just check
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

# Attach serial monitor to the connected board without flashing.
monitor:
    source {{esp_export}} && cargo +esp espflash monitor --chip {{chip}} --port {{port}}

# Full firmware CI: fmt-check + lint + gated coverage + ESP target check. Requires export-esp.sh.
ci: fmt-check lint coverage check

# Host-only CI: fmt-check + lint + gated coverage. Works without ESP toolchain.
ci-no-esp: fmt-check lint coverage
