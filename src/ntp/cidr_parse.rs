// Shared between `build.rs` (validates CONFIG_GPS_NTP_ACL_CIDR at compile
// time) and `src/ntp/protection.rs` (parses the same string at runtime).
// Pulled in with `include!` from both so the two parsers cannot drift; keep
// this file free of `use` items and crate-internal references.

/// Parse an IPv4 CIDR string (`a.b.c.d/prefix`, prefix 0–32).
///
/// Returns `None` for empty or malformed input, out-of-range octets, a
/// prefix above 32, or extra octets.
pub fn parse_ipv4_cidr(s: &str) -> Option<(u8, u8, u8, u8, u8)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (addr, prefix) = s.split_once('/')?;
    let prefix: u8 = prefix.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mut octets = addr.split('.');
    let a: u8 = octets.next()?.parse().ok()?;
    let b: u8 = octets.next()?.parse().ok()?;
    let c: u8 = octets.next()?.parse().ok()?;
    let d: u8 = octets.next()?.parse().ok()?;
    if octets.next().is_some() {
        return None;
    }
    Some((a, b, c, d, prefix))
}
