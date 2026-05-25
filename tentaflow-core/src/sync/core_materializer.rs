// =============================================================================
// Plik: sync/core_materializer.rs
// Opis: Bezpieczne aplikowanie odebranych operacji Core Sync do glownej SQLite.
// =============================================================================

use super::core_registry::{CORE_SYNC_ADDON_ID, CoreSyncResourceKind, descriptor_for_table};
use super::ledger::{ActionType, FieldValue, LedgerResult, SyncLedgerError, SyncOperation};
use crate::db::DbPool;

pub fn apply_core_operation(pool: &DbPool, operation: &SyncOperation) -> LedgerResult<usize> {
    if operation.body.addon_id != CORE_SYNC_ADDON_ID {
        return Err(SyncLedgerError::Runtime(format!(
            "operation is not core sync: {}",
            operation.body.addon_id
        )));
    }
    let descriptor = descriptor_for_table(&operation.body.table_name).ok_or_else(|| {
        SyncLedgerError::Runtime(format!(
            "unknown core sync table: {}",
            operation.body.table_name
        ))
    })?;
    if descriptor.resource_type != operation.body.resource_type {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync resource type mismatch for table {}",
            operation.body.table_name
        )));
    }
    let mut conn = pool
        .lock()
        .map_err(|e| SyncLedgerError::Runtime(format!("Blad blokady bazy: {e}")))?;
    let tx = conn
        .transaction()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    let rows = match descriptor.kind {
        CoreSyncResourceKind::Organization => apply_organization(&tx, operation)?,
        CoreSyncResourceKind::UserAccount => apply_user_account(&tx, operation)?,
        CoreSyncResourceKind::UserGroup => apply_user_group(&tx, operation)?,
        CoreSyncResourceKind::GroupMember => apply_group_member(&tx, operation)?,
        CoreSyncResourceKind::Role => apply_role(&tx, operation)?,
        CoreSyncResourceKind::OrgMembership => apply_org_membership(&tx, operation)?,
        CoreSyncResourceKind::Flow => apply_flow(&tx, operation)?,
        CoreSyncResourceKind::FlowVersion => apply_flow_version(&tx, operation)?,
        CoreSyncResourceKind::FlowModelBinding => apply_flow_model_binding(&tx, operation)?,
        CoreSyncResourceKind::LegacyUser => {
            return Err(SyncLedgerError::Runtime(
                "legacy users materialization is disabled".to_string(),
            ));
        }
    };
    tx.commit()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(rows)
}

fn apply_organization(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO organizations \
                 (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(org_id) DO UPDATE SET \
                 name = excluded.name, contact_email = excluded.contact_email, \
                 dpo_contact = excluded.dpo_contact, retention_policy_json = excluded.retention_policy_json, \
                 status = excluded.status",
                rusqlite::params![
                    operation.body.resource_id,
                    field_string(operation, "name")?,
                    field_string(operation, "slug")?,
                    field_optional_string(operation, "contact_email")?,
                    field_optional_string(operation, "dpo_contact")?,
                    field_optional_string(operation, "retention_policy_json")?,
                    field_string_or(operation, "status", "active")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE organizations SET \
                 name = COALESCE(?2, name), \
                 contact_email = CASE WHEN ?3 THEN ?4 ELSE contact_email END, \
                 dpo_contact = CASE WHEN ?5 THEN ?6 ELSE dpo_contact END, \
                 retention_policy_json = CASE WHEN ?7 THEN ?8 ELSE retention_policy_json END, \
                 status = COALESCE(?9, status) \
                 WHERE org_id = ?1",
                rusqlite::params![
                    operation.body.resource_id,
                    optional_present_string(operation, "name")?,
                    nullable_update_string(operation, "contact_email")?.0,
                    nullable_update_string(operation, "contact_email")?.1,
                    nullable_update_string(operation, "dpo_contact")?.0,
                    nullable_update_string(operation, "dpo_contact")?.1,
                    nullable_update_string(operation, "retention_policy_json")?.0,
                    nullable_update_string(operation, "retention_policy_json")?.1,
                    optional_present_string(operation, "status")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "UPDATE organizations SET status = 'deleted' WHERE org_id = ?1",
                rusqlite::params![operation.body.resource_id],
            )
            .map_err(sql_error),
    }
}

fn apply_user_account(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = resource_i64(operation)?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO user_accounts \
                 (id, username, password_hash, display_name, email, is_active, is_admin, role, must_change_password) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1) \
                 ON CONFLICT(id) DO UPDATE SET \
                 display_name = excluded.display_name, email = excluded.email, \
                 is_active = excluded.is_active, is_admin = excluded.is_admin, role = excluded.role, \
                 updated_at = datetime('now')",
                rusqlite::params![
                    id,
                    field_string(operation, "username")?,
                    "!synced-account-no-local-password!",
                    field_string_or(operation, "display_name", "")?,
                    field_string_or(operation, "email", "")?,
                    field_bool_or(operation, "is_active", true)?,
                    field_bool_or(operation, "is_admin", false)?,
                    field_string_or(operation, "role", "user")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => {
            if operation.body.changed_fields.contains_key("password_changed") {
                return Ok(0);
            }
            tx.execute(
                "UPDATE user_accounts SET \
                 display_name = COALESCE(?2, display_name), email = COALESCE(?3, email), \
                 is_active = COALESCE(?4, is_active), is_admin = COALESCE(?5, is_admin), \
                 role = COALESCE(?6, role), updated_at = datetime('now') \
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    optional_present_string(operation, "display_name")?,
                    optional_present_string(operation, "email")?,
                    optional_present_bool(operation, "is_active")?,
                    optional_present_bool(operation, "is_admin")?,
                    optional_present_string(operation, "role")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation))
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM user_accounts WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

fn apply_user_group(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = resource_i64(operation)?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO user_groups (id, name, description) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, description = excluded.description",
                rusqlite::params![
                    id,
                    field_string(operation, "name")?,
                    field_string_or(operation, "description", "")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE user_groups SET name = COALESCE(?2, name), description = COALESCE(?3, description) \
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    optional_present_string(operation, "name")?,
                    optional_present_string(operation, "description")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute("DELETE FROM user_groups WHERE id = ?1", rusqlite::params![id])
            .map_err(sql_error),
    }
}

fn apply_group_member(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let group_id = field_i64(operation, "group_id")?;
    let user_id = field_i64(operation, "user_id")?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?1, ?2)",
                rusqlite::params![group_id, user_id],
            )
            .map_err(sql_error),
        ActionType::Update => Err(SyncLedgerError::Runtime(
            "group_members update is not supported".to_string(),
        )),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
                rusqlite::params![group_id, user_id],
            )
            .map_err(sql_error),
    }
}

fn apply_role(tx: &rusqlite::Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO roles (role_id, name, permissions_json, created_at) \
                 VALUES (?1, ?2, ?3, datetime('now')) \
                 ON CONFLICT(role_id) DO UPDATE SET \
                 name = excluded.name, permissions_json = excluded.permissions_json",
                rusqlite::params![
                    operation.body.resource_id,
                    field_string(operation, "name")?,
                    field_string(operation, "permissions_json")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE roles SET name = COALESCE(?2, name), permissions_json = COALESCE(?3, permissions_json) \
                 WHERE role_id = ?1",
                rusqlite::params![
                    operation.body.resource_id,
                    optional_present_string(operation, "name")?,
                    optional_present_string(operation, "permissions_json")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM roles WHERE role_id = ?1",
                rusqlite::params![operation.body.resource_id],
            )
            .map_err(sql_error),
    }
}

fn apply_org_membership(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let org_id = field_string(operation, "org_id")?;
    let user_id = field_string(operation, "user_id")?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, ?2, ?3, datetime('now'), ?4) \
                 ON CONFLICT(org_id, user_id) DO UPDATE SET \
                 role_id = excluded.role_id, granted_by = excluded.granted_by",
                rusqlite::params![
                    org_id,
                    user_id,
                    field_string(operation, "role_id")?,
                    field_string_or(operation, "granted_by", "sync")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE org_memberships SET role_id = COALESCE(?3, role_id), granted_by = COALESCE(?4, granted_by) \
                 WHERE org_id = ?1 AND user_id = ?2",
                rusqlite::params![
                    org_id,
                    user_id,
                    optional_present_string(operation, "role_id")?,
                    optional_present_string(operation, "granted_by")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
                rusqlite::params![org_id, user_id],
            )
            .map_err(sql_error),
    }
}

fn apply_flow(tx: &rusqlite::Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let id = resource_i64(operation)?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO flows \
                 (id, name, description, is_default, service_type, flow_json, status, published_model_name) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, description = excluded.description, is_default = excluded.is_default, \
                 service_type = excluded.service_type, flow_json = excluded.flow_json, \
                 status = excluded.status, published_model_name = excluded.published_model_name, \
                 updated_at = datetime('now')",
                rusqlite::params![
                    id,
                    field_string(operation, "name")?,
                    field_optional_string(operation, "description")?,
                    field_bool_or(operation, "is_default", false)?,
                    field_optional_string(operation, "service_type")?,
                    field_string(operation, "flow_json")?,
                    field_string_or(operation, "status", "draft")?,
                    field_optional_string(operation, "published_model_name")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE flows SET \
                 name = COALESCE(?2, name), \
                 description = CASE WHEN ?3 THEN ?4 ELSE description END, \
                 is_default = COALESCE(?5, is_default), \
                 service_type = CASE WHEN ?6 THEN ?7 ELSE service_type END, \
                 flow_json = COALESCE(?8, flow_json), status = COALESCE(?9, status), \
                 published_model_name = CASE WHEN ?10 THEN ?11 ELSE published_model_name END, \
                 version = version + 1, updated_at = datetime('now') \
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    optional_present_string(operation, "name")?,
                    nullable_update_string(operation, "description")?.0,
                    nullable_update_string(operation, "description")?.1,
                    optional_present_bool(operation, "is_default")?,
                    nullable_update_string(operation, "service_type")?.0,
                    nullable_update_string(operation, "service_type")?.1,
                    optional_present_string(operation, "flow_json")?,
                    optional_present_string(operation, "status")?,
                    nullable_update_string(operation, "published_model_name")?.0,
                    nullable_update_string(operation, "published_model_name")?.1,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute("DELETE FROM flows WHERE id = ?1", rusqlite::params![id])
            .map_err(sql_error),
    }
}

fn apply_flow_version(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = resource_i64(operation)?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO flow_versions \
                 (id, flow_id, version_num, flow_json, name, description, status, created_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(id) DO UPDATE SET \
                 flow_json = excluded.flow_json, name = excluded.name, description = excluded.description, \
                 status = excluded.status, created_by = excluded.created_by",
                rusqlite::params![
                    id,
                    field_i64(operation, "flow_id")?,
                    field_i64(operation, "version_num")?,
                    field_string(operation, "flow_json")?,
                    field_string(operation, "name")?,
                    field_optional_string(operation, "description")?,
                    field_optional_string(operation, "status")?,
                    field_optional_string(operation, "created_by")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => Err(SyncLedgerError::Runtime(
            "flow_versions update is not supported".to_string(),
        )),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM flow_versions WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

fn apply_flow_model_binding(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = resource_i64(operation)?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO flow_model_bindings (id, flow_id, model_pattern, priority) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(id) DO UPDATE SET \
                 flow_id = excluded.flow_id, model_pattern = excluded.model_pattern, priority = excluded.priority",
                rusqlite::params![
                    id,
                    field_i64(operation, "flow_id")?,
                    field_string(operation, "model_pattern")?,
                    field_i64_or(operation, "priority", 0)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE flow_model_bindings SET flow_id = COALESCE(?2, flow_id), \
                 model_pattern = COALESCE(?3, model_pattern), priority = COALESCE(?4, priority) \
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    optional_present_i64(operation, "flow_id")?,
                    optional_present_string(operation, "model_pattern")?,
                    optional_present_i64(operation, "priority")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM flow_model_bindings WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

fn require_existing(operation: &SyncOperation) -> impl FnOnce(usize) -> LedgerResult<usize> + '_ {
    |rows| {
        if rows == 0 {
            Err(SyncLedgerError::Runtime(format!(
                "core sync target row not found: {}/{}",
                operation.body.resource_type, operation.body.resource_id
            )))
        } else {
            Ok(rows)
        }
    }
}

fn resource_i64(operation: &SyncOperation) -> LedgerResult<i64> {
    operation
        .body
        .resource_id
        .parse::<i64>()
        .map_err(|e| SyncLedgerError::Runtime(format!("invalid integer resource_id: {e}")))
}

fn field_string(operation: &SyncOperation, key: &str) -> LedgerResult<String> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::String(value)) => Ok(value.clone()),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation missing string field: {key}"
        ))),
    }
}

fn field_string_or(operation: &SyncOperation, key: &str, default: &str) -> LedgerResult<String> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::String(value)) => Ok(value.clone()),
        Some(FieldValue::Null) | None => Ok(default.to_string()),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid string type: {key}"
        ))),
    }
}

fn field_optional_string(operation: &SyncOperation, key: &str) -> LedgerResult<Option<String>> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::String(value)) => Ok(Some(value.clone())),
        Some(FieldValue::Null) | None => Ok(None),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid optional string type: {key}"
        ))),
    }
}

fn optional_present_string(operation: &SyncOperation, key: &str) -> LedgerResult<Option<String>> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::String(value)) => Ok(Some(value.clone())),
        Some(FieldValue::Null) | None => Ok(None),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid string type: {key}"
        ))),
    }
}

fn nullable_update_string(
    operation: &SyncOperation,
    key: &str,
) -> LedgerResult<(bool, Option<String>)> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::String(value)) => Ok((true, Some(value.clone()))),
        Some(FieldValue::Null) => Ok((true, None)),
        None => Ok((false, None)),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid nullable string type: {key}"
        ))),
    }
}

fn field_i64(operation: &SyncOperation, key: &str) -> LedgerResult<i64> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::I64(value)) => Ok(*value),
        Some(FieldValue::U64(value)) => i64::try_from(*value)
            .map_err(|e| SyncLedgerError::Runtime(format!("invalid i64 field {key}: {e}"))),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation missing i64 field: {key}"
        ))),
    }
}

fn field_i64_or(operation: &SyncOperation, key: &str, default: i64) -> LedgerResult<i64> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::I64(value)) => Ok(*value),
        Some(FieldValue::U64(value)) => i64::try_from(*value)
            .map_err(|e| SyncLedgerError::Runtime(format!("invalid i64 field {key}: {e}"))),
        Some(FieldValue::Null) | None => Ok(default),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid i64 type: {key}"
        ))),
    }
}

fn optional_present_i64(operation: &SyncOperation, key: &str) -> LedgerResult<Option<i64>> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::I64(value)) => Ok(Some(*value)),
        Some(FieldValue::U64(value)) => i64::try_from(*value)
            .map(Some)
            .map_err(|e| SyncLedgerError::Runtime(format!("invalid i64 field {key}: {e}"))),
        Some(FieldValue::Null) | None => Ok(None),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid i64 type: {key}"
        ))),
    }
}

fn field_bool_or(operation: &SyncOperation, key: &str, default: bool) -> LedgerResult<bool> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::Bool(value)) => Ok(*value),
        Some(FieldValue::Null) | None => Ok(default),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid bool type: {key}"
        ))),
    }
}

fn optional_present_bool(operation: &SyncOperation, key: &str) -> LedgerResult<Option<bool>> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::Bool(value)) => Ok(Some(*value)),
        Some(FieldValue::Null) | None => Ok(None),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid bool type: {key}"
        ))),
    }
}

fn sql_error(error: rusqlite::Error) -> SyncLedgerError {
    SyncLedgerError::Runtime(error.to_string())
}
