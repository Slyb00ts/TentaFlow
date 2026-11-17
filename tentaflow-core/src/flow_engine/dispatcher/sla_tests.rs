//! The SLA admission of `FlowDispatcher`, driven through the real dispatch
//! entry points over a real database holding real policies and assignments.
//! The only stand-ins are the nodes: one that takes a configurable time and
//! counts how many run at once, and one that streams what the test feeds it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, Weak};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::sync::{mpsc, watch};

use super::*;
use crate::flow_engine::envelope::{EnvelopeDelta, FinishReason, LlmStreamChunk, NodeInput};
use crate::flow_engine::node_adapter::{NodeAdapter, PortSpec, StreamProducerAdapter};
use crate::flow_engine::types::FlowDataType;
use crate::sla::{repository, GateStats, ServiceGate, SlaPolicySpec};

/// Counts the nodes that run at once. A flow whose node has `hold: true` stays
/// in it until the test opens the hold, so a test decides when the runs end
/// instead of guessing with sleeps.
struct Probe {
    current: AtomicUsize,
    peak: AtomicUsize,
    total: AtomicUsize,
    hold: watch::Sender<bool>,
    /// The cancel token the last run of a node saw: the run's own.
    run_token: Mutex<Option<tokio_util::sync::CancellationToken>>,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            hold: watch::channel(true).0,
            run_token: Mutex::new(None),
        }
    }
}

struct InFlight<'a>(&'a Probe);

impl Probe {
    fn enter(&self) -> InFlight<'_> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        self.total.fetch_add(1, Ordering::SeqCst);
        InFlight(self)
    }

    fn close_hold(&self) {
        self.hold.send_replace(false);
    }

    fn open_hold(&self) {
        self.hold.send_replace(true);
    }

    fn run_token(&self) -> tokio_util::sync::CancellationToken {
        self.run_token
            .lock()
            .unwrap()
            .clone()
            .expect("a run started")
    }

    async fn held(&self) {
        let mut open = self.hold.subscribe();
        open.wait_for(|open| *open).await.expect("probe alive");
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.current.fetch_sub(1, Ordering::SeqCst);
    }
}

type DispatcherSlot = Arc<OnceLock<Weak<FlowDispatcher>>>;

/// Takes `config.delay_ms`. With `config.reenter_flow` it first runs that flow
/// through the dispatcher as a nested call, the way an alias that resolves to a
/// flow does.
struct SlowNode {
    probe: Arc<Probe>,
    dispatcher: DispatcherSlot,
}

#[async_trait]
impl NodeAdapter for SlowNode {
    fn node_type(&self) -> &str {
        "sla_slow"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("text", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("text", FlowDataType::Any)]
    }
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &crate::flow_engine::node_adapter::ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let _in_flight = self.probe.enter();
        *self.probe.run_token.lock().unwrap() = Some(ctx.cancel_token.clone());
        if let (0, Some(flow)) = (
            ctx.subflow_depth,
            node.config.get("reenter_flow").and_then(|v| v.as_str()),
        ) {
            let dispatcher = self
                .dispatcher
                .get()
                .and_then(Weak::upgrade)
                .expect("dispatcher alive");
            let mut meta = FlowRequestMeta::new("nested", FlowOrigin::Api, FlowActor::system());
            // What the executor does for a call a node of this run makes: the
            // call is marked as made from inside the run. The depth is the
            // recursion guard's business and is set independently of it.
            meta.nested_in_run = true;
            meta.flow_depth = node
                .config
                .get("reenter_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u8;
            dispatcher
                .dispatch_by_flow_id(flow.to_string(), (*ctx.initial_envelope).clone(), meta)
                .await
                .map_err(|e| anyhow::anyhow!("nested dispatch: {e}"))?;
        }
        if node.config.get("hold").and_then(|v| v.as_bool()) == Some(true) {
            self.probe.held().await;
        }
        let delay = Duration::from_millis(
            node.config
                .get("delay_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        );
        if node.config.get("stop_on_cancel").and_then(|v| v.as_bool()) == Some(true) {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = ctx.cancel_token.cancelled() => {
                    anyhow::bail!("stopped on cancel");
                }
            }
        } else {
            tokio::time::sleep(delay).await;
        }
        Ok(inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone()))
    }
}

/// Calls `config.model` once, through the real runtime path: the dispatcher a
/// `llm` node uses, the executor, the resolver and the catalog. A model that is
/// published as a flow sends the call back into the dispatcher.
struct ModelCallNode {
    probe: Arc<Probe>,
    fired: AtomicBool,
}

#[async_trait]
impl NodeAdapter for ModelCallNode {
    fn node_type(&self) -> &str {
        "sla_model_call"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("text", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("text", FlowDataType::Any)]
    }
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &crate::flow_engine::node_adapter::ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let _in_flight = self.probe.enter();
        // Only the first run calls out, or a flow published as its own model
        // would call itself for ever.
        if !self.fired.swap(true, Ordering::SeqCst) {
            let model = node
                .config
                .get("model")
                .and_then(|v| v.as_str())
                .expect("model configured");
            let mut request =
                crate::flow_engine::dispatchers::LlmRequest::new(model, ctx.provenance());
            request
                .messages
                .push(crate::flow_engine::envelope::ChatMessage::user("hi"));
            ctx.llm.execute_chat(request).await?;
        }
        Ok(inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone()))
    }
}

/// What a test feeds a streaming run: a chunk, or a failure.
type Feed = std::result::Result<LlmStreamChunk, &'static str>;

/// A stream producer fed by the test: every run takes the next receiver.
#[derive(Default)]
struct StreamNode {
    feeds: Mutex<VecDeque<mpsc::Receiver<Feed>>>,
    run_token: Mutex<Option<tokio_util::sync::CancellationToken>>,
}

impl StreamNode {
    fn run_token(&self) -> tokio_util::sync::CancellationToken {
        self.run_token
            .lock()
            .unwrap()
            .clone()
            .expect("a run started")
    }

    fn feed(&self) -> mpsc::Sender<Feed> {
        let (tx, rx) = mpsc::channel(8);
        self.feeds.lock().unwrap().push_back(rx);
        tx
    }
}

#[async_trait]
impl NodeAdapter for StreamNode {
    fn node_type(&self) -> &str {
        "sla_stream"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("stream", FlowDataType::Text),
            PortSpec::new("full", FlowDataType::Text),
        ]
    }
    async fn execute(
        &self,
        _node: &FlowNode,
        _inputs: &[NodeInput],
        ctx: &crate::flow_engine::node_adapter::ExecutionContext,
    ) -> Result<FlowEnvelope> {
        Ok((*ctx.initial_envelope).clone())
    }
}

#[async_trait]
impl StreamProducerAdapter for StreamNode {
    async fn produce_stream(
        &self,
        _node: &FlowNode,
        _inputs: &[NodeInput],
        ctx: &crate::flow_engine::node_adapter::ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        *self.run_token.lock().unwrap() = Some(ctx.cancel_token.clone());
        let rx = self
            .feeds
            .lock()
            .unwrap()
            .pop_front()
            .expect("the test fed this run");
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| {
                (
                    item.map(EnvelopeDelta::Llm)
                        .map_err(|message| anyhow::anyhow!(message)),
                    rx,
                )
            })
        })
        .boxed())
    }
}

struct Rig {
    dispatcher: Arc<FlowDispatcher>,
    probe: Arc<Probe>,
    streams: Arc<StreamNode>,
    executor: Arc<crate::services::runtime::executor::ModelRuntimeExecutor>,
    catalog: Arc<crate::services::catalog::CatalogProvider>,
    sla: &'static SlaRegistry,
    /// The registry's clock. Frozen, so a call window can never roll over in
    /// the middle of a test.
    clock: Arc<AtomicU64>,
}

/// Where the frozen clock stands: 30 minutes into an hour-long window, which is
/// also inside a 24 h one.
const CLOCK_START_MS: u64 = 1_700_000_000_000 - (1_700_000_000_000 % 86_400_000) + 1_800_000;

fn rig() -> Rig {
    let db = repository::test_pool();
    let probe = Arc::new(Probe::default());
    let slot: DispatcherSlot = Arc::new(OnceLock::new());
    let streams = Arc::new(StreamNode::default());
    let mut adapters = AdapterRegistry::new();
    adapters.register(Arc::new(
        crate::flow_engine::node_adapters::TriggerNodeAdapter::new(),
    ));
    adapters.register(Arc::new(
        crate::flow_engine::node_adapters::OutputNodeAdapter::new(),
    ));
    adapters.register(Arc::new(SlowNode {
        probe: probe.clone(),
        dispatcher: slot.clone(),
    }));
    adapters.register(Arc::new(ModelCallNode {
        probe: probe.clone(),
        fired: AtomicBool::new(false),
    }));
    adapters.register_stream_producer(streams.clone());
    // Each rig has a registry of its own, so a test never sees the gates of
    // another one running beside it.
    let clock = Arc::new(AtomicU64::new(CLOCK_START_MS));
    let handle = clock.clone();
    let sla: &'static SlaRegistry =
        Box::leak(Box::new(SlaRegistry::with_clock(Arc::new(move || {
            handle.load(Ordering::SeqCst)
        }))));
    // The `llm` dispatcher of the nodes is the real one, wired to a real
    // executor, resolver and catalog, so a model call a node makes takes the
    // path it takes in production.
    let runtime_slot: crate::flow_engine::dispatchers_impl::ModelRuntimeSlot =
        Arc::new(parking_lot::RwLock::new(None));
    let blobs: Arc<dyn BlobStore> =
        Arc::new(crate::flow_engine::blob_store::InMemoryBlobStore::new());
    let mut factory = stub_context_factory();
    factory.llm = Arc::new(
        crate::flow_engine::dispatchers_impl::LlmDispatcherImpl::new(
            runtime_slot.clone(),
            blobs,
            None,
            Arc::new(|| "test-node".to_string()),
        ),
    );
    let dispatcher = Arc::new(FlowDispatcher {
        db: db.clone(),
        cache: FlowCache::new(60),
        registry: Arc::new(adapters),
        ctx_factory: Arc::new(factory),
        addon_flow_blocks: parking_lot::RwLock::new(None),
        process_tool_host: parking_lot::RwLock::new(None),
        process_tool_gate: Arc::new(crate::processes::tool_host::ToolCallGate::default()),
        process_agent_runs: parking_lot::RwLock::new(None),
        agent_service: Arc::new(parking_lot::RwLock::new(None)),
        subflow_runner: Arc::new(parking_lot::RwLock::new(None)),
        frame_blobs: Arc::new(crate::flow_engine::blob_store::InMemoryBlobStore::new()),
        process_runtime: parking_lot::Mutex::new(None),
        local_node_id: Arc::new(|| "test-node".to_string()),
        sla: parking_lot::RwLock::new(sla),
    });
    slot.set(Arc::downgrade(&dispatcher)).ok();

    let catalog = Arc::new(crate::services::catalog::CatalogProvider::new());
    let resolver = Arc::new(
        crate::services::runtime::resolver::AliasResolver::new_with_static_id(
            Arc::new(crate::services::handles_cache::LiveHandlesCache::new()),
            "test-node".to_string(),
        ),
    );
    let executor = Arc::new(
        crate::services::runtime::executor::ModelRuntimeExecutor::new(
            catalog.clone(),
            resolver,
            Some(dispatcher.clone()),
            Arc::new(crate::inference::local::LocalInferenceHandler::new(
                crate::inference::shared_inference_manager(),
            )),
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
            Some(db),
        ),
    );
    *runtime_slot.write() = Some(executor.clone());
    Rig {
        dispatcher,
        probe,
        streams,
        executor,
        catalog,
        sla,
        clock,
    }
}

fn slow_flow(delay_ms: u64, reenter: Option<&str>) -> String {
    node_flow(serde_json::json!({
        "delay_ms": delay_ms, "reenter_flow": reenter
    }))
}

/// A flow whose node stays until the test opens the probe's hold.
fn held_flow() -> String {
    node_flow(serde_json::json!({"hold": true}))
}

fn node_flow(config: serde_json::Value) -> String {
    graph_of("sla_slow", config)
}

fn graph_of(node_type: &str, config: serde_json::Value) -> String {
    serde_json::json!({
        "nodes": [
            {"id": "t", "type": "trigger", "config": {}},
            {"id": "s", "type": node_type, "config": config},
            {"id": "o", "type": "output", "config": {}}
        ],
        "edges": [
            {"from": "t", "to": "s", "from_port": "text", "to_port": "text"},
            {"from": "s", "to": "o", "from_port": "text", "to_port": "text"}
        ]
    })
    .to_string()
}

fn stream_flow() -> String {
    serde_json::json!({
        "nodes": [
            {"id": "t", "type": "trigger", "config": {}},
            {"id": "p", "type": "sla_stream", "config": {}},
            {"id": "o", "type": "output", "config": {"mode": "stream"}}
        ],
        "edges": [
            {"from": "t", "to": "p", "from_port": "text", "to_port": "in"},
            {"from": "p", "to": "o", "from_port": "stream", "to_port": "text"}
        ]
    })
    .to_string()
}

impl Rig {
    fn insert_flow(&self, id: &str, flow_json: &str) {
        let conn = self.dispatcher.db.write().expect("db lock");
        conn.execute(
            "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, ?1, ?2, 'active')",
            rusqlite::params![id, flow_json],
        )
        .expect("insert flow");
    }

    /// Stores a flow that clients reach by `model` (flow-as-model) as a
    /// `service_type` service, and republishes the catalog.
    fn publish_flow(&self, id: &str, model: &str, service_type: &str, flow_json: &str) {
        {
            let conn = self.dispatcher.db.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status, service_type, published_model_name) \
                 VALUES (?1, ?1, ?2, 'active', ?3, ?4)",
                rusqlite::params![id, flow_json, service_type, model],
            )
            .expect("insert published flow");
        }
        self.catalog
            .rebuild(
                &crate::services::mesh_registry::MeshServicesRegistry::new(),
                &self.dispatcher.db,
            )
            .expect("rebuild catalog");
    }

    /// The id of the audit row of the run `request_id`.
    fn execution_id(&self, request_id: &str) -> i64 {
        let conn = self.dispatcher.db.read().expect("db lock");
        conn.query_row(
            "SELECT id FROM flow_executions WHERE request_id = ?1 AND parent_execution_id IS NULL",
            rusqlite::params![request_id],
            |row| row.get(0),
        )
        .expect("the run has an audit row")
    }

    /// The status the audit row of the run `request_id` ended in.
    fn execution_status(&self, request_id: &str) -> Option<String> {
        let conn = self.dispatcher.db.read().expect("db lock");
        conn.query_row(
            "SELECT status FROM flow_executions WHERE request_id = ?1 AND parent_execution_id IS NULL",
            rusqlite::params![request_id],
            |row| row.get(0),
        )
        .ok()
    }

    /// Stores a policy and assigns it, then loads the registry from the rows
    /// the way the first dispatch would, so a test that fires requests at once
    /// does not race that first load.
    fn govern(&self, flow_id: &str, spec: SlaPolicySpec) -> Arc<ServiceGate> {
        let policy = repository::create_policy(
            &self.dispatcher.db,
            "org-a",
            "admin",
            &SlaPolicySpec {
                name: format!("policy-{flow_id}"),
                ..spec
            },
        )
        .expect("create policy");
        repository::assign(
            &self.dispatcher.db,
            "org-a",
            "admin",
            "flow",
            flow_id,
            &policy.id,
        )
        .expect("assign policy");
        self.sla
            .refresh_if_stale(&self.dispatcher.db)
            .expect("load policies");
        self.sla.gate(flow_id).expect("gate exists")
    }
}

fn limit(max_concurrency: u32, max_queue: u32, queue_timeout_ms: u32) -> SlaPolicySpec {
    SlaPolicySpec {
        max_concurrency: Some(max_concurrency),
        max_queue: Some(max_queue),
        queue_timeout_ms: Some(queue_timeout_ms),
        ..Default::default()
    }
}

fn api_meta(id: &str) -> FlowRequestMeta {
    FlowRequestMeta::new(id, FlowOrigin::Api, FlowActor::system())
}

fn input() -> FlowEnvelope {
    FlowEnvelope::with_payload(FlowValue::Text("go".into()))
}

async fn wait_for(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..300 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn only_the_stream(exec: StreamingExecution) -> BoxStream<'static, Result<EnvelopeDelta>> {
    // What the HTTP bridges do: keep the stream, let the rest of the execution
    // go out of scope with the function.
    let StreamingExecution { stream, .. } = exec;
    stream
}

fn stats(gate: &ServiceGate) -> GateStats {
    gate.stats()
}

/// Spawns `n` requests for `flow`, each as a top-level API call.
fn spawn_requests(
    rig: &Rig,
    flow: &str,
    n: usize,
) -> Vec<tokio::task::JoinHandle<std::result::Result<FlowExecutionOutcome, DispatchError>>> {
    (0..n)
        .map(|i| {
            let dispatcher = rig.dispatcher.clone();
            let flow = flow.to_string();
            tokio::spawn(async move {
                dispatcher
                    .dispatch_by_flow_id(flow, input(), api_meta(&format!("r{i}")))
                    .await
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_requests_against_a_limit_of_two_never_run_more_than_two_at_once() {
    let rig = rig();
    rig.insert_flow("slow", &slow_flow(150, None));
    let gate = rig.govern("slow", limit(2, 8, 5_000));

    for task in spawn_requests(&rig, "slow", 10) {
        task.await.unwrap().expect("a queued request still runs");
    }

    assert_eq!(rig.probe.peak.load(Ordering::SeqCst), 2);
    assert_eq!(rig.probe.total.load(Ordering::SeqCst), 10);
    let stats = stats(&gate);
    assert_eq!(
        (stats.accepted, stats.ok, stats.in_flight, stats.waiting),
        (10, 10, 0, 0)
    );
    assert_eq!(stats.rejected_concurrency, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_beyond_the_queue_are_refused_with_a_retry_after() {
    let rig = rig();
    rig.insert_flow("held", &held_flow());
    let gate = rig.govern("held", limit(2, 3, 30_000));
    rig.probe.close_hold();

    let tasks = spawn_requests(&rig, "held", 10);
    // 2 run, 3 wait, and the other 5 are refused: nothing moves on until
    // the hold opens, so the picture below is settled, not a race.
    wait_for("2 running, 3 queued, 5 refused", || {
        let s = stats(&gate);
        s.in_flight == 2 && s.waiting == 3 && s.rejected_concurrency == 5
    })
    .await;
    rig.probe.open_hold();

    let (mut ran, mut refused) = (0, 0);
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => ran += 1,
            Err(DispatchError::Overloaded {
                reason,
                retry_after_ms,
            }) => {
                assert_eq!(reason, RefusalReason::QueueFull);
                assert!(retry_after_ms > 0, "a refusal says when to come back");
                refused += 1;
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    assert_eq!((ran, refused), (5, 5));
    assert_eq!(rig.probe.peak.load(Ordering::SeqCst), 2);
    assert_eq!(stats(&gate).rejected_concurrency, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_request_is_refused_when_its_wait_runs_out() {
    let rig = rig();
    rig.insert_flow("held", &held_flow());
    let gate = rig.govern("held", limit(1, 1, 100));
    rig.probe.close_hold();

    let holder = spawn_requests(&rig, "held", 1).pop().unwrap();
    wait_for("the holder to take the slot", || {
        stats(&gate).in_flight == 1
    })
    .await;

    let refused = rig
        .dispatcher
        .dispatch_by_flow_id("held".into(), input(), api_meta("waiter"))
        .await;
    assert!(matches!(
        refused,
        Err(DispatchError::Overloaded {
            reason: RefusalReason::QueueTimeout,
            ..
        })
    ));
    rig.probe.open_hold();
    holder.await.unwrap().expect("the holder finishes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_call_limit_refuses_at_once_with_the_time_to_the_next_window() {
    let rig = rig();
    rig.insert_flow("quick", &slow_flow(0, None));
    // 24 h window; the clock is frozen 30 minutes into it.
    let gate = rig.govern(
        "quick",
        SlaPolicySpec {
            call_limit: Some(1),
            call_window_secs: Some(86_400),
            ..Default::default()
        },
    );

    rig.dispatcher
        .dispatch_by_flow_id("quick".into(), input(), api_meta("first"))
        .await
        .expect("the first call fits the window");
    let started = Instant::now();
    let refused = rig
        .dispatcher
        .dispatch_by_flow_id("quick".into(), input(), api_meta("second"))
        .await;

    match refused {
        Err(DispatchError::Overloaded {
            reason: RefusalReason::CallLimit,
            retry_after_ms,
        }) => assert_eq!(retry_after_ms, 86_400_000 - 1_800_000),
        other => panic!("expected a call-limit refusal, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(1), "no queueing");
    assert_eq!(stats(&gate).rejected_calls, 1);

    rig.clock.fetch_add(86_400_000, Ordering::SeqCst);
    rig.dispatcher
        .dispatch_by_flow_id("quick".into(), input(), api_meta("next window"))
        .await
        .expect("a new window has room");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_streaming_permit_is_held_until_the_stream_ends() {
    let rig = rig();
    rig.insert_flow("streamer", &stream_flow());
    let gate = rig.govern("streamer", limit(1, 0, 1_000));

    let feed = rig.streams.feed();
    let exec = rig
        .dispatcher
        .dispatch_by_flow_id_streaming("streamer".into(), input(), api_meta("s1"))
        .await
        .expect("the stream starts");
    // The bridges destructure the execution and drop the rest; the slot has
    // to survive that.
    let mut stream = only_the_stream(exec);
    assert_eq!(stats(&gate).in_flight, 1, "held while the stream is open");

    let refused = rig
        .dispatcher
        .dispatch_by_flow_id_streaming("streamer".into(), input(), api_meta("s2"))
        .await;
    assert!(matches!(
        refused,
        Err(DispatchError::Overloaded {
            reason: RefusalReason::QueueFull,
            ..
        })
    ));

    feed.send(Ok(LlmStreamChunk {
        text_delta: "hello".into(),
        ..Default::default()
    }))
    .await
    .unwrap();
    stream.next().await.expect("a chunk").expect("not an error");
    assert_eq!(stats(&gate).in_flight, 1, "still held mid-stream");

    drop(feed);
    while stream.next().await.is_some() {}
    let ended = stats(&gate);
    assert_eq!((ended.in_flight, ended.ok, ended.errors), (0, 1, 0));

    let next_feed = rig.streams.feed();
    drop(next_feed);
    let again = rig
        .dispatcher
        .dispatch_by_flow_id_streaming("streamer".into(), input(), api_meta("s3"))
        .await
        .expect("the slot is free again");
    drop(again);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_stream_cancels_the_run_and_releases_the_slot_as_cancelled() {
    let rig = rig();
    rig.insert_flow("streamer", &stream_flow());
    let gate = rig.govern("streamer", limit(1, 0, 1_000));

    let _feed = rig.streams.feed();
    let meta = api_meta("s1");
    let cancel = meta.cancel_token.clone();
    let exec = rig
        .dispatcher
        .dispatch_by_flow_id_streaming("streamer".into(), input(), meta)
        .await
        .expect("the stream starts");
    assert_eq!(stats(&gate).in_flight, 1);
    assert!(!cancel.is_cancelled());

    drop(exec);

    assert!(
        rig.streams.run_token().is_cancelled(),
        "the producer is told to stop before the slot is given away"
    );
    assert!(
        !cancel.is_cancelled(),
        "the caller's own token is not touched"
    );
    let released = stats(&gate);
    assert_eq!(
        (released.in_flight, released.cancelled, released.ok),
        (0, 1, 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_is_recorded_as_the_way_it_ended() {
    let rig = rig();
    rig.insert_flow("streamer", &stream_flow());
    let gate = rig.govern("streamer", limit(1, 0, 1_000));
    let chunk = |finish: Option<FinishReason>, error: Option<&str>| -> Feed {
        Ok(LlmStreamChunk {
            text_delta: "x".into(),
            finish_reason: finish,
            error: error.map(str::to_string),
            ..Default::default()
        })
    };
    // Starts a run, sends `items`, and either ends the feed or leaves it
    // open, then drains `read` items before dropping the stream.
    async fn run_once(rig: &Rig, id: &str, items: Vec<Feed>, end_feed: bool, cancel_first: bool) {
        let feed = rig.streams.feed();
        let meta = api_meta(id);
        let cancel = meta.cancel_token.clone();
        let exec = rig
            .dispatcher
            .dispatch_by_flow_id_streaming("streamer".into(), input(), meta)
            .await
            .expect("the stream starts");
        let mut stream = only_the_stream(exec);
        for item in items {
            feed.send(item).await.unwrap();
            let _ = stream.next().await;
        }
        if cancel_first {
            cancel.cancel();
        }
        if end_feed {
            drop(feed);
            while stream.next().await.is_some() {}
        }
        drop(stream);
    }

    // A chunk that reports the run was cancelled.
    run_once(
        &rig,
        "c1",
        vec![chunk(Some(FinishReason::Cancelled), None)],
        true,
        false,
    )
    .await;
    let s = stats(&gate);
    assert_eq!((s.cancelled, s.errors, s.ok, s.in_flight), (1, 0, 0, 0));

    // A run cancelled through its token, whose stream then simply ends.
    run_once(&rig, "c2", vec![chunk(None, None)], true, true).await;
    let s = stats(&gate);
    assert_eq!((s.cancelled, s.errors, s.ok), (2, 0, 0));

    // A trailer that carries an error.
    run_once(
        &rig,
        "e1",
        vec![chunk(Some(FinishReason::Error), Some("backend down"))],
        true,
        false,
    )
    .await;
    let s = stats(&gate);
    assert_eq!((s.cancelled, s.errors, s.ok), (2, 1, 0));

    // An error item, and the consumer goes away after it.
    run_once(&rig, "e2", vec![Err("boom")], false, false).await;
    let s = stats(&gate);
    assert_eq!((s.cancelled, s.errors, s.ok, s.in_flight), (2, 2, 0, 0));

    // And a stream that ends cleanly is still a success.
    run_once(
        &rig,
        "ok",
        vec![chunk(Some(FinishReason::Stop), None)],
        true,
        false,
    )
    .await;
    let s = stats(&gate);
    assert_eq!((s.cancelled, s.errors, s.ok), (2, 2, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_policy_timeout_cancels_the_run_and_settles_its_audit_row() {
    let rig = rig();
    // A node that does not look at the cancel token: it is dropped where it
    // stands once the grace period is over.
    rig.insert_flow("hangs", &slow_flow(60_000, None));
    let gate = rig.govern(
        "hangs",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(100),
            ..Default::default()
        },
    );

    let meta = api_meta("t1");
    let cancel = meta.cancel_token.clone();
    let started = Instant::now();
    let result = rig
        .dispatcher
        .dispatch_by_flow_id("hangs".into(), input(), meta)
        .await;

    match result {
        Err(DispatchError::TimedOut {
            flow_id,
            timeout_ms,
        }) => assert_eq!((flow_id.as_str(), timeout_ms), ("hangs", 100)),
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(
        rig.probe.run_token().is_cancelled(),
        "the run is told to stop"
    );
    assert!(
        !cancel.is_cancelled(),
        "the caller's own token is not cancelled by the policy"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(100) + crate::flow_engine::sla_admission::CANCEL_GRACE
            && elapsed < Duration::from_secs(10),
        "a run that ignores the token gets the grace period, then is dropped: {elapsed:?}"
    );
    assert!(
        !crate::flow_engine::sla_admission::repository_timed_out::contains(rig.execution_id("t1")),
        "the timed-out mark does not outlive the run"
    );
    assert_eq!(
        rig.execution_status("t1").as_deref(),
        Some("error"),
        "the audit row is final, and it is not a cancellation by the caller"
    );
    let stats = stats(&gate);
    assert_eq!((stats.timeouts, stats.errors, stats.in_flight), (1, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_that_stops_on_cancel_ends_inside_the_grace_period_as_a_timeout() {
    let rig = rig();
    rig.insert_flow(
        "listens",
        &node_flow(serde_json::json!({"delay_ms": 60_000, "stop_on_cancel": true})),
    );
    let gate = rig.govern(
        "listens",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(100),
            ..Default::default()
        },
    );

    let started = Instant::now();
    let result = rig
        .dispatcher
        .dispatch_by_flow_id("listens".into(), input(), api_meta("t2"))
        .await;

    assert!(matches!(result, Err(DispatchError::TimedOut { .. })));
    assert!(
        started.elapsed() < crate::flow_engine::sla_admission::CANCEL_GRACE,
        "it stopped on its own, well inside the grace period"
    );
    assert_eq!(rig.execution_status("t2").as_deref(), Some("error"));
    let stats = stats(&gate);
    assert_eq!(
        (stats.timeouts, stats.cancelled, stats.in_flight),
        (1, 0, 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_stricter_of_the_callers_deadline_and_the_policy_timeout_wins() {
    let rig = rig();
    rig.insert_flow("quick", &slow_flow(0, None));
    rig.govern(
        "quick",
        SlaPolicySpec {
            max_concurrency: Some(4),
            timeout_ms: Some(60_000),
            ..Default::default()
        },
    );

    let mut sooner = api_meta("sooner");
    let caller_deadline = Instant::now() + Duration::from_secs(5);
    sooner.deadline = Some(caller_deadline);
    let admission = rig
        .dispatcher
        .admit_run("quick", &mut sooner, AdmitOrigin::Request)
        .await
        .expect("admitted");
    assert_eq!(
        sooner.deadline,
        Some(caller_deadline),
        "the caller's is stricter"
    );
    admission.finish_ok();

    let mut later = api_meta("later");
    later.deadline = Some(Instant::now() + Duration::from_secs(3_600));
    let admission = rig
        .dispatcher
        .admit_run("quick", &mut later, AdmitOrigin::Request)
        .await
        .expect("admitted");
    let applied = later.deadline.expect("a deadline");
    assert!(
        applied <= Instant::now() + Duration::from_secs(60),
        "the policy's is stricter"
    );
    assert_eq!(Some(applied), admission.deadline());
    admission.finish_ok();

    let mut none = api_meta("none");
    let admission = rig
        .dispatcher
        .admit_run("quick", &mut none, AdmitOrigin::Request)
        .await
        .expect("admitted");
    assert_eq!(none.deadline, admission.deadline(), "the policy sets one");
    admission.finish_ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_callers_own_stricter_deadline_is_not_reported_as_the_policys_timeout() {
    let rig = rig();
    rig.insert_flow("slowish", &slow_flow(300, None));
    let gate = rig.govern(
        "slowish",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(60_000),
            ..Default::default()
        },
    );

    // The caller's deadline passes while the node is still running; the
    // executor notices it before the next node and ends the run with it. The
    // policy's deadline is a minute away, so this is not the policy's timeout:
    // the run returns its outcome with the error, as it did before the policy.
    let mut meta = api_meta("deadline");
    meta.deadline = Some(Instant::now() + Duration::from_millis(100));
    let outcome = rig
        .dispatcher
        .dispatch_by_flow_id("slowish".into(), input(), meta)
        .await
        .expect("an outcome, not a timeout");

    assert_eq!(outcome.error.as_deref(), Some("deadline exceeded"));
    let stats = stats(&gate);
    assert_eq!(
        (stats.timeouts, stats.errors, stats.in_flight),
        (0, 1, 0),
        "a failure of the run, not the policy's timeout"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_timeout_closes_only_its_own_attempts_row_when_attempts_share_a_request_id() {
    let rig = rig();
    rig.insert_flow("retried", &held_flow());
    rig.govern(
        "retried",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(300),
            ..Default::default()
        },
    );

    // Attempt 1 finishes (the hold is open). Attempt 2 has the same request
    // id, hangs and is timed out.
    rig.dispatcher
        .dispatch_by_flow_id("retried".into(), input(), api_meta("same"))
        .await
        .expect("the first attempt completes");
    rig.probe.close_hold();
    let second = rig
        .dispatcher
        .dispatch_by_flow_id("retried".into(), input(), api_meta("same"))
        .await;
    assert!(matches!(second, Err(DispatchError::TimedOut { .. })));

    let conn = rig.dispatcher.db.read().expect("db lock");
    let statuses: Vec<String> = conn
        .prepare("SELECT status FROM flow_executions WHERE request_id = 'same' ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        statuses,
        vec!["completed", "error"],
        "the earlier attempt's row is left as it was"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_background_run_that_times_out_does_not_cancel_the_token_its_owner_holds() {
    let rig = rig();
    rig.insert_flow(
        "agent-flow",
        &node_flow(serde_json::json!({"delay_ms": 60_000, "stop_on_cancel": true})),
    );
    let gate = rig.govern(
        "agent-flow",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(100),
            ..Default::default()
        },
    );

    // The agent run manager decides `Cancelled` from this very token, so a
    // timeout must leave it alone and the run must end as failed.
    let owner = FlowRequestMeta::new("agent", FlowOrigin::Agent, FlowActor::system());
    let owners_token = owner.cancel_token.clone();
    let result = rig
        .dispatcher
        .dispatch_by_flow_id_background(FlowRef::live("agent-flow"), input(), owner)
        .await;

    assert!(matches!(result, Err(DispatchError::TimedOut { .. })));
    assert!(!owners_token.is_cancelled());
    assert!(rig.probe.run_token().is_cancelled(), "the run was stopped");
    let stats = stats(&gate);
    assert_eq!(
        (stats.timeouts, stats.cancelled, stats.in_flight),
        (1, 0, 0)
    );

    // And the owner cancelling still reaches the run.
    let owner = FlowRequestMeta::new("agent2", FlowOrigin::Agent, FlowActor::system());
    let owners_token = owner.cancel_token.clone();
    let dispatcher = rig.dispatcher.clone();
    let run = tokio::spawn(async move {
        dispatcher
            .dispatch_by_flow_id_background(FlowRef::live("agent-flow"), input(), owner)
            .await
    });
    wait_for("the run to start", || {
        rig.probe.current.load(Ordering::SeqCst) == 1
    })
    .await;
    owners_token.cancel();
    let outcome = run.await.unwrap();
    assert!(
        !matches!(outcome, Err(DispatchError::TimedOut { .. })),
        "a cancellation by the owner is not the policy's timeout: {outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_that_outlives_the_policy_timeout_fails_and_cancels_the_run() {
    let rig = rig();
    rig.insert_flow("streamer", &stream_flow());
    let gate = rig.govern(
        "streamer",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(100),
            ..Default::default()
        },
    );

    let _silent = rig.streams.feed();
    let meta = api_meta("s1");
    let cancel = meta.cancel_token.clone();
    let exec = rig
        .dispatcher
        .dispatch_by_flow_id_streaming("streamer".into(), input(), meta)
        .await
        .expect("the stream starts");
    let mut stream = only_the_stream(exec);

    let first = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("the timeout ends the wait");
    match first {
        Some(Err(error)) => assert!(
            error.to_string().starts_with("sla.timeout"),
            "the stream error carries the stable code: {error}"
        ),
        other => panic!(
            "the timeout is an error item, got {:?}",
            other.map(|r| r.is_ok())
        ),
    }
    assert!(stream.next().await.is_none(), "and the stream is over");
    let row = rig.execution_id("s1");
    // The finalizer takes the mark and then writes the row, so wait for the row
    // first, then for the mark to be gone.
    wait_for("the row to be settled", || {
        rig.execution_status("s1").as_deref() == Some("error")
    })
    .await;
    wait_for("the timed-out mark to be released", || {
        !crate::flow_engine::sla_admission::repository_timed_out::contains(row)
    })
    .await;
    assert!(
        rig.streams.run_token().is_cancelled(),
        "the producer is told to stop"
    );
    assert!(
        !cancel.is_cancelled(),
        "the caller's own token is not touched"
    );
    let stats = stats(&gate);
    assert_eq!((stats.timeouts, stats.in_flight), (1, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_call_of_the_same_flow_does_not_take_a_second_slot() {
    let rig = rig();
    rig.insert_flow("outer", &slow_flow(10, Some("outer")));
    // No queue and a limit of one: a nested call that asked for a slot would be
    // refused at once, and one that queued would wait for its own caller.
    let gate = rig.govern("outer", limit(1, 0, 1_000));

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        rig.dispatcher
            .dispatch_by_flow_id("outer".into(), input(), api_meta("o1")),
    )
    .await
    .expect("no deadlock");

    outcome.expect("the outer run and its nested call both finish");
    assert_eq!(
        rig.probe.total.load(Ordering::SeqCst),
        2,
        "outer and nested"
    );
    let stats = stats(&gate);
    assert_eq!(
        (stats.accepted, stats.ok, stats.rejected_concurrency),
        (1, 1, 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_nesting_depth_is_not_what_decides_whether_a_call_is_nested() {
    let rig = rig();
    // The outer flow calls the inner one from inside its run, and the call
    // carries depth 0, as the chat and TTS branches of the executor make it.
    rig.insert_flow(
        "outer",
        &node_flow(serde_json::json!({"reenter_flow": "inner", "reenter_depth": 0})),
    );
    rig.insert_flow("inner", &slow_flow(0, None));
    let outer_gate = rig.govern("outer", limit(1, 0, 1_000));
    let inner_gate = rig.govern("inner", limit(1, 0, 1_000));

    rig.dispatcher
        .dispatch_by_flow_id("outer".into(), input(), api_meta("o1"))
        .await
        .expect("the nested call at depth 0 is not a top-level call");
    assert_eq!(stats(&inner_gate).accepted, 0, "it took no slot of its own");
    assert_eq!(stats(&outer_gate).accepted, 1);

    // And the other way round: a top-level call that is already one deep
    // (the embeddings, rerank and ingest branches seed it) is governed.
    let mut deep = api_meta("i1");
    deep.flow_depth = 1;
    rig.dispatcher
        .dispatch_by_flow_id("inner".into(), input(), deep)
        .await
        .expect("it runs");
    assert_eq!(
        stats(&inner_gate).accepted,
        1,
        "the depth-1 call took a slot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flow_published_as_its_own_model_runs_with_a_limit_of_one() {
    let rig = rig();
    // The model call goes the whole way: node, dispatcher, executor,
    // resolver, catalog, and back into the dispatcher for the same flow.
    rig.publish_flow(
        "self-flow",
        "self-model",
        "chat",
        &graph_of("sla_model_call", serde_json::json!({"model": "self-model"})),
    );
    let gate = rig.govern("self-flow", limit(1, 0, 1_000));

    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        rig.dispatcher
            .dispatch_by_flow_id("self-flow".into(), input(), api_meta("c1")),
    )
    .await
    .expect("a nested call must not wait for its own caller's slot")
    .expect("the run completes");

    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(
        rig.probe.total.load(Ordering::SeqCst),
        2,
        "outer and nested"
    );
    let stats = stats(&gate);
    assert_eq!(
        (stats.accepted, stats.ok, stats.rejected_concurrency),
        (1, 1, 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn top_level_runtime_calls_are_governed_and_nested_ones_are_not() {
    use crate::services::runtime::context::ExecutionContext as RuntimeContext;
    use crate::services::runtime::executor::ExecutorError;
    use crate::services::runtime::ResolvedExecutionTarget;

    let rig = rig();
    rig.publish_flow(
        "embed-flow",
        "embed-model",
        "embeddings",
        &slow_flow(0, None),
    );
    rig.publish_flow("ingest-flow", "ingest-model", "ingest", &slow_flow(0, None));
    let one_call = SlaPolicySpec {
        call_limit: Some(1),
        call_window_secs: Some(3_600),
        ..Default::default()
    };
    let embed_gate = rig.govern("embed-flow", one_call.clone());
    let ingest_gate = rig.govern("ingest-flow", one_call);
    let ctx = |nested: bool| {
        let mut ctx = RuntimeContext::new(None, FlowOrigin::Api, FlowActor::system());
        ctx.nested_in_run = nested;
        ctx
    };

    // Embeddings. The branch seeds the depth after entering the flow, so
    // the call arrives one deep, and it is still a top-level call.
    let target = ResolvedExecutionTarget::Flow {
        flow_id: "embed-flow".into(),
        published_name: "embed-model".into(),
    };
    let request = || crate::api::openai::types::EmbeddingRequest {
        model: "embed-model".into(),
        input: crate::api::openai::types::EmbeddingInput::Single("hello".into()),
        encoding_format: None,
        dimensions: None,
        user: None,
        extra: serde_json::Map::new(),
    };
    let first = rig
        .executor
        .dispatch_embeddings_blocking(&target, request(), &mut ctx(false))
        .await;
    assert!(!matches!(first, Err(ExecutorError::Sla(_))), "admitted");
    assert_eq!(stats(&embed_gate).accepted, 1);
    let second = rig
        .executor
        .dispatch_embeddings_blocking(&target, request(), &mut ctx(false))
        .await;
    assert!(
        matches!(
            second,
            Err(ExecutorError::Sla(DispatchError::Overloaded {
                reason: RefusalReason::CallLimit,
                ..
            }))
        ),
        "refused, and typed: {second:?}"
    );
    let nested = rig
        .executor
        .dispatch_embeddings_blocking(&target, request(), &mut ctx(true))
        .await;
    assert!(
        !matches!(nested, Err(ExecutorError::Sla(_))),
        "a nested call is not counted"
    );
    assert_eq!(stats(&embed_gate).accepted, 1);

    // Ingest, the same way.
    let ingest = |nested: bool| {
        let rig = &rig;
        async move {
            rig.executor
                .execute_ingest(
                    crate::services::runtime::executor::IngestRequest {
                        model: "ingest-model".into(),
                        document_bytes: b"doc".to_vec(),
                        mime: "text/plain".into(),
                        options: serde_json::Map::new(),
                        vector_home: None,
                        graph_home: None,
                        cancel_token: None,
                        flow_depth: 0,
                    },
                    &mut ctx(nested),
                )
                .await
        }
    };
    assert!(!matches!(ingest(false).await, Err(ExecutorError::Sla(_))));
    assert_eq!(stats(&ingest_gate).accepted, 1);
    assert!(matches!(
        ingest(false).await,
        Err(ExecutorError::Sla(DispatchError::Overloaded { .. }))
    ));
    assert!(!matches!(ingest(true).await, Err(ExecutorError::Sla(_))));
    assert_eq!(stats(&ingest_gate).accepted, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flow_without_a_policy_runs_as_before() {
    let rig = rig();
    rig.insert_flow("free", &held_flow());
    rig.insert_flow("governed", &slow_flow(0, None));
    // A policy on another flow must not touch this one.
    rig.govern("governed", limit(1, 0, 1_000));
    rig.probe.close_hold();

    let tasks = spawn_requests(&rig, "free", 6);
    wait_for("all six to be running at once", || {
        rig.probe.current.load(Ordering::SeqCst) == 6
    })
    .await;
    rig.probe.open_hold();
    for task in tasks {
        task.await.unwrap().expect("never refused");
    }

    assert_eq!(rig.probe.peak.load(Ordering::SeqCst), 6, "no limit applies");
    assert!(rig.sla.gate("free").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bus_run_is_never_refused_while_the_same_limit_refuses_others() {
    let rig = rig();
    rig.insert_flow("held", &held_flow());
    let gate = rig.govern("held", limit(1, 0, 1_000));
    rig.probe.close_hold();

    let holder = spawn_requests(&rig, "held", 1).pop().unwrap();
    wait_for("the holder to take the slot", || {
        stats(&gate).in_flight == 1
    })
    .await;

    let refused = rig
        .dispatcher
        .dispatch_by_flow_id("held".into(), input(), api_meta("api"))
        .await;
    assert!(matches!(refused, Err(DispatchError::Overloaded { .. })));
    let refused_background = rig
        .dispatcher
        .dispatch_by_flow_id_background(
            FlowRef::live("held"),
            input(),
            FlowRequestMeta::new("agent", FlowOrigin::Agent, FlowActor::system()),
        )
        .await;
    assert!(matches!(
        refused_background,
        Err(DispatchError::Overloaded { .. })
    ));

    let bus = {
        let dispatcher = rig.dispatcher.clone();
        tokio::spawn(async move {
            dispatcher
                .dispatch_by_flow_id_background(
                    FlowRef::live("held"),
                    input(),
                    FlowRequestMeta::new("bus", FlowOrigin::Bus, FlowActor::system()),
                )
                .await
        })
    };
    wait_for("the bus run to run beside the holder", || {
        rig.probe.current.load(Ordering::SeqCst) == 2
    })
    .await;
    assert_eq!(stats(&gate).in_flight, 1, "the bus run holds no slot");
    rig.probe.open_hold();

    bus.await
        .unwrap()
        .expect("a bus run is admitted while the slot is taken");
    holder.await.unwrap().expect("the holder finishes");
    let stats = stats(&gate);
    assert_eq!(
        (stats.accepted, stats.rejected_concurrency),
        (1, 2),
        "the bus run took no slot and was never counted as a refusal"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bus_run_gets_the_policy_timeout_as_a_backstop_and_is_not_counted_yet() {
    let rig = rig();
    rig.insert_flow(
        "hangs",
        &node_flow(serde_json::json!({
            "delay_ms": 60_000, "stop_on_cancel": true
        })),
    );
    let gate = rig.govern(
        "hangs",
        SlaPolicySpec {
            max_concurrency: Some(1),
            timeout_ms: Some(100),
            ..Default::default()
        },
    );

    let result = rig
        .dispatcher
        .dispatch_by_flow_id_background(
            FlowRef::live("hangs"),
            input(),
            FlowRequestMeta::new("bus", FlowOrigin::Bus, FlowActor::system()),
        )
        .await;

    assert!(matches!(result, Err(DispatchError::TimedOut { .. })));
    let stats = stats(&gate);
    // The gate's permit is the reactor's to take, in S4. Until then a bus
    // run is bounded by the timeout but is not counted in the gate.
    assert_eq!((stats.accepted, stats.timeouts, stats.in_flight), (0, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_published_as_a_streaming_flow_is_admitted_through_try_dispatch_streaming() {
    let rig = rig();
    rig.publish_flow("stream-flow", "stream-model", "chat", &stream_flow());
    let gate = rig.govern("stream-flow", limit(1, 0, 1_000));

    let feed = rig.streams.feed();
    let exec = rig
        .dispatcher
        .try_dispatch_streaming("stream-model", "chat", input(), api_meta("m1"))
        .await
        .expect("the stream starts");
    let mut stream = only_the_stream(exec);
    assert_eq!(stats(&gate).in_flight, 1);

    let refused = rig
        .dispatcher
        .try_dispatch_streaming("stream-model", "chat", input(), api_meta("m2"))
        .await;
    assert!(matches!(
        refused,
        Err(DispatchError::Overloaded {
            reason: RefusalReason::QueueFull,
            ..
        })
    ));

    drop(feed);
    while stream.next().await.is_some() {}
    let ended = stats(&gate);
    assert_eq!(
        (ended.in_flight, ended.ok, ended.rejected_concurrency),
        (0, 1, 1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocking_flow_wrapped_as_a_stream_is_admitted_for_the_whole_run() {
    let rig = rig();
    rig.publish_flow("block-flow", "block-model", "chat", &held_flow());
    let gate = rig.govern("block-flow", limit(1, 0, 1_000));
    rig.probe.close_hold();

    let first = {
        let dispatcher = rig.dispatcher.clone();
        tokio::spawn(async move {
            dispatcher
                .try_dispatch_streaming("block-model", "chat", input(), api_meta("b1"))
                .await
        })
    };
    wait_for("the blocking run to hold the slot", || {
        stats(&gate).in_flight == 1
    })
    .await;
    let refused = rig
        .dispatcher
        .try_dispatch_streaming("block-model", "chat", input(), api_meta("b2"))
        .await;
    assert!(matches!(refused, Err(DispatchError::Overloaded { .. })));

    rig.probe.open_hold();
    let exec = first
        .await
        .unwrap()
        .expect("the wrapped stream is returned");
    drop(exec);
    let ended = stats(&gate);
    assert_eq!((ended.in_flight, ended.ok, ended.accepted), (0, 1, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pinned_run_cannot_spend_an_admission_taken_for_another_flow() {
    let rig = rig();
    rig.insert_flow("other", &slow_flow(0, None));
    let gate = rig.govern(
        "other",
        SlaPolicySpec {
            max_concurrency: Some(1),
            call_limit: Some(1),
            call_window_secs: Some(3_600),
            ..Default::default()
        },
    );
    let mut meta = api_meta("p1");
    let admission = rig
        .dispatcher
        .admit_run("other", &mut meta, AdmitOrigin::Request)
        .await
        .expect("admitted for `other`");
    assert_eq!(stats(&gate).in_flight, 1);

    let snapshot = PinnedFlowSnapshot {
        flow_id: "pinned".into(),
        source_version: 1,
        graph_json: slow_flow(0, None),
        graph_sha256: String::new(),
    };
    let result = rig
        .dispatcher
        .dispatch_pinned_flow_admitted(&snapshot, input(), meta, admission)
        .await;

    assert!(
        matches!(result, Err(DispatchError::Internal(_))),
        "{result:?}"
    );
    let after = stats(&gate);
    assert_eq!(
        (after.in_flight, after.accepted, after.window_calls),
        (0, 0, 0),
        "the refused admission is given back"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pinned_run_refused_before_it_starts_gives_back_its_slot_and_its_call() {
    let rig = rig();
    rig.insert_flow("pinned", &slow_flow(0, None));
    let gate = rig.govern(
        "pinned",
        SlaPolicySpec {
            max_concurrency: Some(1),
            call_limit: Some(1),
            call_window_secs: Some(3_600),
            ..Default::default()
        },
    );
    let snapshot = PinnedFlowSnapshot {
        flow_id: "pinned".into(),
        source_version: 1,
        graph_json: slow_flow(0, None),
        graph_sha256: String::new(),
    };

    // No user on the meta: the pinned run is denied before it starts.
    let mut meta = api_meta("p1");
    let admission = rig
        .dispatcher
        .admit_pinned_flow(&snapshot, &mut meta)
        .await
        .expect("admitted");
    assert_eq!(stats(&gate).in_flight, 1);
    let denied = rig
        .dispatcher
        .dispatch_pinned_flow_admitted(&snapshot, input(), meta, admission)
        .await;
    assert!(
        matches!(denied, Err(DispatchError::Denied { .. })),
        "{denied:?}"
    );

    let after = stats(&gate);
    assert_eq!(
        (after.in_flight, after.accepted, after.window_calls),
        (0, 0, 0)
    );
    // The one call of the window is still there for a real run.
    let mut meta = api_meta("p2");
    rig.dispatcher
        .admit_pinned_flow(&snapshot, &mut meta)
        .await
        .expect("the window call was returned")
        .forgo();
}

#[test]
fn an_overload_carries_the_stable_code_in_its_text() {
    let error = DispatchError::Overloaded {
        reason: RefusalReason::QueueTimeout,
        retry_after_ms: 1_000,
    };
    assert_eq!(
        error.to_string(),
        "sla.overloaded: concurrency_queue_timeout, retry after 1000 ms"
    );
}

#[test]
fn a_typed_sla_error_from_the_executor_keeps_its_status_at_the_edges() {
    use crate::error::CoreError;
    use crate::services::runtime::executor::ExecutorError;

    let overloaded = ExecutorError::Sla(DispatchError::Overloaded {
        reason: RefusalReason::CallLimit,
        retry_after_ms: 2_500,
    });
    for core in [
        crate::routing::chat::executor_error_to_core(
            ExecutorError::Sla(DispatchError::Overloaded {
                reason: RefusalReason::CallLimit,
                retry_after_ms: 2_500,
            }),
            "m",
        ),
        crate::routing::embeddings::executor_err_to_core(overloaded, "m"),
    ] {
        assert_eq!(core.status_code(), 429);
        assert_eq!(core.retry_after_secs(), Some(3));
    }
    let timed_out = ExecutorError::Sla(DispatchError::TimedOut {
        flow_id: "f".into(),
        timeout_ms: 1_000,
    });
    assert!(matches!(
        crate::routing::embeddings::executor_err_to_core(timed_out, "m"),
        CoreError::Timeout { .. }
    ));
}
