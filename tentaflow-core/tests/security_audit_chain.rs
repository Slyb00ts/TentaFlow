// =============================================================================
// File: tests/security_audit_chain.rs — integration coverage for the F1b P4
// audit Merkle chain. Drives the public `audit::verify::verify_chain` API
// against rows produced by the production audit writer (`log_audit` in
// `db::repository`) and asserts the standard tamper scenarios from
// `notes/tentavision-f1b-handoff.md` §4.
// =============================================================================

use rusqlite::params;
use tempfile::TempDir;

use tentaflow_core::audit::verify::{verify_chain, TamperKind};
use tentaflow_core::db;
use tentaflow_core::db::repository::log_audit;

fn open_pool() -> (TempDir, db::DbPool) {
    let td = TempDir::new().expect("tempdir");
    let db_path = td.path().join("router.db");
    let pool = db::init(&db_path).expect("init db");
    (td, pool)
}

#[test]
fn fresh_chain_verifies_clean() {
    let (_td, pool) = open_pool();

    for i in 0..5 {
        log_audit(
            &pool,
            Some(i as i64),
            Some("com.test"),
            "smoke",
            Some("res"),
            Some("{}"),
            None,
            None,
        )
        .expect("write audit row");
    }

    let conn = pool.lock().expect("db lock");
    let report = verify_chain(&conn).expect("verify ok");
    assert_eq!(report.total, 5, "report: {:?}", report);
    assert_eq!(report.chained_ok, 5, "report: {:?}", report);
    assert_eq!(report.legacy_unchained, 0, "report: {:?}", report);
    assert!(report.is_clean(), "tampered: {:?}", report.tampered);
}

#[test]
fn tampering_action_is_detected() {
    let (_td, pool) = open_pool();

    log_audit(&pool, Some(1), Some("a1"), "act_a", None, None, None, None).unwrap();
    log_audit(&pool, Some(2), Some("a2"), "act_b", None, None, None, None).unwrap();
    log_audit(&pool, Some(3), Some("a3"), "act_c", None, None, None, None).unwrap();

    {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE audit_log SET action = 'evil' WHERE id = ?1",
            params![2i64],
        )
        .unwrap();
    }

    let conn = pool.lock().unwrap();
    let report = verify_chain(&conn).expect("verify ok");
    assert!(!report.is_clean());
    assert!(
        report
            .tampered
            .iter()
            .any(|t| t.id == 2 && t.kind == TamperKind::HashMismatch),
        "expected HashMismatch on id=2, got {:?}",
        report.tampered
    );
}

#[test]
fn deleting_middle_row_breaks_prev_hash() {
    let (_td, pool) = open_pool();

    log_audit(&pool, Some(1), Some("a"), "a1", None, None, None, None).unwrap();
    log_audit(&pool, Some(2), Some("a"), "a2", None, None, None, None).unwrap();
    log_audit(&pool, Some(3), Some("a"), "a3", None, None, None, None).unwrap();

    {
        let conn = pool.lock().unwrap();
        conn.execute("DELETE FROM audit_log WHERE id = ?1", params![2i64])
            .unwrap();
    }

    let conn = pool.lock().unwrap();
    let report = verify_chain(&conn).expect("verify ok");
    assert!(!report.is_clean());
    assert!(
        report
            .tampered
            .iter()
            .any(|t| t.id == 3 && t.kind == TamperKind::PrevHashMismatch),
        "expected PrevHashMismatch on id=3, got {:?}",
        report.tampered
    );
}

#[test]
fn raw_bypass_insert_after_chain_start_is_tamper() {
    let (_td, pool) = open_pool();

    log_audit(&pool, Some(1), Some("a"), "first", None, None, None, None).unwrap();

    {
        let conn = pool.lock().unwrap();
        // Direct raw INSERT bypassing `log_audit` — leaves prev_hash/hash NULL.
        conn.execute(
            "INSERT INTO audit_log (action, risk_class) VALUES ('snuck_in', 'unclassified')",
            [],
        )
        .unwrap();
    }

    let conn = pool.lock().unwrap();
    let report = verify_chain(&conn).expect("verify ok");
    assert!(!report.is_clean());
    assert!(report
        .tampered
        .iter()
        .any(|t| t.kind == TamperKind::NullHashAfterChainStart));
}
