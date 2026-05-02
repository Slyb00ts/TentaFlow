// =============================================================================
// File: services/models/mod.rs
// Alias domain logic invoked from binary RPC handlers in `dispatch::handlers`.
// The unified mesh-model view used to live here too — that role is now served
// by `services::catalog::CatalogProvider`, which `/v1/models`, `catalog.list`,
// and the GUI all share.
// =============================================================================

use std::sync::Arc;

use crate::db::models::DbModelAlias;
use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;
use anyhow::Result;

/// All aliases stored in the local DB.
pub fn list_aliases(pool: &DbPool) -> Result<Vec<DbModelAlias>> {
    db::repository::list_model_aliases(pool)
}

/// Creates an alias. Rejects when `alias` or `target_model` is empty after
/// trimming, or when the alias name would collide with a published flow or
/// another existing alias.
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
    crate::services::catalog::guards::check_alias_collision(pool, alias, None)?;
    db::repository::create_model_alias(pool, alias, target_model, fallback_targets, strategy)
}

/// Updates an existing alias by id. Returns `Ok(false)` when the row does not
/// exist so the caller can map it to a 404-style response. Re-validates the
/// chosen name against the catalog (published flows, other aliases) so that
/// renaming an alias cannot smuggle in a collision.
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
    crate::services::catalog::guards::check_alias_collision(pool, alias, Some(id))?;
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

/// Reload local router alias cache, rebuild the public catalog snapshot, and
/// broadcast the latest alias snapshot to the mesh after a successful
/// create/update/delete.
pub fn broadcast_alias_mutation(
    pool: &DbPool,
    router: &Arc<crate::routing::router::Router>,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
) {
    router.reload_alias_cache();
    router.rebuild_catalog();
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
