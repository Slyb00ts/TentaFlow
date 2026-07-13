// =============================================================================
// File: tests/e2e_smoke_cbor_test.rs
// E2E smoke test — validates the full CBOR UI pipeline against the bundled
// sdk-showcase addon: PanelShell on on_start, "increment" action producing
// StatePatch, and all CBOR canonical and decodable by tentaflow-sdk-spec.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

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

const E2E_SMOKE_WASM: &str =
    "../target-addon-wasm/wasm32-wasip1/release/tentaflow_addon_sdk_showcase.wasm";

// =============================================================================
// Fixtures
// =============================================================================

fn create_test_db() -> db::DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-memory DB");
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .expect("pragmas");
    db::migrations::run(&conn).expect("migrations");
    Arc::new(tentaflow_core::db::Db::from_connection(conn))
}

fn load_e2e_smoke_wasm() -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(E2E_SMOKE_WASM);
    std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "Cannot read WASM at {:?}: {}. Build addon first: \
             cd addons/sdk-showcase && cargo build --target wasm32-wasip1 --release",
            wasm_path, e
        )
    })
}

fn create_addon_state(db: db::DbPool, user_id: Option<&str>) -> AddonState {
    let ui_panels = Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));
    AddonState {
        addon_id: "sdk-showcase".to_string(),
        instance_id: "sdk-showcase-test-001".to_string(),
        user_id: user_id.map(str::to_owned),
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
    user_id: Option<&str>,
) -> (
    wasmtime::Store<AddonState>,
    wasmtime::Instance,
    Arc<parking_lot::RwLock<std::collections::HashMap<(String, String, String), Vec<u8>>>>,
) {
    let wasm_bytes = load_e2e_smoke_wasm();
    let engine = create_engine().expect("create engine");
    let module = compile_module(&engine, &wasm_bytes).expect("compile WASM");

    let state = create_addon_state(db, user_id);
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

/// Drains all `ui.cbor_message` events captured by the dispatch channel and
/// decodes them into UiPayloads (in send order). Each message is also checked
/// for canonical encoding.
fn drain_ui_payloads(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<tentaflow_core::addon::event_bus::Event>,
) -> Vec<UiPayload> {
    let mut payloads = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if event.event_type != "ui.cbor_message" {
            continue;
        }
        let cbor: Vec<u8> = event.payload["cbor"]
            .as_array()
            .expect("cbor bytes array")
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        validate_canonical(&cbor).expect("CBOR is not canonical");
        payloads.push(minicbor::decode(&cbor).expect("failed to decode UiPayload"));
    }
    payloads
}

#[test]
fn on_start_emits_canonical_panel_shell_then_initial_slot_content() {
    let db = create_test_db();
    let (mut store, instance, _ui_panels) = create_instance(db, None);

    // Capture every ui_render_cbor message in order via the event-bus
    // dispatch channel (the ui_panels cache only keeps the last message).
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    store.data().event_bus.set_dispatch_sender(tx);

    // Call on_start — addon emits PanelShell + initial SlotContent.
    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start export");
    let result = on_start.call(&mut store, ()).expect("on_start call");
    assert_eq!(result, 0, "on_start returned non-zero");

    let payloads = drain_ui_payloads(&mut rx);
    assert_eq!(payloads.len(), 2, "expected PanelShell + SlotContent");

    match &payloads[0] {
        UiPayload::PanelShell(shell) => {
            assert_eq!(shell.addon_id, "sdk-showcase");
            assert_eq!(shell.panel_id, "main");
            assert_eq!(shell.panel_epoch, 1);
            assert_eq!(shell.slots.len(), 1);
            assert_eq!(shell.slots[0].id, "content");
            // counter, tick_counter, active_tab, demo_result
            assert_eq!(shell.initial_state.len(), 4);
        }
        other => panic!("expected PanelShell, got tag {:?}", other.tag()),
    }

    // The default tab content must follow immediately — without it the
    // content slot stays on its Loading placeholder until a tab click.
    match &payloads[1] {
        UiPayload::SlotContent(content) => {
            assert_eq!(content.addon_id, "sdk-showcase");
            assert_eq!(content.panel_id, "main");
            assert_eq!(content.panel_epoch, 1);
            assert_eq!(content.slot_id, "content");
        }
        other => panic!("expected SlotContent, got tag {:?}", other.tag()),
    }
}

#[test]
fn increment_action_emits_canonical_state_patch() {
    let db = create_test_db();
    let (mut store, instance, ui_panels) = create_instance(db, None);

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
        "sdk-showcase".to_string(),
        "cbor_msg".to_string(),
    );
    let cbor_bytes = cache.get(&key).expect("StatePatch not in ui_panels cache");

    // Verify canonical encoding.
    validate_canonical(cbor_bytes).expect("CBOR is not canonical");

    // Decode and verify StatePatch.
    let payload: UiPayload = minicbor::decode(cbor_bytes).expect("failed to decode UiPayload");

    match &payload {
        UiPayload::StatePatch(patch) => {
            assert_eq!(patch.addon_id, "sdk-showcase");
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
    let (mut store, instance, ui_panels) = create_instance(db, None);

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
        "sdk-showcase".to_string(),
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
    let (mut store, instance, ui_panels) = create_instance(db, None);

    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start");
    on_start.call(&mut store, ()).expect("on_start");

    let cache = ui_panels.read();
    let key = (
        String::new(),
        "sdk-showcase".to_string(),
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

/// Calls the addon's `ui.main.increment` action through on_request.
fn call_increment(store: &mut wasmtime::Store<AddonState>, instance: &wasmtime::Instance) {
    let request_json = serde_json::json!({
        "tool": "ui.main.increment",
        "params": {},
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .expect("alloc export");
    let input_ptr = alloc_fn
        .call(&mut *store, request_bytes.len() as i32)
        .expect("alloc input");
    let memory = instance
        .get_memory(&mut *store, "memory")
        .expect("memory export");
    memory.data_mut(&mut *store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    let out_cap: i32 = 4096;
    let out_ptr = alloc_fn.call(&mut *store, out_cap).expect("alloc output");
    let out_len_ptr = alloc_fn.call(&mut *store, 4).expect("alloc out_len");

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut *store, "on_request")
        .expect("on_request export");
    let result = on_request
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
        .expect("on_request call");
    assert_eq!(result, 0, "on_request returned non-zero");
}

/// Calls the addon's `on_panel_open` export with the given panel id and epoch.
fn call_panel_open(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    panel_id: &str,
    epoch: u64,
) {
    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .expect("alloc export");
    let bytes = panel_id.as_bytes();
    let ptr = alloc_fn
        .call(&mut *store, bytes.len() as i32)
        .expect("alloc panel_id");
    let memory = instance
        .get_memory(&mut *store, "memory")
        .expect("memory export");
    memory.data_mut(&mut *store)[ptr as usize..ptr as usize + bytes.len()].copy_from_slice(bytes);

    let on_panel_open = instance
        .get_typed_func::<(i32, i32, i64), i32>(&mut *store, "on_panel_open")
        .expect("on_panel_open export");
    let result = on_panel_open
        .call(&mut *store, (ptr, bytes.len() as i32, epoch as i64))
        .expect("on_panel_open call");
    assert_eq!(result, 0, "on_panel_open returned non-zero");
}

/// Regression: closing and reopening the panel resets the host-side expected
/// state revision (open_panel → revision 0, fresh epoch). The addon must adopt
/// the new epoch in on_panel_open, restart its own revision counter and stop
/// advancing it on rejected patches — otherwise every StatePatch after reopen
/// is rejected with "state revision mismatch".
#[test]
fn panel_reopen_resets_state_revision() {
    use tentaflow_core::addon::ui_session;

    let db = create_test_db();
    // Distinct user_id so the global-registry connection mapping created here
    // never matches the other tests in this binary (their user_id is "").
    let (mut store, instance, ui_panels) = create_instance(db.clone(), Some("reopen-user"));

    // With a concrete user_id the system-call permission bypass does not
    // apply — grant the "ui" permission as an addon default.
    db.write()
        .unwrap()
        .execute(
            "INSERT INTO addon_permission_defaults (addon_id, permission_id, grant_mode) \
             VALUES ('sdk-showcase', 'ui', 'allow')",
            [],
        )
        .expect("grant ui permission");
    store.data().permission_checker.refresh_all();

    ui_session::init_global_registry(Arc::new(ui_session::SessionRegistry::new()));
    let registry = ui_session::global_registry().expect("global registry");

    const CONN_ID: u64 = 7;
    let session_lock = registry.get_or_create(CONN_ID);

    // First panel session: epoch 1, expected revision starts at 0.
    let epoch1 = session_lock
        .lock()
        .open_panel("sdk-showcase", "main")
        .expect("open panel");
    assert_eq!(epoch1, 1);
    registry.register_addon_connection("sdk-showcase", "reopen-user", CONN_ID);

    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("on_start export");
    assert_eq!(on_start.call(&mut store, ()).expect("on_start call"), 0);
    assert!(
        session_lock
            .lock()
            .validate_slot_ownership("sdk-showcase", "main", "content")
            .is_ok(),
        "PanelShell was rejected by session validation"
    );

    // Two accepted patches: host expected revision advances 0 → 1 → 2.
    call_increment(&mut store, &instance);
    call_increment(&mut store, &instance);
    assert_eq!(
        session_lock
            .lock()
            .get_panel("sdk-showcase", "main")
            .unwrap()
            .state_revision,
        2
    );

    // Panel closed; a service tick fires while no panel is open. The host
    // rejects the patch (panel_not_open) and the addon must NOT advance its
    // local revision counter on that rejection.
    session_lock.lock().close_panel("sdk-showcase", "main");
    let on_tick = instance
        .get_typed_func::<i64, i32>(&mut store, "on_tick")
        .expect("on_tick export");
    assert_eq!(on_tick.call(&mut store, 0).expect("on_tick call"), 0);

    // Reopen on the same connection: fresh PanelOwnership with epoch 2 and
    // expected revision reset to 0. The host then calls on_panel_open.
    let epoch2 = session_lock
        .lock()
        .open_panel("sdk-showcase", "main")
        .expect("reopen panel");
    assert_eq!(epoch2, 2);
    call_panel_open(&mut store, &instance, "main", epoch2);

    // on_panel_open re-sends PanelShell then the initial SlotContent; the
    // single-slot cache holds the last ACCEPTED message. SlotContent with the
    // new epoch proves the re-sent shell registered and the slot push passed
    // session validation.
    {
        let cache = ui_panels.read();
        let key = (
            "reopen-user".to_string(),
            "sdk-showcase".to_string(),
            "cbor_msg".to_string(),
        );
        let cbor_bytes = cache.get(&key).expect("SlotContent not in ui_panels cache");
        validate_canonical(cbor_bytes).expect("CBOR is not canonical");
        let payload: UiPayload = minicbor::decode(cbor_bytes).expect("decode UiPayload");
        match &payload {
            UiPayload::SlotContent(content) => {
                assert_eq!(content.panel_epoch, 2);
                assert_eq!(content.slot_id, "content");
            }
            other => panic!(
                "expected SlotContent after reopen, got tag {:?}",
                other.tag()
            ),
        }
    }

    // The next patch must be ACCEPTED with the reset revision (0 → 1) — this
    // is exactly the case that previously failed with "state revision
    // mismatch: expected 0, got <drifted>".
    call_increment(&mut store, &instance);
    assert_eq!(
        session_lock
            .lock()
            .get_panel("sdk-showcase", "main")
            .unwrap()
            .state_revision,
        1,
        "StatePatch after reopen was rejected — revision did not reset"
    );

    let cache = ui_panels.read();
    let key = (
        "reopen-user".to_string(),
        "sdk-showcase".to_string(),
        "cbor_msg".to_string(),
    );
    let cbor_bytes = cache.get(&key).expect("StatePatch not in ui_panels cache");
    validate_canonical(cbor_bytes).expect("CBOR is not canonical");
    let payload: UiPayload = minicbor::decode(cbor_bytes).expect("decode UiPayload");
    match &payload {
        UiPayload::StatePatch(patch) => {
            assert_eq!(patch.panel_epoch, 2);
            assert_eq!(patch.base_revision, 0);
            assert_eq!(patch.new_revision, 1);
            // Counter restarted with the panel session.
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
fn catalog_tabs_emit_canonical_slot_content() {
    let db = create_test_db();
    let (mut store, instance, ui_panels) = create_instance(db, None);

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

    let tabs = [
        "live",
        "molecules",
        "layout",
        "data",
        "form",
        "action",
        "feedback",
        "specialized",
        "storage",
    ];

    for tab in tabs {
        let request_json = serde_json::json!({
            "tool": "ui.main.panel-navigate",
            "params": { "item_id": tab },
            "user_id": 1,
        });
        let request_bytes = serde_json::to_vec(&request_json).unwrap();

        let input_ptr = alloc_fn
            .call(&mut store, request_bytes.len() as i32)
            .expect("alloc input");
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        memory.data_mut(&mut store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
            .copy_from_slice(&request_bytes);

        let out_cap: i32 = 65536;
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
        assert_eq!(result, 0, "panel-navigate '{}' returned non-zero", tab);

        // The last UI message must be a canonical SlotContent for slot 'content'.
        let cache = ui_panels.read();
        let key = (
            String::new(),
            "sdk-showcase".to_string(),
            "cbor_msg".to_string(),
        );
        let cbor_bytes = cache.get(&key).expect("SlotContent not in ui_panels cache");

        validate_canonical(cbor_bytes)
            .unwrap_or_else(|e| panic!("tab '{}': CBOR not canonical: {:?}", tab, e));

        let payload: UiPayload = minicbor::decode(cbor_bytes)
            .unwrap_or_else(|e| panic!("tab '{}': UiPayload decode failed: {}", tab, e));
        match &payload {
            UiPayload::SlotContent(slot) => {
                assert_eq!(slot.addon_id, "sdk-showcase");
                assert_eq!(slot.panel_id, "main");
                assert_eq!(slot.slot_id, "content");
            }
            other => panic!(
                "tab '{}': expected SlotContent, got tag {:?}",
                tab,
                other.tag()
            ),
        }
    }
}
