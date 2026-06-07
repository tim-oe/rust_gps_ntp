"""Shared NTP v4 client helpers for validate_ntp.py and load_ntp.py."""

from __future__ import annotations

import socket
import struct
import time

NTP_PORT = 123
NTP_EPOCH_DELTA = 2_208_988_800  # seconds between 1900-01-01 and 1970-01-01
TIMEOUT_S = 5
# Must exceed the device's MIN_POLL_INTERVAL (2 s) or clients receive KoD RATE.
MIN_POLL_INTERVAL_S = 2.5

LEAP_LABELS = {0: "no-leap", 1: "+1s", 2: "-1s", 3: "unsync"}


def ntp_timestamp_to_unix(seconds: int, fraction: int) -> float:
    return (seconds - NTP_EPOCH_DELTA) + fraction / 2**32


def query_ntp(host: str, bind_addr: str | None = None) -> dict:
    """
    Send one NTP v4 client request and return timing and header fields.

    Raises OSError on network failure, ValueError on malformed or KoD responses.
    """
    packet = bytearray(48)
    packet[0] = 0b00_100_011  # LI=0, VN=4, Mode=3 (client)

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        if bind_addr:
            s.bind((bind_addr, 0))
        s.settimeout(TIMEOUT_S)
        t1 = time.time()
        s.sendto(bytes(packet), (host, NTP_PORT))
        data, _ = s.recvfrom(1024)
        t4 = time.time()

    if len(data) < 48:
        raise ValueError(f"response too short ({len(data)} bytes)")

    fmt = "!BBbbII4sIIIIIIII"
    fields = struct.unpack(fmt, data[: struct.calcsize(fmt)])
    li_vn_mode, stratum, _poll, _prec, _rdel, _rdisp, ref_id_raw = fields[:7]
    _ref_s, _ref_f, _orig_s, _orig_f, recv_s, recv_f, xmt_s, xmt_f = fields[7:]

    leap = (li_vn_mode >> 6) & 0x3

    if stratum == 0:
        kiss = ref_id_raw.decode("ascii", errors="replace").rstrip("\x00")
        raise ValueError(f"KoD response: kiss-code={kiss!r}")

    t2 = ntp_timestamp_to_unix(recv_s, recv_f)
    t3 = ntp_timestamp_to_unix(xmt_s, xmt_f)

    offset_s = ((t2 - t1) + (t3 - t4)) / 2
    delay_s = (t4 - t1) - (t3 - t2)

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


def percentile(sorted_values: list[float], pct: float) -> float:
    """Linear-interpolation percentile on a pre-sorted list."""
    if not sorted_values:
        return float("nan")
    if len(sorted_values) == 1:
        return sorted_values[0]
    rank = (pct / 100.0) * (len(sorted_values) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(sorted_values) - 1)
    weight = rank - lo
    return sorted_values[lo] * (1.0 - weight) + sorted_values[hi] * weight
