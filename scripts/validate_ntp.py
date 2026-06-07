#!/usr/bin/env python3
"""
validate_ntp.py — query a GPS-NTP device and compare its time against
well-known reference servers using raw UDP NTP packets (no extra packages).

Exit codes:
  0  all checks passed
  1  device offset exceeds tolerance vs references
  2  device unreachable or returned invalid stratum

Usage:
  python3 validate_ntp.py [DEVICE] [--ref HOST ...] [--no-defaults] [--tolerance MS]

  DEVICE          hostname or IP of the device under test (default: gps-ntp)
  --ref HOST      add an extra reference server (repeatable, stacks with defaults)
  --no-defaults   ignore the built-in reference list; only use --ref servers
  --tolerance MS  max allowed offset divergence in ms (default: 100)

Example:
  python3 validate_ntp.py gps-ntp \\
      --ref time.cloudflare.com --ref time.google.com

  python3 validate_ntp.py gps-ntp \\
      --no-defaults --ref ntp.ubuntu.com --tolerance 50
"""

from __future__ import annotations

import argparse
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ntp_common import LEAP_LABELS, MIN_POLL_INTERVAL_S, query_ntp

DEFAULT_DEVICE = "gps-ntp"
DEFAULT_REFS = [
    "time.nist.gov",
    "time.google.com",
    "time.apple.com",
]
SAMPLES = 3
SAMPLE_INTERVAL_S = MIN_POLL_INTERVAL_S


def measure(host: str, samples: int = SAMPLES) -> dict | None:
    """Return median measurement over `samples` queries, or None on failure."""
    results = []
    for _ in range(samples):
        try:
            results.append(query_ntp(host))
            time.sleep(SAMPLE_INTERVAL_S)
        except Exception as exc:
            print(f"  [warn] {host}: {exc}")
    if not results:
        return None
    offsets = sorted(r["offset_ms"] for r in results)
    best = results[len(results) // 2]
    best["offset_ms"] = statistics.median(offsets)
    return best


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("device", nargs="?", default=DEFAULT_DEVICE)
    parser.add_argument(
        "--ref",
        dest="extra_refs",
        action="append",
        default=[],
        metavar="HOST",
        help="add an extra reference server (repeatable, stacks with defaults)",
    )
    parser.add_argument(
        "--no-defaults",
        action="store_true",
        help="drop the built-in reference list; use only servers given via --ref",
    )
    parser.add_argument(
        "--tolerance",
        type=float,
        default=100.0,
        metavar="MS",
        help="max allowed device offset vs reference median (default: 100 ms)",
    )
    args = parser.parse_args()

    refs = ([] if args.no_defaults else DEFAULT_REFS) + args.extra_refs
    if not refs:
        parser.error("no reference servers — either keep defaults or supply at least one --ref HOST")

    all_hosts = [args.device] + refs
    results: dict[str, dict | None] = {}

    print(f"\nQuerying {len(all_hosts)} NTP hosts ({SAMPLES} samples each) …\n")
    print(f"  {'host':<40} {'stratum':>7} {'ref-id':>8} {'delay ms':>9} {'offset ms':>10} {'leap':>8}")
    print(f"  {'-'*40} {'-'*7} {'-'*8} {'-'*9} {'-'*10} {'-'*8}")

    for host in all_hosts:
        tag = " [device]" if host == args.device else ""
        r = measure(host)
        results[host] = r
        if r is None:
            print(f"  {host:<40} UNREACHABLE{tag}")
        else:
            print(
                f"  {host:<40} {r['stratum']:>7} {r['ref_id']:>8}"
                f" {r['delay_ms']:>9.2f} {r['offset_ms']:>10.3f}"
                f" {LEAP_LABELS.get(r['leap'], '?'):>8}{tag}"
            )

    print()

    dev = results[args.device]
    if dev is None:
        print(f"FAIL  device '{args.device}' is unreachable")
        return 2

    if dev["stratum"] != 1:
        print(f"FAIL  device stratum={dev['stratum']} (expected 1 for GPS-disciplined)")
        return 2
    print(f"PASS  stratum=1, ref-id={dev['ref_id']!r}")

    if dev["leap"] == 3:
        print("FAIL  device leap-indicator=3 (unsynchronised)")
        return 2
    print(f"PASS  leap-indicator={dev['leap']} ({LEAP_LABELS[dev['leap']]})")

    ref_offsets = [results[h]["offset_ms"] for h in refs if results[h] is not None]
    if not ref_offsets:
        print("WARN  no reference servers reachable — skipping offset comparison")
        return 0

    ref_median = statistics.median(ref_offsets)
    divergence = abs(dev["offset_ms"] - ref_median)
    print(
        f"      device offset {dev['offset_ms']:+.3f} ms,"
        f" reference median {ref_median:+.3f} ms,"
        f" divergence {divergence:.3f} ms"
    )
    if divergence > args.tolerance:
        print(
            f"FAIL  divergence {divergence:.1f} ms exceeds tolerance {args.tolerance:.0f} ms"
        )
        return 1

    print(
        f"PASS  divergence {divergence:.1f} ms within tolerance {args.tolerance:.0f} ms"
    )
    print()
    return 0


if __name__ == "__main__":
    sys.argv = [sys.argv[0]] + [a for a in sys.argv[1:] if a != "--"]
    sys.exit(main())
