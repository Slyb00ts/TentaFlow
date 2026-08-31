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

use crate::db::models::NewFlowExecution;
use crate::db::{repository, DbPool};
use crate::flow_engine::cache::CompiledFlow;
use crate::flow_engine::envelope::{
    ChatMessage, EnvelopeDelta, FinishReason, FlowEnvelope, FlowExecutionOutcome, FlowValue,
    GenPerf, LlmStreamChunk, NodeInput, TokenUsage, TraceStatus, TraceStep,
};
use crate::flow_engine::io_mapping;
use crate::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext, NodeAdapter, UsageSink};
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

/// Does this chunk carry the first visible token of a streaming step?
///
/// Reasoning counts as a token. For a thinking model the stream genuinely
/// starts with reasoning, and both forwarding gates below already treat text
/// and reasoning as one class of visible narration; a text-only rule would fold
/// the entire thinking phase into TTFT and report backend latency that never
/// happened. Emptiness is tested on the reasoning STRING, not on the `Option` —
/// backends send `Some("")` alongside tool-call framing, and an empty delta is
/// not a token.
fn chunk_carries_first_token(chunk: &LlmStreamChunk) -> bool {
    !chunk.text_delta.is_empty()
        || chunk
            .reasoning_delta
            .as_ref()
            .is_some_and(|r| !r.is_empty())
}

pub struct StreamingExecution {
    pub stream: BoxStream<'static, Result<EnvelopeDelta>>,
    pub outcome: oneshot::Receiver<FlowExecutionOutcome>,
    /// Envelope fed into the stream producer. Pre-producer nodes (stt,
    /// combine, …) finish before `execute_streaming` returns, so a handler can
    /// read their meta (e.g. `stt_transcript`) without waiting for the stream.
    pub producer_input: Arc<FlowEnvelope>,
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
    let execution_id = create_execution_record(&db, &compiled.flow_id, &ctx).await?;
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
        region_internal,
    } = build_dependency_graph(&compiled, n);
    let ctx = Arc::new(ctx);
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    // `live_inputs[pos]` zlicza krawędzie wejściowe, które po rozwiązaniu
    // poprzednika okazały się aktywne (port producenta aktywny). Gdy wszyscy
    // poprzednicy są rozwiązani (`pending_deps==0`), node z ≥1 żywą krawędzią
    // wykonuje się normalnie, a node z zerem — jest Skipped i propaguje skip.
    let mut live_inputs: Vec<usize> = vec![0; n];
    // Aktywne porty wyjściowe per rozwiązany node (None = wszystkie aktywne,
    // Some(pusty) = node Skipped). Czytane przy propagacji skip-semantyki ORAZ
    // przez `build_inputs` — martwa krawędź nie wnosi inputu do następnika.
    let mut active_by_pos: Vec<Option<HashSet<String>>> = vec![None; n];
    let mut trace: Vec<TraceStep> = Vec::with_capacity(n);
    let mut error: Option<String> = None;
    let mut last_finish_reason: Option<FinishReason> = None;

    let mut join_set: JoinSet<NodeRun> = JoinSet::new();
    // Seed: wszystkie nody bez poprzedników (trigger) gotowe od razu.
    // Region-internal members are never seeded — the region runner drives them.
    for pos in 0..n {
        if region_internal[pos] {
            continue;
        }
        if pending_deps[pos] == 0 {
            spawn_unit(
                &mut join_set,
                &compiled,
                &adapters,
                &ctx,
                &outputs,
                &active_by_pos,
                pos,
            )?;
        }
    }

    while let Some(joined) = join_set.join_next().await {
        let run = joined.map_err(|e| anyhow!("flow node task failed to join: {e}"))?;
        // Aktywne porty wyjściowe rozwiązanego node'a (None = wszystkie aktywne).
        // Liczone po Ok/continue_on_error; przy fatalnym błędzie pętla i tak
        // abortuje resztę, więc gałęzie nie startują.
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
                active_by_pos[run.pos] =
                    compute_active_ports(&compiled, adapters.as_ref(), run.pos, &envelope);
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
                    let propagated = build_inputs(&compiled, run.pos, &outputs, &active_by_pos)
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
        let mut to_resolve: Vec<usize> = vec![run.pos];
        while let Some(from_pos) = to_resolve.pop() {
            // Najpierw policz żywe krawędzie wychodzące z tego node'a. Node
            // Skipped ma w `active_by_pos` pusty zbiór — zero żywych portów.
            for (to_pos, from_port) in &out_edges[from_pos] {
                let is_live = match &active_by_pos[from_pos] {
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
                        spawn_unit(
                            &mut join_set,
                            &compiled,
                            &adapters,
                            &ctx,
                            &outputs,
                            &active_by_pos,
                            succ,
                        )?;
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
                        active_by_pos[succ] = Some(HashSet::new());
                        to_resolve.push(succ);
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
        model: ctx.usage_sink.model(),
        perf: None,
        finish_reason,
        total_latency_ms,
        error: error.clone(),
    };

    persist_execution(&db, execution_id, &outcome).await;
    Ok(outcome)
}

/// Direct single-capability execution — no DAG, no trigger/output wrapper, no
/// pii_filter. Called by `FlowDispatcher` when there is no user-defined flow for
/// `model:service_type:modality`: the model runs straight on the executor via
/// its capability node adapter (`llm` / `vision_llm` / `tts` / `stt` /
/// `embeddings`), which is the canonical builder that turns the seed envelope
/// into a dispatcher request. Compliance AI events + token accounting still fire
/// inside the capability dispatcher (`ctx.llm` etc.), so removing the synthetic
/// flow loses no audit. Ephemeral: no `flow_executions` audit row.
pub async fn execute_direct_blocking(
    node: FlowNode,
    initial: FlowEnvelope,
    mut ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
) -> Result<FlowExecutionOutcome> {
    let started = Instant::now();
    let initial_arc = Arc::new(initial);
    ctx.initial_envelope = initial_arc.clone();
    let ctx = Arc::new(ctx);

    let adapter = adapters.get(&node.node_type).ok_or_else(|| {
        anyhow!(
            "no adapter for direct node '{}' (type '{}')",
            node.id,
            node.node_type
        )
    })?;

    let inputs = vec![NodeInput {
        from_node_id: "direct".to_string(),
        from_port: "full".to_string(),
        envelope: initial_arc.clone(),
    }];

    let step_started = ctx.clock.now_ms();
    let attempt = Instant::now();
    emit_node_started(&ctx, &node.id, &node.node_type);
    let final_envelope = match adapter.execute(&node, &inputs, &ctx).await {
        Ok(env) => {
            emit_node_finished(&ctx, &node.id, &TraceStatus::Ok);
            env
        }
        Err(e) => {
            let msg = e.to_string();
            emit_node_finished(
                &ctx,
                &node.id,
                &TraceStatus::Error {
                    message: msg.clone(),
                },
            );
            // `context` (not a fresh `anyhow!`) keeps the adapter's typed error
            // downcastable for `DispatchError::from`.
            return Err(e.context(format!("direct '{}' execution failed", node.node_type)));
        }
    };

    let mut trace = vec![TraceStep {
        node_id: node.id.clone(),
        node_type: node.node_type.clone(),
        started_at_ms: step_started,
        duration_ms: attempt.elapsed().as_millis() as u64,
        status: TraceStatus::Ok,
        usage: None,
    }];
    attribute_usage(&ctx, &mut trace);
    let aggregate_usage = aggregate_usage(&trace);

    Ok(FlowExecutionOutcome {
        final_envelope,
        trace,
        usage: aggregate_usage,
        model: ctx.usage_sink.model(),
        perf: None,
        finish_reason: FinishReason::Stop,
        total_latency_ms: started.elapsed().as_millis() as i64,
        error: None,
    })
}

/// Streaming counterpart of [`execute_direct_blocking`] for a stream-producing
/// capability (the `llm` node). Builds the producer's `EnvelopeDelta` stream and
/// hands it to the shared [`finalize_streaming_flow`] task, so usage/perf/finish
/// accumulation and the usage trailer are identical to the flow path (compliance
/// token accounting downstream depends on that trailer). No DAG, no chain nodes,
/// no pii_filter; ephemeral (`execution_id = 0`, no audit row).
pub async fn execute_direct_streaming(
    db: DbPool,
    node: FlowNode,
    initial: FlowEnvelope,
    mut ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
) -> Result<StreamingExecution> {
    let started = Instant::now();
    let initial_arc = Arc::new(initial);
    ctx.initial_envelope = initial_arc.clone();

    let producer = adapters.stream_producer(&node.node_type).ok_or_else(|| {
        anyhow!(
            "no StreamProducerAdapter for direct node '{}' (type '{}')",
            node.id,
            node.node_type
        )
    })?;

    let inputs = vec![NodeInput {
        from_node_id: "direct".to_string(),
        from_port: "full".to_string(),
        envelope: initial_arc.clone(),
    }];

    let producer_step_started = ctx.clock.now_ms();
    emit_node_started(&ctx, &node.id, &node.node_type);
    let envelope_stream = producer
        .produce_stream(&node, &inputs, &ctx)
        .await
        .map_err(|e| {
            emit_node_finished(
                &ctx,
                &node.id,
                &TraceStatus::Error {
                    message: e.to_string(),
                },
            );
            anyhow!("direct stream producer '{}' failed: {e}", node.id)
        })?;

    let cancel = ctx.cancel_token.clone();
    let progress = ctx.progress.clone();
    let progress_scope = ctx.progress_scope.clone();
    let (outbound_tx, outbound_rx) = mpsc::channel::<Result<EnvelopeDelta>>(64);
    let (outcome_tx, outcome_rx) = oneshot::channel::<FlowExecutionOutcome>();

    tokio::spawn(finalize_streaming_flow(
        0,
        envelope_stream,
        outbound_tx,
        outcome_tx,
        cancel,
        FinalizerInputs {
            started,
            usage_sink: ctx.usage_sink.clone(),
            producer_step_started,
            producer_node_id: node.id.clone(),
            producer_node_type: node.node_type.clone(),
            producer_input_envelope: initial_arc.clone(),
            trace: Vec::new(),
            db,
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
        producer_input: initial_arc,
    })
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
    /// `region_internal[pos]` = true for a contracted loop-region member that is
    /// NOT the entry: it is driven by the region runner, never spawned by the
    /// outer scheduler. The entry position represents the whole region.
    region_internal: Vec<bool>,
}

/// Buduje graf zależności z compiled flow. Toposort w compile gwarantuje brak
/// cykli, więc scheduler zawsze osusza JoinSet.
///
/// Inline loop regions are contracted to a single unit at their `entry_pos`:
/// every region member position is remapped to its entry, so the outer graph
/// sees the region as one node with the entry's external inputs and the exit's
/// external outputs. Internal region edges (and the `loop_back` edge) collapse
/// to self-loops and are dropped. `region_internal[pos]` marks the contracted
/// (non-entry) members so the scheduler never spawns them standalone.
fn build_dependency_graph(compiled: &CompiledFlow, n: usize) -> DependencyGraph {
    // pos → contracted owner: a region member maps to its entry_pos, every
    // other position maps to itself.
    let mut owner: Vec<usize> = (0..n).collect();
    let mut region_internal = vec![false; n];
    for region in &compiled.regions {
        for &m in &region.member_pos {
            owner[m] = region.entry_pos;
            if m != region.entry_pos {
                region_internal[m] = true;
            }
        }
    }

    // Jeden globalny HashSet par (from,pos) zamiast N HashSetów per node —
    // dedupy podwójnych krawędzi tej samej pary (rzadkie, np. dwie krawędzie do
    // jednego combine z tego samego noda) bez alokacji setu na każdy węzeł.
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut pending_deps = vec![0usize; n];
    let mut succ_nodes: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut out_edges: Vec<Vec<(usize, String)>> = vec![Vec::new(); n];
    for pos in 0..n {
        // The contracted consumer: edges into a region member are dependencies
        // of the region unit (entry_pos).
        let to_pos = owner[pos];
        for &edge_idx in &compiled.incoming_edges_per_pos[pos] {
            let edge = &compiled.definition.edges[edge_idx];
            // The back edge is not a forward dependency — the region runner
            // drives the repeat, not the scheduler.
            if edge.is_loop_back() {
                continue;
            }
            if let Some(&raw_from) = compiled.run_idx_by_id.get(edge.from.as_str()) {
                // The contracted producer: the region's output is stored at its
                // entry_pos, so an external edge out of the exit is modelled as
                // coming from the entry.
                let from_pos = owner[raw_from];
                if from_pos == to_pos {
                    continue;
                }
                // Krawędź per port — sterowanie bramkowaniem.
                out_edges[from_pos].push((to_pos, edge.from_port.clone()));
                // Zależność per odrębny poprzednik — sterowanie barierą.
                if seen_pairs.insert((from_pos, to_pos)) {
                    pending_deps[to_pos] += 1;
                    succ_nodes[from_pos].push(to_pos);
                }
            }
        }
    }
    DependencyGraph {
        pending_deps,
        succ_nodes,
        out_edges,
        region_internal,
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
    active_ports: &[Option<HashSet<String>>],
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
    let inputs = build_inputs(compiled, pos, outputs, active_ports);
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

/// Spawns a scheduler unit at `pos`: an inline loop region (when `pos` is a
/// region entry) runs its sub-DAG repeatedly inline; any other position runs as
/// a single node. The region's result envelope is stored at `entry_pos`, so the
/// contracted dependency graph (which maps the region's exit-outgoing edges to
/// the entry) propagates it to the region's successors.
fn spawn_unit(
    join_set: &mut JoinSet<NodeRun>,
    compiled: &Arc<CompiledFlow>,
    adapters: &Arc<AdapterRegistry>,
    ctx: &Arc<ExecutionContext>,
    outputs: &[Option<Arc<FlowEnvelope>>],
    active_ports: &[Option<HashSet<String>>],
    pos: usize,
) -> Result<()> {
    let Some(region) = compiled.region_at_entry(pos) else {
        return spawn_node(
            join_set,
            compiled,
            adapters,
            ctx,
            outputs,
            active_ports,
            pos,
        );
    };
    let region = region.clone();
    let def_idx = compiled.execution_order[region.entry_pos];
    let entry_node = &compiled.definition.nodes[def_idx];
    let entry_node_id = entry_node.id.clone();
    let entry_node_type = entry_node.node_type.clone();
    // Seed envelope = the region's single external input (falls back to the
    // flow's initial envelope for a triggerless harness).
    let inputs = build_inputs(compiled, region.entry_pos, outputs, active_ports);
    let seed: FlowEnvelope = inputs
        .first()
        .map(|i| (*i.envelope).clone())
        .unwrap_or_else(|| (*ctx.initial_envelope).clone());

    emit_node_started(ctx, &entry_node_id, &entry_node_type);
    let compiled = compiled.clone();
    let adapters = adapters.clone();
    let ctx = ctx.clone();
    let step_started_ms = ctx.clock.now_ms();
    join_set.spawn(async move {
        let attempt = Instant::now();
        let result = run_loop_region(&compiled, &adapters, &ctx, &region, seed).await;
        let duration_ms = attempt.elapsed().as_millis() as u64;
        NodeRun {
            pos: region.entry_pos,
            node_id: entry_node_id,
            node_type: entry_node_type,
            step_started_ms,
            duration_ms,
            result: result.map_err(|e| e.to_string()),
        }
    });
    Ok(())
}

/// Runs an inline loop region: the region's internal sub-DAG executes
/// repeatedly over ONE evolving envelope (conversation history accumulates in
/// `context.messages` across iterations — this is the whole point) until a
/// structural stop condition, the iteration budget, or cancel/deadline. The
/// loop stops when the last assistant message carries no tool calls (the agent
/// produced a final answer). With `final_pass`, one extra grace iteration runs
/// after budget exhaustion with `meta.loop_final_pass=true` so the body's llm
/// block drops tools. Cancel/deadline surface as a node error so the executor
/// aborts the flow, mirroring `loop_block.rs`.
async fn run_loop_region(
    compiled: &Arc<CompiledFlow>,
    adapters: &Arc<AdapterRegistry>,
    ctx: &Arc<ExecutionContext>,
    region: &crate::flow_engine::cache::LoopRegion,
    seed: FlowEnvelope,
) -> Result<FlowEnvelope> {
    let mut current = seed;
    let mut iterations: u32 = 0;
    let mut truncated = false;
    // Runtime budget override: agent_context stamps `meta.loop_max_iterations`
    // from the agent's per-definition `max_iterations`, so a single seeded
    // region serves agents with different budgets. It overrides the compile-time
    // region budget but is still clamped to the same hard cap (parity with the
    // legacy loop block's resolution).
    let max_iterations = current
        .meta
        .get("loop_max_iterations")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .map(|n| (n as u32).min(crate::flow_engine::cache::LOOP_REGION_MAX_ITERATIONS_CAP))
        .unwrap_or(region.max_iterations);
    let exit_reason: &str = loop {
        // Cancel / deadline are checked before each iteration so a long agent
        // loop honours a client disconnect or the flow deadline without waiting
        // for the iteration body to finish (parity with loop_block.rs).
        if ctx.cancel_token.is_cancelled() {
            break "cancelled";
        }
        if ctx
            .effective_deadline()
            .is_some_and(|d| Instant::now() >= d)
        {
            break "cancelled";
        }
        if iterations >= max_iterations {
            break "max_iterations";
        }

        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationStarted {
                node_id: region.id.clone(),
                n: iterations + 1,
                max: max_iterations,
            },
        );
        current = execute_subdag(compiled, adapters, ctx, region, current).await?;
        iterations += 1;
        // Lepkosc uciecia: znacznik jedzie w envelope, wiec przezywa kolejne
        // iteracje sam z siebie — ale trzymamy go OBOK, zeby zadna zmiana w ciele
        // petli nie mogla go po cichu zgubic. Wynik tury nie moze twierdzic, ze
        // wszystko sie udalo, jesli ktorykolwiek krok zostal uciety w polowie.
        truncated |= current
            .meta
            .get(crate::flow_engine::cache::LLM_TRUNCATED_META)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationFinished {
                node_id: region.id.clone(),
                n: iterations,
            },
        );

        if region.gated {
            // A gated region spins over deterministic work (delegate → wait →
            // judge), where no assistant turn carries tool calls, so the tool
            // -loop stop would end it after one pass. The `critic_gate` block
            // inside decides instead, and it is visible and deletable in the
            // Flow Builder: delete it and the region falls back to the rule
            // below.
            let satisfied = current
                .meta
                .get(crate::flow_engine::cache::LOOP_SHOULD_EXIT_META)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if satisfied {
                break "gate_satisfied";
            }
        } else if !last_assistant_has_tool_calls(&current) {
            // Structural stop: a final assistant turn WITHOUT tool calls means
            // the agent answered and the loop is done. This replaces
            // meta.harness_done.
            break "no_tool_calls";
        }
    };

    // Grace summary: one extra iteration with loop_final_pass=true after the
    // budget is exhausted, so the body's llm block drops tools and produces a
    // final answer instead of another tool call. Only on max_iterations, and
    // only when cancel/deadline have not already fired.
    if region.final_pass
        && exit_reason == "max_iterations"
        && !ctx.cancel_token.is_cancelled()
        && ctx.effective_deadline().is_none_or(|d| Instant::now() < d)
    {
        current
            .meta
            .insert("loop_final_pass".into(), serde_json::Value::Bool(true));
        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationStarted {
                node_id: region.id.clone(),
                n: iterations + 1,
                max: max_iterations,
            },
        );
        current = execute_subdag(compiled, adapters, ctx, region, current).await?;
        iterations += 1;
        current.meta.remove("loop_final_pass");
        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationFinished {
                node_id: region.id.clone(),
                n: iterations,
            },
        );
        if ctx.cancel_token.is_cancelled()
            || ctx
                .effective_deadline()
                .is_some_and(|d| Instant::now() >= d)
        {
            return Err(anyhow!(
                "loop region '{}': cancelled during final pass after {iterations} iteration(s)",
                region.id
            ));
        }
    }

    current.meta.insert(
        "loop_iterations".into(),
        serde_json::Value::from(iterations),
    );
    current.meta.insert(
        "loop_exit_reason".into(),
        serde_json::Value::String(exit_reason.to_string()),
    );
    // Przepisujemy znacznik z NASZEJ lepkiej flagi, a nie zostawiamy go temu, co
    // przyniosla ostatnia iteracja: grace-pass `final_pass` wola model jeszcze raz
    // i jego envelope moze juz nie niesc uciecia z iteracji wczesniejszej.
    if truncated {
        current.meta.insert(
            crate::flow_engine::cache::LLM_TRUNCATED_META.to_string(),
            serde_json::Value::Bool(true),
        );
    }
    if exit_reason == "cancelled" {
        if matches!(current.payload, FlowValue::Empty) {
            current.payload = FlowValue::Text(String::new());
        }
        return Err(anyhow!(
            "loop region '{}': cancelled after {iterations} iteration(s)",
            region.id
        ));
    }
    Ok(current)
}

/// Runs the region's internal sub-DAG once over `seed`, returning the evolved
/// envelope. The members are in topological order (entry first, back edge
/// excluded), each non-output node has ≤1 internal input (R4), so a sequential
/// topo pass building inputs from already-computed members is correct. The
/// envelope threads through every member — conversation history accumulates in
/// place — and the exit member's output is the iteration result.
async fn execute_subdag(
    compiled: &Arc<CompiledFlow>,
    adapters: &Arc<AdapterRegistry>,
    ctx: &Arc<ExecutionContext>,
    region: &crate::flow_engine::cache::LoopRegion,
    seed: FlowEnvelope,
) -> Result<FlowEnvelope> {
    let member_set: HashSet<usize> = region.member_pos.iter().copied().collect();
    // Per-member output for this single sub-DAG pass. The entry consumes the
    // seed; every other member consumes its internal predecessor(s).
    let mut member_out: HashMap<usize, Arc<FlowEnvelope>> = HashMap::new();
    let seed_arc = Arc::new(seed);

    for &pos in &region.member_pos {
        let def_idx = compiled.execution_order[pos];
        let node = &compiled.definition.nodes[def_idx];
        let adapter = adapters.get(&node.node_type).ok_or_else(|| {
            anyhow!(
                "no adapter for node '{}' (type '{}')",
                node.id,
                node.node_type
            )
        })?;

        // Internal inputs from predecessor members (loop_back excluded). The
        // entry additionally takes the seed, which carries the accumulated
        // conversation from the previous iteration.
        let mut inputs: Vec<NodeInput> = Vec::new();
        for &edge_idx in &compiled.incoming_edges_per_pos[pos] {
            let edge = &compiled.definition.edges[edge_idx];
            if edge.is_loop_back() {
                continue;
            }
            let Some(&from_pos) = compiled.run_idx_by_id.get(edge.from.as_str()) else {
                continue;
            };
            if !member_set.contains(&from_pos) {
                continue;
            }
            if let Some(env) = member_out.get(&from_pos) {
                inputs.push(NodeInput {
                    from_node_id: edge.from.clone(),
                    from_port: edge.from_port.clone(),
                    envelope: env.clone(),
                });
            }
        }
        if pos == region.entry_pos {
            inputs.push(NodeInput {
                from_node_id: "__loop_region_seed__".to_string(),
                from_port: "full".to_string(),
                envelope: seed_arc.clone(),
            });
        }

        let inbound: &FlowEnvelope =
            io_mapping_inbound(&inputs).unwrap_or_else(|| seed_arc.as_ref());
        let result =
            run_node_with_io_mapping(adapter.as_ref(), node, inbound, &inputs, ctx).await?;
        member_out.insert(pos, Arc::new(result));
    }

    let exit_out = member_out
        .remove(&region.exit_pos)
        .ok_or_else(|| anyhow!("loop region '{}': exit node produced no output", region.id))?;
    Ok(Arc::try_unwrap(exit_out).unwrap_or_else(|arc| (*arc).clone()))
}

/// Streams an inline loop region (codex-style live token streaming). Behaves
/// exactly like `run_loop_region` for control flow (structural stop, iteration
/// budget, grace final pass, cancel/deadline) but, on each iteration, runs the
/// region's `llm` member through `produce_stream` and forwards its text /
/// reasoning deltas to `outbound` AS THEY ARRIVE — so the client sees every
/// turn's narration and the final answer token-by-token. The non-llm members
/// (`compact_context`, `tool_exec`) run blocking around the streamed llm step.
///
/// Returns the fully accumulated final envelope (one envelope threaded through
/// every iteration, conversation history grown in place) so the post-producer
/// finalizer can run `persist_turn` / `output` over the complete turn.
///
/// `outbound` carries only forward-progress deltas (text/reasoning + a final
/// finish marker). Tool-call deltas are reassembled internally into the
/// accumulated assistant message; they never go to the client as visible text.
#[allow(clippy::too_many_arguments)]
async fn run_loop_region_streaming(
    compiled: &Arc<CompiledFlow>,
    adapters: &Arc<AdapterRegistry>,
    ctx: &Arc<ExecutionContext>,
    region: &crate::flow_engine::cache::LoopRegion,
    seed: FlowEnvelope,
    outbound: &mpsc::Sender<Result<EnvelopeDelta>>,
    last_usage: &mut Option<TokenUsage>,
    last_perf: &mut Option<GenPerf>,
    last_finish: &mut Option<FinishReason>,
) -> Result<FlowEnvelope> {
    let mut current = seed;
    let mut iterations: u32 = 0;
    let max_iterations = current
        .meta
        .get("loop_max_iterations")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .map(|n| (n as u32).min(crate::flow_engine::cache::LOOP_REGION_MAX_ITERATIONS_CAP))
        .unwrap_or(region.max_iterations);

    let exit_reason: &str = loop {
        if ctx.cancel_token.is_cancelled() {
            break "cancelled";
        }
        if ctx
            .effective_deadline()
            .is_some_and(|d| Instant::now() >= d)
        {
            break "cancelled";
        }
        if iterations >= max_iterations {
            break "max_iterations";
        }

        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationStarted {
                node_id: region.id.clone(),
                n: iterations + 1,
                max: max_iterations,
            },
        );
        current = execute_subdag_streaming(
            compiled, adapters, ctx, region, current, outbound, last_usage, last_perf,
        )
        .await?;
        iterations += 1;
        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationFinished {
                node_id: region.id.clone(),
                n: iterations,
            },
        );

        if !last_assistant_has_tool_calls(&current) {
            break "no_tool_calls";
        }
    };

    if region.final_pass
        && exit_reason == "max_iterations"
        && !ctx.cancel_token.is_cancelled()
        && ctx.effective_deadline().is_none_or(|d| Instant::now() < d)
    {
        current
            .meta
            .insert("loop_final_pass".into(), serde_json::Value::Bool(true));
        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationStarted {
                node_id: region.id.clone(),
                n: iterations + 1,
                max: max_iterations,
            },
        );
        current = execute_subdag_streaming(
            compiled, adapters, ctx, region, current, outbound, last_usage, last_perf,
        )
        .await?;
        iterations += 1;
        current.meta.remove("loop_final_pass");
        ctx.progress.emit(
            &ctx.progress_scope,
            crate::flow_engine::dispatchers::ProgressEvent::IterationFinished {
                node_id: region.id.clone(),
                n: iterations,
            },
        );
        if ctx.cancel_token.is_cancelled()
            || ctx
                .effective_deadline()
                .is_some_and(|d| Instant::now() >= d)
        {
            return Err(anyhow!(
                "loop region '{}': cancelled during final pass after {iterations} iteration(s)",
                region.id
            ));
        }
    }

    current.meta.insert(
        "loop_iterations".into(),
        serde_json::Value::from(iterations),
    );
    current.meta.insert(
        "loop_exit_reason".into(),
        serde_json::Value::String(exit_reason.to_string()),
    );
    if exit_reason == "cancelled" {
        return Err(anyhow!(
            "loop region '{}': cancelled after {iterations} iteration(s)",
            region.id
        ));
    }
    // The final iteration ended without tool calls (structural stop) or the
    // budget summary closed it; the agent's final answer is in place. Surface a
    // Stop finish for the client trailer.
    *last_finish = Some(FinishReason::Stop);
    Ok(current)
}

/// One streaming pass over the region's internal sub-DAG. Mirrors
/// `execute_subdag` (sequential topo pass, ≤1 internal input per non-output
/// member, R4) but the single `llm` member runs through `produce_stream`: its
/// text / reasoning deltas are forwarded to `outbound` live and the full
/// assistant message (content + reassembled tool calls) is appended to the
/// threaded envelope — exactly what the blocking llm adapter's `execute` does,
/// so a tool_exec member downstream sees the same `tool_calls` it would in the
/// blocking region. Non-llm members run blocking.
async fn execute_subdag_streaming(
    compiled: &Arc<CompiledFlow>,
    adapters: &Arc<AdapterRegistry>,
    ctx: &Arc<ExecutionContext>,
    region: &crate::flow_engine::cache::LoopRegion,
    seed: FlowEnvelope,
    outbound: &mpsc::Sender<Result<EnvelopeDelta>>,
    last_usage: &mut Option<TokenUsage>,
    last_perf: &mut Option<GenPerf>,
) -> Result<FlowEnvelope> {
    let member_set: HashSet<usize> = region.member_pos.iter().copied().collect();
    let mut member_out: HashMap<usize, Arc<FlowEnvelope>> = HashMap::new();
    let seed_arc = Arc::new(seed);

    for &pos in &region.member_pos {
        let def_idx = compiled.execution_order[pos];
        let node = &compiled.definition.nodes[def_idx];

        let mut inputs: Vec<NodeInput> = Vec::new();
        for &edge_idx in &compiled.incoming_edges_per_pos[pos] {
            let edge = &compiled.definition.edges[edge_idx];
            if edge.is_loop_back() {
                continue;
            }
            let Some(&from_pos) = compiled.run_idx_by_id.get(edge.from.as_str()) else {
                continue;
            };
            if !member_set.contains(&from_pos) {
                continue;
            }
            if let Some(env) = member_out.get(&from_pos) {
                inputs.push(NodeInput {
                    from_node_id: edge.from.clone(),
                    from_port: edge.from_port.clone(),
                    envelope: env.clone(),
                });
            }
        }
        if pos == region.entry_pos {
            inputs.push(NodeInput {
                from_node_id: "__loop_region_seed__".to_string(),
                from_port: "full".to_string(),
                envelope: seed_arc.clone(),
            });
        }

        // The `llm` member streams; every other member runs blocking. io-mapping
        // never overlays a stream producer (R7), so the streamed member takes the
        // raw-config produce_stream path while the rest keep the io-mapping seam.
        let result = if adapters.is_stream_producer(&node.node_type) {
            stream_llm_member(
                adapters, node, &inputs, ctx, outbound, last_usage, last_perf,
            )
            .await?
        } else {
            let inbound: &FlowEnvelope =
                io_mapping_inbound(&inputs).unwrap_or_else(|| seed_arc.as_ref());
            let adapter = adapters.get(&node.node_type).ok_or_else(|| {
                anyhow!(
                    "no adapter for node '{}' (type '{}')",
                    node.id,
                    node.node_type
                )
            })?;
            run_node_with_io_mapping(adapter.as_ref(), node, inbound, &inputs, ctx).await?
        };
        member_out.insert(pos, Arc::new(result));
    }

    let exit_out = member_out
        .remove(&region.exit_pos)
        .ok_or_else(|| anyhow!("loop region '{}': exit node produced no output", region.id))?;
    Ok(Arc::try_unwrap(exit_out).unwrap_or_else(|arc| (*arc).clone()))
}

/// Runs the region's `llm` member as a live stream: drives `produce_stream`,
/// forwards text / reasoning deltas to the client, and reassembles the full
/// assistant turn (content + tool calls) into the output envelope — the same
/// shape `LlmNodeAdapter::execute` would append, so downstream `tool_exec` and
/// the structural stop see identical `context.messages`. A backend error in the
/// stream is forwarded to the client and surfaced as a node error so the region
/// (and the flow) abort, matching the blocking path's error propagation.
async fn stream_llm_member(
    adapters: &Arc<AdapterRegistry>,
    node: &FlowNode,
    inputs: &[NodeInput],
    ctx: &Arc<ExecutionContext>,
    outbound: &mpsc::Sender<Result<EnvelopeDelta>>,
    last_usage: &mut Option<TokenUsage>,
    last_perf: &mut Option<GenPerf>,
) -> Result<FlowEnvelope> {
    let producer = adapters.stream_producer(&node.node_type).ok_or_else(|| {
        anyhow!(
            "no StreamProducerAdapter for region member '{}' (type '{}')",
            node.id,
            node.node_type
        )
    })?;
    let mut stream = producer.produce_stream(node, inputs, ctx).await?;

    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls = ToolCallAccumulator::new();
    let mut finish_reason: Option<FinishReason> = None;
    // Per-CALL flag, so the harness gets per-STEP semantics: this function runs
    // once per loop-region iteration, and TTFT is measured against the
    // request that started THAT step, not against the whole run.
    let mut first_token_emitted = false;

    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(EnvelopeDelta::Llm(c)) => c,
            Ok(EnvelopeDelta::Audio(_)) => {
                // The region's llm member produces text deltas; an audio delta
                // here would be a bug in the producer, not a recoverable case.
                return Err(anyhow!(
                    "region llm member '{}' produced an audio delta",
                    node.id
                ));
            }
            Err(e) => {
                let msg = format!("{e}");
                let _ = outbound.send(Err(anyhow!("{msg}"))).await;
                return Err(anyhow!(
                    "region llm member '{}' stream error: {msg}",
                    node.id
                ));
            }
        };

        if !first_token_emitted && chunk_carries_first_token(&chunk) {
            first_token_emitted = true;
            ctx.progress.emit(
                &ctx.progress_scope,
                crate::flow_engine::dispatchers::ProgressEvent::FirstToken {
                    node_id: node.id.clone(),
                },
            );
        }

        if !content.is_empty() || !chunk.text_delta.is_empty() {
            content.push_str(&chunk.text_delta);
        }
        if let Some(reasoning) = &chunk.reasoning_delta {
            reasoning_content.push_str(reasoning);
        }
        tool_calls.absorb(&chunk.tool_calls);
        if let Some(u) = chunk.usage.as_ref() {
            *last_usage = Some(*u);
            ctx.usage_sink.record(&node.id, *u);
        }
        if let Some(p) = chunk.perf {
            *last_perf = Some(p);
        }
        if let Some(fr) = chunk.finish_reason {
            finish_reason = Some(fr);
        }

        // Forward only visible narration (text/reasoning); tool-call deltas stay
        // internal. The finish marker is emitted once, by the finalizer's own
        // trailer, so intermediate iteration finishes are suppressed here.
        if !chunk.text_delta.is_empty() || chunk.reasoning_delta.is_some() {
            let forwarded = EnvelopeDelta::Llm(crate::flow_engine::envelope::LlmStreamChunk {
                choice_index: chunk.choice_index,
                text_delta: chunk.text_delta,
                reasoning_delta: chunk.reasoning_delta,
                tool_calls: Vec::new(),
                usage: None,
                perf: None,
                finish_reason: None,
                error: None,
            });
            if outbound.send(Ok(forwarded)).await.is_err() {
                // Client disconnected; stop forwarding but let the iteration body
                // finish so the region's accumulation stays consistent. The next
                // cancel/deadline check in the loop ends the run.
                break;
            }
        }
    }
    let _ = finish_reason;

    // Build the assistant turn identical to the blocking llm adapter: clone the
    // input envelope, set the text payload, append the assistant message with
    // any reassembled tool calls.
    let mut out: FlowEnvelope = inputs
        .first()
        .map(|i| (*i.envelope).clone())
        .unwrap_or_else(|| (*ctx.initial_envelope).clone());
    // Same contract as the blocking adapter: the prompt lives only in the
    // payload, which the next line overwrites with the answer. Without
    // persisting it the harness loop loses the user's request after the first
    // iteration (`LlmNodeAdapter::payload_user_message`).
    if let Some(user) = crate::flow_engine::node_adapters::llm::LlmNodeAdapter::payload_user_message(
        &out,
        &out.context.messages,
    ) {
        out.context.messages.push(user);
    }
    out.payload = FlowValue::Text(content.clone());
    let mut assistant = ChatMessage::assistant(content);
    assistant.reasoning_content = (!reasoning_content.is_empty()).then_some(reasoning_content);
    // Streaming reassembles arguments from deltas, so a stream cut by the token
    // budget ends mid-string exactly like the blocking path — and poisons the
    // history the same way (`LlmNodeAdapter::sanitize_tool_calls`).
    let (calls, note) = crate::flow_engine::node_adapters::llm::LlmNodeAdapter::sanitize_tool_calls(
        tool_calls.finish(),
        // A stream that ended without saying why is treated as complete; only
        // an explicit `length` marks the cut that truncates arguments.
        finish_reason.unwrap_or(FinishReason::Stop),
    );
    if !calls.is_empty() {
        assistant.tool_calls = Some(calls);
    }
    out.context.messages.push(assistant);
    if let Some(note) = note {
        tracing::warn!("{note}");
        out.context.messages.push(ChatMessage::user(note));
    }
    Ok(out)
}

/// Reassembles streamed `ToolCallDelta` fragments into complete `LlmToolCall`s.
/// id/name open a slot; argument text accumulates per `index` (OpenAI tool-call
/// streaming shape). Bounded so a forged index cannot allocate unboundedly.
struct ToolCallAccumulator {
    slots: Vec<(String, String, String)>,
}

impl ToolCallAccumulator {
    const MAX_SLOTS: usize = 256;

    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    fn absorb(&mut self, deltas: &[crate::flow_engine::envelope::ToolCallDelta]) {
        for delta in deltas {
            let idx = delta.index as usize;
            if idx >= Self::MAX_SLOTS {
                continue;
            }
            while self.slots.len() <= idx {
                self.slots
                    .push((String::new(), String::new(), String::new()));
            }
            let slot = &mut self.slots[idx];
            if let Some(id) = &delta.id {
                slot.0 = id.clone();
            }
            if let Some(name) = &delta.function_name {
                slot.1.push_str(name);
            }
            if let Some(args) = &delta.arguments_delta {
                slot.2.push_str(args);
            }
        }
    }

    fn finish(self) -> Vec<crate::flow_engine::envelope::LlmToolCall> {
        self.slots
            .into_iter()
            .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
            .map(
                |(id, name, arguments)| crate::flow_engine::envelope::LlmToolCall {
                    id,
                    name,
                    arguments,
                },
            )
            .collect()
    }
}

/// True when the conversation's last assistant message requested tool calls.
/// The inline loop continues while the agent is still calling tools and stops
/// when it returns a final answer (assistant turn with no tool calls).
fn last_assistant_has_tool_calls(envelope: &FlowEnvelope) -> bool {
    envelope
        .context
        .messages
        .iter()
        .rev()
        .find(|m| m.role == crate::flow_engine::envelope::ChatRole::Assistant)
        .and_then(|m| m.tool_calls.as_ref())
        .is_some_and(|calls| !calls.is_empty())
}

/// Timeout pojedynczego node'a-liścia (np. jeden call LLM / extraction / embeddings).
/// Budżet jest PER-CALL, nie globalny dla całego flow: jeden chat/extraction może
/// trwać do 600 s. W pętli każda iteracja to świeży node LLM, więc każda dostaje
/// własne 600 s — nie ma globalnej ściany czasu, w którą musi zmieścić się suma
/// wszystkich wywołań.
const NODE_TIMEOUT_SECS: u64 = 600;

/// Hard ceiling on a budget a block declares for itself. `timeout_secs` is
/// operator input, and without an upper bound a typo parks an executor slot
/// for years instead of failing.
const MAX_NODE_TIMEOUT_SECS: u64 = 86_400;

/// Wall-clock budget of one leaf node.
///
/// `NODE_TIMEOUT_SECS` is a FLOOR, not a ceiling. Blocks that own a long,
/// bounded wait declare it in their own config — `delegate_cli` drives a whole
/// vendor turn, `ask_user` and `patch_review` wait for a person, `exec_command`
/// runs a build — and a node killed at 600 s while its own limit said 900
/// reports a timeout that was never the configured one. The floor stays because
/// it is what protects every node that declares nothing.
///
/// It is `max`, not "the declared value when present", because for some blocks
/// `timeout_secs` bounds an INNER operation (the command, the question) and the
/// block still has to settle its run, reap a child and write its rows
/// afterwards. Taking the inner budget as the outer wall would abort exactly
/// during that cleanup; a floor can only ever lengthen the wall.
fn node_timeout(node: &FlowNode) -> std::time::Duration {
    let declared = node
        .config
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(MAX_NODE_TIMEOUT_SECS);
    std::time::Duration::from_secs(declared.max(NODE_TIMEOUT_SECS))
}

/// Node'y-kontenery, które ORKIESTRUJĄ wiele iteracji/podflow. Ich wewnętrzne
/// nody-liście są już indywidualnie limitowane przez `NODE_TIMEOUT_SECS`, więc
/// nałożenie 600 s na sam kontener błędnie ograniczyłoby CAŁĄ pętlę do 600 s
/// łącznie. Te typy są zwolnione z per-node timeoutu (ich czas trwania jest
/// ograniczony przez `max_iterations` + per-node budżet ich wewnętrznych nodów).
fn is_container_node_type(node_type: &str) -> bool {
    matches!(node_type, "loop" | "subflow" | "map" | "spawn")
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
    // Containers (loop/subflow/map/spawn) drive many inner nodes, each with its
    // own budget — bounding the container here would kill the whole loop at one
    // node's budget instead of at `max_iterations`.
    if is_container_node_type(node.node_type.as_str()) {
        return run_node_io_inner(adapter, node, inbound, inputs, ctx).await;
    }

    let node_id = node.id.clone();
    let budget = node_timeout(node);
    match tokio::time::timeout(
        budget,
        run_node_io_inner(adapter, node, inbound, inputs, ctx),
    )
    .await
    {
        Ok(res) => res,
        Err(_) => Err(anyhow!(
            "node '{node_id}' timeout after {}s",
            budget.as_secs()
        )),
    }
}

/// Wewnętrzna ścieżka io-mapping bez wrapu timeoutu (timeout nakłada wołający).
async fn run_node_io_inner(
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

/// Streaming execution. Wykonuje pre-producer nody w toposorcie, napędza
/// producenta strumienia (LLM, harness loop/subflow, addon stream block lub
/// inline loop region — codex-style live token streaming), spawnuje finalizer i
/// zwraca StreamingExecution natychmiast.
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

    // An inline loop region wired as the stream producer (its exit emits a
    // `from_port="stream"` edge) streams every iteration's narration and the
    // final answer token-by-token. The region runner drives the iterations
    // inline and the post-producer blocking nodes (`persist_turn`, `output`) run
    // over the fully accumulated turn once the stream settles.
    if compiled.stream_producer_region(adapters.as_ref()).is_some() {
        return execute_streaming_region(db, compiled, initial_arc, ctx, adapters, started).await;
    }

    let execution_id = create_execution_record(&db, &compiled.flow_id, &ctx).await?;
    ctx.execution_id = execution_id;

    let producer_run_idx = compiled
        .stream_producer_run_idx(adapters.as_ref())
        .ok_or_else(|| anyhow!("execute_streaming called on non-streaming flow"))?;
    let producer_def_idx = compiled.execution_order[producer_run_idx];
    let producer_node = &compiled.definition.nodes[producer_def_idx];

    let n = compiled.execution_order.len();
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    // Bramkowanie gałęzi (§3.11 A) jak w `execute_blocking`: aktywne porty per
    // rozwiązany node (None = wszystkie, Some(pusty) = Skipped). Topo loop
    // gwarantuje, że poprzednicy są rozwiązani przed następnikiem.
    let mut active_by_pos: Vec<Option<HashSet<String>>> = vec![None; n];
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
        // Node ze wszystkimi krawędziami wejściowymi nieaktywnymi (np. gałąź
        // STT przy payloadzie Text) jest Skipped — nie wykonuje się, a jego
        // pusty zbiór aktywnych portów propaguje skip na następniki.
        if !node_has_live_input(&compiled, run_idx, &outputs, &active_by_pos) {
            emit_node_started(&ctx, &node.id, &node.node_type);
            emit_node_finished(&ctx, &node.id, &TraceStatus::Skipped);
            trace.push(TraceStep {
                node_id: node.id.clone(),
                node_type: node.node_type.clone(),
                started_at_ms: ctx.clock.now_ms(),
                duration_ms: 0,
                status: TraceStatus::Skipped,
                usage: None,
            });
            active_by_pos[run_idx] = Some(HashSet::new());
            continue;
        }
        let inputs = build_inputs(&compiled, run_idx, &outputs, &active_by_pos);
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
                // `context` keeps the adapter's typed error (e.g. "no STT
                // service") downcastable for `DispatchError::from`.
                e.context(format!("pre-producer node '{}' failed", node.id))
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
        active_by_pos[run_idx] =
            compute_active_ports(&compiled, adapters.as_ref(), run_idx, &envelope);
        outputs[run_idx] = Some(Arc::new(envelope));
    }

    // §3.11 B — streaming dispatch via the generalized stream producer slot
    // (LLM is one such producer). The producer builds the EnvelopeDelta stream;
    // the executor no longer assumes the LLM-only path. The producer config is
    // passed raw (no io-mapping overlay): R7 rejects io-mapping on a stream
    // producer at validation precisely because this path cannot apply it, so
    // blocking and streaming dispatch never diverge on the same saved flow.
    if !node_has_live_input(&compiled, producer_run_idx, &outputs, &active_by_pos) {
        return Err(anyhow!(
            "stream producer '{}' has no live inputs (all upstream branches skipped)",
            producer_node.id
        ));
    }
    let producer_inputs = build_inputs(&compiled, producer_run_idx, &outputs, &active_by_pos);
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
            usage_sink: ctx.usage_sink.clone(),
            producer_step_started,
            producer_node_id,
            producer_node_type,
            producer_input_envelope: producer_input_envelope.clone(),
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
        producer_input: producer_input_envelope,
    })
}

/// Streaming dispatch for a flow whose stream producer is an inline loop region.
/// Pre-producer nodes (positions before the region entry) run blocking in topo
/// order; the region then streams every iteration's narration + final answer to
/// the client live; once the stream settles, the post-producer blocking nodes
/// (`persist_turn`, `output`) run over the fully accumulated turn so the durable
/// history and outcome reflect the complete conversation. The whole tail runs in
/// a spawned task so `StreamingExecution` returns immediately.
async fn execute_streaming_region(
    db: DbPool,
    compiled: Arc<CompiledFlow>,
    initial_arc: Arc<FlowEnvelope>,
    mut ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
    started: Instant,
) -> Result<StreamingExecution> {
    let execution_id = create_execution_record(&db, &compiled.flow_id, &ctx).await?;
    ctx.execution_id = execution_id;

    let region = compiled
        .stream_producer_region(adapters.as_ref())
        .ok_or_else(|| anyhow!("execute_streaming_region called on a non-region flow"))?
        .clone();
    let producer_run_idx = region.entry_pos;

    let n = compiled.execution_order.len();
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    let mut active_by_pos: Vec<Option<HashSet<String>>> = vec![None; n];
    let mut trace: Vec<TraceStep> = Vec::with_capacity(n);

    // Pre-producer topo pass (conversation_history, agent_context, …). Mirrors
    // the generic streaming path: skip-gating, io-mapping seam, cancel/deadline.
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
        if !node_has_live_input(&compiled, run_idx, &outputs, &active_by_pos) {
            emit_node_started(&ctx, &node.id, &node.node_type);
            emit_node_finished(&ctx, &node.id, &TraceStatus::Skipped);
            trace.push(TraceStep {
                node_id: node.id.clone(),
                node_type: node.node_type.clone(),
                started_at_ms: ctx.clock.now_ms(),
                duration_ms: 0,
                status: TraceStatus::Skipped,
                usage: None,
            });
            active_by_pos[run_idx] = Some(HashSet::new());
            continue;
        }
        let inputs = build_inputs(&compiled, run_idx, &outputs, &active_by_pos);
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
        active_by_pos[run_idx] =
            compute_active_ports(&compiled, adapters.as_ref(), run_idx, &envelope);
        outputs[run_idx] = Some(Arc::new(envelope));
    }

    // Region seed = the region's single external input (entry inputs), falling
    // back to the flow's initial envelope.
    let seed_inputs = build_inputs(&compiled, producer_run_idx, &outputs, &active_by_pos);
    let seed: FlowEnvelope = seed_inputs
        .first()
        .map(|i| (*i.envelope).clone())
        .unwrap_or_else(|| (*initial_arc).clone());
    let producer_input = Arc::new(seed.clone());

    let entry_def_idx = compiled.execution_order[region.entry_pos];
    let producer_node_id = compiled.definition.nodes[entry_def_idx].id.clone();
    let producer_node_type = compiled.definition.nodes[entry_def_idx].node_type.clone();
    let producer_step_started = ctx.clock.now_ms();
    emit_node_started(&ctx, &producer_node_id, &producer_node_type);

    let ctx = Arc::new(ctx);
    let (outbound_tx, outbound_rx) = mpsc::channel::<Result<EnvelopeDelta>>(64);
    let (outcome_tx, outcome_rx) = oneshot::channel::<FlowExecutionOutcome>();

    tokio::spawn(run_region_stream_finalizer(RegionFinalizerInputs {
        execution_id,
        compiled,
        adapters,
        ctx,
        region,
        seed,
        outputs,
        active_by_pos,
        trace,
        outbound_tx,
        outcome_tx,
        db,
        started,
        producer_run_idx,
        producer_node_id,
        producer_node_type,
        producer_step_started,
    }));

    let stream = futures::stream::unfold(outbound_rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let stream: BoxStream<'static, Result<EnvelopeDelta>> = Box::pin(stream);
    Ok(StreamingExecution {
        stream,
        outcome: outcome_rx,
        producer_input,
    })
}

struct RegionFinalizerInputs {
    execution_id: i64,
    compiled: Arc<CompiledFlow>,
    adapters: Arc<AdapterRegistry>,
    ctx: Arc<ExecutionContext>,
    region: crate::flow_engine::cache::LoopRegion,
    seed: FlowEnvelope,
    outputs: Vec<Option<Arc<FlowEnvelope>>>,
    active_by_pos: Vec<Option<HashSet<String>>>,
    trace: Vec<TraceStep>,
    outbound_tx: mpsc::Sender<Result<EnvelopeDelta>>,
    outcome_tx: oneshot::Sender<FlowExecutionOutcome>,
    db: DbPool,
    started: Instant,
    producer_run_idx: usize,
    producer_node_id: String,
    producer_node_type: String,
    producer_step_started: u64,
}

/// Drives the region stream, runs post-producer blocking nodes over the
/// accumulated turn, then builds + persists the outcome. Region iterations
/// stream their narration live (forwarded through `outbound_tx`); the loop's
/// final accumulated envelope (full `context.messages`) feeds `persist_turn` and
/// `output` so the durable history and the outcome reflect the complete turn.
async fn run_region_stream_finalizer(inputs: RegionFinalizerInputs) {
    let RegionFinalizerInputs {
        execution_id,
        compiled,
        adapters,
        ctx,
        region,
        seed,
        mut outputs,
        mut active_by_pos,
        mut trace,
        outbound_tx,
        outcome_tx,
        db,
        started,
        producer_run_idx,
        producer_node_id,
        producer_node_type,
        producer_step_started,
    } = inputs;

    let producer_attempt = Instant::now();
    let mut last_usage: Option<TokenUsage> = None;
    let mut last_perf: Option<GenPerf> = None;
    let mut last_finish: Option<FinishReason> = None;

    let region_result = run_loop_region_streaming(
        &compiled,
        &adapters,
        &ctx,
        &region,
        seed,
        &outbound_tx,
        &mut last_usage,
        &mut last_perf,
        &mut last_finish,
    )
    .await;

    let producer_duration_ms = producer_attempt.elapsed().as_millis() as u64;

    let final_envelope = match region_result {
        Ok(env) => {
            emit_node_finished(&ctx, &producer_node_id, &TraceStatus::Ok);
            trace.push(TraceStep {
                node_id: producer_node_id.clone(),
                node_type: producer_node_type.clone(),
                started_at_ms: producer_step_started,
                duration_ms: producer_duration_ms,
                status: TraceStatus::Ok,
                usage: last_usage.filter(|u| *u != TokenUsage::default()),
            });
            env
        }
        Err(e) => {
            let msg = e.to_string();
            emit_node_finished(
                &ctx,
                &producer_node_id,
                &TraceStatus::Error {
                    message: msg.clone(),
                },
            );
            trace.push(TraceStep {
                node_id: producer_node_id.clone(),
                node_type: producer_node_type.clone(),
                started_at_ms: producer_step_started,
                duration_ms: producer_duration_ms,
                status: TraceStatus::Error {
                    message: msg.clone(),
                },
                usage: last_usage.filter(|u| *u != TokenUsage::default()),
            });
            // The region already forwarded any backend error to the client; the
            // outcome carries it for the audit row. No post-producer nodes run.
            let cancelled = ctx.cancel_token.is_cancelled();
            let finish = if cancelled {
                FinishReason::Cancelled
            } else {
                FinishReason::Error
            };
            let outcome = FlowExecutionOutcome {
                final_envelope: (*ctx.initial_envelope).clone(),
                trace,
                usage: TokenUsage::default(),
                model: ctx.usage_sink.model(),
                perf: last_perf,
                finish_reason: finish,
                total_latency_ms: started.elapsed().as_millis() as i64,
                error: Some(msg),
            };
            persist_execution(&db, execution_id, &outcome).await;
            let _ = outcome_tx.send(outcome);
            return;
        }
    };

    // The region's output is stored at its entry slot (the contracted producer
    // position), so post-producer nodes resolve their inputs through it exactly
    // as the blocking scheduler's contraction does.
    let final_arc = Arc::new(final_envelope);
    outputs[producer_run_idx] = Some(final_arc.clone());
    active_by_pos[producer_run_idx] =
        compute_active_ports(&compiled, adapters.as_ref(), producer_run_idx, &final_arc);

    // Run every post-producer blocking node (positions after the region, that
    // are not region-internal members and not the terminal `output` sink) over
    // the accumulated turn. `persist_turn` is the node that matters here; it
    // writes the durable turn delta. The `output` sink never runs as a node on
    // the streaming path — the stream IS the output.
    let n = compiled.execution_order.len();
    let mut post_error: Option<String> = None;
    for run_idx in (producer_run_idx + 1)..n {
        let def_idx = compiled.execution_order[run_idx];
        let node = &compiled.definition.nodes[def_idx];
        if node.node_type == "output" {
            continue;
        }
        if compiled.position_is_region_internal(run_idx) {
            continue;
        }
        if !node_has_live_input(&compiled, run_idx, &outputs, &active_by_pos) {
            active_by_pos[run_idx] = Some(HashSet::new());
            continue;
        }
        let inputs = build_inputs(&compiled, run_idx, &outputs, &active_by_pos);
        let adapter = match adapters.get(&node.node_type) {
            Some(a) => a,
            None => {
                post_error = Some(format!(
                    "no adapter for post-producer node '{}' (type '{}')",
                    node.id, node.node_type
                ));
                break;
            }
        };
        let step_started = ctx.clock.now_ms();
        let attempt = Instant::now();
        emit_node_started(&ctx, &node.id, &node.node_type);
        let inbound: &FlowEnvelope =
            io_mapping_inbound(&inputs).unwrap_or_else(|| ctx.initial_envelope.as_ref());
        match run_node_with_io_mapping(adapter.as_ref(), node, inbound, &inputs, &ctx).await {
            Ok(env) => {
                emit_node_finished(&ctx, &node.id, &TraceStatus::Ok);
                trace.push(TraceStep {
                    node_id: node.id.clone(),
                    node_type: node.node_type.clone(),
                    started_at_ms: step_started,
                    duration_ms: attempt.elapsed().as_millis() as u64,
                    status: TraceStatus::Ok,
                    usage: take_node_usage(&ctx, &node.id),
                });
                active_by_pos[run_idx] =
                    compute_active_ports(&compiled, adapters.as_ref(), run_idx, &env);
                outputs[run_idx] = Some(Arc::new(env));
            }
            Err(e) => {
                let msg = e.to_string();
                emit_node_finished(
                    &ctx,
                    &node.id,
                    &TraceStatus::Error {
                        message: msg.clone(),
                    },
                );
                trace.push(TraceStep {
                    node_id: node.id.clone(),
                    node_type: node.node_type.clone(),
                    started_at_ms: step_started,
                    duration_ms: attempt.elapsed().as_millis() as u64,
                    status: TraceStatus::Error {
                        message: msg.clone(),
                    },
                    usage: None,
                });
                post_error = Some(format!("post-producer node '{}' failed: {msg}", node.id));
                break;
            }
        }
    }

    // Trailer: one terminal finish marker so the client's stream settles with a
    // finish_reason (intermediate iteration finishes were suppressed).
    let finish_reason = if post_error.is_some() {
        FinishReason::Error
    } else {
        last_finish.unwrap_or(FinishReason::Stop)
    };
    let usage = last_usage.unwrap_or_default();
    let trailer = EnvelopeDelta::Llm(crate::flow_engine::envelope::LlmStreamChunk {
        choice_index: 0,
        text_delta: String::new(),
        reasoning_delta: None,
        tool_calls: Vec::new(),
        usage: Some(usage),
        perf: last_perf,
        finish_reason: Some(finish_reason),
        error: post_error.clone(),
    });
    let _ = outbound_tx.send(Ok(trailer)).await;
    drop(outbound_tx);

    trace.sort_by_key(|s| s.started_at_ms);
    let aggregate_usage = aggregate_usage(&trace);
    let outcome = FlowExecutionOutcome {
        final_envelope: (*final_arc).clone(),
        trace,
        usage: aggregate_usage,
        model: ctx.usage_sink.model(),
        perf: last_perf,
        finish_reason,
        total_latency_ms: started.elapsed().as_millis() as i64,
        error: post_error,
    };
    persist_execution(&db, execution_id, &outcome).await;
    let _ = outcome_tx.send(outcome);
}

struct FinalizerInputs {
    started: Instant,
    /// Shared with the executing flow: the finalizer settles the outcome, so it
    /// is the place that has to name the model the tokens were spent on.
    usage_sink: Arc<UsageSink>,
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
    let mut last_perf: Option<GenPerf> = None;
    // Stage 3d Krok 2c-2: audio path agregator. Audio chunki z chain
    // (np. tts_stream_bridge) — outcome.payload to Empty (klient
    // skonsumował bytes przez SSE), ale finish_reason agregowany
    // dla wire trailers.
    let mut last_audio_finish: Option<FinishReason> = None;
    let mut audio_chunks_emitted: usize = 0;
    // The plain streaming flow has exactly one producer step, so one flag for
    // the whole finalizer is the per-step scope here.
    let mut first_token_emitted = false;
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
                    if !first_token_emitted && chunk_carries_first_token(&c) {
                        first_token_emitted = true;
                        inputs.progress.emit(
                            &inputs.progress_scope,
                            crate::flow_engine::dispatchers::ProgressEvent::FirstToken {
                                node_id: inputs.producer_node_id.clone(),
                            },
                        );
                    }
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
                    if let Some(p) = c.perf {
                        last_perf = Some(p);
                    }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            cancelled = true;
                            break 'main;
                        }
                        send_res = outbound_tx.send(Ok(EnvelopeDelta::Llm(c))) => {
                            if send_res.is_err() {
                                // The consumer dropped the stream: nobody will
                                // read another delta, so cancel instead of
                                // grinding through the rest of the flow.
                                cancel.cancel();
                                cancelled = true;
                                break 'main;
                            }
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
                            if send_res.is_err() {
                                // The consumer dropped the stream: nobody will
                                // read another delta, so cancel instead of
                                // grinding through the rest of the flow.
                                cancel.cancel();
                                cancelled = true;
                                break 'main;
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    error = Some(format!("{e}"));
                    // Błąd MUSI dotrzeć do konsumenta strumienia — bez forwardu
                    // klient widzi czysty EOF z zerem chunków ("pusta odpowiedź")
                    // zamiast komunikatu błędu. Outcome i tak niesie error, ale
                    // outcome konsumuje tylko finalizer logging, nie wire.
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            cancelled = true;
                        }
                        send_res = outbound_tx.send(Err(e)) => {
                            let _ = send_res;
                        }
                    }
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
        // A turn that records an assistant must also record the user that
        // prompted it: the prompt lives only in the payload, overwritten on the
        // next line (`LlmNodeAdapter::payload_user_message`).
        if let Some(user) =
            crate::flow_engine::node_adapters::llm::LlmNodeAdapter::payload_user_message(
                &final_envelope,
                &final_envelope.context.messages,
            )
        {
            final_envelope.context.messages.push(user);
        }
        final_envelope.payload = FlowValue::Text(text_buf.clone());
        let mut assistant = ChatMessage::assistant(text_buf);
        assistant.reasoning_content = (!reasoning_buf.is_empty()).then_some(reasoning_buf);
        final_envelope.context.messages.push(assistant);
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
        model: inputs.usage_sink.model(),
        perf: last_perf,
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

/// Czy node ma ≥1 żywą krawędź wejściową (poprzednik z outputem i aktywnym
/// portem). Node bez żadnej krawędzi wejściowej (trigger) jest zawsze żywy.
/// Używane przez streaming topo loop — blocking scheduler liczy żywe krawędzie
/// inkrementalnie w `live_inputs`.
fn node_has_live_input(
    compiled: &CompiledFlow,
    run_idx: usize,
    outputs: &[Option<Arc<FlowEnvelope>>],
    active_ports: &[Option<HashSet<String>>],
) -> bool {
    let edges = &compiled.incoming_edges_per_pos[run_idx];
    if edges.is_empty() {
        return true;
    }
    edges.iter().any(|&edge_idx| {
        let edge = &compiled.definition.edges[edge_idx];
        let Some(raw_from) = compiled.run_idx_by_id.get(edge.from.as_str()).copied() else {
            return false;
        };
        // Contraction: an edge out of a region member (the exit) reads the
        // region's output, stored at the entry slot — same remap `build_inputs`
        // applies, so liveness and input resolution agree on a region producer.
        let from_pos = compiled.contracted_producer_pos(raw_from);
        if outputs.get(from_pos).map(|o| o.is_none()).unwrap_or(true) {
            return false;
        }
        match active_ports.get(from_pos).and_then(|p| p.as_ref()) {
            None => true,
            Some(set) => set.contains(&edge.from_port),
        }
    })
}

/// Buduje inputs node'a z outputów poprzedników. Krawędź wchodzi do inputs
/// tylko gdy poprzednik ma output ORAZ jej port źródłowy jest aktywny w
/// `active_ports[from_pos]` (§3.11 A) — bez tego filtra envelope z martwej
/// krawędzi (np. trigger.audio przy payloadzie Text) wyciekałby do fan-in
/// node'ów, bo producent (trigger) wykonał się i ma output.
fn build_inputs(
    compiled: &CompiledFlow,
    run_idx: usize,
    outputs: &[Option<Arc<FlowEnvelope>>],
    active_ports: &[Option<HashSet<String>>],
) -> Vec<NodeInput> {
    let edges = &compiled.incoming_edges_per_pos[run_idx];
    edges
        .iter()
        .filter_map(|&edge_idx| {
            let edge = &compiled.definition.edges[edge_idx];
            let raw_from = compiled.run_idx_by_id.get(edge.from.as_str()).copied()?;
            // Contraction: an edge out of a region member (the exit) reads the
            // region's output, which is stored at the entry slot — the same
            // owner remap the dependency graph applies.
            let from_pos = compiled.contracted_producer_pos(raw_from);
            let envelope = outputs.get(from_pos)?.clone()?;
            let is_live = match active_ports.get(from_pos).and_then(|p| p.as_ref()) {
                // None = wszystkie porty producenta aktywne (default adaptera).
                None => true,
                Some(set) => set.contains(&edge.from_port),
            };
            if !is_live {
                return None;
            }
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
///
/// The row is stamped with the run's provenance (§2.5) read off the CONTEXT
/// struct fields — `ctx.origin` / `ctx.actor_*` / `ctx.correlation_id`, minted
/// by the entry point after authorization. Never off `envelope.meta`, which any
/// node (including a WASM addon block deserializing a whole envelope from guest
/// memory) can rewrite: §3 invariant 1.
async fn create_execution_record(
    db: &DbPool,
    flow_id: &str,
    ctx: &ExecutionContext,
) -> Result<i64> {
    if flow_id.is_empty() || ctx.light {
        return Ok(0);
    }
    let pool = db.clone();
    let flow_id = flow_id.to_string();
    let request_id = ctx.request_id.clone();
    let parent_execution_id = ctx.parent_execution_id;
    let origin = ctx.origin.as_str();
    let actor_kind = ctx.actor_kind.as_str();
    let actor_id = ctx.actor_id.clone();
    let actor_user_id = ctx.actor_user_id.clone();
    let correlation_id = ctx.correlation_id.clone();
    let id = tokio::task::spawn_blocking(move || {
        repository::create_flow_execution(
            &pool,
            &NewFlowExecution {
                flow_id: &flow_id,
                request_id: &request_id,
                status: "running",
                parent_execution_id,
                origin,
                actor_kind,
                actor_id: actor_id.as_deref(),
                actor_user_id: actor_user_id.as_deref(),
                correlation_id: correlation_id.as_deref(),
            },
        )
    })
    .await??;
    Ok(id)
}

/// `envelope.variables` key a flow can write (via any node's existing
/// `output_mapping`, §3.12 — no new UI needed) to attach domain-specific
/// metadata about what the run analyzed to its `flow_executions` row
/// (migration v136, `result_metadata_json`). Deliberately NOT
/// `envelope.meta`: that channel is engine plumbing writable by any node
/// including a WASM addon deserializing a whole envelope from guest memory
/// (see `create_execution_record`'s provenance note), so it cannot carry data
/// meant to be trusted as this run's own reported result. `variables` is the
/// channel already documented as "user-facing flow variables"
/// (`flow_engine::envelope::FlowEnvelope::variables`).
/// Non-object/array values (string/number/bool) are still accepted — the
/// column is opaque JSON, not a schema.
pub const RESULT_METADATA_VARIABLE: &str = "result_metadata";

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
    // The model is only known once the run is over — `UsageSink` records it at
    // request-build time inside the LLM node. A flow that called no model keeps
    // it NULL rather than inheriting a routing hint it never used.
    let model = outcome.model.clone();
    // Same story as `model`: only known once the flow finished producing it,
    // so it is read off the final envelope, not `ctx`, and serialized to the
    // TEXT column verbatim.
    let result_metadata = outcome
        .final_envelope
        .variables
        .get(RESULT_METADATA_VARIABLE)
        .map(io_mapping::variable_to_json)
        .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| "null".into()));
    let _ = tokio::task::spawn_blocking(move || {
        repository::update_flow_execution(
            &pool,
            execution_id,
            status,
            model.as_deref(),
            Some(&log_json),
            Some(total_ms),
            Some(total_tokens),
            result_metadata.as_deref(),
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
    use crate::flow_engine::node_adapter::{test_support::stub_ctx, AdapterRegistry, PortSpec};
    use crate::flow_engine::node_adapters::{
        CombineNodeAdapter, LlmNodeAdapter, OutputNodeAdapter, PiiFilterNodeAdapter,
        TriggerNodeAdapter, TtsNodeAdapter,
    };
    use crate::flow_engine::types::FlowDataType;
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

    /// Adapter STT-like: jak realny SttNodeAdapter odrzuca payload inny niż
    /// Audio — wykonanie go z payloadem Text dowodzi braku bramkowania gałęzi.
    struct StrictSttAdapter;

    #[async_trait]
    impl crate::flow_engine::node_adapter::NodeAdapter for StrictSttAdapter {
        fn node_type(&self) -> &str {
            "strict_stt"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Audio)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("full", FlowDataType::Text)]
        }
        async fn execute(
            &self,
            _node: &crate::flow_engine::types::FlowNode,
            inputs: &[crate::flow_engine::envelope::NodeInput],
            _ctx: &crate::flow_engine::node_adapter::ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let input = inputs
                .first()
                .ok_or_else(|| anyhow::anyhow!("strict_stt: no input"))?;
            match &input.envelope.payload {
                FlowValue::Audio { .. } => Ok(FlowEnvelope::with_payload(FlowValue::Text(
                    "transcript".into(),
                ))),
                other => Err(anyhow::anyhow!(
                    "stt adapter: payload must be Audio, got {other:?}"
                )),
            }
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
            let conn = pool.write().expect("db lock");
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

    /// Trigger bramkuje pre-producer gałęzie także w streaming path: payload
    /// Text aktywuje tylko port `text`, więc gałąź audio (strict_stt — błąd
    /// przy nie-Audio payloadzie) jest Skipped zamiast wykonana, combine
    /// dostaje samo żywe wejście tekstowe, a LLM streamuje normalnie.
    #[tokio::test]
    async fn streaming_text_payload_skips_audio_branch_before_producer() {
        let mut r = registry_with_chain();
        r.register(Arc::new(CombineNodeAdapter::new()));
        r.register(Arc::new(StrictSttAdapter));
        let registry = Arc::new(r);
        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"stt","type":"strict_stt","config":{}},
                {"id":"c1","type":"combine","config":{}},
                {"id":"l1","type":"llm","config":{"model":"qwen3.5-0.8b"}},
                {"id":"o1","type":"output","config":{"mode":"stream"}}
            ],
            "edges":[
                {"from":"t1","to":"c1","from_port":"text","to_port":"in"},
                {"from":"t1","to":"stt","from_port":"audio","to_port":"in"},
                {"from":"stt","to":"c1","from_port":"full","to_port":"in"},
                {"from":"c1","to":"l1","from_port":"full"},
                {"from":"l1","to":"o1","from_port":"stream","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        let llm_chunks = vec![LlmStreamChunk {
            choice_index: 0,
            text_delta: "Answer.".into(),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }];
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(FakeStreamingLlm::new(llm_chunks));

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let exec = execute_streaming(fresh_db(), compiled, initial, ctx, registry)
            .await
            .expect("text payload must not execute the audio/STT branch");

        let mut stream = exec.stream;
        let mut concat = String::new();
        while let Some(item) = stream.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta ok") {
                concat.push_str(&c.text_delta);
            }
        }
        assert!(concat.contains("Answer."), "stream incomplete: {concat:?}");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        let stt_status = outcome
            .trace
            .iter()
            .find(|s| s.node_id == "stt")
            .map(|s| &s.status);
        assert_eq!(
            stt_status,
            Some(&crate::flow_engine::envelope::TraceStatus::Skipped),
            "audio branch must be Skipped in the streaming path: {:?}",
            outcome.trace
        );
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
        initial.set_output_audio(true);

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

    /// trigger(audio) → stt → llm(stream) → output: the stt node finishes before
    /// `execute_streaming` returns, so `producer_input` already carries
    /// `meta["stt_transcript"]` while the LLM stream is still running.
    #[tokio::test]
    async fn execute_streaming_exposes_stt_transcript_on_producer_input() {
        use crate::flow_engine::dispatchers::{SttDispatcher, SttRequest, SttResponse};
        use crate::flow_engine::node_adapters::SttNodeAdapter;

        struct FakeStt;
        #[async_trait]
        impl SttDispatcher for FakeStt {
            async fn transcribe(&self, _req: SttRequest) -> Result<SttResponse> {
                Ok(SttResponse {
                    text: "dzien dobry".into(),
                    detected_language: Some("pl".into()),
                    ..SttResponse::default()
                })
            }
        }

        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(SttNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));
        let registry = Arc::new(r);

        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"s1","type":"stt","config":{"model":"whisper"}},
                {"id":"l1","type":"llm","config":{"model":"qwen3.5-0.8b"}},
                {"id":"o1","type":"output","config":{"mode":"stream"}}
            ],
            "edges":[
                {"from":"t1","to":"s1","from_port":"audio","to_port":"in"},
                {"from":"s1","to":"l1","from_port":"full","to_port":"in"},
                {"from":"l1","to":"o1","from_port":"stream","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        let mut ctx = stub_ctx();
        ctx.stt = Arc::new(FakeStt);
        ctx.llm = Arc::new(FakeStreamingLlm::new(vec![LlmStreamChunk {
            choice_index: 0,
            text_delta: "ok".into(),
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }]));

        let initial = FlowEnvelope::with_payload(FlowValue::Audio {
            blob_ref: BlobRef {
                id: "blob1".into(),
                sha256: "x".into(),
                size_bytes: 4,
                mime: "audio/wav".into(),
            },
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
        });

        let exec = execute_streaming(fresh_db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");

        assert_eq!(
            exec.producer_input
                .meta
                .get("stt_transcript")
                .and_then(|v| v.as_str()),
            Some("dzien dobry")
        );
        assert_eq!(
            exec.producer_input
                .meta
                .get("language")
                .and_then(|v| v.as_str()),
            Some("pl")
        );

        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            item.expect("delta ok");
        }
        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// §3.11 B — execute_streaming drives ANY registered StreamProducerAdapter,
    /// not just the LLM slot. A non-LLM `TestStreamProducer` terminating at
    /// output(stream) streams its EnvelopeDelta chunks through to the client.
    #[tokio::test]
    async fn execute_streaming_with_non_llm_stream_producer() {
        use crate::flow_engine::node_adapter::test_support::{
            CapturingProgress, TestStreamProducer,
        };
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
    use crate::flow_engine::envelope::TraceStatus;
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
            let conn = pool.write().expect("db lock");
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

    /// Wall-clock budget of everything that is NOT the sleeping: compiling the
    /// graph, opening the db, scheduling. Measured rather than guessed, because
    /// a fixed threshold small enough to prove concurrency on a fast machine is
    /// a threshold that fails on a loaded one — which is exactly how the
    /// previous version of these two tests spent its life red without ever
    /// pointing at a real defect.
    async fn fanout_overhead(json_zero_sleep: &str) -> Duration {
        let start = Instant::now();
        let _ = run_with_db(json_zero_sleep, db()).await;
        start.elapsed()
    }

    #[tokio::test]
    async fn fanout_branches_run_concurrently_and_combine_waits() {
        // trigger → a + b → combine → output. Combine seeing both results is
        // the proof that it waited for the slower branch (a barrier); the
        // timing is the proof that the two did not queue behind each other.
        const SLEEP_MS: u64 = 400;
        let graph = |ms: u64| {
            format!(
                r#"{{
            "nodes":[
                {{"id":"t","type":"trigger","config":{{}}}},
                {{"id":"a","type":"sleep","config":{{"sleep_ms":{ms}}}}},
                {{"id":"b","type":"sleep","config":{{"sleep_ms":{ms}}}}},
                {{"id":"c","type":"combine","config":{{}}}},
                {{"id":"o","type":"output","config":{{}}}}
            ],
            "edges":[
                {{"from":"t","to":"a","from_port":"text","to_port":"in"}},
                {{"from":"t","to":"b","from_port":"text","to_port":"in"}},
                {{"from":"a","to":"c","from_port":"full","to_port":"in"}},
                {{"from":"b","to":"c","from_port":"full","to_port":"in"}},
                {{"from":"c","to":"o","from_port":"full","to_port":"text"}}
            ]
        }}"#
            )
        };

        let overhead = fanout_overhead(&graph(0)).await;
        let db = db();
        let start = Instant::now();
        let outcome = run_with_db(&graph(SLEEP_MS), db).await;
        let elapsed = start.elapsed();

        // Concurrent ≈ one sleep, sequential ≈ two. Half-way between them is the
        // only threshold that separates the two answers without also measuring
        // how busy the machine is.
        let sleeping = elapsed.saturating_sub(overhead);
        assert!(
            sleeping < Duration::from_millis(SLEEP_MS + SLEEP_MS / 2),
            "branches must run concurrently: {sleeping:?} of sleeping for two \
             {SLEEP_MS}ms branches (overhead {overhead:?}, total {elapsed:?})"
        );
        let text = outcome.final_envelope.payload.as_text().unwrap_or("");
        assert!(text.contains("a"), "combine missing branch a: {text:?}");
        assert!(text.contains("b"), "combine missing branch b: {text:?}");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn five_way_fanout_from_one_node_recombines() {
        // src → five independent branches → combine. Sequentially five sleeps,
        // concurrently one; the gap is what this measures.
        const SLEEP_MS: u64 = 300;
        let graph = |ms: u64| {
            let branches: Vec<String> = (1..=5)
                .map(|n| format!(r#"{{"id":"n{n}","type":"sleep","config":{{"sleep_ms":{ms}}}}}"#))
                .collect();
            let from_src: Vec<String> = (1..=5)
                .map(|n| {
                    format!(r#"{{"from":"src","to":"n{n}","from_port":"full","to_port":"in"}}"#)
                })
                .collect();
            let to_combine: Vec<String> = (1..=5)
                .map(|n| format!(r#"{{"from":"n{n}","to":"c","from_port":"full","to_port":"in"}}"#))
                .collect();
            format!(
                r#"{{
                    "nodes":[
                        {{"id":"t","type":"trigger","config":{{}}}},
                        {{"id":"src","type":"sleep","config":{{}}}},
                        {},
                        {{"id":"c","type":"combine","config":{{}}}},
                        {{"id":"o","type":"output","config":{{}}}}
                    ],
                    "edges":[
                        {{"from":"t","to":"src","from_port":"text","to_port":"in"}},
                        {},
                        {},
                        {{"from":"c","to":"o","from_port":"full","to_port":"text"}}
                    ]
                }}"#,
                branches.join(",\n                        "),
                from_src.join(",\n                        "),
                to_combine.join(",\n                        "),
            )
        };

        let overhead = fanout_overhead(&graph(0)).await;
        let db = db();
        let start = Instant::now();
        let outcome = run_with_db(&graph(SLEEP_MS), db).await;
        let elapsed = start.elapsed();

        let sleeping = elapsed.saturating_sub(overhead);
        assert!(
            sleeping < Duration::from_millis(SLEEP_MS * 2),
            "5 branches must run concurrently: {sleeping:?} of sleeping for five \
             {SLEEP_MS}ms branches (overhead {overhead:?}, total {elapsed:?})"
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
        assert_eq!(
            node_status(&outcome, "dead_in"),
            Some(&TraceStatus::Skipped)
        );
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

    /// Trigger bramkuje gałęzie po modalności payloadu (§3.11 A): payload Text
    /// aktywuje wyłącznie port `text` — gałąź `audio` (STT) jest Skipped, a
    /// combine wykonuje się z samym żywym wejściem tekstowym.
    #[tokio::test]
    async fn trigger_text_payload_skips_audio_branch() {
        // trigger.text → c(combine); trigger.audio → stt → c; c → o.
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"stt","type":"sleep","config":{}},
                {"id":"c","type":"combine","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"c","from_port":"text","to_port":"in"},
                {"from":"t","to":"stt","from_port":"audio","to_port":"in"},
                {"from":"stt","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let outcome = run_with_input(json, "hello").await;
        assert_eq!(
            node_status(&outcome, "stt"),
            Some(&TraceStatus::Skipped),
            "audio branch must be skipped for a Text payload: {:?}",
            outcome.trace
        );
        assert_eq!(node_status(&outcome, "c"), Some(&TraceStatus::Ok));
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("hello"));
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
    }

    /// Odwrotnie: payload Audio aktywuje wyłącznie port `audio` — gałąź `text`
    /// jest Skipped, a do combine NIE wycieka envelope martwej krawędzi
    /// trigger.text (combine widzi tylko transkrypt z gałęzi STT).
    #[tokio::test]
    async fn trigger_audio_payload_skips_text_branch() {
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"t_branch","type":"sleep","config":{}},
                {"id":"stt","type":"sleep","config":{}},
                {"id":"c","type":"combine","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"t_branch","from_port":"text","to_port":"in"},
                {"from":"t","to":"stt","from_port":"audio","to_port":"in"},
                {"from":"t_branch","to":"c","from_port":"full","to_port":"in"},
                {"from":"stt","to":"c","from_port":"full","to_port":"in"},
                {"from":"c","to":"o","from_port":"full","to_port":"text"}
            ]
        }"#;
        let reg = registry();
        let compiled = Arc::new(CompiledFlow::from_json("0", json, &reg).expect("compile"));
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Audio {
            blob_ref: crate::flow_engine::blob_store::BlobRef {
                id: "b1".into(),
                sha256: "deadbeef".into(),
                size_bytes: 4,
                mime: "audio/wav".into(),
            },
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
        };
        let outcome = execute_blocking(db(), compiled, initial, stub_ctx(), reg)
            .await
            .expect("exec");
        assert_eq!(
            node_status(&outcome, "t_branch"),
            Some(&TraceStatus::Skipped),
            "text branch must be skipped for an Audio payload: {:?}",
            outcome.trace
        );
        assert_eq!(node_status(&outcome, "stt"), Some(&TraceStatus::Ok));
        assert_eq!(node_status(&outcome, "c"), Some(&TraceStatus::Ok));
        // Sleep nadpisuje payload swoim id — combine widzi tylko gałąź STT.
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("stt"));
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
        assert_eq!(
            node_status(&outcome, "f_branch"),
            Some(&TraceStatus::Skipped)
        );
        assert_eq!(outcome.final_envelope.payload.as_text(), Some("t_branch"));

        // input="stop" ⇒ false branch runs, true branch Skipped.
        let outcome = run_with_input(json, "stop").await;
        assert_eq!(node_status(&outcome, "f_branch"), Some(&TraceStatus::Ok));
        assert_eq!(
            node_status(&outcome, "t_branch"),
            Some(&TraceStatus::Skipped)
        );
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
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn insert_flow(pool: &DbPool, id: &str, json: &str) {
        let conn = pool.write().unwrap();
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
        let compiled =
            Arc::new(CompiledFlow::from_json("0", &outer_json, &registry).expect("compile outer"));

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
        assert_eq!(
            outcome.final_envelope.payload.as_text(),
            Some("blocking iter 3")
        );
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
        let compiled =
            Arc::new(CompiledFlow::from_json("0", &outer_json, &registry).expect("compile outer"));

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
        assert!(
            text.contains("FINAL(iter=2)"),
            "grace pass did not stream: {text:?}"
        );
        assert!(saw_finish, "client never got finish_reason=Stop");

        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        assert_eq!(
            outcome.final_envelope.payload.as_text(),
            Some("FINAL(iter=2)")
        );
    }
}

#[cfg(test)]
mod loop_region_tests {
    //! Inline loop region: a marked sub-DAG executed inline over ONE evolving
    //! envelope by the executor, with a single `loop_back` edge closing the
    //! cycle. The region runner stops on a final assistant turn without tool
    //! calls, honours the iteration budget + cancel, and accumulates
    //! conversation history across iterations.
    use super::execute_blocking;
    use crate::db::DbPool;
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::envelope::{
        ChatMessage, ChatRole, FlowEnvelope, FlowExecutionOutcome, FlowValue, LlmToolCall,
        NodeInput,
    };
    use crate::flow_engine::node_adapter::{
        test_support::stub_ctx, AdapterRegistry, ExecutionContext, NodeAdapter, PortSpec,
    };
    use crate::flow_engine::node_adapters::{OutputNodeAdapter, TriggerNodeAdapter};
    use crate::flow_engine::types::{FlowDataType, FlowNode};
    use crate::flow_engine::validation::{validate, FlowValidationError};
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    /// Agent-iteration test double. Each run increments `meta.iter`, appends an
    /// assistant message, and — until `meta.iter` reaches `stop_at` — gives that
    /// assistant message a tool call so the loop keeps going. At `stop_at` the
    /// assistant message has NO tool calls (the agent's final answer), which is
    /// the region's structural stop signal. History accumulates in
    /// `context.messages`.
    struct AgentBodyAdapter {
        stop_at: i64,
    }

    #[async_trait]
    impl NodeAdapter for AgentBodyAdapter {
        fn node_type(&self) -> &str {
            "region_test_body"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Any)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("full", FlowDataType::Any)]
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
            env.meta.insert("iter".into(), serde_json::Value::from(n));

            // A grace pass (loop_final_pass) always answers without tools.
            let final_pass = env
                .meta
                .get("loop_final_pass")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut msg = ChatMessage::assistant(format!("turn {n}"));
            if n < self.stop_at && !final_pass {
                msg.tool_calls = Some(vec![LlmToolCall {
                    id: format!("call-{n}"),
                    name: "search.run".into(),
                    arguments: "{}".into(),
                }]);
            }
            env.context.messages.push(msg);
            env.payload = FlowValue::Text(format!("turn {n}"));
            Ok(env)
        }
    }

    fn registry(stop_at: i64) -> Arc<AdapterRegistry> {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(AgentBodyAdapter { stop_at }));
        Arc::new(r)
    }

    fn db() -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES ('0', 'test', '{}', 'active')",
                [],
            )
            .expect("seed flow");
        }
        pool
    }

    /// Flow: trigger → [region body] → output, with a loop_back edge body→body.
    /// The single-node region (entry == exit == body) is the minimal inline
    /// loop. `max`/`final_pass` configure the region via the entry node config.
    fn region_flow(max: i64, final_pass: bool) -> serde_json::Value {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "region_test_body", "region": "loop1",
                 "config": {"loop_max_iterations": max, "loop_final_pass": final_pass}},
                {"id": "o", "type": "output", "config": {"format": "text"}}
            ],
            "edges": [
                {"from": "t", "to": "b", "from_port": "text", "to_port": "in"},
                {"from": "b", "to": "b", "from_port": "full", "to_port": "in", "kind": "loop_back"},
                {"from": "b", "to": "o", "from_port": "full", "to_port": "text"}
            ]
        })
    }

    async fn run(flow: &serde_json::Value, stop_at: i64) -> FlowExecutionOutcome {
        let reg = registry(stop_at);
        let compiled =
            Arc::new(CompiledFlow::from_json("0", &flow.to_string(), &reg).expect("compile"));
        execute_blocking(db(), compiled, FlowEnvelope::empty(), stub_ctx(), reg)
            .await
            .expect("execute_blocking")
    }

    /// (f) compile must NOT reject a flow with a loop_back edge — the back edge
    /// is excluded from the toposort, so the outer DAG stays acyclic.
    #[test]
    fn compile_accepts_loop_back_edge() {
        let reg = registry(3);
        let cf = CompiledFlow::from_json("0", &region_flow(25, false).to_string(), &reg)
            .expect("loop_back flow must compile");
        assert_eq!(cf.regions.len(), 1);
        let region = &cf.regions[0];
        assert_eq!(region.id, "loop1");
        assert_eq!(region.entry_pos, region.exit_pos, "single-node region");
        assert_eq!(region.max_iterations, 25);
    }

    /// (a) the region stops when the last assistant turn has no tool calls.
    #[tokio::test]
    async fn stops_on_assistant_without_tool_calls() {
        let outcome = run(&region_flow(25, false), 3).await;
        // 3 iterations: turns 1,2 carry tool calls; turn 3 has none → stop.
        assert_eq!(
            outcome
                .final_envelope
                .meta
                .get("loop_iterations")
                .and_then(|v| v.as_i64()),
            Some(3)
        );
        assert_eq!(
            outcome
                .final_envelope
                .meta
                .get("loop_exit_reason")
                .and_then(|v| v.as_str()),
            Some("no_tool_calls")
        );
    }

    /// (b) the iteration budget caps the loop when the agent never finishes.
    #[tokio::test]
    async fn max_iterations_caps_the_loop() {
        // stop_at far above the budget → the body always emits tool calls.
        let outcome = run(&region_flow(4, false), 1000).await;
        assert_eq!(
            outcome
                .final_envelope
                .meta
                .get("loop_iterations")
                .and_then(|v| v.as_i64()),
            Some(4)
        );
        assert_eq!(
            outcome
                .final_envelope
                .meta
                .get("loop_exit_reason")
                .and_then(|v| v.as_str()),
            Some("max_iterations")
        );
    }

    /// final_pass runs one extra grace iteration after budget exhaustion; the
    /// grace pass answers without tools and the signal is cleared afterwards.
    #[tokio::test]
    async fn final_pass_runs_one_extra_iteration() {
        let outcome = run(&region_flow(2, true), 1000).await;
        // 2 budgeted + 1 grace = 3 body runs.
        assert_eq!(
            outcome
                .final_envelope
                .meta
                .get("iter")
                .and_then(|v| v.as_i64()),
            Some(3)
        );
        assert_eq!(
            outcome
                .final_envelope
                .meta
                .get("loop_iterations")
                .and_then(|v| v.as_i64()),
            Some(3)
        );
        assert!(outcome.final_envelope.meta.get("loop_final_pass").is_none());
    }

    /// (c) a cancelled region surfaces as a node error so the flow aborts.
    #[tokio::test]
    async fn cancelled_region_is_flow_error() {
        let reg = registry(1000);
        let compiled = Arc::new(
            CompiledFlow::from_json("0", &region_flow(10, false).to_string(), &reg)
                .expect("compile"),
        );
        let ctx = stub_ctx();
        ctx.cancel_token.cancel();
        let outcome = execute_blocking(db(), compiled, FlowEnvelope::empty(), ctx, reg)
            .await
            .expect("execute_blocking returns an outcome carrying the error");
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|e| e.contains("cancelled")),
            "expected a cancelled error, got: {:?}",
            outcome.error
        );
    }

    /// (d) conversation history accumulates across iterations in ONE envelope:
    /// each iteration appends exactly one assistant message, so a 3-iteration
    /// run leaves 3 assistant turns in order.
    #[tokio::test]
    async fn conversation_history_accumulates_across_iterations() {
        let outcome = run(&region_flow(25, false), 3).await;
        let assistant_turns: Vec<String> = outcome
            .final_envelope
            .context
            .messages
            .iter()
            .filter(|m| m.role == ChatRole::Assistant)
            .map(|m| m.text_or_default())
            .collect();
        assert_eq!(assistant_turns, vec!["turn 1", "turn 2", "turn 3"]);
        // The final turn has no tool calls (the stop signal).
        let last = outcome
            .final_envelope
            .context
            .messages
            .iter()
            .rev()
            .find(|m| m.role == ChatRole::Assistant)
            .unwrap();
        assert!(last
            .tool_calls
            .as_ref()
            .map(|c| c.is_empty())
            .unwrap_or(true));
    }

    /// (e) R11 rejects a region whose external edge leaves at a non-exit node.
    /// Region `r` = {a (entry), b (exit)}, back edge b→a. The external output
    /// must leave from the exit `b`, but here it leaves from the entry `a` —
    /// a boundary crossing R11 forbids. (R4 is satisfied: a and b each keep one
    /// forward incoming edge, the loop_back not counted.)
    #[test]
    fn r11_rejects_external_edge_out_of_non_exit() {
        let flow = json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "a", "type": "region_test_body", "region": "r", "config": {}},
                {"id": "b", "type": "region_test_body", "region": "r", "config": {}},
                {"id": "o", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t", "to": "a", "from_port": "text", "to_port": "in"},
                {"from": "a", "to": "b", "from_port": "full", "to_port": "in"},
                {"from": "b", "to": "a", "from_port": "full", "to_port": "in", "kind": "loop_back"},
                // External edge OUT of a (entry, not the exit b) — illegal crossing.
                {"from": "a", "to": "o", "from_port": "full", "to_port": "text"}
            ]
        });
        let def = serde_json::from_str(&flow.to_string()).unwrap();
        let reg = registry(3);
        let err = validate(&def, &reg).unwrap_err();
        assert!(
            matches!(err, FlowValidationError::InvalidLoopRegion { .. }),
            "expected InvalidLoopRegion, got: {err:?}"
        );
    }

    /// R11 rejects a region without a loop_back edge (no way to close the loop).
    #[test]
    fn r11_rejects_region_without_loop_back() {
        let flow = json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "region_test_body", "region": "r", "config": {}},
                {"id": "o", "type": "output", "config": {}}
            ],
            "edges": [
                {"from": "t", "to": "b", "from_port": "text", "to_port": "in"},
                {"from": "b", "to": "o", "from_port": "full", "to_port": "text"}
            ]
        });
        let def = serde_json::from_str(&flow.to_string()).unwrap();
        let reg = registry(3);
        let err = validate(&def, &reg).unwrap_err();
        assert!(
            matches!(err, FlowValidationError::InvalidLoopRegion { .. }),
            "expected InvalidLoopRegion, got: {err:?}"
        );
    }
}

#[cfg(test)]
mod direct_execution_tests {
    //! Direct (flow-less) execution: `execute_direct_blocking` /
    //! `execute_direct_streaming` run a single capability node straight on the
    //! executor when there is no user-defined flow. Proves: exactly one node ran
    //! (no trigger/output/pii_filter wrapper), the model output passes through
    //! verbatim (no PII redaction), and token usage is preserved so compliance
    //! accounting downstream still sees it.
    use super::*;
    use crate::flow_engine::dispatchers::{LlmDispatcher, LlmRequest, LlmResponse};
    use crate::flow_engine::envelope::{FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk};
    use crate::flow_engine::node_adapter::{test_support::stub_ctx, AdapterRegistry};
    use crate::flow_engine::node_adapters::LlmNodeAdapter;
    use crate::flow_engine::types::FlowNode;
    use anyhow::Result;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use std::path::Path;
    use std::sync::Mutex;

    /// Blocking fake: returns fixed content + usage; `stream_chat` unused.
    struct FakeBlockingLlm {
        content: String,
        usage: TokenUsage,
    }

    #[async_trait]
    impl LlmDispatcher for FakeBlockingLlm {
        async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
            Ok(LlmResponse {
                audio: None,
                content: self.content.clone(),
                reasoning_content: None,
                usage: self.usage.clone(),
                finish_reason: FinishReason::Stop,
                tool_calls: Vec::new(),
            })
        }
        async fn stream_chat(
            &self,
            _req: LlmRequest,
        ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
            panic!("blocking test must not stream");
        }
    }

    /// Streaming fake: emits a predefined chunk sequence once.
    struct FakeStreamingLlm {
        chunks: Mutex<Option<Vec<LlmStreamChunk>>>,
    }

    #[async_trait]
    impl LlmDispatcher for FakeStreamingLlm {
        async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
            panic!("streaming test must not block");
        }
        async fn stream_chat(
            &self,
            _req: LlmRequest,
        ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
            let chunks = self.chunks.lock().unwrap().take().expect("stream once");
            Ok(futures::stream::iter(chunks.into_iter().map(Ok)).boxed())
        }
    }

    fn llm_registry() -> Arc<AdapterRegistry> {
        let mut r = AdapterRegistry::new();
        r.register_llm(Arc::new(LlmNodeAdapter::new()));
        Arc::new(r)
    }

    fn llm_node() -> FlowNode {
        FlowNode {
            id: "direct_llm".into(),
            node_type: "llm".into(),
            config: serde_json::json!({ "model": "qwen3.5-0.8b" }),
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn direct_blocking_runs_single_node_no_pii_keeps_usage() {
        let registry = llm_registry();
        // Content that a pii_filter would redact — direct path must NOT touch it.
        let raw = "Contact me at jan.kowalski@example.com today.";
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(FakeBlockingLlm {
            content: raw.to_string(),
            usage: TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 7,
                total_tokens: 12,
            },
        });

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let outcome = execute_direct_blocking(llm_node(), initial, ctx, registry)
            .await
            .expect("direct blocking");

        // Verbatim model output — no synthetic pii_filter node in the path.
        assert_eq!(outcome.final_envelope.payload.as_text(), Some(raw));
        // Exactly one node ran (no trigger / output / pii wrapper).
        assert_eq!(outcome.trace.len(), 1);
        assert_eq!(outcome.trace[0].node_type, "llm");
        // Usage preserved for downstream compliance/token accounting.
        assert_eq!(outcome.usage.prompt_tokens, 5);
        assert_eq!(outcome.usage.completion_tokens, 7);
        assert_eq!(outcome.usage.total_tokens, 12);
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        assert!(outcome.error.is_none());
    }

    /// Stalling backend: `execute_chat` never returns. Direct blocking must be
    /// abortable by a wrapping `tokio::time::timeout` — this is exactly the
    /// backstop `FlowDispatcher::run_direct_blocking` wraps around it, so a
    /// blocking-only capability reached on the streaming NotFound path (vision/
    /// tts/stt/embeddings) can never hang forever before the single-chunk wrap.
    #[tokio::test]
    async fn direct_blocking_is_backstop_abortable_when_backend_stalls() {
        struct StallLlm;
        #[async_trait]
        impl LlmDispatcher for StallLlm {
            async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
                // Never completes within the test's backstop window.
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                unreachable!("stall backend must be aborted by the backstop");
            }
            async fn stream_chat(
                &self,
                _req: LlmRequest,
            ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
                panic!("blocking test must not stream");
            }
        }

        let registry = llm_registry();
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(StallLlm);
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let fut = execute_direct_blocking(llm_node(), initial, ctx, registry);
        let res = tokio::time::timeout(std::time::Duration::from_millis(100), fut).await;
        assert!(
            res.is_err(),
            "stalled direct execution must hit the backstop"
        );
    }

    #[tokio::test]
    async fn direct_blocking_missing_model_errors() {
        let registry = llm_registry();
        let ctx = stub_ctx();
        let mut node = llm_node();
        node.config = serde_json::json!({});
        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());
        // No model in node config nor envelope.meta → llm adapter bails; the
        // direct path surfaces the error instead of fabricating a flow.
        let err = execute_direct_blocking(node, initial, ctx, registry)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("direct 'llm' execution failed"));
    }

    #[tokio::test]
    async fn direct_streaming_emits_deltas_and_usage_tail() {
        let registry = llm_registry();
        let chunks = vec![
            LlmStreamChunk {
                choice_index: 0,
                text_delta: "Hello ".into(),
                ..Default::default()
            },
            LlmStreamChunk {
                choice_index: 0,
                text_delta: "world.".into(),
                usage: Some(TokenUsage {
                    prompt_tokens: 3,
                    completion_tokens: 4,
                    total_tokens: 7,
                }),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            },
        ];
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(FakeStreamingLlm {
            chunks: Mutex::new(Some(chunks)),
        });

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("hi".into());

        let db = crate::db::init(Path::new(":memory:")).expect("db");
        let exec = execute_direct_streaming(db, llm_node(), initial, ctx, registry)
            .await
            .expect("direct streaming");

        let mut concat = String::new();
        let mut stream = exec.stream;
        while let Some(item) = stream.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta") {
                concat.push_str(&c.text_delta);
            }
        }
        assert!(concat.contains("Hello world."), "got {concat:?}");

        // Outcome carries the accumulated usage (compliance token tail relies on
        // this) — one node, ephemeral, no audit row.
        let outcome = exec.outcome.await.expect("outcome");
        assert_eq!(outcome.usage.total_tokens, 7);
        assert_eq!(outcome.finish_reason, FinishReason::Stop);
        assert_eq!(outcome.trace.len(), 1);
        assert_eq!(outcome.trace[0].node_type, "llm");
    }
}

#[cfg(test)]
mod node_budget_tests {
    //! The per-node wall clock. A block that declares its own `timeout_secs`
    //! has to actually get it: `delegate_cli` accepts up to 86400 s and its own
    //! approval wait is 600 s on its own, so a hard-coded 600 s node budget
    //! guaranteed the node died first and reported a limit nobody configured.
    use super::*;
    use serde_json::json;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "n1".into(),
            node_type: "delegate_cli".into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    #[test]
    fn a_block_without_its_own_limit_keeps_the_global_floor() {
        assert_eq!(
            node_timeout(&node(json!({}))).as_secs(),
            NODE_TIMEOUT_SECS,
            "a node that declares nothing must stay protected by the global limit"
        );
        // A declared value BELOW the floor never shortens the wall: for several
        // blocks `timeout_secs` bounds an inner operation, and the block still
        // has to settle its run afterwards.
        assert_eq!(
            node_timeout(&node(json!({"timeout_secs": 5}))).as_secs(),
            NODE_TIMEOUT_SECS
        );
    }

    #[test]
    fn a_declared_limit_longer_than_the_floor_is_the_one_that_applies() {
        assert_eq!(
            node_timeout(&node(json!({"timeout_secs": 900}))).as_secs(),
            900,
            "the observed defect: a node configured for 900 s died at 600 s"
        );
        assert_eq!(
            node_timeout(&node(json!({"timeout_secs": 86_400}))).as_secs(),
            86_400
        );
    }

    #[test]
    fn a_declared_limit_is_capped_and_a_nonsense_one_is_ignored() {
        assert_eq!(
            node_timeout(&node(json!({"timeout_secs": u64::MAX}))).as_secs(),
            MAX_NODE_TIMEOUT_SECS,
            "config is operator input; without a ceiling a typo parks the slot forever"
        );
        for bogus in [json!("900"), json!(-5), json!(null)] {
            assert_eq!(
                node_timeout(&node(json!({"timeout_secs": bogus}))).as_secs(),
                NODE_TIMEOUT_SECS
            );
        }
    }
}

#[cfg(test)]
mod first_token_tests {
    //! §2.6 — `FirstToken` is the one emission point TTFT needs, and it fires on
    //! the first NON-EMPTY delta of a streaming step. Deltas are consumed in two
    //! separate loops, so both are covered here: the plain streaming finalizer
    //! (`finalize_streaming_flow`, one producer step per run) and the inline
    //! harness member (`stream_llm_member`, one call per loop-region iteration).
    use super::{chunk_carries_first_token, execute_streaming};
    use crate::db::DbPool;
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::dispatchers::ProgressEvent;
    use crate::flow_engine::envelope::{
        EnvelopeDelta, FinishReason, FlowEnvelope, FlowValue, LlmStreamChunk, NodeInput,
        ToolCallDelta,
    };
    use crate::flow_engine::node_adapter::{
        test_support::{stub_ctx, CapturingProgress},
        AdapterRegistry, ExecutionContext, NodeAdapter, PortSpec, StreamProducerAdapter,
    };
    use crate::flow_engine::node_adapters::{OutputNodeAdapter, TriggerNodeAdapter};
    use crate::flow_engine::types::{FlowDataType, FlowNode};
    use anyhow::Result;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    fn db() -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES ('0', 'test', '{}', 'active')",
                [],
            )
            .expect("seed flow");
        }
        pool
    }

    fn text_chunk(text: &str) -> LlmStreamChunk {
        LlmStreamChunk {
            text_delta: text.to_string(),
            ..Default::default()
        }
    }

    fn reasoning_chunk(text: &str) -> LlmStreamChunk {
        LlmStreamChunk {
            reasoning_delta: Some(text.to_string()),
            ..Default::default()
        }
    }

    fn terminal_chunk() -> LlmStreamChunk {
        LlmStreamChunk {
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }
    }

    /// Node ids of every FirstToken the sink saw, in emission order.
    fn first_tokens(capture: &CapturingProgress) -> Vec<String> {
        capture
            .events()
            .into_iter()
            .filter_map(|(_, e)| match e {
                ProgressEvent::FirstToken { node_id } => Some(node_id),
                _ => None,
            })
            .collect()
    }

    /// Stream producer fed from a channel, so a test can advance the stream one
    /// delta at a time and inspect what was emitted in between.
    struct ChannelProducer {
        rx: Mutex<Option<mpsc::Receiver<LlmStreamChunk>>>,
    }

    impl ChannelProducer {
        fn new(rx: mpsc::Receiver<LlmStreamChunk>) -> Self {
            Self {
                rx: Mutex::new(Some(rx)),
            }
        }
    }

    #[async_trait]
    impl NodeAdapter for ChannelProducer {
        fn node_type(&self) -> &str {
            "first_token_probe"
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
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            Ok((*ctx.initial_envelope).clone())
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for ChannelProducer {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            _inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
            let rx = self
                .rx
                .lock()
                .expect("producer lock")
                .take()
                .expect("produce_stream runs once per flow");
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|c| (Ok(EnvelopeDelta::Llm(c)), rx))
            })
            .boxed())
        }
    }

    /// Plain streaming flow: trigger → probe → output(stream).
    fn probe_flow() -> serde_json::Value {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "p", "type": "first_token_probe", "config": {}},
                {"id": "o", "type": "output", "config": {"mode": "stream"}}
            ],
            "edges": [
                {"from": "t", "to": "p", "from_port": "text", "to_port": "in"},
                {"from": "p", "to": "o", "from_port": "stream", "to_port": "text"}
            ]
        })
    }

    fn probe_registry(rx: mpsc::Receiver<LlmStreamChunk>) -> Arc<AdapterRegistry> {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_stream_producer(Arc::new(ChannelProducer::new(rx)));
        Arc::new(r)
    }

    /// The predicate that decides what counts as the first token. A reasoning
    /// delta counts (a thinking model's stream genuinely starts there); an
    /// empty string does not, whether it arrives as text or as `Some("")`.
    #[test]
    fn only_a_non_empty_delta_counts_as_the_first_token() {
        let deltas = ["", "", "a", "b"];
        let first = deltas
            .iter()
            .position(|d| chunk_carries_first_token(&text_chunk(d)));
        assert_eq!(first, Some(2), "the first two empty deltas are not tokens");

        assert!(chunk_carries_first_token(&reasoning_chunk("thinking")));
        assert!(
            !chunk_carries_first_token(&reasoning_chunk("")),
            "Some(\"\") is an empty delta, not a token"
        );
        assert!(
            !chunk_carries_first_token(&terminal_chunk()),
            "the finish marker carries no narration"
        );
    }

    /// The sequence ["", "", "a", "b"] produces EXACTLY ONE FirstToken, on "a".
    /// Each delta is only sent after the previous one came back out of the flow,
    /// so when an assertion runs the executor provably has not seen the next
    /// delta yet — that is what pins the event to "a" rather than to the run.
    #[tokio::test]
    async fn finalizer_emits_one_first_token_on_the_first_non_empty_delta() {
        let (tx, rx) = mpsc::channel::<LlmStreamChunk>(8);
        let registry = probe_registry(rx);
        let compiled = Arc::new(
            CompiledFlow::from_json("0", &probe_flow().to_string(), &registry).expect("compile"),
        );

        let capture = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = capture.clone();
        ctx.progress_scope = "ttft".into();

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("go".into());

        let exec = execute_streaming(db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");
        let mut stream = exec.stream;

        for _ in 0..2 {
            tx.send(text_chunk("")).await.expect("send empty delta");
            let forwarded = stream.next().await.expect("empty delta forwarded");
            forwarded.expect("empty delta is not an error");
            assert!(
                first_tokens(&capture).is_empty(),
                "an empty delta must not emit FirstToken"
            );
        }

        tx.send(text_chunk("a")).await.expect("send 'a'");
        let forwarded = stream.next().await.expect("'a' forwarded");
        forwarded.expect("'a' delta is not an error");
        assert_eq!(
            first_tokens(&capture),
            vec!["p".to_string()],
            "the first non-empty delta emits FirstToken for the producer node"
        );

        tx.send(text_chunk("b")).await.expect("send 'b'");
        let forwarded = stream.next().await.expect("'b' forwarded");
        forwarded.expect("'b' delta is not an error");
        assert_eq!(
            first_tokens(&capture).len(),
            1,
            "FirstToken must not repeat within a step"
        );

        drop(tx);
        while stream.next().await.is_some() {}
        assert_eq!(first_tokens(&capture).len(), 1);
        assert_eq!(
            capture.events()[0].0,
            "ttft",
            "events carry the configured broadcast scope"
        );
    }

    /// Invariant 6 — a stream that never carries a non-empty delta leaves a GAP
    /// in the log. The finish marker must not be turned into a fabricated
    /// FirstToken just to keep the TTFT query tidy.
    #[tokio::test]
    async fn no_first_token_when_no_delta_is_ever_non_empty() {
        let (tx, rx) = mpsc::channel::<LlmStreamChunk>(8);
        let registry = probe_registry(rx);
        let compiled = Arc::new(
            CompiledFlow::from_json("0", &probe_flow().to_string(), &registry).expect("compile"),
        );

        let capture = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = capture.clone();

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("go".into());

        let exec = execute_streaming(db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");
        let mut stream = exec.stream;

        for chunk in [text_chunk(""), reasoning_chunk(""), terminal_chunk()] {
            tx.send(chunk).await.expect("send delta");
        }
        drop(tx);
        while stream.next().await.is_some() {}
        let _ = exec.outcome.await;

        assert!(
            first_tokens(&capture).is_empty(),
            "no visible token was ever produced, so no FirstToken may be emitted"
        );
    }

    /// Streaming region member. Call 1 ends with a tool call so the harness runs
    /// again; call 2 answers without tools, which is the region's structural
    /// stop. Every call opens with empty deltas, so only a per-call flag can
    /// yield exactly one FirstToken per step.
    struct RegionStreamBody {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl NodeAdapter for RegionStreamBody {
        fn node_type(&self) -> &str {
            "first_token_region_body"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Any)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![
                PortSpec::new("stream", FlowDataType::Text),
                PortSpec::new("full", FlowDataType::Any),
            ]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            _inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            Ok((*ctx.initial_envelope).clone())
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for RegionStreamBody {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            _inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut last = terminal_chunk();
            if call == 0 {
                last.tool_calls = vec![ToolCallDelta {
                    index: 0,
                    id: Some("call-0".into()),
                    function_name: Some("probe.run".into()),
                    arguments_delta: Some("{}".into()),
                }];
            }
            let items = vec![
                Ok(EnvelopeDelta::Llm(text_chunk(""))),
                Ok(EnvelopeDelta::Llm(reasoning_chunk(""))),
                Ok(EnvelopeDelta::Llm(text_chunk(&format!("step {call}")))),
                Ok(EnvelopeDelta::Llm(last)),
            ];
            Ok(futures::stream::iter(items).boxed())
        }
    }

    /// Inline harness: trigger → [region body] → output(stream), loop_back on
    /// the single-node region.
    fn region_flow() -> serde_json::Value {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "first_token_region_body", "region": "loop1",
                 "config": {"loop_max_iterations": 5}},
                {"id": "o", "type": "output", "config": {"mode": "stream"}}
            ],
            "edges": [
                {"from": "t", "to": "b", "from_port": "text", "to_port": "in"},
                {"from": "b", "to": "b", "from_port": "full", "to_port": "in", "kind": "loop_back"},
                {"from": "b", "to": "o", "from_port": "stream", "to_port": "text"}
            ]
        })
    }

    /// Two harness iterations produce TWO FirstToken events — one per step, each
    /// inside its own iteration bracket. Per-run semantics would emit one and
    /// make every step after the first unmeasurable.
    #[tokio::test]
    async fn harness_emits_one_first_token_per_region_iteration() {
        let registry = {
            let mut r = AdapterRegistry::new();
            r.register(Arc::new(TriggerNodeAdapter::new()));
            r.register(Arc::new(OutputNodeAdapter::new()));
            r.register_stream_producer(Arc::new(RegionStreamBody {
                calls: AtomicUsize::new(0),
            }));
            Arc::new(r)
        };
        let compiled = Arc::new(
            CompiledFlow::from_json("0", &region_flow().to_string(), &registry).expect("compile"),
        );

        let capture = Arc::new(CapturingProgress::new());
        let mut ctx = stub_ctx();
        ctx.progress = capture.clone();

        let mut initial = FlowEnvelope::empty();
        initial.payload = FlowValue::Text("go".into());

        let exec = execute_streaming(db(), compiled, initial, ctx, registry)
            .await
            .expect("execute_streaming");
        let mut stream = exec.stream;
        while stream.next().await.is_some() {}
        let _ = exec.outcome.await;

        assert_eq!(
            first_tokens(&capture),
            vec!["b".to_string(), "b".to_string()],
            "one FirstToken per harness step, named after the streaming member"
        );

        // Each FirstToken lands inside its own step's iteration bracket, so
        // `request_started -> first_token` never spans two steps.
        let bracket: Vec<&str> = capture
            .events()
            .iter()
            .filter_map(|(_, e)| match e {
                ProgressEvent::IterationStarted { .. } => Some("iteration_started"),
                ProgressEvent::FirstToken { .. } => Some("first_token"),
                ProgressEvent::IterationFinished { .. } => Some("iteration_finished"),
                _ => None,
            })
            .collect();
        assert_eq!(
            bracket,
            vec![
                "iteration_started",
                "first_token",
                "iteration_finished",
                "iteration_started",
                "first_token",
                "iteration_finished",
            ]
        );
    }
}

#[cfg(test)]
mod provenance_persistence_tests {
    //! §2.5 / §2.11 stage 1 — `flow_executions` has to answer "from where and
    //! who" on its own, without a second table.
    //!
    //! Every test here drives the PRODUCTION writer — `execute_blocking` →
    //! `create_execution_record` → `persist_execution` — and reads the row back
    //! through `repository::list_flow_executions_for_flow`. An `INSERT` written
    //! in a test body would prove only that the column accepts a value, never
    //! that the product supplies one.
    use super::execute_blocking;
    use crate::db::{repository, DbPool};
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::dispatcher::{ActorKind, FlowOrigin};
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
    use crate::flow_engine::node_adapter::{
        test_support::stub_ctx, AdapterRegistry, ExecutionContext, NodeAdapter, PortSpec,
    };
    use crate::flow_engine::node_adapters::{OutputNodeAdapter, TriggerNodeAdapter};
    use crate::flow_engine::types::{FlowDataType, FlowNode};
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::path::Path;
    use std::sync::Arc;

    const FLOW_ID: &str = "prov-flow";
    const MODEL: &str = "qwen3.5-0.8b";

    /// Stands in for the LLM node on the single behaviour that matters here: it
    /// registers the model it resolved with `UsageSink`, exactly where
    /// `LlmNodeAdapter` does. `persist_execution` reads it back off the outcome.
    struct ModelRecordingAdapter;

    #[async_trait]
    impl NodeAdapter for ModelRecordingAdapter {
        fn node_type(&self) -> &str {
            "model_call"
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
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let model = node
                .config
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or(MODEL);
            ctx.usage_sink.record_model(model);
            let mut out = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(FlowEnvelope::empty);
            out.payload = FlowValue::Text("answered".into());
            Ok(out)
        }
    }

    fn registry() -> Arc<AdapterRegistry> {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(ModelRecordingAdapter));
        Arc::new(r)
    }

    fn db() -> DbPool {
        let pool = crate::db::init(Path::new(":memory:")).expect("in-memory db");
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'prov', '{}', 'active')",
                rusqlite::params![FLOW_ID],
            )
            .expect("seed flow");
        }
        pool
    }

    const FLOW_JSON: &str = r#"{
        "nodes":[
            {"id":"t","type":"trigger","config":{}},
            {"id":"m","type":"model_call","config":{"model":"qwen3.5-0.8b"}},
            {"id":"o","type":"output","config":{}}
        ],
        "edges":[
            {"from":"t","to":"m","from_port":"text","to_port":"in"},
            {"from":"m","to":"o","from_port":"full","to_port":"text"}
        ]
    }"#;

    /// Runs the flow on `pool` under `ctx` and hands back the persisted row.
    async fn run_and_read(
        pool: &DbPool,
        ctx: ExecutionContext,
        initial: FlowEnvelope,
    ) -> crate::db::models::DbFlowExecution {
        let reg = registry();
        let compiled =
            Arc::new(CompiledFlow::from_json(FLOW_ID, FLOW_JSON, &reg).expect("compile"));
        execute_blocking(pool.clone(), compiled, initial, ctx, reg)
            .await
            .expect("execute_blocking");
        let rows =
            repository::list_flow_executions_for_flow(pool, FLOW_ID, 10).expect("read executions");
        assert_eq!(rows.len(), 1, "exactly one audit row per top-level run");
        rows.into_iter().next().expect("row")
    }

    /// A user-initiated Code Studio run: the stamp the entry point minted has to
    /// be on the row afterwards, together with the request id and the model the
    /// run actually spent tokens on. This is the §2.11 stage-1 acceptance.
    #[tokio::test]
    async fn a_completed_run_persists_its_full_provenance_stamp() {
        let pool = db();
        let mut ctx = stub_ctx();
        ctx.request_id = "req-7".into();
        ctx.origin = FlowOrigin::CodeStudio;
        ctx.actor_kind = ActorKind::User;
        ctx.actor_id = Some("u-1".into());
        ctx.actor_user_id = Some("u-1".into());
        ctx.correlation_id = Some("corr-1".into());

        let row = run_and_read(&pool, ctx, FlowEnvelope::empty()).await;

        assert_eq!(row.origin.as_deref(), Some("code_studio"));
        assert_eq!(row.actor_kind.as_deref(), Some("user"));
        assert_eq!(row.actor_id.as_deref(), Some("u-1"));
        assert_eq!(row.actor_user_id.as_deref(), Some("u-1"));
        assert_eq!(row.correlation_id.as_deref(), Some("corr-1"));
        // Both columns existed long before the provenance five and were written
        // as NULL by every run — a row that cannot name its request or its model
        // answers neither "which call was this" nor "what did it cost".
        assert_eq!(row.request_id.as_deref(), Some("req-7"));
        assert_eq!(row.model.as_deref(), Some(MODEL));
        assert_eq!(row.status.as_deref(), Some("completed"));
    }

    /// A service API key has no user behind it. The row must say so — deriving a
    /// user here would report an unattended integration call as a person's.
    #[tokio::test]
    async fn a_service_api_key_run_records_no_user_behind_the_key() {
        let pool = db();
        let mut ctx = stub_ctx();
        ctx.request_id = "req-api".into();
        ctx.origin = FlowOrigin::Api;
        ctx.actor_kind = ActorKind::ApiKey;
        ctx.actor_id = Some("key-77".into());
        ctx.actor_user_id = None;

        let row = run_and_read(&pool, ctx, FlowEnvelope::empty()).await;

        assert_eq!(row.origin.as_deref(), Some("api"));
        assert_eq!(row.actor_kind.as_deref(), Some("api_key"));
        assert_eq!(row.actor_id.as_deref(), Some("key-77"));
        assert_eq!(row.actor_user_id, None);
    }

    /// §3 invariant 1: the stamp is server-minted and unreachable from model
    /// output. `envelope.meta` is writable by every node — including an addon
    /// block that deserializes a whole envelope from guest memory — so a run
    /// arriving with a forged stamp in meta must still be recorded as what the
    /// entry point authorized.
    #[tokio::test]
    async fn envelope_meta_cannot_forge_the_stamp() {
        let pool = db();
        let mut ctx = stub_ctx();
        ctx.request_id = "req-real".into();
        ctx.origin = FlowOrigin::Addon;
        ctx.actor_kind = ActorKind::Addon;
        ctx.actor_id = Some("addon-notes".into());
        ctx.actor_user_id = None;
        ctx.correlation_id = Some("corr-real".into());

        let mut hostile = FlowEnvelope::empty();
        for (key, value) in [
            ("origin", "dashboard"),
            ("actor_kind", "user"),
            ("actor_id", "admin"),
            ("actor_user_id", "admin"),
            ("correlation_id", "corr-forged"),
            ("request_id", "req-forged"),
            ("model", "gpt-forged"),
        ] {
            hostile.meta.insert(key.into(), Value::String(value.into()));
        }

        let row = run_and_read(&pool, ctx, hostile).await;

        assert_eq!(row.origin.as_deref(), Some("addon"));
        assert_eq!(row.actor_kind.as_deref(), Some("addon"));
        assert_eq!(row.actor_id.as_deref(), Some("addon-notes"));
        assert_eq!(row.actor_user_id, None);
        assert_eq!(row.correlation_id.as_deref(), Some("corr-real"));
        assert_eq!(row.request_id.as_deref(), Some("req-real"));
        assert_eq!(row.model.as_deref(), Some(MODEL));
    }

    /// A DOCUMENTED GAP, pinned so it cannot widen or close by accident: a
    /// synthetic flow (Universal Flow Gateway) is assembled in memory with an
    /// EMPTY flow id and has no row in `flows`, so `create_execution_record`
    /// returns the `0` sentinel and writes nothing rather than failing the
    /// foreign key. A `/v1` call that falls back to a synthetic flow therefore
    /// leaves no `flow_executions` row at all — the run is accounted for on the
    /// timeline and nowhere else. Closing it needs a schema change; until then
    /// this test is what keeps the behaviour deliberate.
    #[tokio::test]
    async fn a_synthetic_flow_writes_no_run_row_at_all() {
        let pool = db();
        let reg = registry();
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"o","from_port":"text","to_port":"text"}
            ]
        }"#;
        // The shape `synthetic.rs` builds: no id, because there is no row to
        // point at.
        let compiled = Arc::new(CompiledFlow::from_json("", json, &reg).expect("compile"));
        let mut ctx = stub_ctx();
        ctx.request_id = "req-synthetic".into();
        ctx.origin = FlowOrigin::Api;
        ctx.actor_kind = ActorKind::ApiKey;
        ctx.actor_id = Some("key-77".into());
        ctx.correlation_id = Some("corr-synthetic".into());
        execute_blocking(pool.clone(), compiled, FlowEnvelope::empty(), ctx, reg)
            .await
            .expect("execute_blocking");

        let rows: i64 = {
            let conn = pool.read().expect("db lock");
            conn.query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
                .expect("count executions")
        };
        assert_eq!(
            rows, 0,
            "a synthetic flow has no row in `flows`, so it writes no run row"
        );
    }

    /// Invariant 6 — a flow that called no model leaves `model` NULL instead of
    /// inheriting a routing hint it never used.
    #[tokio::test]
    async fn a_run_without_a_model_call_leaves_the_model_column_null() {
        let pool = db();
        let reg = registry();
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t","to":"o","from_port":"text","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(CompiledFlow::from_json(FLOW_ID, json, &reg).expect("compile"));
        let mut ctx = stub_ctx();
        ctx.request_id = "req-nomodel".into();
        execute_blocking(pool.clone(), compiled, FlowEnvelope::empty(), ctx, reg)
            .await
            .expect("execute_blocking");

        let row = repository::list_flow_executions_for_flow(&pool, FLOW_ID, 10)
            .expect("read executions")
            .into_iter()
            .next()
            .expect("row");
        assert_eq!(row.model, None);
        assert_eq!(row.request_id.as_deref(), Some("req-nomodel"));
        assert_eq!(row.origin.as_deref(), Some("system"));
    }
}
