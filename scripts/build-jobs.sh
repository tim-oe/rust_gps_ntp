#!/bin/bash
# Print how many compilers to run at once so a firmware build stays inside
# physical RAM.
#
# Cargo defaults to one rustc per core. On a 4-core Pi those rustc processes
# stack with the ESP-IDF C build and the total crosses 4GB. With no swap the
# OOM killer locks the board instead of paging.
#
# A single release rustc of chrono-tz (the IANA timezone database) peaks
# around 600MB on a Pi 5. The overflow is the parallel jobs, not that crate.
#
# Budget: ~2GB per compiler, 1.5GB left for the OS.
#   4GB Pi (MemTotal is a bit under 4096) -> 1
#   8GB Pi -> 3
#   larger -> the core count
#
# BUILD_MEM_TOTAL_KB overrides /proc/meminfo (for checks).
# CARGO_BUILD_JOBS in the environment still wins; `just` applies that first.

set -euo pipefail

mem_kb="${BUILD_MEM_TOTAL_KB:-$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo)}"
mem_mb=$((mem_kb / 1024))
cpus=$(nproc)

reserve_mb=1536
per_job_mb=2048
jobs=$(((mem_mb - reserve_mb) / per_job_mb))
if ((jobs < 1)); then
    jobs=1
fi
if ((jobs > cpus)); then
    jobs=$cpus
fi

printf '%s\n' "$jobs"
