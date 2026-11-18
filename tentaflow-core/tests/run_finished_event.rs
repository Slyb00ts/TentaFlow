//! TB-02: every top-level flow run ends with exactly one `request_finished`
//! row in the Events log, whatever its origin and whether or not it called an
//! LLM. The run here is trigger -> output: no model, no streaming, so the only
//! thing that can close it in the timeline is the engine's own run-finished
//! event.
//!
//! The run is wired the way `bus_correlation_chain.rs` wires one (the meta
//! from `FlowRequestMeta`, `begin_run`, then `execute_blocking`), and it is its
//! own test binary because `progress_log::start` publishes a process-wide log.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tentaflow_core::db::DbPool;
use tentaflow_core::events::{self, EventKind, EventPayload, StoredEvent};
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
use tentaflow_core::flow_engine::envelope::FlowEnvelope;
use tentaflow_core::flow_engine::executor::execute_blocking;
use tentaflow_core::flow_engine::node_adapter::test_support::stub_ctx;
use tentaflow_core::flow_engine::node_adapter::AdapterRegistry;
use tentaflow_core::flow_engine::node_adapters::{OutputNodeAdapter, TriggerNodeAdapter};
use tentaflow_core::flow_engine::progress_broker::{
    begin_run, global_broker, BrokerProgressSink, RunDescriptor,
};

const ORG: &str = "org-default";
const FLOW_ID: &str = "flow-run-end";
const FLOW_JSON: &str = r#"{
    "nodes":[
        {"id":"t1","type":"trigger","config":{}},
        {"id":"o1","type":"output","config":{}}
    ],
    "edges":[{"from":"t1","to":"o1","from_port":"text","to_port":"text"}]
}"#;

async fn run_flow(db: &DbPool, origin: FlowOrigin, actor: FlowActor, request_id: &str, corr: &str) {
    let mut meta = FlowRequestMeta::new(request_id, origin, actor);
    meta.org_id = Some(ORG.to_string());
    meta.correlation_id = Some(corr.to_string());
    meta.progress_sink = Some(Arc::new(BrokerProgressSink::new(global_broker())));
    begin_run(&meta, RunDescriptor::default().with_flow(FLOW_ID));

    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(TriggerNodeAdapter::new()));
    registry.register(Arc::new(OutputNodeAdapter::new()));
    let registry = Arc::new(registry);
    let compiled =
        Arc::new(CompiledFlow::from_json(FLOW_ID, FLOW_JSON, &registry).expect("compile flow"));

    let mut ctx = stub_ctx();
    ctx.request_id = meta.request_id.clone();
    ctx.org_id = meta.org_id.clone();
    ctx.origin = meta.origin;
    ctx.actor_kind = meta.actor_kind;
    ctx.actor_id = meta.actor_id.clone();
    ctx.actor_user_id = meta.actor_user_id.clone();
    ctx.correlation_id = meta.correlation_id.clone();
    ctx.progress = meta.progress_sink.clone().expect("progress sink");
    ctx.progress_scope = meta.request_id.clone();
    execute_blocking(db.clone(), compiled, FlowEnvelope::empty(), ctx, registry)
        .await
        .expect("flow runs");
}

/// The rows of `run_id` once its end row has landed, plus a settle period for
/// anything that would still be written after it.
async fn rows_after_end(pool: &DbPool, run_id: &str) -> Vec<StoredEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let rows = events::read_run(pool, run_id, 0, 1000).expect("read run");
        if rows.iter().any(|r| r.kind == EventKind::RequestFinished) {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return events::read_run(pool, run_id, 0, 1000).expect("read run");
        }
        assert!(
            Instant::now() < deadline,
            "run {run_id} never got an end event: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_one_clean_end(rows: &[StoredEvent], corr: &str) {
    let ends: Vec<&StoredEvent> = rows
        .iter()
        .filter(|r| r.kind == EventKind::RequestFinished)
        .collect();
    assert_eq!(ends.len(), 1, "exactly one end event: {rows:?}");
    let EventPayload::RequestFinished { status, .. } = &ends[0].payload else {
        panic!("not a request_finished payload: {:?}", ends[0].payload);
    };
    assert_eq!(status, "completed");
    assert_eq!(ends[0].correlation_id.as_deref(), Some(corr));
    assert_eq!(ends[0].org_id.as_deref(), Some(ORG));
    let last = rows.iter().max_by_key(|r| r.seq).expect("rows");
    assert_eq!(last.seq, ends[0].seq, "the end event closes the run");
    assert!(
        !rows.iter().any(|r| r.kind == EventKind::AssistantMessage),
        "no LLM was called, so nothing but the end event can close this run"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bus_run_and_a_dashboard_run_without_llm_each_end_with_one_event() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("init db");
    db.write()
        .expect("db lock")
        .execute(
            "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'run end', '{}', 'active')",
            rusqlite::params![FLOW_ID],
        )
        .expect("seed flow");
    let pool = events::db::init(&tmp.path().join("events.db")).expect("events db");
    events::progress_log::start(pool.clone());

    run_flow(
        &db,
        FlowOrigin::Bus,
        FlowActor::api_key("key-sa", None),
        "run-end-bus",
        "corr-end-bus",
    )
    .await;
    run_flow(
        &db,
        FlowOrigin::Dashboard,
        FlowActor::user("user-1".to_string()),
        "run-end-dashboard",
        "corr-end-dashboard",
    )
    .await;

    let bus = rows_after_end(&pool, "run-end-bus").await;
    assert_one_clean_end(&bus, "corr-end-bus");
    assert!(
        !bus.iter().any(|r| r.kind == EventKind::StepIo),
        "a bus run stores no step content: {bus:?}"
    );
    let mirrored: i64 = pool
        .read()
        .expect("events read")
        .query_row(
            "SELECT COUNT(*) FROM audit_outbox WHERE run_id = 'run-end-bus'",
            [],
            |row| row.get(0),
        )
        .expect("count outbox");
    assert_eq!(
        mirrored, 0,
        "a bus run has no audit mirror, end event included"
    );

    let dashboard = rows_after_end(&pool, "run-end-dashboard").await;
    assert_one_clean_end(&dashboard, "corr-end-dashboard");
    let mirrored: i64 = pool
        .read()
        .expect("events read")
        .query_row(
            "SELECT COUNT(*) FROM audit_outbox WHERE run_id = 'run-end-dashboard'",
            [],
            |row| row.get(0),
        )
        .expect("count outbox");
    assert_eq!(
        mirrored, 1,
        "only the opening of a dashboard run is mirrored"
    );

    events::progress_log::stop();
}
