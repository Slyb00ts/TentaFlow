// ============ File: services/role_catalog/audit.rs — emittery audit_log dla katalogu rol ============
//
// Kazda mutacja `role_catalog` produkuje wpis w `audit_log` z risk_class='B'
// i pelnym uczestnictwem w Merkle hash chain (F1b P4). `user_id` w audit_log
// jest INTEGER NULL — tutaj uzywamy NULL i przekazujemy `actor_user_id` w
// `details.user_id`, analogicznie do `services/legal/rodo_generator.rs`.

use rusqlite::params;

use super::error::{Result, RoleCatalogError};
use super::Role;
use crate::audit::chain::{compute_chain_for_insert, AuditRowHashInput};
use crate::db::DbPool;

fn map_db<E: std::fmt::Display>(e: E) -> RoleCatalogError {
    RoleCatalogError::DbError(e.to_string())
}

fn now_db_timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn insert_audit_row(
    pool: &DbPool,
    action: &str,
    org_id: &str,
    resource_id: &str,
    details: &str,
) -> Result<()> {
    let conn = pool.write().map_err(map_db)?;
    let timestamp = now_db_timestamp();
    let resource_type = Some("role");
    let result = Some("success");
    let severity = Some("info");
    let risk_class = "B";

    let hash_input = AuditRowHashInput {
        user_id: None,
        addon_id: None,
        instance_id: None,
        action,
        resource: None,
        resource_type,
        resource_id: Some(resource_id),
        result,
        error_message: None,
        details: Some(details),
        ip_address: None,
        node_id: None,
        severity,
        risk_class,
        related_claim_id: None,
        request_id: None,
        timestamp: &timestamp,
    };
    let (prev_hash, hash) = compute_chain_for_insert(&conn, &hash_input).map_err(map_db)?;

    conn.execute(
        "INSERT INTO audit_log \
            (timestamp, action, resource_type, resource_id, result, severity, \
             risk_class, details, org_id, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            timestamp,
            action,
            resource_type,
            resource_id,
            result,
            severity,
            risk_class,
            details,
            org_id,
            prev_hash,
            hash,
        ],
    )
    .map_err(map_db)?;
    Ok(())
}

/// Emituje wpis `role_catalog.created` z pelnym snapshotem nowej roli.
pub fn emit_created(pool: &DbPool, actor_user_id: &str, org_id: &str, role: &Role) -> Result<()> {
    let details = serde_json::json!({
        "user_id": actor_user_id,
        "slug": role.slug,
        "kind": role.kind,
        "name_translations": role.name_translations,
        "description_translations": role.description_translations,
        "icon": role.icon,
        "color_hint": role.color_hint,
        "is_manager": role.is_manager,
        "default_visibility_scope": role.default_visibility_scope,
    })
    .to_string();
    insert_audit_row(pool, "role_catalog.created", org_id, &role.id, &details)
}

/// Emituje wpis `role_catalog.updated` z diffem before -> after w `details`.
pub fn emit_updated(
    pool: &DbPool,
    actor_user_id: &str,
    org_id: &str,
    role_id: &str,
    before: &Role,
    after: &Role,
) -> Result<()> {
    let details = serde_json::json!({
        "user_id": actor_user_id,
        "before": before,
        "after": after,
    })
    .to_string();
    insert_audit_row(pool, "role_catalog.updated", org_id, role_id, &details)
}

/// Emituje wpis `role_catalog.deactivated` (soft-delete).
pub fn emit_deactivated(
    pool: &DbPool,
    actor_user_id: &str,
    org_id: &str,
    role_id: &str,
) -> Result<()> {
    let details = serde_json::json!({
        "user_id": actor_user_id,
    })
    .to_string();
    insert_audit_row(pool, "role_catalog.deactivated", org_id, role_id, &details)
}
