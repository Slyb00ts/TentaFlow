// =============================================================================
// File: tests/router_round_robin.rs
// F2 P2.a — integration tests for the round-robin pool strategy. The
// rotation kernel lives in `services::runtime::strategy::rank`; this file
// verifies behaviour at the surface the executor sees (Strategy enum,
// StrategyState shared across requests, fairness under concurrency) and
// pins migration v33 (`model_aliases.strategy` CHECK constraint accepts
// `round_robin`).
// =============================================================================

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tentaflow_core::services::catalog::Strategy;
use tentaflow_core::services::handles_cache::BackendHandle;
use tentaflow_core::services::runtime::strategy::{rank, StrategyState};
use tentaflow_core::services::runtime::target::ResolvedExecutionTarget;

fn target(name: &str) -> ResolvedExecutionTarget {
    ResolvedExecutionTarget::Local {
        service_id: 1,
        model_name: name.to_string(),
        handle: BackendHandle::Embedded {
            model_name: name.to_string(),
            node_id: "n".into(),
            engine_id: "test-engine".into(),
        },
    }
}

#[test]
fn round_robin_cycles_through_targets() {
    let candidates = vec![target("a"), target("b"), target("c")];
    let state = StrategyState::new();
    let mut counts = [0u32; 3];
    for _ in 0..30 {
        let ranked = rank(&candidates, Strategy::RoundRobin, &state);
        match ranked[0].requested_model() {
            "a" => counts[0] += 1,
            "b" => counts[1] += 1,
            "c" => counts[2] += 1,
            other => panic!("unexpected primary: {other}"),
        }
    }
    assert_eq!(counts, [10, 10, 10], "perfect rotation expected");
}

#[test]
fn round_robin_skips_unhealthy_target() {
    // The executor enforces health by trimming the candidate list before
    // calling rank. Simulate that here: a healthy filter drops candidate
    // "b" entirely so rotation runs over [a, c] only.
    let all = vec![target("a"), target("b"), target("c")];
    let down: &str = "b";
    let healthy: Vec<ResolvedExecutionTarget> = all
        .iter()
        .filter(|t| t.requested_model() != down)
        .cloned()
        .collect();
    assert_eq!(healthy.len(), 2);
    let state = StrategyState::new();
    let mut counts = [0u32; 2];
    for _ in 0..20 {
        let ranked = rank(&healthy, Strategy::RoundRobin, &state);
        match ranked[0].requested_model() {
            "a" => counts[0] += 1,
            "c" => counts[1] += 1,
            other => panic!("unhealthy target leaked: {other}"),
        }
    }
    assert_eq!(counts, [10, 10]);
}

#[test]
fn round_robin_resumes_when_target_returns() {
    // Phase 1: only [a, c] healthy.
    let candidates_phase1 = vec![target("a"), target("c")];
    let state = StrategyState::new();
    let mut phase1 = Vec::new();
    for _ in 0..4 {
        phase1.push(
            rank(&candidates_phase1, Strategy::RoundRobin, &state)[0]
                .requested_model()
                .to_string(),
        );
    }
    assert_eq!(phase1, vec!["a", "c", "a", "c"]);

    // Phase 2: "b" comes back. The shared `state` counter keeps advancing,
    // so rotation continues monotonically over the new 3-element list.
    let candidates_phase2 = vec![target("a"), target("b"), target("c")];
    let mut phase2 = Vec::new();
    for _ in 0..6 {
        phase2.push(
            rank(&candidates_phase2, Strategy::RoundRobin, &state)[0]
                .requested_model()
                .to_string(),
        );
    }
    // Distribution must touch every member at least once.
    assert!(phase2.contains(&"a".to_string()));
    assert!(phase2.contains(&"b".to_string()));
    assert!(phase2.contains(&"c".to_string()));
}

#[test]
fn round_robin_concurrency_safe() {
    let candidates = Arc::new(vec![target("a"), target("b"), target("c")]);
    let state = Arc::new(StrategyState::new());
    let counts = Arc::new([
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ]);

    let mut handles = Vec::new();
    for _ in 0..8 {
        let cands = Arc::clone(&candidates);
        let st = Arc::clone(&state);
        let c = Arc::clone(&counts);
        handles.push(std::thread::spawn(move || {
            for _ in 0..100 {
                let ranked = rank(&cands, Strategy::RoundRobin, &st);
                let idx = match ranked[0].requested_model() {
                    "a" => 0,
                    "b" => 1,
                    "c" => 2,
                    _ => unreachable!(),
                };
                c[idx].fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for h in handles {
        h.join().expect("thread join");
    }

    let total: usize = counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    assert_eq!(total, 800);
    // Each bucket should be ~267; allow ±10% slack (the counter rotates
    // monotonically so deviation is bounded by the modulo offset, not by
    // thread interleaving).
    let expected = 800 / 3;
    let slack = expected / 10;
    for (i, c) in counts.iter().enumerate() {
        let v = c.load(Ordering::Relaxed);
        assert!(
            v.abs_diff(expected) <= slack,
            "bucket {i} drift too large: {v} vs {expected}±{slack}"
        );
    }
}

#[test]
fn round_robin_with_single_target_returns_same() {
    let candidates = vec![target("solo")];
    let state = StrategyState::new();
    for _ in 0..10 {
        let ranked = rank(&candidates, Strategy::RoundRobin, &state);
        assert_eq!(ranked[0].requested_model(), "solo");
    }
}

#[test]
fn first_available_does_not_rotate() {
    // The legacy strategy is the implicit fallback when an alias row has
    // strategy = NULL or 'first_available'. Compat test: rank() must keep
    // declaration order regardless of how often it is called.
    let candidates = vec![target("a"), target("b"), target("c")];
    let state = StrategyState::new();
    for _ in 0..5 {
        let ranked = rank(&candidates, Strategy::FirstAvailable, &state);
        let names: Vec<&str> = ranked.iter().map(|t| t.requested_model()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}

// -----------------------------------------------------------------------------
// Migration v33 — model_aliases.strategy CHECK accepts round_robin
// -----------------------------------------------------------------------------

#[test]
fn migration_v33_accepts_round_robin_value() {
    let pool = tentaflow_core::db::init(Path::new(":memory:")).expect("test db");
    let conn = pool.write().expect("lock");
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model, is_active, fallback_targets, strategy) \
         VALUES (?1, ?2, 1, NULL, ?3)",
        rusqlite::params!["rr-alias-1", "target-x", "round_robin"],
    )
    .expect("insert with round_robin must succeed");

    let strategy: String = conn
        .query_row(
            "SELECT strategy FROM model_aliases WHERE alias = ?1",
            rusqlite::params!["rr-alias-1"],
            |r| r.get(0),
        )
        .expect("read back");
    assert_eq!(strategy, "round_robin");
}

#[test]
fn migration_v33_accepts_first_available_value() {
    let pool = tentaflow_core::db::init(Path::new(":memory:")).expect("test db");
    let conn = pool.write().expect("lock");
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model, is_active, fallback_targets, strategy) \
         VALUES (?1, ?2, 1, NULL, ?3)",
        rusqlite::params!["fa-alias-1", "target-y", "first_available"],
    )
    .expect("insert with first_available must succeed");
}

#[test]
fn migration_v33_rejects_unknown_strategy_value() {
    let pool = tentaflow_core::db::init(Path::new(":memory:")).expect("test db");
    let conn = pool.write().expect("lock");
    let res = conn.execute(
        "INSERT INTO model_aliases (alias, target_model, is_active, fallback_targets, strategy) \
         VALUES (?1, ?2, 1, NULL, ?3)",
        rusqlite::params!["bad-alias", "target-z", "least_loaded"],
    );
    assert!(
        res.is_err(),
        "least_loaded must be rejected by the CHECK constraint; got {:?}",
        res
    );
}

#[test]
fn migration_v33_default_strategy_remains_first_available() {
    let pool = tentaflow_core::db::init(Path::new(":memory:")).expect("test db");
    let conn = pool.write().expect("lock");
    conn.execute(
        "INSERT INTO model_aliases (alias, target_model) VALUES (?1, ?2)",
        rusqlite::params!["default-alias", "target-q"],
    )
    .expect("insert with default strategy");
    let strategy: String = conn
        .query_row(
            "SELECT strategy FROM model_aliases WHERE alias = ?1",
            rusqlite::params!["default-alias"],
            |r| r.get(0),
        )
        .expect("read back");
    assert_eq!(strategy, "first_available");
}
