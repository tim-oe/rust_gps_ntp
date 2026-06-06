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

coverage_llvm_cov := "cargo llvm-cov --target " + host_target + " --ignore-filename-regex 'src/main\\.rs$' --fail-under-lines " + coverage_min_lines + " --fail-under-functions " + coverage_min_functions + " --fail-under-regions " + coverage_min_regions

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets --all-features -- -D warnings

lint-host:
    cargo clippy --target {{host_target}} --all-targets --all-features -- -D warnings

test: test-host

test-host:
    cargo test --target {{host_target}}

coverage:
    {{coverage_llvm_cov}} --summary-only

coverage-html:
    {{coverage_llvm_cov}} --html

coverage-gate:
    {{coverage_llvm_cov}} --summary-only

coverage-lcov:
    mkdir -p target/llvm-cov
    {{coverage_llvm_cov}} --lcov --output-path target/llvm-cov/lcov.info

check:
    cargo check

check-esp:
    source {{esp_export}} && cargo +esp check

flash:
    just coverage-gate
    just check-esp
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

flash-lib: flash

monitor:
    source {{esp_export}} && cargo +esp espflash monitor --chip {{chip}} --port {{port}}

flash-monitor:
    just coverage-gate
    just check-esp
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

# Host-only CI (works without ESP toolchain); default for local dev and GitHub Actions.
ci: fmt-check lint-host test-host

# Full firmware CI including ESP target check (requires export-esp.sh).
ci-esp: fmt-check lint-host test-host check-esp

ci-coverage: fmt-check lint-host coverage-lcov
