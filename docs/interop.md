# NTP Client Interoperability Notes

This document records known compatibility observations for each mainstream NTP
client implementation when used against the `rust_gps_ntp` GPS+PPS stratum-1
server.

## Reference specifications

| Spec | Relevance |
|------|-----------|
| RFC 5905 §7.3 | Packet field layout and originate-timestamp echo |
| RFC 5905 §7.4 | Kiss-o'-Death responses |
| RFC 5905 §11  | Clock-filter and clock-select algorithm |
| RFC 1305 §3   | Mode-6 control protocol (`ntpq`) |

---

## `ntpd` (ISC reference implementation, ntp-4.2.x)

### Configuration

```conf
server <ip> prefer minpoll 6 maxpoll 10
```

Omit `iburst` (see note below) or configure `minpoll 4` to keep the burst
interval above the server's 2-second rate-limit floor.

### Known compatibility notes

| Area | Detail |
|------|--------|
| **iburst** | `ntpd` sends 8 back-to-back requests on startup, each ~1 s apart.  This trips the server's 2-second `MIN_POLL_INTERVAL_US` rate limiter and results in the first 7 requests receiving KoD `RATE` responses.  `ntpd` handles KoD correctly and backs off; synchronisation still converges, but initial lock-on takes ~30 s longer.  Workaround: omit `iburst`, or add `minpoll 4` so ntpd only fires two rapid probes. |
| **maxdistance** | Default `maxdist 1.5` (seconds).  Our root dispersion stays well below this during normal GPS-locked operation (< 1 ms).  During holdover, dispersion grows at `HOLDOVER_DISP_RATE_US_PER_SEC = 500 µs/s`; maxdistance is exceeded after ~50 minutes of holdover, at which point ntpd correctly stops using this server. |
| **Mode-6 (`ntpq`)** | `ntpq -pnu` and `ntpq -c rv` work correctly.  `READSTAT` returns association ID `1` with `sel=6` (system peer) when locked.  `READVAR` returns all standard system and peer variables.  The `frequency` field sign prefix (`+`/`-`) may differ from ntpd's canonical output but parses correctly. |
| **Root delay** | Reported as `0.000 ms` (hardware reference).  ntpd accepts zero root delay without issue. |
| **Stratum** | Server reports stratum 1 when locked; stratum 16 (unsynchronised) during holdover timeout or before first GPS fix.  ntpd discards stratum-16 responses as expected. |
| **NTPv3 clients** | Server mirrors the client's VN field.  NTPv3-configured ntpd peers work correctly. |

### Recommended `ntp.conf` snippet

```conf
# GPS+PPS stratum-1 appliance
server 192.168.1.100 prefer minpoll 6 maxpoll 10
```

---

## `chronyd` (Chrony, chrony-4.x)

### Configuration

```conf
server 192.168.1.100 prefer iburst
```

### Known compatibility notes

| Area | Detail |
|------|--------|
| **iburst** | Chrony's iburst sends 4 rapid probes ~2 s apart at startup.  The 2-second rate-limit threshold means the first probe from each burst window is allowed and subsequent ones may receive KoD `RATE`.  Chrony respects KoD and backs off to its normal polling interval.  Synchronisation converges normally within 1-2 poll cycles. |
| **maxdistance** | Default `maxdistance 3.0` seconds.  More permissive than ntpd; holdover is tolerated for up to ~100 minutes before chrony stops selecting this server. |
| **Reference timestamp** | Chrony checks that the reference timestamp advances between successive responses.  This server updates `last_sync_ntp_ts` on every successful PPS discipline event (once per second), so the reference timestamp advances normally. |
| **Mode-6 (`chronyc`)** | Chrony uses its own control socket rather than NTP mode-6, so `ntpq` diagnostics are not relevant here.  Use `chronyc tracking` and `chronyc sources -v` for monitoring. |
| **Leap second** | Chrony relies on the leap indicator in the NTP response.  Use `server.set_leap_indicator(1)` (warning: +1 s) the day before a positive leap second and reset to `0` the day after.  Advance notice comes from IERS Bulletin C (<https://www.iers.org/IERS/EN/Publications/Bulletins/bulletins.html>). |

### Recommended `chrony.conf` snippet

```conf
# GPS+PPS stratum-1 appliance
server 192.168.1.100 prefer iburst minpoll 6 maxpoll 10
```

---

## `systemd-timesyncd` (systemd-250+)

### Configuration (`/etc/systemd/timesyncd.conf`)

```ini
[Time]
NTP=192.168.1.100
FallbackNTP=pool.ntp.org
```

### Known compatibility notes

| Area | Detail |
|------|--------|
| **Polling** | timesyncd uses a single poll with exponential back-off; it does not send iburst bursts, so it is unaffected by the 2-second rate limit. |
| **Mode-6** | Not used; timesyncd is a simple SNTP-like client.  No `ntpq` diagnostics are available. |
| **Quality fields** | timesyncd is permissive about root delay and root dispersion values.  It accepts stratum 1-2 servers without checking dispersion thresholds. |
| **Leap second** | timesyncd reads the leap indicator from the NTP response and steps the clock at the leap-second boundary.  Same guidance as chronyd above: set LI=1 the day before a positive leap second. |
| **Overall** | Fully compatible out of the box; no special configuration required. |

---

## `ntpsec` (NTPsec-1.x)

### Configuration

```conf
server 192.168.1.100 prefer minpoll 6 maxpoll 10
```

### Known compatibility notes

| Area | Detail |
|------|--------|
| **RFC 5905 strictness** | ntpsec enforces RFC 5905 more strictly than classic ntpd, including checking that the originate timestamp in the server response matches the transmit timestamp from the client request (bytes 24-31 must equal client bytes 40-47).  This server correctly echoes the originate timestamp. |
| **iburst** | Same note as ntpd above.  Omit or use `minpoll 4` to avoid early KoD responses. |
| **Mode-6 (`ntpq`)** | Compatible.  ntpsec's `ntpq` parses the same mode-6 format as classic ntpd.  All `READSTAT` and `READVAR` fields are correctly interpreted. |
| **Reference timestamp staleness** | ntpsec flags if a server's reference timestamp has not advanced within a configurable window.  Since this server updates the reference timestamp on every PPS edge, this is not a concern under normal GPS-locked operation. |
| **Root delay zero** | ntpsec accepts zero root delay for a stratum-1 hardware reference without error. |

### Recommended `ntp.conf` snippet

```conf
# GPS+PPS stratum-1 appliance
server 192.168.1.100 prefer minpoll 6 maxpoll 10
```

---

## General interoperability checklist

Before deploying in a production environment, validate the following:

1. **`ntpq -pnu <ip>`** — verify the server appears in the peer list with `*`
   (system peer selected) and stratum 1.
2. **`ntpq -c "rv 0" <ip>`** — confirm `refid=GPS`, `stratum=1`, `leap=00`,
   and that `reftime` and `clock` fields show recent hexadecimal NTP timestamps.
3. **Hold the GPS antenna stationary** for at least 5 minutes and confirm that
   `offset` in `ntpq` output is less than 1 ms and `jitter` is below 500 µs.
4. **Simulate holdover** by disconnecting the GPS fix input; verify that
   responding clients observe stratum rising toward 16 as dispersion grows, and
   that ntpd/chronyd drops the server from the selection set within the expected
   holdover window (~50–100 min depending on client `maxdistance`).
5. **Rate-limit test** — `ntpdate -q <ip>` (which sends a quick single query)
   should succeed; rapid repeated `ntpdate` calls within 2 s should receive KoD
   `RATE` and back off without crashing the client.
6. **Leap second test (optional)** — use `server.set_leap_indicator(1)` to
   broadcast LI=1 and confirm that `ntpq` shows `leap=01` in the server's
   `rv 0` output.
