// =============================================================================
// File: flow_runtime/operators/predict.rs — alias-gated model inference call
// =============================================================================
//
// Per-record dispatch over `services::service_call::dispatch`. The operator
// owns the alias resolve and the response→record merge; the dispatch layer
// owns permission/rate-limit/audit/pickup-token semantics.
//
// The alias resolve happens per record (not once at start) so a midstream
// alias revoke is observed on the very next record — closing a TOCTOU
// window where `dispatch` would otherwise fall back to a same-named live
// service when the alias becomes `Ok(None)`. The first record's resolve
// also serves as the fail-fast check on bad params.
//
// Errors are classified once:
//   * Alias not found / inactive  → `AliasNotFound` / `AliasInactive`
//   * Service permission denied   → `AliasInactive` (per design wording)
//   * Anything else               → `ServiceCallFailed` with the dispatch
//                                   error rendered into the message.
//
// On a per-record failure the `on_error` policy decides whether to fail the
// flow, skip the record, or emit a `prediction = {}` placeholder downstream.

use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, next_record, read_param_string, timeout_ms_from_params,
    toml_to_json, with_timeout, OnError, OperatorContext, OperatorError, OutboundEdge,
};
use crate::db::repository::AliasPermissionDenied;
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;
use crate::services::service_call::{dispatch, ServiceCallError, ServiceCallRequest};

pub async fn run(
    ctx: OperatorContext,
    inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    let alias = read_param_string(&ctx.params, "alias")
        .ok_or_else(|| OperatorError::BadParams("predict: 'alias' required".to_string()))?;
    let on_error = OnError::from_params(&ctx.params, OnError::Fail);
    let timeout_ms = timeout_ms_from_params(&ctx.params);

    let caller = ctx.caller();
    let mut eof_received = vec![false; inbound.len()];
    let mut ok_count: u64 = 0;
    let mut err_count: u64 = 0;

    loop {
        let msg = next_record(&inbound, &mut eof_received, &cancel).await;
        match msg {
            None => break,
            Some(Err(())) => {
                close_outbound(&outbound);
                return Ok(());
            }
            Some(Ok(record)) => {
                // Per-record alias resolve closes the TOCTOU window: a
                // revoke that lands between record N and N+1 must reject
                // record N+1 even though the operator already started.
                // `Ok(None)` from resolve means the alias no longer exists
                // — without this check `dispatch` would treat the name as
                // a concrete service and call it directly.
                match crate::db::repository::resolve_model_alias_for_addon(
                    &ctx.db,
                    &alias,
                    Some(&ctx.addon_id),
                    Some("flow.op.predict"),
                    None,
                ) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        err_count += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "predict",
                            "alias_check_failed",
                            "error",
                            Some(serde_json::json!({
                                "reason": "alias_not_found",
                                "alias": alias,
                            })),
                            ctx.org_id.as_deref(),
                        );
                        match on_error {
                            OnError::Skip => continue,
                            OnError::EmitNull => {
                                let mut wrap = match record {
                                    toml::Value::Table(t) => t,
                                    other => {
                                        let mut t = toml::value::Table::new();
                                        t.insert("input".to_string(), other);
                                        t
                                    }
                                };
                                wrap.insert(
                                    "prediction".to_string(),
                                    toml::Value::Table(toml::value::Table::new()),
                                );
                                for (_, edge) in &outbound {
                                    edge.send(FlowMessage::Record(toml::Value::Table(
                                        wrap.clone(),
                                    )));
                                }
                                continue;
                            }
                            OnError::Fail => {
                                close_outbound(&outbound);
                                return Err(OperatorError::AliasNotFound(alias));
                            }
                        }
                    }
                    Err(e) => {
                        if e.downcast_ref::<AliasPermissionDenied>().is_some() {
                            err_count += 1;
                            emit_op_audit(
                                &ctx.db,
                                &ctx.addon_id,
                                &ctx.flow_id,
                                &ctx.invocation_id,
                                &ctx.operator_id,
                                "predict",
                                "alias_check_failed",
                                "error",
                                Some(serde_json::json!({
                                    "reason": "permission_denied",
                                    "alias": alias,
                                })),
                                ctx.org_id.as_deref(),
                            );
                            match on_error {
                                OnError::Skip => continue,
                                OnError::EmitNull => {
                                    let mut wrap = match record {
                                        toml::Value::Table(t) => t,
                                        other => {
                                            let mut t = toml::value::Table::new();
                                            t.insert("input".to_string(), other);
                                            t
                                        }
                                    };
                                    wrap.insert(
                                        "prediction".to_string(),
                                        toml::Value::Table(toml::value::Table::new()),
                                    );
                                    for (_, edge) in &outbound {
                                        edge.send(FlowMessage::Record(toml::Value::Table(
                                            wrap.clone(),
                                        )));
                                    }
                                    continue;
                                }
                                OnError::Fail => {
                                    close_outbound(&outbound);
                                    return Err(OperatorError::AliasInactive(alias));
                                }
                            }
                        }
                        close_outbound(&outbound);
                        return Err(OperatorError::Internal(format!("alias_gate: {e}")));
                    }
                }

                let payload_json = toml_to_json(&record).to_string();
                let req = ServiceCallRequest {
                    caller: caller.clone(),
                    service_name: alias.clone(),
                    payload_json,
                    timeout_ms,
                    // Predict mints alias-gated calls; dispatch must reject
                    // Ok(None) (treat as concrete service) so a same-named
                    // live service cannot be reached if the alias was
                    // revoked between Predict's preflight resolve and
                    // dispatch's own resolve.
                    alias_required: true,
                };
                let started = Instant::now();
                let sm_ref = ctx.service_manager.as_ref();
                let executor_ref = ctx.executor.as_ref();
                let call_fut = dispatch(
                    req,
                    &ctx.db,
                    sm_ref,
                    executor_ref,
                    Some(&ctx.permission_checker),
                    &ctx.permissions,
                );
                let outcome = with_timeout(timeout_ms, call_fut).await;
                let duration_ms = started.elapsed().as_millis() as i64;

                match outcome {
                    Ok(Ok(resp)) => {
                        let parsed: serde_json::Value = serde_json::from_str(&resp.response_json)
                            .unwrap_or_else(|_| {
                                serde_json::Value::String(resp.response_json.clone())
                            });
                        let mut record = record;
                        if let toml::Value::Table(ref mut t) = record {
                            t.insert("prediction".to_string(), super::json_to_toml(&parsed));
                        } else {
                            // If the inbound record is not a table we wrap it
                            // so the prediction field is reachable downstream.
                            let mut wrap = toml::value::Table::new();
                            wrap.insert("input".to_string(), record);
                            wrap.insert("prediction".to_string(), super::json_to_toml(&parsed));
                            record = toml::Value::Table(wrap);
                        }
                        for (_, edge) in &outbound {
                            edge.send(FlowMessage::Record(record.clone()));
                        }
                        ok_count += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "predict",
                            "ok",
                            "ok",
                            Some(serde_json::json!({"alias": alias, "duration_ms": duration_ms})),
                            ctx.org_id.as_deref(),
                        );
                    }
                    Ok(Err(err)) => {
                        err_count += 1;
                        let mapped = match &err {
                            ServiceCallError::AliasPermission { .. } => {
                                OperatorError::AliasInactive(alias.clone())
                            }
                            ServiceCallError::NotFound { .. } => {
                                OperatorError::AliasNotFound(alias.clone())
                            }
                            other => OperatorError::ServiceCallFailed(other.to_string()),
                        };
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "predict",
                            "error",
                            "error",
                            Some(serde_json::json!({
                                "alias": alias,
                                "reason": err.to_string(),
                            })),
                            ctx.org_id.as_deref(),
                        );
                        match on_error {
                            OnError::Skip => continue,
                            OnError::EmitNull => {
                                let mut wrap = match record {
                                    toml::Value::Table(t) => t,
                                    other => {
                                        let mut t = toml::value::Table::new();
                                        t.insert("input".to_string(), other);
                                        t
                                    }
                                };
                                wrap.insert(
                                    "prediction".to_string(),
                                    toml::Value::Table(toml::value::Table::new()),
                                );
                                for (_, edge) in &outbound {
                                    edge.send(FlowMessage::Record(toml::Value::Table(
                                        wrap.clone(),
                                    )));
                                }
                            }
                            OnError::Fail => {
                                close_outbound(&outbound);
                                return Err(mapped);
                            }
                        }
                    }
                    Err(timeout_err) => {
                        err_count += 1;
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "predict",
                            "error",
                            "error",
                            Some(serde_json::json!({"alias": alias, "reason": "timeout"})),
                            ctx.org_id.as_deref(),
                        );
                        match on_error {
                            OnError::Skip => continue,
                            OnError::EmitNull => {
                                let mut wrap = toml::value::Table::new();
                                wrap.insert("input".to_string(), record);
                                wrap.insert(
                                    "prediction".to_string(),
                                    toml::Value::Table(toml::value::Table::new()),
                                );
                                for (_, edge) in &outbound {
                                    edge.send(FlowMessage::Record(toml::Value::Table(
                                        wrap.clone(),
                                    )));
                                }
                            }
                            OnError::Fail => {
                                close_outbound(&outbound);
                                return Err(timeout_err);
                            }
                        }
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
        "predict",
        "completed",
        "ok",
        Some(serde_json::json!({"ok_count": ok_count, "err_count": err_count})),
        ctx.org_id.as_deref(),
    );
    Ok(())
}
