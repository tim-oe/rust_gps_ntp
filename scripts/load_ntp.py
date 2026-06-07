#!/usr/bin/env python3
"""
load_ntp.py — sustained NTP load against a GPS-NTP device.

Respects the device's 2 s per-client rate limiter.  Use multiple workers
(processes or Docker containers) with distinct source IPs for true multi-client
load; a single host IP is always one client from the server's perspective.

Exit codes:
  0  run completed (may include some query failures; see summary)
  1  all queries failed

Usage:
  python3 scripts/load_ntp.py [DEVICE] [options]

  DEVICE              hostname or IP (default: gps-ntp)
  --duration SEC      wall-clock run time (default: 300)
  --interval SEC      seconds between queries per worker (default: 2.5)
  --workers N         parallel worker processes on this host (default: 1)
  --bind-ip ADDR      source IPv4 for worker 0 (repeatable; one per worker)
  --json              emit machine-readable summary on stdout

Docker (multi-client, distinct LAN IPs):
  just load-test-docker DEVICE=gps-ntp CLIENTS=8 DURATION=300

Examples:
  python3 scripts/load_ntp.py gps-ntp --duration 60
  python3 scripts/load_ntp.py 192.168.1.48 --workers 4 --bind-ip 192.168.1.201 \\
      --bind-ip 192.168.1.202 --bind-ip 192.168.1.203 --bind-ip 192.168.1.204
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import os
import socket
import sys
import time
from dataclasses import dataclass, field

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ntp_common import MIN_POLL_INTERVAL_S, percentile, query_ntp


DEFAULT_DEVICE = "gps-ntp"


@dataclass
class WorkerStats:
    worker_id: int
    bind_ip: str | None
    ok: int = 0
    kod: int = 0
    errors: int = 0
    delays_ms: list[float] = field(default_factory=list)
    offsets_ms: list[float] = field(default_factory=list)


def worker_main(
    worker_id: int,
    device: str,
    duration_s: float,
    interval_s: float,
    bind_ip: str | None,
    result_queue: mp.Queue,
) -> None:
    stats = WorkerStats(worker_id=worker_id, bind_ip=bind_ip)
    deadline = time.monotonic() + duration_s
    label = bind_ip or socket.gethostname()

    while time.monotonic() < deadline:
        try:
            result = query_ntp(device, bind_addr=bind_ip)
            stats.ok += 1
            stats.delays_ms.append(result["delay_ms"])
            stats.offsets_ms.append(result["offset_ms"])
        except ValueError as exc:
            if "KoD" in str(exc):
                stats.kod += 1
            else:
                stats.errors += 1
                print(f"[worker {worker_id} @{label}] {exc}", file=sys.stderr)
        except OSError as exc:
            stats.errors += 1
            print(f"[worker {worker_id} @{label}] {exc}", file=sys.stderr)

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        time.sleep(min(interval_s, remaining))

    result_queue.put(stats)


def merge_stats(workers: list[WorkerStats]) -> dict:
    all_delays: list[float] = []
    all_offsets: list[float] = []
    ok = kod = errors = 0
    for w in workers:
        ok += w.ok
        kod += w.kod
        errors += w.errors
        all_delays.extend(w.delays_ms)
        all_offsets.extend(w.offsets_ms)

    all_delays.sort()
    all_offsets.sort()
    total = ok + kod + errors

    return {
        "queries_total": total,
        "ok": ok,
        "kod_rate": kod,
        "errors": errors,
        "workers": len(workers),
        "delay_ms": {
            "p50": percentile(all_delays, 50),
            "p95": percentile(all_delays, 95),
            "p99": percentile(all_delays, 99),
            "max": max(all_delays) if all_delays else float("nan"),
        },
        "offset_ms": {
            "p50": percentile(all_offsets, 50),
            "p95": percentile(all_offsets, 95),
            "p99": percentile(all_offsets, 99),
        },
        "per_worker": [
            {
                "id": w.worker_id,
                "bind_ip": w.bind_ip,
                "ok": w.ok,
                "kod_rate": w.kod,
                "errors": w.errors,
            }
            for w in workers
        ],
    }


def print_summary(device: str, duration_s: float, interval_s: float, summary: dict) -> None:
    d = summary["delay_ms"]
    o = summary["offset_ms"]
    print(f"\nLoad test summary — {device}")
    print(f"  duration: {duration_s:.0f} s   interval: {interval_s:.1f} s   workers: {summary['workers']}")
    print(
        f"  queries: {summary['queries_total']} total"
        f"  ({summary['ok']} ok, {summary['kod_rate']} KoD, {summary['errors']} err)"
    )
    if summary["ok"]:
        print(
            f"  delay ms   p50={d['p50']:.2f}  p95={d['p95']:.2f}"
            f"  p99={d['p99']:.2f}  max={d['max']:.2f}"
        )
        print(
            f"  offset ms  p50={o['p50']:.3f}  p95={o['p95']:.3f}  p99={o['p99']:.3f}"
        )
    if summary["kod_rate"]:
        print(
            "  note: KoD RATE responses mean a worker polled faster than the"
            f" {MIN_POLL_INTERVAL_S:.1f} s device limit — raise --interval"
        )
    print()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("device", nargs="?", default=DEFAULT_DEVICE)
    parser.add_argument("--duration", type=float, default=300.0, metavar="SEC")
    parser.add_argument("--interval", type=float, default=MIN_POLL_INTERVAL_S, metavar="SEC")
    parser.add_argument("--workers", type=int, default=1, metavar="N")
    parser.add_argument(
        "--bind-ip",
        dest="bind_ips",
        action="append",
        default=[],
        metavar="ADDR",
        help="source IPv4 for a worker (repeat for each worker)",
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    if args.interval < MIN_POLL_INTERVAL_S:
        print(
            f"warning: --interval {args.interval} < {MIN_POLL_INTERVAL_S} s;"
            " expect KoD RATE responses",
            file=sys.stderr,
        )

    workers = max(1, args.workers)
    bind_ips: list[str | None] = list(args.bind_ips)
    while len(bind_ips) < workers:
        bind_ips.append(None)
    if len(bind_ips) > workers:
        print("warning: extra --bind-ip values ignored", file=sys.stderr)
        bind_ips = bind_ips[:workers]

    ctx = mp.get_context("spawn")
    result_queue: mp.Queue = ctx.Queue()
    processes: list[mp.Process] = []

    for i in range(workers):
        proc = ctx.Process(
            target=worker_main,
            args=(i, args.device, args.duration, args.interval, bind_ips[i], result_queue),
            name=f"ntp-load-{i}",
        )
        proc.start()
        processes.append(proc)

    worker_stats: list[WorkerStats] = []
    for _ in processes:
        worker_stats.append(result_queue.get())

    for proc in processes:
        proc.join()

    summary = merge_stats(worker_stats)
    summary["device"] = args.device
    summary["duration_s"] = args.duration
    summary["interval_s"] = args.interval

    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print_summary(args.device, args.duration, args.interval, summary)

    return 0 if summary["ok"] > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
