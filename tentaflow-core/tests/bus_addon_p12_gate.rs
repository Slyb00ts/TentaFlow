//! M3b gate (SUM/tentabus/PLAN.md §9, "Bramka" line for M3b): "P12 · addon
//! `bus_publish_v1`, batch 1000x1KiB (min >= 50 000 msg/s, target
//! >= 150 000 msg/s)". Drives `bus_publish_v1` through the REAL WASM guest
//! (`addons/sdk-showcase`'s "run_bus_publish_batch" tool) — every
//! `on_request` boundary crossing carries a full batch of 1000 records x
//! 1 KiB payload each, never a single message ("nigdy per komunikat", PLAN
//! §6.4).
//!
//! Topic is pre-created with `DurabilityPolicy::FsyncBatch` (Prod default),
//! matching `bus_flow_chain_p11_gate.rs`'s own choice — a throughput number
//! measured against a weaker durability tier would not mean what P12's table
//! entry means.
//!
//! Run the actual gate (release build, otherwise the throughput number is
//! meaningless):
//!   cd addons/sdk-showcase && cargo build --target wasm32-wasip1 --release
//!   cargo test --release --test bus_addon_p12_gate -- --ignored --nocapture \
//!     p12_gate_batch_1000x1kib_via_addon_bus_publish_v1

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex as ParkingMutex;
use serde_json::{json, Value};

use tentaflow_core::addon::event_bus::EventBus;
use tentaflow_core::addon::host_functions;
use tentaflow_core::addon::host_functions::network::NetworkConnectionManager;
use tentaflow_core::addon::oauth_refresh_guard::OAuthRefreshGuard;
use tentaflow_core::addon::permissions::PermissionChecker;
use tentaflow_core::addon::runtime::{compile_module, create_engine, create_linker, instantiate};
use tentaflow_core::addon::{AddonCallProvenance, AddonManifest, AddonState};
use tentaflow_core::bus::{
    self, topics, BusAction, BusCallContext, BusInitConfig, BusServiceError,
};
use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db;

const BUS_TEST_ADDON_WASM: &str =
    "../target-addon-wasm/wasm32-wasip1/release/tentaflow_addon_sdk_showcase.wasm";
const ADDON_ID: &str = "sdk-showcase";
const INSTANCE_ID: &str = "sdk-showcase-bus-p12";
const PERM_BUS_PUBLISH: &str = "bus.publish";

const ORG_ID: &str = "org-default";
const TOPIC: &str = "bus.p12.gate";
/// PLAN §9 P12's literal parameters: batch 1000 records x 1 KiB each.
const BATCH_RECORDS: u64 = 1000;
const RECORD_SIZE_BYTES: u64 = 1024;
const P12_MIN_MSGS_PER_SEC: f64 = 50_000.0;
const P12_TARGET_MSGS_PER_SEC: f64 = 150_000.0;

struct AllowAllAuthorizer;

impl bus::BusAuthorizer for AllowAllAuthorizer {
    fn authorize(
        &self,
        _ctx: &BusCallContext,
        _action: BusAction,
        _topic: &str,
    ) -> Result<(), BusServiceError> {
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

fn load_wasm() -> Option<Vec<u8>> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUS_TEST_ADDON_WASM);
    std::fs::read(&p).ok()
}

fn make_state(db: db::DbPool, permissions: Vec<String>) -> AddonState {
    AddonState {
        addon_id: ADDON_ID.to_string(),
        instance_id: INSTANCE_ID.to_string(),
        user_id: None,
        org_id: None,
        db: db.clone(),
        permissions,
        event_bus: Arc::new(EventBus::new()),
        permission_checker: Arc::new(PermissionChecker::new(db)),
        fuel_consumed: 0,
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
    // One store lives across every cycle of the gate loop (hundreds of
    // on_request calls), unlike the single-shot camera/bus e2e tests this
    // harness was copied from — give it generous headroom accordingly.
    store.set_fuel(100_000_000_000).expect("set_fuel");
    store.epoch_deadline_trap();
    // See bus_addon_integration_e2e.rs's create_test_store: the global 10ms
    // epoch ticker + a one-shot small deadline traps a store that lives
    // across hundreds of real fsync-backed publish calls. This gate reuses
    // ONE store/instance for every cycle, so the budget must cover the
    // whole run, not a single call.
    store.set_epoch_deadline(u64::MAX / 4);
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

fn call_on_request(
    store: &mut wasmtime::Store<AddonState>,
    instance: &wasmtime::Instance,
    tool_name: &str,
    params: Value,
) -> Result<Value, String> {
    let request_json = json!({"tool": tool_name, "params": params, "user_id": 1});
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

    // 8 KiB is plenty for bus_publish_v1's own tiny output (just a record count).
    let out_cap: i32 = 8 * 1024;
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

async fn run_gate(cycles: u64) {
    let Some(wasm) = load_wasm() else {
        panic!(
            "sdk-showcase WASM missing — build with: \
             cd addons/sdk-showcase && cargo build --target wasm32-wasip1 --release"
        );
    };

    let db = db::init(Path::new(":memory:")).expect("init db");
    let tmp = tempfile::tempdir().expect("create temp dir");
    let bus_dir = tmp.path().join("bus");

    {
        let db = db.clone();
        tokio::task::spawn_blocking(move || {
            let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
            bus::db::migrate(&local_conn).expect("migrate local db");
            let local_db: db::DbPool = Arc::new(db::Db::from_connection(local_conn));
            bus::init(BusInitConfig {
                instance_id: bus::instance::BusInstanceId::parse("tentabus-00000001")
                    .expect("valid instance id"),
                local_db,
                bus_dir,
                db,
                authorizer: Arc::new(AllowAllAuthorizer),
                retention_interval: None,
                dedup_expected_rate_per_sec: 200_000,
                partition_handle_lru: None,
                publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .expect("bus::init");
            let svc = bus::global().expect("bus initialized");
            let ctx = BusCallContext {
                instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
                    .expect("BusService::instance_id() is always a valid BusInstanceId"),
                org_id: ORG_ID.to_string(),
                actor: Some("p12-gate".to_string()),
                correlation_id: Some("bus-addon-p12-gate".to_string()),
                origin: "p12-gate-test".to_string(),
            };
            svc.create_topic(
                &ctx,
                TOPIC,
                topics::TopicOptions {
                    partitions: Some(1),
                    durability: Some(topics::DurabilityPolicy::FsyncBatch),
                    ..Default::default()
                },
            )
            .expect("create_topic");
        })
        .await
        .expect("setup task");
    }

    let (mut store, instance) =
        create_wasm_instance(db.clone(), vec![PERM_BUS_PUBLISH.to_string()], &wasm);

    let start = Instant::now();
    for _ in 0..cycles {
        let resp = call_on_request(
            &mut store,
            &instance,
            "run_bus_publish_batch",
            json!({
                "topic": TOPIC,
                "count": BATCH_RECORDS,
                "size": RECORD_SIZE_BYTES,
                "create_if_missing": false,
            }),
        )
        .expect("publish on_request");
        assert_eq!(resp["ok"], Value::Bool(true), "resp={resp}");
        assert_eq!(
            resp["data"]["published"].as_u64(),
            Some(BATCH_RECORDS),
            "resp={resp}"
        );
    }
    let elapsed = start.elapsed();
    let total_messages = cycles * BATCH_RECORDS;
    let msgs_per_sec = total_messages as f64 / elapsed.as_secs_f64().max(1e-9);
    println!(
        "P12 gate ({total_messages} messages, batch {BATCH_RECORDS}x{RECORD_SIZE_BYTES}B, \
         {cycles} on_request calls via bus_publish_v1): {:.3}s, {msgs_per_sec:.0} msg/s \
         (PLAN §9 P12: min >= {P12_MIN_MSGS_PER_SEC:.0} msg/s, target >= {P12_TARGET_MSGS_PER_SEC:.0} msg/s)",
        elapsed.as_secs_f64()
    );
    assert!(
        msgs_per_sec >= P12_MIN_MSGS_PER_SEC,
        "P12 gate FAILED minimum: {msgs_per_sec:.0} msg/s < {P12_MIN_MSGS_PER_SEC:.0} msg/s minimum"
    );
}

/// The actual PLAN §9 gate. `#[ignore]`d — run explicitly, in `--release`,
/// per this file's header doc; a debug build's throughput number does not
/// mean what P12's table entry means.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "throughput gate — release build only, run explicitly (see file header)"]
async fn p12_gate_batch_1000x1kib_via_addon_bus_publish_v1() {
    run_gate(300).await; // 300 * 1000 = 300 000 messages
}
