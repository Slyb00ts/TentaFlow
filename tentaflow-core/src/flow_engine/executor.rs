// =============================================================================
// Plik: flow_engine/executor.rs
// Opis: Executor flow nowego stacku (plan v4.2). Dwa wejścia:
//       `execute_blocking` — pełny topo loop, wynikiem `FlowExecutionOutcome`;
//       `execute_streaming` — wykonuje pre-LLM nody, oddaje stream + outcome
//       receiver z aktywnym finalizerem (cancel/disconnect-resilient,
//       persist po execution_id).
// =============================================================================

use anyhow::{anyhow, Result};
use futures::stream::{BoxStream, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::db::{repository, DbPool};
use crate::flow_engine::cache::CompiledFlow;
use crate::flow_engine::envelope::{
    ChatMessage, EnvelopeDelta, FinishReason, FlowEnvelope, FlowExecutionOutcome, FlowValue,
    NodeInput, TokenUsage, TraceStatus, TraceStep,
};
use crate::flow_engine::io_mapping;
use crate::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const MAX_NODES_PER_EXECUTION: usize = 256;

/// §3.11 C — engine-level progress emission. Only NodeStarted/NodeFinished are
/// emitted here; iteration/map/tool/child/router variants belong to the phases
/// (5/6) that own those blocks. Mapping `TraceStatus` → wire label keeps the
/// UI free of the full trace step.
fn emit_node_started(ctx: &ExecutionContext, node_id: &str, node_type: &str) {
    ctx.progress.emit(
        &ctx.progress_scope,
        crate::flow_engine::dispatchers::ProgressEvent::NodeStarted {
            node_id: node_id.to_string(),
            node_type: node_type.to_string(),
        },
    );
}

fn emit_node_finished(ctx: &ExecutionContext, node_id: &str, status: &TraceStatus) {
    let label = match status {
        TraceStatus::Ok => "ok",
        TraceStatus::Error { .. } => "error",
        TraceStatus::Skipped => "skipped",
    };
    ctx.progress.emit(
        &ctx.progress_scope,
        crate::flow_engine::dispatchers::ProgressEvent::NodeFinished {
            node_id: node_id.to_string(),
            status: label.to_string(),
        },
    );
}

pub struct StreamingExecution {
    pub stream: BoxStream<'static, Result<EnvelopeDelta>>,
    pub outcome: oneshot::Receiver<FlowExecutionOutcome>,
}

/// Blocking execution. Dataflow scheduler: node startuje gdy wszystkie jego
/// poprzedniki dały output; gotowe nody lecą równolegle (`tokio::JoinSet`).
/// Fan-out z dowolnego noda wykonuje gałęzie współbieżnie, a node z N
/// wejściami (`combine`/`output`) jest naturalną barierą — scheduler odpala
/// go dopiero gdy wszystkie N gałęzi skończą. Flow liniowy degeneruje się do
/// jednego gotowego noda naraz (zachowanie jak topo loop). continue_on_error
/// z trigger.config kontroluje czy błąd przerywa flow.
pub async fn execute_blocking(
    db: DbPool,
    compiled: Arc<CompiledFlow>,
    initial: FlowEnvelope,
    mut ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
) -> Result<FlowExecutionOutcome> {
    let started = Instant::now();
    let initial_arc = Arc::new(initial);
    ctx.initial_envelope = initial_arc.clone();

    // SubflowRunner (§3.5) sets `parent_execution_id` so this child links back
    // to the run that spawned it; top-level runs leave it `None`. Light-mode
    // runs (loop iterations / map elements) skip the audit row entirely so a
    // 25-iteration loop never spams `flow_executions`.
    let execution_id =
        create_execution_record(&db, &compiled.flow_id, ctx.parent_execution_id, ctx.light).await?;
    ctx.execution_id = execution_id;

    let continue_on_error = compiled.continue_on_error();
    let n = compiled.execution_order.len();
    if n > MAX_NODES_PER_EXECUTION {
        return Err(anyhow!(
            "flow exceeds {} nodes ({})",
            MAX_NODES_PER_EXECUTION,
            n
        ));
    }

    let DependencyGraph {
        mut pending_deps,
        succ_nodes,
        out_edges,
    } = build_dependency_graph(&compiled, n);
    let ctx = Arc::new(ctx);
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    // `live_inputs[pos]` zlicza krawędzie wejściowe, które po rozwiązaniu
    // poprzednika okazały się aktywne (port producenta aktywny). Gdy wszyscy
    // poprzednicy są rozwiązani (`pending_deps==0`), node z ≥1 żywą krawędzią
    // wykonuje się normalnie, a node z zerem — jest Skipped i propaguje skip.
    let mut live_inputs: Vec<usize> = vec![0; n];
    let mut trace: Vec<TraceStep> = Vec::with_capacity(n);
    let mut error: Option<String> = None;
    let mut last_finish_reason: Option<FinishReason> = None;

    let mut join_set: JoinSet<NodeRun> = JoinSet::new();
    // Seed: wszystkie nody bez poprzedników (trigger) gotowe od razu.
    for pos in 0..n {
        if pending_deps[pos] == 0 {
            spawn_node(&mut join_set, &compiled, &adapters, &ctx, &outputs, pos)?;
        }
    }

    while let Some(joined) = join_set.join_next().await {
        let run = joined.map_err(|e| anyhow!("flow node task failed to join: {e}"))?;
        // Aktywne porty wyjściowe rozwiązanego node'a (None = wszystkie aktywne).
        // Liczone po Ok/continue_on_error; przy fatalnym błędzie pętla i tak
        // abortuje resztę, więc gałęzie nie startują.
        let mut active_ports: Option<HashSet<String>> = None;
        match run.result {
            Ok(envelope) => {
                trace.push(TraceStep {
                    node_id: run.node_id.clone(),
                    node_type: run.node_type.clone(),
                    started_at_ms: run.step_started_ms,
                    duration_ms: run.duration_ms,
                    status: TraceStatus::Ok,
                    usage: None,
                });
                emit_node_finished(&ctx, &run.node_id, &TraceStatus::Ok);
                active_ports = compute_active_ports(&compiled, adapters.as_ref(), run.pos, &envelope);
                outputs[run.pos] = Some(Arc::new(envelope));
            }
            Err(msg) => {
                let status = TraceStatus::Error {
                    message: msg.clone(),
                };
                emit_node_finished(&ctx, &run.node_id, &status);
                trace.push(TraceStep {
                    node_id: run.node_id.clone(),
                    node_type: run.node_type.clone(),
                    started_at_ms: run.step_started_ms,
                    duration_ms: run.duration_ms,
                    status,
                    usage: None,
                });
                if continue_on_error {
                    // Propaguj envelope sprzed błędu — następniki dostaną
                    // pierwsze dostępne wejście tego noda, fallback initial.
                    // Błędny node nie bramkuje (wszystkie porty aktywne), żeby
                    // continue_on_error zachowało dotychczasowe zachowanie.
                    let propagated = build_inputs(&compiled, run.pos, &outputs)
                        .into_iter()
                        .next()
                        .map(|i| i.envelope)
                        .unwrap_or_else(|| initial_arc.clone());
                    outputs[run.pos] = Some(propagated);
                } else {
                    error = Some(msg);
                    last_finish_reason = Some(FinishReason::Error);
                    abort_join_set(&mut join_set).await;
                    break;
                }
            }
        }

        // Cancel/deadline gate po każdym ukończonym node'ie — klient
        // disconnect / operator timeout abortuje resztę in-flight gałęzi.
        if ctx.cancel_token.is_cancelled() {
            error = Some("cancelled".into());
            last_finish_reason = Some(FinishReason::Cancelled);
            abort_join_set(&mut join_set).await;
            break;
        }
        if let Some(dl) = ctx.effective_deadline() {
            if Instant::now() >= dl {
                error = Some("deadline exceeded".into());
                last_finish_reason = Some(FinishReason::Error);
                abort_join_set(&mut join_set).await;
                break;
            }
        }

        // Rozwiązanie node'a propaguje się do następników. `to_resolve` zbiera
        // pozycje, których ostatni poprzednik właśnie się rozwiązał — node ze
        // wszystkimi nieaktywnymi krawędziami wejściowymi staje się Skipped i
        // jego skip propaguje dalej (BFS po następnikach, bez rekursji).
        let mut to_resolve: Vec<(usize, Option<HashSet<String>>)> =
            vec![(run.pos, active_ports.take())];
        while let Some((from_pos, ports)) = to_resolve.pop() {
            // Najpierw policz żywe krawędzie wychodzące z tego node'a. Node
            // Skipped (`ports == Some(empty via skip)`) — patrz niżej — nie ma
            // żywych portów; przekazujemy `Some(empty)` przez `skipped_marker`.
            for (to_pos, from_port) in &out_edges[from_pos] {
                let is_live = match &ports {
                    // None = wszystkie porty aktywne (default adaptera).
                    None => true,
                    Some(set) => set.contains(from_port),
                };
                if is_live {
                    live_inputs[*to_pos] += 1;
                }
            }
            // Następnie zdejmij zależność per DISTINCT poprzednik (combine z
            // dwiema krawędziami od tego samego node'a liczy 1).
            for &succ in &succ_nodes[from_pos] {
                pending_deps[succ] -= 1;
                if pending_deps[succ] == 0 {
                    if live_inputs[succ] > 0 {
                        spawn_node(&mut join_set, &compiled, &adapters, &ctx, &outputs, succ)?;
                    } else {
                        // Wszystkie krawędzie wejściowe nieaktywne → Skipped.
                        // Brak wykonania, brak usage; skip propaguje dalej
                        // (puste porty aktywne).
                        let def_idx = compiled.execution_order[succ];
                        let node = &compiled.definition.nodes[def_idx];
                        let now_ms = ctx.clock.now_ms();
                        // §3.11 C — a skipped node still surfaces as reached:
                        // started then finished with status `skipped`.
                        emit_node_started(&ctx, &node.id, &node.node_type);
                        emit_node_finished(&ctx, &node.id, &TraceStatus::Skipped);
                        trace.push(TraceStep {
                            node_id: node.id.clone(),
                            node_type: node.node_type.clone(),
                            started_at_ms: now_ms,
                            duration_ms: 0,
                            status: TraceStatus::Skipped,
                            usage: None,
                        });
                        to_resolve.push((succ, Some(HashSet::new())));
                    }
                }
            }
        }
    }

    // Usage attribution post-pass: per-node `usage_sink` drain raz, bucket po
    // node_id. Drain-per-node nie nadaje się przy współbieżności (drain zbiera
    // cały sink — wyścig o cudze wpisy).
    attribute_usage(&ctx, &mut trace);
    // Trace kończy się w kolejności ukończenia (out-of-order przy
    // równoległości) — sortujemy po starcie dla stabilnego widoku.
    trace.sort_by_key(|s| s.started_at_ms);

    let final_envelope = pick_final_envelope(&outputs, &initial_arc);
    let aggregate_usage = aggregate_usage(&trace);
    let total_latency_ms = started.elapsed().as_millis() as i64;
    let finish_reason = last_finish_reason.unwrap_or(if error.is_some() {
        FinishReason::Error
    } else {
        FinishReason::Stop
    });
    let outcome = FlowExecutionOutcome {
        final_envelope,
        trace,
        usage: aggregate_usage,
        finish_reason,
        total_latency_ms,
        error: error.clone(),
    };

    persist_execution(&db, execution_id, &outcome).await;
    Ok(outcome)
}

/// Wynik wykonania pojedynczego node'a w schedulerze dataflow — wraca z taska
/// `JoinSet` do pętli koordynującej.
struct NodeRun {
    pos: usize,
    node_id: String,
    node_type: String,
    step_started_ms: u64,
    duration_ms: u64,
    result: std::result::Result<FlowEnvelope, String>,
}

/// Graf zależności sterujący schedulerem dataflow + bramkowaniem gałęzi.
struct DependencyGraph {
    /// `pending_deps[pos]` = liczba odrębnych poprzedników node'a (in-degree
    /// po node'ach, nie krawędziach — combine z dwiema krawędziami od tego
    /// samego noda liczy 1). Maleje gdy poprzednik się rozwiąże (wykona lub
    /// zostanie Skipped).
    pending_deps: Vec<usize>,
    /// `succ_nodes[from]` = odrębne pozycje zależne od `from`; po jednym wpisie
    /// na poprzednika (steruje dekrementem `pending_deps`).
    succ_nodes: Vec<Vec<usize>>,
    /// `out_edges[from]` = WSZYSTKIE krawędzie wychodzące jako `(to_pos,
    /// from_port)`. Bez dedupu — liczność krawędzi (z portami) decyduje o
    /// aktywności wejść następnika (`condition` ma dwie krawędzie różnych
    /// portów do dwóch gałęzi; tylko aktywny port daje żywe wejście).
    out_edges: Vec<Vec<(usize, String)>>,
}

/// Buduje graf zależności z compiled flow. Toposort w compile gwarantuje brak
/// cykli, więc scheduler zawsze osusza JoinSet.
fn build_dependency_graph(compiled: &CompiledFlow, n: usize) -> DependencyGraph {
    // Jeden globalny HashSet par (from,pos) zamiast N HashSetów per node —
    // dedupy podwójnych krawędzi tej samej pary (rzadkie, np. dwie krawędzie do
    // jednego combine z tego samego noda) bez alokacji setu na każdy węzeł.
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut pending_deps = vec![0usize; n];
    let mut succ_nodes: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut out_edges: Vec<Vec<(usize, String)>> = vec![Vec::new(); n];
    for pos in 0..n {
        for &edge_idx in &compiled.incoming_edges_per_pos[pos] {
            let edge = &compiled.definition.edges[edge_idx];
            if let Some(&from_pos) = compiled.run_idx_by_id.get(edge.from.as_str()) {
                if from_pos == pos {
                    continue;
                }
                // Krawędź per port — sterowanie bramkowaniem.
                out_edges[from_pos].push((pos, edge.from_port.clone()));
                // Zależność per odrębny poprzednik — sterowanie barierą.
                if seen_pairs.insert((from_pos, pos)) {
                    pending_deps[pos] += 1;
                    succ_nodes[from_pos].push(pos);
                }
            }
        }
    }
    DependencyGraph {
        pending_deps,
        succ_nodes,
        out_edges,
    }
}

/// Pyta adapter rozwiązanego node'a, które porty wyjściowe są aktywne dla danego
/// wyniku (§3.11 A). `None` = wszystkie aktywne (default). Lookup adaptera nie
/// może zawieść tu (spawn już go znalazł), ale defensywnie zwracamy `None`.
fn compute_active_ports(
    compiled: &CompiledFlow,
    adapters: &AdapterRegistry,
    pos: usize,
    result: &FlowEnvelope,
) -> Option<HashSet<String>> {
    let def_idx = compiled.execution_order[pos];
    let node = &compiled.definition.nodes[def_idx];
    adapters
        .get(&node.node_type)
        .and_then(|a| a.active_output_ports(node, result))
}

/// Buduje inputs z ukończonych poprzedników i spawnuje adapter node'a jako task.
/// Wołane tylko gdy `pending_deps[pos]==0`, więc wszystkie poprzedniki mają już
/// `Some(output)`.
fn spawn_node(
    join_set: &mut JoinSet<NodeRun>,
    compiled: &Arc<CompiledFlow>,
    adapters: &Arc<AdapterRegistry>,
    ctx: &Arc<ExecutionContext>,
    outputs: &[Option<Arc<FlowEnvelope>>],
    pos: usize,
) -> Result<()> {
    let def_idx = compiled.execution_order[pos];
    // Borrow node tylko do lookupu adaptera — NIE klonujemy FlowNode (config to
    // potencjalnie duży serde_json::Value). Task dostaje `Arc<CompiledFlow>`
    // (refcount bump) i czyta node przez indeks.
    let node = &compiled.definition.nodes[def_idx];
    let adapter = adapters.get(&node.node_type).ok_or_else(|| {
        anyhow!(
            "no adapter for node '{}' (type '{}')",
            node.id,
            node.node_type
        )
    })?;
    let inputs = build_inputs(compiled, pos, outputs);
    // §3.11 C — NodeStarted emitted on the coordinator thread before the task
    // spawns, so events keep the scheduler's resolution order (NodeFinished is
    // emitted back in the join loop).
    emit_node_started(ctx, &node.id, &node.node_type);
    let compiled = compiled.clone();
    let ctx = ctx.clone();
    let initial = ctx.initial_envelope.clone();
    let step_started_ms = ctx.clock.now_ms();
    join_set.spawn(async move {
        let node = &compiled.definition.nodes[def_idx];
        let attempt = Instant::now();
        // io-mapping seam (§3.12): the inbound scope is the deterministically
        // chosen first input (single-input is the common shape; for fan-in
        // nodes like combine/output we pick the branch with the lowest
        // `from_node_id`, matching combine's own merge ordering so the same
        // flow reads the same branch across runs), falling back to the flow's
        // initial envelope for the trigger. The adapter sees a config with
        // input_mapping results overlaid and output_mapping writes its results
        // into the result's variables.
        let inbound: &FlowEnvelope =
            io_mapping_inbound(&inputs).unwrap_or_else(|| initial.as_ref());
        let result = run_node_with_io_mapping(adapter.as_ref(), node, inbound, &inputs, &ctx).await;
        let duration_ms = attempt.elapsed().as_millis() as u64;
        NodeRun {
            pos,
            node_id: node.id.clone(),
            node_type: node.node_type.clone(),
            step_started_ms,
            duration_ms,
            result: result.map_err(|e| e.to_string()),
        }
    });
    Ok(())
}

/// Generic io-mapping seam (§3.12) shared by the blocking and streaming paths.
/// Evaluates `input_mapping` against `inbound`, runs the adapter on a config
/// with the results overlaid, then evaluates `output_mapping` against the
/// result and writes the variables. Nodes without either mapping key take the
/// zero-cost fast path (no scope built, no node clone). io-mapping expression
/// failures become node errors (with node name + expression + cause).
async fn run_node_with_io_mapping(
    adapter: &dyn NodeAdapter,
    node: &FlowNode,
    inbound: &FlowEnvelope,
    inputs: &[NodeInput],
    ctx: &ExecutionContext,
) -> Result<FlowEnvelope> {
    if !io_mapping::has_io_mapping(node) {
        return adapter.execute(node, inputs, ctx).await;
    }

    let overlaid_config = io_mapping::apply_input_mapping(node, inbound).map_err(|e| anyhow!(e))?;
    // Clone the node only when a mapping is present; swap in the overlaid config
    // so the adapter (incl. addon.*) reads computed settings transparently.
    let mut mapped_node = node.clone();
    mapped_node.config = overlaid_config;

    let mut result = adapter.execute(&mapped_node, inputs, ctx).await?;
    io_mapping::apply_output_mapping(node, &mut result).map_err(|e| anyhow!(e))?;
    Ok(result)
}

/// Abortuje i osusza pozostałe taski po fatalnym błędzie/cancel/deadline, żeby
/// żaden in-flight adapter nie pisał do `usage_sink` po `attribute_usage`.
async fn abort_join_set(join_set: &mut JoinSet<NodeRun>) {
    join_set.abort_all();
    while join_set.join_next().await.is_some() {}
}

/// Rozdziela usage z `usage_sink` na TraceStep wg node_id. Drain raz na koniec
/// (po osuszeniu JoinSet) — bezpieczne przy współbieżności, w przeciwieństwie do
/// drain-per-node w trakcie.
fn attribute_usage(ctx: &ExecutionContext, trace: &mut [TraceStep]) {
    let drained = ctx.usage_sink.drain();
    if drained.is_empty() {
        return;
    }
    let mut by_node: HashMap<String, TokenUsage> = HashMap::new();
    for (id, u) in drained {
        by_node.entry(id).or_default().add(&u);
    }
    for step in trace.iter_mut() {
        if let Some(u) = by_node.get(&step.node_id) {
            if *u != TokenUsage::default() {
                step.usage = Some(u.clone());
            }
        }
    }
}

/// Streaming execution. Wykonuje pre-LLM nody w toposorcie, na node'ie LLM
/// (z `from_port="stream"` na edge'u out) buduje LlmRequest przez typed
/// accessor, dispatchuje stream_chat, spawnuje finalizer i zwraca
/// StreamingExecution natychmiast.
pub async fn execute_streaming(
    db: DbPool,
    compiled: Arc<CompiledFlow>,
    initial: FlowEnvelope,
    mut ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
) -> Result<StreamingExecution> {
    let started = Instant::now();
    let initial_arc = Arc::new(initial);
    ctx.initial_envelope = initial_arc.clone();

    let execution_id =
        create_execution_record(&db, &compiled.flow_id, ctx.parent_execution_id, ctx.light).await?;
    ctx.execution_id = execution_id;

    let producer_run_idx = compiled
        .stream_producer_run_idx(adapters.as_ref())
        .ok_or_else(|| anyhow!("execute_streaming called on non-streaming flow"))?;
    let producer_def_idx = compiled.execution_order[producer_run_idx];
    let producer_node = &compiled.definition.nodes[producer_def_idx];

    let n = compiled.execution_order.len();
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    let mut trace: Vec<TraceStep> = Vec::with_capacity(n);

    // Pre-producer topo loop. Cancel/deadline checked between nodes — same
    // contract as `execute_blocking`. The producer's streaming dispatch has
    // its own finalizer honouring these flags during the stream.
    for run_idx in 0..producer_run_idx {
        if ctx.cancel_token.is_cancelled() {
            return Err(anyhow!("cancelled"));
        }
        if let Some(dl) = ctx.effective_deadline() {
            if Instant::now() >= dl {
                return Err(anyhow!("deadline exceeded"));
            }
        }
        let def_idx = compiled.execution_order[run_idx];
        let node = &compiled.definition.nodes[def_idx];
        let inputs = build_inputs(&compiled, run_idx, &outputs);
        let adapter = adapters.get(&node.node_type).ok_or_else(|| {
            anyhow!(
                "no adapter for node '{}' (type '{}')",
                node.id,
                node.node_type
            )
        })?;
        let step_started = ctx.clock.now_ms();
        let attempt_started = Instant::now();
        emit_node_started(&ctx, &node.id, &node.node_type);
        // Same io-mapping seam as the blocking path: input_mapping overlay
        // before execute, output_mapping write after. Variables computed by a
        // pre-producer node are visible to the streaming producer downstream.
        // Inbound scope chosen deterministically (see `io_mapping_inbound`).
        let inbound: &FlowEnvelope =
            io_mapping_inbound(&inputs).unwrap_or_else(|| ctx.initial_envelope.as_ref());
        let envelope = run_node_with_io_mapping(adapter.as_ref(), node, inbound, &inputs, &ctx)
            .await
            .map_err(|e| {
                emit_node_finished(
                    &ctx,
                    &node.id,
                    &TraceStatus::Error {
                        message: e.to_string(),
                    },
                );
                anyhow!("pre-producer node '{}' failed: {e}", node.id)
            })?;
        emit_node_finished(&ctx, &node.id, &TraceStatus::Ok);
        let duration_ms = attempt_started.elapsed().as_millis() as u64;
        let usage = take_node_usage(&ctx, &node.id);
        trace.push(TraceStep {
            node_id: node.id.clone(),
            node_type: node.node_type.clone(),
            started_at_ms: step_started,
            duration_ms,
            status: TraceStatus::Ok,
            usage,
        });
        outputs[run_idx] = Some(Arc::new(envelope));
    }

    // §3.11 B — streaming dispatch via the generalized stream producer slot
    // (LLM is one such producer). The producer builds the EnvelopeDelta stream;
    // the executor no longer assumes the LLM-only path. The producer config is
    // passed raw (no io-mapping overlay): R7 rejects io-mapping on a stream
    // producer at validation precisely because this path cannot apply it, so
    // blocking and streaming dispatch never diverge on the same saved flow.
    let producer_inputs = build_inputs(&compiled, producer_run_idx, &outputs);
    let producer = adapters
        .stream_producer(&producer_node.node_type)
        .ok_or_else(|| {
            anyhow!(
                "no StreamProducerAdapter for node '{}' (type '{}')",
                producer_node.id,
                producer_node.node_type
            )
        })?;
    let producer_step_started = ctx.clock.now_ms();
    emit_node_started(&ctx, &producer_node.id, &producer_node.node_type);
    let mut envelope_stream: BoxStream<'static, Result<EnvelopeDelta>> = producer
        .produce_stream(producer_node, &producer_inputs, &ctx)
        .await
        .map_err(|e| {
            emit_node_finished(
                &ctx,
                &producer_node.id,
                &TraceStatus::Error {
                    message: e.to_string(),
                },
            );
            anyhow!("stream producer '{}' failed: {e}", producer_node.id)
        })?;

    // Stage 3d Krok 2c-2: fold streaming chain (intermediate streaming-aware
    // nodes po producencie, np. pii_filter / tts_stream_bridge). Każdy node
    // konsumuje upstream EnvelopeDelta i produkuje downstream — mogą zmienić
    // kind (LLM → Audio przez tts_stream_bridge).
    let producer_input_envelope = producer_inputs
        .first()
        .map(|i| i.envelope.clone())
        .unwrap_or_else(|| initial_arc.clone());
    let chain_run_idxs = compiled.streaming_chain_run_idxs(adapters.as_ref());
    for chain_run_idx in chain_run_idxs {
        let chain_def_idx = compiled.execution_order[chain_run_idx];
        let chain_node = &compiled.definition.nodes[chain_def_idx];
        let streaming = adapters
            .streaming_adapter(&chain_node.node_type)
            .ok_or_else(|| {
                anyhow!(
                    "streaming chain node '{}' (type '{}') missing StreamingNodeAdapter — \
                     compile-time R7 should have rejected this",
                    chain_node.id,
                    chain_node.node_type
                )
            })?;
        envelope_stream = streaming
            .process_stream(
                chain_node,
                envelope_stream,
                producer_input_envelope.clone(),
                &ctx,
            )
            .await
            .map_err(|e| anyhow!("chain node '{}' process_stream failed: {e}", chain_node.id))?;
    }

    let cancel = ctx.cancel_token.clone();
    let (outbound_tx, outbound_rx) = mpsc::channel::<Result<EnvelopeDelta>>(64);
    let (outcome_tx, outcome_rx) = oneshot::channel::<FlowExecutionOutcome>();

    let producer_node_id = producer_node.id.clone();
    let producer_node_type = producer_node.node_type.clone();
    let db_for_task = db.clone();
    // §3.11 C — the streaming producer's NodeFinished is emitted by the
    // finalizer once the stream settles (ok/cancel/error), carrying the
    // producer identity captured here so the spawned task stays self-contained.
    let progress = ctx.progress.clone();
    let progress_scope = ctx.progress_scope.clone();

    tokio::spawn(finalize_streaming_flow(
        execution_id,
        envelope_stream,
        outbound_tx,
        outcome_tx,
        cancel,
        FinalizerInputs {
            started,
            producer_step_started,
            producer_node_id,
            producer_node_type,
            producer_input_envelope,
            trace,
            db: db_for_task,
            progress,
            progress_scope,
        },
    ));

    let stream = futures::stream::unfold(outbound_rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let stream: BoxStream<'static, Result<EnvelopeDelta>> = Box::pin(stream);
    Ok(StreamingExecution {
        stream,
        outcome: outcome_rx,
    })
}

struct FinalizerInputs {
    started: Instant,
    producer_step_started: u64,
    producer_node_id: String,
    producer_node_type: String,
    producer_input_envelope: Arc<FlowEnvelope>,
    trace: Vec<TraceStep>,
    db: DbPool,
    /// §3.11 C — NodeFinished for the streaming producer is emitted here once
    /// the stream settles (the producer's start was emitted by the executor).
    progress: Arc<dyn crate::flow_engine::dispatchers::ProgressSink>,
    progress_scope: String,
}

async fn finalize_streaming_flow(
    execution_id: i64,
    mut envelope_stream: BoxStream<'static, Result<EnvelopeDelta>>,
    outbound_tx: mpsc::Sender<Result<EnvelopeDelta>>,
    outcome_tx: oneshot::Sender<FlowExecutionOutcome>,
    cancel: CancellationToken,
    mut inputs: FinalizerInputs,
) {
    let mut error: Option<String> = None;
    let mut cancelled = false;
    let mut text_buf = String::new();
    let mut reasoning_buf = String::new();
    let mut last_finish: Option<FinishReason> = None;
    let mut last_usage: Option<TokenUsage> = None;
    // Stage 3d Krok 2c-2: audio path agregator. Audio chunki z chain
    // (np. tts_stream_bridge) — outcome.payload to Empty (klient
    // skonsumował bytes przez SSE), ale finish_reason agregowany
    // dla wire trailers.
    let mut last_audio_finish: Option<FinishReason> = None;
    let mut audio_chunks_emitted: usize = 0;
    let producer_attempt_started = Instant::now();

    'main: loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break 'main;
            }
            delta = envelope_stream.next() => match delta {
                Some(Ok(EnvelopeDelta::Llm(c))) => {
                    if !c.text_delta.is_empty() {
                        text_buf.push_str(&c.text_delta);
                    }
                    if let Some(r) = &c.reasoning_delta {
                        reasoning_buf.push_str(r);
                    }
                    if let Some(fr) = c.finish_reason {
                        last_finish = Some(fr);
                    }
                    if let Some(u) = c.usage.as_ref() {
                        last_usage = Some(*u);
                    }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            cancelled = true;
                            break 'main;
                        }
                        send_res = outbound_tx.send(Ok(EnvelopeDelta::Llm(c))) => {
                            let _ = send_res;
                        }
                    }
                }
                Some(Ok(EnvelopeDelta::Audio(a))) => {
                    audio_chunks_emitted += 1;
                    if let Some(fr) = a.finish_reason {
                        last_audio_finish = Some(fr);
                    }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            cancelled = true;
                            break 'main;
                        }
                        send_res = outbound_tx.send(Ok(EnvelopeDelta::Audio(a))) => {
                            let _ = send_res;
                        }
                    }
                }
                Some(Err(e)) => {
                    error = Some(format!("{e}"));
                    break 'main;
                }
                None => break 'main,
            }
        }
    }
    drop(outbound_tx);

    // Stage 3d Krok 2c-2 fix: outcome shape zależny od końcowego kind'u
    // strumienia. Audio path (chain skończony przez tts_stream_bridge)
    // emit'uje bytes przez SSE — outcome.payload = Empty (bytes już
    // skonsumowane przez klienta, nie ma sensu duplikować w outcome).
    // LLM path agreguje text/reasoning do final_envelope.
    let is_audio_path = audio_chunks_emitted > 0;
    let mut final_envelope: FlowEnvelope = (*inputs.producer_input_envelope).clone();
    if is_audio_path {
        final_envelope.payload = FlowValue::Empty;
        // Audio path nie dopisuje assistant message — głos czytał
        // odpowiedź LLM, ale w envelope kontekstowym text już szedł
        // przez chain (tts_stream_bridge konsumował) lub został
        // pochłonięty wewnątrz bridge. Brak text_buf do append'u.
    } else {
        final_envelope.payload = FlowValue::Text(text_buf.clone());
        final_envelope
            .context
            .messages
            .push(ChatMessage::assistant(text_buf));
    }

    let producer_duration_ms = producer_attempt_started.elapsed().as_millis() as u64;
    let producer_usage = last_usage.unwrap_or_default();
    // The producer always ran (its stream was being consumed), so a cancel is
    // an interrupted execution, not a gate-out. §3.11 A reserves `Skipped` for
    // nodes that never executed because a branch was gated off; using it here
    // would mislabel a producer that did work. Cancel/error both map to Error.
    let producer_status = if cancelled {
        TraceStatus::Error {
            message: "cancelled".into(),
        }
    } else if let Some(e) = error.clone() {
        TraceStatus::Error { message: e }
    } else {
        TraceStatus::Ok
    };
    // §3.11 C — producer NodeFinished once the stream settles.
    {
        let label = match &producer_status {
            TraceStatus::Ok => "ok",
            TraceStatus::Error { .. } => "error",
            TraceStatus::Skipped => "skipped",
        };
        inputs.progress.emit(
            &inputs.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::NodeFinished {
                node_id: inputs.producer_node_id.clone(),
                status: label.to_string(),
            },
        );
    }
    inputs.trace.push(TraceStep {
        node_id: inputs.producer_node_id.clone(),
        node_type: inputs.producer_node_type.clone(),
        started_at_ms: inputs.producer_step_started,
        duration_ms: producer_duration_ms,
        status: producer_status,
        usage: if producer_usage == TokenUsage::default() {
            None
        } else {
            Some(producer_usage)
        },
    });

    let aggregate_usage = aggregate_usage(&inputs.trace);
    let total_latency_ms = inputs.started.elapsed().as_millis() as i64;
    // finish_reason priority: cancel/error > audio_finish (chain
    // terminal) > llm_finish > Stop default.
    let finish_reason = if cancelled {
        FinishReason::Cancelled
    } else if error.is_some() {
        FinishReason::Error
    } else if is_audio_path {
        last_audio_finish.unwrap_or(FinishReason::Stop)
    } else {
        last_finish.unwrap_or(FinishReason::Stop)
    };

    let outcome = FlowExecutionOutcome {
        final_envelope,
        trace: inputs.trace,
        usage: aggregate_usage,
        finish_reason,
        total_latency_ms,
        error: error.clone().or(if cancelled {
            Some("cancelled".into())
        } else {
            None
        }),
    };

    persist_execution(&inputs.db, execution_id, &outcome).await;
    let _ = outcome_tx.send(outcome);
}

/// Deterministic inbound envelope for the io-mapping scope (§3.12). For a
/// single-input node it is that input; for a fan-in node (combine/output) it is
/// the input with the lowest `from_node_id`, mirroring combine's own merge
/// ordering (`combine.rs` sorts by `from_node_id`) so the same flow always
/// evaluates io-mapping against the same branch. Returns `None` for the trigger
/// (no inputs), where the caller falls back to the flow's initial envelope.
fn io_mapping_inbound(inputs: &[NodeInput]) -> Option<&FlowEnvelope> {
    inputs
        .iter()
        .min_by(|a, b| a.from_node_id.cmp(&b.from_node_id))
        .map(|i| i.envelope.as_ref())
}

fn build_inputs(
    compiled: &CompiledFlow,
    run_idx: usize,
    outputs: &[Option<Arc<FlowEnvelope>>],
) -> Vec<NodeInput> {
    let edges = &compiled.incoming_edges_per_pos[run_idx];
    edges
        .iter()
        .filter_map(|&edge_idx| {
            let edge = &compiled.definition.edges[edge_idx];
            let from_pos = compiled.run_idx_by_id.get(edge.from.as_str()).copied()?;
            let envelope = outputs.get(from_pos)?.clone()?;
            Some(NodeInput {
                from_node_id: edge.from.clone(),
                from_port: edge.from_port.clone(),
                envelope,
            })
        })
        .collect()
}

fn pick_final_envelope(
    outputs: &[Option<Arc<FlowEnvelope>>],
    initial: &Arc<FlowEnvelope>,
) -> FlowEnvelope {
    for slot in outputs.iter().rev() {
        if let Some(env) = slot {
            return (**env).clone();
        }
    }
    (**initial).clone()
}

fn take_node_usage(ctx: &ExecutionContext, node_id: &str) -> Option<TokenUsage> {
    let drained = ctx.usage_sink.drain();
    let mut total = TokenUsage::default();
    let mut found = false;
    for (id, u) in drained {
        if id == node_id {
            total.add(&u);
            found = true;
        } else {
            // Re-rejestrujemy (niezgodność node_id zostawiamy następnemu
            // krokowi) — defensywnie, w praktyce drain idzie zaraz po
            // execute() więc 1 wpis w typowym przypadku.
            ctx.usage_sink.record(id, u);
        }
    }
    if found {
        Some(total)
    } else {
        None
    }
}

fn aggregate_usage(trace: &[TraceStep]) -> TokenUsage {
    let mut total = TokenUsage::default();
    for step in trace {
        if let Some(u) = step.usage.as_ref() {
            total.add(u);
        }
    }
    total
}

/// Returns the freshly created `flow_executions` id, or `0` as a sentinel for
/// runs that intentionally skip the audit row:
///   - synthetic flows (Universal Flow Gateway, `flow_id` empty) are ephemeral
///     and not present in `flows`, so an FK-bound insert would fail;
///   - light-mode runs (loop iterations / map elements, §3.5 blocks 1/2) carry
///     a real `flow_id` but must NOT insert a row per iteration/element — their
///     accounting lives in the agent run log and the parent's `TraceStep`.
/// `persist_execution` honours the same `0` sentinel and skips the update.
async fn create_execution_record(
    db: &DbPool,
    flow_id: &str,
    parent_execution_id: Option<i64>,
    light: bool,
) -> Result<i64> {
    if flow_id.is_empty() || light {
        return Ok(0);
    }
    let pool = db.clone();
    let flow_id = flow_id.to_string();
    let id = tokio::task::spawn_blocking(move || {
        repository::create_flow_execution(&pool, &flow_id, None, None, "running", parent_execution_id)
    })
    .await??;
    Ok(id)
}

async fn persist_execution(db: &DbPool, execution_id: i64, outcome: &FlowExecutionOutcome) {
    // execution_id == 0 = no audit row was created (synthetic or light run —
    // see create_execution_record). Nothing to update.
    if execution_id == 0 {
        return;
    }
    let pool = db.clone();
    let status = if outcome.finish_reason == FinishReason::Cancelled {
        "cancelled"
    } else if outcome.error.is_some() {
        "error"
    } else {
        "completed"
    };
    let log_json = serde_json::to_string(&outcome.trace).unwrap_or_else(|_| "[]".into());
    let total_ms = outcome.total_latency_ms;
    let total_tokens = outcome.usage.total_tokens as i64;
    let _ = tokio::task::spawn_blocking(move || {
        repository::update_flow_execution(
            &pool,
            execution_id,
            status,
            Some(&log_json),
            Some(total_ms),
            Some(total_tokens),
        )
    })
    .await;
}

#[cfg(test)]
mod chain_integration_tests {
    //! Krok 8 items 33/34: end-to-end execute_streaming chain integration.
    //!
    //! Test 33: trigger → llm → pii_filter → output(stream) — fake LLM emit
    //! 2 stream chunki, output stream zawiera EnvelopeDelta::Llm po
    //! przejściu przez pii_filter (z empty rules pii_filter jest identity,
    //! ale chain'owe pipe'owanie weryfikujemy).
    //!
    //! Test 34: trigger → llm → pii_filter → tts_stream_bridge → output(stream)
    //! — fake LLM emit zdanie, tts bridge syntetyzuje audio per zdanie,
    //! output stream zawiera EnvelopeDelta::Audio z bytes z BlobStore.
    //!
    //! flow_id=0 → executor pomija create_execution_record + persist_execution
    //! (synthetic-style audit skip), więc nie potrzebujemy realnej tabeli flows.
    use super::*;
    use crate::flow_engine::blob_store::{BlobRef, BlobStore, InMemoryBlobStore};
    use crate::flow_engine::dispatchers::{
        LlmDispatcher, LlmRequest, TtsDispatcher, TtsRequest, TtsResponse,
    };
    use crate::flow_engine::envelope::{
        AudioStreamChunk, EnvelopeDelta, FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk,
        TokenUsage,
    };
    use crate::flow_engine::node_adapter::{test_support::stub_ctx, AdapterRegistry};
    use crate::flow_engine::node_adapters::{
        LlmNodeAdapter, OutputNodeAdapter, PiiFilterNodeAdapter, TriggerNodeAdapter, TtsNodeAdapter,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use std::path::Path;
    use std::sync::Mutex;

    /// Fake LLM dispatcher emitujący predefiniowaną sekwencję chunków.
    /// `execute_chat` panikuje — chain testy używają wyłącznie streaming path.
    struct FakeStreamingLlm {
        chunks: Mutex<Option<Vec<LlmStreamChunk>>>,
    }

    impl FakeStreamingLlm {
        fn new(chunks: Vec<LlmStreamChunk>) -> Self {
            Self {
                chunks: Mutex::new(Some(chunks)),
            }
        }
    }

    #[async_trait]
    impl LlmDispatcher for FakeStreamingLlm {
        async fn execute_chat(
            &self,
            _req: LlmRequest,
        ) -> Result<crate::flow_engine::dispatchers::LlmResponse> {
            panic!("FakeStreamingLlm::execute_chat not used in chain tests");
        }
        async fn stream_chat(
            &self,
            _req: LlmRequest,
        ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
            let chunks = self
                .chunks
                .lock()
                .unwrap()
                .take()
                .expect("FakeStreamingLlm::stream_chat called twice");
            Ok(futures::stream::iter(chunks.into_iter().map(Ok)).boxed())
        }
    }

    /// Fake TTS dispatcher liczący wywołania synthesize i zwracający blob_ref
    /// wskazujący na wstępnie wgrane bajty w `ctx.blobs`.
    struct FakeTts {
        blob_ref: BlobRef,
        bytes: Vec<u8>,
        synthesized: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TtsDispatcher for FakeTts {
        async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse> {
            self.synthesized.lock().unwrap().push(req.text);
            Ok(TtsResponse {
                audio: self.blob_ref.clone(),
                mime: "audio/wav".into(),
                sample_rate: Some(22_050),
            })
        }
        async fn stream_synthesize(
            &self,
            _req: TtsRequest,
        ) -> Result<BoxStream<'static, Result<crate::flow_engine::dispatchers::TtsStreamChunk>>>
        {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn registry_with_chain() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
        r.register_streaming(Arc::new(TtsNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));
        r
    }

    fn fresh_db() -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        // `execute_streaming` writes a `flow_executions` row FK-bound to `flows(id)`;
        // seed the flow these tests compile under id "0" so the log write succeeds.
        {
            let conn = pool.lock().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES ('0', 'test', '{}', 'active')",
                [],
            )
            .expect("seed flow");
        }
        pool
    }

    /// Krok 8 item 33: chain LLM → pii_filter → output(stream).
    ///
    /// FakeLlm emit 2 chunki: "Hello world." + "Done.". Sentence flush w
    /// pii_filter wypycha cleaned tekst (z empty PII rules = identity).
    /// Verify że stream wyjściowy zawiera EnvelopeDelta::Llm chunki z
    /// nie-pustym text_delta i sumarycznie cały tekst.
    #[tokio::test]
    async fn streaming_chain_llm_pii_output() {
        let registry = Arc::new(registry_with_chain());
        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"l1","type":"llm","config":{"model":"qwen3.5-0.8b"}},
                {"id":"p1","type":"pii_filter","config":{}},
                {"id":"o1","type":"output","config":{"mode":"stream"}}
            ],
            "edges":[
                {"from":"t1","to":"l1","from_port":"text"},
                {"from":"l1","to":"p1","from_port":"stream"},
                {"from":"p1","to":"o1","from_port":"stream","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        let llm_chunks = vec![
            LlmStreamChunk {
                choice_index: 0,
                text_delta: "Hello world.".into(),
                ..Default::default()
            },
            LlmStreamChunk {
                choice_index: 0,
                text_delta: " Done.".into(),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            },
        ];
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(FakeStreamingLlm::new(llm_chunks));

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let exec = execute_streaming(fresh_db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");

        let mut deltas: Vec<EnvelopeDelta> = Vec::new();
        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            deltas.push(item.expect("delta ok"));
        }

        assert!(!deltas.is_empty(), "chain output stream empty");
        let mut concat = String::new();
        let mut saw_finish = false;
        for d in &deltas {
            let EnvelopeDelta::Llm(c) = d else {
                panic!("expected Llm delta, got Audio");
            };
            concat.push_str(&c.text_delta);
            if c.finish_reason == Some(FinishReason::Stop) {
                saw_finish = true;
            }
        }
        assert!(
            concat.contains("Hello world.") && concat.contains("Done."),
            "chain wycisnął tekst niepełny: {concat:?}"
        );
        assert!(saw_finish, "klient nie dostał finish_reason=Stop");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// Krok 8 item 34: chain LLM → pii_filter → tts(stream) → output(stream).
    /// LLM emit 1 zdanie kończące się kropką → pii_filter flush → tts node w
    /// trybie streaming syntetyzuje audio → output stream zawiera
    /// EnvelopeDelta::Audio z prawdziwymi bajtami z BlobStore.
    #[tokio::test]
    async fn streaming_chain_llm_pii_tts_audio_output() {
        let registry = Arc::new(registry_with_chain());
        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"l1","type":"llm","config":{"model":"qwen3.5-0.8b"}},
                {"id":"p1","type":"pii_filter","config":{}},
                {"id":"b1","type":"tts","config":{"model":"voxcpm"}},
                {"id":"o1","type":"output","config":{"mode":"stream"}}
            ],
            "edges":[
                {"from":"t1","to":"l1","from_port":"text"},
                {"from":"l1","to":"p1","from_port":"stream"},
                {"from":"p1","to":"b1","from_port":"stream"},
                {"from":"b1","to":"o1","from_port":"stream","to_port":"audio"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        let audio_bytes = vec![0xAA, 0xBB, 0xCC];
        let blobs = Arc::new(InMemoryBlobStore::new());
        let blob_ref = blobs
            .put(audio_bytes.clone(), "audio/wav")
            .await
            .expect("put audio");

        let llm_chunks = vec![LlmStreamChunk {
            choice_index: 0,
            text_delta: "Hello world.".into(),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }];

        let fake_tts = Arc::new(FakeTts {
            blob_ref: blob_ref.clone(),
            bytes: audio_bytes.clone(),
            synthesized: Mutex::new(Vec::new()),
        });

        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(FakeStreamingLlm::new(llm_chunks));
        ctx.tts = fake_tts.clone();
        ctx.blobs = blobs.clone() as Arc<dyn BlobStore>;

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let exec = execute_streaming(fresh_db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");

        let mut audio_chunks: Vec<AudioStreamChunk> = Vec::new();
        let mut saw_finish = false;
        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            match item.expect("delta ok") {
                EnvelopeDelta::Audio(a) => {
                    if a.finish_reason == Some(FinishReason::Stop) {
                        saw_finish = true;
                    }
                    audio_chunks.push(a);
                }
                EnvelopeDelta::Llm(_) => panic!("audio chain emitted Llm delta"),
            }
        }

        assert!(!audio_chunks.is_empty(), "audio chain empty");
        let synthesized = fake_tts.synthesized.lock().unwrap().clone();
        assert_eq!(
            synthesized.len(),
            1,
            "FakeTts.synthesize wywołane {} razy zamiast 1",
            synthesized.len()
        );
        assert!(
            synthesized[0].contains("Hello world."),
            "tts dostał obcięty tekst: {:?}",
            synthesized[0]
        );
        // Pierwszy audio chunk niesie bajty syntezy; ostatni może być
        // pustym terminalnym z finish_reason=Stop (parytet z bridge tests).
        assert_eq!(audio_chunks[0].bytes_delta, audio_bytes);
        assert_eq!(audio_chunks[0].mime, "audio/wav");
        assert!(saw_finish, "klient nie dostał finish_reason=Stop dla audio");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// §3.11 B — execute_streaming drives ANY registered StreamProducerAdapter,
    /// not just the LLM slot. A non-LLM `TestStreamProducer` terminating at
    /// output(stream) streams its EnvelopeDelta chunks through to the client.
    #[tokio::test]
    async fn execute_streaming_with_non_llm_stream_producer() {
        use crate::flow_engine::node_adapter::test_support::{CapturingProgress, TestStreamProducer};
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_stream_producer(Arc::new(TestStreamProducer::new("test_producer")));
        let registry = Arc::new(r);

        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"p1","type":"test_producer","config":{}},
                {"id":"o1","type":"output","config":{"mode":"stream"}}
            ],
            "edges":[
                {"from":"t1","to":"p1","from_port":"text","to_port":"in"},
                {"from":"p1","to":"o1","from_port":"stream","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        let capture = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = capture.clone();

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let exec = execute_streaming(fresh_db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");

        let mut concat = String::new();
        let mut saw_finish = false;
        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            let EnvelopeDelta::Llm(c) = item.expect("delta ok") else {
                panic!("expected Llm delta");
            };
            concat.push_str(&c.text_delta);
            if c.finish_reason == Some(FinishReason::Stop) {
                saw_finish = true;
            }
        }
        assert!(
            concat.contains("hello from test producer"),
            "non-LLM producer text not streamed: {concat:?}"
        );
        assert!(saw_finish, "client never saw finish_reason=Stop");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);

        // §3.11 C — the producer node surfaced started + finished(ok).
        use crate::flow_engine::dispatchers::ProgressEvent;
        let evs: Vec<ProgressEvent> = capture.events().into_iter().map(|(_, e)| e).collect();
        assert!(evs.iter().any(|e| matches!(
            e,
            ProgressEvent::NodeStarted { node_id, .. } if node_id == "p1"
        )));
        assert!(evs.iter().any(|e| matches!(
            e,
            ProgressEvent::NodeFinished { node_id, status } if node_id == "p1" && status == "ok"
        )));
    }

    /// §3.11 A — a stream producer cancelled MID-FLIGHT (after it already
    /// emitted output) must be traced as Error("cancelled"), NOT Skipped.
    /// `Skipped` is reserved for nodes gated off that never executed; a producer
    /// whose stream was being consumed did run. The audio path is the exact
    /// scenario from review (audio chunks emitted, then cancel).
    #[tokio::test]
    async fn cancelled_mid_audio_producer_traced_error_not_skipped() {
        use crate::flow_engine::envelope::{AudioStreamChunk, EnvelopeDelta};
        use crate::flow_engine::node_adapter::{NodeAdapter, PortSpec, StreamProducerAdapter};
        use crate::flow_engine::types::{FlowDataType, FlowNode};

        // Producer emits one audio chunk, then parks forever — so the finalizer
        // is mid-stream (audio_chunks_emitted=1) when the cancel token fires.
        struct AudioThenPark;
        #[async_trait]
        impl NodeAdapter for AudioThenPark {
            fn node_type(&self) -> &str {
                "audio_park"
            }
            fn input_ports(&self) -> Vec<PortSpec> {
                vec![PortSpec::new("in", FlowDataType::Text)]
            }
            fn output_ports(&self) -> Vec<PortSpec> {
                vec![
                    PortSpec::new("stream", FlowDataType::Audio),
                    PortSpec::new("full", FlowDataType::Audio),
                ]
            }
            async fn execute(
                &self,
                _node: &FlowNode,
                _inputs: &[NodeInput],
                _ctx: &ExecutionContext,
            ) -> Result<FlowEnvelope> {
                Ok(FlowEnvelope::empty())
            }
        }
        #[async_trait]
        impl StreamProducerAdapter for AudioThenPark {
            async fn produce_stream(
                &self,
                _node: &FlowNode,
                _inputs: &[NodeInput],
                _ctx: &ExecutionContext,
            ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
                let chunk = AudioStreamChunk {
                    choice_index: 0,
                    bytes_delta: vec![0x01, 0x02],
                    mime: "audio/wav".into(),
                    sample_rate: Some(22_050),
                    finish_reason: None,
                };
                let head = futures::stream::iter(vec![Ok(EnvelopeDelta::Audio(chunk))]);
                // Park: a stream that never yields again, so the finalizer waits
                // on next() until the cancel token wins the biased select.
                let tail = futures::stream::pending::<Result<EnvelopeDelta>>();
                Ok(head.chain(tail).boxed())
            }
        }

        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_stream_producer(Arc::new(AudioThenPark));
        let registry = Arc::new(r);

        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"p1","type":"audio_park","config":{}},
                {"id":"o1","type":"output","config":{"mode":"stream"}}
            ],
            "edges":[
                {"from":"t1","to":"p1","from_port":"text","to_port":"in"},
                {"from":"p1","to":"o1","from_port":"stream","to_port":"audio"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        let cancel = CancellationToken::new();
        let mut ctx = stub_ctx();
        ctx.cancel_token = cancel.clone();

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let exec = execute_streaming(fresh_db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");

        // Drain exactly the one emitted audio chunk, then cancel so the parked
        // producer settles via the cancel branch (not stream end).
        let mut stream = exec.stream;
        let first = stream.next().await.expect("audio chunk").expect("delta ok");
        assert!(
            matches!(first, EnvelopeDelta::Audio(_)),
            "expected the emitted audio chunk first"
        );
        cancel.cancel();

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Cancelled);
        let producer = outcome
            .trace
            .iter()
            .find(|s| s.node_id == "p1")
            .expect("producer trace step");
        assert!(
            matches!(producer.status, TraceStatus::Error { ref message } if message == "cancelled"),
            "cancelled-after-run producer must be Error(cancelled), not Skipped: {:?}",
            producer.status
        );
        assert_ne!(
            producer.status,
            TraceStatus::Skipped,
            "Skipped is reserved for gated-out nodes that never ran"
        );
    }
}

#[cfg(test)]
mod concurrent_executor_tests {
    //! Dataflow scheduler: fan-out z dowolnego noda lecą równolegle, a node z
    //! N wejściami (`combine`/`output`) jest naturalną barierą.
    use super::execute_blocking;
    use crate::db::DbPool;
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::envelope::{
        FinishReason, FlowEnvelope, FlowExecutionOutcome, FlowValue, NodeInput,
    };
    use crate::flow_engine::node_adapter::{
        test_support::stub_ctx, AdapterRegistry, ExecutionContext, NodeAdapter, PortSpec,
    };
    use crate::flow_engine::node_adapters::{
        CombineNodeAdapter, ConditionNodeAdapter, OutputNodeAdapter, TriggerNodeAdapter,
    };
    use crate::flow_engine::types::{FlowDataType, FlowNode};
    use crate::flow_engine::envelope::TraceStatus;
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Adapter który śpi `config.sleep_ms` (timer, nie blocking) i zwraca
    /// Text(node.id). `config.fail=true` → błąd (test fail-fast). Sleepy z
    /// równolegle odpalonych gałęzi nakładają się czasowo, więc wall-clock
    /// dowodzi współbieżności nawet na single-thread runtime.
    struct SleepAdapter;
    #[async_trait]
    impl NodeAdapter for SleepAdapter {
        fn node_type(&self) -> &str {
            "sleep"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Any)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("full", FlowDataType::Text)]
        }
        async fn execute(
            &self,
            node: &FlowNode,
            inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            if node
                .config
                .get("fail")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Err(anyhow!("boom from {}", node.id));
            }
            let ms = node
                .config
                .get("sleep_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            // Passthrough envelope (preserves variables travelling with it),
            // payload overwritten with the node id — like real adapters that
            // clone their input rather than discarding the data channel.
            let mut out = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(FlowEnvelope::empty);
            out.payload = FlowValue::Text(node.id.clone());
            Ok(out)
        }
    }

    fn registry() -> Arc<AdapterRegistry> {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(CombineNodeAdapter::new()));
        r.register(Arc::new(ConditionNodeAdapter::new()));
        r.register(Arc::new(SleepAdapter));
        Arc::new(r)
    }

    /// Zwraca status node'a z trace wyniku (None gdy node nie pojawił się w
    /// trace — np. nie został spawnowany ani oznaczony Skipped).
    fn node_status<'a>(
        outcome: &'a FlowExecutionOutcome,
        node_id: &str,
    ) -> Option<&'a TraceStatus> {
        outcome
            .trace
            .iter()
            .find(|s| s.node_id == node_id)
            .map(|s| &s.status)
    }

    fn db() -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        // `execute_blocking` writes a `flow_executions` row FK-bound to `flows(id)`;
        // seed the flow these tests compile under id "0" so the log write succeeds.
        {
            let conn = pool.lock().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES ('0', 'test', '{}', 'active')",
                [],
            )
            .expect("seed flow");
        }
        pool
    }

    async fn run(json: &str) -> FlowExecutionOutcome {
        run_with_db(json, db()).await
    }

    /// Runs `json` against a pre-built db. Timing-sensitive tests build the db
    /// outside the timed region (running the full migration suite on a fresh
    /// in-memory db costs ~100ms, which would otherwise dominate the measurement).
    async fn run_with_db(json: &str, db: DbPool) -> FlowExecutionOutcome {
        let reg = registry();
        let compiled = Arc::new(CompiledFlow::from_json("0", json, &reg).expect("compile"));
        execute_blocking(db, compiled, FlowEnvelope::empty(), stub_ctx(), reg)
            .await
            .expect("exec")
    }

    #[tokio::test]
    async fn fanout_branches_run_concurrently_and_combine_waits() {
        // trigger → a(150ms) + b(150ms) → combine → output.
        // Sekwencyjnie ~300ms; równolegle ~150ms. Combine widzi oba wyniki
        // (dowód że czekał na wolniejszą gałąź = bariera).
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"a","type":"sleep","config":{"sleep_ms":150}},
                {"id":"b","type":"sleep","config":{"sleep_ms":150}},
                {"id":"c","type":"combine","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"a","from_port":"text","to_port":"in"},
                {"from":"t","to":"b","from_port":"text","to_port":"in"},
                {"from":"a","to":"c","from_port":"full","to_port":"in"},
                {"from":"b","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let db = db();
        let start = Instant::now();
        let outcome = run_with_db(json, db).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(280),
            "branches must run concurrently, took {elapsed:?}"
        );
        let text = outcome.final_envelope.payload.as_text().unwrap_or("");
        assert!(text.contains("a"), "combine missing branch a: {text:?}");
        assert!(text.contains("b"), "combine missing branch b: {text:?}");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn five_way_fanout_from_one_node_recombines() {
        // src → 5 niezależnych gałęzi (80ms) → combine. Dowolny fan-out z
        // dowolnego noda + zbiórka na końcu.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"src","type":"sleep","config":{}},
                {"id":"n1","type":"sleep","config":{"sleep_ms":80}},
                {"id":"n2","type":"sleep","config":{"sleep_ms":80}},
                {"id":"n3","type":"sleep","config":{"sleep_ms":80}},
                {"id":"n4","type":"sleep","config":{"sleep_ms":80}},
                {"id":"n5","type":"sleep","config":{"sleep_ms":80}},
                {"id":"c","type":"combine","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"src","from_port":"text","to_port":"in"},
                {"from":"src","to":"n1","from_port":"full","to_port":"in"},
                {"from":"src","to":"n2","from_port":"full","to_port":"in"},
                {"from":"src","to":"n3","from_port":"full","to_port":"in"},
                {"from":"src","to":"n4","from_port":"full","to_port":"in"},
                {"from":"src","to":"n5","from_port":"full","to_port":"in"},
                {"from":"n1","to":"c","from_port":"full","to_port":"in"},
                {"from":"n2","to":"c","from_port":"full","to_port":"in"},
                {"from":"n3","to":"c","from_port":"full","to_port":"in"},
                {"from":"n4","to":"c","from_port":"full","to_port":"in"},
                {"from":"n5","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let db = db();
        let start = Instant::now();
        let outcome = run_with_db(json, db).await;
        let elapsed = start.elapsed();
        // Sekwencyjnie 5×80=400ms; równolegle ~80ms.
        assert!(
            elapsed < Duration::from_millis(280),
            "5 branches must run concurrently, took {elapsed:?}"
        );
        let text = outcome.final_envelope.payload.as_text().unwrap_or("");
        for id in ["n1", "n2", "n3", "n4", "n5"] {
            assert!(text.contains(id), "combine missing {id}: {text:?}");
        }
    }

    #[tokio::test]
    async fn linear_flow_unchanged() {
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"a","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"a","from_port":"text","to_port":"in"},
                {"from":"a","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run(json).await;
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("a"));
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// Wykonuje `json` z initial payloadem Text(`input_text`) — pozwala
    /// condition (`field:"input"`) zdecydować gałąź deterministycznie.
    async fn run_with_input(json: &str, input_text: &str) -> FlowExecutionOutcome {
        let reg = registry();
        let compiled = Arc::new(CompiledFlow::from_json("0", json, &reg).expect("compile"));
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text(input_text.into());
        execute_blocking(db(), compiled, initial, stub_ctx(), reg)
            .await
            .expect("exec")
    }

    /// condition → tylko aktywna gałąź się wykonuje; nieaktywna jest Skipped,
    /// a node poniżej nieaktywnej gałęzi też jest Skipped (propagacja).
    #[tokio::test]
    async fn condition_runs_only_active_branch_and_skips_the_other() {
        // trigger → cond. cond.true → t_branch → t_tail; cond.false → f_branch.
        // input="go" ⇒ true aktywne: t_branch + t_tail wykonane, f_branch
        // Skipped.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"cond","type":"condition","config":{"field":"input","operator":"equals","value":"go"}},
                {"id":"t_branch","type":"sleep","config":{}},
                {"id":"t_tail","type":"sleep","config":{}},
                {"id":"f_branch","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"cond","from_port":"text","to_port":"in"},
                {"from":"cond","to":"t_branch","from_port":"true","to_port":"in"},
                {"from":"t_branch","to":"t_tail","from_port":"full","to_port":"in"},
                {"from":"cond","to":"f_branch","from_port":"false","to_port":"in"},
                {"from":"t_tail","to":"o","from_port":"full","to_port":"text"},
                {"from":"f_branch","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "go").await;
        assert_eq!(node_status(&outcome, "t_branch"), Some(&TraceStatus::Ok));
        assert_eq!(node_status(&outcome, "t_tail"), Some(&TraceStatus::Ok));
        assert_eq!(
            node_status(&outcome, "f_branch"),
            Some(&TraceStatus::Skipped),
            "false branch must be skipped: {:?}",
            outcome.trace
        );
        // Output dostaje wejście tylko z żywej gałęzi (t_tail), drugie wejście
        // pochodzi od Skipped f_branch (brak output) — bariera spełniona.
        assert_eq!(node_status(&outcome, "o"), Some(&TraceStatus::Ok));
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("t_tail"));
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// Odwrotny wybór: input nie pasuje ⇒ false aktywne, true Skipped wraz z
    /// całym łańcuchem za nim.
    #[tokio::test]
    async fn condition_false_branch_active_skips_true_chain() {
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"cond","type":"condition","config":{"field":"input","operator":"equals","value":"go"}},
                {"id":"t_branch","type":"sleep","config":{}},
                {"id":"t_tail","type":"sleep","config":{}},
                {"id":"f_branch","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"cond","from_port":"text","to_port":"in"},
                {"from":"cond","to":"t_branch","from_port":"true","to_port":"in"},
                {"from":"t_branch","to":"t_tail","from_port":"full","to_port":"in"},
                {"from":"cond","to":"f_branch","from_port":"false","to_port":"in"},
                {"from":"t_tail","to":"o","from_port":"full","to_port":"text"},
                {"from":"f_branch","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "nope").await;
        assert_eq!(node_status(&outcome, "f_branch"), Some(&TraceStatus::Ok));
        assert_eq!(
            node_status(&outcome, "t_branch"),
            Some(&TraceStatus::Skipped)
        );
        assert_eq!(
            node_status(&outcome, "t_tail"),
            Some(&TraceStatus::Skipped),
            "node below skipped branch must be skipped too: {:?}",
            outcome.trace
        );
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("f_branch"));
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// Node zasilany jednocześnie przez gałąź Skipped i gałąź żywą NADAL się
    /// wykonuje (≥1 żywa krawędź wejściowa). combine traktuje Skipped jako
    /// nieobecne wejście.
    #[tokio::test]
    async fn node_fed_by_skipped_and_live_branch_still_runs() {
        // trigger → live(sleep) + cond. cond.true → dead_in (Skipped, bo input
        // != "go"). combine zbiera live + dead_in: dead_in Skipped, live żywy
        // ⇒ combine wykonuje się z samym live.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"live","type":"sleep","config":{}},
                {"id":"cond","type":"condition","config":{"field":"input","operator":"equals","value":"go"}},
                {"id":"dead_in","type":"sleep","config":{}},
                {"id":"c","type":"combine","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"live","from_port":"text","to_port":"in"},
                {"from":"t","to":"cond","from_port":"text","to_port":"in"},
                {"from":"cond","to":"dead_in","from_port":"true","to_port":"in"},
                {"from":"live","to":"c","from_port":"full","to_port":"in"},
                {"from":"dead_in","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "nope").await;
        assert_eq!(node_status(&outcome, "live"), Some(&TraceStatus::Ok));
        assert_eq!(node_status(&outcome, "dead_in"), Some(&TraceStatus::Skipped));
        assert_eq!(
            node_status(&outcome, "c"),
            Some(&TraceStatus::Ok),
            "combine must run with the single live input: {:?}",
            outcome.trace
        );
        // combine widzi tylko żywe wejście (Skipped nie ma output).
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("live"));
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// Gdy WSZYSTKIE wejścia barriery są Skipped, sama bariera jest Skipped.
    #[tokio::test]
    async fn barrier_with_all_skipped_inputs_is_skipped() {
        // cond.true → a → c; cond.false → b → c. Tylko jedna gałąź żyje, więc
        // c (combine) ma jedno żywe + jedno Skipped wejście → c żyje. Żeby
        // sprawdzić all-skipped, kierujemy OBA wejścia combine z tej samej
        // (nieaktywnej) gałęzi: cond.true → a → c; a → mid → c (oba za 'a').
        // input != "go" ⇒ true Skipped ⇒ a, mid, c wszystkie Skipped.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"cond","type":"condition","config":{"field":"input","operator":"equals","value":"go"}},
                {"id":"a","type":"sleep","config":{}},
                {"id":"mid","type":"sleep","config":{}},
                {"id":"c","type":"combine","config":{}},
                {"id":"f_branch","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"cond","from_port":"text","to_port":"in"},
                {"from":"cond","to":"a","from_port":"true","to_port":"in"},
                {"from":"a","to":"mid","from_port":"full","to_port":"in"},
                {"from":"a","to":"c","from_port":"full","to_port":"in"},
                {"from":"mid","to":"c","from_port":"full","to_port":"in"},
                {"from":"cond","to":"f_branch","from_port":"false","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"},
                {"from":"f_branch","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "nope").await;
        assert_eq!(node_status(&outcome, "f_branch"), Some(&TraceStatus::Ok));
        assert_eq!(node_status(&outcome, "a"), Some(&TraceStatus::Skipped));
        assert_eq!(node_status(&outcome, "mid"), Some(&TraceStatus::Skipped));
        assert_eq!(
            node_status(&outcome, "c"),
            Some(&TraceStatus::Skipped),
            "barrier with all-skipped inputs must be skipped: {:?}",
            outcome.trace
        );
        // Output zasilane przez żywą f_branch i Skipped combine ⇒ żyje.
        assert_eq!(node_status(&outcome, "o"), Some(&TraceStatus::Ok));
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("f_branch"));
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// condition.expression (CEL) drives the stage-A gating end-to-end: a bool
    /// expression over the envelope selects which branch runs, the other is
    /// Skipped (§3.11 A + §3.12).
    #[tokio::test]
    async fn condition_expression_cel_drives_gating_end_to_end() {
        // input="go" ⇒ expression `payload == "go"` is true ⇒ true branch runs,
        // false branch Skipped.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"cond","type":"condition","config":{"expression":"payload == \"go\""}},
                {"id":"t_branch","type":"sleep","config":{}},
                {"id":"f_branch","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"cond","from_port":"text","to_port":"in"},
                {"from":"cond","to":"t_branch","from_port":"true","to_port":"in"},
                {"from":"cond","to":"f_branch","from_port":"false","to_port":"in"},
                {"from":"t_branch","to":"o","from_port":"full","to_port":"text"},
                {"from":"f_branch","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "go").await;
        assert_eq!(node_status(&outcome, "t_branch"), Some(&TraceStatus::Ok));
        assert_eq!(node_status(&outcome, "f_branch"), Some(&TraceStatus::Skipped));
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("t_branch"));

        // input="stop" ⇒ false branch runs, true branch Skipped.
        let outcome = run_with_input(json, "stop").await;
        assert_eq!(node_status(&outcome, "f_branch"), Some(&TraceStatus::Ok));
        assert_eq!(node_status(&outcome, "t_branch"), Some(&TraceStatus::Skipped));
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("f_branch"));
    }

    /// input_mapping computes a node's config from variables; output_mapping
    /// writes the node's result into a flow variable. End-to-end over the
    /// blocking executor seam (§3.12).
    #[tokio::test]
    async fn io_mapping_overlay_and_variable_write_end_to_end() {
        // The sleep node's `sleep_ms` is computed from a variable seeded via the
        // trigger's output_mapping; the combine writes a variable read back from
        // the final envelope.
        let json = r#"{
            "variables":[{"name":"greeting","type":"text"}],
            "nodes":[
                {"id":"t","type":"trigger","config":{"output_mapping":{"greeting":"payload"}}},
                {"id":"a","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"a","from_port":"text","to_port":"in"},
                {"from":"a","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "hello").await;
        // The variable written by the trigger's output_mapping rides the
        // envelope downstream (sleep clones its input envelope).
        assert_eq!(
            outcome.final_envelope.variables.get("greeting"),
            Some(&FlowValue::Text("hello".into()))
        );
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// §3.12 fan-in io-mapping determinism: a `combine` carrying `input_mapping`
    /// must evaluate it against the branch with the lowest `from_node_id`,
    /// matching combine's own merge ordering — never an arbitrary first input.
    /// Each branch tags the envelope with its own id via output_mapping; the
    /// combine's input_mapping reads `vars.branch_tag` into its `separator`
    /// config so the joined payload reveals which branch the scope used. The
    /// SleepAdapter emits `Text(node.id)`, so branches "a" and "z" yield
    /// payloads "a" and "z"; combine joins them sorted ("a" then "z") with the
    /// chosen separator. Inbound scope = lowest id "a" ⇒ separator "a" ⇒ "aaz".
    #[tokio::test]
    async fn fan_in_io_mapping_reads_lowest_from_node_id_branch() {
        let json = r#"{
            "variables":[{"name":"branch_tag","type":"text"}],
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"a","type":"sleep","config":{"output_mapping":{"branch_tag":"\"a\""}}},
                {"id":"z","type":"sleep","config":{"output_mapping":{"branch_tag":"\"z\""}}},
                {"id":"c","type":"combine","config":{
                    "variable_merge_policy":{"branch_tag":"last_wins"},
                    "input_mapping":{"separator":"vars.branch_tag"}
                }},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"a","from_port":"text","to_port":"in"},
                {"from":"t","to":"z","from_port":"text","to_port":"in"},
                {"from":"a","to":"c","from_port":"full","to_port":"in"},
                {"from":"z","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run(json).await;
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        // Separator "a" (lowest-id branch's tag) ⇒ "a" + "a" + "z" = "aaz".
        // An arbitrary "z" scope would instead give "azz" — the assertion
        // pins the deterministic choice.
        assert_eq!(
            outcome.final_envelope.payload.as_text(),
            Some("aaz"),
            "combine io-mapping must read the lowest-id branch deterministically: {:?}",
            outcome.final_envelope.payload
        );
    }

    #[tokio::test]
    async fn branch_error_fails_fast() {
        // Gałąź b zwraca błąd; continue_on_error=false (default) → flow error,
        // siostrzane in-flight taski abortowane.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"a","type":"sleep","config":{"sleep_ms":200}},
                {"id":"b","type":"sleep","config":{"fail":true}},
                {"id":"c","type":"combine","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"a","from_port":"text","to_port":"in"},
                {"from":"t","to":"b","from_port":"text","to_port":"in"},
                {"from":"a","to":"c","from_port":"full","to_port":"in"},
                {"from":"b","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run(json).await;
        assert_eq!(outcome.finish_reason, FinishReason::Error);
        assert!(
            outcome.error.as_deref().unwrap_or("").contains("boom"),
            "expected boom error, got {:?}",
            outcome.error
        );
    }

    /// §3.11 C — the executor emits NodeStarted/NodeFinished for every node,
    /// captured via a test ProgressSink injected on the ExecutionContext.
    #[tokio::test]
    async fn progress_sink_receives_node_started_and_finished() {
        use crate::flow_engine::dispatchers::ProgressEvent;
        use crate::flow_engine::node_adapter::test_support::CapturingProgress;
        let reg = registry();
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"a","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"a","from_port":"text","to_port":"in"},
                {"from":"a","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(CompiledFlow::from_json("0", json, &reg).expect("compile"));
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let capture = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = capture.clone();
        ctx.progress_scope = "session-x".into();

        let outcome = execute_blocking(db(), compiled, initial, ctx, reg)
            .await
            .expect("exec");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);

        let events = capture.events();
        // Every event carries the configured scope.
        assert!(events.iter().all(|(s, _)| s == "session-x"));
        let only = |events: &[(String, ProgressEvent)]| {
            events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>()
        };
        let evs = only(&events);
        for id in ["t", "a", "o"] {
            assert!(
                evs.iter().any(|e| matches!(
                    e,
                    ProgressEvent::NodeStarted { node_id, .. } if node_id == id
                )),
                "missing NodeStarted for {id}: {evs:?}"
            );
            assert!(
                evs.iter().any(|e| matches!(
                    e,
                    ProgressEvent::NodeFinished { node_id, status } if node_id == id && status == "ok"
                )),
                "missing NodeFinished(ok) for {id}: {evs:?}"
            );
        }
    }

    /// §3.11 C — a skipped node still surfaces as NodeStarted + NodeFinished
    /// with status `skipped`, so the UI shows it was reached.
    #[tokio::test]
    async fn progress_sink_marks_skipped_branch() {
        use crate::flow_engine::dispatchers::ProgressEvent;
        use crate::flow_engine::node_adapter::test_support::CapturingProgress;
        let reg = registry();
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"cond","type":"condition","config":{"field":"input","operator":"equals","value":"go"}},
                {"id":"t_branch","type":"sleep","config":{}},
                {"id":"f_branch","type":"sleep","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"cond","from_port":"text","to_port":"in"},
                {"from":"cond","to":"t_branch","from_port":"true","to_port":"in"},
                {"from":"cond","to":"f_branch","from_port":"false","to_port":"in"},
                {"from":"t_branch","to":"o","from_port":"full","to_port":"text"},
                {"from":"f_branch","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(CompiledFlow::from_json("0", json, &reg).expect("compile"));
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("nope".into());

        let capture = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = capture.clone();

        execute_blocking(db(), compiled, initial, ctx, reg)
            .await
            .expect("exec");

        let evs: Vec<ProgressEvent> = capture.events().into_iter().map(|(_, e)| e).collect();
        assert!(
            evs.iter().any(|e| matches!(
                e,
                ProgressEvent::NodeFinished { node_id, status } if node_id == "t_branch" && status == "skipped"
            )),
            "true branch must be reported skipped: {evs:?}"
        );
    }
}

#[cfg(test)]
mod harness_streaming_tests {
    //! §3.11 B end-to-end: `execute_streaming` drives a `loop` block as the
    //! stream producer. The outer flow is `trigger → loop → output(stream)`; the
    //! loop's body flow is `trigger → stream_body → output(stream)`. Intermediate
    //! iterations run blocking (driving a counter), and the FINAL iteration
    //! streams its deltas, which `execute_streaming` forwards to the client. This
    //! is the harness final-answer path: Agent Run's loop produces, Agent
    //! Iteration ends in output(stream).
    use super::execute_streaming;
    use crate::db::{migrations, DbPool};
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::envelope::{
        EnvelopeDelta, FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk, NodeInput,
    };
    use crate::flow_engine::node_adapter::{
        test_support::stub_ctx, ExecutionContext, NodeAdapter, PortSpec, StreamProducerAdapter,
    };
    use crate::flow_engine::subflow_runner::{SubflowRunner, SubflowRunnerSlot};
    use crate::flow_engine::types::{FlowDataType, FlowNode};
    use anyhow::Result;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(Mutex::new(conn))
    }

    fn insert_flow(pool: &DbPool, id: &str, json: &str) {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, service_type, flow_json, status, is_default) \
             VALUES (?1, ?2, NULL, ?3, 'active', 0)",
            rusqlite::params![id, id, json],
        )
        .expect("insert flow");
    }

    /// Body producer: `execute` (blocking iterations) bumps a counter + sets
    /// harness_done at `stop_at`; `produce_stream` (final pass) streams the
    /// final answer tagged with the iteration count.
    struct StreamBody {
        stop_at: i64,
    }

    #[async_trait]
    impl NodeAdapter for StreamBody {
        fn node_type(&self) -> &str {
            "harness_stream_body"
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
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let mut env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let n = env.meta.get("iter").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            env.meta.insert("iter".into(), Value::from(n));
            if n >= self.stop_at {
                env.meta.insert("harness_done".into(), Value::Bool(true));
            }
            env.payload = FlowValue::Text(format!("blocking iter {n}"));
            Ok(env)
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for StreamBody {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
            let env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let iter = env.meta.get("iter").and_then(|v| v.as_i64()).unwrap_or(0);
            let first = LlmStreamChunk {
                text_delta: format!("FINAL(iter={iter})"),
                ..Default::default()
            };
            let last = LlmStreamChunk {
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            };
            Ok(futures::stream::iter(vec![
                Ok(EnvelopeDelta::Llm(first)),
                Ok(EnvelopeDelta::Llm(last)),
            ])
            .boxed())
        }
    }

    #[tokio::test]
    async fn execute_streaming_drives_loop_producer_end_to_end() {
        let pool = db();

        // The loop block (a registered stream producer) drives the body flow
        // through a live SubflowRunner. Wire the runner's slot into the loop
        // adapter via `build_registry_with_runner`, then fill the slot with a
        // runner whose registry Weak points back at the Arc-wrapped registry.
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let registry = {
            let mut r = crate::flow_engine::dispatcher::build_registry_with_runner(slot.clone());
            r.register_stream_producer(Arc::new(StreamBody { stop_at: 3 }));
            Arc::new(r)
        };
        *slot.write() = Some(Arc::new(SubflowRunner::new(
            pool.clone(),
            Arc::downgrade(&registry),
        )));

        // Body flow (streaming end-shape): trigger → stream_body → output(stream).
        let body_id = "00000000-harn-strm-body-000000000001";
        insert_flow(
            &pool,
            body_id,
            &json!({
                "nodes": [
                    {"id": "t", "type": "trigger", "config": {}},
                    {"id": "b", "type": "harness_stream_body", "config": {}},
                    {"id": "o", "type": "output", "config": {"mode": "stream"}}
                ],
                "edges": [
                    {"from": "t", "from_port": "text", "to": "b", "to_port": "in"},
                    {"from": "b", "from_port": "stream", "to": "o", "to_port": "text"}
                ]
            })
            .to_string(),
        );

        // Outer flow row under id "0" so the executor's flow_executions write is
        // FK-satisfiable (the outer run is a real audit row; the loop body
        // iterations run light, no per-iteration rows).
        insert_flow(&pool, "0", "{}");

        // Outer flow: trigger → loop(body) → output(stream).
        let outer_json = format!(
            r#"{{
                "nodes":[
                    {{"id":"t1","type":"trigger","config":{{}}}},
                    {{"id":"l1","type":"loop","config":{{"body_flow_id":"{body_id}","max_iterations":10}}}},
                    {{"id":"o1","type":"output","config":{{"mode":"stream"}}}}
                ],
                "edges":[
                    {{"from":"t1","to":"l1","from_port":"text","to_port":"in"}},
                    {{"from":"l1","to":"o1","from_port":"stream","to_port":"text"}}
                ]
            }}"#
        );
        let compiled = Arc::new(
            CompiledFlow::from_json("0", &outer_json, &registry).expect("compile outer"),
        );

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("go".into());

        let exec = execute_streaming(pool.clone(), compiled, initial, stub_ctx(), registry)
            .await
            .expect("execute_streaming");

        let mut text = String::new();
        let mut saw_finish = false;
        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta ok") {
                text.push_str(&c.text_delta);
                if c.finish_reason == Some(FinishReason::Stop) {
                    saw_finish = true;
                }
            }
        }
        // 3 blocking iterations drove harness_done → the loop exits `until` with
        // the already-computed answer ("blocking iter 3") and forwards THAT as a
        // terminal stream. It must NOT run a fresh streaming pass (no
        // "FINAL(iter=...)" marker) — that would re-answer a finished turn.
        assert!(
            text.contains("blocking iter 3"),
            "loop did not forward the computed answer: {text:?}"
        );
        assert!(
            !text.contains("FINAL("),
            "until exit must not run a streaming pass: {text:?}"
        );
        assert!(saw_finish, "client never got finish_reason=Stop");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("blocking iter 3"));
    }

    /// Finding 3 (end-to-end) — the grace-summary streaming pass runs through the
    /// executor ONLY when the budget is exhausted with `final_pass=true`. The
    /// body never sets harness_done, so the loop runs its full budget blocking,
    /// then streams one final pass that emits the "FINAL(iter=N)" marker.
    #[tokio::test]
    async fn execute_streaming_runs_grace_pass_end_to_end() {
        let pool = db();

        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let registry = {
            let mut r = crate::flow_engine::dispatcher::build_registry_with_runner(slot.clone());
            // stop_at far above the budget → harness_done never set.
            r.register_stream_producer(Arc::new(StreamBody { stop_at: 1000 }));
            Arc::new(r)
        };
        *slot.write() = Some(Arc::new(SubflowRunner::new(
            pool.clone(),
            Arc::downgrade(&registry),
        )));

        let body_id = "00000000-harn-strm-body-000000000002";
        insert_flow(
            &pool,
            body_id,
            &json!({
                "nodes": [
                    {"id": "t", "type": "trigger", "config": {}},
                    {"id": "b", "type": "harness_stream_body", "config": {}},
                    {"id": "o", "type": "output", "config": {"mode": "stream"}}
                ],
                "edges": [
                    {"from": "t", "from_port": "text", "to": "b", "to_port": "in"},
                    {"from": "b", "from_port": "stream", "to": "o", "to_port": "text"}
                ]
            })
            .to_string(),
        );
        insert_flow(&pool, "0", "{}");

        // Outer flow: budget 2 + final_pass so the grace pass streams.
        let outer_json = format!(
            r#"{{
                "nodes":[
                    {{"id":"t1","type":"trigger","config":{{}}}},
                    {{"id":"l1","type":"loop","config":{{"body_flow_id":"{body_id}","max_iterations":2,"final_pass":true}}}},
                    {{"id":"o1","type":"output","config":{{"mode":"stream"}}}}
                ],
                "edges":[
                    {{"from":"t1","to":"l1","from_port":"text","to_port":"in"}},
                    {{"from":"l1","to":"o1","from_port":"stream","to_port":"text"}}
                ]
            }}"#
        );
        let compiled = Arc::new(
            CompiledFlow::from_json("0", &outer_json, &registry).expect("compile outer"),
        );

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("go".into());

        let exec = execute_streaming(pool.clone(), compiled, initial, stub_ctx(), registry)
            .await
            .expect("execute_streaming");

        let mut text = String::new();
        let mut saw_finish = false;
        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta ok") {
                text.push_str(&c.text_delta);
                if c.finish_reason == Some(FinishReason::Stop) {
                    saw_finish = true;
                }
            }
        }
        // 2 blocking iterations exhausted the budget; the grace pass streamed
        // with the last blocking iteration's count (iter=2).
        assert!(text.contains("FINAL(iter=2)"), "grace pass did not stream: {text:?}");
        assert!(saw_finish, "client never got finish_reason=Stop");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("FINAL(iter=2)"));
    }
}
