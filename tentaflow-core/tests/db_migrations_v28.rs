// =============================================================================
// File: tests/db_migrations_v28.rs
// Purpose: Verifies F1c P4 migration v28 (policy_claims + policy_claim_signatures)
//          lands cleanly, is idempotent across reopens, and enforces the
//          composite PK on signatures.
// =============================================================================

use rusqlite::params;
use tempfile::TempDir;

fn open() -> (TempDir, tentaflow_core::db::DbPool) {
    let d = TempDir::new().expect("tempdir");
    let p = d.path().join("v28.db");
    let pool = tentaflow_core::db::init(&p).expect("init");
    (d, pool)
}

#[test]
fn migration_v28_creates_both_tables() {
    let (_d, pool) = open();
    let conn = pool.lock().unwrap();
    let n_claims: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='policy_claims'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let n_sigs: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='policy_claim_signatures'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_claims, 1);
    assert_eq!(n_sigs, 1);
}

#[test]
fn migration_v28_recorded_in_migrations() {
    let (_d, pool) = open();
    let conn = pool.lock().unwrap();
    let exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM _migrations WHERE version=28",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(exists, 1);
}

#[test]
fn migration_v28_idempotent_reopen() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("v28.db");
    let _pool1 = tentaflow_core::db::init(&p).expect("first init");
    let _pool2 = tentaflow_core::db::init(&p).expect("second init noop");
}

#[test]
fn signatures_pk_rejects_duplicate_role_user() {
    let (_d, pool) = open();
    let conn = pool.lock().unwrap();
    conn.execute(
        "INSERT INTO policy_claims (claim_id, claim_type, label, valid_from, valid_until, issued_by_user, created_at) \
         VALUES ('c1','dpia','t','2026-01-01T00:00:00Z','2027-01-01T00:00:00Z','admin','2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO policy_claim_signatures (claim_id, signer_role, signer_user, signed_at) \
         VALUES ('c1','dpo','alice','2026-01-02T00:00:00Z')",
        [],
    )
    .unwrap();
    let dup = conn.execute(
        "INSERT INTO policy_claim_signatures (claim_id, signer_role, signer_user, signed_at) \
         VALUES ('c1','dpo','alice','2026-01-03T00:00:00Z')",
        [],
    );
    assert!(dup.is_err(), "duplicate (claim,role,user) must violate PK");
}

#[test]
fn cascade_delete_claim_removes_signatures() {
    let (_d, pool) = open();
    let conn = pool.lock().unwrap();
    conn.execute(
        "INSERT INTO policy_claims (claim_id, claim_type, label, valid_from, valid_until, issued_by_user, created_at) \
         VALUES ('c1','dpia','t','2026-01-01T00:00:00Z','2027-01-01T00:00:00Z','admin','2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO policy_claim_signatures (claim_id, signer_role, signer_user, signed_at) \
         VALUES ('c1','dpo','alice','2026-01-02T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM policy_claims WHERE claim_id=?1", params!["c1"])
        .unwrap();
    let remaining: i64 = conn
        .query_row(
            "SELECT count(*) FROM policy_claim_signatures WHERE claim_id='c1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn indexes_present() {
    let (_d, pool) = open();
    let conn = pool.lock().unwrap();
    let idx_type: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_policy_claims_type'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let idx_scope: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_policy_claims_scope'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_type, 1);
    assert_eq!(idx_scope, 1);
}
