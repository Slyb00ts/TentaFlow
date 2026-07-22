// ============ tests/recording_list_filters.rs — recordings-browser search SQL ============
//
// Proves the server-side filters added to `list_recordings` for the
// recordings browser search: date range (`created_from`/`created_to` over
// `created_at`), case-insensitive plate substring, and ADR substring — each in
// isolation and composed. The plate/ADR columns are the ones the per-vehicle
// event recorder writes at finalize; here we insert rows directly to drive the
// SQL without standing up the recorder.

#![cfg(feature = "camera")]

use tentaflow_core::db::repository::{self as repo, RecordingListFilters};
use tentaflow_core::db::DbPool;

const ADDON: &str = "tentavision";
const ORG: &str = "org-a";

fn open_pool() -> (tempfile::TempDir, DbPool) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("rec_filters.db");
    let pool = tentaflow_core::db::init(&path).expect("db init");
    (dir, pool)
}

/// Inserts an event `segment` row then rewrites its `created_at` to an explicit
/// unix-seconds value so the date-range filter is testable (insert_recording
/// stamps `now()` itself).
fn insert_event(
    pool: &DbPool,
    recording_ref: &str,
    camera_id: &str,
    plate: Option<&str>,
    adr: Option<&str>,
    created_at: i64,
) {
    repo::insert_recording(
        pool,
        recording_ref,
        "segment",
        ADDON,
        camera_id,
        "/tmp/clip.mp4",
        4096,
        Some(8000),
        None,
        None,
        None,
        "deadbeef",
        "B",
        Some(ORG),
        Some("{\"event\":\"vehicle_presence\"}"),
        plate,
        adr,
        plate.map(|_| "snap_00000000-0000-0000-0000-000000000001"),
        adr.map(|_| "snap_00000000-0000-0000-0000-000000000002"),
    )
    .expect("insert_recording ok");

    let conn = pool.write().expect("db write");
    conn.execute(
        "UPDATE recordings SET created_at = ?1 WHERE ref = ?2",
        rusqlite::params![created_at, recording_ref],
    )
    .expect("stamp created_at");
}

fn refs(rows: &[repo::RecordingRow]) -> Vec<String> {
    rows.iter().map(|r| r.recording_ref.clone()).collect()
}

#[test]
fn filters_by_plate_case_insensitive_substring() {
    let (_d, pool) = open_pool();
    insert_event(&pool, "clip_a", "cam-1", Some("WGM12345"), None, 1_000);
    insert_event(&pool, "clip_b", "cam-1", Some("KR9021X"), None, 2_000);
    insert_event(&pool, "clip_c", "cam-1", None, None, 3_000);

    // Lowercase query matches the uppercase stored plate (NOCASE) as a substring.
    let filters = RecordingListFilters {
        owner_addon_id: Some(ADDON),
        kind: Some("segment"),
        plate: Some("gm123"),
        ..Default::default()
    };
    let rows = repo::list_recordings(&pool, Some(ORG), &filters, 100).unwrap();
    assert_eq!(refs(&rows), vec!["clip_a".to_string()]);
}

#[test]
fn filters_by_adr_substring() {
    let (_d, pool) = open_pool();
    insert_event(&pool, "clip_a", "cam-1", None, Some("30/1202"), 1_000);
    insert_event(&pool, "clip_b", "cam-1", None, Some("33/1088"), 2_000);

    let filters = RecordingListFilters {
        owner_addon_id: Some(ADDON),
        kind: Some("segment"),
        adr: Some("1202"),
        ..Default::default()
    };
    let rows = repo::list_recordings(&pool, Some(ORG), &filters, 100).unwrap();
    assert_eq!(refs(&rows), vec!["clip_a".to_string()]);
}

#[test]
fn filters_by_created_at_range() {
    let (_d, pool) = open_pool();
    insert_event(&pool, "clip_old", "cam-1", None, None, 1_000);
    insert_event(&pool, "clip_mid", "cam-1", None, None, 5_000);
    insert_event(&pool, "clip_new", "cam-1", None, None, 9_000);

    // Inclusive bounds keep only the middle row; results are newest-first.
    let filters = RecordingListFilters {
        owner_addon_id: Some(ADDON),
        kind: Some("segment"),
        created_from: Some(2_000),
        created_to: Some(8_000),
        ..Default::default()
    };
    let rows = repo::list_recordings(&pool, Some(ORG), &filters, 100).unwrap();
    assert_eq!(refs(&rows), vec!["clip_mid".to_string()]);
}

#[test]
fn filters_compose_with_and() {
    let (_d, pool) = open_pool();
    insert_event(
        &pool,
        "clip_hit",
        "cam-1",
        Some("WGM12345"),
        Some("30/1202"),
        5_000,
    );
    // Same plate, out of the date window → excluded by the AND.
    insert_event(
        &pool,
        "clip_late",
        "cam-1",
        Some("WGM12345"),
        Some("30/1202"),
        9_999,
    );
    // In window, wrong plate → excluded.
    insert_event(
        &pool,
        "clip_wrong",
        "cam-1",
        Some("KR9021X"),
        Some("30/1202"),
        5_000,
    );

    let filters = RecordingListFilters {
        owner_addon_id: Some(ADDON),
        kind: Some("segment"),
        created_from: Some(4_000),
        created_to: Some(6_000),
        plate: Some("WGM"),
        adr: Some("1202"),
        ..Default::default()
    };
    let rows = repo::list_recordings(&pool, Some(ORG), &filters, 100).unwrap();
    assert_eq!(refs(&rows), vec!["clip_hit".to_string()]);
}

/// A LIKE metacharacter in the query is matched LITERALLY (escaped), not as a
/// wildcard — a plate containing a real `%` is found, and `%` alone does not
/// match every row.
#[test]
fn plate_like_metacharacters_are_escaped() {
    let (_d, pool) = open_pool();
    insert_event(&pool, "clip_pct", "cam-1", Some("AB%CD"), None, 1_000);
    insert_event(&pool, "clip_plain", "cam-1", Some("ABXCD"), None, 2_000);

    let filters = RecordingListFilters {
        owner_addon_id: Some(ADDON),
        kind: Some("segment"),
        plate: Some("AB%CD"),
        ..Default::default()
    };
    let rows = repo::list_recordings(&pool, Some(ORG), &filters, 100).unwrap();
    assert_eq!(refs(&rows), vec!["clip_pct".to_string()]);
}

/// The four new columns round-trip through insert → select.
#[test]
fn insert_populates_search_columns_and_thumbs() {
    let (_d, pool) = open_pool();
    insert_event(
        &pool,
        "clip_a",
        "cam-1",
        Some("WGM12345"),
        Some("30/1202"),
        1_000,
    );
    let filters = RecordingListFilters {
        owner_addon_id: Some(ADDON),
        kind: Some("segment"),
        ..Default::default()
    };
    let rows = repo::list_recordings(&pool, Some(ORG), &filters, 100).unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.plate_text.as_deref(), Some("WGM12345"));
    assert_eq!(r.adr_text.as_deref(), Some("30/1202"));
    assert_eq!(
        r.plate_thumb_ref.as_deref(),
        Some("snap_00000000-0000-0000-0000-000000000001")
    );
    assert_eq!(
        r.adr_thumb_ref.as_deref(),
        Some("snap_00000000-0000-0000-0000-000000000002")
    );
}
