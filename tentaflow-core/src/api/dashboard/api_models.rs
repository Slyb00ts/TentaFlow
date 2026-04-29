// ============ File: api/dashboard/api_models.rs — alias + unified-models domain helpers shared with binary RPC handlers ============
//
// Krok N2 removed every REST handler that lived in this file. The chat picker
// and Services tab now talk to `ModelListRequest` / `ServiceListRequest` over
// binary WS. What remains is the alias domain logic + unified-models view used
// by the binary handlers in `dispatch::handlers` (FAZA 2 surface).

use std::sync::Arc;

use crate::db::models::DbModelAlias;
use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::service_registry::UnifiedModelInfo;
use anyhow::Result;

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

/// Distinct models reported by mesh peers via service registry. Empty when
/// the mesh manager is not initialized (no QUIC mesh on this node).
pub fn collect_unified(quic_mesh: &Option<Arc<IrohMeshManager>>) -> Vec<UnifiedModelInfo> {
    match quic_mesh {
        Some(qm) => qm.service_registry().unique_models(),
        None => Vec::new(),
    }
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
