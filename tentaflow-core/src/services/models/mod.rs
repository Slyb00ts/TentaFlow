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
/// trimming, when the alias name would collide with a published flow or
/// another existing alias, or when `target_model` / any fallback is itself
/// an active alias (no alias-of-alias chains).
///
/// Chain check + insert run under a single SQLite transaction inside
/// `create_model_alias_with_chain_check`, so two concurrent admin writes
/// cannot pass the validation independently and then jointly create a chain.
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
    db::repository::create_model_alias_with_chain_check(
        pool,
        alias,
        target_model,
        fallback_targets,
        strategy,
    )
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
    db::repository::update_model_alias_with_chain_check(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fresh_db() -> DbPool {
        crate::db::init(Path::new(":memory:")).expect("test DB init")
    }

    /// R4.C: Plan v7 D.17 only models a single resolution layer; an
    /// alias whose `target_model` is itself an existing alias must be
    /// rejected at write time so the catalog never has to expand a
    /// chain at dispatch.
    #[test]
    fn create_alias_rejects_alias_of_alias() {
        let db = fresh_db();
        // Stage 1 — primary alias: smart-chat → llama-cpp model.
        create_alias(&db, "smart-chat", "qwen-base", None, None)
            .expect("primary alias should be created");

        // Stage 2 — try to chain: meta → smart-chat. Must fail.
        let err = create_alias(&db, "meta", "smart-chat", None, None)
            .expect_err("alias-of-alias must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("alias"),
            "error must mention the chain rule, got: {msg}"
        );
    }

    /// Same rule for fallbacks: a fallback that resolves to another
    /// alias is forbidden.
    #[test]
    fn create_alias_rejects_alias_in_fallback_targets() {
        let db = fresh_db();
        create_alias(&db, "smart-chat", "qwen-base", None, None)
            .expect("primary alias should be created");

        let err = create_alias(
            &db,
            "router-alias",
            "qwen-base",
            None,
            Some(r#"["smart-chat"]"#),
        )
        .expect_err("fallback that is itself an alias must be rejected");
        let msg = format!("{err}");
        assert!(msg.contains("smart-chat"), "error must name the offender, got: {msg}");
    }

    /// R6.P1: an inbound chain — `child → real-model` already exists
    /// active, then someone tries to register `real-model` itself as an
    /// active alias. Outbound check on the new row passes (its target
    /// is a real model), but the runtime now sees a two-step chain.
    /// The inbound check must reject the second insert.
    #[test]
    fn create_alias_rejects_inbound_chain() {
        let db = fresh_db();
        // Existing chain target.
        create_alias(&db, "child", "real-model", None, None)
            .expect("primary alias should be created");

        // Now try to make `real-model` an alias as well.
        let err = create_alias(&db, "real-model", "actual-backend", None, None)
            .expect_err("inbound chain must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("real-model"),
            "error must name the inbound conflict, got: {msg}"
        );
    }

    /// Same inbound check kicks in on reactivation: a parked alias
    /// reused as an active target by someone else cannot be reactivated.
    #[test]
    fn reactivate_rejects_inbound_chain() {
        use crate::db::repository::set_model_alias_active;
        let db = fresh_db();
        // Create then park `gateway`.
        create_alias(&db, "gateway", "real-backend", None, None).unwrap();
        set_model_alias_active(&db, "gateway", false).unwrap();

        // While parked, `gateway` becomes another alias's target.
        create_alias(&db, "user-alias", "gateway", None, None)
            .expect("alias targeting parked name must be allowed");

        // Reactivating `gateway` would create chain user-alias → gateway → real-backend.
        let err = set_model_alias_active(&db, "gateway", true)
            .expect_err("inbound chain must block reactivation");
        let msg = format!("{err}");
        assert!(
            msg.contains("gateway"),
            "error must name the inbound conflict, got: {msg}"
        );
    }

    /// R5.P2.chain: a row that targets a name which becomes an active
    /// alias *after* deactivation must not be allowed to silently
    /// reactivate. `set_model_alias_active(true)` re-runs the chain
    /// check on the stored target/fallbacks before flipping the flag.
    #[test]
    fn reactivation_rejects_chain_introduced_after_deactivation() {
        use crate::db::repository::{
            create_model_alias_with_chain_check, set_model_alias_active,
        };

        let db = fresh_db();
        // 1. Create alias `child → real-model`, then deactivate it.
        let child_id =
            create_model_alias_with_chain_check(&db, "child", "real-model", None, None)
                .expect("child alias create");
        set_model_alias_active(&db, "child", false).expect("deactivate child");

        // 2. While `child` is parked, register a new active alias under
        //    the name `real-model`. Now reactivating `child` would point
        //    at an alias — chain.
        create_model_alias_with_chain_check(&db, "real-model", "actual-backend", None, None)
            .expect("create alias under what was a model name");

        // 3. Reactivation must reject.
        let err = set_model_alias_active(&db, "child", true)
            .expect_err("reactivation must detect the chain");
        let msg = format!("{err}");
        assert!(msg.contains("real-model"), "error must name target, got: {msg}");
        // Avoid an unused-binding warning on child_id while still asserting
        // we created the row we then tried to reactivate.
        let _ = child_id;
    }

    /// Update path mirrors create — repointing an existing alias at
    /// another alias is forbidden.
    #[test]
    fn update_alias_rejects_alias_of_alias() {
        let db = fresh_db();
        create_alias(&db, "primary-alias", "qwen-base", None, None).unwrap();
        let id = create_alias(&db, "raw-alias", "raw-target", None, None).unwrap();

        let err =
            update_alias(&db, id, "raw-alias", "primary-alias", true, None, None)
                .expect_err("update must reject pointing at another alias");
        let msg = format!("{err}");
        assert!(msg.contains("primary-alias"), "error must name target, got: {msg}");
    }
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
