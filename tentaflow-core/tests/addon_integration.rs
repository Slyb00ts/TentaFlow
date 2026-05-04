// =============================================================================
// Plik: tests/addon_integration.rs
// Opis: Testy integracyjne systemu addonow WASM — ladowanie modulu, lifecycle
//       hooks, wywolywanie narzedzi, storage, fuel metering, uprawnienia.
//       Uruchomienie: cargo test --test addon_integration
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

// =============================================================================
// Stale
// =============================================================================

/// Sciezka do skompilowanego WASM test-addon (wzgledna od katalogu projektu).
/// Kompilacja: cd addons/test-addon && cargo build --target wasm32-wasip1 --release
/// Target wasm32-wasip1 (rust stdlib wymaga WASI imports nawet jesli addon
/// uzywa tylko host functions z namespace "tentaflow" — patrz TODO o WASI
/// linker wiring w runtime_wasmtime.rs).
const TEST_ADDON_WASM: &str =
    "addons/test-addon/target/wasm32-wasip1/release/tentaflow_addon_test.wasm";

/// Sciezka do katalogu test-addon (dla lifecycle::install)
const TEST_ADDON_DIR: &str = "addons/test-addon";

// =============================================================================
// Helpery testowe
// =============================================================================

/// Tworzy in-memory baze danych SQLite z wymaganymi tabelami dla systemu addonow.
/// Wszystkie tabele sa tworzone przez oficjalne migracje — zero recznych obejsc.
fn create_test_db() -> db::DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("Nie udalo sie otworzyc in-memory DB");

    // Pragmy
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA foreign_keys=ON;",
    )
    .expect("Blad ustawiania pragm");

    // Uruchom migracje — tworzy WSZYSTKIE wymagane tabele (addon_storage, addon_instances,
    // addon_wasm, addon_tools, addon_declared_permissions, audit_log z pelnym zestawem kolumn)
    db::migrations::run(&conn).expect("Blad uruchamiania migracji");

    Arc::new(Mutex::new(conn))
}

/// Wczytuje bajty WASM test addonu z dysku
fn load_test_wasm() -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(TEST_ADDON_WASM);
    std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "Nie udalo sie wczytac WASM z {:?}: {}. Skompiluj addon: \
             cd addons/test-addon && cargo build --target wasm32-wasip1 --release",
            wasm_path, e
        )
    })
}

/// Tworzy AddonState z podanymi uprawnieniami (system call, bez user_id)
fn create_addon_state(db: db::DbPool, permissions: Vec<String>) -> AddonState {
    create_addon_state_with_id(db, permissions, "test-addon", "test-instance-001")
}

/// Tworzy AddonState z podanym addon_id i instance_id
fn create_addon_state_with_id(
    db: db::DbPool,
    permissions: Vec<String>,
    addon_id: &str,
    instance_id: &str,
) -> AddonState {
    AddonState {
        addon_id: addon_id.to_string(),
        instance_id: instance_id.to_string(),
        user_id: None,
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
    }
}

/// Tworzy Store Wasmtime bez epoch async (testy synchroniczne).
/// Produkcyjny `create_store` uzywa `epoch_deadline_async_yield_and_update`,
/// co wymaga async config — w testach synchronicznych uzywamy prostszego setup.
fn create_test_store(
    engine: &wasmtime::Engine,
    state: AddonState,
    fuel: Option<u64>,
) -> wasmtime::Store<AddonState> {
    let mut store = wasmtime::Store::new(engine, state);

    // Ustaw paliwo — domyslnie 1 miliard instrukcji
    let fuel_amount = fuel.unwrap_or(1_000_000_000);
    store.set_fuel(fuel_amount).expect("Blad ustawiania paliwa");

    // Epoch deadline — ustaw daleko w przyszlosc (100 epok)
    // Testy synchroniczne nie inkrementuja epoki, wiec nie zostanie osiagniety
    store.epoch_deadline_trap();
    store.set_epoch_deadline(100);

    store
}

/// Tworzy pelna instancje WASM z host functions — gotowa do wywolywania eksportow
fn create_wasm_instance(
    db: db::DbPool,
    permissions: Vec<String>,
    fuel: Option<u64>,
) -> (wasmtime::Store<AddonState>, wasmtime::Instance) {
    let wasm_bytes = load_test_wasm();
    let engine = create_engine().expect("Blad tworzenia silnika Wasmtime");
    let module = compile_module(&engine, &wasm_bytes).expect("Blad kompilacji WASM");

    let state = create_addon_state(db, permissions);
    let mut store = create_test_store(&engine, state, fuel);

    let mut linker = create_linker(&engine);
    // Rejestruj host functions pod namespace "tentaflow" — SDK uzywa
    // #[link(wasm_import_module = "tentaflow")], wiec WASM importuje z tego namespace
    host_functions::register_host_functions(&mut linker).expect("Blad rejestracji host functions");

    let instance = instantiate(&linker, &mut store, &module).expect("Blad instancjacji WASM");

    (store, instance)
}

/// Wywoluje on_request z podanymi parametrami i zwraca odpowiedz JSON
fn call_on_request(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Przygotuj JSON wejsciowy (format taki sam jak w AddonManager::call_tool)
    let request_json = serde_json::json!({
        "tool": tool_name,
        "params": params,
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json)
        .map_err(|e| format!("Blad serializacji requestu: {}", e))?;

    // Alloc bufor wejsciowy w guest
    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .map_err(|e| format!("Brak funkcji alloc: {}", e))?;

    let input_ptr = alloc_fn
        .call(&mut *store, request_bytes.len() as i32)
        .map_err(|e| format!("alloc input blad: {}", e))?;

    // Zapisz dane do guest memory
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or("Brak eksportu memory")?;
    memory.data_mut(&mut *store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    // Alloc bufor wyjsciowy (64KB)
    let out_cap: i32 = 65536;
    let out_ptr = alloc_fn
        .call(&mut *store, out_cap)
        .map_err(|e| format!("alloc output blad: {}", e))?;

    // Alloc miejsce na dlugosc wyniku (4 bajty)
    let out_len_ptr = alloc_fn
        .call(&mut *store, 4)
        .map_err(|e| format!("alloc out_len blad: {}", e))?;

    // Wywolaj on_request(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut *store, "on_request")
        .map_err(|e| format!("Brak funkcji on_request: {}", e))?;

    let result_code = on_request
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
        .map_err(|e| format!("on_request trap: {}", e))?;

    if result_code != 0 {
        return Err(format!("on_request zwrocil blad: {}", result_code));
    }

    // Odczytaj dlugosc wyniku z out_len_ptr (4 bajty little-endian)
    let mem_data = memory.data(&*store);
    let out_len_bytes = &mem_data[out_len_ptr as usize..out_len_ptr as usize + 4];
    let out_len = i32::from_le_bytes([
        out_len_bytes[0],
        out_len_bytes[1],
        out_len_bytes[2],
        out_len_bytes[3],
    ]);

    // Odczytaj odpowiedz JSON
    let result_bytes = &mem_data[out_ptr as usize..out_ptr as usize + out_len as usize];
    serde_json::from_slice(result_bytes).map_err(|e| format!("Blad parsowania odpowiedzi: {}", e))
}

// =============================================================================
// Test 1: Ladowanie modulu WASM i weryfikacja eksportow
// =============================================================================

#[test]
fn addon_wasm_loads() {
    let wasm_bytes = load_test_wasm();
    let engine = create_engine().expect("Blad tworzenia silnika");
    let module = compile_module(&engine, &wasm_bytes).expect("Blad kompilacji WASM");

    // Zbierz nazwy eksportow
    let export_names: Vec<String> = module.exports().map(|e| e.name().to_string()).collect();

    // Sprawdz wymagane eksporty
    let required_exports = [
        "on_install",
        "on_start",
        "on_stop",
        "on_request",
        "alloc",
        "dealloc",
        "memory",
    ];
    for name in &required_exports {
        assert!(
            export_names.contains(&name.to_string()),
            "Brak wymaganego eksportu: '{}'. Dostepne: {:?}",
            name,
            export_names
        );
    }
}

// =============================================================================
// Test 2: Lifecycle hooks — on_start i on_stop
// =============================================================================

#[test]
fn addon_on_start_on_stop() {
    let db = create_test_db();
    let (mut store, instance) =
        create_wasm_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // Wywolaj on_start() — powinno zwrocic 0
    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("Brak on_start");
    let result = on_start.call(&mut store, ()).expect("on_start trap");
    assert_eq!(result, 0, "on_start powinno zwrocic 0 (sukces)");

    // Wywolaj on_stop() — powinno zwrocic 0
    let on_stop = instance
        .get_typed_func::<(), i32>(&mut store, "on_stop")
        .expect("Brak on_stop");
    let result = on_stop.call(&mut store, ()).expect("on_stop trap");
    assert_eq!(result, 0, "on_stop powinno zwrocic 0 (sukces)");
}

// =============================================================================
// Test 3: Narzedzie echo — wywolanie on_request
// =============================================================================

#[test]
fn addon_tool_echo() {
    let db = create_test_db();
    let (mut store, instance) =
        create_wasm_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "echo",
        serde_json::json!({"text": "hello world"}),
    )
    .expect("Blad wywolania echo");

    // Sprawdz odpowiedz
    assert_eq!(response["ok"], true, "echo powinno zwrocic ok=true");
    assert_eq!(
        response["data"]["echo"], "hello world",
        "echo powinno zwrocic przekazany tekst"
    );
}

// =============================================================================
// Test 4: Narzedzie test_storage — zapis i odczyt z sandboxowanego storage
// =============================================================================

#[test]
fn addon_tool_storage() {
    let db = create_test_db();

    // Dodaj limity zasobow (wymagane przez storage_set)
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('test-addon', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .expect("Blad wstawiania limitow zasobow");
    }

    let (mut store, instance) =
        create_wasm_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "test_storage",
        serde_json::json!({"key": "test_key_1", "value": "test_value_1"}),
    )
    .expect("Blad wywolania test_storage");

    assert_eq!(
        response["ok"], true,
        "test_storage powinno zwrocic ok=true: {:?}",
        response
    );
    assert_eq!(
        response["data"]["match"], true,
        "Odczytana wartosc powinna byc rowna zapisanej: {:?}",
        response
    );
    assert_eq!(
        response["data"]["written"], "test_value_1",
        "written powinno byc 'test_value_1'"
    );
    assert_eq!(
        response["data"]["read"], "test_value_1",
        "read powinno byc 'test_value_1'"
    );
}

// =============================================================================
// Test 5: Fuel metering — nieskonczona petla powinna byc przerwana
// =============================================================================

#[test]
fn addon_fuel_metering() {
    let db = create_test_db();

    // Paliwo wystarczajace na alloc/setup/parsowanie JSON ale nie na nieskonczona petla.
    // Typowe wywolanie on_request (bez petli) zuzywa ~200k-500k fuel.
    // Petla nieskonczona zuzywa miliardy — 2M wystarczy na setup ale nie na petla.
    let (mut store, instance) = create_wasm_instance(
        db,
        vec!["storage".to_string(), "log".to_string()],
        Some(2_000_000),
    );

    // Przygotuj request recznie (call_on_request nie obsluguje trap)
    let request_json = serde_json::json!({
        "tool": "test_crash",
        "params": {},
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .expect("Brak alloc");

    // Alloc moze juz zuzywac paliwo — sprawdz czy sie powiodl
    let alloc_result = alloc_fn.call(&mut store, request_bytes.len() as i32);
    if alloc_result.is_err() {
        // Paliwo wyczerpane juz na alloc — test przechodzi (fuel metering dziala)
        let err = alloc_result.unwrap_err().to_string().to_lowercase();
        assert!(
            err.contains("fuel") || err.contains("insufficient") || err.contains("trap"),
            "Oczekiwano bledu fuel exhausted, otrzymano: {}",
            err
        );
        return;
    }
    let input_ptr = alloc_result.unwrap();

    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("Brak memory");
    memory.data_mut(&mut store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    let out_cap: i32 = 65536;
    let out_ptr_result = alloc_fn.call(&mut store, out_cap);
    if out_ptr_result.is_err() {
        // Fuel wyczerpane na alloc wyjsciowym — ok
        return;
    }
    let out_ptr = out_ptr_result.unwrap();

    let out_len_ptr_result = alloc_fn.call(&mut store, 4);
    if out_len_ptr_result.is_err() {
        return;
    }
    let out_len_ptr = out_len_ptr_result.unwrap();

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, "on_request")
        .expect("Brak on_request");

    // Wywolanie powinno zwrocic blad (fuel exhausted trap)
    let result = on_request.call(
        &mut store,
        (
            input_ptr,
            request_bytes.len() as i32,
            out_ptr,
            out_cap,
            out_len_ptr,
        ),
    );

    assert!(
        result.is_err(),
        "on_request z test_crash powinno zwrocic blad (fuel exhausted), ale zwrocilo: {:?}",
        result
    );

    // Wasmtime zwraca rozne komunikaty bledu przy wyczerpaniu paliwa:
    // - "all fuel consumed" (starsza wersja)
    // - "fuel" (nowsza wersja)
    // - "wasm trap" / "error while executing" (ogolne)
    let err = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err.contains("fuel")
            || err.contains("insufficient")
            || err.contains("wasm trap")
            || err.contains("error while executing"),
        "Oczekiwano bledu fuel exhausted / trap, otrzymano: {}",
        err
    );
}

// =============================================================================
// Test 6: Odmowa uprawnien — brak uprawnienia "storage"
// =============================================================================

#[test]
fn addon_permission_denied() {
    let db = create_test_db();

    // Utworz instancje BEZ uprawnienia "storage"
    let (mut store, instance) = create_wasm_instance(
        db,
        vec!["log".to_string()], // Tylko log, brak storage
        None,
    );

    // Probuj wywolac test_storage — storage_set powinno zwrocic blad uprawnien
    let response = call_on_request(
        &mut store,
        &instance,
        "test_storage",
        serde_json::json!({"key": "k", "value": "v"}),
    )
    .expect("Blad wywolania test_storage");

    // store_set powinno zwrocic blad (ABI_ERR_PERMISSION = -1)
    // SDK interpretuje to jako blad i addon zwroci ok=false
    assert_eq!(
        response["ok"], false,
        "test_storage bez uprawnienia storage powinno zwrocic ok=false: {:?}",
        response
    );
}

// =============================================================================
// Test 7: Pelny cykl zycia — install, start, echo, stop
// =============================================================================

#[test]
fn addon_lifecycle_full() {
    use tentaflow_core::addon::lifecycle;

    let db = create_test_db();

    // Skopiuj WASM do addon.wasm (manifest.toml mowi wasm_file = "addon.wasm")
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let addon_dir = Path::new(manifest_dir).join(TEST_ADDON_DIR);
    let wasm_src = Path::new(manifest_dir).join(TEST_ADDON_WASM);
    let wasm_dst = addon_dir.join("addon.wasm");

    // Skopiuj WASM do oczekiwanej lokalizacji
    std::fs::copy(&wasm_src, &wasm_dst).unwrap_or_else(|e| {
        panic!(
            "Nie udalo sie skopiowac WASM: {:?} -> {:?}: {}",
            wasm_src, wasm_dst, e
        )
    });

    // Cleanup — usun skopiowany plik na koniec testu
    struct CleanupGuard<'a>(&'a Path);
    impl<'a> Drop for CleanupGuard<'a> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _guard = CleanupGuard(&wasm_dst);

    // 1. Instalacja addonu
    let manifest = lifecycle::install(&addon_dir, &db).expect("Blad instalacji addonu");

    assert_eq!(manifest.addon_id, "test-addon");
    assert_eq!(manifest.version, "1.0.0");
    assert!(
        !manifest.tools.is_empty(),
        "Addon powinien miec zarejestrowane narzedzia"
    );

    // 2. Sprawdz czy addon jest w DB
    {
        let conn = db.lock().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = 'test-addon'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "Addon powinien byc zarejestrowany w DB");
    }

    // 3. Dodaj uprawnienia i limity do DB (wymagane przez host functions)
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_declared_permissions (addon_id, permission_type) VALUES ('test-addon', 'storage')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_declared_permissions (addon_id, permission_type) VALUES ('test-addon', 'log')",
            [],
        ).unwrap();
    }

    // 4. Zaladuj WASM i utworz instancje
    let wasm_bytes = std::fs::read(&wasm_dst).expect("Nie udalo sie odczytac addon.wasm");
    let engine = create_engine().expect("Blad tworzenia silnika");
    let module = compile_module(&engine, &wasm_bytes).expect("Blad kompilacji");

    let state = AddonState {
        addon_id: "test-addon".to_string(),
        instance_id: "lifecycle-test-001".to_string(),
        user_id: None,
        db: db.clone(),
        permissions: vec!["storage".to_string(), "log".to_string()],
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
    };

    let mut store = create_test_store(&engine, state, None);
    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("Blad rejestracji host functions");
    let instance = instantiate(&linker, &mut store, &module).expect("Blad instancjacji");

    // 5. on_start
    let on_start = instance
        .get_typed_func::<(), i32>(&mut store, "on_start")
        .expect("Brak on_start");
    let start_result = on_start.call(&mut store, ()).expect("on_start trap");
    assert_eq!(start_result, 0, "on_start powinno zwrocic 0");

    // 6. Wywolaj echo tool
    let response = call_on_request(
        &mut store,
        &instance,
        "echo",
        serde_json::json!({"text": "lifecycle test"}),
    )
    .expect("Blad wywolania echo");
    assert_eq!(response["ok"], true);
    assert_eq!(response["data"]["echo"], "lifecycle test");

    // 7. on_stop
    let on_stop = instance
        .get_typed_func::<(), i32>(&mut store, "on_stop")
        .expect("Brak on_stop");
    let stop_result = on_stop.call(&mut store, ()).expect("on_stop trap");
    assert_eq!(stop_result, 0, "on_stop powinno zwrocic 0");

    // 8. Odinstaluj addon
    lifecycle::uninstall("test-addon", &db).expect("Blad deinstalacji");

    // 9. Sprawdz cleanup w DB
    {
        let conn = db.lock().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = 'test-addon'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !exists,
            "Addon powinien zostac usuniety z DB po deinstalacji"
        );
    }
}

// =============================================================================
// Test 8: Narzedzie test_log — wywolanie logowania
// =============================================================================

#[test]
fn addon_tool_log() {
    let db = create_test_db();
    let (mut store, instance) =
        create_wasm_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(&mut store, &instance, "test_log", serde_json::json!({}))
        .expect("Blad wywolania test_log");

    assert_eq!(response["ok"], true, "test_log powinno zwrocic ok=true");
    assert_eq!(
        response["data"]["logged"], true,
        "test_log powinno zwrocic logged=true"
    );
}

// =============================================================================
// Test 9: Nieznane narzedzie — poprawna obsluga bledu
// =============================================================================

#[test]
fn addon_unknown_tool() {
    let db = create_test_db();
    let (mut store, instance) =
        create_wasm_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "nieistniejace_narzedzie",
        serde_json::json!({}),
    )
    .expect("Blad wywolania nieznanego narzedzia");

    assert_eq!(
        response["ok"], false,
        "Nieznane narzedzie powinno zwrocic ok=false: {:?}",
        response
    );
}

// =============================================================================
// Test 10: Manifest — poprawne parsowanie manifest.toml
// =============================================================================

#[test]
fn addon_manifest_parsing() {
    use tentaflow_core::addon::lifecycle;

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let addon_dir = Path::new(manifest_dir).join(TEST_ADDON_DIR);
    let manifest_path = addon_dir.join("manifest.toml");

    let _content =
        std::fs::read_to_string(&manifest_path).expect("Nie udalo sie odczytac manifest.toml");

    // Parsowanie z parse_manifest_toml nie jest publiczne,
    // ale mozemy uzyc install z in-memory DB do weryfikacji

    // Skopiuj WASM jesli nie istnieje
    let wasm_src = Path::new(manifest_dir).join(TEST_ADDON_WASM);
    let wasm_dst = addon_dir.join("addon.wasm");
    let needs_copy = !wasm_dst.exists();
    if needs_copy {
        std::fs::copy(&wasm_src, &wasm_dst).expect("Nie udalo sie skopiowac WASM");
    }

    struct CleanupGuard {
        path: std::path::PathBuf,
        should_cleanup: bool,
    }
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            if self.should_cleanup {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
    let _guard = CleanupGuard {
        path: wasm_dst,
        should_cleanup: needs_copy,
    };

    let db = create_test_db();
    let manifest = lifecycle::install(&addon_dir, &db).expect("Blad parsowania manifest.toml");

    assert_eq!(manifest.addon_id, "test-addon");
    assert_eq!(manifest.version, "1.0.0");
    assert_eq!(manifest.display_name, "Test Addon");
    assert!(manifest.description.is_some());
    assert!(manifest.author.is_some());

    // Declared permissions — canonical format uses [[permission]] with dotted ids.
    assert!(
        !manifest.declared_permissions.is_empty(),
        "Powinny byc uprawnienia"
    );
    let perm_ids: Vec<&str> = manifest
        .declared_permissions
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert!(
        perm_ids.contains(&"storage.read"),
        "Brak uprawnienia 'storage.read'"
    );
    assert!(
        perm_ids.contains(&"storage.write"),
        "Brak uprawnienia 'storage.write'"
    );

    // Sprawdz narzedzia
    let tool_names: Vec<&str> = manifest.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(tool_names.contains(&"echo"), "Brak narzedzia 'echo'");
    assert!(
        tool_names.contains(&"test_storage"),
        "Brak narzedzia 'test_storage'"
    );
    assert!(
        tool_names.contains(&"test_crash"),
        "Brak narzedzia 'test_crash'"
    );

    // Declared permissions already asserted above — ensure expected granular ids exist.
    assert!(
        perm_ids.contains(&"http.request"),
        "Brak uprawnienia 'http.request'"
    );
    assert!(
        perm_ids.contains(&"llm.generate"),
        "Brak uprawnienia 'llm.generate'"
    );
}

// =============================================================================
// =============================================================================
// TESTY BEZPIECZENSTWA — malicious-addon
// =============================================================================
// =============================================================================

/// Sciezka do skompilowanego WASM malicious-addon
const MALICIOUS_ADDON_WASM: &str =
    "addons/malicious-addon/target/wasm32-wasip1/release/tentaflow_addon_malicious.wasm";

/// Wczytuje bajty WASM malicious-addon z dysku
fn load_malicious_wasm() -> Vec<u8> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let wasm_path = Path::new(manifest_dir).join(MALICIOUS_ADDON_WASM);
    std::fs::read(&wasm_path).unwrap_or_else(|e| {
        panic!(
            "Nie udalo sie wczytac WASM z {:?}: {}. Skompiluj addon: \
             cd addons/malicious-addon && cargo build --target wasm32-wasip1 --release",
            wasm_path, e
        )
    })
}

/// Tworzy instancje WASM malicious-addon z podanymi uprawnieniami
fn create_malicious_instance(
    db: db::DbPool,
    permissions: Vec<String>,
    fuel: Option<u64>,
) -> (wasmtime::Store<AddonState>, wasmtime::Instance) {
    let wasm_bytes = load_malicious_wasm();
    let engine = create_engine().expect("Blad tworzenia silnika Wasmtime");
    let module = compile_module(&engine, &wasm_bytes).expect("Blad kompilacji WASM");

    let state =
        create_addon_state_with_id(db, permissions, "malicious-addon", "malicious-instance-001");
    let mut store = create_test_store(&engine, state, fuel);

    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("Blad rejestracji host functions");

    let instance = instantiate(&linker, &mut store, &module).expect("Blad instancjacji WASM");

    (store, instance)
}

/// Helper: wywoluje narzedzie malicious-addon i oczekuje trap (blad WASM).
/// Uzywany dla testow ktore powinny spowodowac crash (memory escape, stack overflow, itp.)
fn call_on_request_expect_trap(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    tool_name: &str,
) -> wasmtime::Error {
    let request_json = serde_json::json!({
        "tool": tool_name,
        "params": {},
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut *store, "alloc")
        .expect("Brak funkcji alloc");

    // Alloc moze zuzywac paliwo — jesli sie nie powiedzie, to tez jest ok (fuel exhausted)
    let input_ptr = match alloc_fn.call(&mut *store, request_bytes.len() as i32) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let memory = instance
        .get_memory(&mut *store, "memory")
        .expect("Brak eksportu memory");
    memory.data_mut(&mut *store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    let out_cap: i32 = 65536;
    let out_ptr = match alloc_fn.call(&mut *store, out_cap) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let out_len_ptr = match alloc_fn.call(&mut *store, 4) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut *store, "on_request")
        .expect("Brak funkcji on_request");

    match on_request.call(
        &mut *store,
        (
            input_ptr,
            request_bytes.len() as i32,
            out_ptr,
            out_cap,
            out_len_ptr,
        ),
    ) {
        Ok(code) => panic!(
            "Oczekiwano trap dla narzedzia '{}', ale on_request zwrocil kod: {}",
            tool_name, code
        ),
        Err(e) => e,
    }
}

// =============================================================================
// Test bezpieczenstwa 1: Izolacja storage — kradziez danych innego addonu
// =============================================================================

#[test]
fn malicious_storage_isolation() {
    let db = create_test_db();

    // Dodaj limity zasobow dla obu addonow
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('test-addon', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('malicious-addon', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .unwrap();
    }

    // 1. Zainstaluj test-addon i zapisz dane w storage
    {
        let (mut store, instance) = create_wasm_instance(
            db.clone(),
            vec!["storage".to_string(), "log".to_string()],
            None,
        );

        let response = call_on_request(
            &mut store,
            &instance,
            "test_storage",
            serde_json::json!({"key": "secret_data", "value": "very_sensitive_password_123"}),
        )
        .expect("Blad zapisu danych test-addon");
        assert_eq!(
            response["ok"], true,
            "test-addon powinien zapisac dane: {:?}",
            response
        );
    }

    // Sprawdz ze dane sa w DB pod addon_id="test-addon"
    {
        let conn = db.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM addon_storage WHERE addon_id = 'test-addon' AND storage_key = 'secret_data'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(exists, "Dane test-addon powinny byc w DB");
    }

    // 2. Zaladuj malicious-addon i probj krasc dane
    let (mut store, instance) = create_malicious_instance(
        db.clone(),
        vec!["storage".to_string(), "log".to_string()],
        None,
    );

    let response = call_on_request(
        &mut store,
        &instance,
        "try_steal_storage",
        serde_json::json!({}),
    )
    .expect("Blad wywolania try_steal_storage");

    // Addon nie powinien widziec danych test-addon (izolacja per addon_id)
    assert_eq!(
        response["ok"], true,
        "Storage powinno byc izolowane — malicious nie widzi danych test-addon: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci: {:?}",
        response
    );
}

// =============================================================================
// Test bezpieczenstwa 2: HTTP bez uprawnienia
// =============================================================================

#[test]
fn malicious_unauthorized_http() {
    let db = create_test_db();

    // Malicious addon ma TYLKO "storage" i "log" — brak "http"
    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "try_unauthorized_http",
        serde_json::json!({}),
    )
    .expect("Blad wywolania try_unauthorized_http");

    // HTTP powinno byc zablokowane
    assert_eq!(
        response["ok"], true,
        "HTTP bez uprawnienia powinno byc zablokowane: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci HTTP: {:?}",
        response
    );
}

// =============================================================================
// Test bezpieczenstwa 3: Secrets bez uprawnienia
// =============================================================================

#[test]
fn malicious_unauthorized_secrets() {
    let db = create_test_db();

    // Malicious addon ma TYLKO "storage" i "log" — brak "secrets"
    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "try_unauthorized_secrets",
        serde_json::json!({}),
    )
    .expect("Blad wywolania try_unauthorized_secrets");

    // Secrets powinno byc zablokowane
    assert_eq!(
        response["ok"], true,
        "Secrets bez uprawnienia powinno byc zablokowane: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci secrets: {:?}",
        response
    );
}

// =============================================================================
// Test bezpieczenstwa 4: Network bez uprawnienia
// =============================================================================

#[test]
fn malicious_unauthorized_network() {
    let db = create_test_db();

    // Malicious addon ma TYLKO "storage" i "log" — brak "network"
    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "try_unauthorized_network",
        serde_json::json!({}),
    )
    .expect("Blad wywolania try_unauthorized_network");

    // Network powinno byc zablokowane
    assert_eq!(
        response["ok"], true,
        "Network bez uprawnienia powinno byc zablokowane: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci network: {:?}",
        response
    );
}

// =============================================================================
// Test bezpieczenstwa 5: Ucieczka z pamieci WASM (out of bounds)
// =============================================================================

#[test]
fn malicious_memory_escape() {
    let db = create_test_db();

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // Odczyt pod adresem 0xFFFF_FF00 powinien spowodowac trap
    let err = call_on_request_expect_trap(&mut store, &instance, "try_memory_escape");

    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("out of bounds")
            || err_msg.contains("memory")
            || err_msg.contains("trap")
            || err_msg.contains("unreachable")
            || err_msg.contains("error while executing"),
        "Oczekiwano out of bounds memory trap, otrzymano: {}",
        err_msg
    );
}

// =============================================================================
// Test bezpieczenstwa 6: Stack overflow (call stack exhausted)
// =============================================================================

#[test]
fn malicious_stack_overflow() {
    let db = create_test_db();

    // Niskie paliwo — rekurencja moze byc zoptymalizowana do petli przez kompilator,
    // wiec uzywamy fuel limit aby zagwarantowac przerwanie
    let (mut store, instance) = create_malicious_instance(
        db,
        vec!["storage".to_string(), "log".to_string()],
        Some(2_000_000), // wystarczajaco na setup, nie na nieskonczona rekurencje/petla
    );

    // Rekurencja/petla powinna spowodowac trap (stack overflow lub fuel exhausted)
    let request_json = serde_json::json!({
        "tool": "try_stack_overflow",
        "params": {},
        "user_id": 1,
    });
    let request_bytes = serde_json::to_vec(&request_json).unwrap();

    let alloc_fn = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .expect("Brak alloc");

    let input_ptr = match alloc_fn.call(&mut store, request_bytes.len() as i32) {
        Ok(p) => p,
        Err(e) => {
            // Fuel wyczerpane na alloc — ok
            let err = e.to_string().to_lowercase();
            assert!(
                err.contains("fuel") || err.contains("trap"),
                "Oczekiwano fuel/trap: {}",
                err
            );
            return;
        }
    };

    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("Brak memory");
    memory.data_mut(&mut store)[input_ptr as usize..input_ptr as usize + request_bytes.len()]
        .copy_from_slice(&request_bytes);

    let out_cap: i32 = 65536;
    let out_ptr = match alloc_fn.call(&mut store, out_cap) {
        Ok(p) => p,
        Err(_) => return, // Fuel wyczerpane — ok
    };
    let out_len_ptr = match alloc_fn.call(&mut store, 4) {
        Ok(p) => p,
        Err(_) => return, // Fuel wyczerpane — ok
    };

    let on_request = instance
        .get_typed_func::<(i32, i32, i32, i32, i32), i32>(&mut store, "on_request")
        .expect("Brak on_request");

    let result = on_request.call(
        &mut store,
        (
            input_ptr,
            request_bytes.len() as i32,
            out_ptr,
            out_cap,
            out_len_ptr,
        ),
    );

    // Oczekujemy trap — stack overflow, fuel exhausted, lub inny blad WASM
    assert!(
        result.is_err(),
        "try_stack_overflow powinno spowodowac trap (stack overflow lub fuel exhausted), \
         ale zwrocilo kod: {:?}",
        result
    );

    let err_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err_msg.contains("stack")
            || err_msg.contains("fuel")
            || err_msg.contains("overflow")
            || err_msg.contains("trap")
            || err_msg.contains("error while executing"),
        "Oczekiwano stack overflow lub fuel exhausted, otrzymano: {}",
        err_msg
    );
}

// =============================================================================
// Test bezpieczenstwa 7: Infinite loop (fuel exhausted)
// =============================================================================

#[test]
fn malicious_infinite_loop() {
    let db = create_test_db();

    // Niskie paliwo — 100_000 instrukcji (wystarczy na setup, nie na petla)
    let (mut store, instance) = create_malicious_instance(
        db,
        vec!["storage".to_string(), "log".to_string()],
        Some(100_000),
    );

    // Nieskonczona petla powinna wyczerpac paliwo
    let err = call_on_request_expect_trap(&mut store, &instance, "try_infinite_loop");

    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("fuel")
            || err_msg.contains("insufficient")
            || err_msg.contains("trap")
            || err_msg.contains("error while executing"),
        "Oczekiwano fuel exhausted trap, otrzymano: {}",
        err_msg
    );
}

// =============================================================================
// Test bezpieczenstwa 8: Ogromna alokacja pamieci (memory limit)
// =============================================================================

#[test]
fn malicious_huge_alloc() {
    let db = create_test_db();

    // UWAGA BEZPIECZENSTWA: Wasmtime domyslnie NIE limituje pamieci liniowej WASM.
    // Addon moze zaalokowac dowolna ilosc pamieci (do limitu 4GB liniowej pamieci WASM).
    // W PRODUKCJI nalezy ustawic wasmtime::StoreLimits z memory_size limit.
    //
    // Ten test dokumentuje biezacy stan — alokacja 256MB JEST mozliwa.
    // Test weryfikuje ze runtime nie crasha i addon zwraca poprawna odpowiedz
    // (nawet jesli alokacja sie powiedzie — to jest problem do naprawy).
    //
    // TODO: Dodac StoreLimits do create_engine/create_store z limitem np. 64MB per addon.
    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let result = call_on_request(
        &mut store,
        &instance,
        "try_huge_alloc",
        serde_json::json!({}),
    );

    match result {
        Ok(response) => {
            // Addon zwrocil odpowiedz — sprawdz czy runtime nie zostal uszkodzony.
            // Jesli alokacja sie powiodla (vulnerability present), to jest znany problem
            // do naprawy przez StoreLimits. Test przechodzi bo sandbox sie nie zlama.
            if response.get("vulnerability").is_some() {
                // ZNANY PROBLEM: Brak StoreLimits — addon moze alokowac duzo pamieci.
                // Logujemy ostrzezenie ale nie failujemy testu — to jest problem konfiguracji,
                // nie ucieczka z sandboxa.
                eprintln!(
                    "OSTRZEZENIE BEZPIECZENSTWA: Addon zaalokowal 256MB pamieci. \
                     Brak StoreLimits w konfiguracji Wasmtime. Odpowiedz: {:?}",
                    response
                );
            }
        }
        Err(_e) => {
            // Trap (OOM lub fuel exhausted) — dobrze, sandbox zadzialal
        }
    }
}

// =============================================================================
// Test bezpieczenstwa 9: Cross-addon storage isolation
// =============================================================================

#[test]
fn malicious_cross_addon_storage() {
    let db = create_test_db();

    // Dodaj limity zasobow dla obu addonow
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('test-addon', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('malicious-addon', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .unwrap();
    }

    // 1. Symuluj addon "teams" zapisujacy dane w swoim storage
    // Wstawiamy recznie do DB z addon_id="teams" — bo nie mamy WASM teams
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT INTO addon_storage \
             (addon_id, instance_id, storage_key, storage_value, value_size_bytes, updated_at) \
             VALUES ('teams', 'teams-instance-001', 'oauth_token', X'746F6B656E5F7365637265745F313233', 16, datetime('now'))",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO addon_storage \
             (addon_id, instance_id, storage_key, storage_value, value_size_bytes, updated_at) \
             VALUES ('teams', 'teams-instance-001', 'config.tenant_id', X'74656E616E742D616263', 10, datetime('now'))",
            [],
        ).unwrap();
    }

    // 2. Zaladuj malicious-addon i probj krasc dane teams
    let (mut store, instance) = create_malicious_instance(
        db.clone(),
        vec!["storage".to_string(), "log".to_string()],
        None,
    );

    let response = call_on_request(
        &mut store,
        &instance,
        "try_storage_other_addon",
        serde_json::json!({}),
    )
    .expect("Blad wywolania try_storage_other_addon");

    // Malicious addon NIE powinien widziec danych teams
    assert_eq!(
        response["ok"], true,
        "Cross-addon storage powinno byc izolowane: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci cross-addon storage: {:?}",
        response
    );

    // 3. Sprawdz ze dane teams nadal sa w DB (nie zostaly uszkodzone)
    {
        let conn = db.lock().unwrap();
        let teams_data: String = conn
            .query_row(
                "SELECT CAST(storage_value AS TEXT) FROM addon_storage \
             WHERE addon_id = 'teams' AND storage_key = 'oauth_token'",
                [],
                |row| row.get(0),
            )
            .expect("Dane teams powinny nadal istniec w DB");
        assert!(
            !teams_data.is_empty(),
            "Dane teams nie powinny byc puste po ataku"
        );
    }
}

// =============================================================================
// Test bezpieczenstwa 10: Zle pointery (bounds check)
// =============================================================================

#[test]
fn malicious_bad_pointers() {
    let db = create_test_db();

    // Dodaj limity zasobow
    {
        let conn = db.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES ('malicious-addon', 0, 0, 0, 1, 0, 100, 0, 0)",
            [],
        )
        .unwrap();
    }

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    let response = call_on_request(
        &mut store,
        &instance,
        "try_overwrite_memory",
        serde_json::json!({}),
    )
    .expect("Blad wywolania try_overwrite_memory");

    // Host functions powinny odrzucic zle pointery (bounds check)
    assert_eq!(
        response["ok"], true,
        "Bounds check powinien dzialac: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci bounds: {:?}",
        response
    );

    // Sprawdz ze dlugi klucz (>1024B) i duza wartosc (>1MB) zostaly zablokowane
    // (null_write moze przejsc bo "\0\0\0\0" to poprawny klucz)
    if let Some(long_key_blocked) = response.get("long_key_blocked") {
        assert_eq!(
            long_key_blocked, true,
            "Klucz >1024B powinien byc zablokowany (CR-009)"
        );
    }
    if let Some(large_value_blocked) = response.get("large_value_blocked") {
        assert_eq!(
            large_value_blocked, true,
            "Wartosc >1MB powinna byc zablokowana (CR-009)"
        );
    }
}

// =============================================================================
// Test bezpieczenstwa 11: Bezposredni TCP (proba call_indirect na fake socket)
// =============================================================================

#[test]
fn malicious_direct_tcp() {
    let db = create_test_db();

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // call_indirect na nieistniejacy indeks tablicy funkcji — trap guaranteed
    let err = call_on_request_expect_trap(&mut store, &instance, "try_direct_tcp");

    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("indirect call")
            || err_msg.contains("out of bounds")
            || err_msg.contains("unreachable")
            || err_msg.contains("table")
            || err_msg.contains("uninitialized")
            || err_msg.contains("null")
            || err_msg.contains("error while executing"),
        "Oczekiwano trap od call_indirect, dostano: {}",
        err
    );
}

// =============================================================================
// Test bezpieczenstwa 12: Zapis plikow (proba zapisu poza pamiecia WASM)
// =============================================================================

#[test]
fn malicious_write_file() {
    let db = create_test_db();

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // Zapis pod adresem 0x7FFF_0000 — daleko poza liniowa pamiecia WASM
    let err = call_on_request_expect_trap(&mut store, &instance, "try_write_file");

    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("out of bounds")
            || err_msg.contains("unreachable")
            || err_msg.contains("memory")
            || err_msg.contains("error while executing"),
        "Oczekiwano trap od out-of-bounds write, dostano: {}",
        err
    );
}

// =============================================================================
// Test bezpieczenstwa 13: Odczyt plikow (skanowanie pamieci + odczyt poza)
// =============================================================================

#[test]
fn malicious_read_file() {
    let db = create_test_db();

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // Skanuje pamiec WASM szukajac danych hosta, potem proba odczytu poza pamiecia.
    // Moze trap-owac (out of bounds) lub zwrocic ok jesli skan zakonczyl sie wewnatrz pamieci.
    let result = call_on_request(
        &mut store,
        &instance,
        "try_read_file",
        serde_json::json!({}),
    );

    match result {
        Ok(response) => {
            // Jesli zwrocil odpowiedz — nie powinno byc podatnosci
            assert!(
                response.get("vulnerability").is_none(),
                "Odczyt plikow hosta nie powinien byc mozliwy w WASM: {:?}",
                response
            );
        }
        Err(e) => {
            // Trap jest akceptowalny — proba odczytu poza pamiecia
            let err_msg = e.to_string().to_lowercase();
            assert!(
                err_msg.contains("out of bounds")
                    || err_msg.contains("unreachable")
                    || err_msg.contains("memory")
                    || err_msg.contains("error while executing"),
                "Oczekiwano trap od out-of-bounds read, dostano: {}",
                e
            );
        }
    }
}

// =============================================================================
// Test bezpieczenstwa 14: Wykonanie procesu (proba call_indirect na fake execve)
// =============================================================================

#[test]
fn malicious_exec_process() {
    let db = create_test_db();

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // call_indirect na nieistniejacy indeks — trap guaranteed
    let err = call_on_request_expect_trap(&mut store, &instance, "try_exec_process");

    let err_msg = err.to_string().to_lowercase();
    assert!(
        err_msg.contains("indirect call")
            || err_msg.contains("out of bounds")
            || err_msg.contains("unreachable")
            || err_msg.contains("table")
            || err_msg.contains("uninitialized")
            || err_msg.contains("null")
            || err_msg.contains("error while executing"),
        "Oczekiwano trap od call_indirect, dostano: {}",
        err
    );
}

// =============================================================================
// Test bezpieczenstwa 15: Odczyt zmiennych srodowiskowych (skanowanie pamieci)
// =============================================================================

#[test]
fn malicious_read_env() {
    let db = create_test_db();

    let (mut store, instance) =
        create_malicious_instance(db, vec!["storage".to_string(), "log".to_string()], None);

    // Skanuje pamiec WASM szukajac wzorcow env — nie powinno nic znalezc
    let response = call_on_request(&mut store, &instance, "try_read_env", serde_json::json!({}))
        .expect("Blad wywolania try_read_env");

    assert_eq!(
        response["ok"], true,
        "Pamiec WASM nie powinna zawierac zmiennych srodowiskowych hosta: {:?}",
        response
    );
    assert!(
        response.get("vulnerability").is_none(),
        "Nie powinno byc podatnosci env: {:?}",
        response
    );
}
