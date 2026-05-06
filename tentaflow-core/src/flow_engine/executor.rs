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
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::db::{repository, DbPool};
use crate::flow_engine::cache::CompiledFlow;
use crate::flow_engine::envelope::{
    ChatMessage, EnvelopeDelta, FinishReason, FlowEnvelope, FlowExecutionOutcome, FlowValue,
    LlmStreamChunk, NodeInput, TokenUsage, TraceStatus, TraceStep,
};
use crate::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext};

const MAX_NODES_PER_EXECUTION: usize = 256;

pub struct StreamingExecution {
    pub stream: BoxStream<'static, Result<EnvelopeDelta>>,
    pub outcome: oneshot::Receiver<FlowExecutionOutcome>,
}

/// Blocking execution. Toposort, każdy node wywołany przez adapter z mapy,
/// outputs trzymane w `Arc<FlowEnvelope>` per pos. continue_on_error z
/// trigger.config kontroluje czy błąd przerywa flow.
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

    let execution_id = create_execution_record(&db, compiled.flow_id).await?;
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
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    let mut trace: Vec<TraceStep> = Vec::with_capacity(n);
    let mut error: Option<String> = None;
    let mut last_finish_reason: Option<FinishReason> = None;

    for (run_idx, &def_idx) in compiled.execution_order.iter().enumerate() {
        // Cancel + deadline gate między node'ami: klient disconnect /
        // operator timeout abortuje flow zanim wystartuje kolejny adapter.
        // Per-adapter cancel/deadline propaguje się przez ExecutionContext
        // wewnątrz LLM dispatcher; tu pilnujemy granicy topo-loopa.
        if ctx.cancel_token.is_cancelled() {
            error = Some("cancelled".into());
            last_finish_reason = Some(FinishReason::Cancelled);
            break;
        }
        if let Some(dl) = ctx.deadline {
            if Instant::now() >= dl {
                error = Some("deadline exceeded".into());
                last_finish_reason = Some(FinishReason::Error);
                break;
            }
        }
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
        match adapter.execute(node, &inputs, &ctx).await {
            Ok(envelope) => {
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
            Err(e) => {
                let duration_ms = attempt_started.elapsed().as_millis() as u64;
                trace.push(TraceStep {
                    node_id: node.id.clone(),
                    node_type: node.node_type.clone(),
                    started_at_ms: step_started,
                    duration_ms,
                    status: TraceStatus::Error {
                        message: e.to_string(),
                    },
                    usage: None,
                });
                if continue_on_error {
                    // Propaguj envelope sprzed błędu — kolejny node dostanie
                    // ostatni dostępny output. Brak ustalonego producenta →
                    // initial.
                    let propagated = inputs
                        .first()
                        .map(|i| i.envelope.clone())
                        .unwrap_or_else(|| initial_arc.clone());
                    outputs[run_idx] = Some(propagated);
                    continue;
                } else {
                    error = Some(e.to_string());
                    last_finish_reason = Some(FinishReason::Error);
                    break;
                }
            }
        }
    }

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

    let execution_id = create_execution_record(&db, compiled.flow_id).await?;
    ctx.execution_id = execution_id;

    let llm_run_idx = compiled
        .streaming_llm_run_idx()
        .ok_or_else(|| anyhow!("execute_streaming called on non-streaming flow"))?;
    let llm_def_idx = compiled.execution_order[llm_run_idx];
    let llm_node = &compiled.definition.nodes[llm_def_idx];

    let n = compiled.execution_order.len();
    let mut outputs: Vec<Option<Arc<FlowEnvelope>>> = vec![None; n];
    let mut trace: Vec<TraceStep> = Vec::with_capacity(n);

    // Pre-LLM topo loop. Cancel/deadline checked between nodes — same
    // contract as `execute_blocking`. LLM streaming dispatch ma własny
    // wrapper (StreamBoundary) honorujący te flagi w trakcie streamu.
    for run_idx in 0..llm_run_idx {
        if ctx.cancel_token.is_cancelled() {
            return Err(anyhow!("cancelled"));
        }
        if let Some(dl) = ctx.deadline {
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
        let envelope = adapter
            .execute(node, &inputs, &ctx)
            .await
            .map_err(|e| anyhow!("pre-LLM node '{}' failed: {e}", node.id))?;
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

    // Streaming LLM dispatch via typed accessor.
    let llm_inputs = build_inputs(&compiled, llm_run_idx, &outputs);
    let llm_adapter = adapters
        .llm()
        .ok_or_else(|| anyhow!("no LLM adapter registered for streaming flow"))?;
    let request = llm_adapter.prepare_llm_request(llm_node, &llm_inputs, &ctx);
    let llm_step_started = ctx.clock.now_ms();
    let adapter_stream = ctx
        .llm
        .stream_chat(request)
        .await
        .map_err(|e| anyhow!("stream_chat failed: {e}"))?;

    let cancel = ctx.cancel_token.clone();
    let (outbound_tx, outbound_rx) = mpsc::channel::<Result<EnvelopeDelta>>(64);
    let (outcome_tx, outcome_rx) = oneshot::channel::<FlowExecutionOutcome>();

    // Materializujemy parametry potrzebne po move'ie do task'a.
    let llm_input_envelope = llm_inputs
        .first()
        .map(|i| i.envelope.clone())
        .unwrap_or_else(|| initial_arc.clone());
    let llm_node_id = llm_node.id.clone();
    let llm_node_type = llm_node.node_type.clone();
    let db_for_task = db.clone();

    tokio::spawn(finalize_streaming_flow(
        execution_id,
        adapter_stream,
        outbound_tx,
        outcome_tx,
        cancel,
        FinalizerInputs {
            started,
            llm_step_started,
            llm_node_id,
            llm_node_type,
            llm_input_envelope,
            trace,
            db: db_for_task,
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
    llm_step_started: u64,
    llm_node_id: String,
    llm_node_type: String,
    llm_input_envelope: Arc<FlowEnvelope>,
    trace: Vec<TraceStep>,
    db: DbPool,
}

async fn finalize_streaming_flow(
    execution_id: i64,
    mut adapter_stream: BoxStream<'static, Result<LlmStreamChunk>>,
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
    let llm_attempt_started = Instant::now();

    'main: loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break 'main;
            }
            chunk = adapter_stream.next() => match chunk {
                Some(Ok(c)) => {
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
                            // SendError = klient disconnect; backpressure-resilient
                            // bo cancel idzie razem przez select.
                            let _ = send_res;
                        }
                    }
                }
                Some(Err(e)) => {
                    error = Some(format!("{e}"));
                    break 'main;
                }
                None => break 'main, // EOF
            }
        }
    }

    drop(outbound_tx);

    // Buduj final envelope: klon input envelope LLM + payload Text(content)
    // + dopisana assistant message (parytet z LlmNodeAdapter::execute).
    let mut final_envelope: FlowEnvelope = (*inputs.llm_input_envelope).clone();
    final_envelope.payload = FlowValue::Text(text_buf.clone());
    final_envelope
        .context
        .messages
        .push(ChatMessage::assistant(text_buf));

    let llm_duration_ms = llm_attempt_started.elapsed().as_millis() as u64;
    let llm_usage = last_usage.unwrap_or_default();
    inputs.trace.push(TraceStep {
        node_id: inputs.llm_node_id.clone(),
        node_type: inputs.llm_node_type.clone(),
        started_at_ms: inputs.llm_step_started,
        duration_ms: llm_duration_ms,
        status: if cancelled {
            TraceStatus::Skipped
        } else if let Some(e) = error.clone() {
            TraceStatus::Error { message: e }
        } else {
            TraceStatus::Ok
        },
        usage: if llm_usage == TokenUsage::default() {
            None
        } else {
            Some(llm_usage)
        },
    });

    let aggregate_usage = aggregate_usage(&inputs.trace);
    let total_latency_ms = inputs.started.elapsed().as_millis() as i64;
    let finish_reason = if cancelled {
        FinishReason::Cancelled
    } else if error.is_some() {
        FinishReason::Error
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

async fn create_execution_record(db: &DbPool, flow_id: i64) -> Result<i64> {
    let pool = db.clone();
    let id = tokio::task::spawn_blocking(move || {
        repository::create_flow_execution(&pool, flow_id, None, None, "running")
    })
    .await??;
    Ok(id)
}

async fn persist_execution(db: &DbPool, execution_id: i64, outcome: &FlowExecutionOutcome) {
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
