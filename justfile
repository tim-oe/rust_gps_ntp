set shell := ["bash", "-cu"]

port := "/dev/ttyACM0"
chip := "esp32s3"
esp_export := "$HOME/export-esp.sh"
partition_table := "partitions.csv"

default:
    @just --list

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test

check:
    cargo check

flash:
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}}

monitor:
    source {{esp_export}} && cargo +esp espflash monitor --chip {{chip}} --port {{port}}

flash-monitor:
    source {{esp_export}} && cargo +esp espflash flash --release --chip {{chip}} --port {{port}} --partition-table {{partition_table}} --monitor

ci: fmt-check lint test
