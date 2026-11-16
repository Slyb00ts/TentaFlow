//! Runs started from the chat surface reach the Events log.
//!
//! The AI entry points stamp their meta and call
//! `FlowRequestMeta::record_to_event_log`; without a progress sink a run
//! publishes nothing. The content of every step comes from the engine itself:
//! the flow is fed a real prompt and the executor reports what the nodes
//! received and produced. The checks here are what that call has to guarantee: the run is recorded under a scope of its own (never
//! the client's session id), under its organisation, with its step content in
//! full, while a bus run still stores no bodies.
//!
//! Own test binary because `progress_log::start` publishes a process-wide log.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tentaflow_core::db::DbPool;
use tentaflow_core::events::{self, EventKind, EventPayload, StoredEvent};
use tentaflow_core::flow_engine::cache::CompiledFlow;
use tentaflow_core::flow_engine::dispatcher::{FlowActor, FlowOrigin, FlowRequestMeta};
use tentaflow_core::flow_engine::dispatchers::{LlmDispatcher, LlmRequest, LlmResponse};
use tentaflow_core::flow_engine::envelope::LlmStreamChunk;
use tentaflow_core::flow_engine::envelope::{FlowEnvelope, FlowValue};
use tentaflow_core::flow_engine::executor::execute_blocking;
use tentaflow_core::flow_engine::node_adapter::test_support::stub_ctx;
use tentaflow_core::flow_engine::node_adapter::AdapterRegistry;
use tentaflow_core::flow_engine::node_adapters::{
    LlmNodeAdapter, OutputNodeAdapter, TriggerNodeAdapter,
};
use tentaflow_core::flow_engine::progress_broker::{
    begin_run, global_broker, session_scope_key, RunDescriptor,
};
use tentaflow_core::routing::direct_run::DirectRun;

const LLM_FLOW_ID: &str = "flow-chat-llm";
const LLM_FLOW_JSON: &str = r#"{
    "nodes":[
        {"id":"t1","type":"trigger","config":{}},
        {"id":"m1","type":"llm","config":{"model":"m","stream":false}},
        {"id":"o1","type":"output","config":{}}
    ],
    "edges":[
        {"from":"t1","to":"m1","from_port":"text","to_port":"in"},
        {"from":"m1","to":"o1","from_port":"full","to_port":"text"}
    ]
}"#;
const FLOW_ID: &str = "flow-chat-run";
const SESSION: &str = "conversation-1";
const PROMPT: &str = "the user's prompt";
const FLOW_JSON: &str = r#"{
    "nodes":[
        {"id":"t1","type":"trigger","config":{}},
        {"id":"o1","type":"output","config":{}}
    ],
    "edges":[{"from":"t1","to":"o1","from_port":"text","to_port":"text"}]
}"#;

/// A model that never answers: it tells the test it was asked and then waits.
struct HangingLlm {
    asked: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl LlmDispatcher for HangingLlm {
    async fn execute_chat(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.asked.notify_one();
        std::future::pending().await
    }

    async fn stream_chat(
        &self,
        _req: LlmRequest,
    ) -> anyhow::Result<futures::stream::BoxStream<'static, anyhow::Result<LlmStreamChunk>>> {
        unreachable!("the blocking flow never streams")
    }
}

fn chat_meta(run_id: &str, user: &str, org: &str) -> FlowRequestMeta {
    let mut meta = FlowRequestMeta::new(run_id, FlowOrigin::Chat, FlowActor::user(user));
    meta.user_id = Some(user.to_string());
    meta.org_id = Some(org.to_string());
    meta.session_id = Some(SESSION.to_string());
    meta.record_to_event_log();
    meta
}

/// Runs trigger -> output for `meta` on a real prompt; the executor reports the
/// content of each step.
async fn run_chat(db: &DbPool, meta: &FlowRequestMeta) {
    begin_run(meta, RunDescriptor::default().with_flow(FLOW_ID));
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(TriggerNodeAdapter::new()));
    registry.register(Arc::new(OutputNodeAdapter::new()));
    let registry = Arc::new(registry);
    let compiled =
        Arc::new(CompiledFlow::from_json(FLOW_ID, FLOW_JSON, &registry).expect("compile flow"));
    let mut ctx = stub_ctx();
    ctx.request_id = meta.request_id.clone();
    ctx.session_id = meta.session_id.clone();
    ctx.org_id = meta.org_id.clone();
    ctx.origin = meta.origin;
    ctx.actor_kind = meta.actor_kind;
    ctx.actor_id = meta.actor_id.clone();
    ctx.progress = meta.progress_sink.clone().expect("progress sink");
    ctx.progress_scope = meta.progress_scope();
    execute_blocking(
        db.clone(),
        compiled,
        FlowEnvelope::with_payload(FlowValue::Text(PROMPT.to_string())),
        ctx,
        registry,
    )
    .await
    .expect("flow runs");
}

/// Chat step content is stored in full: the step row is there and carries
/// the bodies.
fn assert_step_bodies_kept(rows: &[StoredEvent]) {
    let io: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == EventKind::StepIo)
        .collect();
    assert!(!io.is_empty(), "the step row is kept: {rows:?}");
    for row in io {
        let EventPayload::StepIo {
            z,
            input_bytes,
            output_bytes,
            ..
        } = &row.payload
        else {
            unreachable!()
        };
        let body = events::step_io::unpack(z).expect("unpack");
        assert!(
            body.input.contains(PROMPT) || body.output.contains(PROMPT),
            "the prompt the flow was given is stored: {body:?}"
        );
        assert!(*input_bytes + *output_bytes > 0);
    }
}

async fn rows_after_end(pool: &DbPool, run_id: &str) -> Vec<StoredEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let rows = events::read_run(pool, run_id, 0, 1000).expect("read run");
        // The end event is the last row of a run, so everything before it is on
        // the log already.
        if rows.last().map(|r| r.kind) == Some(EventKind::RequestFinished) {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "chat run {run_id} never got an end event: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_runs_are_recorded_under_their_own_scope_and_organisation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("init db");
    db.write()
        .expect("db lock")
        .execute(
            "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'chat run', '{}', 'active')",
            rusqlite::params![FLOW_ID],
        )
        .expect("seed flow");
    let pool = events::db::init(&tmp.path().join("events.db")).expect("events db");
    events::progress_log::start(pool.clone());

    // Anyone can watch the client-chosen session id; nothing may be published there.
    let mut session_watcher = global_broker().subscribe(SESSION);

    // Two principals, one conversation id.
    let a = chat_meta("chat-run-a", "user-a", "org-a");
    let b = chat_meta("chat-run-b", "user-b", "org-b");
    assert_ne!(
        a.progress_scope(),
        SESSION,
        "the scope is not the session id"
    );
    assert_ne!(a.progress_scope(), b.progress_scope());
    run_chat(&db, &a).await;
    run_chat(&db, &b).await;

    let rows_a = rows_after_end(&pool, "chat-run-a").await;
    assert_eq!(
        rows_a.first().map(|r| r.kind),
        Some(EventKind::RequestStarted)
    );
    assert!(rows_a.iter().all(|r| r.origin == "chat"));
    assert!(rows_a.iter().all(|r| r.org_id.as_deref() == Some("org-a")));
    assert!(rows_a
        .iter()
        .all(|r| r.actor_id.as_deref() == Some("user-a")));
    assert_step_bodies_kept(&rows_a);

    let rows_b = rows_after_end(&pool, "chat-run-b").await;
    assert_eq!(
        rows_b
            .iter()
            .filter(|r| r.kind == EventKind::RequestFinished)
            .count(),
        1,
        "the second principal's run is recorded, not suppressed: {rows_b:?}"
    );
    assert!(rows_b.iter().all(|r| r.org_id.as_deref() == Some("org-b")));
    assert_step_bodies_kept(&rows_b);

    // A bus run is the one origin that never keeps step content: its records
    // may carry data the publisher's field policies hide.
    let mut bus = FlowRequestMeta::new(
        "bus-run",
        FlowOrigin::Bus,
        FlowActor::api_key("key-sa", None),
    );
    bus.org_id = Some("org-bus".to_string());
    bus.record_to_event_log();
    run_chat(&db, &bus).await;
    let rows_bus = rows_after_end(&pool, "bus-run").await;
    assert!(
        !rows_bus.iter().any(|r| r.kind == EventKind::StepIo),
        "a bus run stores no bodies: {rows_bus:?}"
    );

    // The conversation's live view: a run started through `begin_run` with a
    // real request is mirrored onto the owner's own session key, tagged with the
    // run it belongs to, and not onto another user's key for the same id.
    let mut owner_view = global_broker().subscribe(&session_scope_key("user-m", SESSION));
    let mut other_view = global_broker().subscribe(&session_scope_key("user-x", SESSION));
    let mirrored = chat_meta("chat-run-mirrored", "user-m", "org-m");
    run_chat(&db, &mirrored).await;
    let mut tagged = Vec::new();
    while let Ok(stamped) = owner_view.try_recv() {
        tagged.push(stamped);
    }
    assert!(
        !tagged.is_empty()
            && tagged
                .iter()
                .all(|e| e.run_scope.as_deref() == Some("chat-run-mirrored")),
        "mirrored events name their run: {tagged:?}"
    );
    assert!(
        !tagged.iter().any(|e| matches!(
            e.event,
            tentaflow_core::flow_engine::dispatchers::ProgressEvent::NodeIo { .. }
        )),
        "step content is not copied to the live view"
    );
    assert!(
        tagged.iter().any(|e| matches!(
            &e.event,
            tentaflow_core::flow_engine::dispatchers::ProgressEvent::RunFinished { status, .. }
                if status == "completed"
        )),
        "the end of the run reaches the conversation's subscriber: {tagged:?}"
    );
    assert!(other_view.try_recv().is_err());

    // A run whose future is dropped in the middle of an `llm` step (a timeout, a
    // client that went away) still keeps the prompt that step had reported, and
    // closes as cancelled.
    db.write()
        .expect("db lock")
        .execute(
            "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'chat llm', '{}', 'active')",
            rusqlite::params![LLM_FLOW_ID],
        )
        .expect("seed llm flow");
    let cut = chat_meta("chat-run-cut", "user-c", "org-c");
    begin_run(&cut, RunDescriptor::default().with_flow(LLM_FLOW_ID));
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(TriggerNodeAdapter::new()));
    registry.register(Arc::new(LlmNodeAdapter::new()));
    registry.register(Arc::new(OutputNodeAdapter::new()));
    let registry = Arc::new(registry);
    let compiled = Arc::new(
        CompiledFlow::from_json(LLM_FLOW_ID, LLM_FLOW_JSON, &registry).expect("compile llm flow"),
    );
    let asked = Arc::new(tokio::sync::Notify::new());
    let mut ctx = stub_ctx();
    ctx.request_id = cut.request_id.clone();
    ctx.org_id = cut.org_id.clone();
    ctx.origin = cut.origin;
    ctx.progress = cut.progress_sink.clone().expect("progress sink");
    ctx.progress_scope = cut.progress_scope();
    ctx.llm = Arc::new(HangingLlm {
        asked: asked.clone(),
    });
    let running = tokio::spawn(execute_blocking(
        db.clone(),
        compiled,
        FlowEnvelope::with_payload(FlowValue::Text(PROMPT.to_string())),
        ctx,
        registry,
    ));
    tokio::time::timeout(Duration::from_secs(20), asked.notified())
        .await
        .expect("the model was asked");
    running.abort();
    let _ = running.await;
    let rows_cut = rows_after_end(&pool, "chat-run-cut").await;
    let EventPayload::RequestFinished { status, .. } = &rows_cut.last().expect("rows").payload
    else {
        panic!("not a request_finished row: {rows_cut:?}");
    };
    assert_eq!(status, "cancelled");
    let io = rows_cut
        .iter()
        .find(|r| r.kind == EventKind::StepIo && r.node_id.as_deref() == Some("m1"))
        .expect("the llm step kept what it had reported");
    let EventPayload::StepIo { z, .. } = &io.payload else {
        unreachable!()
    };
    assert!(events::step_io::unpack(z)
        .expect("unpack")
        .input
        .contains(PROMPT));

    // An addon block inside a chat run calls a model: the worker's provenance
    // names the chat run as parent, so the call is one more step of that run.
    // Inside a Bus run (which keeps no content) it stores nothing at all.
    let host = chat_meta("chat-run-host", "user-h", "org-h");
    begin_run(&host, RunDescriptor::default().with_flow(FLOW_ID));
    let sink = host.progress_sink.clone().expect("progress sink");
    let scope = host.progress_scope();
    let provenance = tentaflow_core::addon::AddonCallProvenance::nested(
        tentaflow_core::flow_engine::dispatchers::ParentRun::new(
            FlowOrigin::Chat,
            sink.clone(),
            scope.clone(),
        ),
    );
    let call = tentaflow_core::compliance::ai_gateway::AiGatewayContext {
        parent_run: provenance.parent.clone(),
        ..Default::default()
    };
    let model_request: tentaflow_core::api::openai::types::ChatCompletionRequest =
        serde_json::from_value(serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "the addon's own prompt"}],
        }))
        .expect("request");
    let child = DirectRun::begin(
        &model_request,
        provenance.origin,
        &provenance.actor_for("notes"),
        None,
        None,
        Some(&call),
        None,
    )
    .expect("a call inside a chat run is recorded");
    child.finish_output(r#"{"choices":[{"message":{"content":"the addon's answer"}}]}"#.into());
    sink.emit(
        &scope,
        tentaflow_core::flow_engine::dispatchers::ProgressEvent::RunFinished {
            run_id: "chat-run-host".into(),
            status: "completed".into(),
        },
    );
    let rows_host = rows_after_end(&pool, "chat-run-host").await;
    let child_io = rows_host
        .iter()
        .find(|r| {
            r.kind == EventKind::StepIo
                && r.node_id
                    .as_deref()
                    .is_some_and(|n| n.starts_with("llm-call-"))
        })
        .expect("the addon's model call is a step of the chat run");
    let EventPayload::StepIo { z, .. } = &child_io.payload else {
        unreachable!()
    };
    let body = events::step_io::unpack(z).expect("unpack");
    assert!(body.input.contains("the addon's own prompt"));
    assert!(body.output.contains("the addon's answer"));
    assert!(rows_host.iter().all(|r| r.origin == "chat"));

    let bus_parent = tentaflow_core::flow_engine::dispatchers::ParentRun::new(
        FlowOrigin::Bus,
        sink,
        "bus-run-host".to_string(),
    );
    let bus_provenance = tentaflow_core::addon::AddonCallProvenance::nested(bus_parent);
    let bus_call = tentaflow_core::compliance::ai_gateway::AiGatewayContext {
        parent_run: bus_provenance.parent.clone(),
        ..Default::default()
    };
    assert!(DirectRun::begin(
        &model_request,
        bus_provenance.origin,
        &bus_provenance.actor_for("notes"),
        None,
        None,
        Some(&bus_call),
        None,
    )
    .is_none());
    assert!(
        events::read_run(&pool, "bus-run-host", 0, 100)
            .expect("read run")
            .is_empty(),
        "a model call inside a Bus run leaves nothing on the log"
    );

    assert!(
        session_watcher.try_recv().is_err(),
        "a chat run publishes nothing on the shared session scope"
    );

    events::progress_log::stop();
}
