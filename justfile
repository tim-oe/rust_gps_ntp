set shell := ["bash", "-cu"]

port := "/dev/ttyACM0"
chip := "esp32s3"
esp_export := "$HOME/export-esp.sh"
partition_table := "partitions.csv"
host_target := `rustc -vV | sed -n 's/^host: //p'`

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
    cargo llvm-cov --target {{host_target}} --summary-only

coverage-html:
    cargo llvm-cov --target {{host_target}} --html

coverage-lcov:
    mkdir -p target/llvm-cov
    cargo llvm-cov --target {{host_target}} --lcov --output-path target/llvm-cov/lcov.info

check:
    cargo check

check-esp:
    source {{esp_export}} && cargo +esp check

flash:
    just check-esp
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

flash-lib: flash

monitor:
    source {{esp_export}} && cargo +esp espflash monitor --chip {{chip}} --port {{port}}

flash-monitor:
    just check-esp
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

ci: fmt-check lint test

ci-esp: fmt-check lint-host test-host check-esp

ci-coverage: fmt-check lint-host test-host coverage-lcov
