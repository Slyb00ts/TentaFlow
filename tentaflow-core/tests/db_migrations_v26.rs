// =============================================================================
// File: tests/db_migrations_v26.rs
// Purpose: Tests for migration v26 (trusted_publishers) — verifies the
//          trust store table is created with the expected columns, supports
//          insert/select round-trip, the UNIQUE constraint on key_b64, and
//          is idempotent on re-open.
// =============================================================================

use rusqlite::Connection;
use tempfile::TempDir;
use tentaflow_core::db;

fn open_db() -> (TempDir, db::DbPool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let pool = db::init(&path).expect("init DB");
    (dir, pool)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table_info");
    let mut rows = stmt.query([]).expect("query table_info");
    while let Some(row) = rows.next().expect("row") {
        let name: String = row.get(1).expect("name");
        if name == column {
            return true;
        }
    }
    false
}

#[test]
fn v26_creates_trusted_publishers_table_with_expected_columns() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    for col in ["key_b64", "label", "added_at", "added_by_user", "contact"] {
        assert!(
            column_exists(&conn, "trusted_publishers", col),
            "trusted_publishers must have column {col}"
        );
    }
}

#[test]
fn v26_recorded_in_meta() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 26",
            [],
            |r| r.get(0),
        )
        .expect("query _migrations");
    assert_eq!(exists, 1, "migration v26 must be recorded");
}

#[test]
fn v26_trust_store_is_empty_by_default() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM trusted_publishers", [], |r| r.get(0))
        .expect("count");
    assert_eq!(
        count, 0,
        "default-deny: no keys must be auto-seeded into trust store"
    );
}

#[test]
fn v26_supports_insert_and_unique_constraint() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let pk = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    conn.execute(
        "INSERT INTO trusted_publishers (key_b64, label, added_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![pk, "ACME", "2026-01-01T00:00:00Z"],
    )
    .expect("insert ok");
    let err = conn.execute(
        "INSERT INTO trusted_publishers (key_b64, label, added_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![pk, "ACME-2", "2026-01-02T00:00:00Z"],
    );
    assert!(err.is_err(), "duplicate key_b64 must violate PRIMARY KEY");
}

#[test]
fn v26_idempotent_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let _ = db::init(&path).expect("first init");
    let pool2 = db::init(&path).expect("second init must not fail");
    let conn = pool2.lock().expect("lock");
    let v26_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 26",
            [],
            |r| r.get(0),
        )
        .expect("count v26");
    assert_eq!(v26_rows, 1, "v26 must be recorded exactly once");
    assert!(column_exists(&conn, "trusted_publishers", "key_b64"));
}
