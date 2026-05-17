// =============================================================================
// File: flow_runtime/operators/source.rs — Source operator (record generator)
// =============================================================================
//
// Generates records into the DAG. Two supported streams in F1c-P5:
//
//   * `input`            — emits `ctx.input_toml` `count` times. Used by the
//                          synchronous `flow_invoke_v1` ABI.
//   * `camera.<id>`      — reserved for F2; rejected with `BadParams`. The
//                          rejection is recorded so a missing camera does
//                          not look like silent success.
//
// `fps` controls inter-record spacing in milliseconds. `fps=0` emits the
// whole batch in a tight loop. Cancellation is honored between records.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, read_param_string, read_param_u32, OperatorContext,
    OperatorError, OutboundEdge,
};
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;

pub async fn run(
    ctx: OperatorContext,
    _inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    // Default to the single-shot "input" generator so a minimal flow with
    // bare `{ "type": "Source" }` works without explicit params.
    let stream = read_param_string(&ctx.params, "stream").unwrap_or_else(|| "input".to_string());
    let count = read_param_u32(&ctx.params, "count").unwrap_or(1);
    let fps = read_param_u32(&ctx.params, "fps").unwrap_or(0);

    if stream.starts_with("camera.") {
        emit_op_audit(
            &ctx.db,
            &ctx.addon_id,
            &ctx.flow_id,
            &ctx.invocation_id,
            &ctx.operator_id,
            "source",
            "error",
            "error",
            Some(serde_json::json!({"reason": "camera_source_unsupported", "stream": stream})),
        );
        close_outbound(&outbound);
        return Err(OperatorError::BadParams(
            "camera.* source not supported in F1c-P5; carried in F2".to_string(),
        ));
    }
    if stream != "input" {
        close_outbound(&outbound);
        return Err(OperatorError::BadParams(format!(
            "source: unknown stream '{stream}'"
        )));
    }

    emit_op_audit(
        &ctx.db,
        &ctx.addon_id,
        &ctx.flow_id,
        &ctx.invocation_id,
        &ctx.operator_id,
        "source",
        "start",
        "ok",
        Some(serde_json::json!({"stream": stream, "count": count, "fps": fps})),
    );

    let sleep_ms = if fps > 0 { 1000u32 / fps.max(1) } else { 0 };
    let mut emitted: u32 = 0;
    for _ in 0..count {
        if cancel.is_cancelled() {
            break;
        }
        for (_, edge) in &outbound {
            edge.send(FlowMessage::Record(ctx.input_toml.clone()));
        }
        emitted += 1;
        if sleep_ms > 0 && emitted < count {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(sleep_ms as u64)) => {}
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
        "source",
        "completed",
        "ok",
        Some(serde_json::json!({"records_emitted": emitted})),
    );
    Ok(())
}
