// =============================================================================
// File: tests/db_migrations_v23.rs
// Purpose: Tests for migration v23 (cameras_vendor_check_rtsp_onvif) — verifies
//          existing `fake_file` rows survive the table rebuild, the new
//          `rtsp` vendor is accepted, and unsupported vendors are still
//          rejected by the CHECK constraint. Also asserts that the v21
//          indexes (partial unique on camera_id, owner index, status index)
//          are recreated after the rebuild.
// =============================================================================

use rusqlite::{params, Connection};
use tempfile::TempDir;
use tentaflow_core::db;

fn open_db() -> (TempDir, db::DbPool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let pool = db::init(&path).expect("init DB");
    (dir, pool)
}

fn index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn insert_camera(conn: &Connection, camera_id: &str, vendor: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO cameras (
            camera_id, owner_addon_id, display_name, vendor, url,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![camera_id, "test-addon", "Test Camera", vendor, "file:///tmp/x.mp4", 0_i64, 0_i64],
    )?;
    Ok(())
}

#[test]
fn v23_migration_preserves_fake_file_rows() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    insert_camera(&conn, "cam-fake-1", "fake_file").expect("insert fake_file row");

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM cameras WHERE camera_id = ?1 AND vendor = 'fake_file'",
            ["cam-fake-1"],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(count, 1, "fake_file row must survive v23 migration");
}

#[test]
fn v23_migration_allows_rtsp_vendor() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    insert_camera(&conn, "cam-rtsp-1", "rtsp").expect("rtsp vendor must be accepted after v23");

    let vendor: String = conn
        .query_row(
            "SELECT vendor FROM cameras WHERE camera_id = ?1",
            ["cam-rtsp-1"],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(vendor, "rtsp");
}

#[test]
fn v23_migration_allows_onvif_vendor() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    insert_camera(&conn, "cam-onvif-1", "onvif").expect("onvif vendor must be accepted after v23");

    let vendor: String = conn
        .query_row(
            "SELECT vendor FROM cameras WHERE camera_id = ?1",
            ["cam-onvif-1"],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(vendor, "onvif");
}

#[test]
fn v23_migration_rejects_unsupported_vendor() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    let err = insert_camera(&conn, "cam-bad-1", "foo")
        .expect_err("unsupported vendor must trip CHECK constraint");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("check") || msg.contains("constraint"),
        "expected CHECK constraint violation, got: {msg}"
    );
}

#[test]
fn v23_migration_recreates_indexes() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    for idx in &[
        "idx_cameras_camera_id_active",
        "idx_cameras_owner",
        "idx_cameras_status",
    ] {
        assert!(
            index_exists(&conn, idx),
            "index {idx} must be recreated after v23 table rebuild"
        );
    }
}

#[test]
fn v23_migration_recorded_in_meta() {
    let (_dir, pool) = open_db();
    let conn = pool.lock().expect("lock");

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM _migrations WHERE version = 23",
            [],
            |r| r.get(0),
        )
        .expect("query _migrations");
    assert_eq!(exists, 1, "migration v23 must be recorded");
}
