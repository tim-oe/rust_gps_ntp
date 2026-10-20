use super::*;

// --- ACL tests ---

#[test]
fn parse_ipv4_cidr_accepts_slash24() {
    assert_eq!(
        parse_ipv4_cidr("192.168.1.0/24"),
        Some((192, 168, 1, 0, 24))
    );
}

#[test]
fn parse_ipv4_cidr_rejects_missing_prefix() {
    assert_eq!(parse_ipv4_cidr("192.168.1.0"), None);
}

#[test]
fn parse_ipv4_cidr_rejects_prefix_out_of_range() {
    assert_eq!(parse_ipv4_cidr("10.0.0.0/33"), None);
}

#[test]
fn from_config_empty_uses_private_lan() {
    let acl = Acl::from_config("");
    assert!(acl.allows(0xC0_A8_00_01));
    assert!(!acl.allows(0x08_08_08_08));
}

#[test]
fn from_config_cidr_restricts_to_subnet() {
    let acl = Acl::from_config("192.168.1.0/24");
    assert!(acl.allows(0xC0_A8_01_01));
    assert!(!acl.allows(0xC0_A8_02_01));
    assert!(!acl.allows(0x08_08_08_08));
}

#[test]
fn acl_allow_all_permits_any_ipv4() {
    let acl = Acl::allow_all();
    assert!(acl.allows(0x08_08_08_08)); // 8.8.8.8 (public)
    assert!(acl.allows(0xC0_A8_01_01)); // 192.168.1.1 (private)
    assert!(acl.allows(0x00_00_00_00)); // 0.0.0.0
}

#[test]
fn acl_deny_all_blocks_any_ipv4() {
    let acl = Acl::deny_all();
    assert!(!acl.allows(0x08_08_08_08));
    assert!(!acl.allows(0xC0_A8_01_01));
    assert!(!acl.allows(0x7F_00_00_01)); // 127.0.0.1
}

#[test]
fn acl_private_lan_allows_rfc1918_and_loopback() {
    let acl = Acl::private_lan();
    assert!(acl.allows(0x7F_00_00_01), "127.0.0.1 (loopback)");
    assert!(acl.allows(0x0A_00_00_01), "10.0.0.1 (RFC 1918 /8)");
    assert!(
        acl.allows(0x0A_FF_FF_FF),
        "10.255.255.255 (RFC 1918 /8 edge)"
    );
    assert!(acl.allows(0xAC_10_00_01), "172.16.0.1 (RFC 1918 /12)");
    assert!(
        acl.allows(0xAC_1F_FF_FF),
        "172.31.255.255 (RFC 1918 /12 edge)"
    );
    assert!(acl.allows(0xC0_A8_00_01), "192.168.0.1 (RFC 1918 /16)");
    assert!(
        acl.allows(0xC0_A8_FF_FF),
        "192.168.255.255 (RFC 1918 /16 edge)"
    );
}

#[test]
fn acl_private_lan_blocks_public_internet() {
    let acl = Acl::private_lan();
    assert!(!acl.allows(0x08_08_08_08), "8.8.8.8 (Google DNS)");
    assert!(!acl.allows(0x01_01_01_01), "1.1.1.1 (Cloudflare)");
    assert!(
        !acl.allows(0xAC_0F_FF_FF),
        "172.15.255.255 (just below RFC 1918 /12)"
    );
    assert!(
        !acl.allows(0xAC_20_00_00),
        "172.32.0.0 (just above RFC 1918 /12)"
    );
}

#[test]
fn acl_add_ipv4_cidr_single_host() {
    let mut acl = Acl::deny_all();
    acl.add_ipv4_cidr(192, 168, 1, 100, 32);
    assert!(acl.allows(0xC0A8_0164_u32)); // 192.168.1.100
    assert!(!acl.allows(0xC0A8_0165_u32)); // 192.168.1.101
}

#[test]
fn acl_add_ipv4_cidr_slash24() {
    let mut acl = Acl::deny_all();
    acl.add_ipv4_cidr(10, 0, 1, 0, 24);
    assert!(acl.allows(0x0A_00_01_01)); // 10.0.1.1
    assert!(acl.allows(0x0A_00_01_FF)); // 10.0.1.255
    assert!(!acl.allows(0x0A_00_02_01)); // 10.0.2.1 (different subnet)
}

#[test]
fn acl_table_full_returns_false() {
    let mut acl = Acl::deny_all();
    for i in 0..ACL_MAX_ENTRIES {
        assert!(acl.add_ipv4_cidr(10, 0, i as u8, 0, 24));
    }
    // Table is now full; next add should fail.
    assert!(!acl.add_ipv4_cidr(172, 16, 0, 0, 16));
}

// --- Rate limiter tests ---

#[test]
fn rate_limiter_allows_first_request_from_new_client() {
    let mut rl = RateLimiter::new();
    assert!(rl.check(0xC0_A8_01_01, 1_000_000));
}

#[test]
fn rate_limiter_allows_request_after_sufficient_interval() {
    let mut rl = RateLimiter::new();
    let addr = 0xC0_A8_01_01_u32;
    assert!(rl.check(addr, 0), "first request is always admitted");
    // 3 seconds later — above MIN_POLL_INTERVAL_US (2 s).
    assert!(rl.check(addr, 3_000_000));
}

#[test]
fn rate_limiter_boundary_is_exactly_min_poll_interval() {
    let mut rl = RateLimiter::new();
    let addr = 0xC0_A8_01_09_u32;
    assert!(rl.check(addr, 0));
    assert!(
        !rl.check(addr, MIN_POLL_INTERVAL_US - 1),
        "one microsecond short of the interval must be denied"
    );
    assert!(
        rl.check(addr, MIN_POLL_INTERVAL_US),
        "exactly the interval must be admitted"
    );
}

#[test]
fn rate_limiter_denies_request_within_min_interval() {
    let mut rl = RateLimiter::new();
    let addr = 0xC0_A8_01_02_u32;
    rl.check(addr, 0);
    // 0.5 seconds — below MIN_POLL_INTERVAL_US (2 s).
    assert!(!rl.check(addr, MIN_POLL_INTERVAL_US / 4));
}

#[test]
fn rate_limiter_allows_after_previous_deny() {
    let mut rl = RateLimiter::new();
    let addr = 0xC0_A8_01_03_u32;
    rl.check(addr, 0); // allow (first)
    rl.check(addr, MIN_POLL_INTERVAL_US / 4); // deny (too fast, last_us not updated)
    assert!(rl.check(addr, MIN_POLL_INTERVAL_US * 3)); // allow (enough time since first)
}

#[test]
fn rate_limiter_evicts_oldest_when_full() {
    let mut rl = RateLimiter::new();
    // Fill the table with distinct clients, oldest at t=0, newest at t=N.
    // Slot 5 is made the oldest so eviction order is not just slot order.
    let oldest = 0x0A_00_00_05_u32;
    for i in 0..RATE_TABLE_SIZE {
        let t = if i == 5 { 0 } else { (i as i64 + 1) * 1_000 };
        assert!(rl.check(0x0A_00_00_00 + i as u32, t));
    }
    let now = (RATE_TABLE_SIZE as i64 + 1) * 1_000;
    // A new client should be admitted (evicting the oldest).
    assert!(rl.check(0xFF_FF_FF_FF, now));

    // The evicted client is treated as new: admitted even though it polls
    // again within MIN_POLL_INTERVAL_US of its previous request.
    assert!(
        rl.check(oldest, now + 1),
        "evicted (oldest) client must be re-admitted as a new client"
    );
    // A client that was *not* evicted is still rate limited.
    assert!(
        !rl.check(0x0A_00_00_06, now + 2),
        "surviving entry must still enforce the interval"
    );
    // The newcomer itself is now tracked.
    assert!(!rl.check(0xFF_FF_FF_FF, now + 3));
}
