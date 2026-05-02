// =============================================================================
// File: services/models/mod.rs
// Opis: Alias domain logic + unified-models view dla binary RPC handlerów.
// =============================================================================

use std::sync::Arc;

use crate::db::models::DbModelAlias;
use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::services::mesh_registry::MeshServicesRegistry;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Aggregated unified-model view: single `model_name` × `service_type` row with
/// every node instance hosting it. Built from `MeshServicesRegistry` snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedModelInfo {
    pub model_name: String,
    pub service_type: String,
    pub instances: Vec<ModelInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInstance {
    pub node_id: String,
    pub node_name: String,
    pub service_id: String,
    pub status: String,
    pub backend: Option<String>,
    pub size_mb: Option<u64>,
}

// =============================================================================
// Alias domain logic (called from binary handlers in `dispatch::handlers`).
// =============================================================================

/// All aliases stored in the local DB.
pub fn list_aliases(pool: &DbPool) -> Result<Vec<DbModelAlias>> {
    db::repository::list_model_aliases(pool)
}

/// Creates an alias. Validates that both `alias` and `target_model` are
/// non-empty after trimming.
pub fn create_alias(
    pool: &DbPool,
    alias: &str,
    target_model: &str,
    strategy: Option<&str>,
    fallback_targets: Option<&str>,
) -> Result<i64> {
    if alias.trim().is_empty() || target_model.trim().is_empty() {
        anyhow::bail!("alias and target_model must be non-empty");
    }
    db::repository::create_model_alias(pool, alias, target_model, fallback_targets, strategy)
}

/// Updates an existing alias by id. Returns `Ok(false)` when the row does not
/// exist so the caller can map it to a 404-style response.
pub fn update_alias(
    pool: &DbPool,
    id: i64,
    alias: &str,
    target_model: &str,
    is_active: bool,
    strategy: Option<&str>,
    fallback_targets: Option<&str>,
) -> Result<bool> {
    if db::repository::get_model_alias(pool, id)?.is_none() {
        return Ok(false);
    }
    if alias.trim().is_empty() || target_model.trim().is_empty() {
        anyhow::bail!("alias and target_model must be non-empty");
    }
    db::repository::update_model_alias(
        pool,
        id,
        alias,
        target_model,
        is_active,
        fallback_targets,
        strategy,
    )?;
    Ok(true)
}

/// Deletes an alias by id. Returns `Ok(false)` when the row does not exist.
pub fn delete_alias(pool: &DbPool, id: i64) -> Result<bool> {
    if db::repository::get_model_alias(pool, id)?.is_none() {
        return Ok(false);
    }
    db::repository::delete_model_alias(pool, id)?;
    Ok(true)
}

/// Distinct models advertised across the mesh, grouped by `(model_name,
/// category)` with one `ModelInstance` per advertising node. Sourced from the
/// V2 `MeshServicesRegistry` aggregator (local + remote snapshots).
pub fn collect_unified(registry: &MeshServicesRegistry) -> Vec<UnifiedModelInfo> {
    use std::collections::HashMap;

    let mut by_key: HashMap<(String, String), Vec<ModelInstance>> = HashMap::new();
    for svc in registry.visible_services() {
        let loaded_status = svc.status.clone();
        for model in &svc.models {
            let key = (model.model_name.clone(), svc.category.clone());
            by_key.entry(key).or_default().push(ModelInstance {
                node_id: svc.node_id.clone(),
                node_name: svc.display_name.clone(),
                service_id: svc.id.to_string(),
                status: loaded_status.clone(),
                backend: Some(svc.engine_id.clone()),
                size_mb: None,
            });
        }
    }

    by_key
        .into_iter()
        .map(|((model_name, service_type), instances)| UnifiedModelInfo {
            model_name,
            service_type,
            instances,
        })
        .collect()
}

/// Reload local router alias cache and broadcast the latest alias snapshot to
/// the mesh after a successful create/update/delete.
pub fn broadcast_alias_mutation(
    pool: &DbPool,
    router: &Arc<crate::routing::router::Router>,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
) {
    router.reload_alias_cache();
    if let Some(qm) = quic_mesh {
        if let Ok(aliases) = db::repository::list_model_aliases(pool) {
            if let Ok(json) = serde_json::to_vec(&aliases) {
                let qm = Arc::clone(qm);
                tokio::spawn(async move {
                    qm.broadcast_alias_sync(json).await;
                });
            }
        }
    }
}
