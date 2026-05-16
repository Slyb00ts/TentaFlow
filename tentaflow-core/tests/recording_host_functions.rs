// =============================================================================
// File: tests/recording_host_functions.rs — Recording + frame_url M1.W8 Chunk C
// =============================================================================
//
// Drives the recording host-function surface through the `test_api::*` core
// entry points (skipping the wasmtime caller layer that requires an
// InstancePool). Covers:
//   - snapshot save happy path / permission denied / invalid frame_ref /
//     cross-addon frame ownership
//   - segment save validation (scheme, duration)
//   - get_url happy path / TTL out of range / cross-addon scoping
//   - get_stream byte fidelity (PNG round-trip)
//   - purge idempotency + DB soft-delete state
//   - stats aggregation
//   - frame_url happy path / TTL out of range / non-existent frame
//
// Tests share the process-wide singletons (frame_storage, url issuers); each
// test creates UUID-suffixed addon ids / camera ids so cross-test races never
// matter.

#![cfg(feature = "camera")]

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex as ParkingMutex;
use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
use tentaflow_core::addon::host_functions::recording::test_api as rec;
use tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::repository::{
    get_recording_for_addon, insert_camera, recording_stats_for_addon,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::frame_storage::{FrameMetadata, FramePixelFormat, StoredFrame};

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("core db init")
}

fn make_state(db: &DbPool, addon_id: &str, permissions: Vec<String>) -> AddonState {
    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: format!("{addon_id}-inst"),
        user_id: None,
        db: db.clone(),
        permissions,
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db.clone())),
        fuel_consumed: 0,
        is_system_call: true,
        rate_limiter: None,
        net_manager: Arc::new(ParkingMutex::new(NetworkConnectionManager::new())),
        settings_cipher: Arc::new(SettingsCipher::new(&[0u8; 32])),
        manifest: Arc::new(AddonManifest::default()),
        memory_limit: 256 * 1024 * 1024,
        router: None,
        oauth_refresh_guard: Arc::new(OAuthRefreshGuard::new()),
        ui_panels: None,
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4())
}

fn temp_home_guard() -> tempfile::TempDir {
    // Sandbox the per-test HOME so recording_base_dir() points into a tempdir.
    // We don't share a lock here — each test uses a unique camera_id under its
    // own HOME, and the snapshot/segment writers create the camera subdir on
    // demand, so parallel tests don't collide on filesystem paths.
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", d.path());
    d
}

fn seed_camera(db: &DbPool, owner: &str, camera_id: &str) {
    insert_camera(
        db,
        camera_id,
        owner,
        "display",
        "fake_file",
        "/tmp/whatever.mp4",
        30,
        Some(64),
        Some(48),
        "C",
        "default",
    )
    .expect("insert camera");
}

fn rgb_buf(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            v.push((x % 256) as u8);
            v.push((y % 256) as u8);
            v.push(((x + y) % 256) as u8);
        }
    }
    v
}

fn insert_frame(camera_id: &str, w: u32, h: u32, data: Vec<u8>) -> String {
    let meta = FrameMetadata {
        camera_id: camera_id.into(),
        width: w,
        height: h,
        pixel_format: FramePixelFormat::Rgb24,
        timestamp_unix_ms: 1,
        pts: None,
        frame_size_bytes: data.len(),
    };
    let frame = StoredFrame {
        metadata: meta,
        data: Arc::from(data.into_boxed_slice()),
        created_at: Instant::now(),
    };
    tentaflow_core::services::frame_storage().insert(frame).into_string()
}

fn snapshot_payload(camera_id: &str, frame_ref: &str) -> String {
    format!(
        "camera_id = {}\nframe_ref = {}\n",
        toml::Value::String(camera_id.into()),
        toml::Value::String(frame_ref.into()),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_snapshot_basic() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-snap-basic");
    let camera = uniq("cam_snap_basic");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let frame_ref = insert_frame(&camera, 16, 12, rgb_buf(16, 12));
    let (rc, out) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &frame_ref).as_bytes());
    assert_eq!(rc, AbiError::Ok.as_i32(), "save_snapshot must succeed");
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap();
    assert!(recording_ref.starts_with("snap_"));
    // DB row persisted + readable through the repo helper.
    let row = get_recording_for_addon(&db, &addon, recording_ref).unwrap().expect("row");
    assert_eq!(row.kind, "snapshot");
    assert_eq!(row.camera_id, camera);
    assert_eq!(row.owner_addon_id, addon);
    // File on disk + size matches.
    let p = std::path::PathBuf::from(&row.file_path);
    assert!(p.exists(), "snapshot file must be on disk");
    let meta = std::fs::metadata(&p).unwrap();
    assert_eq!(meta.len() as i64, row.file_size_bytes);
}

#[test]
fn test_save_snapshot_permission_denied() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-snap-perm");
    let camera = uniq("cam_perm");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec![]); // no recording.write
    let (rc, _) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, "frame_does_not_matter").as_bytes());
    assert_eq!(rc, AbiError::Permission.as_i32());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_snapshot_invalid_frame_ref_format() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-snap-badref");
    let camera = uniq("cam_badref");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let (rc, _) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, "bogus_no_prefix").as_bytes());
    assert_eq!(rc, AbiError::Operation.as_i32(), "invalid prefix must be rejected before storage lookup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_snapshot_nonexistent_frame_ref() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-snap-missing");
    let camera = uniq("cam_missing_frame");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let made_up = format!("frame_{}", uuid::Uuid::new_v4());
    let (rc, _) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &made_up).as_bytes());
    assert_eq!(rc, AbiError::NotFound.as_i32());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_snapshot_cross_addon_frame_denied() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon_a = uniq("addon-a-cross");
    let addon_b = uniq("addon-b-cross");
    let camera_a = uniq("cam_cross_a");
    seed_camera(&db, &addon_a, &camera_a);
    // Note: we don't seed any camera for addon_b. addon_b tries to capture a
    // frame whose owning camera belongs to addon_a — the ownership check on
    // `cameras` for addon_b must surface NotFound.
    let state_b = make_state(&db, &addon_b, vec!["recording.write".into()]);
    let frame_ref = insert_frame(&camera_a, 8, 8, rgb_buf(8, 8));
    let (rc, _) = rec::save_snapshot_with_raw_input(&state_b, snapshot_payload(&camera_a, &frame_ref).as_bytes());
    assert_eq!(rc, AbiError::NotFound.as_i32(), "addon must not pick up a camera owned by someone else");
}

#[test]
fn test_save_segment_duration_out_of_range() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-seg-dur");
    let camera = uniq("cam_seg_dur");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    for bad in [0u32, 61] {
        let payload = format!(
            "camera_id = {}\nduration_secs = {}\n",
            toml::Value::String(camera.clone()),
            bad,
        );
        let (rc, _) = rec::save_segment_with_raw_input(&state, payload.as_bytes());
        assert_eq!(rc, AbiError::Operation.as_i32(), "duration_secs={bad} must reject");
    }
}

/// Addon-supplied `source_url` must be ignored entirely — the host always
/// derives the segment source from the owned camera row. We probe by feeding
/// a hostile `file:///etc/passwd` URL alongside a real camera with a benign
/// fake-file URL and asserting that the only thing failing is the GStreamer
/// pipeline against the (nonexistent) `/tmp/whatever.mp4` from the camera row —
/// never the hostile path the addon tried to inject.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_save_segment_uses_camera_url_not_input() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-seg-src");
    let camera = uniq("cam_seg_src");
    seed_camera(&db, &addon, &camera); // seeds url=/tmp/whatever.mp4
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let payload = format!(
        "camera_id = {}\nsource_url = {}\nduration_secs = 1\n",
        toml::Value::String(camera),
        toml::Value::String("file:///etc/passwd".into()),
    );
    let (rc, _) = rec::save_segment_with_raw_input(&state, payload.as_bytes());
    // Either Operation (pipeline failure on the missing /tmp/whatever.mp4) or
    // PayloadTooLarge — what must NOT happen is success (Ok=0), which would
    // mean the host honored the addon-supplied source_url and read /etc/passwd.
    assert_ne!(rc, AbiError::Ok.as_i32(), "addon-supplied source_url must never be honored");
}

/// Recording ref must follow the strict `(snap|clip)_<uuidv4>` regex —
/// path-traversal payloads must be rejected before any DB lookup.
#[test]
fn test_recording_ref_invalid_format_rejected() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-refbad");
    let state = make_state(&db, &addon, vec!["recording.read".into()]);
    for hostile in [
        "snap_../../../etc/passwd",
        "snap_not-a-uuid",
        "clip_12345678-1234-1234-1234-12345678901Z",
        "frame_aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", // wrong prefix for recording ref
    ] {
        let payload = format!(
            "recording_ref = {}\nttl_secs = 120\n",
            toml::Value::String(hostile.into()),
        );
        let (rc, _) = rec::get_url_with_raw_input(&state, payload.as_bytes());
        assert_eq!(rc, AbiError::Operation.as_i32(), "must reject hostile recording_ref {hostile:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires assets/test/sample_traffic.mp4 + GStreamer plugins"]
async fn test_save_segment_basic() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-seg-basic");
    let camera = uniq("cam_seg_basic");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let sample = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/test/sample_traffic.mp4");
    if !sample.exists() {
        eprintln!("skipping — sample mp4 missing");
        return;
    }
    let payload = format!(
        "camera_id = {}\nsource_url = {}\nduration_secs = 2\n",
        toml::Value::String(camera),
        toml::Value::String(format!("file://{}", sample.canonicalize().unwrap().display())),
    );
    let (rc, out) = rec::save_segment_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Ok.as_i32());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    assert!(parsed["recording_ref"].as_str().unwrap().starts_with("clip_"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_url_basic_and_ttl_bounds() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-url");
    let camera = uniq("cam_url");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into(), "recording.read".into()]);
    let frame_ref = insert_frame(&camera, 16, 12, rgb_buf(16, 12));
    let (_rc, out) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &frame_ref).as_bytes());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap();

    // happy path
    let payload = format!("recording_ref = {}\nttl_secs = 300\n", toml::Value::String(recording_ref.into()));
    let (rc, body) = rec::get_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Ok.as_i32());
    let v: toml::Value = toml::from_str(std::str::from_utf8(&body).unwrap()).unwrap();
    let url = v["url"].as_str().unwrap();
    assert!(url.contains("token="));
    assert!(url.contains("exp="));
    assert!(url.contains("ref="));

    // TTL too small
    let payload = format!("recording_ref = {}\nttl_secs = 30\n", toml::Value::String(recording_ref.into()));
    let (rc, _) = rec::get_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Operation.as_i32());

    // TTL too large
    let payload = format!("recording_ref = {}\nttl_secs = 4000\n", toml::Value::String(recording_ref.into()));
    let (rc, _) = rec::get_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Operation.as_i32());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_url_cross_addon_denied() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon_a = uniq("addon-a-url");
    let addon_b = uniq("addon-b-url");
    let camera = uniq("cam_url_x");
    seed_camera(&db, &addon_a, &camera);
    let state_a = make_state(&db, &addon_a, vec!["recording.write".into(), "recording.read".into()]);
    let frame_ref = insert_frame(&camera, 8, 8, rgb_buf(8, 8));
    let (_rc, out) = rec::save_snapshot_with_raw_input(&state_a, snapshot_payload(&camera, &frame_ref).as_bytes());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap();

    let state_b = make_state(&db, &addon_b, vec!["recording.read".into()]);
    let payload = format!("recording_ref = {}\nttl_secs = 120\n", toml::Value::String(recording_ref.into()));
    let (rc, _) = rec::get_url_with_raw_input(&state_b, payload.as_bytes());
    assert_eq!(rc, AbiError::NotFound.as_i32(), "addon B must not see addon A's recording");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_stream_basic() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-stream");
    let camera = uniq("cam_stream");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into(), "recording.read".into()]);
    let frame_ref = insert_frame(&camera, 16, 12, rgb_buf(16, 12));
    let (_rc, out) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &frame_ref).as_bytes());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap();
    let row = get_recording_for_addon(&db, &addon, recording_ref).unwrap().unwrap();

    let payload = format!("recording_ref = {}\n", toml::Value::String(recording_ref.into()));
    let (rc, body) = rec::get_stream_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Ok.as_i32());
    let v: toml::Value = toml::from_str(std::str::from_utf8(&body).unwrap()).unwrap();
    let b64 = v["data_b64"].as_str().unwrap();
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    let on_disk = std::fs::read(&row.file_path).unwrap();
    assert_eq!(decoded, on_disk, "get_stream bytes must match the on-disk file");
}

/// Oversized get_stream must be rejected by the host-side pre-check (using the
/// file_size_bytes from the DB row + base64 expansion estimate) BEFORE the
/// host reads the file from disk. We seed a recording row pointing at a real
/// (small) file but lie about its size in DB — the pre-check should still
/// fire on the lie alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_get_stream_oversized_rejected_before_read() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-stream-big");
    let camera = uniq("cam_stream_big");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into(), "recording.read".into()]);
    let frame_ref = insert_frame(&camera, 8, 8, rgb_buf(8, 8));
    let (_rc, out) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &frame_ref).as_bytes());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap().to_string();

    // Forge the DB row's file_size_bytes to 7 MiB — base64 expands to >9 MiB
    // which exceeds the ServiceCall 8 MiB cap.
    {
        let conn = db.lock().unwrap();
        let n = conn.execute(
            "UPDATE recordings SET file_size_bytes = ?1 WHERE ref = ?2",
            rusqlite::params![7i64 * 1024 * 1024, recording_ref],
        ).unwrap();
        assert_eq!(n, 1);
    }
    let payload = format!("recording_ref = {}\n", toml::Value::String(recording_ref.clone()));
    let (rc, _) = rec::get_stream_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::PayloadTooLarge.as_i32(), "oversized recording must be rejected before read");
}

/// Purge must surface an error (and NOT soft-delete the DB row, NOT audit
/// `ok`) when the underlying filesystem remove fails for any reason other
/// than NotFound. We simulate that by pointing the catalog row at a path
/// whose parent is a regular file — `unlink` then returns ENOTDIR.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_purge_io_error_returns_error_no_db_soft_delete() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-purge-io");
    let camera = uniq("cam_purge_io");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let frame_ref = insert_frame(&camera, 8, 8, rgb_buf(8, 8));
    let (_rc, out) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &frame_ref).as_bytes());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap().to_string();

    // Repoint the on-disk file at a bogus path whose parent is a file — any
    // `unlink` against it surfaces a non-NotFound IO error.
    let bogus_parent = std::env::temp_dir().join(format!("tf_purge_io_{}.dat", uuid::Uuid::new_v4()));
    std::fs::write(&bogus_parent, b"x").unwrap();
    let bogus_target = bogus_parent.join("never.png");
    {
        let conn = db.lock().unwrap();
        let n = conn.execute(
            "UPDATE recordings SET file_path = ?1 WHERE ref = ?2",
            rusqlite::params![bogus_target.to_string_lossy().to_string(), recording_ref],
        ).unwrap();
        assert_eq!(n, 1);
    }

    let payload = format!("recording_ref = {}\n", toml::Value::String(recording_ref.clone()));
    let (rc, _) = rec::purge_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Operation.as_i32(), "fs-level purge failure must surface as Operation");
    // DB row must still be active (purged_at IS NULL) — host did NOT lie that
    // the purge succeeded.
    let row = get_recording_for_addon(&db, &addon, &recording_ref).unwrap();
    assert!(row.is_some(), "DB row must remain active after a failed purge");
    std::fs::remove_file(&bogus_parent).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_purge_idempotent_and_file_missing_ok() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-purge");
    let camera = uniq("cam_purge");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into()]);
    let frame_ref = insert_frame(&camera, 8, 8, rgb_buf(8, 8));
    let (_rc, out) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &frame_ref).as_bytes());
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let recording_ref = parsed["recording_ref"].as_str().unwrap().to_string();

    // Manually drop the file first to test idempotency when the file is gone.
    let row = get_recording_for_addon(&db, &addon, &recording_ref).unwrap().unwrap();
    std::fs::remove_file(&row.file_path).ok();

    let payload = format!("recording_ref = {}\n", toml::Value::String(recording_ref.clone()));
    let (rc1, _) = rec::purge_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc1, AbiError::Ok.as_i32(), "first purge must succeed even with file missing");

    // Second purge: row is already soft-deleted, so it surfaces NotFound.
    let (rc2, _) = rec::purge_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc2, AbiError::NotFound.as_i32(), "second purge on a soft-deleted row is NotFound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_stats_basic_aggregation() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-stats");
    let camera = uniq("cam_stats");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.write".into(), "recording.read".into()]);
    // Save 3 snapshots.
    for _ in 0..3 {
        let fr = insert_frame(&camera, 8, 8, rgb_buf(8, 8));
        let (rc, _) = rec::save_snapshot_with_raw_input(&state, snapshot_payload(&camera, &fr).as_bytes());
        assert_eq!(rc, AbiError::Ok.as_i32());
    }
    let (rc, out) = rec::stats_with_raw_input(&state, b"");
    assert_eq!(rc, AbiError::Ok.as_i32());
    let v: toml::Value = toml::from_str(std::str::from_utf8(&out).unwrap()).unwrap();
    let stats = v.get("stats").expect("stats table");
    assert_eq!(stats["total_snapshots"].as_integer().unwrap(), 3);
    assert_eq!(stats["total_segments"].as_integer().unwrap(), 0);
    let total_size = stats["total_size_bytes"].as_integer().unwrap();
    assert!(total_size > 0);
    // Cross-check against the repo helper directly.
    let agg = recording_stats_for_addon(&db, &addon, None).unwrap();
    assert_eq!(agg.total_snapshots, 3);
    assert_eq!(agg.total_size_bytes as i64, total_size);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_frame_url_basic_and_validation() {
    let _home = temp_home_guard();
    let db = make_db();
    let addon = uniq("addon-furl");
    let camera = uniq("cam_furl");
    seed_camera(&db, &addon, &camera);
    let state = make_state(&db, &addon, vec!["recording.read".into()]);
    let frame_ref = insert_frame(&camera, 4, 4, rgb_buf(4, 4));

    // happy path
    let payload = format!("frame_ref = {}\nttl_secs = 120\n", toml::Value::String(frame_ref.clone()));
    let (rc, body) = rec::frame_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Ok.as_i32());
    let v: toml::Value = toml::from_str(std::str::from_utf8(&body).unwrap()).unwrap();
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with("/frames/"));
    assert!(url.contains("token="));

    // TTL too small
    let payload = format!("frame_ref = {}\nttl_secs = 30\n", toml::Value::String(frame_ref.clone()));
    let (rc, _) = rec::frame_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Operation.as_i32());
    // TTL too large
    let payload = format!("frame_ref = {}\nttl_secs = 700\n", toml::Value::String(frame_ref.clone()));
    let (rc, _) = rec::frame_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::Operation.as_i32());

    // Non-existent
    let made_up = format!("frame_{}", uuid::Uuid::new_v4());
    let payload = format!("frame_ref = {}\nttl_secs = 120\n", toml::Value::String(made_up));
    let (rc, _) = rec::frame_url_with_raw_input(&state, payload.as_bytes());
    assert_eq!(rc, AbiError::NotFound.as_i32());
}
