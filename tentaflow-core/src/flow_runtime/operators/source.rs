// =============================================================================
// File: flow_runtime/operators/source.rs — Source operator (record generator)
// =============================================================================
//
// Generates records into the DAG. Two supported streams:
//
//   * `input`            — emits `ctx.input_toml` `count` times. Used by the
//                          synchronous `flow_invoke_v1` ABI.
//   * `camera.<id>`      — subscribes to the camera ingest streaming bus and
//                          emits one `{ camera_id, ts, raw_ref }` record per
//                          live frame. Ownership is verified per
//                          `(org_id, addon_id, camera_id)` before subscribing.
//
// `fps` controls inter-record spacing in milliseconds. For `input` it is a
// pacing parameter (sleep between emissions). For `camera.<id>` it is a rate
// limit: frames arriving faster than `1000 / fps` ms apart are dropped.
// `fps=0` means "no pacing / no rate limit". Cancellation is honored at every
// await point.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, read_param_string, read_param_u32, OperatorContext,
    OperatorError, OutboundEdge,
};
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;

const CAMERA_STREAM_PREFIX: &str = "camera.";
const MAX_FPS: u32 = 120;
/// Window over which backpressure drops are collapsed into a single audit row.
const BACKPRESSURE_AUDIT_WINDOW: Duration = Duration::from_secs(60);
/// Bounded wait per subscriber poll. Lets the loop interleave drop accounting
/// without spinning when no frames are arriving.
const SUBSCRIBER_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub async fn run(
    ctx: OperatorContext,
    _inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    // Default to the single-shot "input" generator so a minimal flow with
    // bare `{ "type": "Source" }` works without explicit params.
    let stream = read_param_string(&ctx.params, "stream").unwrap_or_else(|| "input".to_string());
    let fps = read_param_u32(&ctx.params, "fps").unwrap_or(0);
    if fps > MAX_FPS {
        close_outbound(&outbound);
        return Err(OperatorError::BadParams(format!(
            "source: fps {fps} exceeds max {MAX_FPS}"
        )));
    }

    if let Some(camera_id) = stream.strip_prefix(CAMERA_STREAM_PREFIX) {
        if camera_id.is_empty() {
            close_outbound(&outbound);
            return Err(OperatorError::BadParams(
                "source: empty camera id after 'camera.' prefix".to_string(),
            ));
        }
        return run_camera_source(ctx, camera_id, fps, outbound, cancel).await;
    }

    if stream != "input" {
        close_outbound(&outbound);
        return Err(OperatorError::BadParams(format!(
            "source: unknown stream '{stream}'"
        )));
    }

    run_input_source(ctx, fps, outbound, cancel).await
}

/// `stream = "input"` — emit `ctx.input_toml` `count` times with optional
/// inter-record pacing. Behaviour is identical to F1c-P5.
async fn run_input_source(
    ctx: OperatorContext,
    fps: u32,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    let count = read_param_u32(&ctx.params, "count").unwrap_or(1);

    emit_op_audit(
        &ctx.db,
        &ctx.addon_id,
        &ctx.flow_id,
        &ctx.invocation_id,
        &ctx.operator_id,
        "source",
        "start",
        "ok",
        Some(serde_json::json!({"stream": "input", "count": count, "fps": fps})),
        ctx.org_id.as_deref(),
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
        ctx.org_id.as_deref(),
    );
    Ok(())
}

/// `stream = "camera.<id>"` — subscribe to the streaming bus and forward frames.
///
/// Hot path:
///   1. Verify the camera belongs to `(org_id, addon_id)` via
///      `get_camera_for_addon`. Cross-tenant lookups return `Ok(None)` and we
///      fail the operator with `camera_not_found_or_not_in_org`.
///   2. Subscribe to `services::streaming_bus()` and run a `select!` loop:
///      cancel | next message | 60 s audit tick.
///   3. For each `Frame`, optionally rate-limit (fps), then emit a record
///      `{ camera_id, ts, raw_ref }` to every outbound edge. Drop signals are
///      accumulated and folded into a collapsed audit row every 60 s.
///   4. `CameraOffline` ends the loop (camera was removed); the operator
///      completes successfully.
///
/// The subscriber is automatically pruned from the bus when its `rx` is dropped
/// (the bus detects `Closed` on the next `try_send`); there is no explicit
/// `unsubscribe` call needed in the happy path.
#[cfg(feature = "camera")]
async fn run_camera_source(
    ctx: OperatorContext,
    camera_id: &str,
    fps: u32,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    use crate::db::repository::get_camera_for_addon;
    use crate::services::streaming::{NextOutcome, StreamFilter, StreamMessage};

    // Tenant + ownership guard. `get_camera_for_addon` matches on
    // `(owner_addon_id, camera_id, org_id, removed_at IS NULL)` so a cross-org
    // or cross-addon lookup returns `Ok(None)` and we deny here.
    match get_camera_for_addon(&ctx.db, &ctx.addon_id, camera_id, ctx.org_id.as_deref()) {
        Ok(Some(_)) => {}
        Ok(None) => {
            emit_op_audit(
                &ctx.db,
                &ctx.addon_id,
                &ctx.flow_id,
                &ctx.invocation_id,
                &ctx.operator_id,
                "source",
                "error",
                "denied",
                Some(serde_json::json!({
                    "reason": "camera_not_found_or_not_in_org",
                    "camera_id": camera_id,
                })),
                ctx.org_id.as_deref(),
            );
            close_outbound(&outbound);
            return Err(OperatorError::Internal(
                "camera_not_found_or_not_in_org".to_string(),
            ));
        }
        Err(e) => {
            emit_op_audit(
                &ctx.db,
                &ctx.addon_id,
                &ctx.flow_id,
                &ctx.invocation_id,
                &ctx.operator_id,
                "source",
                "error",
                "error",
                Some(serde_json::json!({
                    "reason": "camera_lookup_failed",
                    "camera_id": camera_id,
                    "error": e.to_string(),
                })),
                ctx.org_id.as_deref(),
            );
            close_outbound(&outbound);
            return Err(OperatorError::Internal(format!(
                "camera lookup failed: {e}"
            )));
        }
    }

    let bus = crate::services::streaming_bus();
    let mut subscriber = bus.subscribe(camera_id, StreamFilter::default());
    // Snapshot the bus-assigned id so we can include it in audit but the
    // subscriber itself stays mutable for `next` calls.
    let stream_id = subscriber.stream_id.to_string();

    emit_op_audit(
        &ctx.db,
        &ctx.addon_id,
        &ctx.flow_id,
        &ctx.invocation_id,
        &ctx.operator_id,
        "source",
        "subscribe",
        "ok",
        Some(serde_json::json!({
            "camera_id": camera_id,
            "stream_id": stream_id,
            "fps": fps,
        })),
        ctx.org_id.as_deref(),
    );

    // Rate-limit window between emissions. `0` disables rate limiting. We
    // compute the interval in nanoseconds (sub-ms precision) so high fps
    // targets do not round down to a coarser ms interval — `1000/120 = 8 ms`
    // would otherwise permit ~125 fps on a 120-fps target.
    let min_emit_interval = if fps > 0 {
        Some(Duration::from_nanos(
            1_000_000_000u64 / u64::from(fps.max(1)),
        ))
    } else {
        None
    };
    let mut last_emit_at: Option<tokio::time::Instant> = None;

    // Collapsed backpressure accounting. We flush every 60 s if drops > 0.
    let mut dropped_in_window: u64 = 0;
    let mut rate_limit_skipped: u64 = 0;
    let mut audit_tick = tokio::time::interval(BACKPRESSURE_AUDIT_WINDOW);
    audit_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first tick fires immediately by default — burn it so we don't emit
    // a zero-row right after subscribe.
    audit_tick.tick().await;

    let mut emitted: u64 = 0;
    #[allow(unused_assignments)]
    let mut completed_reason: &'static str = "cancelled";

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                completed_reason = "cancelled";
                break;
            }
            _ = audit_tick.tick() => {
                if dropped_in_window > 0 || rate_limit_skipped > 0 {
                    emit_op_audit(
                        &ctx.db,
                        &ctx.addon_id,
                        &ctx.flow_id,
                        &ctx.invocation_id,
                        &ctx.operator_id,
                        "source",
                        "backpressure_drop",
                        "denied",
                        Some(serde_json::json!({
                            "camera_id": camera_id,
                            "stream_id": stream_id,
                            "dropped_count": dropped_in_window,
                            "rate_limit_skipped": rate_limit_skipped,
                            "window_secs": BACKPRESSURE_AUDIT_WINDOW.as_secs(),
                        })),
                        ctx.org_id.as_deref(),
                    );
                    dropped_in_window = 0;
                    rate_limit_skipped = 0;
                }
            }
            outcome = subscriber.next(SUBSCRIBER_POLL_INTERVAL) => {
                match outcome {
                    NextOutcome::Message(StreamMessage::Frame { frame_ref, metadata }) => {
                        // fps rate-limit: drop frames arriving inside the
                        // minimum window. Counted separately from bus drops so
                        // the audit row is diagnostic-friendly.
                        if let Some(min_iv) = min_emit_interval {
                            let now = tokio::time::Instant::now();
                            if let Some(prev) = last_emit_at {
                                if now.duration_since(prev) < min_iv {
                                    rate_limit_skipped += 1;
                                    continue;
                                }
                            }
                            last_emit_at = Some(now);
                        }
                        let record = build_frame_record(&metadata, &frame_ref);
                        for (_, edge) in &outbound {
                            edge.send(FlowMessage::Record(record.clone()));
                        }
                        emitted += 1;
                    }
                    NextOutcome::Message(StreamMessage::Drop { count }) => {
                        dropped_in_window = dropped_in_window.saturating_add(count);
                    }
                    NextOutcome::Message(StreamMessage::CameraOffline { reason }) => {
                        emit_op_audit(
                            &ctx.db,
                            &ctx.addon_id,
                            &ctx.flow_id,
                            &ctx.invocation_id,
                            &ctx.operator_id,
                            "source",
                            "camera_offline",
                            "ok",
                            Some(serde_json::json!({
                                "camera_id": camera_id,
                                "stream_id": stream_id,
                                "reason": reason,
                            })),
                            ctx.org_id.as_deref(),
                        );
                        completed_reason = "camera_offline";
                        break;
                    }
                    NextOutcome::Closed => {
                        completed_reason = "subscriber_closed";
                        break;
                    }
                    NextOutcome::Timeout => {
                        // No frame this slice — loop back to the select so
                        // cancel/audit_tick get a chance to fire.
                    }
                }
            }
        }
    }

    // Final flush — pick up any drops still sitting in the subscriber's
    // internal counter that never made it across as a `StreamMessage::Drop`
    // (e.g. cancellation arrived first) and combine them with the locally
    // accumulated window before the audit row is written.
    dropped_in_window = dropped_in_window.saturating_add(subscriber.dropped_pending());

    if dropped_in_window > 0 || rate_limit_skipped > 0 {
        emit_op_audit(
            &ctx.db,
            &ctx.addon_id,
            &ctx.flow_id,
            &ctx.invocation_id,
            &ctx.operator_id,
            "source",
            "backpressure_drop",
            "denied",
            Some(serde_json::json!({
                "camera_id": camera_id,
                "stream_id": stream_id,
                "dropped_count": dropped_in_window,
                "rate_limit_skipped": rate_limit_skipped,
                "window_secs": BACKPRESSURE_AUDIT_WINDOW.as_secs(),
                "partial_window": true,
            })),
            ctx.org_id.as_deref(),
        );
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
        Some(serde_json::json!({
            "camera_id": camera_id,
            "stream_id": stream_id,
            "records_emitted": emitted,
            "reason": completed_reason,
        })),
        ctx.org_id.as_deref(),
    );

    // Explicit unsubscribe — without this, a quiet camera with no further
    // broadcast traffic would leave a stale entry in StreamingBus until the
    // next publish triggers the Closed-prune path. Cancellation on a silent
    // stream is a common case, so we eagerly clean up.
    crate::services::streaming_bus().unsubscribe(&camera_id, &subscriber.stream_id);
    drop(subscriber);
    Ok(())
}

/// Builds the per-frame record shape consumed by downstream operators.
#[cfg(feature = "camera")]
fn build_frame_record(
    metadata: &crate::services::frame_storage::FrameMetadata,
    frame_ref: &crate::services::frame_storage::RawFrameRef,
) -> toml::Value {
    let mut t = toml::value::Table::new();
    t.insert(
        "camera_id".to_string(),
        toml::Value::String(metadata.camera_id.clone()),
    );
    t.insert(
        "ts".to_string(),
        toml::Value::Integer(metadata.timestamp_unix_ms as i64),
    );
    t.insert(
        "raw_ref".to_string(),
        toml::Value::String(frame_ref.as_str().to_string()),
    );
    t.insert(
        "width".to_string(),
        toml::Value::Integer(metadata.width as i64),
    );
    t.insert(
        "height".to_string(),
        toml::Value::Integer(metadata.height as i64),
    );
    toml::Value::Table(t)
}

/// When the `camera` feature is off, camera sources are unsupported. We keep
/// the operator returning a clear error so an installed flow that targets a
/// camera fails fast rather than silently emitting nothing.
#[cfg(not(feature = "camera"))]
async fn run_camera_source(
    ctx: OperatorContext,
    camera_id: &str,
    _fps: u32,
    outbound: Vec<OutboundEdge>,
    _cancel: CancellationToken,
) -> Result<(), OperatorError> {
    emit_op_audit(
        &ctx.db,
        &ctx.addon_id,
        &ctx.flow_id,
        &ctx.invocation_id,
        &ctx.operator_id,
        "source",
        "error",
        "error",
        Some(serde_json::json!({
            "reason": "camera_feature_disabled",
            "camera_id": camera_id,
        })),
        ctx.org_id.as_deref(),
    );
    close_outbound(&outbound);
    Err(OperatorError::BadParams(
        "camera.* source requires the 'camera' feature".to_string(),
    ))
}
