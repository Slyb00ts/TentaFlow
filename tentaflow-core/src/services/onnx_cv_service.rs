// =============================================================================
// File: services/onnx_cv_service.rs — materializes the `onnx-cv` service row
// =============================================================================
//
// The generic dynamic-model engine (`onnx-cv`, `vision::onnx_cv`) is not
// deployed by an admin: this reconciler keeps a single `services` row (plus
// its `model_registry` rows) in sync with the `vision_models` registry, so
// every registered model whose ONNX file is PRESENT on this node surfaces in
// the catalog as a `camera_cv` model (per-model surfaces come from the
// `onnx-cv` engine manifest). Rows replicated from other nodes without the
// local file are skipped — those models resolve through the mesh forward to
// the owning node instead. Runs at supervisor boot and after every registry
// mutation (publish / delete / sync apply is picked up by the next tick).

use anyhow::Result;
use tracing::info;

use crate::db::DbPool;
use crate::services::transport::Transport;
use crate::services_repo::services::{DeployMethod, NewService, ServiceStatus};
use crate::services_repo::{models, services};

const ENGINE_ID: &str = "onnx-cv";
const DISPLAY_NAME: &str = "ONNX CV (modele dynamiczne)";

/// Brings the local `onnx-cv` service row in line with `vision_models`.
/// Returns `true` when anything changed (caller should refresh the mesh
/// snapshot / catalog). Without the `inference-supertonic` feature the node
/// cannot run ort sessions, so the service is kept absent regardless of
/// registry contents.
pub fn reconcile(db: &DbPool) -> Result<bool> {
    let desired: Vec<String> = if cfg!(feature = "inference-supertonic") {
        let rows = crate::db::repository::list_vision_models_all(db)?;
        let dir = crate::paths::vision_models_dir();
        rows.into_iter()
            .filter(|r| dir.join(&r.file_name).is_file())
            .map(|r| r.model_name)
            .collect()
    } else {
        Vec::new()
    };

    let conn = db
        .write()
        .map_err(|e| anyhow::anyhow!("onnx-cv reconcile: db write: {e}"))?;
    // One transaction for the whole service/models sync: a concurrent
    // supervisor health tick never observes the row without its models
    // (that transient state would be flagged "service has no registered
    // models" and marked Failed).
    let tx = conn.unchecked_transaction()?;
    let existing = services::list_by_category(&tx, "vision", Some(ENGINE_ID))?
        .into_iter()
        .next();

    if desired.is_empty() {
        if let Some(svc) = existing {
            models::delete_for_service(&tx, svc.id)?;
            services::delete(&tx, svc.id)?;
            tx.commit()?;
            info!("[onnx-cv] no servable registry models — service row removed");
            return Ok(true);
        }
        return Ok(false);
    }

    let (service_id, mut changed) = match existing {
        Some(svc) => (svc.id, false),
        None => {
            let new = NewService {
                category: "vision".to_string(),
                display_name: DISPLAY_NAME.to_string(),
                status: ServiceStatus::Running,
                deployment_progress_pct: 100,
                ..NewService::minimal(ENGINE_ID, DeployMethod::NativeEmbedded, Transport::Embedded)
            };
            let id = services::insert(&tx, &new)?;
            info!("[onnx-cv] service row materialized (id {id})");
            (id, true)
        }
    };

    // Sync model_registry rows to exactly the servable registry names.
    let current = models::list_for_service(&tx, service_id)?;
    for row in &current {
        if !desired.contains(&row.model_name) {
            tx.execute(
                "DELETE FROM model_registry WHERE id = ?1",
                rusqlite::params![row.id],
            )?;
            changed = true;
        }
    }
    for name in &desired {
        if !current.iter().any(|row| row.model_name == *name) {
            models::insert(
                &tx,
                &models::NewModel {
                    service_id,
                    model_name: name.clone(),
                    display_name: None,
                    capabilities: "[]".to_string(),
                    context_length: None,
                    quantization: None,
                    is_default: false,
                },
            )?;
            changed = true;
        }
    }
    tx.commit()?;
    if changed {
        info!(
            "[onnx-cv] model_registry synced to {} registry model(s)",
            desired.len()
        );
    }
    Ok(changed)
}

/// Reconcile + immediately republish the local mesh snapshot and rebuild the
/// catalog, so a freshly published model resolves without waiting for the
/// next supervisor tick. Used by the ML Studio publish/delete handlers.
pub fn reconcile_and_announce(state: &crate::dispatch::state::AppState) {
    match reconcile(&state.db) {
        Ok(true) => {
            match crate::services::snapshot_builder::build_local_snapshot(
                &state.db,
                &state.local_node_id,
            ) {
                Ok(snapshot) => {
                    state
                        .mesh_services_registry
                        .replace_local(state.local_node_id.to_string(), snapshot);
                }
                Err(e) => {
                    tracing::warn!("onnx-cv reconcile: snapshot rebuild failed: {e:#}");
                }
            }
            state.router.rebuild_catalog();
        }
        Ok(false) => {}
        Err(e) => tracing::warn!("onnx-cv reconcile failed: {e:#}"),
    }
}
