// =============================================================================
// File: tests/service_call_rate_limit.rs — F1b P5 per-addon limiter integration
// =============================================================================
//
// Black-box validation of the per-addon rate limiter consumed by
// `addon::host_functions::service::service_request` (a.k.a. `service_call_v1`).
// End-to-end driving of the host function requires a QUIC router + service
// manager stack — instead this exercises the public limiter API plus the
// collapsed audit decision the host wrapper makes on denial.

use std::time::Duration;

use tentaflow_core::services::service_call_rate_limit::{
    note_denial_for_audit, AuditEmitDecision, RateLimitResult, ServiceCallRateLimitConfig,
    ServiceCallRateLimiter,
};

/// 100 sequential calls from one addon under default-equivalent config all
/// pass. The 101-st returns `AddonLimit` carrying a non-zero retry hint.
#[test]
fn burst_of_100_allowed_then_101_denied() {
    let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
        per_addon_capacity: 100,
        per_addon_refill_per_sec: 0.01,
    });
    for i in 0..100 {
        assert_eq!(
            rl.check("addon-x"),
            RateLimitResult::Allow,
            "call {} should be allowed",
            i + 1
        );
    }
    match rl.check("addon-x") {
        RateLimitResult::AddonLimit { addon_id, retry_after_secs } => {
            assert_eq!(addon_id, "addon-x");
            assert!(retry_after_secs > 0.0);
        }
        other => panic!("call 101 expected AddonLimit, got {:?}", other),
    }
}

/// Two addons hitting the limiter concurrently must not steal each other's
/// budget — addon-a exhausts then addon-b still gets its full burst.
#[test]
fn addon_isolation() {
    let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
        per_addon_capacity: 5,
        per_addon_refill_per_sec: 0.01,
    });
    for _ in 0..5 {
        assert_eq!(rl.check("addon-a"), RateLimitResult::Allow);
    }
    assert!(matches!(rl.check("addon-a"), RateLimitResult::AddonLimit { .. }));
    for _ in 0..5 {
        assert_eq!(
            rl.check("addon-b"),
            RateLimitResult::Allow,
            "addon-b must have a fresh bucket"
        );
    }
}

/// After exhaustion, waiting long enough for ≥1 token refill resumes service.
/// Refill 5/s + 250 ms sleep → 1.25 tokens (>=1) → Allow.
#[test]
fn quota_refills_with_time() {
    let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
        per_addon_capacity: 2,
        per_addon_refill_per_sec: 5.0,
    });
    assert_eq!(rl.check("addon-r"), RateLimitResult::Allow);
    assert_eq!(rl.check("addon-r"), RateLimitResult::Allow);
    assert!(matches!(rl.check("addon-r"), RateLimitResult::AddonLimit { .. }));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rl.check("addon-r"), RateLimitResult::Allow);
}

/// Audit collapsing: under a flood of denials, only the first emit returns
/// `Emit{denied_count=1}`; subsequent denials inside the 60 s window must
/// return `Skip` so we do not amplify the DoS into an audit-log DoS.
#[test]
fn audit_emit_collapses_inside_window() {
    let addon_id = format!("collapse-flood-{}", uuid::Uuid::new_v4());
    match note_denial_for_audit(&addon_id) {
        AuditEmitDecision::Emit { denied_count } => assert_eq!(denied_count, 1),
        AuditEmitDecision::Skip => panic!("first denial must emit"),
    }
    let mut skipped = 0;
    for _ in 0..1_000 {
        if let AuditEmitDecision::Skip = note_denial_for_audit(&addon_id) {
            skipped += 1;
        }
    }
    assert_eq!(skipped, 1_000, "every in-window denial must be Skip");
}

/// Hard cap on the per-addon map: pump 11 000 unique addon-ids; map size
/// must stay <= MAX_ADDON_ENTRIES (10 000) after the LRU eviction pass.
#[test]
fn map_bounded_under_addon_id_churn() {
    let rl = ServiceCallRateLimiter::new(ServiceCallRateLimitConfig {
        per_addon_capacity: 1,
        per_addon_refill_per_sec: 1.0,
    });
    for n in 0..11_000 {
        let id = format!("churn-addon-{n}");
        let _ = rl.check(&id);
    }
    assert!(
        rl.addon_entry_count() <= 10_000,
        "addon map size {} must stay within hard cap",
        rl.addon_entry_count()
    );
}
