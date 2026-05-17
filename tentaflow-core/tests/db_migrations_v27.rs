// =============================================================================
// File: tests/db_migrations_v27.rs
// Purpose: Tests for migration v27 (addon_vector_namespaces) — verifies the
//          per-addon namespace registry table is created with the expected
//          columns + CHECK constraints, supports insert/select round-trip,
//          enforces the (addon_id, namespace) PRIMARY KEY, and is idempotent
//          on re-open.
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
fn v27_creates_addon_vector_namespaces_table_with_expected_columns() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    for col in [
        "addon_id",
        "namespace",
        "dim",
        "metric",
        "count",
        "file_path",
        "created_at",
        "updated_at",
    ] {
        assert!(
            column_exists(&conn, "addon_vector_namespaces", col),
            "addon_vector_namespaces must have column {col}"
        );
    }
}

#[test]
fn v27_recorded_in_meta() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 27",
            [],
            |r| r.get(0),
        )
        .expect("query _migrations");
    assert_eq!(exists, 1, "migration v27 must be recorded");
}

#[test]
fn v27_supports_insert_and_primary_key_constraint() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    conn.execute(
        "INSERT INTO addon_vector_namespaces \
         (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
        rusqlite::params![
            "addon_a",
            "faces",
            512,
            "cosine",
            "/tmp/faces.usearch",
            "2026-01-01 00:00:00",
        ],
    )
    .expect("first insert ok");
    let err = conn.execute(
        "INSERT INTO addon_vector_namespaces \
         (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
        rusqlite::params![
            "addon_a",
            "faces",
            512,
            "cosine",
            "/tmp/faces2.usearch",
            "2026-01-01 00:00:00",
        ],
    );
    assert!(
        err.is_err(),
        "duplicate (addon_id, namespace) must violate PRIMARY KEY"
    );
}

#[test]
fn v27_metric_check_constraint_rejects_invalid_value() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let res = conn.execute(
        "INSERT INTO addon_vector_namespaces \
         (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
        rusqlite::params![
            "addon_a",
            "weird",
            512,
            "manhattan",
            "/tmp/x.usearch",
            "2026-01-01 00:00:00",
        ],
    );
    assert!(res.is_err(), "metric='manhattan' must violate CHECK");
}

#[test]
fn v27_dim_check_constraint_rejects_out_of_range() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    let res = conn.execute(
        "INSERT INTO addon_vector_namespaces \
         (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?6)",
        rusqlite::params![
            "addon_a",
            "huge",
            8192,
            "cosine",
            "/tmp/x.usearch",
            "2026-01-01 00:00:00",
        ],
    );
    assert!(res.is_err(), "dim=8192 must violate CHECK (dim<=4096)");
}

#[test]
fn v27_cross_addon_same_namespace_name_is_allowed() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");
    conn.execute(
        "INSERT INTO addon_vector_namespaces \
         (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
         VALUES (?1, 'faces', 512, 'cosine', 0, '/tmp/a.usearch', '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
        rusqlite::params!["addon_a"],
    )
    .expect("addon_a faces ok");
    conn.execute(
        "INSERT INTO addon_vector_namespaces \
         (addon_id, namespace, dim, metric, count, file_path, created_at, updated_at) \
         VALUES (?1, 'faces', 768, 'cosine', 0, '/tmp/b.usearch', '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
        rusqlite::params!["addon_b"],
    )
    .expect("addon_b faces ok — different PK component");
}

#[test]
fn v27_idempotent_on_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let _ = db::init(&path).expect("first init");
    let pool2 = db::init(&path).expect("second init must not fail");
    let conn = pool2.lock().expect("lock");
    let v27_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 27",
            [],
            |r| r.get(0),
        )
        .expect("count v27");
    assert_eq!(v27_rows, 1, "v27 must be recorded exactly once");
    assert!(column_exists(&conn, "addon_vector_namespaces", "addon_id"));
}
