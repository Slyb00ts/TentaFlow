// =============================================================================
// File: tests/camera_integration_e2e.rs — Camera M1.W6 Chunk D e2e WASM tests
// =============================================================================
//
// Drives the camera_* host functions through a real WASM guest
// (`addons/camera-test-addon`). The addon's `on_request` exposes three tools:
//   - "run_lifecycle"      camera_add -> health -> snapshot -> remove
//   - "run_path_traversal" camera_add with a hostile URL
//   - "run_no_write_probe" surfaces Permission denial when cameras.write is
//                          missing on AddonState
//
// Build prerequisite for every test in this file:
//     cd addons/camera-test-addon && cargo build --target wasm32-wasip1 --release
// All tests are `#[ignore]` so a developer machine without the WASM artifact
// (or without `assets/test/sample_traffic.mp4` for the snapshot variant) is
// not blocked.

#![cfg(feature = "camera")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use parking_lot::Mutex as ParkingMutex;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions;
use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
use tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::runtime::{compile_module, create_engine, create_linker, instantiate};
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db;

const CAMERA_TEST_ADDON_WASM: &str =
    "addons/camera-test-addon/target/wasm32-wasip1/release/tentaflow_addon_camera_test.wasm";

const ADDON_ID: &str = "camera-test-addon";
const INSTANCE_ID: &str = "camera-test-addon-001";

// =============================================================================
// DB + AddonState helpers
// =============================================================================

fn create_test_db() -> db::DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .expect("pragmas");
    db::migrations::run(&conn).expect("migrations");
    Arc::new(Mutex::new(conn))
}

fn load_wasm() -> Option<Vec<u8>> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(CAMERA_TEST_ADDON_WASM);
    std::fs::read(&p).ok()
}

fn sample_path() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test/sample_traffic.mp4");
    p.exists().then_some(p)
}

fn make_state(db: db::DbPool, permissions: Vec<String>) -> AddonState {
    AddonState {
        addon_id: ADDON_ID.to_string(),
        instance_id: INSTANCE_ID.to_string(),
        user_id: None,
        db: db.clone(),
        permissions,
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db)),
        fuel_consumed: 0,
        // System call so check_permission() does not require a user_id.
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

fn create_test_store(
    engine: &wasmtime::Engine,
    state: AddonState,
) -> wasmtime::Store<AddonState> {
    let mut store = wasmtime::Store::new(engine, state);
    store.set_fuel(1_000_000_000).expect("set_fuel");
    store.epoch_deadline_trap();
    store.set_epoch_deadline(100);
    store
}

fn create_wasm_instance(
    db: db::DbPool,
    permissions: Vec<String>,
    wasm_bytes: &[u8],
) -> (wasmtime::Store<AddonState>, wasmtime::Instance) {
    let engine = create_engine().expect("engine");
    let module = compile_module(&engine, wasm_bytes).expect("compile module");
    let state = make_state(db, permissions);
    let mut store = create_test_store(&engine, state);
    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("register host fns");
    let instance = instantiate(&linker, &mut store, &module).expect("instantiate");
    (store, instance)
}

// =============================================================================
// on_request marshaling — JSON in, JSON out (mirrors test-addon dispatcher)
// =============================================================================

fn call_on_request(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_json = serde_json::json!({
        "tool": tool_name,
        "params": params,
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).map_err(|e| e.to_string())?;

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .map_err(|e| format!("alloc lookup: {e}"))?;
    let input_ptr = alloc_fn
        .call(&mut *store, request_bytes.len() as i32)
        .map_err(|e| format!("alloc input: {e}"))?;
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or("memory export missing")?;
    memory.data_mut(&mut *store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    // 16 MiB output buffer accommodates a 1280x720 RGB24 snapshot (~2.6 MiB).
    let out_cap: i32 = 16 * 1024 * 1024;
    let out_ptr = alloc_fn
        .call(&mut *store, out_cap)
        .map_err(|e| format!("alloc out: {e}"))?;
    let out_len_ptr = alloc_fn
        .call(&mut *store, 4)
        .map_err(|e| format!("alloc out_len: {e}"))?;

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut *store, "on_request")
        .map_err(|e| format!("on_request lookup: {e}"))?;
    let rc = on_request
        .call(
            &mut *store,
            (
                input_ptr,
                request_bytes.len() as i32,
                out_ptr,
                out_cap,
                out_len_ptr,
            ),
        )
        .map_err(|e| format!("on_request trap: {e}"))?;
    if rc != 0 {
        return Err(format!("on_request rc={rc}"));
    }
    let data = memory.data(&*store);
    let out_len = i32::from_le_bytes([
        data[out_len_ptr as usize],
        data[out_len_ptr as usize + 1],
        data[out_len_ptr as usize + 2],
        data[out_len_ptr as usize + 3],
    ]);
    let slice = &data[out_ptr as usize..out_ptr as usize + out_len as usize];
    serde_json::from_slice(slice).map_err(|e| format!("parse response: {e}"))
}

// =============================================================================
// Audit log inspector
// =============================================================================

#[derive(Debug)]
struct AuditEntry {
    action: String,
    resource_id: Option<String>,
    result: String,
    error_message: Option<String>,
    risk_class: String,
}

fn fetch_audit_entries(db: &db::DbPool, action_prefix: &str) -> Vec<AuditEntry> {
    let conn = db.lock().expect("lock db");
    let mut stmt = conn
        .prepare(
            "SELECT action, resource_id, result, error_message, risk_class \
             FROM audit_log \
             WHERE addon_id = ?1 AND action LIKE ?2 \
             ORDER BY id ASC",
        )
        .expect("prepare audit query");
    let rows = stmt
        .query_map(
            rusqlite::params![ADDON_ID, format!("{action_prefix}%")],
            |r| {
                Ok(AuditEntry {
                    action: r.get(0)?,
                    resource_id: r.get(1)?,
                    result: r.get(2)?,
                    error_message: r.get(3)?,
                    risk_class: r.get(4)?,
                })
            },
        )
        .expect("query map");
    rows.filter_map(|r| r.ok()).collect()
}

// =============================================================================
// Cross-test serialization — every test in this file touches the singleton
// supervisor; running them serially avoids cross-test interference even though
// each one uses fresh camera_ids.
// =============================================================================

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    L.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires camera-test-addon WASM build + sample_traffic.mp4"]
async fn camera_addon_lifecycle_e2e() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!(
            "camera-test-addon WASM missing — build with: \
             cd addons/camera-test-addon && cargo build --target wasm32-wasip1 --release"
        );
    };
    let Some(sample) = sample_path() else {
        panic!("assets/test/sample_traffic.mp4 missing");
    };
    let db = create_test_db();
    let (mut store, instance) = create_wasm_instance(
        db.clone(),
        vec![
            "cameras.read".into(),
            "cameras.write".into(),
            "cameras.snapshot".into(),
        ],
        &wasm,
    );

    let resp = call_on_request(
        &mut store,
        &instance,
        "run_lifecycle",
        serde_json::json!({"sample_path": sample.to_string_lossy()}),
    )
    .expect("on_request");

    assert_eq!(resp["ok"], serde_json::Value::Bool(true), "resp={resp}");
    let camera_id = resp["camera_id"].as_str().expect("camera_id").to_string();
    assert!(
        camera_id.starts_with("cam_") && camera_id.len() == 4 + 36,
        "camera_id format: {camera_id}"
    );
    assert_eq!(resp["status_after_add"], "starting");

    // Snapshot may or may not have arrived depending on pipeline warmup; both
    // are valid for this milestone. If it did, the buffer must be RGB24 sized.
    if resp["snapshot_ok"] == serde_json::Value::Bool(true) {
        let w = resp["snapshot_width"].as_u64().expect("width");
        let h = resp["snapshot_height"].as_u64().expect("height");
        let len = resp["snapshot_len"].as_u64().expect("len");
        assert!(w > 0 && h > 0, "dimensions: {w}x{h}");
        assert_eq!(len, w * h * 3, "RGB24 bytes mismatch");
    }

    // Verify audit log: every lifecycle stage MUST land an ok entry with the
    // documented risk_class. The lifecycle visits camera.add (A), .health (B),
    // .snapshot (A — Risk A even if the snapshot didn't fire), .remove (A).
    //
    // The snapshot entry is present iff the addon actually called camera_snapshot.
    // The lifecycle tool always issues the call, so the snapshot audit row must
    // exist regardless of whether a frame was buffered (the call audits on both
    // ok and error paths).
    let entries = fetch_audit_entries(&db, "camera.");
    let mine: Vec<&AuditEntry> = entries
        .iter()
        .filter(|e| e.resource_id.as_deref() == Some(camera_id.as_str()))
        .collect();

    let find = |action: &str, result: &str| -> Option<&&AuditEntry> {
        mine.iter().find(|e| e.action == action && e.result == result)
    };

    let add_entry = find("camera.add", "ok").unwrap_or_else(|| {
        panic!("expected camera.add ok audit entry; got {entries:?}");
    });
    assert_eq!(add_entry.risk_class, "A", "camera.add must be risk A");

    let health_entry = find("camera.health", "ok").unwrap_or_else(|| {
        panic!("expected camera.health ok audit entry; got {entries:?}");
    });
    assert_eq!(health_entry.risk_class, "B", "camera.health must be risk B");

    // Snapshot may be ok or error depending on warmup; whichever it is, the
    // audit row MUST be Risk A.
    let snap_entry = mine
        .iter()
        .find(|e| e.action == "camera.snapshot")
        .unwrap_or_else(|| panic!("expected camera.snapshot audit entry; got {entries:?}"));
    assert_eq!(snap_entry.risk_class, "A", "camera.snapshot must be risk A");

    let remove_entry = find("camera.remove", "ok").unwrap_or_else(|| {
        panic!("expected camera.remove ok audit entry; got {entries:?}");
    });
    assert_eq!(remove_entry.risk_class, "A", "camera.remove must be risk A");

    // Lifecycle ordering: add MUST come before health, health before snapshot,
    // snapshot before remove. We assert via the audit_log id ordering which
    // fetch_audit_entries already gives us.
    let pos = |action: &str, result: &str| -> usize {
        mine.iter()
            .position(|e| e.action == action && e.result == result)
            .unwrap_or(usize::MAX)
    };
    let p_add = pos("camera.add", "ok");
    let p_health = pos("camera.health", "ok");
    let p_snap = mine
        .iter()
        .position(|e| e.action == "camera.snapshot")
        .unwrap_or(usize::MAX);
    let p_remove = pos("camera.remove", "ok");
    assert!(
        p_add < p_health && p_health < p_snap && p_snap < p_remove,
        "audit ordering must be add < health < snapshot < remove, got add={p_add} health={p_health} snapshot={p_snap} remove={p_remove}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires camera-test-addon WASM build"]
async fn camera_addon_path_traversal_blocked() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!("camera-test-addon WASM missing");
    };
    let db = create_test_db();
    let (mut store, instance) = create_wasm_instance(
        db.clone(),
        vec![
            "cameras.read".into(),
            "cameras.write".into(),
            "cameras.snapshot".into(),
        ],
        &wasm,
    );

    // Build a hostile path that resolve_file_url MUST reject: a symlinked
    // leaf inside a temp dir. /etc/passwd is a regular file and passes
    // resolve_file_url, so it cannot stand in for "hostile URL"; symlinks
    // are the canonical exfil vector our guards block.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.bin");
    std::fs::write(&target, b"x").unwrap();
    let link = dir.path().join("link.bin");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(not(unix))]
    {
        eprintln!("symlinks unsupported on this platform — skipping");
        return;
    }
    let bad_url = link.to_string_lossy().into_owned();
    let resp = call_on_request(
        &mut store,
        &instance,
        "run_path_traversal",
        serde_json::json!({"bad_url": bad_url}),
    )
    .expect("on_request");

    assert_eq!(resp["ok"], serde_json::Value::Bool(true), "resp={resp}");
    assert_eq!(resp["rejected"], serde_json::Value::Bool(true));
    let abi_error = resp["abi_error"].as_i64().expect("abi_error");
    // Any non-zero abi error code is acceptable — the contract is that the
    // host MUST refuse to register the camera.
    assert!(abi_error != 0, "abi_error must be non-zero");

    // Audit log MUST contain a camera.add error entry whose reason names the
    // guard that fired. resolve_file_url rejects symlinks with
    // SymlinkNotAllowed, which the host audits as session_start_failed
    // wrapping the supervisor error string.
    let entries = fetch_audit_entries(&db, "camera.add");
    let blocked = entries.iter().find(|e| e.result == "error").unwrap_or_else(|| {
        panic!("expected error audit entry for camera.add; got {entries:?}");
    });
    assert_eq!(blocked.risk_class, "A", "camera.add must be risk A");
    let reason = blocked.error_message.as_deref().unwrap_or("");
    assert!(
        reason.contains("symlink_not_allowed") || reason.contains("symlink not allowed") || reason.contains("file_not_found") || reason.contains("file not found"),
        "expected guard reason in error_message, got: {reason:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires camera-test-addon WASM build"]
async fn camera_addon_permission_denied_without_write() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!("camera-test-addon WASM missing");
    };
    let db = create_test_db();
    // Read-only permissions: no cameras.write granted on AddonState.
    let (mut store, instance) =
        create_wasm_instance(db.clone(), vec!["cameras.read".into()], &wasm);

    let resp = call_on_request(
        &mut store,
        &instance,
        "run_no_write_probe",
        serde_json::json!({"sample_path": "/tmp/whatever.mp4"}),
    )
    .expect("on_request");

    assert_eq!(resp["ok"], serde_json::Value::Bool(true), "resp={resp}");
    assert_eq!(resp["granted"], serde_json::Value::Bool(false), "must be denied");

    // Audit log MUST record a camera.add denial with missing_permission reason
    // and risk_class A.
    let entries = fetch_audit_entries(&db, "camera.add");
    let denied = entries
        .iter()
        .find(|e| {
            e.result == "denied"
                && e.error_message
                    .as_deref()
                    .map(|m| m.contains("missing_permission"))
                    .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!("expected denied audit with missing_permission reason; got {entries:?}");
        });
    assert_eq!(denied.risk_class, "A", "permission-denied camera.add must be risk A");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires camera-test-addon WASM build + sample_traffic.mp4 + warmup"]
async fn camera_addon_snapshot_inline_rgb24_after_warmup() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!("camera-test-addon WASM missing");
    };
    let Some(sample) = sample_path() else {
        panic!("sample_traffic.mp4 missing");
    };
    let db = create_test_db();

    // First instantiate addon and register the camera so the pipeline can
    // warm up while we sleep, then request a snapshot via a second tool call.
    let (mut store, instance) = create_wasm_instance(
        db.clone(),
        vec![
            "cameras.read".into(),
            "cameras.write".into(),
            "cameras.snapshot".into(),
        ],
        &wasm,
    );

    // Drive run_lifecycle twice — the second pass usually finds a frame
    // already buffered in the fakefile session. The lifecycle itself is
    // already covered above; here we only assert snapshot bytes.
    let mut got_snapshot = false;
    for _ in 0..3 {
        let resp = call_on_request(
            &mut store,
            &instance,
            "run_lifecycle",
            serde_json::json!({"sample_path": sample.to_string_lossy()}),
        )
        .expect("on_request");
        if resp["snapshot_ok"] == serde_json::Value::Bool(true) {
            let len = resp["snapshot_len"].as_u64().unwrap();
            let w = resp["snapshot_width"].as_u64().unwrap();
            let h = resp["snapshot_height"].as_u64().unwrap();
            assert_eq!(len, w * h * 3, "RGB24 size mismatch");
            assert!(len > 0, "snapshot payload empty");
            got_snapshot = true;
            break;
        }
        // No frame yet — give the pipeline time to push one through.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(got_snapshot, "no snapshot frame observed after retries");
}
