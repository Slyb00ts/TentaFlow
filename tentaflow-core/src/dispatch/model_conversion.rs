// =============================================================================
// File: dispatch/model_conversion.rs — TF→ONNX model-conversion orchestration
// =============================================================================
//
// ROADMAP Z11. Deploying a TensorFlow model (SavedModel/H5) first has to go
// through the `tf-onnx-converter` python-bundle service
// (`tentaflow-containers/tools/_services/tf-onnx-converter.toml`). Conversion
// runs the SAME async START + POLL contract already shipped for the
// PyTorch→ONNX LLM export (`dispatch/ml_studio.rs::ml_studio_ft_export_start`
// / `_status` + `ml_studio/export_llm.rs`): the start handler kicks off a
// background task and answers immediately with `status="converting"`; the
// status handler is a cheap read of the last known state, polled by the
// wizard. Nothing here invents a new contract shape.
//
// State lives in `services.config_json` of the TARGET service row (the row
// the deploy wizard already created for the model being converted) — reused
// per the ZADANIA.md Z11 allocation, no new table. Merging preserves
// unrelated keys already in the JSON (mirrors
// `ml_studio::export_llm::merge_export_state`).
//
// Numeric-compatibility tolerance: `test_input_path` (a `.npy` file holding
// ONE real sample input) travels with the start request. When present, the
// converter service runs the original TF model and the converted ONNX model
// on that SAME real test input and reports the measured `max_abs_diff` — Core
// never fabricates a comparison. The PASS/FAIL decision against the
// caller-supplied `tolerance` is Core's job (`evaluate_tolerance` below),
// which is why that decision is a small pure function and is unit-tested
// directly as well as through `run_conversion` against a mock converter
// (`spawn_conversion` / `run_conversion` tests below). A conversion that
// succeeds but diverges beyond tolerance is recorded as `failed` (reason in
// `error`) — a real failure that must route the wizard to the TF-serving
// fallback, not a warning glossed over as "succeeded". When `test_input_path`
// is absent, the conversion may still finish `succeeded`, but
// `validated=false` is written to `services.config_json` and returned in
// `ModelConversionStatusResponse` explicitly — there is no silent "succeeded
// with validation" state (ZADANIA.md Z11 pitfall #2).
// =============================================================================

use std::time::Duration;

use serde_json::{json, Value};
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ModelConversionPayload, ModelConversionStartRequest, ModelConversionStartResponse,
    ModelConversionStatusRequest, ModelConversionStatusResponse, ProtocolError, ProtocolErrorCode,
    SessionAuth,
};

use super::HandlerContext;
use crate::db::repository;
use crate::services_repo;

/// Category + engine id the `tf-onnx-converter` python-bundle service
/// registers under (`tentaflow-containers/tools/_services/tf-onnx-converter.toml`).
const CONVERTER_CATEGORY: &str = "tools";
const CONVERTER_ENGINE_ID: &str = "tf-onnx-converter";

/// Interval between `GET /convert_status/{id}` polls.
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Hard cap on the whole conversion (model load + graph conversion +
/// numeric-compatibility check) so a wedged job cannot leave the target
/// service in `converting` forever.
const JOB_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// Timeout for a single HTTP request (start / one status poll).
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

const ALLOWED_SOURCE_FORMATS: [&str; 2] = ["tensorflow_savedmodel", "tensorflow_h5"];
const ALLOWED_PRECISIONS: [&str; 2] = ["fp32", "fp16"];

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("model conversion database error: {}", e))
}

/// `audit_log.resource` for a conversion event. `<kind>:<id>` is the repo-wide
/// convention (`apikey:`, `cluster:`, `node:`, `session:`, and `service:` from
/// the supervisor's own restart audit), so a `services` row is addressable the
/// same way regardless of which subsystem wrote the entry.
fn audit_resource(service_id: i64) -> String {
    format!("service:{}", service_id)
}

fn user_uuid(ctx: &HandlerContext) -> Option<String> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).to_string())
        }
        _ => None,
    }
}

/// Whether a measured max-abs-diff satisfies a tolerance. Boundary is
/// inclusive (`diff == tolerance` passes, matching the wizard copy "within
/// tolerance"). A non-finite or negative diff never passes — a converter bug
/// that produced garbage numbers must not silently read as "close enough".
/// `tolerance` itself is validated non-negative at the wire boundary
/// (`validate_start_request`), so it is trusted here.
fn evaluate_tolerance(max_abs_diff: f64, tolerance: f64) -> bool {
    max_abs_diff.is_finite() && max_abs_diff >= 0.0 && max_abs_diff <= tolerance
}

fn validate_start_request(req: &ModelConversionStartRequest) -> Result<(), ProtocolError> {
    if req.source_path.trim().is_empty() {
        return Err(ProtocolError::bad_request("source_path is required"));
    }
    if !ALLOWED_SOURCE_FORMATS.contains(&req.source_format.as_str()) {
        return Err(ProtocolError::bad_request(
            "source_format: tensorflow_savedmodel|tensorflow_h5",
        ));
    }
    if !ALLOWED_PRECISIONS.contains(&req.precision.as_str()) {
        return Err(ProtocolError::bad_request("precision: fp32|fp16"));
    }
    if !req.tolerance.is_finite() || req.tolerance < 0.0 {
        return Err(ProtocolError::bad_request(
            "tolerance must be a finite, non-negative number",
        ));
    }
    if let Some(path) = &req.test_input_path {
        if path.trim().is_empty() {
            return Err(ProtocolError::bad_request(
                "test_input_path must not be empty when provided",
            ));
        }
    }
    Ok(())
}

/// Merges conversion state into an existing `services.config_json` object,
/// preserving unrelated keys already stored there (deploy params, secrets,
/// …). Mirrors `ml_studio::export_llm::merge_export_state`.
#[allow(clippy::too_many_arguments)]
fn merge_conversion_state(
    current_config_json: &str,
    status: &str,
    source_format: Option<&str>,
    precision: Option<&str>,
    tolerance: Option<f64>,
    onnx_path: Option<&str>,
    max_abs_diff: Option<f64>,
    tolerance_passed: Option<bool>,
    error: Option<&str>,
    validated: bool,
) -> String {
    let mut root = match serde_json::from_str::<Value>(current_config_json) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    };
    let obj = root
        .as_object_mut()
        .expect("root is an object by construction");
    obj.insert("conversion_status".to_string(), json!(status));
    if let Some(v) = source_format {
        obj.insert("source_format".to_string(), json!(v));
    }
    if let Some(v) = precision {
        obj.insert("conversion_precision".to_string(), json!(v));
    }
    if let Some(v) = tolerance {
        obj.insert("conversion_tolerance".to_string(), json!(v));
    }
    obj.insert("onnx_path".to_string(), json!(onnx_path));
    obj.insert("max_abs_diff".to_string(), json!(max_abs_diff));
    obj.insert("tolerance_passed".to_string(), json!(tolerance_passed));
    obj.insert("converted".to_string(), json!(status == "succeeded"));
    obj.insert("conversion_error".to_string(), json!(error));
    // Explicit, always written — never left absent/null so a `succeeded`
    // conversion that skipped the numeric check (no `test_input_path`) cannot
    // be mistaken for a validated one downstream.
    obj.insert("validated".to_string(), json!(validated));
    root.to_string()
}

// =============================================================================
// Handlers
// =============================================================================

#[handler(variant = "ModelConversionStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_conversion_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelConversionStartRequest",
            ))
        }
    };
    start(ctx, payload)
}

fn start(
    ctx: &HandlerContext,
    payload: &ModelConversionStartRequest,
) -> Result<MessageBody, ProtocolError> {
    validate_start_request(payload)?;

    let row = {
        let conn = ctx
            .state
            .db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        services_repo::services::get(&conn, payload.service_id).map_err(db_err)?
    };
    let row =
        row.ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "service not found"))?;

    // Preflight: the converter must be reachable BEFORE the row is claimed
    // for `converting` — otherwise a missing converter would leave the target
    // stuck in `converting` forever (nothing would ever run to flip it back).
    {
        let conn = ctx
            .state
            .db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        let converter = services_repo::services::list_by_category(
            &conn,
            CONVERTER_CATEGORY,
            Some(CONVERTER_ENGINE_ID),
        )
        .map_err(db_err)?;
        if converter.is_empty() {
            return Err(ProtocolError::bad_request(
                "tf-onnx-converter service is not running — deploy it in Services first",
            ));
        }
    }

    let running_config = merge_conversion_state(
        &row.config_json,
        "converting",
        Some(&payload.source_format),
        Some(&payload.precision),
        Some(payload.tolerance),
        None,
        None,
        None,
        None,
        false,
    );
    {
        let conn = ctx
            .state
            .db
            .write()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        services_repo::services::update_config_json(&conn, payload.service_id, &running_config)
            .map_err(db_err)?;
    }

    let user_id = user_uuid(ctx);
    let _ = repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "model_conversion.started",
        Some(&audit_resource(payload.service_id)),
        Some(
            &json!({
                "source_format": payload.source_format,
                "precision": payload.precision,
                "tolerance": payload.tolerance,
            })
            .to_string(),
        ),
        None,
        Some(&ctx.state.local_node_id),
    );

    spawn_conversion(
        payload.service_id,
        payload.source_path.clone(),
        payload.source_format.clone(),
        payload.precision.clone(),
        payload.tolerance,
        payload.test_input_path.clone(),
        running_config,
        user_id,
    );

    Ok(MessageBody::ModelConversionBody(
        ModelConversionPayload::StartResponse(ModelConversionStartResponse {
            service_id: payload.service_id,
            status: "converting".to_string(),
        }),
    ))
}

#[handler(variant = "ModelConversionStatusRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn model_conversion_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ModelConversionBody(ModelConversionPayload::StatusRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected ModelConversionStatusRequest",
            ))
        }
    };
    status(ctx, payload)
}

fn status(
    ctx: &HandlerContext,
    payload: &ModelConversionStatusRequest,
) -> Result<MessageBody, ProtocolError> {
    let row = {
        let conn = ctx
            .state
            .db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        services_repo::services::get(&conn, payload.service_id).map_err(db_err)?
    };
    let row =
        row.ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "service not found"))?;

    let config: Value = serde_json::from_str(&row.config_json).unwrap_or_else(|_| json!({}));
    let status = config
        .get("conversion_status")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();
    let onnx_path = config
        .get("onnx_path")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let max_abs_diff = config.get("max_abs_diff").and_then(|v| v.as_f64());
    let tolerance_passed = config.get("tolerance_passed").and_then(|v| v.as_bool());
    let error = config
        .get("conversion_error")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // Absent (peer/state predates the field) reads as unvalidated, never a
    // silent pass — same default as the wire `#[serde(default)]`.
    let validated = config
        .get("validated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(MessageBody::ModelConversionBody(
        ModelConversionPayload::StatusResponse(ModelConversionStatusResponse {
            service_id: payload.service_id,
            status,
            onnx_path,
            max_abs_diff,
            tolerance_passed,
            error,
            validated,
        }),
    ))
}

// =============================================================================
// Background task: POST /convert, then poll GET /convert_status/{id}.
//
// Runs OUTSIDE any HandlerContext (same reason `ml_studio/export_llm.rs`
// does), so DB access goes through `crate::db::global_pool()` /
// `crate::sync::runtime::local_node_id()` rather than `ctx.state`.
// =============================================================================

/// Terminal outcome of a conversion that completed without a transport/converter
/// error. Kept distinct from `Err` because `ToleranceExceeded` already wrote
/// its own final `services.config_json` state (onnx_path, max_abs_diff, …) —
/// the generic error handler in `spawn_conversion` must not overwrite it with
/// a bare `failed` + no onnx_path.
enum ConversionOutcome {
    /// `validated` is `true` only when `test_input_path` was supplied AND the
    /// measured `max_abs_diff` passed `evaluate_tolerance` — never inferred
    /// from `status == "succeeded"` alone.
    Succeeded {
        validated: bool,
    },
    ToleranceExceeded {
        max_abs_diff: f64,
    },
}

#[allow(clippy::too_many_arguments)]
fn spawn_conversion(
    service_id: i64,
    source_path: String,
    source_format: String,
    precision: String,
    tolerance: f64,
    test_input_path: Option<String>,
    running_config_json: String,
    user_id: Option<String>,
) {
    tokio::spawn(async move {
        let node_id = crate::sync::runtime::local_node_id();
        match run_conversion(
            service_id,
            &source_path,
            &source_format,
            &precision,
            tolerance,
            test_input_path.as_deref(),
            &running_config_json,
        )
        .await
        {
            Ok(ConversionOutcome::Succeeded { validated }) => {
                if let Some(pool) = crate::db::global_pool() {
                    let _ = repository::log_audit(
                        &pool,
                        user_id.as_deref(),
                        None,
                        "model_conversion.completed",
                        Some(&audit_resource(service_id)),
                        Some(&json!({ "validated": validated }).to_string()),
                        None,
                        node_id.as_deref(),
                    );
                }
            }
            Ok(ConversionOutcome::ToleranceExceeded { max_abs_diff }) => {
                tracing::warn!(
                    service_id,
                    max_abs_diff,
                    tolerance,
                    "TF→ONNX conversion exceeded numeric tolerance"
                );
                if let Some(pool) = crate::db::global_pool() {
                    let _ = repository::log_audit(
                        &pool,
                        user_id.as_deref(),
                        None,
                        "model_conversion.failed",
                        Some(&audit_resource(service_id)),
                        Some(
                            &json!({
                                "reason": "tolerance_exceeded",
                                "max_abs_diff": max_abs_diff,
                                "tolerance": tolerance,
                            })
                            .to_string(),
                        ),
                        None,
                        node_id.as_deref(),
                    );
                }
            }
            Err(err) => {
                tracing::warn!(service_id, error = %err, "TF→ONNX conversion failed");
                let merged = merge_conversion_state(
                    &running_config_json,
                    "failed",
                    Some(&source_format),
                    Some(&precision),
                    Some(tolerance),
                    None,
                    None,
                    None,
                    Some(&err.to_string()),
                    false,
                );
                let _ = update_service_config(service_id, &merged);
                if let Some(pool) = crate::db::global_pool() {
                    let _ = repository::log_audit(
                        &pool,
                        user_id.as_deref(),
                        None,
                        "model_conversion.failed",
                        Some(&audit_resource(service_id)),
                        Some(
                            &json!({
                                "reason": "conversion_error",
                                "error": err.to_string(),
                            })
                            .to_string(),
                        ),
                        None,
                        node_id.as_deref(),
                    );
                }
            }
        }
    });
}

async fn run_conversion(
    service_id: i64,
    source_path: &str,
    source_format: &str,
    precision: &str,
    tolerance: f64,
    test_input_path: Option<&str>,
    current_config_json: &str,
) -> anyhow::Result<ConversionOutcome> {
    let endpoint = resolve_converter_endpoint()?;
    let base = endpoint.trim_end_matches('/').to_string();

    let mut convert_body = json!({
        "source_path": source_path,
        "source_format": source_format,
        "precision": precision,
    });
    if let Some(path) = test_input_path {
        convert_body["test_input_path"] = json!(path);
    }
    let conversion_id = {
        let url = format!("{}/convert", base);
        tokio::task::spawn_blocking(move || post_convert(&url, convert_body)).await??
    };
    // The id is interpolated into the poll URL below, so it is checked against
    // the shape the converter actually mints (`uuid4().hex`) rather than
    // trusted: a `?`, `#` or `../` coming back from a wedged or tampered
    // converter would otherwise rewrite the request path/query.
    if conversion_id.is_empty() || !conversion_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        anyhow::bail!("tf-onnx-converter returned a malformed conversion_id");
    }

    let deadline = tokio::time::Instant::now() + JOB_TIMEOUT;
    let status_url = format!("{}/convert_status/{}", base, conversion_id);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "TF→ONNX conversion timed out after {}s",
                JOB_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let url = status_url.clone();
        let st = tokio::task::spawn_blocking(move || get_convert_status(&url)).await??;

        match st.status.as_str() {
            "running" => continue,
            "succeeded" => {
                let onnx_path = st.onnx_path.ok_or_else(|| {
                    anyhow::anyhow!("tf-onnx-converter reported success without onnx_path")
                })?;

                // No real test input was supplied — the numeric-compatibility
                // check never ran. This still ends in `succeeded` (the ONNX
                // file exists and is deployable), but `validated` is written
                // as an explicit `false`, never left absent, so the wizard
                // cannot render an unvalidated conversion as a passed check.
                if test_input_path.is_none() {
                    let merged = merge_conversion_state(
                        current_config_json,
                        "succeeded",
                        Some(source_format),
                        Some(precision),
                        Some(tolerance),
                        Some(&onnx_path),
                        None,
                        None,
                        None,
                        false,
                    );
                    update_service_config(service_id, &merged)?;
                    return Ok(ConversionOutcome::Succeeded { validated: false });
                }

                // A real test input WAS supplied, so the converter must have
                // measured a `max_abs_diff` — its absence here is a converter
                // bug, not "not validated", and must surface as an error
                // rather than silently degrade to `validated=false`.
                let max_abs_diff = st.max_abs_diff.ok_or_else(|| {
                    anyhow::anyhow!(
                        "tf-onnx-converter reported success with a test_input_path but no max_abs_diff"
                    )
                })?;
                let tolerance_passed = evaluate_tolerance(max_abs_diff, tolerance);
                if !tolerance_passed {
                    let merged = merge_conversion_state(
                        current_config_json,
                        "failed",
                        Some(source_format),
                        Some(precision),
                        Some(tolerance),
                        Some(&onnx_path),
                        Some(max_abs_diff),
                        Some(false),
                        Some(&format!(
                            "tolerance exceeded: max_abs_diff={} > tolerance={}",
                            max_abs_diff, tolerance
                        )),
                        false,
                    );
                    update_service_config(service_id, &merged)?;
                    return Ok(ConversionOutcome::ToleranceExceeded { max_abs_diff });
                }
                let merged = merge_conversion_state(
                    current_config_json,
                    "succeeded",
                    Some(source_format),
                    Some(precision),
                    Some(tolerance),
                    Some(&onnx_path),
                    Some(max_abs_diff),
                    Some(true),
                    None,
                    true,
                );
                update_service_config(service_id, &merged)?;
                return Ok(ConversionOutcome::Succeeded { validated: true });
            }
            "failed" => {
                let msg = st.error.unwrap_or_else(|| {
                    "tf-onnx-converter reported failure without detail".to_string()
                });
                anyhow::bail!("TF→ONNX conversion failed: {}", msg);
            }
            other => anyhow::bail!("tf-onnx-converter returned unknown status '{}'", other),
        }
    }
}

fn resolve_converter_endpoint() -> anyhow::Result<String> {
    let pool = crate::db::global_pool()
        .ok_or_else(|| anyhow::anyhow!("core service registry unavailable"))?;
    let conn = pool.read().map_err(|_| anyhow::anyhow!("core db read"))?;
    let svcs = services_repo::services::list_by_category(
        &conn,
        CONVERTER_CATEGORY,
        Some(CONVERTER_ENGINE_ID),
    )?;
    let svc = svcs.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!("tf-onnx-converter service is not running — deploy it in Services first")
    })?;
    svc.endpoint_url
        .ok_or_else(|| anyhow::anyhow!("tf-onnx-converter service has no endpoint URL"))
}

fn update_service_config(service_id: i64, config_json: &str) -> anyhow::Result<()> {
    let pool = crate::db::global_pool()
        .ok_or_else(|| anyhow::anyhow!("core service registry unavailable"))?;
    let conn = pool.write().map_err(|_| anyhow::anyhow!("core db write"))?;
    services_repo::services::update_config_json(&conn, service_id, config_json)?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct ConvertResponse {
    conversion_id: String,
}

#[derive(serde::Deserialize)]
struct ConvertStatusResponse {
    status: String,
    #[serde(default)]
    onnx_path: Option<String>,
    #[serde(default)]
    max_abs_diff: Option<f64>,
    #[serde(default)]
    error: Option<String>,
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

/// Synchronous (blocking ureq) POST /convert. Called in `spawn_blocking`.
fn post_convert(url: &str, body: Value) -> anyhow::Result<String> {
    let http = http_agent();
    let mut resp = http
        .post(url)
        .send_json(&body)
        .map_err(|e| anyhow::anyhow!("POST {} failed: {}", url, e))?;
    let parsed: ConvertResponse = resp
        .body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("decode /convert response: {}", e))?;
    Ok(parsed.conversion_id)
}

/// Synchronous (blocking ureq) GET /convert_status. Called in `spawn_blocking`.
fn get_convert_status(url: &str) -> anyhow::Result<ConvertStatusResponse> {
    let http = http_agent();
    let mut resp = http
        .get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("GET {} failed: {}", url, e))?;
    resp.body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("decode /convert_status response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::transport::Transport;
    use crate::services_repo::services::{DeployMethod, NewService, ServiceStatus};

    fn ctx_admin() -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: crate::dispatch::state::AppState::for_test(),
            org_context: None,
        }
    }

    fn insert_service(ctx: &HandlerContext, new: &NewService) -> i64 {
        let conn = ctx.state.db.write().expect("db write");
        services_repo::services::insert(&conn, new).expect("insert service")
    }

    fn target_service() -> NewService {
        let mut n = NewService::minimal(
            "vision-onnx-classifier",
            DeployMethod::NativePythonBundle,
            Transport::HttpDirect,
        );
        n.category = "vision".to_string();
        n
    }

    fn converter_service() -> NewService {
        let mut n = NewService::minimal(
            CONVERTER_ENGINE_ID,
            DeployMethod::NativePythonBundle,
            Transport::HttpDirect,
        );
        n.category = CONVERTER_CATEGORY.to_string();
        n.status = ServiceStatus::Running;
        n.endpoint_url = Some("http://127.0.0.1:8300".to_string());
        n
    }

    fn start_body(service_id: i64) -> MessageBody {
        MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(
            ModelConversionStartRequest {
                service_id,
                source_path: "/data/models/adr-classifier".to_string(),
                source_format: "tensorflow_savedmodel".to_string(),
                precision: "fp32".to_string(),
                tolerance: 0.001,
                test_input_path: None,
            },
        ))
    }

    fn start_body_with_test_input(service_id: i64, tolerance: f64) -> MessageBody {
        MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(
            ModelConversionStartRequest {
                service_id,
                source_path: "/data/models/adr-classifier".to_string(),
                source_format: "tensorflow_savedmodel".to_string(),
                precision: "fp32".to_string(),
                tolerance,
                test_input_path: Some("/data/models/adr-classifier/test_input.npy".to_string()),
            },
        ))
    }

    // -------------------------------------------------------------------
    // evaluate_tolerance — the comparison decision itself.
    // -------------------------------------------------------------------

    #[test]
    fn tolerance_boundary_is_inclusive() {
        assert!(evaluate_tolerance(0.001, 0.001));
    }

    #[test]
    fn tolerance_just_over_fails() {
        assert!(!evaluate_tolerance(0.0010001, 0.001));
    }

    #[test]
    fn tolerance_well_under_passes() {
        assert!(evaluate_tolerance(0.0, 0.001));
    }

    #[test]
    fn tolerance_nan_never_passes() {
        assert!(!evaluate_tolerance(f64::NAN, 0.001));
    }

    #[test]
    fn tolerance_negative_diff_never_passes() {
        // A converter bug producing a negative diff must not silently pass —
        // there is no such thing as a "negative" divergence.
        assert!(!evaluate_tolerance(-0.0001, 0.001));
    }

    // -------------------------------------------------------------------
    // validate_start_request
    // -------------------------------------------------------------------

    #[test]
    fn rejects_unknown_source_format() {
        let mut req = match start_body(1) {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(r)) => r,
            _ => unreachable!(),
        };
        req.source_format = "pytorch".to_string();
        let err = validate_start_request(&req).expect_err("unknown format");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn rejects_unknown_precision() {
        let mut req = match start_body(1) {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(r)) => r,
            _ => unreachable!(),
        };
        req.precision = "int8".to_string();
        let err = validate_start_request(&req).expect_err("unknown precision");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn rejects_negative_tolerance() {
        let mut req = match start_body(1) {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(r)) => r,
            _ => unreachable!(),
        };
        req.tolerance = -1.0;
        let err = validate_start_request(&req).expect_err("negative tolerance");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn rejects_empty_source_path() {
        let mut req = match start_body(1) {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(r)) => r,
            _ => unreachable!(),
        };
        req.source_path = "   ".to_string();
        let err = validate_start_request(&req).expect_err("empty path");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn rejects_blank_test_input_path() {
        let mut req = match start_body(1) {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(r)) => r,
            _ => unreachable!(),
        };
        req.test_input_path = Some("   ".to_string());
        let err = validate_start_request(&req).expect_err("blank test_input_path");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn accepts_a_missing_test_input_path() {
        let req = match start_body(1) {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(r)) => r,
            _ => unreachable!(),
        };
        assert!(req.test_input_path.is_none());
        validate_start_request(&req).expect("None test_input_path is valid — validation is opt-in");
    }

    // -------------------------------------------------------------------
    // Handler-level behaviour, routed through `crate::dispatch::dispatch`
    // (proves the inventory registration resolves, not just the fn body).
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn a_non_admin_session_is_denied() {
        let mut ctx = ctx_admin();
        ctx.session = SessionAuth::UserSession {
            user_id: *uuid::Uuid::new_v4().as_bytes(),
            role: Some("power_user".to_string()),
        };
        let id = insert_service(&ctx, &target_service());
        let (body, is_err) = crate::dispatch::dispatch(&start_body(id), &ctx).await;
        assert!(is_err, "power_user must not pass an Admin gate");
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::PolicyDenied),
            other => panic!("expected PolicyDenied error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn starting_conversion_for_an_unknown_service_is_not_found() {
        let ctx = ctx_admin();
        let (body, is_err) = crate::dispatch::dispatch(&start_body(999_999), &ctx).await;
        assert!(is_err);
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::NotFound),
            other => panic!("expected NotFound error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn starting_conversion_without_a_running_converter_is_refused() {
        // `ctx.state.db` is usually a private per-test pool, but the FIRST
        // `AppState::for_test()` in the binary also becomes `db::global_pool()`
        // (a `OnceLock`) — so if this test happens to win that race, its pool
        // is the very one the mock-converter tests insert `running` converter
        // rows into, and the preflight below would find one. Take the same
        // lock they do and clear any converter row already in this pool, so
        // "no converter" is a fact about the pool, not about test ordering.
        let _guard = CONVERTER_TEST_LOCK.lock().await;
        let ctx = ctx_admin();
        stop_converter_rows(&ctx.state.db);
        let id = insert_service(&ctx, &target_service());
        let (body, is_err) = crate::dispatch::dispatch(&start_body(id), &ctx).await;
        assert!(is_err, "no tf-onnx-converter row must refuse the start");
        match body {
            MessageBody::Error(e) => {
                assert_eq!(e.code, ProtocolErrorCode::BadRequest);
                assert!(e.message.contains("tf-onnx-converter"));
            }
            other => panic!("expected BadRequest error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_valid_start_claims_the_row_as_converting() {
        // Inserts a `running` converter row, which lands in `db::global_pool()`
        // when this test owns it — the same registry the mock-converter tests
        // resolve through. Serialised with them so neither side sees the
        // other's converter.
        let _guard = CONVERTER_TEST_LOCK.lock().await;
        let ctx = ctx_admin();
        let id = insert_service(&ctx, &target_service());
        insert_service(&ctx, &converter_service());

        let (body, is_err) = crate::dispatch::dispatch(&start_body(id), &ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::ModelConversionBody(ModelConversionPayload::StartResponse(r)) => {
                assert_eq!(r.service_id, id);
                assert_eq!(r.status, "converting");
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let conn = ctx.state.db.read().expect("db read");
        let row = services_repo::services::get(&conn, id)
            .expect("get")
            .expect("row exists");
        let cfg: Value = serde_json::from_str(&row.config_json).expect("valid json");
        assert_eq!(cfg["conversion_status"], "converting");
        assert_eq!(cfg["source_format"], "tensorflow_savedmodel");
        assert_eq!(cfg["conversion_precision"], "fp32");
    }

    #[tokio::test]
    async fn status_reflects_whatever_is_stored_in_config_json() {
        let ctx = ctx_admin();
        let id = insert_service(&ctx, &target_service());
        {
            let conn = ctx.state.db.write().expect("db write");
            let stored = json!({
                "conversion_status": "failed",
                "onnx_path": "/data/models/adr-classifier/model.onnx",
                "max_abs_diff": 0.05,
                "tolerance_passed": false,
                "conversion_error": "tolerance exceeded: max_abs_diff=0.05 > tolerance=0.001",
                "validated": false,
            })
            .to_string();
            services_repo::services::update_config_json(&conn, id, &stored).expect("update");
        }

        let status_body = MessageBody::ModelConversionBody(ModelConversionPayload::StatusRequest(
            ModelConversionStatusRequest { service_id: id },
        ));
        let (body, is_err) = crate::dispatch::dispatch(&status_body, &ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::ModelConversionBody(ModelConversionPayload::StatusResponse(r)) => {
                assert_eq!(r.service_id, id);
                assert_eq!(r.status, "failed");
                assert_eq!(
                    r.onnx_path.as_deref(),
                    Some("/data/models/adr-classifier/model.onnx")
                );
                assert_eq!(r.max_abs_diff, Some(0.05));
                assert_eq!(r.tolerance_passed, Some(false));
                assert!(r.error.is_some());
                assert!(
                    !r.validated,
                    "tolerance-exceeded conversion is never validated"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_for_an_unstarted_service_is_none() {
        let ctx = ctx_admin();
        let id = insert_service(&ctx, &target_service());
        let status_body = MessageBody::ModelConversionBody(ModelConversionPayload::StatusRequest(
            ModelConversionStatusRequest { service_id: id },
        ));
        let (body, is_err) = crate::dispatch::dispatch(&status_body, &ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::ModelConversionBody(ModelConversionPayload::StatusResponse(r)) => {
                assert_eq!(r.status, "none");
                assert!(r.onnx_path.is_none());
                assert!(
                    !r.validated,
                    "a never-started conversion is never validated"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_valid_start_with_test_input_claims_the_row_unvalidated_until_the_job_finishes() {
        // Same reason as `a_valid_start_claims_the_row_as_converting`.
        let _guard = CONVERTER_TEST_LOCK.lock().await;
        let ctx = ctx_admin();
        let id = insert_service(&ctx, &target_service());
        insert_service(&ctx, &converter_service());

        let (body, is_err) =
            crate::dispatch::dispatch(&start_body_with_test_input(id, 0.01), &ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");

        let conn = ctx.state.db.read().expect("db read");
        let row = services_repo::services::get(&conn, id)
            .expect("get")
            .expect("row exists");
        let cfg: Value = serde_json::from_str(&row.config_json).expect("valid json");
        assert_eq!(cfg["conversion_status"], "converting");
        // Not yet validated — the background job has not run against the
        // mock converter yet, so this must never read as an early pass.
        assert_eq!(cfg["validated"], false);
    }

    // -------------------------------------------------------------------
    // `run_conversion` against a mock `tf-onnx-converter` HTTP service —
    // exercises the actual POST /convert + poll GET /convert_status round
    // trip and the real `evaluate_tolerance` decision, covering the three
    // outcomes ZADANIA.md Z11 requires: validated pass, tolerance-exceeded
    // failure, and unvalidated success when no test input was supplied.
    // -------------------------------------------------------------------

    /// The `services` table `run_conversion` writes into lives behind
    /// `crate::db::global_pool()` (a process-wide `OnceLock`, set once by the
    /// FIRST `AppState::for_test()`/`crate::db::init()` call in this test
    /// binary), not `ctx.state.db` — mirroring production, where the
    /// background task has no `HandlerContext`. Tests that exercise
    /// `run_conversion` must insert their fixture row into THAT pool (not a
    /// fresh, unrelated one) so the write-back lands where the test can read
    /// it back, and so it succeeds at all (`update_config_json` errors on an
    /// unknown id).
    fn global_pool_for_test() -> crate::db::DbPool {
        if let Some(pool) = crate::db::global_pool() {
            return pool;
        }
        let _ = ctx_admin();
        crate::db::global_pool().expect("global pool set by AppState::for_test()")
    }

    fn insert_service_into_global_pool(new: &NewService) -> i64 {
        let pool = global_pool_for_test();
        let conn = pool.write().expect("db write");
        services_repo::services::insert(&conn, new).expect("insert service")
    }

    fn read_config_json_from_global_pool(service_id: i64) -> Value {
        let pool = global_pool_for_test();
        let conn = pool.read().expect("db read");
        let row = services_repo::services::get(&conn, service_id)
            .expect("get")
            .expect("row exists");
        serde_json::from_str(&row.config_json).expect("valid json")
    }

    /// Starts a mock converter on loopback that answers `POST /convert` with
    /// a fixed `conversion_id` and every `GET /convert_status/<id>` with the
    /// given fixed JSON body — enough for `run_conversion`'s single-poll
    /// happy path (the fixture responds "succeeded" on the very first poll).
    async fn spawn_mock_converter(status_body: Value) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock converter");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let mut buf = [0u8; 4096];
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let request = String::from_utf8_lossy(&buf[..n]);
                let body = if request.starts_with("POST /convert ") {
                    json!({ "conversion_id": "mockconversionid00" }).to_string()
                } else {
                    status_body.to_string()
                };
                // `Connection: close` forces ureq to open a fresh TCP
                // connection per request instead of pooling one — this mock
                // only reads/answers ONE request per accepted connection, so
                // a reused (kept-alive) connection would leave the second
                // request (`GET /convert_status`) unread and hang until the
                // client's HTTP timeout.
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        format!("http://{}", addr)
    }

    /// Serializes tests that resolve `tf-onnx-converter` through
    /// `crate::db::global_pool()`: `resolve_converter_endpoint`
    /// (`list_by_category`) picks the FIRST `running` row of that
    /// category/engine in whichever pool happens to be global, with no
    /// per-test scoping — two such tests running concurrently (the default
    /// under `cargo test`) could otherwise resolve to EACH OTHER's mock
    /// server. Held for the whole insert-converter → `run_conversion` →
    /// assert sequence of each test below, not just setup.
    static CONVERTER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// `run_conversion` needs a running `tf-onnx-converter` service row to
    /// resolve the endpoint through (`resolve_converter_endpoint`), pointed
    /// at the mock server instead of a real container. Stops any `running`
    /// row left behind by an earlier test in this file first — the resolver
    /// filters purely on category/engine/status, not on which test created
    /// the row.
    /// Marks every `tf-onnx-converter` row in `pool` as stopped, so
    /// `list_by_category(.., status = 'running')` no longer resolves it.
    fn stop_converter_rows(pool: &crate::db::DbPool) {
        let conn = pool.write().expect("db write");
        for stale in services_repo::services::list_by_category(
            &conn,
            CONVERTER_CATEGORY,
            Some(CONVERTER_ENGINE_ID),
        )
        .expect("list converters")
        {
            services_repo::services::update_status(&conn, stale.id, ServiceStatus::Stopped)
                .expect("stop stale converter row");
        }
    }

    fn insert_converter_pointing_at(endpoint: &str) {
        stop_converter_rows(&global_pool_for_test());
        let mut n = converter_service();
        n.endpoint_url = Some(endpoint.to_string());
        insert_service_into_global_pool(&n);
    }

    #[tokio::test]
    async fn start_with_test_input_and_diff_within_tolerance_is_validated() {
        let _guard = CONVERTER_TEST_LOCK.lock().await;
        let id = insert_service_into_global_pool(&target_service());
        let endpoint = spawn_mock_converter(json!({
            "status": "succeeded",
            "onnx_path": "/data/models/adr-classifier/model.onnx",
            "max_abs_diff": 0.0005,
        }))
        .await;
        insert_converter_pointing_at(&endpoint);

        let outcome = run_conversion(
            id,
            "/data/models/adr-classifier",
            "tensorflow_savedmodel",
            "fp32",
            0.001,
            Some("/data/models/adr-classifier/test_input.npy"),
            "{}",
        )
        .await
        .expect("conversion succeeds");
        match outcome {
            ConversionOutcome::Succeeded { validated } => assert!(validated),
            ConversionOutcome::ToleranceExceeded { max_abs_diff } => {
                panic!("expected validated success, got tolerance exceeded: {max_abs_diff}")
            }
        }

        let cfg = read_config_json_from_global_pool(id);
        assert_eq!(cfg["conversion_status"], "succeeded");
        assert_eq!(cfg["validated"], true);
        assert_eq!(cfg["max_abs_diff"], 0.0005);
        assert_eq!(cfg["tolerance_passed"], true);
    }

    #[tokio::test]
    async fn start_with_test_input_and_diff_over_tolerance_fails_with_a_reason() {
        let _guard = CONVERTER_TEST_LOCK.lock().await;
        let id = insert_service_into_global_pool(&target_service());
        let endpoint = spawn_mock_converter(json!({
            "status": "succeeded",
            "onnx_path": "/data/models/adr-classifier/model.onnx",
            "max_abs_diff": 0.5,
        }))
        .await;
        insert_converter_pointing_at(&endpoint);

        let outcome = run_conversion(
            id,
            "/data/models/adr-classifier",
            "tensorflow_savedmodel",
            "fp32",
            0.001,
            Some("/data/models/adr-classifier/test_input.npy"),
            "{}",
        )
        .await
        .expect("conversion completes (tolerance exceeded is Ok, not Err)");
        match outcome {
            ConversionOutcome::ToleranceExceeded { max_abs_diff } => {
                assert_eq!(max_abs_diff, 0.5)
            }
            ConversionOutcome::Succeeded { .. } => panic!("expected tolerance exceeded"),
        }

        let cfg = read_config_json_from_global_pool(id);
        assert_eq!(cfg["conversion_status"], "failed");
        assert_eq!(cfg["validated"], false);
        assert_eq!(cfg["max_abs_diff"], 0.5);
        let reason = cfg["conversion_error"]
            .as_str()
            .expect("failure carries a reason");
        assert!(
            reason.contains("tolerance"),
            "reason must explain WHY it failed, got: {reason}"
        );
    }

    #[tokio::test]
    async fn start_without_test_input_succeeds_unvalidated() {
        let _guard = CONVERTER_TEST_LOCK.lock().await;
        let id = insert_service_into_global_pool(&target_service());
        let endpoint = spawn_mock_converter(json!({
            "status": "succeeded",
            "onnx_path": "/data/models/adr-classifier/model.onnx",
            "max_abs_diff": null,
        }))
        .await;
        insert_converter_pointing_at(&endpoint);

        let outcome = run_conversion(
            id,
            "/data/models/adr-classifier",
            "tensorflow_savedmodel",
            "fp32",
            0.001,
            None,
            "{}",
        )
        .await
        .expect("conversion succeeds");
        match outcome {
            ConversionOutcome::Succeeded { validated } => {
                assert!(!validated, "no test_input_path means never validated")
            }
            ConversionOutcome::ToleranceExceeded { max_abs_diff } => {
                panic!("expected unvalidated success, got tolerance exceeded: {max_abs_diff}")
            }
        }

        let cfg = read_config_json_from_global_pool(id);
        assert_eq!(cfg["conversion_status"], "succeeded");
        assert_eq!(cfg["validated"], false);
        assert!(cfg["max_abs_diff"].is_null());
    }
}
