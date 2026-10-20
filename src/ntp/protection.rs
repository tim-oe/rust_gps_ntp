//! Per-client rate limiting and IP ACL for NTP service protection.
//!
//! Both data structures use fixed-capacity arrays (no heap allocation) to
//! stay compatible with `no_std`/ESP-IDF targets.
//!
//! # Rate limiter
//! [`RateLimiter`] tracks the last-accepted-request time for up to
//! [`RATE_TABLE_SIZE`] IPv4 clients. Clients that poll faster than
//! [`MIN_POLL_INTERVAL_US`] receive a Kiss-o'-Death `RATE` response
//! (RFC 5905 §7.4).
//!
//! Limits of the design, accepted for a LAN appliance: only IPv4 sources are
//! keyed (IPv6 is never rate limited), and more than `RATE_TABLE_SIZE`
//! distinct addresses polling inside one interval can evict each other and
//! defeat the limit. The ACL is the primary defence against untrusted
//! sources; the limiter is a safety net against misbehaving trusted clients.
//!
//! # Access control list
//! [`Acl`] is a fixed-capacity CIDR allowlist for IPv4 addresses.
//! IPv6 sources are always passed through (not matched against the list).
//! Factory presets: [`Acl::allow_all`], [`Acl::deny_all`], [`Acl::private_lan`].

/// Maximum number of IPv4 client entries tracked in the rate-limiter table.
/// Entries are evicted LRU-style when the table is full.
pub(super) const RATE_TABLE_SIZE: usize = 32;
/// Minimum inter-request interval for a single client before a KoD RATE is sent.
/// 2 seconds is well below normal NTP polling (minpoll = 2^4 = 16 s) but
/// still protects against accidental loops and flood attacks.
pub(super) const MIN_POLL_INTERVAL_US: i64 = 2_000_000;
/// Maximum number of CIDR entries in the ACL allowlist.
pub(super) const ACL_MAX_ENTRIES: usize = 8;

// --- Per-client rate limiter ---

/// One slot in the per-client rate-limiter lookup table.
#[derive(Clone, Copy)]
struct ClientRecord {
    /// IPv4 source address as a host-order `u32`.
    addr: u32,
    /// Monotonic microseconds of the most recent accepted request.
    last_us: i64,
}

/// Fixed-capacity per-client rate limiter.
///
/// On each incoming time or control request the caller checks the source IP.
/// A request is accepted if the client has not been seen before, or if the time
/// since its last accepted request exceeds `MIN_POLL_INTERVAL_US`. Otherwise
/// the caller sends a KoD RATE (48-byte modes) or silently drops (mode-6).
///
/// The table holds up to `RATE_TABLE_SIZE` entries. When full, the entry
/// with the oldest `last_us` is evicted to make room for new clients.
pub(super) struct RateLimiter {
    table: [Option<ClientRecord>; RATE_TABLE_SIZE],
}

impl RateLimiter {
    pub(super) fn new() -> Self {
        Self {
            table: [None; RATE_TABLE_SIZE],
        }
    }

    /// Record a request from `addr` at `now_us`.
    ///
    /// Returns `true` if the request should be served, `false` if it should
    /// receive a KoD RATE response.
    pub(super) fn check(&mut self, addr: u32, now_us: i64) -> bool {
        for entry in self.table.iter_mut().flatten() {
            if entry.addr == addr {
                let elapsed = now_us.saturating_sub(entry.last_us);
                if elapsed < MIN_POLL_INTERVAL_US {
                    return false; // too fast → KoD
                }
                entry.last_us = now_us;
                return true;
            }
        }
        // New client: find an empty slot, or evict the oldest entry.
        let slot = self
            .table
            .iter()
            .position(|e| e.is_none())
            .unwrap_or_else(|| {
                self.table
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.map(|r| r.last_us).unwrap_or(i64::MAX))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        self.table[slot] = Some(ClientRecord {
            addr,
            last_us: now_us,
        });
        true
    }

    /// Subtract `by_us` from every entry's `last_us` timestamp.
    /// Used in tests to simulate time passing without real sleeps.
    #[cfg(test)]
    pub(super) fn subtract_time(&mut self, by_us: i64) {
        for slot in self.table.iter_mut().flatten() {
            slot.last_us -= by_us;
        }
    }
}

// --- ACL allowlist ---

/// One CIDR prefix entry in an ACL allowlist.
#[derive(Clone, Copy)]
struct AclEntry {
    /// Network address (host-order u32), e.g. `0xC0A80000` for 192.168.0.0.
    network: u32,
    /// Subnet mask (host-order u32), e.g. `0xFFFF0000` for /16.
    mask: u32,
}

impl AclEntry {
    fn contains(self, addr: u32) -> bool {
        (addr & self.mask) == (self.network & self.mask)
    }
}

/// Fixed-capacity IP allowlist for NTP service protection.
///
/// When the list is empty and `default_allow` is `true` (the default),
/// all clients are accepted.  Set `default_allow = false` (via
/// `Acl::deny_all()` or `Acl::private_lan()`) and then call
/// `add_ipv4_cidr` to build a strict allowlist.
///
/// Only IPv4 addresses are matched; IPv6 sources always pass through.
///
/// # Examples
/// ```
/// use rust_gps_ntp::ntp::Acl;
/// let mut acl = Acl::private_lan();   // allow all RFC 1918 + loopback
/// ```
#[derive(Clone)]
pub struct Acl {
    entries: [Option<AclEntry>; ACL_MAX_ENTRIES],
    len: usize,
    /// Whether to allow an address when no entry matches.
    default_allow: bool,
}

impl Acl {
    /// Create an ACL that allows all IPv4 addresses (no restrictions).
    pub fn allow_all() -> Self {
        Self {
            entries: [None; ACL_MAX_ENTRIES],
            len: 0,
            default_allow: true,
        }
    }

    /// Create an ACL that denies all addresses; use `add_ipv4_cidr` to
    /// explicitly permit ranges.
    pub fn deny_all() -> Self {
        Self {
            entries: [None; ACL_MAX_ENTRIES],
            len: 0,
            default_allow: false,
        }
    }

    /// Build the ACL from `CONFIG_GPS_NTP_ACL_CIDR` in `sdkconfig.defaults`.
    ///
    /// When the CIDR string is empty, returns [`Self::private_lan`]. Otherwise
    /// denies all sources except the configured prefix (validated at build time).
    pub fn from_config(cidr: &str) -> Self {
        if let Some((a, b, c, d, prefix)) = parse_ipv4_cidr(cidr) {
            let mut acl = Self::deny_all();
            acl.add_ipv4_cidr(a, b, c, d, prefix);
            acl
        } else {
            Self::private_lan()
        }
    }

    /// Create an ACL that allows only private RFC 1918 ranges and loopback:
    /// `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`.
    ///
    /// This is the recommended setting for a trusted LAN deployment.
    pub fn private_lan() -> Self {
        let mut acl = Self::deny_all();
        acl.add_ipv4_cidr(127, 0, 0, 0, 8); // loopback
        acl.add_ipv4_cidr(10, 0, 0, 0, 8); // RFC 1918 class A
        acl.add_ipv4_cidr(172, 16, 0, 0, 12); // RFC 1918 class B
        acl.add_ipv4_cidr(192, 168, 0, 0, 16); // RFC 1918 class C
        acl
    }

    /// Add a CIDR entry to the allowlist.
    ///
    /// # Parameters
    /// - `a`–`d`: The four octets of the network address.
    /// - `prefix_bits`: Prefix length (0–32).
    ///
    /// Returns `true` on success or `false` if the table is full
    /// (`ACL_MAX_ENTRIES` entries already added).
    pub fn add_ipv4_cidr(&mut self, a: u8, b: u8, c: u8, d: u8, prefix_bits: u8) -> bool {
        if self.len >= ACL_MAX_ENTRIES {
            return false;
        }
        let network = u32::from_be_bytes([a, b, c, d]);
        let mask = if prefix_bits == 0 {
            0
        } else {
            !((1u32 << (32 - prefix_bits as u32)) - 1)
        };
        let slot = self.entries.iter().position(|e| e.is_none()).unwrap_or(0);
        self.entries[slot] = Some(AclEntry { network, mask });
        self.len += 1;
        true
    }

    /// Returns `true` if `addr` (host-order u32) is permitted by this ACL.
    pub(super) fn allows(&self, addr: u32) -> bool {
        if self.len == 0 {
            return self.default_allow;
        }
        self.entries
            .iter()
            .filter_map(|e| *e)
            .any(|e| e.contains(addr))
    }
}

// `parse_ipv4_cidr` is shared with build.rs via `include!` so the
// compile-time ACL validation cannot drift from the runtime parser.
include!("cidr_parse.rs");

#[cfg(test)]
mod tests;
