// =============================================================================
// File: flow_runtime/operators/sink.rs — terminal side-effect operator
// =============================================================================
//
// Sink writes records out of the DAG. Four kinds are supported in F1c-P5:
//
//   * `invocation_result` — append to the per-invocation Vec the scheduler
//     encodes into `flow_invocations.result_toml` on completion.
//   * `event`              — `event_publish::publish_event` on the global bus.
//   * `sql_exec`           — `storage_sql_exec::exec_for_addon` (DML). Requires
//                            manifest permission `sql.write`.
//   * `ui_notify`          — publishes a `"ui.notification"` event carrying
//                            the per-record payload plus `level`/`message`.
//
// Per design, every per-record failure is `Skip` — Sink errors are audited
// and the flow keeps running. A missing required parameter or a bad `kind`
// IS a fail (caught before any record is pulled).

use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, next_record, read_param_string, toml_to_json,
    OperatorContext, OperatorError, OutboundEdge,
};
use crate::addon::event_bus::Event;
use crate::addon::event_publish::publish_event;
use crate::addon::storage_sql_exec::exec_for_addon;
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SinkKind {
    InvocationResult,
    Event,
    SqlExec,
    UiNotify,
}

fn parse_kind(s: &str) -> Option<SinkKind> {
    match s {
        "invocation_result" => Some(SinkKind::InvocationResult),
        "event" => Some(SinkKind::Event),
        "sql_exec" => Some(SinkKind::SqlExec),
        "ui_notify" => Some(SinkKind::UiNotify),
        _ => None,
    }
}

pub async fn run(
    ctx: OperatorContext,
    inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    let kind_str = read_param_string(&ctx.params, "kind")
        .unwrap_or_else(|| "invocation_result".to_string());
    let kind = parse_kind(&kind_str)
        .ok_or_else(|| OperatorError::BadParams(format!("sink: unknown kind '{kind_str}'")))?;

    // Kind-specific param validation up front so a missing topic / query
    // fails the flow before pulling any record.
    let topic = match kind {
        SinkKind::Event => Some(read_param_string(&ctx.params, "topic").ok_or_else(|| {
            OperatorError::BadParams("sink event: 'topic' required".to_string())
        })?),
        _ => None,
    };
    let query = match kind {
        SinkKind::SqlExec => {
            if !ctx.permissions.iter().any(|p| p == "sql.write") {
                return Err(OperatorError::SinkFailed(
                    "sink sql_exec: missing 'sql.write' permission".to_string(),
                ));
            }
            Some(read_param_string(&ctx.params, "query").ok_or_else(|| {
                OperatorError::BadParams("sink sql_exec: 'query' required".to_string())
            })?)
        }
        _ => None,
    };
    let ui_level = match kind {
        SinkKind::UiNotify => Some(
            read_param_string(&ctx.params, "level").unwrap_or_else(|| "info".to_string()),
        ),
        _ => None,
    };
    let ui_message = match kind {
        SinkKind::UiNotify => Some(read_param_string(&ctx.params, "message").ok_or_else(|| {
            OperatorError::BadParams("sink ui_notify: 'message' required".to_string())
        })?),
        _ => None,
    };

    if matches!(kind, SinkKind::Event | SinkKind::UiNotify) && ctx.event_bus.is_none() {
        return Err(OperatorError::SubsystemNotInitialized(
            "event_bus".to_string(),
        ));
    }

    let mut eof_received = vec![false; inbound.len()];
    let mut ok_count: u64 = 0;
    let mut err_count: u64 = 0;
    loop {
        let msg = next_record(&inbound, &mut eof_received, &cancel).await;
        match msg {
            None => break,
            Some(Err(())) => break,
            Some(Ok(record)) => {
                let outcome = match kind {
                    SinkKind::InvocationResult => {
                        let mut g = ctx.sink_outputs.lock().await;
                        g.push(record);
                        Ok(())
                    }
                    SinkKind::Event => {
                        let bus = ctx.event_bus.as_ref().unwrap();
                        let topic = topic.as_deref().unwrap();
                        let payload = toml_to_json(&record);
                        publish_event(
                            bus,
                            &ctx.db,
                            &ctx.caller(),
                            Some(&ctx.permission_checker),
                            &ctx.permissions,
                            topic,
                            payload,
                        )
                        .map_err(|e| e.to_string())
                    }
                    SinkKind::SqlExec => {
                        let q = query.as_deref().unwrap();
                        let substituted = substitute_placeholders(q, &record);
                        match exec_for_addon(&ctx.addon_id, &substituted.0, &substituted.1) {
                            Ok(_) => Ok(()),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    SinkKind::UiNotify => {
                        let bus = ctx.event_bus.as_ref().unwrap();
                        let payload = json!({
                            "level": ui_level.as_deref().unwrap_or("info"),
                            "message": ui_message.as_deref().unwrap_or(""),
                            "record": toml_to_json(&record),
                        });
                        // ui_notify bypasses the per-event permission resolve
                        // because `ui.notification` is a runtime-owned topic
                        // (not a user-declared event). The audit row for the
                        // sink is written by `emit_op_audit` below.
                        let event = Event {
                            event_type: "ui.notification".to_string(),
                            source_addon: Some(ctx.addon_id.clone()),
                            source_user: None,
                            payload,
                            timestamp: chrono::Utc::now(),
                        };
                        bus.publish(event);
                        Ok(())
                    }
                };
                match outcome {
                    Ok(()) => {
                        ok_count += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "sink",
                            "ok",
                            "ok",
                            Some(json!({"kind": kind_str})),
                        );
                    }
                    Err(e) => {
                        err_count += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "sink",
                            "error",
                            "error",
                            Some(json!({"kind": kind_str, "reason": e})),
                        );
                        // PM rule: Sink always Skip on error — flow continues.
                    }
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
        "sink",
        "completed",
        "ok",
        Some(json!({"kind": kind_str, "ok_count": ok_count, "err_count": err_count})),
    );
    Ok(())
}

/// Replaces every `:name` placeholder in `query` with `?` and returns the
/// rewritten query plus the bound JSON value sequence in the same order.
/// Placeholder names map to record table keys; missing keys bind `null`.
fn substitute_placeholders(query: &str, record: &toml::Value) -> (String, Vec<serde_json::Value>) {
    let bytes = query.as_bytes();
    let mut out = String::with_capacity(query.len());
    let mut params: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
        {
            let mut end = i + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let name = &query[i + 1..end];
            let val = record
                .get(name)
                .map(toml_to_json)
                .unwrap_or(serde_json::Value::Null);
            params.push(val);
            out.push('?');
            i = end;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    (out, params)
}
