// =============================================================================
// File: flow_runtime/operators/threshold.rs — numeric range filter
// =============================================================================
//
// Drops records whose `field` does not fall inside the [`min`, `max`] window.
// Both bounds are optional — supply only `min` for a lower gate, only `max`
// for an upper gate. Records without the field or with a non-numeric value
// are dropped (NOT a flow failure) and the drop is collapsed-audited.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, next_record, read_param_f64, read_param_string,
    record_field_dot, OperatorContext, OperatorError, OutboundEdge,
};
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;

pub async fn run(
    ctx: OperatorContext,
    inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    let field = read_param_string(&ctx.params, "field")
        .ok_or_else(|| OperatorError::BadParams("threshold: 'field' required".to_string()))?;
    let min = read_param_f64(&ctx.params, "min");
    let max = read_param_f64(&ctx.params, "max");

    let mut eof_received = vec![false; inbound.len()];
    let mut passed: u64 = 0;
    let mut dropped: u64 = 0;
    loop {
        let msg = next_record(&inbound, &mut eof_received, &cancel).await;
        match msg {
            None => break,
            Some(Err(())) => {
                close_outbound(&outbound);
                return Ok(());
            }
            Some(Ok(record)) => {
                let val_opt = record_field_dot(&record, &field);
                let numeric = match val_opt {
                    None => {
                        dropped += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "threshold",
                            "field_missing",
                            "drop",
                            Some(serde_json::json!({"field": field})),
                            ctx.org_id.as_deref(),
                        );
                        continue;
                    }
                    Some(v) => {
                        if let Some(i) = v.as_integer() {
                            Some(i as f64)
                        } else {
                            v.as_float()
                        }
                    }
                };
                let value = match numeric {
                    Some(v) => v,
                    None => {
                        dropped += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "threshold",
                            "drop",
                            "drop",
                            Some(serde_json::json!({"reason": "not_numeric", "field": field})),
                            ctx.org_id.as_deref(),
                        );
                        continue;
                    }
                };
                if let Some(lo) = min {
                    if value < lo {
                        dropped += 1;
                        continue;
                    }
                }
                if let Some(hi) = max {
                    if value > hi {
                        dropped += 1;
                        continue;
                    }
                }
                passed += 1;
                for (_, edge) in &outbound {
                    edge.send(FlowMessage::Record(record.clone()));
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
        "threshold",
        "completed",
        "ok",
        Some(serde_json::json!({"passed": passed, "dropped": dropped})),
        ctx.org_id.as_deref(),
    );
    Ok(())
}
