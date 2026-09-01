//! M3a gate (SUM/tentabus/PLAN.md §9, "Bramka" line for M3a): "P11 · e2e:
//! `bus_consume -> bus_transform -> bus_publish` na 100k komunikatów". Unlike
//! `bus_full_path_1m.rs` (which drives the raw `BusService` API directly),
//! this file drives the REAL production reactive path: a flow whose entry is
//! `bus_consume` is registered in the database, `bus::reactor::start` (the
//! same public entry point `bus::reactor::init_global` uses at real app
//! startup) discovers it via its normal 5s reconcile scan, and every fetched
//! batch runs through the real `bus_consume -> bus_transform -> bus_publish`
//! node chain via `flow_engine::executor::execute_blocking`.
//!
//! SINGLETON NOTE: this file calls `bus::init`/`bus::global()` (the
//! process-global `BusService`), NOT `BusService::new` directly — unlike
//! `bus_full_path_1m.rs`, this test genuinely needs the global singleton,
//! because `BusPublishNodeAdapter::execute` calls `bus::global()` internally
//! with no injection point (see `bus/reactor.rs`'s own module-level test
//! doc, and POSTEP.md's "odkryty i świadomie obejściowy hazard" section, for
//! why `bus::reactor`'s OWN unit tests deliberately avoid the singleton).
//! That hazard is specific to sharing the singleton with OTHER `#[cfg(test)]`
//! modules inside the same `cargo test --lib` process — it does not apply
//! here: every `tests/*.rs` file compiles into its own separate integration
//! test binary/process, so this file's `bus::init` call has a fresh,
//! uncontended `OnceLock`, entirely isolated from `dispatch::bus::tests::
//! bus_fixture()` or any other unit test module.
//!
//! Run the actual gate (release build, otherwise the throughput number is
//! meaningless):
//!   cargo test --release --test bus_flow_chain_p11_gate -- --ignored --nocapture \
//!     p11_gate_100k_messages_through_bus_consume_transform_publish

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use tentaflow_core::agents::{AgentPrincipal, ReactorFlowDispatch};
use tentaflow_core::bus::{
    self, groups::CommitMode, topics, BusAction, BusCallContext, BusInitConfig, BusService,
    BusServiceError, ConsumerConfig, PublishBatch, PublishRecord,
};
use tentaflow_core::db::models::FlowParams;
use tentaflow_core::db::{repository, DbPool};
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::envelope::{FinishReason, FlowEnvelope, FlowValue};
use tentaflow_core::flow_engine::executor::execute_blocking;
use tentaflow_core::flow_engine::node_adapter::test_support::stub_ctx;
use tentaflow_core::flow_engine::node_adapter::AdapterRegistry;
use tentaflow_core::flow_engine::node_adapters::{
    BusConsumeNodeAdapter, BusPublishNodeAdapter, BusTransformNodeAdapter,
};

const ORG_ID: &str = "org-default";
const SOURCE_TOPIC: &str = "bus.p11.source";
const DEST_TOPIC: &str = "bus.p11.dest";
const GROUP: &str = "p11-gate";
const BATCH_SIZE: usize = 500; // PLAN §9 P11's literal parameter.

/// Allow-all authorizer — this file tests the reactive flow-engine path, not
/// RBAC (already covered by `src/bus/mod.rs`'s own `#[cfg(test)]` suite and
/// `dispatch::bus::tests`). Duplicated from `bus_full_path_1m.rs`/`bus::
/// reactor`'s own test module rather than shared — each is private to its
/// own file/module.
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}

fn call_ctx() -> BusCallContext {
    BusCallContext {
        org_id: ORG_ID.to_string(),
        actor: Some("p11-gate".to_string()),
        correlation_id: Some("bus-flow-chain-p11-gate".to_string()),
        origin: "p11-gate-test".to_string(),
    }
}

/// Initializes the process-global `BusService` against a private tempdir —
/// safe to call exactly once per test process (see module doc).
fn init_bus(bus_dir: PathBuf, db: DbPool) {
    bus::init(BusInitConfig {
        bus_dir,
        db,
        authorizer: Arc::new(AllowAllAuthorizer),
        retention_interval: None,
        dedup_expected_rate_per_sec: 200_000,
        partition_handle_lru: None,
        publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
    })
    .expect("bus::init");
}

fn create_topic(svc: &BusService, ctx: &BusCallContext, name: &str) {
    svc.create_topic(
        ctx,
        name,
        topics::TopicOptions {
            partitions: Some(1),
            // Explicit Prod-shape durability (PLAN §7.1's "fsync_batch is the
            // Prod default"), matching `bus_full_path_1m.rs`'s own choice —
            // a throughput number measured against a weaker durability tier
            // would not mean what P11's table entry means.
            durability: Some(topics::DurabilityPolicy::FsyncBatch),
            ..Default::default()
        },
    )
    .expect("create_topic");
}

/// Publishes `total` records (`{"seq": N}` JSON bodies, N in `0..total`) to
/// `SOURCE_TOPIC` in chunks, backing off on `QuotaExceeded`/`Throttled` the
/// same way `bus_full_path_1m.rs::publish_with_backoff` does — a burst this
/// size can transiently exceed the default org token bucket even though it
/// is far under its steady-state rate.
fn publish_source_messages(svc: &BusService, ctx: &BusCallContext, total: usize) {
    const CHUNK: usize = 2_000;
    let mut seq = 0usize;
    while seq < total {
        let n = CHUNK.min(total - seq);
        let records: Vec<PublishRecord> = (0..n)
            .map(|i| PublishRecord {
                key: None,
                headers: vec![],
                payload: Bytes::from(json!({"seq": seq + i}).to_string()),
                timestamp_ms: now_ms(),
                schema_id: 0,
            })
            .collect();
        let batch = PublishBatch {
            partition: Some(0),
            producer: None,
            records,
        };
        let mut attempts = 0u32;
        loop {
            match svc.publish(ctx, SOURCE_TOPIC, batch.clone()) {
                Ok(result) => {
                    assert_eq!(result.accepted, n as u32, "publish must accept the whole chunk");
                    break;
                }
                Err(BusServiceError::QuotaExceeded { retry_after_ms })
                | Err(BusServiceError::Throttled { retry_after_ms }) => {
                    attempts += 1;
                    assert!(attempts < 10_000, "publish backed off {attempts} times without succeeding");
                    std::thread::sleep(Duration::from_millis(retry_after_ms.max(1) as u64));
                }
                Err(e) => panic!("publish(source) failed: {e}"),
            }
        }
        seq += n;
    }
}

/// Builds the `bus_consume -> bus_transform -> bus_publish` flow JSON (PLAN
/// §9's P11 gate literal chain), inserts it as an active flow, and returns
/// its assigned id. `bus_transform`'s expression is the identity function
/// (`payload`) — PLAN's own P11 wording is "przebieg flow trywialny", the
/// transform step exists to be present in the chain, not to do work.
fn seed_flow(db: &DbPool) -> (String, String) {
    let flow_json = json!({
        "nodes": [
            {"id": "c", "type": "bus_consume", "config": {
                "topic": SOURCE_TOPIC,
                "group": GROUP,
                "batch_size": BATCH_SIZE,
                "max_wait_ms": 1000,
                "on_error": "dlq",
                "org_id": ORG_ID,
            }},
            {"id": "t", "type": "bus_transform", "config": {"expression": "payload"}},
            {"id": "p", "type": "bus_publish", "config": {"topic": DEST_TOPIC}}
        ],
        "edges": [
            {"from": "c", "to": "t", "from_port": "batch", "to_port": "in"},
            {"from": "t", "to": "p", "from_port": "full", "to_port": "in"}
        ]
    })
    .to_string();
    let id = repository::create_flow(
        db,
        &FlowParams {
            name: "p11-gate",
            description: None,
            is_default: false,
            service_type: Some("chat"),
            flow_json: &flow_json,
            status: "active",
            published_model_name: None,
            actor_user_id: None,
        },
    )
    .expect("create_flow");
    (id, flow_json)
}

fn registry() -> Arc<AdapterRegistry> {
    let mut r = AdapterRegistry::new();
    r.register(Arc::new(BusConsumeNodeAdapter::new()));
    r.register(Arc::new(BusTransformNodeAdapter::new()));
    r.register(Arc::new(BusPublishNodeAdapter::new()));
    Arc::new(r)
}

/// Runs the compiled `bus_consume -> bus_transform -> bus_publish` flow via
/// the real executor for every batch `bus::reactor` fetches, and tallies how
/// many source messages have been carried all the way through. Times the
/// window from the FIRST successful dispatch (excludes `bus::reactor`'s own
/// up-to-5s supervisor discovery latency, which is a one-time detection cost,
/// not part of P11's "batch 500, trivial flow" cycle cost).
struct FlowRunDispatch {
    db: DbPool,
    compiled: Arc<CompiledFlow>,
    registry: Arc<AdapterRegistry>,
    consumed: Arc<AtomicU64>,
    first_dispatch_at: Arc<Mutex<Option<Instant>>>,
    last_dispatch_finished_at: Arc<Mutex<Option<Instant>>>,
}

#[async_trait]
impl ReactorFlowDispatch for FlowRunDispatch {
    async fn dispatch(
        &self,
        _flow_id: String,
        initial: FlowEnvelope,
        _principal: AgentPrincipal,
    ) -> anyhow::Result<()> {
        {
            let mut guard = self.first_dispatch_at.lock().unwrap();
            if guard.is_none() {
                *guard = Some(Instant::now());
            }
        }
        let batch_len = match &initial.payload {
            FlowValue::Json(serde_json::Value::Array(a)) => a.len(),
            FlowValue::Json(_) => 1,
            other => anyhow::bail!("p11 gate: unexpected seeded payload shape: {other:?}"),
        };
        let mut ctx = stub_ctx();
        ctx.org_id = Some(ORG_ID.to_string());
        let outcome = execute_blocking(
            self.db.clone(),
            self.compiled.clone(),
            initial,
            ctx,
            self.registry.clone(),
        )
        .await?;
        if outcome.finish_reason != FinishReason::Stop {
            anyhow::bail!(
                "p11 gate: flow run finished as {:?}, expected Stop",
                outcome.finish_reason
            );
        }
        self.consumed.fetch_add(batch_len as u64, Ordering::Relaxed);
        *self.last_dispatch_finished_at.lock().unwrap() = Some(Instant::now());
        Ok(())
    }
}

/// Drains every record currently on `DEST_TOPIC` and returns the union of
/// every `seq` value found across every published batch — used to verify
/// exactly-once, gap-free delivery end to end (source publish -> bus_consume
/// -> bus_transform -> bus_publish -> dest topic).
fn drain_dest_seqs(svc: &BusService, ctx: &BusCallContext, expected_total: usize) -> Vec<u64> {
    let handle = svc
        .open_consumer(
            ctx,
            "p11-gate-verify",
            &[DEST_TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: CommitMode::Explicit,
            },
        )
        .expect("open_consumer(dest)");
    let mut seqs = Vec::with_capacity(expected_total);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let batch = handle.fetch(8 * 1024 * 1024, 200).expect("fetch(dest)");
        if batch.records.is_empty() {
            if seqs.len() >= expected_total || Instant::now() > deadline {
                break;
            }
            continue;
        }
        for record in &batch.records {
            let value: serde_json::Value =
                serde_json::from_slice(&record.payload).expect("dest record payload must be JSON");
            let array = value.as_array().expect("dest record payload must be a JSON array");
            for item in array {
                let seq = item.get("seq").and_then(|v| v.as_u64()).expect("item must carry 'seq'");
                seqs.push(seq);
            }
        }
    }
    seqs
}

async fn run_gate(total_messages: usize) {
    assert_eq!(
        total_messages % BATCH_SIZE,
        0,
        "this harness assumes an exact multiple of BATCH_SIZE so every cycle is full"
    );

    let tmp = tempfile::tempdir().expect("create temp dir");
    let bus_dir = tmp.path().join("bus");
    let db = tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("init db");

    {
        let db = db.clone();
        tokio::task::spawn_blocking(move || init_bus(bus_dir, db))
            .await
            .expect("init_bus task");
    }

    let ctx = call_ctx();
    {
        let ctx = ctx.clone();
        tokio::task::spawn_blocking(move || {
            let svc = bus::global().expect("bus initialized");
            create_topic(&svc, &ctx, SOURCE_TOPIC);
            create_topic(&svc, &ctx, DEST_TOPIC);
            publish_source_messages(&svc, &ctx, total_messages);
        })
        .await
        .expect("setup task");
    }

    let (flow_id, flow_json) = seed_flow(&db);
    let compiled = Arc::new(
        CompiledFlow::from_json(&flow_id, &flow_json, &registry()).expect("compile p11 gate flow"),
    );

    let consumed = Arc::new(AtomicU64::new(0));
    let first_dispatch_at = Arc::new(Mutex::new(None));
    let last_dispatch_finished_at = Arc::new(Mutex::new(None));
    let dispatch: Arc<dyn ReactorFlowDispatch> = Arc::new(FlowRunDispatch {
        db: db.clone(),
        compiled,
        registry: registry(),
        consumed: consumed.clone(),
        first_dispatch_at: first_dispatch_at.clone(),
        last_dispatch_finished_at: last_dispatch_finished_at.clone(),
    });

    let cancel = CancellationToken::new();
    let _reactor_handle = bus::reactor::start(db.clone(), dispatch, cancel.clone());

    // Supervisor reconcile is every 5s (RECONCILE_INTERVAL) — generous ceiling
    // covers that discovery latency plus the actual consume+execute work.
    let poll_deadline = Instant::now() + Duration::from_secs(300);
    while consumed.load(Ordering::Relaxed) < total_messages as u64 {
        assert!(
            Instant::now() < poll_deadline,
            "p11 gate: only consumed {}/{} messages before timeout",
            consumed.load(Ordering::Relaxed),
            total_messages
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cancel.cancel();

    let start = first_dispatch_at.lock().unwrap().expect("at least one dispatch happened");
    let end = last_dispatch_finished_at
        .lock()
        .unwrap()
        .expect("at least one dispatch finished");
    let elapsed = end.saturating_duration_since(start);
    let msgs_per_sec = total_messages as f64 / elapsed.as_secs_f64().max(1e-9);
    let cycles = total_messages / BATCH_SIZE;
    println!(
        "P11 gate ({total_messages} messages, batch {BATCH_SIZE}, {cycles} cycles): \
         {:.3}s cycle time, {msgs_per_sec:.0} msg/s \
         (PLAN §9 P11: min >= 20 000 msg/s, target >= 50 000 msg/s)",
        elapsed.as_secs_f64()
    );

    let mut seqs = {
        let ctx = ctx.clone();
        tokio::task::spawn_blocking(move || {
            let svc = bus::global().expect("bus initialized");
            drain_dest_seqs(&svc, &ctx, total_messages)
        })
        .await
        .expect("drain task")
    };
    assert_eq!(
        seqs.len(),
        total_messages,
        "dest topic must carry exactly one record per source message (no loss, no duplication)"
    );
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(
        seqs.len(),
        total_messages,
        "every seq 0..{total_messages} must appear EXACTLY once on the dest topic"
    );
    assert_eq!(seqs.first(), Some(&0));
    assert_eq!(seqs.last(), Some(&((total_messages - 1) as u64)));
}

/// The actual PLAN §9 gate: 100,000 messages, batch 500, full `bus_consume ->
/// bus_transform -> bus_publish` chain driven by the real `bus::reactor`.
/// `#[ignore]`d — run explicitly, in `--release`, per this file's header doc;
/// a debug build's throughput number does not mean what P11's table entry
/// means, and 100k messages through the real executor is not something every
/// `cargo test` run should pay for.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn p11_gate_100k_messages_through_bus_consume_transform_publish() {
    run_gate(100_000).await;
}

/// Fast, always-on smoke variant (two cycles) that keeps this gate's wiring
/// alive in every normal `cargo test` run — same flow, same reactor, same
/// dest-topic verification, just two orders of magnitude smaller.
#[tokio::test(flavor = "multi_thread")]
async fn bus_consume_transform_publish_smoke() {
    run_gate(1_000).await;
}
