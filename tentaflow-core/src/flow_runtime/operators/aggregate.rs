// =============================================================================
// File: flow_runtime/operators/aggregate.rs — tumbling-window reducer
// =============================================================================
//
// Buffers numeric values inside a fixed-size tumbling window and emits one
// summary record per window: `{ window_start, window_end, count, value }`.
// The `op` parameter selects the reduction (`count`/`sum`/`min`/`max`/
// `avg`). `count` ignores the `field` parameter; the other ops require it.
//
// Empty windows are dropped on the floor — emitting zero-record batches
// pollutes downstream operators that count events. EOF flushes any pending
// (partial) window before propagating.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, read_param_string, read_param_u32, record_field_dot,
    OperatorContext, OperatorError, OutboundEdge,
};
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggOp {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

impl AggOp {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "count" => Some(AggOp::Count),
            "sum" => Some(AggOp::Sum),
            "min" => Some(AggOp::Min),
            "max" => Some(AggOp::Max),
            "avg" => Some(AggOp::Avg),
            _ => None,
        }
    }
    fn requires_field(self) -> bool {
        !matches!(self, AggOp::Count)
    }
}

fn flush_window(
    op: AggOp,
    samples: &mut Vec<f64>,
    count: u64,
    start: &str,
    end: &str,
) -> (Option<toml::Value>, bool) {
    if count == 0 {
        return (None, false);
    }
    let value = match op {
        AggOp::Count => count as f64,
        AggOp::Sum => samples.iter().sum::<f64>(),
        AggOp::Min => samples.iter().cloned().fold(f64::INFINITY, f64::min),
        AggOp::Max => samples.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        AggOp::Avg => {
            if samples.is_empty() {
                0.0
            } else {
                samples.iter().sum::<f64>() / (samples.len() as f64)
            }
        }
    };
    // `count` is a `u64` but the TOML `count` field is `i64`. A window with
    // >i64::MAX records is not physically reachable in a single tumbling
    // window today, but the cast is still unchecked — saturate so the
    // field stays monotonic non-negative if the invariant ever breaks.
    let saturated = count > i64::MAX as u64;
    let count_i64 = if saturated { i64::MAX } else { count as i64 };
    let mut t = toml::value::Table::new();
    t.insert(
        "window_start".to_string(),
        toml::Value::String(start.to_string()),
    );
    t.insert(
        "window_end".to_string(),
        toml::Value::String(end.to_string()),
    );
    t.insert("count".to_string(), toml::Value::Integer(count_i64));
    t.insert("value".to_string(), toml::Value::Float(value));
    samples.clear();
    (Some(toml::Value::Table(t)), saturated)
}

pub async fn run(
    ctx: OperatorContext,
    inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    let window_ms = read_param_u32(&ctx.params, "window_ms")
        .ok_or_else(|| OperatorError::BadParams("aggregate: 'window_ms' required".to_string()))?;
    if window_ms < 100 {
        return Err(OperatorError::BadParams(
            "aggregate: window_ms must be >= 100".to_string(),
        ));
    }
    let op_str = read_param_string(&ctx.params, "op")
        .ok_or_else(|| OperatorError::BadParams("aggregate: 'op' required".to_string()))?;
    let op = AggOp::from_str(&op_str)
        .ok_or_else(|| OperatorError::BadParams(format!("aggregate: unknown op '{op_str}'")))?;
    let field = read_param_string(&ctx.params, "field");
    if op.requires_field() && field.is_none() {
        return Err(OperatorError::BadParams(format!(
            "aggregate: op '{op_str}' requires 'field'"
        )));
    }

    let mut interval = tokio::time::interval(Duration::from_millis(window_ms as u64));
    // First tick is immediate by default — burn it so the initial window has
    // a full period to fill before the first flush.
    interval.tick().await;

    let mut samples: Vec<f64> = Vec::new();
    let mut count: u64 = 0;
    let mut window_start = Utc::now().to_rfc3339();
    let mut eof_received = vec![false; inbound.len()];
    let mut active = inbound.len();
    let mut windows_emitted: u64 = 0;

    loop {
        if active == 0 {
            // Flush the final partial window before tearing down outbound.
            let window_end = Utc::now().to_rfc3339();
            let (maybe_rec, saturated) =
                flush_window(op, &mut samples, count, &window_start, &window_end);
            if let Some(rec) = maybe_rec {
                for (_, edge) in &outbound {
                    edge.send(FlowMessage::Record(rec.clone()));
                }
                windows_emitted += 1;
            }
            if saturated {
                emit_op_audit(
                    &ctx.db,
                    &ctx.addon_id,
                    &ctx.flow_id,
                    &ctx.invocation_id,
                    &ctx.operator_id,
                    "aggregate",
                    "count_saturated",
                    "warn",
                    Some(serde_json::json!({"raw_count_u64": count})),
                    ctx.org_id.as_deref(),
                );
            }
            break;
        }

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                close_outbound(&outbound);
                return Ok(());
            }
            _ = interval.tick() => {
                let window_end = Utc::now().to_rfc3339();
                let (maybe_rec, saturated) =
                    flush_window(op, &mut samples, count, &window_start, &window_end);
                if let Some(rec) = maybe_rec {
                    for (_, edge) in &outbound {
                        edge.send(FlowMessage::Record(rec.clone()));
                    }
                    windows_emitted += 1;
                }
                if saturated {
                    emit_op_audit(
                        &ctx.db,
                        &ctx.addon_id,
                        &ctx.flow_id,
                        &ctx.invocation_id,
                        &ctx.operator_id,
                        "aggregate",
                        "count_saturated",
                        "warn",
                        Some(serde_json::json!({"raw_count_u64": count})),
                        ctx.org_id.as_deref(),
                    );
                }
                count = 0;
                window_start = window_end;
            }
            msg = recv_any(&inbound, &mut eof_received) => {
                match msg {
                    AggInput::Record(v) => {
                        if op.requires_field() {
                            // We already validated that field is Some above.
                            let f = field.as_deref().unwrap_or("");
                            if let Some(raw) = record_field_dot(&v, f) {
                                let num = if let Some(i) = raw.as_integer() {
                                    Some(i as f64)
                                } else {
                                    raw.as_float()
                                };
                                if let Some(n) = num {
                                    samples.push(n);
                                    count += 1;
                                }
                            }
                        } else {
                            count += 1;
                        }
                    }
                    AggInput::Eof => {
                        active = active.saturating_sub(1);
                    }
                    AggInput::Idle => {}
                }
            }
        }
    }

    close_outbound(&outbound);
    emit_op_audit(
        &ctx.db,
        &ctx.addon_id,
        &ctx.flow_id,
        &ctx.invocation_id,
        &ctx.operator_id,
        "aggregate",
        "completed",
        "ok",
        Some(serde_json::json!({"windows_emitted": windows_emitted, "op": op_str})),
        ctx.org_id.as_deref(),
    );
    Ok(())
}

enum AggInput {
    Record(toml::Value),
    Eof,
    Idle,
}

/// Polls every still-open inbound edge once and returns the first message.
/// `Idle` indicates no edge had a message ready and every poll was satisfied
/// only by the EOF flag check — the caller treats it as a no-op tick.
async fn recv_any(
    inbound: &[Arc<BoundedDropOldest<FlowMessage>>],
    eof_received: &mut [bool],
) -> AggInput {
    // Sequential drain over still-open edges. The first edge that yields a
    // record returns immediately; EOF on an edge marks it and falls through
    // to the next edge. Aggregate typically has one inbound edge — for >1
    // edges the await order biases toward the lower index, which is the
    // same bias the scheduler's other operators use. If every edge is EOF
    // by the end of the scan we return `Eof` so the caller can stop the
    // active counter.
    for (idx, edge) in inbound.iter().enumerate() {
        if eof_received[idx] {
            continue;
        }
        match edge.recv().await {
            Some(FlowMessage::Record(v)) => return AggInput::Record(v),
            Some(FlowMessage::Eof) | None => {
                eof_received[idx] = true;
                return AggInput::Eof;
            }
        }
    }
    AggInput::Idle
}
