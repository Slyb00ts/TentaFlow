//! TB-02 gate: one correlation id follows a chain of reactive flows across
//! TentaBus. A record entering topic A with an id runs flow F1 (`bus_consume`
//! A -> `bus_publish` B), whose record runs flow F2 (`bus_consume` B ->
//! `bus_publish` C). The same `tf.correlation_id` must be on A, B and C, in
//! both `flow_executions` rows, and in the Events browser store (`run_events`),
//! stamped with the run's organisation. The Events browser org pin itself is
//! tested in `dispatch/events_browser.rs`.
//!
//! The entry record is published the way `api::bus_rest` does it: the request's
//! id becomes `BusCallContext::correlation_id` and `bus::correlation::
//! settle_records` settles the records. The REST handler itself is driven by
//! the unit tests in `api/bus_rest.rs` (it needs a hyper request with an
//! authenticated principal, which only the crate-private harness builds).
//!
//! The reactor's dispatch (`RelayDispatch`) mirrors `ContextFactory::make_context`
//! and `FlowDispatcher::dispatch_by_flow_id_background`:
//! the run meta comes from the production `agents::reactive_run_meta` and the
//! run is opened with `begin_run`, so the timeline sees what it sees in the
//! running app. Like `bus_flow_chain_p11_gate.rs` this registers an engine in
//! the process registry, which is why it is its own test binary.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use tentaflow_core::agents::{reactive_run_meta, AgentPrincipal, ReactorFlowDispatch};
use tentaflow_core::bus::{
    self, groups::CommitMode, topics, BusAction, BusCallContext, BusInitConfig, BusService,
    BusServiceError, ConsumerConfig, PublishBatch, PublishRecord,
};
use tentaflow_core::db::models::FlowParams;
use tentaflow_core::db::{repository, DbPool};
use tentaflow_core::events::{self, EventFilter, EventPayload, RunEvent};
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::dispatcher::{FlowActor, FlowOrigin};
use tentaflow_core::flow_engine::envelope::{FinishReason, FlowEnvelope};
use tentaflow_core::flow_engine::executor::execute_blocking;
use tentaflow_core::flow_engine::node_adapter::test_support::stub_ctx;
use tentaflow_core::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext};
use tentaflow_core::flow_engine::node_adapters::{BusConsumeNodeAdapter, BusPublishNodeAdapter};
use tentaflow_core::flow_engine::progress_broker::{begin_run, RunDescriptor};

const ORG_ID: &str = "org-default";
const OTHER_ORG_ID: &str = "org-other";
const CORRELATION_ID: &str = "req-tb02-chain-1";
const TOPIC_A: &str = "tb02.a";
const TOPIC_B: &str = "tb02.b";
const TOPIC_C: &str = "tb02.c";

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

fn instance_id() -> bus::instance::BusInstanceId {
    bus::instance::BusInstanceId::parse("tentabus-00000001").expect("valid instance id")
}

fn bus_service() -> Arc<BusService> {
    bus::instance(&instance_id()).expect("bus initialized")
}

fn call_ctx(correlation_id: Option<&str>) -> BusCallContext {
    BusCallContext {
        instance_id: instance_id(),
        org_id: ORG_ID.to_string(),
        actor: Some("tb02-gate".to_string()),
        actor_kind: tentaflow_core::auth::actor::ActorKind::User,
        correlation_id: correlation_id.map(str::to_string),
        origin: "v1.bus.rest".to_string(),
    }
}

fn init_bus(bus_dir: std::path::PathBuf, db: DbPool) {
    let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
    bus::db::migrate(&local_conn).expect("migrate local db");
    let local_db: DbPool = Arc::new(tentaflow_core::db::Db::from_connection(local_conn));
    bus::init(BusInitConfig {
        instance_id: instance_id(),
        local_db,
        bus_dir,
        db,
        authorizer: Arc::new(AllowAllAuthorizer),
        retention_interval: None,
        dedup_expected_rate_per_sec: 10_000,
        partition_handle_lru: None,
        publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        settings_cipher: Some(Arc::new(tentaflow_core::crypto::SettingsCipher::new(
            &[7u8; 32],
        ))),
    })
    .expect("bus::init");
}

/// The reactor skips a subscription whose instance has no enabled `addons`
/// row (see `bus_flow_chain_p11_gate.rs`).
fn enable_bus_instance(db: &DbPool) {
    let conn = db.write().expect("db lock");
    conn.execute(
        "INSERT INTO addons (addon_id, name, version, package_id, is_enabled) \
         VALUES (?1, ?1, '1.0.0', 'tentabus', 1)",
        rusqlite::params![instance_id().as_str()],
    )
    .expect("seed bus instance row");
}

fn create_topic(svc: &BusService, ctx: &BusCallContext, name: &str) {
    svc.create_topic(
        ctx,
        name,
        topics::TopicOptions {
            partitions: Some(1),
            ..Default::default()
        },
    )
    .expect("create_topic");
}

/// `bus_consume(from) -> bus_publish(to)`, one record per run.
fn seed_relay_flow(db: &DbPool, name: &str, from: &str, to: &str, group: &str) -> (String, String) {
    let flow_json = json!({
        "nodes": [
            {"id": "c", "type": "bus_consume", "config": {
                "instance_id": instance_id().as_str(),
                "topic": from,
                "group": group,
                "batch_size": 1,
                "max_wait_ms": 200,
                "on_error": "dlq",
                "org_id": ORG_ID,
            }},
            {"id": "p", "type": "bus_publish", "config": {
                "instance_id": instance_id().as_str(),
                "topic": to,
            }}
        ],
        "edges": [
            {"from": "c", "to": "p", "from_port": "message", "to_port": "in"}
        ]
    })
    .to_string();
    let id = repository::create_flow(
        db,
        &FlowParams {
            name,
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
    r.register(Arc::new(BusPublishNodeAdapter::new()));
    Arc::new(r)
}

/// What `FlowDispatcher` does for a reactive run, minus the services the flow
/// does not touch.
struct RelayDispatch {
    db: DbPool,
    flows: HashMap<String, Arc<CompiledFlow>>,
    registry: Arc<AdapterRegistry>,
    base_ctx: ExecutionContext,
}

#[async_trait]
impl ReactorFlowDispatch for RelayDispatch {
    async fn dispatch(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        principal: AgentPrincipal,
    ) -> anyhow::Result<()> {
        let compiled = self
            .flows
            .get(&flow_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown flow {flow_id}"))?;
        let meta = reactive_run_meta(uuid::Uuid::new_v4().to_string(), &principal);
        begin_run(&meta, RunDescriptor::default().with_flow(&flow_id));

        let mut ctx = self.base_ctx.clone();
        ctx.request_id = meta.request_id.clone();
        ctx.org_id = meta.org_id.clone();
        ctx.origin = meta.origin;
        ctx.actor_kind = meta.actor_kind;
        ctx.actor_id = meta.actor_id.clone();
        ctx.actor_user_id = meta.actor_user_id.clone();
        ctx.correlation_id = meta.correlation_id.clone();
        ctx.progress = meta
            .progress_sink
            .clone()
            .expect("a reactive run has a progress sink");
        ctx.progress_scope = meta.request_id.clone();

        let outcome = execute_blocking(
            self.db.clone(),
            compiled,
            initial,
            ctx,
            self.registry.clone(),
        )
        .await?;
        if outcome.finish_reason != FinishReason::Stop {
            anyhow::bail!("flow run finished as {:?}", outcome.finish_reason);
        }
        Ok(())
    }
}

/// Every record currently on `topic` as `(payload, tf.correlation_id)`.
fn read_topic(svc: &BusService, topic: &str, group: &str) -> Vec<(Bytes, Option<String>)> {
    let handle = svc
        .open_consumer(
            &call_ctx(None),
            group,
            &[topic.to_string()],
            ConsumerConfig {
                commit_mode: CommitMode::Explicit,
            },
        )
        .expect("open_consumer");
    let batch = handle.fetch(1024 * 1024, 50).expect("fetch");
    batch
        .records
        .iter()
        .map(|r| {
            let id = r
                .headers
                .iter()
                .find(|(k, _)| k.as_ref() == b"tf.correlation_id")
                .map(|(_, v)| String::from_utf8(v.to_vec()).expect("utf-8 id"));
            (r.payload.clone(), id)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_correlation_id_follows_a_chain_of_reactive_flows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("init db");
    enable_bus_instance(&db);
    let events_pool = events::db::init(&tmp.path().join("events.db")).expect("events db");
    events::progress_log::start(events_pool.clone(), db.clone());

    {
        let (db, bus_dir) = (db.clone(), tmp.path().join("bus"));
        tokio::task::spawn_blocking(move || init_bus(bus_dir, db))
            .await
            .expect("init_bus task");
    }

    let (f1, f1_json) = seed_relay_flow(&db, "tb02-f1", TOPIC_A, TOPIC_B, "tb02-g1");
    let (f2, f2_json) = seed_relay_flow(&db, "tb02-f2", TOPIC_B, TOPIC_C, "tb02-g2");
    let mut compiled = HashMap::new();
    for (id, json) in [(&f1, &f1_json), (&f2, &f2_json)] {
        compiled.insert(
            id.clone(),
            Arc::new(CompiledFlow::from_json(id, json, &registry()).expect("compile flow")),
        );
    }

    // The entry record, published the way the REST handler does.
    {
        let rest_ctx = call_ctx(Some(CORRELATION_ID));
        tokio::task::spawn_blocking(move || {
            let svc = bus_service();
            for topic in [TOPIC_A, TOPIC_B, TOPIC_C] {
                create_topic(&svc, &call_ctx(None), topic);
            }
            let mut records = vec![PublishRecord {
                key: None,
                headers: vec![],
                payload: Bytes::from(json!({"order": 7}).to_string()),
                timestamp_ms: 1,
                schema_id: 0,
            }];
            let ids =
                bus::correlation::settle_records(&mut records, rest_ctx.correlation_id.as_deref());
            assert_eq!(ids, vec![CORRELATION_ID.to_string()]);
            svc.publish(
                &rest_ctx,
                TOPIC_A,
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records,
                },
            )
            .expect("publish A");
        })
        .await
        .expect("setup task");
    }

    let mut base_ctx = stub_ctx();
    base_ctx.org_id = Some(ORG_ID.to_string());
    let dispatch: Arc<dyn ReactorFlowDispatch> = Arc::new(RelayDispatch {
        db: db.clone(),
        flows: compiled,
        registry: registry(),
        base_ctx,
    });
    let cancel = CancellationToken::new();
    let _reactor = bus::reactor::start(db.clone(), dispatch, cancel.clone());

    // C holds the record once both relays ran (the reactor's discovery scan
    // runs every 5 s).
    let deadline = Instant::now() + Duration::from_secs(90);
    let on_c = loop {
        let on_c =
            tokio::task::spawn_blocking(|| read_topic(&bus_service(), TOPIC_C, "tb02-verify-c"))
                .await
                .expect("read C");
        if !on_c.is_empty() {
            break on_c;
        }
        assert!(
            Instant::now() < deadline,
            "the record never reached topic C"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    cancel.cancel();

    let (on_a, on_b) = tokio::task::spawn_blocking(|| {
        let svc = bus_service();
        (
            read_topic(&svc, TOPIC_A, "tb02-verify-a"),
            read_topic(&svc, TOPIC_B, "tb02-verify-b"),
        )
    })
    .await
    .expect("read A and B");
    for (name, records) in [("A", &on_a), ("B", &on_b), ("C", &on_c)] {
        assert_eq!(records.len(), 1, "topic {name}");
        assert_eq!(
            records[0].1.as_deref(),
            Some(CORRELATION_ID),
            "topic {name} carries the entry id"
        );
    }

    for flow_id in [&f1, &f2] {
        let rows = repository::list_flow_executions_for_flow(&db, flow_id, 10).expect("rows");
        assert!(!rows.is_empty(), "flow {flow_id} ran");
        assert!(
            rows.iter()
                .all(|r| r.correlation_id.as_deref() == Some(CORRELATION_ID)),
            "flow_executions of {flow_id} carry the id: {rows:?}"
        );
    }

    // A run of another organisation under the same id, as a foreign tenant
    // reusing a guessed id would produce.
    events::append(
        &events_pool,
        &db,
        RunEvent::new(
            "run-foreign",
            1,
            FlowOrigin::Bus,
            &FlowActor::system(),
            EventPayload::RequestStarted {
                model: None,
                flow_id: None,
                service_type: None,
                modality: None,
            },
        )
        .with_correlation(CORRELATION_ID)
        .with_org(OTHER_ORG_ID),
    )
    .expect("append foreign run");

    // The timeline is written asynchronously by the progress subscriber.
    let browse = |org: &str| {
        events::browse(
            &events_pool,
            &EventFilter {
                origins: None,
                actor_id: None,
                org_id: Some(org),
                session_id: None,
                correlation_id: Some(CORRELATION_ID),
                from_ms: None,
                to_ms: None,
                search: None,
                restrict_to_actor_user_id: None,
                cursor: None,
            },
            500,
        )
        .expect("browse")
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let runs = loop {
        let mut runs: Vec<String> = browse(ORG_ID)
            .events
            .into_iter()
            .map(|e| e.run_id)
            .collect();
        runs.sort();
        runs.dedup();
        if runs.len() >= 2 {
            break runs;
        }
        assert!(
            Instant::now() < deadline,
            "both runs must reach run_events, got {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(runs.len(), 2, "exactly the two relay runs: {runs:?}");
    assert!(!runs.iter().any(|r| r == "run-foreign"));

    let foreign: Vec<String> = browse(OTHER_ORG_ID)
        .events
        .into_iter()
        .map(|e| e.run_id)
        .collect();
    assert_eq!(foreign, vec!["run-foreign".to_string()]);
}
