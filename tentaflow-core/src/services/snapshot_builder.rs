// ============ File: snapshot_builder.rs — Build ServiceInfo snapshots from local SQLite for mesh sync ============

// The mesh services registry needs to advertise the local node's services to
// other peers, both as a periodic full-state announce and as incremental push
// updates after deploy/stop/pin/pause/rename/delete. This module owns the
// `ServiceRow` (+ `ModelRow`) → `ServiceInfo` projection so the dispatcher
// handler and the heartbeat sender share one implementation. The same
// projection lives in `dispatch::handlers::build_service_info` for the local
// `ServiceListRequest`; we reuse the helpers below from the handler too.

use anyhow::Result;
use rusqlite::Connection;
use tentaflow_protocol::{KeyValue, RequestTimeParameters, ServiceInfo, ServiceModelEntry};

use crate::services_repo;

/// Wyciagniecie typed `request_time_parameters` z `services.config_json`.
/// Format: `{ "request_time_parameters": { "ollama_options": { ... },
/// "python_request": { ... }, "whisper_overridable": { ... },
/// "mlx_overridable": { ... } } }`. Brak pola → puste mapy. Pojedyncze
/// brakujace pod-mapy → puste vec'y.
fn parse_request_time_parameters(config_json: &str) -> RequestTimeParameters {
    let value: serde_json::Value =
        serde_json::from_str(config_json).unwrap_or(serde_json::Value::Null);
    let Some(rtp) = value.get("request_time_parameters") else {
        return RequestTimeParameters::default();
    };
    let extract = |key: &str| -> Vec<KeyValue> {
        rtp.get(key)
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(k, v)| KeyValue {
                        key: k.clone(),
                        value_json: v.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    RequestTimeParameters {
        ollama_options: extract("ollama_options"),
        python_request: extract("python_request"),
        whisper_overridable: extract("whisper_overridable"),
        mlx_overridable: extract("mlx_overridable"),
    }
}

/// Convert a JSON-encoded capabilities column into a `Vec<String>`. Returns
/// an empty vec when the column is null / not a JSON array — same behaviour
/// as the dispatcher copy that this duplicates intentionally to avoid a
/// cross-module dependency on `dispatch`.
fn parse_capabilities_array(capabilities_json: &str) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_str(capabilities_json).unwrap_or(serde_json::Value::Null);
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Project a `ServiceRow` (already loaded from `services`) into the wire
/// `ServiceInfo`, joining its `model_registry` rows. `local_node_id` is
/// stamped onto every entry so receivers know which mesh node owns the row.
pub fn project_service_row(
    conn: &Connection,
    svc: services_repo::services::ServiceRow,
    local_node_id: &str,
) -> Result<ServiceInfo> {
    let model_rows = services_repo::models::list_for_service(conn, svc.id)?;
    // Manifest TEGO silnika (anonsujący node go ma) — liczymy z niego
    // service_surfaces per model i wysyłamy WPROST, żeby peer bez manifestu
    // (albo z `category` nie-mapowalną jak `vision`) mógł resolwować zdalny model.
    let engine_manifest = crate::services::manifest::registry().by_id(&svc.engine_id);
    let models: Vec<ServiceModelEntry> = model_rows
        .into_iter()
        .map(|m| {
            let service_surfaces = engine_manifest
                .map(|em| {
                    let preset = em
                        .model_presets
                        .iter()
                        .find(|p| p.id == m.model_name || p.repo == m.model_name);
                    em.engine.effective_service_surfaces(preset)
                })
                .unwrap_or_default();
            ServiceModelEntry {
                model_name: m.model_name,
                display_name: m.display_name,
                capabilities: parse_capabilities_array(&m.capabilities),
                context_length: m.context_length.and_then(|v| u32::try_from(v).ok()),
                quantization: m.quantization,
                is_default: m.is_default,
                service_surfaces,
            }
        })
        .collect();

    let request_time_parameters = parse_request_time_parameters(&svc.config_json);
    let gpu_selection = gpu_selection_summary(&svc.config_json);

    // Wykrycie aktualizacji: hash zrodel z deployu vs aktualny hash z manifestu
    // (build.rs liczy go w czasie kompilacji). Roznica = wbudowany bundle
    // zostal podbity od czasu deployu tego serwisu.
    let update_available = {
        use services_repo::services::DeployMethod;
        let deployed = svc.deployed_source_hash.as_str();
        let current = crate::services::manifest::registry()
            .by_id(&svc.engine_id)
            .map(|m| match svc.deploy_method {
                DeployMethod::Docker => m.docker_source_hash.as_str(),
                DeployMethod::NativeBinary | DeployMethod::NativePythonBundle => {
                    m.native_source_hash.as_str()
                }
                _ => "",
            })
            .unwrap_or("");
        !deployed.is_empty() && !current.is_empty() && deployed != current
    };

    Ok(ServiceInfo {
        id: svc.id,
        node_id: local_node_id.to_string(),
        engine_id: svc.engine_id,
        category: svc.category,
        display_name: svc.display_name,
        deploy_method: svc.deploy_method.as_db_tag().to_string(),
        transport: svc.transport.as_db_tag().to_string(),
        status: svc.status.as_db_tag().to_string(),
        pinned: svc.pinned,
        paused: svc.paused,
        runtime_pid: svc.runtime_pid,
        runtime_port: svc.runtime_port,
        sidecar_quic_port: svc.sidecar_quic_port,
        endpoint_url: svc.endpoint_url,
        restart_count: u32::try_from(svc.restart_count).unwrap_or(u32::MAX),
        health_last_err: svc.health_last_err,
        active_deploy_id: svc.active_deploy_id,
        last_deploy_id: svc.last_deploy_id,
        deployment_progress_pct: i32::try_from(svc.deployment_progress_pct).unwrap_or(100),
        progress_message: svc.progress_message,
        models,
        update_available,
        created_at: svc.created_at,
        updated_at: svc.updated_at,
        request_time_parameters,
        gpu_selection,
    })
}

/// Podsumowanie kart GPU serwisu z deploy configu (`gpu_select_mode` + `gpu_ids`)
/// do kolumny "Karty" w liscie serwisow: `"all"` (wszystkie widoczne), `"0,1"`
/// (konkretne indeksy), `"CPU"` (bez GPU). Domyslnie `"all"` — tak jak
/// `apply_gpu_selection_env` traktuje brak trybu.
fn gpu_selection_summary(config_json: &str) -> String {
    let cfg: serde_json::Value =
        serde_json::from_str(config_json).unwrap_or(serde_json::Value::Null);
    let mode = cfg
        .get("gpu_select_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    match mode {
        "none" => "CPU".to_string(),
        "specific" => {
            let ids: Vec<String> = cfg
                .get("gpu_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|id| match id {
                            serde_json::Value::String(s) => {
                                let t = s.trim();
                                (!t.is_empty()).then(|| t.to_string())
                            }
                            serde_json::Value::Number(n) => Some(n.to_string()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if ids.is_empty() {
                "all".to_string()
            } else {
                ids.join(",")
            }
        }
        _ => "all".to_string(),
    }
}

/// Build the full `Vec<ServiceInfo>` snapshot of the local node's currently
/// known services. Called from the periodic anti-drift announce and from the
/// pull-on-connect responder.
pub fn build_local_snapshot(
    db: &crate::db::DbPool,
    local_node_id: &str,
) -> Result<Vec<ServiceInfo>> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let rows = services_repo::services::list_all(&conn)?;
    let mut out = Vec::with_capacity(rows.len());
    for svc in rows {
        out.push(project_service_row(&conn, svc, local_node_id)?);
    }
    Ok(out)
}

/// Build a single `ServiceInfo` for `service_id` if the row still exists. Used
/// by push-on-change handlers to construct `ServiceChange::Added` /
/// `ServiceChange::Updated` payloads after a successful local mutation.
pub fn build_one(
    db: &crate::db::DbPool,
    service_id: i64,
    local_node_id: &str,
) -> Result<Option<ServiceInfo>> {
    let conn = db.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let svc = match services_repo::services::get(&conn, service_id)? {
        Some(s) => s,
        None => return Ok(None),
    };
    Ok(Some(project_service_row(&conn, svc, local_node_id)?))
}
