// =============================================================================
// File: tests/camera_host_functions.rs — Camera host functions integration (M1.W6)
// =============================================================================
//
// Drives the Camera Chunk C surface without standing up a wasmtime caller.
// The integration tests cover:
//   - DB helpers: ownership guard, soft delete, partial unique index re-use,
//     list/get filtering per addon.
//   - Validation helpers exposed through the camera module (vendor whitelist,
//     fps range, retention class).
//   - End-to-end add/health/snapshot/remove through the singleton supervisor
//     (require `assets/test/sample_traffic.mp4` + GStreamer plugins — those
//     tests are `#[ignore]` so a developer machine without the asset is not
//     blocked).
//   - test_connection happy + sad paths via `camera_ingest::fakefile`.
//
// Tests share the process-wide `CameraIngestSupervisor` singleton — every
// test that touches the supervisor uses a UUID-suffixed camera_id to avoid
// cross-test interference.

#![cfg(feature = "camera")]

use std::path::PathBuf;

use tentaflow_core::db::repository::{
    delete_camera_hard, get_camera_for_addon, insert_camera, list_cameras_for_addon,
    soft_delete_camera, update_camera, CameraPatch,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::camera_ingest::{
    fakefile, CameraConfig, CameraIngestError,
};

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("core db init")
}

fn insert(
    db: &DbPool,
    camera_id: &str,
    owner: &str,
    url: &str,
) {
    insert_camera(
        db,
        camera_id,
        owner,
        "display",
        "fake_file",
        url,
        30,
        Some(1280),
        Some(720),
        "C",
        "default",
    )
    .expect("insert");
}

fn sample_path() -> Option<PathBuf> {
    // Manifest dir at runtime is the workspace member root for cargo test.
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/test/sample_traffic.mp4");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

// =============================================================================
// DB helpers
// =============================================================================

#[test]
fn db_insert_then_list_filters_by_owner() {
    let db = make_db();
    insert(&db, "cam_alpha", "addon-a", "/tmp/a.mp4");
    insert(&db, "cam_beta", "addon-a", "/tmp/b.mp4");
    insert(&db, "cam_gamma", "addon-b", "/tmp/c.mp4");

    let a = list_cameras_for_addon(&db, "addon-a").expect("list a");
    let b = list_cameras_for_addon(&db, "addon-b").expect("list b");
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 1);
    assert!(a.iter().all(|r| r.owner_addon_id == "addon-a"));
    assert_eq!(b[0].camera_id, "cam_gamma");
}

#[test]
fn db_get_returns_none_for_foreign_owner() {
    let db = make_db();
    insert(&db, "cam_x", "addon-a", "/tmp/x.mp4");
    let foreign = get_camera_for_addon(&db, "addon-b", "cam_x").expect("query");
    assert!(foreign.is_none());
    let mine = get_camera_for_addon(&db, "addon-a", "cam_x").expect("query");
    assert!(mine.is_some());
}

#[test]
fn db_update_patches_only_provided_fields() {
    let db = make_db();
    insert(&db, "cam_u", "addon-a", "/tmp/u.mp4");
    let patch = CameraPatch {
        display_name: Some("new name".into()),
        target_fps: Some(15),
        retention_class: Some("B".into()),
        ..Default::default()
    };
    assert!(update_camera(&db, "addon-a", "cam_u", &patch).expect("update"));
    let row = get_camera_for_addon(&db, "addon-a", "cam_u").unwrap().unwrap();
    assert_eq!(row.display_name, "new name");
    assert_eq!(row.target_fps, 15);
    assert_eq!(row.retention_class, "B");
    // Untouched fields preserved
    assert_eq!(row.vendor, "fake_file");
    assert_eq!(row.url, "/tmp/u.mp4");
}

#[test]
fn db_update_foreign_owner_does_not_match() {
    let db = make_db();
    insert(&db, "cam_u2", "addon-a", "/tmp/u2.mp4");
    let patch = CameraPatch {
        display_name: Some("hijack".into()),
        ..Default::default()
    };
    assert!(!update_camera(&db, "addon-b", "cam_u2", &patch).expect("update"));
    let row = get_camera_for_addon(&db, "addon-a", "cam_u2").unwrap().unwrap();
    assert_eq!(row.display_name, "display");
}

#[test]
fn db_soft_delete_then_get_returns_none() {
    let db = make_db();
    insert(&db, "cam_s", "addon-a", "/tmp/s.mp4");
    assert!(soft_delete_camera(&db, "addon-a", "cam_s").expect("delete"));
    let row = get_camera_for_addon(&db, "addon-a", "cam_s").unwrap();
    assert!(row.is_none(), "soft-deleted row must not appear in active queries");
}

#[test]
fn db_soft_delete_idempotent_for_already_removed() {
    let db = make_db();
    insert(&db, "cam_s2", "addon-a", "/tmp/s2.mp4");
    assert!(soft_delete_camera(&db, "addon-a", "cam_s2").unwrap());
    assert!(!soft_delete_camera(&db, "addon-a", "cam_s2").unwrap());
}

#[test]
fn db_re_insert_after_soft_delete_allowed_by_partial_unique_index() {
    // Migration v21 unique index is partial WHERE removed_at IS NULL, so
    // re-using the same camera_id after a soft-delete must be accepted.
    let db = make_db();
    insert(&db, "cam_recycle", "addon-a", "/tmp/r.mp4");
    soft_delete_camera(&db, "addon-a", "cam_recycle").unwrap();
    // Second insert must succeed.
    insert(&db, "cam_recycle", "addon-a", "/tmp/r2.mp4");
    let row = get_camera_for_addon(&db, "addon-a", "cam_recycle").unwrap().unwrap();
    assert_eq!(row.url, "/tmp/r2.mp4");
}

#[test]
fn db_re_insert_active_id_collides() {
    let db = make_db();
    insert(&db, "cam_dup", "addon-a", "/tmp/d.mp4");
    let res = insert_camera(
        &db,
        "cam_dup",
        "addon-a",
        "display",
        "fake_file",
        "/tmp/d2.mp4",
        30,
        None,
        None,
        "C",
        "default",
    );
    assert!(res.is_err(), "active row must trigger unique index violation");
}

#[test]
fn db_delete_hard_only_matches_owner() {
    let db = make_db();
    insert(&db, "cam_h", "addon-a", "/tmp/h.mp4");
    delete_camera_hard(&db, "addon-b", "cam_h").unwrap();
    // Owner-mismatch hard-delete must NOT remove the row.
    let row = get_camera_for_addon(&db, "addon-a", "cam_h").unwrap();
    assert!(row.is_some());
    delete_camera_hard(&db, "addon-a", "cam_h").unwrap();
    let row = get_camera_for_addon(&db, "addon-a", "cam_h").unwrap();
    assert!(row.is_none());
}

// =============================================================================
// test_connection / resolve_file_url helpers
// =============================================================================

#[test]
fn test_connection_helper_rejects_missing_file() {
    let err = fakefile::resolve_file_url("/definitely/not/here/x.mp4").unwrap_err();
    assert!(matches!(err, CameraIngestError::FileNotFound(_)));
}

#[test]
fn test_connection_helper_rejects_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.bin");
    std::fs::write(&target, b"x").unwrap();
    let link = dir.path().join("link.bin");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = fakefile::resolve_file_url(link.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, CameraIngestError::SymlinkNotAllowed(_)));
}

#[test]
fn test_connection_helper_accepts_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.bin");
    std::fs::write(&target, b"x").unwrap();
    let resolved = fakefile::resolve_file_url(target.to_str().unwrap()).expect("resolve");
    assert!(resolved.is_file());
}

// =============================================================================
// Supervisor integration — gated on sample mp4 availability
// =============================================================================

/// Uniqueizes camera_id across tests sharing the singleton.
fn uniq(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_add_and_health_via_test_api() {
    let Some(path) = sample_path() else {
        eprintln!("skipping — sample mp4 missing");
        return;
    };
    // Share the lock with reset-drain tests — otherwise a parallel drain
    // wipes our session before the health probe finishes.
    let _g = drain_mutex().lock().unwrap();
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_test_add");
    let cfg = CameraConfig {
        camera_id: id.clone(),
        vendor: "fake_file".into(),
        url: path.to_string_lossy().into_owned(),
        target_fps: 30,
        resolution: None,
        owner_addon_id: None,
    };
    sup.add_camera(cfg).await.expect("add");

    // Health is immediately queryable even before the first frame arrives.
    let h = sup.get_health(&id).await.expect("health");
    assert_eq!(h.camera_id, id);
    sup.remove_camera(&id).await.expect("remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_rejects_unsupported_vendor() {
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_bad_vendor");
    let err = sup
        .add_camera(CameraConfig {
            camera_id: id,
            vendor: "rtsp".into(),
            url: "rtsp://example/foo".into(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CameraIngestError::UnsupportedVendor(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_rejects_fps_out_of_range() {
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_bad_fps");
    let err = sup
        .add_camera(CameraConfig {
            camera_id: id.clone(),
            vendor: "fake_file".into(),
            url: "/tmp/whatever.mp4".into(),
            target_fps: 0,
            resolution: None,
            owner_addon_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CameraIngestError::InvalidConfig(_)));
    let id = uniq("cam_bad_fps2");
    let err = sup
        .add_camera(CameraConfig {
            camera_id: id,
            vendor: "fake_file".into(),
            url: "/tmp/whatever.mp4".into(),
            target_fps: 61,
            resolution: None,
            owner_addon_id: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CameraIngestError::InvalidConfig(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires assets/test/sample_traffic.mp4 + GStreamer plugins"]
async fn supervisor_snapshot_returns_rgb24_frame() {
    let Some(path) = sample_path() else {
        panic!("sample_traffic.mp4 missing");
    };
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_snap");
    sup.add_camera(CameraConfig {
        camera_id: id.clone(),
        vendor: "fake_file".into(),
        url: path.to_string_lossy().into_owned(),
        target_fps: 30,
        resolution: None,
        owner_addon_id: None,
    })
    .await
    .expect("add");

    let snap = sup.snapshot(&id).await.expect("snapshot");
    assert!(snap.width > 0 && snap.height > 0);
    assert!(!snap.data.is_empty());
    // RGB24 = 3 bytes/pixel.
    assert_eq!(snap.data.len(), (snap.width * snap.height * 3) as usize);
    sup.remove_camera(&id).await.ok();
}

// =============================================================================
// Validator unit tests (Issue #3) — camera_id format + display_name + profile
// =============================================================================

#[test]
fn camera_id_valid_accepts_uuid_v4_format() {
    use tentaflow_core::addon::host_functions::camera::test_api::camera_id_valid_for_test;
    let id = format!("cam_{}", uuid::Uuid::new_v4());
    assert!(camera_id_valid_for_test(&id), "fresh uuid id must validate: {id}");
}

#[test]
fn camera_id_valid_rejects_bad_inputs() {
    use tentaflow_core::addon::host_functions::camera::test_api::camera_id_valid_for_test;
    assert!(!camera_id_valid_for_test(""));
    assert!(!camera_id_valid_for_test("cam_short"));
    assert!(!camera_id_valid_for_test("cam_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx")); // not hex
    // Uppercase hex not allowed (DB stores lowercase from uuid crate).
    assert!(!camera_id_valid_for_test("cam_DEADBEEF-DEAD-BEEF-DEAD-BEEFDEADBEEF"));
    // No prefix.
    let raw = uuid::Uuid::new_v4().to_string();
    assert!(!camera_id_valid_for_test(&raw));
    // Wrong prefix.
    assert!(!camera_id_valid_for_test(&format!("camera_{}", uuid::Uuid::new_v4())));
}

#[test]
fn display_name_valid_enforces_length_and_charset() {
    use tentaflow_core::addon::host_functions::camera::test_api::display_name_valid_for_test;
    assert!(display_name_valid_for_test("Front Door Cam"));
    assert!(display_name_valid_for_test("Kamera nr 1 (parking)"));
    assert!(!display_name_valid_for_test(""));
    assert!(!display_name_valid_for_test("   "));
    // 257 chars rejected (max 256).
    let big = "x".repeat(257);
    assert!(!display_name_valid_for_test(&big));
    // Disallowed control / shell metachar.
    assert!(!display_name_valid_for_test("cam`whoami`"));
    assert!(!display_name_valid_for_test("cam;rm -rf /"));
}

#[test]
fn profile_valid_lowercase_alnum_dash_underscore() {
    use tentaflow_core::addon::host_functions::camera::test_api::profile_valid_for_test;
    assert!(profile_valid_for_test("default"));
    assert!(profile_valid_for_test("high-fps_2"));
    assert!(!profile_valid_for_test("Default"));
    assert!(!profile_valid_for_test(""));
    assert!(!profile_valid_for_test("has space"));
    assert!(!profile_valid_for_test(&"a".repeat(129)));
}

// =============================================================================
// Supervisor singleton — init race + drain reset (Issue #5 + #6)
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supervisor_init_is_singleton_under_concurrent_callers() {
    use std::sync::Arc as StdArc;
    // Two concurrent awaiters must observe exactly one initialized instance.
    let h1 = tokio::spawn(async {
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests().await
    });
    let h2 = tokio::spawn(async {
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests().await
    });
    let a = h1.await.expect("join1").expect("sup1");
    let b = h2.await.expect("join2").expect("sup2");
    assert!(StdArc::ptr_eq(&a, &b), "supervisor must be a process-wide singleton");
}

/// Cross-test mutex used by tests that mutate the shared supervisor's
/// registry in a way that would race other tests using the same singleton.
/// `reset_supervisor_for_test` drains EVERY session, so any test that calls
/// it must hold this mutex.
fn drain_mutex() -> &'static std::sync::Mutex<()> {
    static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_reset_drains_sessions_without_dropping_singleton() {
    let Some(path) = sample_path() else {
        eprintln!("skipping — sample mp4 missing");
        return;
    };
    let _g = drain_mutex().lock().unwrap();
    let sup_before =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_reset");
    sup_before
        .add_camera(CameraConfig {
            camera_id: id.clone(),
            vendor: "fake_file".into(),
            url: path.to_string_lossy().into_owned(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
        })
        .await
        .expect("add");

    tentaflow_core::addon::host_functions::camera::test_api::reset_supervisor_for_test().await;

    // Singleton survives, but the session is gone.
    let sup_after =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor still alive");
    assert!(std::sync::Arc::ptr_eq(&sup_before, &sup_after));
    let err = sup_after.get_health(&id).await.unwrap_err();
    assert!(matches!(err, CameraIngestError::NotFound(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_drain_is_idempotent() {
    let _g = drain_mutex().lock().unwrap();
    let _ = tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
        .await
        .expect("init");
    tentaflow_core::addon::host_functions::camera::test_api::reset_supervisor_for_test().await;
    tentaflow_core::addon::host_functions::camera::test_api::reset_supervisor_for_test().await;
}

// =============================================================================
// DB+supervisor cycle: add → soft-delete → re-add reuses camera_id (Issue #7/#8)
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_add_then_soft_delete_then_reuse_id() {
    let Some(path) = sample_path() else {
        eprintln!("skipping — sample mp4 missing");
        return;
    };
    let _g = drain_mutex().lock().unwrap();
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let db = make_db();
    let id = uniq("cam_recycle_e2e");
    insert(&db, &id, "addon-recycle", path.to_str().unwrap());
    sup.add_camera(CameraConfig {
        camera_id: id.clone(),
        vendor: "fake_file".into(),
        url: path.to_string_lossy().into_owned(),
        target_fps: 30,
        resolution: None,
        owner_addon_id: None,
    })
    .await
    .expect("first add");

    // Soft-delete row first (mirrors host fn camera_remove order).
    assert!(soft_delete_camera(&db, "addon-recycle", &id).expect("soft delete"));
    sup.remove_camera(&id).await.expect("remove");

    // Re-insert with same id must succeed (partial unique index).
    insert(&db, &id, "addon-recycle", path.to_str().unwrap());
    sup.add_camera(CameraConfig {
        camera_id: id.clone(),
        vendor: "fake_file".into(),
        url: path.to_string_lossy().into_owned(),
        target_fps: 30,
        resolution: None,
        owner_addon_id: None,
    })
    .await
    .expect("re-add");
    sup.remove_camera(&id).await.ok();
}

// =============================================================================
// Supervisor rejects file-not-found (probe path that camera_add hits when DB
// insert would have left an orphan in the legacy ordering).
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_rejects_missing_file_path_fakefile() {
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_missing_path");
    let err = sup
        .add_camera(CameraConfig {
            camera_id: id,
            vendor: "fake_file".into(),
            url: "/var/empty/this/path/does/not/exist.mp4".into(),
            target_fps: 30,
            resolution: None,
            owner_addon_id: None,
        })
        .await
        .unwrap_err();
    // Either FileNotFound (resolved by fakefile::resolve_file_url) or a
    // downstream pipeline init error — both are valid "no orphan" outcomes
    // because the row is never inserted on failure under the Issue #7
    // reorder (supervisor first, DB second).
    assert!(
        matches!(err, CameraIngestError::FileNotFound(_) | CameraIngestError::PipelineBuild(_) | CameraIngestError::Internal(_)),
        "expected FileNotFound or pipeline-side failure, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires assets/test/sample_traffic.mp4 + GStreamer plugins"]
async fn supervisor_fps_actual_approaches_target_after_warmup() {
    let Some(path) = sample_path() else {
        panic!("sample_traffic.mp4 missing");
    };
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_fps");
    sup.add_camera(CameraConfig {
        camera_id: id.clone(),
        vendor: "fake_file".into(),
        url: path.to_string_lossy().into_owned(),
        target_fps: 30,
        resolution: None,
        owner_addon_id: None,
    })
    .await
    .expect("add");

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let h = sup.get_health(&id).await.expect("health");
    assert!(h.frames_total > 30, "frames_total={}", h.frames_total);
    sup.remove_camera(&id).await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_remove_closes_streaming_bus() {
    use tentaflow_core::services::streaming::{NextOutcome, StreamFilter, StreamMessage};

    let Some(path) = sample_path() else {
        eprintln!("skipping — sample mp4 missing");
        return;
    };
    let _g = drain_mutex().lock().unwrap();
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let id = uniq("cam_close_bus");
    sup.add_camera(CameraConfig {
        camera_id: id.clone(),
        vendor: "fake_file".into(),
        url: path.to_string_lossy().into_owned(),
        target_fps: 30,
        resolution: None,
        owner_addon_id: None,
    })
    .await
    .expect("add");

    let bus = tentaflow_core::services::streaming_bus();
    let mut sub = bus.subscribe(&id, StreamFilter::default());

    sup.remove_camera(&id).await.expect("remove");

    // Either an explicit CameraOffline or channel-close (None) is acceptable.
    let mut saw_terminal = false;
    for _ in 0..50 {
        match sub.next(std::time::Duration::from_millis(200)).await {
            NextOutcome::Message(StreamMessage::CameraOffline { .. }) => {
                saw_terminal = true;
                break;
            }
            NextOutcome::Message(_) => continue,
            NextOutcome::Closed => {
                saw_terminal = true;
                break;
            }
            NextOutcome::Timeout => continue,
        }
    }
    assert!(saw_terminal, "subscriber must observe terminal event after remove");
    assert!(bus.list_subscribers(&id).is_empty());
}
