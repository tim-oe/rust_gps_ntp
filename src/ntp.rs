//! Minimal NTP server with GPS/PPS-backed discipline and mode-6 diagnostics.
//!
//! The server responds to standard client time requests (mode 3/4 flow) and
//! a focused subset of mode-6 control queries used by `ntpq`.

use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
#[cfg(target_os = "espidf")]
use esp_idf_svc::sys;

const NTP_PORT: u16 = 123;
const NTP_PACKET_LEN: usize = 48;
const NTP_MAX_PACKET_LEN: usize = 512;
const NTP_CONTROL_HEADER_LEN: usize = 12;
const NTP_UNIX_EPOCH_OFFSET_SECS: u64 = 2_208_988_800;
const NTP_FRAC_SCALE: u128 = 1u128 << 32;
const MICROS_PER_SEC: i64 = 1_000_000;
const MODE6_OPCODE_READSTAT: u8 = 1;
const MODE6_OPCODE_READVAR: u8 = 2;
const MODE6_ASSOC_ID: u16 = 1;

#[derive(Clone, Copy)]
struct ClockAnchor {
    /// Unix epoch seconds at the anchor instant.
    unix_seconds: i64,
    /// Monotonic microseconds when the anchor was captured.
    monotonic_us: i64,
}

/// Stateful NTP server engine and discipline metrics.
pub struct NtpServer {
    socket: UdpSocket,
    served: u64,
    clock_anchor: Option<ClockAnchor>,
    last_gps_utc_seconds: Option<i64>,
    pps_locked: bool,
    pps_offset_us: i32,
    pps_jitter_us: f32,
    pps_has_sample: bool,
    proc_delay_us: f32,
    proc_delay_has_sample: bool,
}

impl NtpServer {
    /// Bind a nonblocking UDP socket on NTP port 123.
    ///
    /// # Parameters
    /// - None.
    ///
    /// # Returns
    /// - `Ok(NtpServer)` when socket bind and nonblocking setup succeed.
    /// - `Err` when socket initialization fails.
    pub fn bind() -> anyhow::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", NTP_PORT))
            .context("failed to bind UDP socket on port 123")?;
        socket
            .set_nonblocking(true)
            .context("failed to set NTP socket nonblocking")?;

        Ok(Self {
            socket,
            served: 0,
            clock_anchor: None,
            last_gps_utc_seconds: None,
            pps_locked: false,
            pps_offset_us: 0,
            pps_jitter_us: 0.0,
            pps_has_sample: false,
            proc_delay_us: 0.0,
            proc_delay_has_sample: false,
        })
    }

    /// Update absolute UTC seconds from a parsed GPS RMC sample.
    ///
    /// # Parameters
    /// - `utc_unix_seconds`: UTC seconds since Unix epoch from GPS.
    ///
    /// # Returns
    /// - No return value.
    pub fn update_gps_utc_seconds(&mut self, utc_unix_seconds: i64) {
        self.last_gps_utc_seconds = Some(utc_unix_seconds);
        let now_us = monotonic_us_now();
        match self.clock_anchor {
            Some(anchor) => {
                let elapsed_sec = now_us
                    .saturating_sub(anchor.monotonic_us)
                    .div_euclid(MICROS_PER_SEC);
                let predicted = anchor.unix_seconds.saturating_add(elapsed_sec);
                if predicted.saturating_sub(utc_unix_seconds).abs() > 1 {
                    self.clock_anchor = Some(ClockAnchor {
                        unix_seconds: utc_unix_seconds,
                        monotonic_us: now_us,
                    });
                    log::debug!("NTP: UTC re-anchor from GPS");
                }
            }
            None => {
                self.clock_anchor = Some(ClockAnchor {
                    unix_seconds: utc_unix_seconds,
                    monotonic_us: now_us,
                });
            }
        }
    }

    /// Feed PPS pulse timing into the discipline loop.
    ///
    /// Pass `None` for the first observed pulse to align to current GPS UTC,
    /// then pass `Some(interval_us)` for subsequent pulse deltas.
    ///
    /// # Parameters
    /// - `pps_interval_us`: `None` for first pulse alignment, or pulse interval in microseconds.
    ///
    /// # Returns
    /// - No return value.
    pub fn observe_pps_pulse(&mut self, pps_interval_us: Option<u32>) {
        let now_us = monotonic_us_now();

        if let Some(anchor) = &mut self.clock_anchor {
            if let Some(interval) = pps_interval_us {
                if (800_000..=1_200_000).contains(&interval) {
                    anchor.unix_seconds = anchor.unix_seconds.saturating_add(1);
                    anchor.monotonic_us = now_us;
                    self.pps_locked = true;
                }
            } else if let Some(gps_utc) = self.last_gps_utc_seconds {
                anchor.unix_seconds = gps_utc;
                anchor.monotonic_us = now_us;
                self.pps_locked = true;
            }
        } else if let Some(gps_utc) = self.last_gps_utc_seconds {
            self.clock_anchor = Some(ClockAnchor {
                unix_seconds: gps_utc,
                monotonic_us: now_us,
            });
            self.pps_locked = pps_interval_us.is_none();
        }

        if let Some(interval) = pps_interval_us {
            self.update_pps_interval_us(interval);
        }
    }

    /// Update PPS-derived offset and jitter estimates used in diagnostics.
    ///
    /// # Parameters
    /// - `pps_interval_us`: Interval between consecutive PPS pulses.
    ///
    /// # Returns
    /// - No return value.
    fn update_pps_interval_us(&mut self, pps_interval_us: u32) {
        // Ignore obviously invalid intervals.
        if !(800_000..=1_200_000).contains(&pps_interval_us) {
            return;
        }
        let offset_us = (pps_interval_us as i32).saturating_sub(1_000_000);
        let sample_jitter = (offset_us.unsigned_abs()) as f32;
        self.pps_offset_us = offset_us;
        if self.pps_has_sample {
            // EWMA with moderate smoothing to avoid noisy jumps in ntpq view.
            self.pps_jitter_us = self.pps_jitter_us * 0.8 + sample_jitter * 0.2;
        } else {
            self.pps_jitter_us = sample_jitter;
            self.pps_has_sample = true;
        }
    }

    /// Poll and serve all immediately available NTP packets.
    ///
    /// # Parameters
    /// - `gps_fix`: Current GPS fix status used to determine sync state.
    ///
    /// # Returns
    /// - `Ok(())` when no more packets are pending or all served successfully.
    /// - `Err` on socket receive/send failures.
    pub fn poll(&mut self, gps_fix: bool) -> anyhow::Result<()> {
        let mut req = [0_u8; NTP_MAX_PACKET_LEN];

        loop {
            match self.socket.recv_from(&mut req) {
                Ok((len, peer)) => {
                    if len == 0 {
                        continue;
                    }

                    let mode = req[0] & 0x07;
                    let version = (req[0] >> 3) & 0x07;
                    match mode {
                        6 => {
                            if len < NTP_CONTROL_HEADER_LEN {
                                log::debug!(
                                    "NTP: ignoring short mode-6 request ({} bytes) from {}",
                                    len,
                                    peer
                                );
                                continue;
                            }
                            let synced = gps_fix && self.pps_locked && self.clock_anchor.is_some();
                            let resp = build_mode6_response(
                                &req[..len],
                                version,
                                synced,
                                self.pps_offset_us,
                                self.pps_jitter_us,
                                self.proc_delay_us,
                            );
                            self.socket.send_to(&resp, peer).with_context(|| {
                                format!("failed to send mode-6 response to {}", peer)
                            })?;
                        }
                        _ => {
                            if len < NTP_PACKET_LEN {
                                log::debug!(
                                    "NTP: ignoring short time request ({} bytes) from {}",
                                    len,
                                    peer
                                );
                                continue;
                            }

                            let mut req48 = [0_u8; NTP_PACKET_LEN];
                            req48.copy_from_slice(&req[..NTP_PACKET_LEN]);

                            let synced = gps_fix && self.pps_locked && self.clock_anchor.is_some();
                            let started_us = monotonic_us_now();
                            let now_ntp = self.current_ntp_timestamp();
                            let mut resp = build_response(&req48, synced, now_ntp);
                            let transmit_ts = self.current_ntp_timestamp();
                            write_u64_be(&mut resp[40..48], transmit_ts);

                            self.socket.send_to(&resp, peer).with_context(|| {
                                format!("failed to send NTP response to {}", peer)
                            })?;
                            let finished_us = monotonic_us_now();
                            let sample_us = (finished_us.saturating_sub(started_us)).max(1) as f32;
                            if self.proc_delay_has_sample {
                                self.proc_delay_us = self.proc_delay_us * 0.8 + sample_us * 0.2;
                            } else {
                                self.proc_delay_us = sample_us;
                                self.proc_delay_has_sample = true;
                            }
                        }
                    }

                    self.served += 1;

                    if self.served.is_multiple_of(64) {
                        log::debug!("NTP: served {} requests", self.served);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(err).context("NTP recv_from failed"),
            }
        }
    }
}

/// Build a mode-6 control response for a request packet.
///
/// # Parameters
/// - `req`: Raw mode-6 request bytes.
/// - `version`: NTP version extracted from request header.
/// - `synced`: Current synchronized-state flag.
/// - `pps_offset_us`: Latest PPS-derived offset in microseconds.
/// - `pps_jitter_us`: Smoothed jitter estimate in microseconds.
/// - `proc_delay_us`: Smoothed processing delay estimate in microseconds.
///
/// # Returns
/// - Serialized mode-6 response bytes ready to send.
fn build_mode6_response(
    req: &[u8],
    version: u8,
    synced: bool,
    pps_offset_us: i32,
    pps_jitter_us: f32,
    proc_delay_us: f32,
) -> Vec<u8> {
    let mut resp = vec![0_u8; NTP_CONTROL_HEADER_LEN];
    let vn = if (1..=4).contains(&version) {
        version
    } else {
        4
    };
    let opcode = req[1] & 0x1f;
    let associd = u16::from_be_bytes([req[6], req[7]]);
    let mut payload: Vec<u8> = Vec::new();
    let mut error = false;

    // LI=0, VN from request, Mode=6 (control)
    resp[0] = (vn << 3) | 6;
    match opcode {
        MODE6_OPCODE_READSTAT => {
            // Return one pseudo-association so `ntpq -p` can continue querying it.
            let peer_status = if synced { 0x9624_u16 } else { 0x16_u16 };
            payload.extend_from_slice(&MODE6_ASSOC_ID.to_be_bytes());
            payload.extend_from_slice(&peer_status.to_be_bytes());
        }
        MODE6_OPCODE_READVAR => {
            let vars = if associd == 0 {
                if synced {
                    String::from(
                        "stratum=1,leap=00,precision=-20,rootdelay=0.000,rootdisp=1.000,refid=GPS,peer=1,system=\"rust_gps_ntp\"",
                    )
                } else {
                    String::from(
                        "stratum=16,leap=11,precision=-20,rootdelay=0.000,rootdisp=5.000,refid=INIT,peer=0,system=\"rust_gps_ntp\"",
                    )
                }
            } else if associd == MODE6_ASSOC_ID {
                if synced {
                    let offset_ms = (pps_offset_us as f32) / 1_000.0;
                    let jitter_ms = (pps_jitter_us.max(1.0)) / 1_000.0;
                    let delay_ms = (proc_delay_us.max(1.0)) / 1_000.0;
                    format!(
                        "srcadr=GPS,srcport=123,refid=GPS,stratum=1,leap=00,hmode=3,pmode=4,hpoll=6,ppoll=6,reach=255,delay={delay_ms:.3},offset={offset_ms:.3},jitter={jitter_ms:.3}"
                    )
                } else {
                    String::from(
                        "srcadr=INIT,srcport=123,refid=INIT,stratum=16,leap=11,hmode=3,pmode=4,hpoll=6,ppoll=6,reach=0,delay=0.001,offset=0.000,jitter=0.000",
                    )
                }
            } else {
                String::new()
            };
            payload.extend_from_slice(vars.as_bytes());
        }
        _ => {
            // Unsupported opcode: reply with a mode-6 error response.
            error = true;
        }
    }

    // Response bit set, keep opcode from request.
    resp[1] = 0x80 | ((error as u8) << 6) | opcode;
    // Sequence from request.
    resp[2] = req[2];
    resp[3] = req[3];

    // status
    let sys_status = if synced { 0x0604_u16 } else { 0x0_u16 };
    resp[4..6].copy_from_slice(&sys_status.to_be_bytes());
    // associd (echo request association)
    resp[6..8].copy_from_slice(&associd.to_be_bytes());
    // offset=0 (single-fragment response)
    resp[8..10].copy_from_slice(&0_u16.to_be_bytes());
    // count
    let count = payload.len().min(u16::MAX as usize) as u16;
    resp[10..12].copy_from_slice(&count.to_be_bytes());

    if count > 0 {
        resp.extend_from_slice(&payload[..count as usize]);
        // Mode-6 control payloads are 32-bit aligned on the wire.
        while !resp.len().is_multiple_of(4) {
            resp.push(0);
        }
    }
    resp
}

/// Build a standard 48-byte NTP server response template.
///
/// # Parameters
/// - `req`: Parsed 48-byte client request.
/// - `gps_fix`: Current synchronized-state flag.
/// - `receive_ts`: Server receive timestamp in NTP 64-bit format.
///
/// # Returns
/// - 48-byte response packet with originate/receive/reference fields populated.
fn build_response(
    req: &[u8; NTP_PACKET_LEN],
    gps_fix: bool,
    receive_ts: u64,
) -> [u8; NTP_PACKET_LEN] {
    let mut resp = [0_u8; NTP_PACKET_LEN];
    let client_vn = (req[0] >> 3) & 0x07;
    let version = if (1..=4).contains(&client_vn) {
        client_vn
    } else {
        4
    };
    let leap = if gps_fix { 0 } else { 3 };
    resp[0] = (leap << 6) | (version << 3) | 4; // server mode
    resp[1] = if gps_fix { 1 } else { 16 }; // stratum
    resp[2] = req[2]; // copy poll interval from client
    resp[3] = (-20_i8) as u8; // precision ~1us

    // Root delay / root dispersion in NTP short format (16.16).
    write_u32_be(&mut resp[4..8], 0);
    write_u32_be(&mut resp[8..12], if gps_fix { 1 << 16 } else { 5 << 16 });

    // Reference ID:
    if gps_fix {
        resp[12..16].copy_from_slice(b"GPS\0");
    } else {
        resp[12..16].copy_from_slice(b"INIT");
    }

    let reference_ts = if gps_fix { receive_ts } else { 0 };
    write_u64_be(&mut resp[16..24], reference_ts);

    // Originate timestamp = client's transmit timestamp.
    resp[24..32].copy_from_slice(&req[40..48]);
    // Receive timestamp set by server when request was read.
    write_u64_be(&mut resp[32..40], receive_ts);
    // Transmit timestamp is filled just before send.
    resp
}

/// Compute current NTP timestamp directly from system wall clock.
///
/// # Parameters
/// - None.
///
/// # Returns
/// - Current NTP timestamp (`seconds.fraction` packed into `u64`).
fn ntp_timestamp_now() -> u64 {
    let unix = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(v) => v,
        Err(_) => Duration::from_secs(0),
    };
    let seconds = unix.as_secs().saturating_add(NTP_UNIX_EPOCH_OFFSET_SECS);
    let fraction = ((unix.subsec_nanos() as u128) * NTP_FRAC_SCALE / 1_000_000_000u128) as u64;
    (seconds << 32) | fraction
}

impl NtpServer {
    /// Compute the current NTP timestamp from the disciplined anchor clock.
    ///
    /// # Parameters
    /// - `self`: NTP server state containing discipline anchor.
    ///
    /// # Returns
    /// - Current disciplined NTP timestamp.
    fn current_ntp_timestamp(&self) -> u64 {
        if let Some(anchor) = self.clock_anchor {
            let now_us = monotonic_us_now();
            let elapsed_us = now_us.saturating_sub(anchor.monotonic_us);
            let elapsed_seconds = elapsed_us.div_euclid(MICROS_PER_SEC);
            let rem_us = elapsed_us.rem_euclid(MICROS_PER_SEC) as u64;

            let unix_seconds = anchor.unix_seconds.saturating_add(elapsed_seconds);
            let ntp_seconds = if unix_seconds >= 0 {
                (unix_seconds as u64).saturating_add(NTP_UNIX_EPOCH_OFFSET_SECS)
            } else {
                NTP_UNIX_EPOCH_OFFSET_SECS
            };
            let ntp_fraction = ((rem_us as u128) * NTP_FRAC_SCALE / 1_000_000u128) as u64;
            (ntp_seconds << 32) | ntp_fraction
        } else {
            ntp_timestamp_now()
        }
    }
}

/// Return monotonic microseconds from ESP-IDF high-resolution timer.
fn monotonic_us_now() -> i64 {
    #[cfg(target_os = "espidf")]
    {
        return unsafe { sys::esp_timer_get_time() };
    }
    #[cfg(not(target_os = "espidf"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64
    }
}

/// Write a big-endian `u32` into an output byte slice.
///
/// # Parameters
/// - `dst`: Destination slice (must be length 4).
/// - `value`: Integer value to encode.
///
/// # Returns
/// - No return value.
fn write_u32_be(dst: &mut [u8], value: u32) {
    dst.copy_from_slice(&value.to_be_bytes());
}

/// Write a big-endian `u64` into an output byte slice.
///
/// # Parameters
/// - `dst`: Destination slice (must be length 8).
/// - `value`: Integer value to encode.
///
/// # Returns
/// - No return value.
fn write_u64_be(dst: &mut [u8], value: u64) {
    dst.copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_response_sets_server_mode_and_stratum_when_synced() {
        let req = [0_u8; NTP_PACKET_LEN];
        let resp = build_response(&req, true, 0x0123_0000_0000_0000);
        assert_eq!(resp[0] & 0x07, 4);
        assert_eq!(resp[1], 1);
        assert_eq!(&resp[12..16], b"GPS\0");
    }

    #[test]
    fn build_response_marks_unsynced_with_init_refid() {
        let req = [0_u8; NTP_PACKET_LEN];
        let resp = build_response(&req, false, 0);
        assert_eq!(resp[1], 16);
        assert_eq!(&resp[12..16], b"INIT");
    }

    #[test]
    fn build_mode6_readvar_includes_gps_peer_when_synced() {
        let req = [
            0,
            MODE6_OPCODE_READVAR,
            0,
            0,
            0,
            0,
            0,
            MODE6_ASSOC_ID as u8,
            0,
            0,
            0,
            0,
        ];
        let resp = build_mode6_response(&req, 4, true, 250, 500.0, 1200.0);
        let payload = String::from_utf8_lossy(&resp[NTP_CONTROL_HEADER_LEN..]);
        assert!(payload.contains("refid=GPS"));
        assert!(payload.contains("stratum=1"));
    }

    #[test]
    fn write_u64_be_roundtrip() {
        let mut buf = [0_u8; 8];
        write_u64_be(&mut buf, 0x0123_4567_89ab_cdef);
        assert_eq!(buf, 0x0123_4567_89ab_cdef_u64.to_be_bytes());
    }
}
