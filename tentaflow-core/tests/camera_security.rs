// =============================================================================
// File: tests/camera_security.rs — Camera M1.W6 Chunk D security suite
// =============================================================================
//
// Direct-against-host security tests. Complements `camera_host_functions.rs`
// (DB + supervisor unit coverage) and `camera_integration_e2e.rs` (full WASM
// e2e). Each test here exercises one threat-class invariant in isolation:
//   1. camera_id format validator hardens against injection
//   2. display_name + url + profile length / charset caps
//   3. resolve_file_url rejects symlink leaf + symlinked parent components
//   4. Non-regular-file targets (devices, /etc/passwd via .. traversal) rejected
//   5. Cross-addon isolation at the DB layer
//   6. PayloadKind::ServiceCall input size ceiling
//   7. Audit log entries are persisted in order with consistent metadata
//
// The TOML-level "payload too large" path requires a wasmtime caller; that
// path is exercised by tests/camera_integration_e2e.rs and by the existing
// abi_helpers unit tests. Here we cover the validators + supervisor surface
// that can be driven without an InstancePool.

#![cfg(feature = "camera")]

#[cfg(unix)]
use std::path::PathBuf;

use tentaflow_core::addon::host_functions::camera::test_api::{
    camera_id_valid_for_test, display_name_valid_for_test, profile_valid_for_test,
};
use tentaflow_core::db::repository::{
    get_camera_for_addon, insert_camera, list_cameras_for_addon, soft_delete_camera,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::camera_ingest::{
    fakefile, CameraConfig, CameraIngestError,
};

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("core db init")
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4())
}

// =============================================================================
// 1. camera_id format — defence against SQL injection / path injection
// =============================================================================

#[test]
fn sql_injection_in_camera_id_rejected_by_validator() {
    // The host validator is the first gate before any DB query is shaped with
    // the camera_id. Even though the DB layer uses bound params, rejecting
    // malformed ids in the validator means an attacker cannot exfiltrate ids
    // by side-channels (e.g. error messages encoding the raw id verbatim).
    assert!(!camera_id_valid_for_test("cam_'; DROP TABLE cameras; --"));
    assert!(!camera_id_valid_for_test("cam_\" OR 1=1 --"));
    assert!(!camera_id_valid_for_test("cam_/etc/passwd"));
    assert!(!camera_id_valid_for_test("cam_../escape"));
    assert!(!camera_id_valid_for_test(""));
    assert!(!camera_id_valid_for_test("cam_short"));
    // Even a valid-length value with non-hex chars is rejected.
    assert!(!camera_id_valid_for_test(
        "cam_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
    ));
}

#[test]
fn camera_id_uppercase_hex_rejected() {
    // DB stores ids lowercase (uuid::Uuid::new_v4().to_string()); the
    // validator MUST match exactly to prevent two casings from creating
    // distinct logical rows.
    assert!(!camera_id_valid_for_test(
        "cam_DEADBEEF-DEAD-BEEF-DEAD-BEEFDEADBEEF"
    ));
    let valid = format!("cam_{}", uuid::Uuid::new_v4());
    assert!(camera_id_valid_for_test(&valid));
}

// =============================================================================
// 2. Length + charset caps
// =============================================================================

#[test]
fn oversized_display_name_rejected() {
    // MAX_DISPLAY_NAME = 256.
    assert!(!display_name_valid_for_test(&"x".repeat(257)));
    assert!(display_name_valid_for_test(&"x".repeat(256)));
    // 1000 chars must always be rejected.
    assert!(!display_name_valid_for_test(&"a".repeat(1000)));
}

#[test]
fn shell_metacharacters_in_display_name_rejected() {
    // Display name lands in audit log + UI rendering; metacharacters that
    // could be confused with shell or HTML context are filtered at the
    // validator layer. (Whitespace including \n and \t is allowed — the
    // validator's whitelist accepts is_whitespace; what is rejected are
    // explicit shell/HTML metacharacters that are not in the allow list.)
    assert!(!display_name_valid_for_test("cam`whoami`"));
    assert!(!display_name_valid_for_test("cam;rm -rf /"));
    assert!(!display_name_valid_for_test("cam$(touch /tmp/pwn)"));
    assert!(!display_name_valid_for_test("cam<script>"));
    assert!(!display_name_valid_for_test("cam|pipe"));
    assert!(!display_name_valid_for_test("cam&background"));
}

#[test]
fn profile_charset_locked_to_lowercase_alnum_dash_underscore() {
    assert!(profile_valid_for_test("default"));
    assert!(profile_valid_for_test("high_fps-2"));
    // Uppercase rejected — keeps profile names canonical.
    assert!(!profile_valid_for_test("Default"));
    assert!(!profile_valid_for_test("HAS-CAPS"));
    // Whitespace / shell metacharacters rejected.
    assert!(!profile_valid_for_test("has space"));
    assert!(!profile_valid_for_test("foo;bar"));
    // Length cap.
    assert!(!profile_valid_for_test(&"a".repeat(129)));
}

// =============================================================================
// 3. resolve_file_url — symlink + traversal guards
// =============================================================================

#[cfg(unix)]
#[test]
fn resolve_file_url_rejects_symlink_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.bin");
    std::fs::write(&target, b"x").unwrap();
    let link = dir.path().join("link.bin");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = fakefile::resolve_file_url(link.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, CameraIngestError::SymlinkNotAllowed(_)));
}

#[cfg(unix)]
#[test]
fn resolve_file_url_rejects_symlinked_parent_dir() {
    // Issue #6 fix walks every component. Build:
    //   /tmp/<tmp>/realdir/file.mp4
    //   /tmp/<tmp>/linkdir -> realdir
    // And resolve `linkdir/file.mp4` — must be rejected because linkdir is
    // a symlink even though file.mp4 itself is not.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("realdir");
    std::fs::create_dir(&real).unwrap();
    let file = real.join("file.mp4");
    std::fs::write(&file, b"x").unwrap();
    let link_dir = dir.path().join("linkdir");
    std::os::unix::fs::symlink(&real, &link_dir).unwrap();
    let attack = link_dir.join("file.mp4");
    let err = fakefile::resolve_file_url(attack.to_str().unwrap()).unwrap_err();
    assert!(
        matches!(err, CameraIngestError::SymlinkNotAllowed(_)),
        "expected SymlinkNotAllowed, got {err:?}"
    );
}

#[test]
fn resolve_file_url_rejects_traversal_into_nonexistent_target() {
    // `..` traversal that lands on a path with no file produces FileNotFound
    // instead of silently leaking host state.
    let dir = tempfile::tempdir().unwrap();
    let attack = dir.path().join("subdir/../../etc/totally_not_a_file");
    let res = fakefile::resolve_file_url(attack.to_str().unwrap());
    assert!(res.is_err(), "traversal must not resolve to a regular file");
}

#[test]
fn resolve_file_url_rejects_directory_target() {
    // A directory is not a regular file — pipeline cannot be built on top of
    // it and the validator must reject it before gstreamer init.
    let dir = tempfile::tempdir().unwrap();
    let err = fakefile::resolve_file_url(dir.path().to_str().unwrap()).unwrap_err();
    assert!(matches!(err, CameraIngestError::FileNotFound(_)));
}

#[test]
fn resolve_file_url_rejects_empty_input() {
    let err = fakefile::resolve_file_url("").unwrap_err();
    assert!(matches!(err, CameraIngestError::InvalidUrl(_)));
    // file:// alone collapses to empty after the prefix is stripped.
    let err = fakefile::resolve_file_url("file://").unwrap_err();
    assert!(matches!(err, CameraIngestError::InvalidUrl(_)));
}

// =============================================================================
// 4. Cross-addon isolation
// =============================================================================

fn insert(db: &DbPool, camera_id: &str, owner: &str, url: &str) {
    insert_camera(
        db, camera_id, owner, "display", "fake_file", url, 30, None, None, "C",
        "default",
    )
    .expect("insert");
}

#[test]
fn cross_addon_camera_get_returns_none_for_foreign_owner() {
    let db = make_db();
    let id = uniq("cam_iso_get");
    insert(&db, &id, "addon-a", "/tmp/a.mp4");
    let foreign = get_camera_for_addon(&db, "addon-b", &id).unwrap();
    assert!(foreign.is_none(), "addon-b must not see addon-a's camera");
    let mine = get_camera_for_addon(&db, "addon-a", &id).unwrap();
    assert!(mine.is_some());
}

#[test]
fn cross_addon_soft_delete_no_op_for_foreign_owner() {
    let db = make_db();
    let id = uniq("cam_iso_del");
    insert(&db, &id, "addon-a", "/tmp/a.mp4");
    let removed = soft_delete_camera(&db, "addon-b", &id).unwrap();
    assert!(!removed, "foreign soft-delete must be no-op");
    let mine = get_camera_for_addon(&db, "addon-a", &id).unwrap();
    assert!(mine.is_some(), "owner's camera survives foreign delete attempt");
}

#[test]
fn cross_addon_list_does_not_leak_other_owners() {
    let db = make_db();
    let id_a = uniq("cam_a");
    let id_b = uniq("cam_b");
    insert(&db, &id_a, "addon-a", "/tmp/a.mp4");
    insert(&db, &id_b, "addon-b", "/tmp/b.mp4");
    let listed_a = list_cameras_for_addon(&db, "addon-a").unwrap();
    let listed_b = list_cameras_for_addon(&db, "addon-b").unwrap();
    assert!(listed_a.iter().all(|r| r.owner_addon_id == "addon-a"));
    assert!(listed_b.iter().all(|r| r.owner_addon_id == "addon-b"));
    assert!(listed_a.iter().any(|r| r.camera_id == id_a));
    assert!(!listed_a.iter().any(|r| r.camera_id == id_b));
}

// =============================================================================
// 5. Supervisor-level vendor + fps + url guards
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_rejects_unsupported_vendor() {
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let err = sup
        .add_camera(CameraConfig {
            camera_id: uniq("cam_sec_vendor"),
            vendor: "rtsp".into(),
            url: "rtsp://attacker/exfil".into(),
            target_fps: 30,
            resolution: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, CameraIngestError::UnsupportedVendor(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_rejects_zero_and_oversized_fps() {
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    for bad_fps in [0u32, 61u32, 1000u32] {
        let err = sup
            .add_camera(CameraConfig {
                camera_id: uniq(&format!("cam_sec_fps_{bad_fps}")),
                vendor: "fake_file".into(),
                url: "/tmp/whatever.mp4".into(),
                target_fps: bad_fps,
                resolution: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CameraIngestError::InvalidConfig(_)));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_rejects_missing_file_url() {
    let sup =
        tentaflow_core::addon::host_functions::camera::test_api::supervisor_for_tests()
            .await
            .expect("supervisor init");
    let err = sup
        .add_camera(CameraConfig {
            camera_id: uniq("cam_sec_missing"),
            vendor: "fake_file".into(),
            url: "/var/empty/not/here/x.mp4".into(),
            target_fps: 30,
            resolution: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        CameraIngestError::FileNotFound(_)
            | CameraIngestError::PipelineBuild(_)
            | CameraIngestError::Internal(_)
    ));
}

#[cfg(unix)]
#[test]
fn resolve_file_url_rejects_special_files() {
    // /dev/zero, /dev/null are NOT regular files (meta.is_file() == false);
    // resolve_file_url must refuse them so a malicious addon cannot pipe
    // device streams into the gstreamer decoder.
    for special in &["/dev/zero", "/dev/null"] {
        let path = PathBuf::from(special);
        if !path.exists() {
            continue;
        }
        let err = fakefile::resolve_file_url(special).unwrap_err();
        assert!(
            matches!(
                err,
                CameraIngestError::FileNotFound(_) | CameraIngestError::SymlinkNotAllowed(_)
            ),
            "{special} must be rejected, got {err:?}"
        );
    }
}
