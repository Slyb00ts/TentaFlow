// =============================================================================
// Plik: tests/addon_dotnet_e2e.rs
// Opis: E2E testy addonu .NET (hello-dotnet, NativeAOT-LLVM → wasm32-wasip1).
//       Weryfikuja kontrakt DotnetAdapter: _initialize + prefiksowane eksporty
//       tentaflow_*, alloc/dealloc oraz host functions (log, storage).
//       Wymaga zbudowanego addonu: cd addons/hello-dotnet &&
//       WASI_SDK_PATH=<wasi-sdk> dotnet publish -c Release -r wasi-wasm
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex as ParkingMutex;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions;
use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
use tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::runtime::{
    adapter_for_runtime, compile_module, create_engine, create_linker, instantiate,
};
use tentaflow_core::addon::{AddonManifest, AddonState};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db;

/// Sciezka do WASM zbudowanego przez dotnet publish (build.rs core buduje go
/// automatycznie gdy dotnet SDK + WASI SDK sa dostepne).
const DOTNET_ADDON_WASM: &str =
    "addons/hello-dotnet/bin/Release/net10.0/wasi-wasm/publish/HelloDotnet.wasm";

/// Sciezka do WASM addonu Tłumacz (.NET) — buduje go ta sama sciezka build.rs.
const TRANSLATOR_ADDON_WASM: &str =
    "addons/translator/bin/Release/net10.0/wasi-wasm/publish/Translator.wasm";

fn create_test_db() -> db::DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory DB");
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;",
    )
    .expect("pragmas");
    db::migrations::run(&conn).expect("migrations");
    Arc::new(db::Db::from_connection(conn))
}

fn load_dotnet_wasm() -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(DOTNET_ADDON_WASM);
    std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "Nie udalo sie wczytac WASM z {:?}: {}. Zbuduj addon: \
             cd addons/hello-dotnet && WASI_SDK_PATH=<wasi-sdk> \
             dotnet publish -c Release -r wasi-wasm",
            wasm_path, e
        )
    })
}

fn create_addon_state(db: db::DbPool, permissions: Vec<String>) -> AddonState {
    AddonState {
        addon_id: "hello-dotnet".to_string(),
        instance_id: "dotnet-e2e-001".to_string(),
        user_id: None,
        org_id: None,
        db: db.clone(),
        permissions,
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
        ui_panels: None,
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
    }
}

/// Instancjacja + boot .NET runtime przez _initialize (kontrakt DotnetAdapter:
/// needs_wasi_start). Zwraca gotowa instancje po on_start.
fn boot_dotnet_instance(
    db: db::DbPool,
    permissions: Vec<String>,
) -> (wasmtime::Store<AddonState>, wasmtime::Instance) {
    let wasm_bytes = load_dotnet_wasm();
    let engine = create_engine().expect("engine");
    let module = compile_module(&engine, &wasm_bytes).expect("compile");

    let state = create_addon_state(db, permissions);
    let mut store = wasmtime::Store::new(&engine, state);
    store.set_fuel(10_000_000_000).expect("fuel");
    store.epoch_deadline_trap();
    store.set_epoch_deadline(u64::MAX / 4);

    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("host fns");
    let instance = instantiate(&linker, &mut store, &module).expect("instantiate");

    let adapter = adapter_for_runtime("dotnet").expect("dotnet adapter");
    assert!(adapter.needs_wasi_start());

    // .NET NativeAOT reactor: _initialize bootuje runtime + module initializers
    // (rejestracja AddonRuntime.Register).
    let init = instance
        .get_typed_func::<(), ()>(&mut store, "_initialize")
        .expect("brak eksportu _initialize");
    init.call(&mut store, ()).expect("_initialize trap");

    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, adapter.export_on_start())
        .expect("brak eksportu tentaflow_on_start");
    let rc = on_start.call(&mut store, ()).expect("on_start trap");
    assert_eq!(rc, 0, "tentaflow_on_start powinno zwrocic 0");

    (store, instance)
}

/// Wywoluje tentaflow_on_request z JSON-em narzedzia (ten sam ABI co Rust:
/// (in_ptr, in_len, out_ptr, out_cap, out_len_ptr) -> i32).
fn call_on_request(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    tool_name: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let request_json = serde_json::json!({
        "tool": tool_name,
        "params": params,
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .expect("brak eksportu alloc");

    let input_ptr = alloc_fn
        .call(&mut *store, request_bytes.len() as i32)
        .expect("alloc input");
    assert!(input_ptr > 0, "alloc zwrocil niepoprawny wskaznik");

    let memory = instance
        .get_memory(&mut *store, "memory")
        .expect("brak eksportu memory");
    memory.data_mut(&mut *store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    let out_cap: i32 = 65536;
    let out_ptr = alloc_fn.call(&mut *store, out_cap).expect("alloc out");
    let out_len_ptr = alloc_fn.call(&mut *store, 4).expect("alloc out_len");

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut *store, "tentaflow_on_request")
        .expect("brak eksportu tentaflow_on_request");

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
        .expect("on_request trap");
    assert_eq!(rc, 0, "tentaflow_on_request powinno zwrocic 0");

    let mem_data = memory.data(&*store);
    let out_len = i32::from_le_bytes(
        mem_data[out_len_ptr as usize..out_len_ptr as usize + 4]
            .try_into()
            .unwrap(),
    );
    assert!(out_len > 0, "pusta odpowiedz z addonu");
    let result_bytes = &mem_data[out_ptr as usize..out_ptr as usize + out_len as usize];
    let response: serde_json::Value =
        serde_json::from_slice(result_bytes).expect("odpowiedz nie jest JSON");

    // Zwolnij pamiec guest przez dealloc (kontrakt hosta).
    let dealloc_fn = instance
        .get_typed_func::<(i32, i32), ()>(&mut *store, "dealloc")
        .expect("brak eksportu dealloc");
    dealloc_fn
        .call(&mut *store, (input_ptr, request_bytes.len() as i32))
        .expect("dealloc input");
    dealloc_fn
        .call(&mut *store, (out_ptr, out_cap))
        .expect("dealloc out");
    dealloc_fn
        .call(&mut *store, (out_len_ptr, 4))
        .expect("dealloc out_len");

    response
}

#[test]
fn dotnet_addon_exports_match_adapter_contract() {
    let wasm_bytes = load_dotnet_wasm();
    let engine = create_engine().expect("engine");
    let module = compile_module(&engine, &wasm_bytes).expect("compile");

    let exports: Vec<String> = module.exports().map(|e| e.name().to_string()).collect();
    for name in [
        "_initialize",
        "memory",
        "alloc",
        "dealloc",
        "tentaflow_on_start",
        "tentaflow_on_stop",
        "tentaflow_on_request",
        "tentaflow_on_event",
        "tentaflow_on_tick",
        "tentaflow_on_panel_open",
    ] {
        assert!(
            exports.contains(&name.to_string()),
            "brak wymaganego eksportu '{}' (dostepne: {:?})",
            name,
            exports
        );
    }
}

#[test]
fn dotnet_addon_start_echo_stop() {
    let db = create_test_db();
    let (mut store, instance) =
        boot_dotnet_instance(db, vec!["storage".to_string(), "log".to_string()]);

    let response = call_on_request(
        &mut store,
        &instance,
        "echo",
        serde_json::json!({"text": "hello from rust host"}),
    );
    assert_eq!(response["ok"], true, "echo: {:?}", response);
    assert_eq!(response["data"]["echo"], "hello from rust host");

    let on_stop = instance
        .get_typed_func::<(), i32>(&mut store, "tentaflow_on_stop")
        .expect("brak tentaflow_on_stop");
    assert_eq!(on_stop.call(&mut store, ()).expect("on_stop trap"), 0);
}

#[test]
fn dotnet_addon_storage_roundtrip() {
    let db = create_test_db();
    {
        let conn = db.write().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('hello-dotnet', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .expect("limity zasobow");
    }
    let (mut store, instance) =
        boot_dotnet_instance(db, vec!["storage".to_string(), "log".to_string()]);

    let response = call_on_request(
        &mut store,
        &instance,
        "test_storage",
        serde_json::json!({"key": "dotnet_key", "value": "dotnet_value"}),
    );
    assert_eq!(response["ok"], true, "test_storage: {:?}", response);
    assert_eq!(
        response["data"]["match"], true,
        "storage mismatch: {:?}",
        response
    );
    assert_eq!(response["data"]["read"], "dotnet_value");
}

/// Laduje dowolny zbudowany addon .NET po sciezce wzglednej i bootuje instancje
/// z podanym addon_id (kontrakt DotnetAdapter: _initialize + on_start).
fn boot_named_instance(
    db: db::DbPool,
    addon_id: &str,
    wasm_rel_path: &str,
    permissions: Vec<String>,
) -> (wasmtime::Store<AddonState>, wasmtime::Instance) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(wasm_rel_path);
    let wasm_bytes = std::fs::read(&wasm_path)
        .unwrap_or_else(|e| panic!("Nie udalo sie wczytac WASM z {:?}: {}", wasm_path, e));

    let engine = create_engine().expect("engine");
    let module = compile_module(&engine, &wasm_bytes).expect("compile");

    let mut state = create_addon_state(db, permissions);
    state.addon_id = addon_id.to_string();
    let mut store = wasmtime::Store::new(&engine, state);
    store.set_fuel(10_000_000_000).expect("fuel");
    store.epoch_deadline_trap();
    store.set_epoch_deadline(u64::MAX / 4);

    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("host fns");
    let instance = instantiate(&linker, &mut store, &module).expect("instantiate");

    let adapter = adapter_for_runtime("dotnet").expect("dotnet adapter");
    let init = instance
        .get_typed_func::<(), ()>(&mut store, "_initialize")
        .expect("brak eksportu _initialize");
    init.call(&mut store, ()).expect("_initialize trap");

    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, adapter.export_on_start())
        .expect("brak eksportu tentaflow_on_start");
    assert_eq!(on_start.call(&mut store, ()).expect("on_start trap"), 0);

    (store, instance)
}

/// Wywoluje tentaflow_on_panel_open(panel_id, epoch) — ABI: (ptr, len, i64) -> i32.
fn call_on_panel_open(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    panel_id: &str,
    epoch: i64,
) -> i32 {
    let bytes = panel_id.as_bytes();
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .expect("alloc");
    let ptr = alloc
        .call(&mut *store, bytes.len() as i32)
        .expect("alloc panel_id");
    let memory = instance.get_memory(&mut *store, "memory").expect("memory");
    memory.data_mut(&mut *store)[ptr as usize..ptr as usize + bytes.len()].copy_from_slice(bytes);

    let on_panel_open = instance
        .get_typed_func::<(i32, i32, i64), i32>(&mut *store, "tentaflow_on_panel_open")
        .expect("tentaflow_on_panel_open");
    on_panel_open
        .call(&mut *store, (ptr, bytes.len() as i32, epoch))
        .expect("on_panel_open trap")
}

#[test]
fn translator_addon_exports_match_adapter_contract() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(TRANSLATOR_ADDON_WASM);
    let wasm_bytes = std::fs::read(&wasm_path)
        .unwrap_or_else(|e| panic!("Zbuduj addon translator (dotnet publish): {e}"));
    let engine = create_engine().expect("engine");
    let module = compile_module(&engine, &wasm_bytes).expect("compile");
    let exports: Vec<String> = module.exports().map(|e| e.name().to_string()).collect();
    for name in [
        "_initialize",
        "memory",
        "alloc",
        "dealloc",
        "tentaflow_on_start",
        "tentaflow_on_request",
        "tentaflow_on_panel_open",
    ] {
        assert!(
            exports.contains(&name.to_string()),
            "brak eksportu '{}' w translatorze (dostepne: {:?})",
            name,
            exports
        );
    }
}

#[test]
fn translator_addon_panel_open_renders() {
    let db = create_test_db();
    let (mut store, instance) = boot_named_instance(
        db,
        "translator",
        TRANSLATOR_ADDON_WASM,
        vec!["ui".to_string()],
    );

    // on_panel_open renderuje PanelShell + SlotContent + StatePatch przez
    // ui_render_cbor — musi przejsc bez trapa/bledu (kanoniczny CBOR).
    let rc = call_on_panel_open(&mut store, &instance, "main", 7);
    assert_eq!(rc, 0, "tentaflow_on_panel_open powinno zwrocic 0");
}

/// Kolektuje wszystkie zdarzenia `ui.cbor_message` z ring buffera event busa,
/// ktore pojawily sie od `since` (indeks). Zwraca pary (tag, cbor_bytes).
fn drain_ui_cbor(store: &wasmtime::Store<AddonState>, since: usize) -> Vec<(u16, Vec<u8>)> {
    // recent_events zwraca najnowsze-pierwsze; odwracamy do chronologicznego,
    // zeby skip(since) pominal wczesniejsze wiadomosci a nie najnowsze.
    let mut events = store.data().event_bus.recent_events(1000);
    events.reverse();
    events
        .into_iter()
        .filter(|e| e.event_type == "ui.cbor_message")
        .skip(since)
        .filter_map(|e| {
            let tag = e.payload.get("tag").and_then(|t| t.as_u64())? as u16;
            let cbor = e
                .payload
                .get("cbor")?
                .as_array()?
                .iter()
                .map(|n| n.as_u64().unwrap_or(0) as u8)
                .collect::<Vec<u8>>();
            Some((tag, cbor))
        })
        .collect()
}

fn count_ui_cbor(store: &wasmtime::Store<AddonState>) -> usize {
    store
        .data()
        .event_bus
        .recent_events(1000)
        .into_iter()
        .filter(|e| e.event_type == "ui.cbor_message")
        .count()
}

/// Przechwytuje realne payloady UI (PanelShell + SlotContent + StatePatch) z
/// addonu Tłumacz dla wszystkich czterech trybow i zrzuca je do JSON-a wskaza-
/// nego przez env `TRANSLATOR_CAPTURE_DIR`. Sluzy jako zrodlo dla wizualnego
/// harnessa (renderuje realny sdk-runtime). Bez env — test jest no-op pass.
#[test]
fn translator_capture_payloads() {
    let Ok(out_dir) = std::env::var("TRANSLATOR_CAPTURE_DIR") else {
        return;
    };
    std::fs::create_dir_all(&out_dir).expect("create capture dir");

    let db = create_test_db();
    let (mut store, instance) = boot_named_instance(
        db,
        "translator",
        TRANSLATOR_ADDON_WASM,
        vec!["ui".to_string(), "storage".to_string()],
    );

    // Segmentuje przechwycone wiadomosci per wywolanie (panel_open + set_mode).
    let mut segments: Vec<(String, Vec<(u16, Vec<u8>)>)> = Vec::new();

    let mark = count_ui_cbor(&store);
    let rc = call_on_panel_open(&mut store, &instance, "main", 7);
    assert_eq!(rc, 0, "panel_open trap");
    segments.push(("open".to_string(), drain_ui_cbor(&store, mark)));

    for mode in ["live", "settings", "text"] {
        let mark = count_ui_cbor(&store);
        let resp = call_on_request(
            &mut store,
            &instance,
            "ui.main.set_mode",
            serde_json::json!({ "value": mode, "__panel_epoch": 7 }),
        );
        assert_eq!(resp["ok"], true, "set_mode {mode}: {resp:?}");
        segments.push((mode.to_string(), drain_ui_cbor(&store, mark)));
    }

    // Serializuj: [{ call, messages: [{ tag, cbor: [u8...] }] }]
    let json = serde_json::json!(segments
        .iter()
        .map(|(call, msgs)| serde_json::json!({
            "call": call,
            "messages": msgs
                .iter()
                .map(|(tag, cbor)| serde_json::json!({ "tag": tag, "cbor": cbor }))
                .collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>());

    let path = Path::new(&out_dir).join("payloads.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).expect("write payloads");
    eprintln!("translator payloads captured → {}", path.display());
}

/// Regression guard for mobile (iOS/Android): a .NET NativeAOT `wasm32-wasip1`
/// module MUST instantiate under the `wasmi` interpreter with the production
/// WASI preview1 shim (`runtime::runtime_wasmi::create_linker`), then boot via
/// `_initialize` and return 0 from `tentaflow_on_start`.
///
/// This runs the REAL mobile backend (not a copy): `wasmi-runtime-test` pulls
/// the same wasmi engine + `runtime_wasmi` module the phones use. It caught the
/// original break where the shim was missing 7 preview1 imports (`fd_close`,
/// `fd_fdstat_get`, `fd_prestat_get`, `fd_prestat_dir_name`, `fd_seek`,
/// `poll_oneoff`, `sched_yield`) and every .NET addon failed to instantiate.
///
/// `register_host_functions` is wasmtime-typed, so the `tentaflow` host imports
/// are stubbed directly on the wasmi linker — the shim under test is WASI, not
/// the host ABI (which the wasmtime e2e tests above already exercise).
#[cfg(feature = "wasmi-runtime-test")]
#[test]
fn dotnet_addon_instantiates_under_wasmi_mobile_shim() {
    use tentaflow_core::addon::runtime::runtime_wasmi;
    use tentaflow_core::addon::AddonState;

    let wasm_bytes = load_dotnet_wasm();
    let engine = runtime_wasmi::create_engine().expect("wasmi engine");
    let module = runtime_wasmi::compile_module(&engine, &wasm_bytes).expect("wasmi compile");

    // Real production WASI shim (wire_wasi_preview1) under test.
    let mut linker = runtime_wasmi::create_linker(&engine);

    // Minimal `tentaflow` host stubs matching the module's import signatures.
    type Caller<'a> = runtime_wasmi::WasmCaller<'a, AddonState>;
    linker
        .func_wrap(
            "tentaflow",
            "log_info",
            |_c: Caller<'_>, _p: i32, _n: i32| -> i32 { 0 },
        )
        .expect("log_info");
    linker
        .func_wrap(
            "tentaflow",
            "log_error",
            |_c: Caller<'_>, _p: i32, _n: i32| -> i32 { 0 },
        )
        .expect("log_error");
    linker
        .func_wrap(
            "tentaflow",
            "storage_get",
            |_c: Caller<'_>, _a: i32, _b: i32, _d: i32, _e: i32, _f: i32| -> i32 { -1 },
        )
        .expect("storage_get");
    linker
        .func_wrap(
            "tentaflow",
            "storage_set",
            |_c: Caller<'_>, _a: i32, _b: i32, _d: i32, _e: i32| -> i32 { 0 },
        )
        .expect("storage_set");
    linker
        .func_wrap(
            "tentaflow",
            "ui_render_cbor",
            |_c: Caller<'_>, _a: i32, _b: i32| -> i32 { 0 },
        )
        .expect("ui_render_cbor");

    let db = create_test_db();
    let state = create_addon_state(
        db,
        vec!["storage".to_string(), "log".to_string(), "ui".to_string()],
    );
    let mut store = runtime_wasmi::WasmStore::new(&engine, state);
    store.set_fuel(10_000_000_000).expect("fuel");

    // Real production instantiate (linker.instantiate_and_start under wasmi).
    let instance =
        runtime_wasmi::instantiate(&linker, &mut store, &module).expect("wasmi instantiate");

    // .NET reactor: bootstrap the managed runtime, then lifecycle start.
    let init = instance
        .get_typed_func::<(), ()>(&store, "_initialize")
        .expect("brak eksportu _initialize");
    init.call(&mut store, ())
        .expect("_initialize trap under wasmi");

    let on_start = instance
        .get_typed_func::<(), i32>(&store, "tentaflow_on_start")
        .expect("brak eksportu tentaflow_on_start");
    assert_eq!(
        on_start
            .call(&mut store, ())
            .expect("on_start trap under wasmi"),
        0,
        "tentaflow_on_start powinno zwrocic 0 pod wasmi"
    );
}

#[test]
fn dotnet_addon_permission_denied_without_storage() {
    let db = create_test_db();
    let (mut store, instance) = boot_dotnet_instance(db, vec!["log".to_string()]);

    let response = call_on_request(
        &mut store,
        &instance,
        "test_storage",
        serde_json::json!({"key": "k", "value": "v"}),
    );
    // storage_set zwraca ABI_ERR_PERMISSION → SDK rzuca → AddonBase zwraca
    // JSON-owy error envelope.
    assert_eq!(
        response["ok"], false,
        "test_storage bez uprawnienia storage powinno zwrocic ok=false: {:?}",
        response
    );
}
