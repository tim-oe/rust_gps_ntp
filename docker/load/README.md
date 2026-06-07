# Docker NTP load testing

Run sustained NTP queries against `gps-ntp` from one or many client IPs.

## Prerequisites

- Device reachable on the LAN (`ping gps-ntp.local`)
- ACL allows client subnets (`CONFIG_GPS_NTP_ACL_CIDR` in `sdkconfig.defaults`, default `192.168.1.0/24`)
- Docker installed on a host on the same LAN as the ESP32

## Single client (host network)

All traffic uses the Docker host's LAN IP — one client as far as the device's rate limiter is concerned.

```bash
DEVICE=gps-ntp DURATION=120 \
  docker compose -f docker/load/docker-compose.yml --profile host up
```

Equivalent without Docker:

```bash
just load-test gps-ntp -- --duration 120
```

## Multi-client (macvlan)

Each scaled container receives a distinct `192.168.1.210–223` address, so the device's per-IP rate limiter treats them as separate clients.

```bash
DEVICE=gps-ntp CLIENTS=8 DURATION=300 MACVLAN_PARENT=eth0 \
  docker compose -f docker/load/docker-compose.yml --profile macvlan up --scale ntp-client=8
```

Or via `just`:

```bash
just load-test-docker DEVICE=gps-ntp CLIENTS=8 DURATION=300 MACVLAN_PARENT=eth0
```

### Macvlan notes

- Set `MACVLAN_PARENT` to your LAN interface (`eth0`, `enp0s31f6`, `wlan0`, etc.).
- Macvlan containers reach other LAN hosts (the ESP32) but **cannot** reach the Docker host on the same subnet without an extra macvlan shim — that is fine for NTP load testing.
- The IP pool `192.168.1.210/28` must not overlap addresses already in use on your LAN.
- On Wi-Fi-only hosts, macvlan often fails; use wired Ethernet or run `just load-test` with multiple `--bind-ip` aliases on the host instead:

```bash
sudo ip addr add 192.168.1.201/32 dev eth0
sudo ip addr add 192.168.1.202/32 dev eth0
just load-test gps-ntp -- --workers 2 --bind-ip 192.168.1.201 --bind-ip 192.168.1.202
```

## Interpreting results

The script prints delay p50/p95/p99 and offset percentiles. While load runs, watch the device TFT **NTP page** for `Proc` (processing delay EWMA), `srv` (served count), and `ko` (KoD count).

After load, verify accuracy:

```bash
just validate-ntp gps-ntp
```
