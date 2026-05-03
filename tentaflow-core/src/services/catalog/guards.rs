// =============================================================================
// File: services/catalog/guards.rs
// Pre-write checks for any name that becomes addressable through the public
// catalog. The catalog id space is shared by service models, published flows,
// and aliases — these guards make sure two writers never both claim the same
// id, regardless of which side calls first.
// =============================================================================

use anyhow::Result;
use thiserror::Error;

use crate::db::{repository, DbPool};

/// What kind of write was rejected because the name was already taken. The
/// error carries the conflicting kind so callers can render a useful message
/// (e.g. "alias 'chat-pl' clashes with a published flow").
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GuardError {
    #[error("name is empty")]
    Empty,
    #[error("alias '{name}' clashes with a published flow")]
    AliasVsFlow { name: String },
    #[error("alias '{name}' clashes with an existing alias")]
    AliasVsAlias { name: String },
    #[error("flow publish name '{name}' clashes with an active alias")]
    FlowVsAlias { name: String },
    #[error("flow publish name '{name}' is already used by another flow")]
    FlowVsFlow { name: String },
    #[error("service model '{name}' clashes with an active alias")]
    ServiceVsAlias { name: String },
    #[error("service model '{name}' clashes with a published flow")]
    ServiceVsFlow { name: String },
}

/// Reject if `alias_name` is already taken by another alias (excluding the
/// one being updated, if any) or by a published flow. Service models live in
/// a separate, transient namespace fed by deploys; collisions with them are
/// caught at deploy time, not here, so that adding an alias for a future
/// model name remains possible.
pub fn check_alias_collision(
    pool: &DbPool,
    alias_name: &str,
    excluding_alias_id: Option<i64>,
) -> Result<(), GuardError> {
    let trimmed = alias_name.trim();
    if trimmed.is_empty() {
        return Err(GuardError::Empty);
    }

    let aliases = repository::list_model_aliases(pool)
        .map_err(|_| GuardError::AliasVsAlias { name: trimmed.to_string() })?;
    if aliases
        .iter()
        .any(|a| a.alias == trimmed && Some(a.id) != excluding_alias_id)
    {
        return Err(GuardError::AliasVsAlias { name: trimmed.to_string() });
    }

    if published_flow_name_exists(pool, trimmed)? {
        return Err(GuardError::AliasVsFlow { name: trimmed.to_string() });
    }
    Ok(())
}

/// Reject if `published_name` is taken by an active alias or by another
/// published flow. Used by the flow publish handler before writing
/// `flows.published_model_name`.
pub fn check_flow_publish_collision(
    pool: &DbPool,
    published_name: &str,
    excluding_flow_id: Option<i64>,
) -> Result<(), GuardError> {
    let trimmed = published_name.trim();
    if trimmed.is_empty() {
        return Err(GuardError::Empty);
    }

    let aliases = repository::list_model_aliases(pool)
        .map_err(|_| GuardError::FlowVsAlias { name: trimmed.to_string() })?;
    if aliases.iter().any(|a| a.is_active && a.alias == trimmed) {
        return Err(GuardError::FlowVsAlias { name: trimmed.to_string() });
    }

    let conflicting = published_flow_owner(pool, trimmed)?;
    if let Some(other_id) = conflicting {
        if Some(other_id) != excluding_flow_id {
            return Err(GuardError::FlowVsFlow { name: trimmed.to_string() });
        }
    }
    Ok(())
}

/// Reject when a deploy is about to create a service model whose name is
/// already advertised by an active alias or a published flow. Returning
/// `Ok(())` does not guarantee uniqueness against other concurrent deploys
/// — the deploy layer holds its own per-engine locking — but it does close
/// the alias/flow ↔ deploy collision window.
pub fn check_service_deploy_collision(pool: &DbPool, model_name: &str) -> Result<(), GuardError> {
    let trimmed = model_name.trim();
    if trimmed.is_empty() {
        return Err(GuardError::Empty);
    }

    let aliases = repository::list_model_aliases(pool)
        .map_err(|_| GuardError::ServiceVsAlias { name: trimmed.to_string() })?;
    if aliases.iter().any(|a| a.is_active && a.alias == trimmed) {
        return Err(GuardError::ServiceVsAlias { name: trimmed.to_string() });
    }

    if published_flow_name_exists(pool, trimmed)? {
        return Err(GuardError::ServiceVsFlow { name: trimmed.to_string() });
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Internal helpers — small SQL helpers kept here because they are only ever
// called from the guard checks; making them part of the public repository
// surface would invite reuse for cases that should go through the guard.
// -----------------------------------------------------------------------------

fn published_flow_name_exists(pool: &DbPool, name: &str) -> Result<bool, GuardError> {
    Ok(published_flow_owner(pool, name)?.is_some())
}

fn published_flow_owner(pool: &DbPool, name: &str) -> Result<Option<i64>, GuardError> {
    // Reject the name across every status. Allowing two flows to share
    // `published_model_name` while one is `draft` would create a hidden
    // landmine: the moment the draft is activated it would collide with the
    // already-active flow. The collision is reported now, not later.
    let lookup = || -> Result<Option<i64>> {
        let conn = pool
            .lock()
            .map_err(|_| anyhow::anyhow!("db pool lock poisoned"))?;
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM flows WHERE published_model_name = ?1 LIMIT 1",
        )?;
        let result: Option<i64> = stmt
            .query_row(rusqlite::params![name], |row| row.get(0))
            .ok();
        Ok(result)
    };
    lookup().map_err(|_| GuardError::FlowVsFlow {
        name: name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn fresh_db() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        crate::db::seed::seed_defaults(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn publish_seeded_llm_flow(pool: &DbPool, name: &str) {
        let conn = pool.lock().unwrap();
        conn.execute(
            "UPDATE flows SET published_model_name = ?1 WHERE name = 'Standardowy pipeline LLM'",
            rusqlite::params![name],
        )
        .unwrap();
    }

    fn seed_test_alias(pool: &DbPool, alias: &str, target: &str) -> i64 {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO model_aliases (alias, target_model, is_active) VALUES (?1, ?2, 1)",
            rusqlite::params![alias, target],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn empty_name_is_rejected_everywhere() {
        let pool = fresh_db();
        assert_eq!(check_alias_collision(&pool, "  ", None), Err(GuardError::Empty));
        assert_eq!(
            check_flow_publish_collision(&pool, "", None),
            Err(GuardError::Empty)
        );
        assert_eq!(
            check_service_deploy_collision(&pool, ""),
            Err(GuardError::Empty)
        );
    }

    #[test]
    fn alias_collides_with_existing_alias() {
        let pool = fresh_db();
        seed_test_alias(&pool, "test-alias", "embeddings-gemma");
        match check_alias_collision(&pool, "test-alias", None) {
            Err(GuardError::AliasVsAlias { name }) => assert_eq!(name, "test-alias"),
            other => panic!("expected AliasVsAlias, got {:?}", other),
        }
    }

    #[test]
    fn alias_update_excludes_self() {
        let pool = fresh_db();
        let alias_id = seed_test_alias(&pool, "test-alias", "embeddings-gemma");
        // Updating the same row to keep its name must be allowed.
        check_alias_collision(&pool, "test-alias", Some(alias_id)).unwrap();
    }

    #[test]
    fn alias_collides_with_published_flow() {
        let pool = fresh_db();
        publish_seeded_llm_flow(&pool, "chat-pl");
        match check_alias_collision(&pool, "chat-pl", None) {
            Err(GuardError::AliasVsFlow { name }) => assert_eq!(name, "chat-pl"),
            other => panic!("expected AliasVsFlow, got {:?}", other),
        }
    }

    #[test]
    fn flow_publish_collides_with_alias() {
        let pool = fresh_db();
        seed_test_alias(&pool, "test-alias", "embeddings-gemma");
        match check_flow_publish_collision(&pool, "test-alias", None) {
            Err(GuardError::FlowVsAlias { name }) => assert_eq!(name, "test-alias"),
            other => panic!("expected FlowVsAlias, got {:?}", other),
        }
    }

    #[test]
    fn flow_publish_excludes_self() {
        let pool = fresh_db();
        publish_seeded_llm_flow(&pool, "chat-pl");
        let flow_id = {
            let conn = pool.lock().unwrap();
            conn.query_row(
                "SELECT id FROM flows WHERE name = 'Standardowy pipeline LLM'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        // Re-publishing the same flow under the same name must succeed.
        check_flow_publish_collision(&pool, "chat-pl", Some(flow_id)).unwrap();
    }

    #[test]
    fn service_deploy_collides_with_alias_then_flow() {
        let pool = fresh_db();
        seed_test_alias(&pool, "test-alias", "embeddings-gemma");
        // First: collides with the seeded alias.
        match check_service_deploy_collision(&pool, "test-alias") {
            Err(GuardError::ServiceVsAlias { .. }) => (),
            other => panic!("expected ServiceVsAlias, got {:?}", other),
        }

        // Switch the conflict to a published flow and re-run.
        publish_seeded_llm_flow(&pool, "chat-pl");
        match check_service_deploy_collision(&pool, "chat-pl") {
            Err(GuardError::ServiceVsFlow { .. }) => (),
            other => panic!("expected ServiceVsFlow, got {:?}", other),
        }
    }

    #[test]
    fn service_deploy_with_unique_name_passes() {
        let pool = fresh_db();
        check_service_deploy_collision(&pool, "fresh-llm-name").unwrap();
    }

    /// Surface a draft (non-active) flow that already grabbed a publish
    /// name as a collision — pre-fix the guard ignored draft rows so a
    /// hidden draft would silently steal a name when activated.
    #[test]
    fn flow_publish_collision_includes_draft_status() {
        let pool = fresh_db();
        // Mark the seeded LLM flow as draft *and* publishing under the
        // requested name — that is the regression case.
        {
            let conn = pool.lock().unwrap();
            conn.execute(
                "UPDATE flows SET published_model_name = 'chat-pl', status = 'draft' \
                 WHERE name = 'Standardowy pipeline LLM'",
                [],
            )
            .unwrap();
        }
        match check_flow_publish_collision(&pool, "chat-pl", None) {
            Err(GuardError::FlowVsFlow { name }) => assert_eq!(name, "chat-pl"),
            other => panic!("expected FlowVsFlow, got {:?}", other),
        }
    }
}
