// =============================================================================
// File: tests/db_migrations_v112.rs
// Purpose: Verifies migration v112 drops the legacy built-in `notes` table
//          (replaced by the notes addon, which owns its own addon SQLite) and
//          that the drop is idempotent across reopens.
// =============================================================================

use tempfile::TempDir;

fn open() -> (TempDir, tentaflow_core::db::DbPool) {
    let d = TempDir::new().expect("tempdir");
    let p = d.path().join("v112.db");
    let pool = tentaflow_core::db::init(&p).expect("init");
    (d, pool)
}

#[test]
fn migration_v112_drops_notes_table_and_indexes() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();
    let n_tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='notes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let n_indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' \
             AND name IN ('idx_notes_user', 'idx_notes_user_updated')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n_tables, 0, "legacy notes table must be dropped");
    assert_eq!(n_indexes, 0, "legacy notes indexes must be dropped");
}

#[test]
fn migration_v112_recorded_in_migrations() {
    let (_d, pool) = open();
    let conn = pool.read().unwrap();
    let exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM _migrations WHERE version=112",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(exists, 1);
}

#[test]
fn migration_v112_idempotent_reopen() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("v112.db");
    let _pool1 = tentaflow_core::db::init(&p).expect("first init");
    let _pool2 = tentaflow_core::db::init(&p).expect("second init noop");
}
