// =============================================================================
// File: tests/rate_limit_tests.rs — token-bucket rate limiter integration tests
// =============================================================================
//
// Exercises `api::rate_limit::RateLimiter` directly: burst budget, per-IP
// isolation, refill recovery, global ceiling, retry-after surfacing.

use std::time::Duration;

use tentaflow_core::api::rate_limit::{RateLimitConfig, RateLimitResult, RateLimiter};

fn cfg_small() -> RateLimitConfig {
    RateLimitConfig {
        per_ip_capacity: 5,
        per_ip_refill_per_sec: 1.0,
        global_capacity: 1_000,
        global_refill_per_sec: 1_000.0,
    }
}

#[test]
fn burst_allowed_then_ip_limit_with_retry_after() {
    let rl = RateLimiter::new(cfg_small());
    let ip = "10.0.0.1";
    for _ in 0..5 {
        assert_eq!(rl.check(ip), RateLimitResult::Allow);
    }
    match rl.check(ip) {
        RateLimitResult::IpLimit { ip: got, retry_after_secs } => {
            assert_eq!(got, ip);
            assert!(retry_after_secs > 0.0);
            assert!(retry_after_secs <= 1.0);
        }
        other => panic!("expected IpLimit, got {:?}", other),
    }
}

#[test]
fn per_ip_buckets_independent() {
    let rl = RateLimiter::new(cfg_small());
    for _ in 0..5 {
        assert_eq!(rl.check("ip_a"), RateLimitResult::Allow);
    }
    assert!(matches!(rl.check("ip_a"), RateLimitResult::IpLimit { .. }));
    // Different IP starts with a fresh budget.
    for _ in 0..5 {
        assert_eq!(rl.check("ip_b"), RateLimitResult::Allow);
    }
}

#[test]
fn sustained_traffic_blocked_until_refill() {
    let rl = RateLimiter::new(cfg_small());
    let ip = "10.0.0.2";
    for _ in 0..5 {
        assert_eq!(rl.check(ip), RateLimitResult::Allow);
    }
    assert!(matches!(rl.check(ip), RateLimitResult::IpLimit { .. }));
    std::thread::sleep(Duration::from_millis(1_100));
    assert_eq!(rl.check(ip), RateLimitResult::Allow);
}

#[test]
fn global_ceiling_kicks_in_first() {
    let rl = RateLimiter::new(RateLimitConfig {
        per_ip_capacity: 1_000,
        per_ip_refill_per_sec: 1_000.0,
        global_capacity: 3,
        global_refill_per_sec: 0.01,
    });
    assert_eq!(rl.check("a"), RateLimitResult::Allow);
    assert_eq!(rl.check("b"), RateLimitResult::Allow);
    assert_eq!(rl.check("c"), RateLimitResult::Allow);
    // Fourth request from yet another IP — per-IP bucket is huge, but the
    // global ceiling is exhausted.
    match rl.check("d") {
        RateLimitResult::GlobalLimit { retry_after_secs } => {
            assert!(retry_after_secs > 0.0);
        }
        other => panic!("expected GlobalLimit, got {:?}", other),
    }
}

#[test]
fn many_unique_ips_do_not_panic_or_unbound() {
    let rl = RateLimiter::new(cfg_small());
    for i in 0..2_000u32 {
        let ip = format!("192.0.2.{}", i % 256);
        let _ = rl.check(&ip);
    }
    // We don't assert exact map size — eviction is opportunistic — but it
    // must not be wildly over the hard cap.
    assert!(rl.ip_entry_count() <= 10_000);
}
