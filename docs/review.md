# Code Review — Issues Needing Resolution

Review date: 2026-06-07. Scope: full firmware crate (`src/`), tests, build
config, and `docs/`. Issues are listed in priority order (P0 = highest).

Status legend: `[ ]` open · `[x]` resolved.

---

## P0 — Broken build / test gate

### P0-1 · `cargo test` fails: two doctests do not compile
- [x] **Files:** `src/ntp/mod.rs:257` (`set_acl`), `src/ntp/mod.rs:283`
  (`set_leap_indicator`)
- **Symptom:** `cargo test` exits 101 (`just test` is red). The 121 lib unit
  tests pass, but the doctests fail to compile:
  - `set_acl` example → `E0425` (`server` not found), `E0433` (`Acl` not found)
  - `set_leap_indicator` example → `E0425` (`server` not found)
- **Cause:** The examples are fenced ` ```rust,no_run `. `no_run` still
  **compiles** the snippet; neither `server` nor `Acl` is in scope.
- **Fix:** Follow the working pattern in `src/ntp/protection.rs:130`
  (`use rust_gps_ntp::ntp::Acl;` plus a real binding), or change the fences to
  ` ```text ` / ` ```ignore ` if they are illustrative only.
- **Why P0:** The advertised test command does not pass; any CI relying on
  `cargo test` is broken.

---

## P1 — Security

### P1-1 · IP ACL is implemented and recommended but never enabled
- [x] **Files:** `src/ntp/mod.rs:243` (`acl: Acl::allow_all()` in `bind()`);
  `src/app.rs` (`set_acl` via `CONFIG_GPS_NTP_ACL_CIDR` / `Acl::from_config`)
- **Symptom:** The device binds `0.0.0.0:123` and answers every source. The ACL
  only changes in tests.
- **Mismatch:** `docs/technical.md:356` calls `private_lan()` "recommended for
  LAN deployment" and `docs/rfp.md` lists the ACL as a completed M5 deliverable.
- **Fix:** Call `ntp_server.set_acl(Acl::private_lan())` after `bind()` in
  `app.rs` (ideally gated by an `sdkconfig`/env knob), or downgrade the docs to
  reflect that allow-all is the shipped default.

### P1-2 · Mode-6 is an unauthenticated amplification/reflection vector
- [x] **File:** `src/ntp/mod.rs:648-663` (mode-6 exempt from rate limiting)
- **Symptom:** A 12-byte `READVAR` request yields a ~200+ byte response, with no
  rate limit and (by default) no ACL. With a spoofed source this is a classic
  NTP reflection/amplification primitive.
- **Fix:** Restrict mode-6 to an admin/loopback ACL, or rate-limit it, rather
  than exempting it globally. At minimum, gate it behind the P1-1 ACL.

### P1-3 · Rate limiter only covers mode 3; comment/docs claim more
- [x] **File:** `src/ntp/mod.rs:650` (`if mode == 3`)
- **Symptom:** The `_ =>` arm serves a full 48-byte response for modes 0/1/2/4/5
  with **no** rate-limit check. An attacker can flood with mode-1 packets to
  bypass the limiter.
- **Mismatch:** Comment at `src/ntp/mod.rs:648` and `docs/technical.md:325` say
  mode-1 (symmetric) is also limited.
- **Fix:** Apply rate limiting to all client-bearing modes (or explicitly
  restrict served modes), then align comment/docs.

### P1-4 · Lower-risk hardening notes
- [x] Wi-Fi credentials are compiled into the firmware image in plaintext
  (`src/wifi.rs:27`); SSID is logged (`src/wifi.rs:38`). Documented in
  `docs/setup.md` and `docs/technical.md`.
- [x] Timezone lookups use plain `http://` (`src/timezone.rs:90,101`) → switched
  to HTTPS with mbedTLS cert bundle (`sdkconfig.defaults`).

---

## P2 — Accuracy / correctness

### P2-1 · NTP receive timestamp (T2) captured too late
- [x] **File:** `src/ntp/mod.rs:668-673`
- **Symptom:** `poll()` drains all queued packets in a loop and samples the
  receive timestamp only after ACL + rate-limit + `discipline_params()` work.
  Under burst, later packets get a T2 inflated by earlier packets' processing,
  biasing client offset (`((T2−T1)+(T3−T4))/2`). Consistent with the ~2–4 ms
  residual offsets seen in `validate-ntp` runs.
- **Fix:** Capture T2 immediately after `recv_from`, before any per-packet work.

### P2-2 · Advertised precision is optimistic
- [x] **File:** `src/ntp/mod.rs:936` (precision hard-coded `-20` ≈ 0.95 µs)
- **Symptom:** Real precision is bounded by the 1 kHz loop and PPS jitter
  (hundreds of µs); `-20` overstates the clock to clients.
- **Fix:** Advertise a precision derived from measured jitter, or a more honest
  fixed value.

---

## P3 — Documentation mismatches

- [x] **P3-1 · Main-loop period wrong.** `docs/technical.md:467,491,497` say the
  loop is 10 ms / `delay_ms(10)`; actual is `delay_ms(1)` (`src/app.rs:353`,
  emphasized in `sdkconfig.defaults` and the `app.rs` header).
- [x] **P3-2 · Module path.** `docs/technical.md:63` lists `ntp.rs`; the module
  is `src/ntp/mod.rs` plus `src/ntp/protection.rs`.
- [x] **P3-3 · Mode-1 rate-limit claim.** `docs/technical.md:325` (see P1-3).
- [x] **P3-4 · Validation-checklist step count.** `docs/rfp.md:84` says
  "7-step", `docs/technical.md:548` says "6-step"; `docs/interop.md` has 7. Fix
  `technical.md`.
- [x] **P3-5 · README display pages.** `README.md:24-25` omits the 5th NTP page
  (`Page::Ntp` in `src/display.rs`).

---

## P4 — Test quality gaps

The suite is strong (asserts concrete values and wire-format bytes, not just
execution). Gaps where tests check structure but not correctness:

- [ ] **P4-1 · No end-to-end "serves the right time" assertion.**
  `src/ntp/mod.rs:1332` (`poll_serves_client_time_request`) checks only mode and
  stratum, never decodes `resp[40..48]` to confirm the transmit timestamp ≈
  anchored GPS time. Add: set a known anchor, `poll()`, decode transmit ts,
  assert within a few ms.
- [ ] **P4-2 · PPS→second fudge untested.** No test asserts
  `clock_anchor.unix_seconds == gps_utc + NMEA_PPS_FUDGE_S`
  (`src/ntp/mod.rs:373,426`).
- [x] **P4-3 · Mode-coverage of rate limiter untested.** No test sends a
  non-mode-3 client packet; this gap hid P1-3.
- [ ] **P4-4 · ACL default/`private_lan` not integration-tested.** Only
  `deny_all()` is exercised through `poll()`.
- [ ] **P4-5 · Mislabeled test.** `parse_gga_rejects_short_sentence`
  (`src/gps.rs:350`) uses a sentence that is not short; it is rejected by
  checksum/field-parse. Rename to reflect intent.
- [ ] **P4-6 · Holdover tests poke private fields.** Tests set
  `last_pps_monotonic_us` directly rather than driving the public pulse API with
  injected time; couples tests to internals.

---

## Verification snapshot (2026-06-07)

- `cargo test --lib` → **121 passed**.
- `cargo test` (full) → **fails** on doctests (see P0-1).
- Live `just validate-ntp` (terminal log): stratum=1, refid=GPS, leap=0,
  divergence ~3.9–9.2 ms vs reference median (within 100 ms tolerance).
