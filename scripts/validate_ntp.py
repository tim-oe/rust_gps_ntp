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
  python3 scripts/validate_ntp.py gps-ntp \\
      --ref time.cloudflare.com --ref time.google.com

  python3 scripts/validate_ntp.py gps-ntp \\
      --no-defaults --ref ntp.ubuntu.com --tolerance 50
"""

import socket
import struct
import sys
import time
import argparse
import statistics

NTP_PORT = 123
NTP_EPOCH_DELTA = 2_208_988_800  # seconds between 1900-01-01 and 1970-01-01
TIMEOUT_S = 5
SAMPLES = 3  # queries per host for a better median
# Interval between samples.  Must exceed the device's MIN_POLL_INTERVAL (2 s)
# or the device returns a KoD RATE response instead of a real reply.
SAMPLE_INTERVAL_S = 2.5

DEFAULT_DEVICE = "gps-ntp"
DEFAULT_REFS = [
    "time.nist.gov",
    "time.google.com",
    "time.apple.com",
]


def ntp_timestamp_to_unix(seconds: int, fraction: int) -> float:
    return (seconds - NTP_EPOCH_DELTA) + fraction / 2**32


def query_ntp(host: str) -> dict:
    """
    Send one NTP v4 client request and return a dict with:
      offset_ms, delay_ms, stratum, ref_id_str, leap

    Raises OSError on network failure, ValueError on malformed response.
    """
    packet = bytearray(48)
    packet[0] = 0b00_100_011  # LI=0, VN=4, Mode=3 (client)

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(TIMEOUT_S)
        t1 = time.time()
        s.sendto(bytes(packet), (host, NTP_PORT))
        data, _ = s.recvfrom(1024)
        t4 = time.time()

    if len(data) < 48:
        raise ValueError(f"response too short ({len(data)} bytes)")

    # Unpack header fields we care about.
    # Layout: B(LI/VN/Mode) B(stratum) b(poll) b(prec) I(root_delay) I(root_disp)
    #         4s(ref_id) II(ref_ts) II(orig_ts) II(recv_ts) II(xmt_ts)
    fmt = "!BBbbII4sIIIIIIII"
    fields = struct.unpack(fmt, data[: struct.calcsize(fmt)])
    li_vn_mode, stratum, _poll, _prec, _rdel, _rdisp, ref_id_raw = fields[:7]
    _ref_s, _ref_f, _orig_s, _orig_f, recv_s, recv_f, xmt_s, xmt_f = fields[7:]

    leap = (li_vn_mode >> 6) & 0x3

    # stratum=0 is a Kiss-o'-Death packet; ref_id is the kiss code ("RATE", "DENY", …).
    if stratum == 0:
        kiss = ref_id_raw.decode("ascii", errors="replace").rstrip("\x00")
        raise ValueError(f"KoD response: kiss-code={kiss!r}")

    t2 = ntp_timestamp_to_unix(recv_s, recv_f)
    t3 = ntp_timestamp_to_unix(xmt_s, xmt_f)

    # RFC 5905 offset and round-trip delay.
    offset_s = ((t2 - t1) + (t3 - t4)) / 2
    delay_s = (t4 - t1) - (t3 - t2)

    # Reference ID is ASCII for stratum-1 sources, dotted-quad for stratum-2+.
    if stratum <= 1:
        ref_id_str = ref_id_raw.decode("ascii", errors="replace").rstrip("\x00")
    else:
        ref_id_str = ".".join(str(b) for b in ref_id_raw)

    return {
        "offset_ms": offset_s * 1000,
        "delay_ms": delay_s * 1000,
        "stratum": stratum,
        "ref_id": ref_id_str,
        "leap": leap,
    }


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
    best = results[len(results) // 2]  # pick the median-offset sample
    best["offset_ms"] = statistics.median(offsets)
    return best


LEAP_LABELS = {0: "no-leap", 1: "+1s", 2: "-1s", 3: "unsync"}


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

    # ── Device reachability ──────────────────────────────────────────────────
    dev = results[args.device]
    if dev is None:
        print(f"FAIL  device '{args.device}' is unreachable")
        return 2

    # ── Stratum check ────────────────────────────────────────────────────────
    if dev["stratum"] != 1:
        print(f"FAIL  device stratum={dev['stratum']} (expected 1 for GPS-disciplined)")
        return 2
    print(f"PASS  stratum=1, ref-id={dev['ref_id']!r}")

    # ── Leap-indicator check ─────────────────────────────────────────────────
    if dev["leap"] == 3:
        print("FAIL  device leap-indicator=3 (unsynchronised)")
        return 2
    print(f"PASS  leap-indicator={dev['leap']} ({LEAP_LABELS[dev['leap']]})")

    # ── Offset vs reference median ───────────────────────────────────────────
    ref_offsets = [
        results[h]["offset_ms"] for h in refs if results[h] is not None
    ]
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
    # just(1) passes its own `--` separator through to the script when flags
    # like --ref are forwarded via `*flags`.  Strip it before argparse runs.
    sys.argv = [sys.argv[0]] + [a for a in sys.argv[1:] if a != "--"]
    sys.exit(main())
