// =============================================================================
// File: tests/e2e_smoke_cbor_test.rs
// E2E smoke test — validates the full CBOR UI pipeline: addon emits PanelShell
// on on_start, handles "increment" action producing StatePatch, and all CBOR
// is canonical and decodable by tentaflow-sdk-spec types.
// =============================================================================

use std::path::Path;
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

use tentaflow_sdk_spec::protocol::ui::ui_payload::UiPayload;
use tentaflow_sdk_spec::validate_canonical;

const E2E_SMOKE_WASM: &str = "addons/e2e-smoke/target/wasm32-wasip1/release/e2e_smoke.wasm";

// =============================================================================
// Fixtures
// =============================================================================

fn create_test_db() -> db::DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-memory DB");
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .expect("pragmas");
    db::migrations::run(&conn).expect("migrations");
    Arc::new(Mutex::new(conn))
}

fn load_e2e_smoke_wasm() -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(E2E_SMOKE_WASM);
    std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "Cannot read WASM at {:?}: {}. Build addon first: \
             cd addons/e2e-smoke && cargo build --target wasm32-wasip1 --release",
            wasm_path, e
        )
    })
}

fn create_addon_state(db: db::DbPool) -> AddonState {
    let ui_panels = Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));
    AddonState {
        addon_id: "e2e-smoke".to_string(),
        instance_id: "e2e-smoke-test-001".to_string(),
        user_id: None,
        org_id: None,
        db: db.clone(),
        permissions: vec!["ui".to_string()],
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db)),
        fuel_consumed: 0,
        is_system_call: true,
        rate_limiter: None,
        net_manager: Arc::new(ParkingMutex::new(NetworkConnectionManager::new())),
        settings_cipher: Arc::new(SettingsCipher::new(&[0u8; 32])),
        manifest: Arc::new(AddonManifest::default()),
        memory_limit: 256 * 1024 * 1024,
        router: None,
        oauth_refresh_guard: Arc::new(OAuthRefreshGuard::new()),
        ui_panels: Some(ui_panels),
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

fn create_instance(
    db: db::DbPool,
) -> (
    wasmtime::Store<AddonState>,
    wasmtime::Instance,
    Arc<parking_lot::RwLock<std::collections::HashMap<(String, String, String), Vec<u8>>>>,
) {
    let wasm_bytes = load_e2e_smoke_wasm();
    let engine = create_engine().expect("create engine");
    let module = compile_module(&engine, &wasm_bytes).expect("compile WASM");

    let state = create_addon_state(db);
    let ui_panels = state.ui_panels.clone().unwrap();

    let mut store = wasmtime::Store::new(&engine, state);
    store.set_fuel(1_000_000_000).expect("set fuel");
    store.epoch_deadline_trap();
    store.set_epoch_deadline(100);

    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("register host fns");

    let instance = instantiate(&linker, &mut store, &module).expect("instantiate WASM");

    (store, instance, ui_panels)
}

// =============================================================================
// Tests
// =============================================================================

#[test]
fn on_start_emits_canonical_panel_shell() {
    let db = create_test_db();
    let (mut store, instance, ui_panels) = create_instance(db);

    // Call on_start — addon emits PanelShell via ui_render_cbor.
    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start export");
    let result = on_start.call(&mut store, ()).expect("on_start call");
    assert_eq!(result, 0, "on_start returned non-zero");

    // Verify PanelShell was stored in ui_panels cache.
    let cache = ui_panels.read();
    let key = (
        String::new(),
        "e2e-smoke".to_string(),
        "cbor_msg".to_string(),
    );
    let cbor_bytes = cache.get(&key).expect("PanelShell not in ui_panels cache");

    // Verify canonical CBOR encoding.
    validate_canonical(cbor_bytes).expect("CBOR is not canonical");

    // Decode as UiPayload and verify it's a PanelShell.
    let payload: UiPayload = minicbor::decode(cbor_bytes).expect("failed to decode UiPayload");

    match &payload {
        UiPayload::PanelShell(shell) => {
            assert_eq!(shell.addon_id, "e2e-smoke");
            assert_eq!(shell.panel_id, "main");
            assert_eq!(shell.panel_epoch, 1);
            assert_eq!(shell.slots.len(), 1);
            assert_eq!(shell.slots[0].id, "content");
            assert_eq!(shell.initial_state.len(), 1);
        }
        other => panic!("expected PanelShell, got tag {:?}", other.tag()),
    }
}

#[test]
fn increment_action_emits_canonical_state_patch() {
    let db = create_test_db();
    let (mut store, instance, ui_panels) = create_instance(db);

    // First call on_start to establish the panel.
    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start export");
    on_start.call(&mut store, ()).expect("on_start call");

    // Now call on_request with the increment action.
    let request_json = serde_json::json!({
        "tool": "ui.main.increment",
        "params": {},
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .expect("alloc export");

    let input_ptr = alloc_fn
        .call(&mut store, request_bytes.len() as i32)
        .expect("alloc input");

    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("memory export");
    memory.data_mut(&mut store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    let out_cap: i32 = 4096;
    let out_ptr = alloc_fn.call(&mut store, out_cap).expect("alloc output");
    let out_len_ptr = alloc_fn.call(&mut store, 4).expect("alloc out_len");

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, "on_request")
        .expect("on_request export");

    let result = on_request
        .call(
            &mut store,
            (
                input_ptr,
                request_bytes.len() as i32,
                out_ptr,
                out_cap,
                out_len_ptr,
            ),
        )
        .expect("on_request call");
    assert_eq!(result, 0, "on_request returned non-zero");

    // The StatePatch should now be in the ui_panels cache (overwrites the PanelShell).
    let cache = ui_panels.read();
    let key = (
        String::new(),
        "e2e-smoke".to_string(),
        "cbor_msg".to_string(),
    );
    let cbor_bytes = cache.get(&key).expect("StatePatch not in ui_panels cache");

    // Verify canonical encoding.
    validate_canonical(cbor_bytes).expect("CBOR is not canonical");

    // Decode and verify StatePatch.
    let payload: UiPayload = minicbor::decode(cbor_bytes).expect("failed to decode UiPayload");

    match &payload {
        UiPayload::StatePatch(patch) => {
            assert_eq!(patch.addon_id, "e2e-smoke");
            assert_eq!(patch.panel_id, "main");
            assert_eq!(patch.panel_epoch, 1);
            assert_eq!(patch.base_revision, 0);
            assert_eq!(patch.new_revision, 1);
            assert_eq!(patch.ops.len(), 1);
            assert_eq!(
                patch.ops[0].op,
                tentaflow_sdk_spec::protocol::ui::patch::PatchOpKind::Set {
                    value: tentaflow_sdk_spec::protocol::value::Value::U64(1),
                }
            );
        }
        other => panic!("expected StatePatch, got tag {:?}", other.tag()),
    }
}

#[test]
fn multiple_increments_advance_revision() {
    let db = create_test_db();
    let (mut store, instance, ui_panels) = create_instance(db);

    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start");
    on_start.call(&mut store, ()).expect("on_start");

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .expect("alloc");
    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, "on_request")
        .expect("on_request");

    let request_json = serde_json::json!({
        "tool": "ui.main.increment",
        "params": {},
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let out_cap: i32 = 4096;

    // Call increment 3 times.
    for _ in 0..3 {
        let input_ptr = alloc_fn
            .call(&mut store, request_bytes.len() as i32)
            .expect("alloc input");
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        memory.data_mut(&mut store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
            .copy_from_slice(&request_bytes);

        let out_ptr = alloc_fn.call(&mut store, out_cap).expect("alloc out");
        let out_len_ptr = alloc_fn.call(&mut store, 4).expect("alloc out_len");

        let result = on_request
            .call(
                &mut store,
                (
                    input_ptr,
                    request_bytes.len() as i32,
                    out_ptr,
                    out_cap,
                    out_len_ptr,
                ),
            )
            .expect("on_request");
        assert_eq!(result, 0);
    }

    // Verify the last StatePatch has counter=3 and revision 2->3.
    let cache = ui_panels.read();
    let key = (
        String::new(),
        "e2e-smoke".to_string(),
        "cbor_msg".to_string(),
    );
    let cbor_bytes = cache.get(&key).unwrap();

    validate_canonical(cbor_bytes).expect("canonical");

    let payload: UiPayload = minicbor::decode(cbor_bytes).unwrap();
    match &payload {
        UiPayload::StatePatch(patch) => {
            assert_eq!(patch.base_revision, 2);
            assert_eq!(patch.new_revision, 3);
            assert_eq!(
                patch.ops[0].op,
                tentaflow_sdk_spec::protocol::ui::patch::PatchOpKind::Set {
                    value: tentaflow_sdk_spec::protocol::value::Value::U64(3),
                }
            );
        }
        other => panic!("expected StatePatch, got {:?}", other.tag()),
    }
}

#[test]
fn cbor_roundtrip_bit_identical() {
    let db = create_test_db();
    let (mut store, instance, ui_panels) = create_instance(db);

    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start");
    on_start.call(&mut store, ()).expect("on_start");

    let cache = ui_panels.read();
    let key = (
        String::new(),
        "e2e-smoke".to_string(),
        "cbor_msg".to_string(),
    );
    let original = cache.get(&key).unwrap().clone();

    // Decode then re-encode — must produce identical bytes (canonical determinism).
    let payload: UiPayload = minicbor::decode(&original).unwrap();
    let mut re_encoded = Vec::new();
    minicbor::encode(&payload, &mut re_encoded).unwrap();
    assert_eq!(
        original, re_encoded,
        "re-encoded CBOR differs from original — not canonical"
    );
}
