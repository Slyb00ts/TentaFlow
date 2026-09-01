// =============================================================================
// File: tests/bus_addon_integration_e2e.rs — M3b bus_* addon e2e WASM tests
// =============================================================================
//
// Drives the `bus_publish_v1` / `bus_consume_open/next/commit/close_v1` host
// functions through a real WASM guest (`addons/sdk-showcase`), mirroring
// `camera_integration_e2e.rs`'s harness shape. The addon's `on_request`
// exposes five bus tools:
//   - "run_bus_publish_batch"  bus_publish_v1
//   - "run_bus_open"           bus_consume_open_v1
//   - "run_bus_next"           bus_consume_next_v1
//   - "run_bus_commit"         bus_consume_commit_v1
//   - "run_bus_close"          bus_consume_close_v1
//
// Unlike camera, the bus host functions go through the process-global
// `bus::global()` singleton (see `bus.rs`'s own file doc — no injection
// point), so this file initializes it ONCE via `shared_env()` and every test
// shares that same `BusService` + db, exactly like
// `bus_flow_chain_p11_gate.rs` initializes it per-process. Tests use unique
// topic/group names to avoid interfering with each other, and `lock()`
// serializes them against the shared consumer registry in `bus.rs`.
//
// Build prerequisite for every test in this file:
//     cd addons/sdk-showcase && cargo build --target wasm32-wasip1 --release
// All tests are `#[ignore]` so a developer machine without the WASM artifact
// is not blocked.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex as ParkingMutex;
use serde_json::{json, Value};

use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions;
use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
use tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::runtime::{compile_module, create_engine, create_linker, instantiate};
use tentaflow_core::addon::{AddonCallProvenance, AddonManifest, AddonState};
use tentaflow_core::bus::{self, BusAction, BusCallContext, BusInitConfig, BusServiceError};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db;

const BUS_TEST_ADDON_WASM: &str =
    "../target-addon-wasm/wasm32-wasip1/release/tentaflow_addon_sdk_showcase.wasm";

const ADDON_ID: &str = "sdk-showcase";
const INSTANCE_ID: &str = "sdk-showcase-bus-001";

const PERM_BUS_PUBLISH: &str = "bus.publish";
const PERM_BUS_SUBSCRIBE: &str = "bus.subscribe";

// =============================================================================
// Process-global bus singleton — one per test binary, shared by every test
// =============================================================================

struct AllowAllAuthorizer;

impl bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(&self, _ctx: &BusCallContext, _action: BusAction, _topic: &str) -> Result<(), BusServiceError> {
        Ok(())
    }
    fn authorize_group(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        Ok(())
    }
    fn generation(&self) -> u64 {
        0
    }
}

struct SharedEnv {
    db: db::DbPool,
}

/// Initializes `bus::global()` exactly once for the whole test process and
/// hands out the shared db every `AddonState` in this file must reference —
/// `bus::init` itself is idempotent (returns the already-initialized service
/// on a second call), but the db/bus_dir arguments of a second call would be
/// silently discarded, so every test must funnel through this one shared db
/// rather than building its own.
fn shared_env() -> &'static SharedEnv {
    static ENV: OnceLock<SharedEnv> = OnceLock::new();
    ENV.get_or_init(|| {
        let db = db::init(Path::new(":memory:")).expect("init db");
        let tmp = tempfile::tempdir().expect("create temp dir");
        let bus_dir = tmp.path().join("bus");
        // Leaked deliberately: this dir must outlive every test in the
        // process, and the process exits at test-binary teardown anyway.
        std::mem::forget(tmp);
        bus::init(BusInitConfig {
            bus_dir,
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 200_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus::init");
        SharedEnv { db }
    })
}

// =============================================================================
// AddonState + WASM instance helpers
// =============================================================================

fn load_wasm() -> Option<Vec<u8>> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUS_TEST_ADDON_WASM);
    std::fs::read(&p).ok()
}

fn make_state(db: db::DbPool, permissions: Vec<String>, org_id: Option<String>) -> AddonState {
    AddonState {
        addon_id: ADDON_ID.to_string(),
        instance_id: INSTANCE_ID.to_string(),
        user_id: None,
        org_id,
        db: db.clone(),
        permissions,
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db)),
        fuel_consumed: 0,
        // System call so check_permission() does not require a user_id.
        is_system_call: true,
        call_provenance: AddonCallProvenance::addon(),
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

fn create_test_store(engine: &wasmtime::Engine, state: AddonState) -> wasmtime::Store<AddonState> {
    let mut store = wasmtime::Store::new(engine, state);
    store.set_fuel(1_000_000_000).expect("set_fuel");
    store.epoch_deadline_trap();
    // `create_engine()` starts a global 10ms epoch ticker, and this deadline
    // is set ONCE for the store's whole lifetime (never refreshed per-call
    // the way the production `set_call_epoch_deadline` does) — a small
    // deadline like the camera tests' `100` (≈1s cumulative) traps a store
    // that lives across real fsync-backed publishes and a multi-second
    // consume long-poll. Match production's non-timeout-testing default
    // (`clear_call_epoch_deadline`'s `u64::MAX / 4`) since these tests exist
    // to exercise the bus ABI, not epoch-deadline enforcement.
    store.set_epoch_deadline(u64::MAX / 4);
    store
}

fn create_wasm_instance(
    db: db::DbPool,
    permissions: Vec<String>,
    org_id: Option<String>,
    wasm_bytes: &[u8],
) -> (wasmtime::Store<AddonState>, wasmtime::Instance) {
    let engine = create_engine().expect("engine");
    let module = compile_module(&engine, wasm_bytes).expect("compile module");
    let state = make_state(db, permissions, org_id);
    let mut store = create_test_store(&engine, state);
    let mut linker = create_linker(&engine);
    host_functions::register_host_functions(&mut linker).expect("register host fns");
    let instance = instantiate(&linker, &mut store, &module).expect("instantiate");
    (store, instance)
}

// =============================================================================
// on_request marshaling — JSON in, JSON out (mirrors camera_integration_e2e.rs)
// =============================================================================

fn call_on_request(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    tool_name: &str,
    params: Value,
) -> Result<Value, String> {
    let request_json = json!({
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

    // 16 MiB output buffer — generous headroom over BusBatch's own 8 MiB cap.
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
    result: String,
    error_message: Option<String>,
}

fn fetch_audit_entries(db: &db::DbPool, action_prefix: &str) -> Vec<AuditEntry> {
    let conn = db.read().expect("read db");
    let mut stmt = conn
        .prepare(
            "SELECT action, result, error_message \
             FROM audit_log \
             WHERE addon_id = ?1 AND action LIKE ?2 \
             ORDER BY id ASC",
        )
        .expect("prepare audit query");
    let rows = stmt
        .query_map(rusqlite::params![ADDON_ID, format!("{action_prefix}%")], |r| {
            Ok(AuditEntry {
                action: r.get(0)?,
                result: r.get(1)?,
                error_message: r.get(2)?,
            })
        })
        .expect("query map");
    rows.filter_map(|r| r.ok()).collect()
}

// =============================================================================
// Cross-test serialization — every test in this file touches the shared bus
// singleton and consumer registry; running them serially avoids interference
// even though each uses its own topic/group names.
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
#[ignore = "requires sdk-showcase WASM build"]
async fn bus_addon_publish_consume_roundtrip() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!(
            "sdk-showcase WASM missing — build with: \
             cd addons/sdk-showcase && cargo build --target wasm32-wasip1 --release"
        );
    };
    let db = shared_env().db.clone();
    let (mut store, instance) = create_wasm_instance(
        db.clone(),
        vec![PERM_BUS_PUBLISH.to_string(), PERM_BUS_SUBSCRIBE.to_string()],
        None,
        &wasm,
    );

    let topic = "bus.addon.e2e.roundtrip";
    let group = "bus-addon-e2e-roundtrip";

    let pub_resp = call_on_request(
        &mut store,
        &instance,
        "run_bus_publish_batch",
        json!({"topic": topic, "count": 5, "size": 32, "create_if_missing": true}),
    )
    .expect("publish on_request");
    assert_eq!(pub_resp["ok"], Value::Bool(true), "resp={pub_resp}");
    assert_eq!(pub_resp["data"]["published"], 5);

    let open_resp = call_on_request(
        &mut store,
        &instance,
        "run_bus_open",
        json!({"topics": [topic], "group": group}),
    )
    .expect("open on_request");
    assert_eq!(open_resp["ok"], Value::Bool(true), "resp={open_resp}");
    let consumer_id = open_resp["consumer_id"]
        .as_str()
        .expect("consumer_id")
        .to_string();

    let next_resp = call_on_request(
        &mut store,
        &instance,
        "run_bus_next",
        json!({"consumer_id": consumer_id, "max_records": 10, "timeout_ms": 2000}),
    )
    .expect("next on_request");
    assert_eq!(next_resp["ok"], Value::Bool(true), "resp={next_resp}");
    assert_eq!(next_resp["data"]["count"], 5, "resp={next_resp}");
    let records = next_resp["data"]["records"].as_array().expect("records array");
    let offsets: Vec<Value> = records
        .iter()
        .map(|r| json!({"topic": r["topic"], "partition": r["partition"], "offset": r["offset"]}))
        .collect();

    let commit_resp = call_on_request(
        &mut store,
        &instance,
        "run_bus_commit",
        json!({"consumer_id": consumer_id, "offsets": offsets}),
    )
    .expect("commit on_request");
    assert_eq!(commit_resp["ok"], Value::Bool(true), "resp={commit_resp}");

    let close_resp = call_on_request(
        &mut store,
        &instance,
        "run_bus_close",
        json!({"consumer_id": consumer_id}),
    )
    .expect("close on_request");
    assert_eq!(close_resp["ok"], Value::Bool(true), "resp={close_resp}");
    assert_eq!(close_resp["closed"], Value::Bool(true));

    // PLAN §8.2: publish/open/close audit on success; next/commit must NOT
    // (per-message audit logging on the bus's hot path is forbidden). Only
    // check for the ABSENCE of a success-result entry, not the absence of
    // any entry with that action — this file shares one audit_log/addon_id
    // across every test in the process (see `shared_env()`), and a sibling
    // test's `bus.consume.next` DENIAL is legitimately audited and would
    // otherwise be misread as a violation here.
    let entries = fetch_audit_entries(&db, "bus.");
    let has = |action: &str, result: &str| {
        entries.iter().any(|e| e.action == action && e.result == result)
    };
    assert!(has("bus.publish", "ok"), "entries={entries:?}");
    assert!(has("bus.consume.open", "ok"), "entries={entries:?}");
    assert!(has("bus.consume.close", "ok"), "entries={entries:?}");
    assert!(
        !has("bus.consume.next", "ok"),
        "next must not audit on the success path, entries={entries:?}"
    );
    assert!(
        !has("bus.consume.commit", "ok"),
        "commit must not audit on the success path, entries={entries:?}"
    );
}

/// PLAN §6.4's security gate, verbatim: a missing `bus.subscribe` permission
/// must deny "na `open` i na kazdym `next`" — not just at handle-creation
/// time. Simulates a malicious/compromised addon instance by opening a
/// consumer from a LEGIT instance, then driving `next` on that same
/// `consumer_id` from a SECOND instance sharing the same `addon_id` (the
/// registry's key) but carrying an empty permission set — the host must
/// re-check permission fresh on every call, not trust anything cached from
/// `open` time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires sdk-showcase WASM build"]
async fn bus_addon_malicious_denied_on_open_and_next() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!("sdk-showcase WASM missing");
    };
    let db = shared_env().db.clone();
    let topic = "bus.addon.e2e.security";
    let group = "bus-addon-e2e-security";

    let (mut store_legit, instance_legit) = create_wasm_instance(
        db.clone(),
        vec![PERM_BUS_PUBLISH.to_string(), PERM_BUS_SUBSCRIBE.to_string()],
        None,
        &wasm,
    );
    call_on_request(
        &mut store_legit,
        &instance_legit,
        "run_bus_publish_batch",
        json!({"topic": topic, "count": 3, "size": 16, "create_if_missing": true}),
    )
    .expect("seed publish");
    let open_resp = call_on_request(
        &mut store_legit,
        &instance_legit,
        "run_bus_open",
        json!({"topics": [topic], "group": group}),
    )
    .expect("legit open on_request");
    assert_eq!(open_resp["ok"], Value::Bool(true), "resp={open_resp}");
    let consumer_id = open_resp["consumer_id"]
        .as_str()
        .expect("consumer_id")
        .to_string();

    // Same addon_id (ADDON_ID constant, see make_state), zero permissions.
    let (mut store_evil, instance_evil) = create_wasm_instance(db.clone(), vec![], None, &wasm);

    let evil_open = call_on_request(
        &mut store_evil,
        &instance_evil,
        "run_bus_open",
        json!({"topics": [topic], "group": "bus-addon-e2e-security-evil"}),
    )
    .expect("evil open on_request");
    assert_eq!(evil_open["ok"], Value::Bool(true), "resp={evil_open}");
    assert_eq!(
        evil_open["granted"],
        Value::Bool(false),
        "open must be denied without bus.subscribe, resp={evil_open}"
    );
    assert_eq!(
        evil_open["abi_error"].as_i64(),
        Some(AbiError::Permission.as_i32() as i64),
        "resp={evil_open}"
    );

    let evil_next = call_on_request(
        &mut store_evil,
        &instance_evil,
        "run_bus_next",
        json!({"consumer_id": consumer_id}),
    )
    .expect("evil next on_request");
    assert_eq!(evil_next["ok"], Value::Bool(true), "resp={evil_next}");
    assert_eq!(
        evil_next["granted"],
        Value::Bool(false),
        "next on a sibling-opened handle must be denied without bus.subscribe, resp={evil_next}"
    );
    assert_eq!(
        evil_next["abi_error"].as_i64(),
        Some(AbiError::Permission.as_i32() as i64),
        "resp={evil_next}"
    );

    // The legit instance's handle must be unaffected — it can still close it.
    let close_resp = call_on_request(
        &mut store_legit,
        &instance_legit,
        "run_bus_close",
        json!({"consumer_id": consumer_id}),
    )
    .expect("legit close on_request");
    assert_eq!(close_resp["ok"], Value::Bool(true), "resp={close_resp}");

    let entries = fetch_audit_entries(&db, "bus.consume.");
    let denied_open = entries
        .iter()
        .find(|e| e.action == "bus.consume.open" && e.result == "denied")
        .unwrap_or_else(|| panic!("expected denied bus.consume.open audit entry; got {entries:?}"));
    assert_eq!(denied_open.error_message.as_deref(), Some("missing_permission"));

    let denied_next = entries
        .iter()
        .find(|e| e.action == "bus.consume.next" && e.result == "denied")
        .unwrap_or_else(|| panic!("expected denied bus.consume.next audit entry; got {entries:?}"));
    assert_eq!(denied_next.error_message.as_deref(), Some("missing_permission"));
}

/// No cross-org leak: a topic published under org A must be invisible (not
/// merely permission-denied — genuinely not found) to a consumer opened
/// under a DIFFERENT org, same addon. Mirrors the M2
/// `multi_tenant_org_isolation_full.rs` pattern (insert under org A, assert
/// invisible from org B) but through the WASM addon boundary this time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires sdk-showcase WASM build"]
async fn bus_addon_org_isolation_no_cross_org_leak() {
    let _g = lock();
    let Some(wasm) = load_wasm() else {
        panic!("sdk-showcase WASM missing");
    };
    let db = shared_env().db.clone();
    let topic = "bus.addon.e2e.orgiso";

    let (mut store_a, instance_a) = create_wasm_instance(
        db.clone(),
        vec![PERM_BUS_PUBLISH.to_string(), PERM_BUS_SUBSCRIBE.to_string()],
        Some("bus-addon-test-org-a".to_string()),
        &wasm,
    );
    let pub_resp = call_on_request(
        &mut store_a,
        &instance_a,
        "run_bus_publish_batch",
        json!({"topic": topic, "count": 4, "size": 16, "create_if_missing": true}),
    )
    .expect("publish org-a on_request");
    assert_eq!(pub_resp["ok"], Value::Bool(true), "resp={pub_resp}");
    assert_eq!(pub_resp["data"]["published"], 4);

    let (mut store_b, instance_b) = create_wasm_instance(
        db.clone(),
        vec![PERM_BUS_PUBLISH.to_string(), PERM_BUS_SUBSCRIBE.to_string()],
        Some("bus-addon-test-org-b".to_string()),
        &wasm,
    );
    let open_b = call_on_request(
        &mut store_b,
        &instance_b,
        "run_bus_open",
        json!({"topics": [topic], "group": "bus-addon-e2e-orgiso-b"}),
    )
    .expect("open org-b on_request");
    assert_eq!(
        open_b["ok"],
        Value::Bool(false),
        "org B must not be able to open a consumer on org A's topic, resp={open_b}"
    );
    assert_eq!(
        open_b["abi_error"].as_i64(),
        Some(AbiError::NotFound.as_i32() as i64),
        "expected NotFound (topic does not exist under org B's scope), resp={open_b}"
    );

    let entries = fetch_audit_entries(&db, "bus.consume.open");
    assert!(
        entries.iter().any(|e| e.result == "error"),
        "expected an error-result bus.consume.open audit entry for org B, entries={entries:?}"
    );
}
