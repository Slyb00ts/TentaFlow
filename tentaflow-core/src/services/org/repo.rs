// ============ File: services/org/repo.rs — DB layer for org/role/membership ============
//
// Pure CRUD over the three multi-tenant tables seeded by migration v32:
//   * organizations          — tenant root, one row per tenant
//   * roles                  — five preseed roles + future custom roles
//   * org_memberships        — many-to-many (user, org) → role assignment
//
// Timestamps are UTC ISO-8601 ("YYYY-MM-DDTHH:MM:SSZ"). Org IDs and role IDs
// are UUIDv4 strings minted at create time; the `org-default` row is the
// single hard-coded id (matches `DEFAULT_ORG_ID` in mod.rs) so migration
// backfills can target it.

use rusqlite::{params, OptionalExtension};
use std::collections::BTreeMap;

use super::error::{OrgError, Result};
use super::{Organization, Role};
use crate::db::DbPool;

fn map_db<E: std::fmt::Display>(e: E) -> OrgError {
    OrgError::DbError(e.to_string())
}

fn now_utc() -> String {
    // SQLite's `datetime('now')` returns "YYYY-MM-DD HH:MM:SS"; we use
    // chrono-free formatting via the std `SystemTime` epoch so callers get a
    // proper ISO-8601 with explicit `Z` suffix. Sub-second precision is not
    // useful for org row creation — drop it deliberately.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
    let (y, mo, d) = days_to_ymd(days as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
}

fn field_string(value: &str) -> crate::sync::ledger::FieldValue {
    crate::sync::ledger::FieldValue::String(value.to_string())
}

fn field_optional_string(value: Option<&str>) -> crate::sync::ledger::FieldValue {
    value
        .map(|v| crate::sync::ledger::FieldValue::String(v.to_string()))
        .unwrap_or(crate::sync::ledger::FieldValue::Null)
}

fn organization_changed_fields(
    name: Option<&str>,
    slug: Option<&str>,
    contact_email: Option<Option<&str>>,
    dpo_contact: Option<Option<&str>>,
    retention_policy_json: Option<Option<&str>>,
    status: Option<&str>,
) -> BTreeMap<String, crate::sync::ledger::FieldValue> {
    let mut fields = BTreeMap::new();
    if let Some(value) = name {
        fields.insert("name".to_string(), field_string(value));
    }
    if let Some(value) = slug {
        fields.insert("slug".to_string(), field_string(value));
    }
    if let Some(value) = contact_email {
        fields.insert("contact_email".to_string(), field_optional_string(value));
    }
    if let Some(value) = dpo_contact {
        fields.insert("dpo_contact".to_string(), field_optional_string(value));
    }
    if let Some(value) = retention_policy_json {
        fields.insert(
            "retention_policy_json".to_string(),
            field_optional_string(value),
        );
    }
    if let Some(value) = status {
        fields.insert("status".to_string(), field_string(value));
    }
    fields
}

fn org_membership_resource_id(org_id: &str, user_id: &str) -> String {
    format!("{}:{}", org_id, user_id)
}

fn org_membership_changed_fields(
    org_id: &str,
    user_id: &str,
    role_id: Option<&str>,
    granted_by: Option<&str>,
) -> BTreeMap<String, crate::sync::ledger::FieldValue> {
    let mut fields = BTreeMap::new();
    fields.insert("org_id".to_string(), field_string(org_id));
    fields.insert("user_id".to_string(), field_string(user_id));
    if let Some(value) = role_id {
        fields.insert("role_id".to_string(), field_string(value));
    }
    if let Some(value) = granted_by {
        fields.insert("granted_by".to_string(), field_string(value));
    }
    fields
}

fn record_core_capture_tx(
    tx: &rusqlite::Transaction<'_>,
    org_id: &str,
    kind: crate::sync::core_registry::CoreSyncResourceKind,
    resource_id: impl Into<String>,
    action: crate::sync::runtime::SqlWriteAction,
    changed_fields: BTreeMap<String, crate::sync::ledger::FieldValue>,
    actor_user_id: Option<String>,
) -> Result<()> {
    let resource_id = resource_id.into();
    // Mint the HLC inside this write transaction so the capture, the drained
    // ledger operation and the local resource-version index share one instant.
    let hlc = crate::sync::runtime::core_hlc_now();
    let epoch = crate::sync::runtime::core_epoch();
    let descriptor = crate::sync::core_registry::descriptor_for_kind(kind);
    let capture = crate::sync::core_capture::CoreWriteCapture::new(
        kind,
        org_id,
        resource_id.clone(),
        action,
        changed_fields,
        actor_user_id,
        hlc.clone(),
        epoch,
    );
    crate::sync::core_capture::record_core_write_capture(tx, &capture).map_err(map_db)?;
    tx.execute(
        "INSERT INTO core_resource_versions (resource_type, resource_id, hlc_wall, hlc_logical, hlc_node) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(resource_type, resource_id) DO UPDATE SET \
         hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical, hlc_node = excluded.hlc_node",
        params![
            descriptor.resource_type,
            resource_id,
            hlc.wall_time_ms,
            hlc.logical as i64,
            hlc.node_id,
        ],
    )
    .map_err(map_db)?;
    Ok(())
}

// Civil-from-days (Howard Hinnant). Computes (year, month, day) from days
// since 1970-01-01. Mirrors the chrono-free helper used elsewhere in the
// codebase to keep this module independent of the chrono / time crate.
fn days_to_ymd(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub fn create_organization(
    pool: &DbPool,
    name: &str,
    slug: &str,
    contact_email: Option<&str>,
    dpo_contact: Option<&str>,
    retention_policy_json: Option<&str>,
    _created_by_user_id: Option<&str>,
) -> Result<Organization> {
    let mut conn = pool.write().map_err(|e| OrgError::DbError(e.to_string()))?;
    let tx = conn.transaction().map_err(map_db)?;
    let org_id = uuid::Uuid::new_v4().to_string();
    let created_at = now_utc();

    let res = tx.execute(
        "INSERT INTO organizations (org_id, name, slug, contact_email, dpo_contact, \
            retention_policy_json, status, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7)",
        params![
            org_id,
            name,
            slug,
            contact_email,
            dpo_contact,
            retention_policy_json,
            created_at,
        ],
    );
    match res {
        Ok(_) => {
            crate::compliance::repository::ensure_org_defaults(&tx, &org_id).map_err(map_db)?;
            record_core_capture_tx(
                &tx,
                &org_id,
                crate::sync::core_registry::CoreSyncResourceKind::Organization,
                org_id.clone(),
                crate::sync::runtime::SqlWriteAction::Insert,
                organization_changed_fields(
                    Some(name),
                    Some(slug),
                    Some(contact_email),
                    Some(dpo_contact),
                    Some(retention_policy_json),
                    Some("active"),
                ),
                _created_by_user_id.map(|id| id.to_string()),
            )?;
            tx.commit().map_err(map_db)?;
            Ok(Organization {
                org_id,
                name: name.to_string(),
                slug: slug.to_string(),
                contact_email: contact_email.map(String::from),
                dpo_contact: dpo_contact.map(String::from),
                retention_policy_json: retention_policy_json.map(String::from),
                status: "active".to_string(),
                created_at,
            })
        }
        Err(rusqlite::Error::SqliteFailure(ref err, ref msg))
            if err.code == rusqlite::ErrorCode::ConstraintViolation
                && msg
                    .as_deref()
                    .map(|m| m.contains("organizations.slug"))
                    .unwrap_or(false) =>
        {
            Err(OrgError::SlugConflict(slug.to_string()))
        }
        Err(e) => Err(map_db(e)),
    }
}

pub fn get_organization(pool: &DbPool, org_id: &str) -> Result<Option<Organization>> {
    let conn = pool.read().map_err(|e| OrgError::DbError(e.to_string()))?;
    conn.query_row(
        "SELECT org_id, name, slug, contact_email, dpo_contact, retention_policy_json, \
                status, created_at \
         FROM organizations WHERE org_id = ?1",
        params![org_id],
        |row| {
            Ok(Organization {
                org_id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                contact_email: row.get(3)?,
                dpo_contact: row.get(4)?,
                retention_policy_json: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(map_db)
}

pub fn list_organizations(pool: &DbPool, status_filter: Option<&str>) -> Result<Vec<Organization>> {
    let conn = pool.read().map_err(|e| OrgError::DbError(e.to_string()))?;
    let (sql, has_filter) = match status_filter {
        Some(_) => (
            "SELECT org_id, name, slug, contact_email, dpo_contact, retention_policy_json, \
                    status, created_at \
             FROM organizations WHERE status = ?1 ORDER BY created_at ASC, org_id ASC",
            true,
        ),
        None => (
            "SELECT org_id, name, slug, contact_email, dpo_contact, retention_policy_json, \
                    status, created_at \
             FROM organizations ORDER BY created_at ASC, org_id ASC",
            false,
        ),
    };
    let mut stmt = conn.prepare(sql).map_err(map_db)?;
    let mapper = |row: &rusqlite::Row<'_>| {
        Ok(Organization {
            org_id: row.get(0)?,
            name: row.get(1)?,
            slug: row.get(2)?,
            contact_email: row.get(3)?,
            dpo_contact: row.get(4)?,
            retention_policy_json: row.get(5)?,
            status: row.get(6)?,
            created_at: row.get(7)?,
        })
    };
    let iter = if has_filter {
        stmt.query_map(params![status_filter.unwrap()], mapper)
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
    } else {
        stmt.query_map([], mapper)
            .map_err(map_db)?
            .collect::<std::result::Result<Vec<_>, _>>()
    };
    iter.map_err(map_db)
}

pub fn update_organization(pool: &DbPool, org_id: &str, patch: &OrgPatch) -> Result<bool> {
    let mut conn = pool.write().map_err(|e| OrgError::DbError(e.to_string()))?;
    let tx = conn.transaction().map_err(map_db)?;

    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(name) = &patch.name {
        sets.push(format!("name = ?{}", binds.len() + 1));
        binds.push(Box::new(name.clone()));
    }
    if let Some(email_patch) = &patch.contact_email {
        sets.push(format!("contact_email = ?{}", binds.len() + 1));
        binds.push(Box::new(email_patch.clone()));
    }
    if let Some(dpo_patch) = &patch.dpo_contact {
        sets.push(format!("dpo_contact = ?{}", binds.len() + 1));
        binds.push(Box::new(dpo_patch.clone()));
    }
    if let Some(rp) = &patch.retention_policy_json {
        sets.push(format!("retention_policy_json = ?{}", binds.len() + 1));
        binds.push(Box::new(rp.clone()));
    }
    if let Some(status) = &patch.status {
        sets.push(format!("status = ?{}", binds.len() + 1));
        binds.push(Box::new(status.clone()));
    }

    if sets.is_empty() {
        // Nothing to update — verify the row exists so callers can distinguish
        // "not found" from "no-op".
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM organizations WHERE org_id = ?1",
                params![org_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_db)?;
        return Ok(exists.is_some());
    }

    let sql = format!(
        "UPDATE organizations SET {} WHERE org_id = ?{}",
        sets.join(", "),
        binds.len() + 1
    );
    binds.push(Box::new(org_id.to_string()));
    let params_dyn: Vec<&dyn rusqlite::ToSql> = binds
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    let n = tx
        .execute(&sql, rusqlite::params_from_iter(params_dyn.iter().copied()))
        .map_err(map_db)?;
    if n > 0 {
        record_core_capture_tx(
            &tx,
            org_id,
            crate::sync::core_registry::CoreSyncResourceKind::Organization,
            org_id.to_string(),
            crate::sync::runtime::SqlWriteAction::Update,
            organization_changed_fields(
                patch.name.as_deref(),
                None,
                patch.contact_email.as_ref().map(|v| v.as_deref()),
                patch.dpo_contact.as_ref().map(|v| v.as_deref()),
                patch.retention_policy_json.as_ref().map(|v| v.as_deref()),
                patch.status.as_deref(),
            ),
            None,
        )?;
    }
    tx.commit().map_err(map_db)?;
    Ok(n > 0)
}

pub fn delete_organization(pool: &DbPool, org_id: &str) -> Result<bool> {
    let mut conn = pool.write().map_err(|e| OrgError::DbError(e.to_string()))?;
    let tx = conn.transaction().map_err(map_db)?;
    let n = tx
        .execute(
            "UPDATE organizations SET status = 'deleted' WHERE org_id = ?1 AND status != 'deleted'",
            params![org_id],
        )
        .map_err(map_db)?;
    if n > 0 {
        record_core_capture_tx(
            &tx,
            org_id,
            crate::sync::core_registry::CoreSyncResourceKind::Organization,
            org_id.to_string(),
            crate::sync::runtime::SqlWriteAction::Update,
            organization_changed_fields(None, None, None, None, None, Some("deleted")),
            None,
        )?;
    }
    tx.commit().map_err(map_db)?;
    drop(conn);
    if n > 0 {
        // Org deletion implicitly revokes every per-org gate decision; the
        // permission matrix is invalidated wholesale by callers/the next
        // membership write, but the gate-check cache is keyed on org_id in
        // the ctx hash so a wholesale flush is required here.
        crate::services::policy::GateCheckCache::global().invalidate_all();
    }
    Ok(n > 0)
}

pub fn add_membership(
    pool: &DbPool,
    org_id: &str,
    user_id: &str,
    role_id: &str,
    granted_by: &str,
) -> Result<bool> {
    let mut conn = pool.write().map_err(|e| OrgError::DbError(e.to_string()))?;
    let tx = conn.transaction().map_err(map_db)?;
    // Validate the foreign references before INSERT so we can surface
    // structured errors instead of bare FK constraint failures.
    let org_exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM organizations WHERE org_id = ?1",
            params![org_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_db)?;
    if org_exists.is_none() {
        return Err(OrgError::NotFound(org_id.to_string()));
    }
    let role_exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM roles WHERE role_id = ?1",
            params![role_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_db)?;
    if role_exists.is_none() {
        return Err(OrgError::RoleNotFound(role_id.to_string()));
    }

    let granted_at = now_utc();
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![org_id, user_id, role_id, granted_at, granted_by],
        )
        .map_err(map_db)?;
    if n > 0 {
        record_core_capture_tx(
            &tx,
            org_id,
            crate::sync::core_registry::CoreSyncResourceKind::OrgMembership,
            org_membership_resource_id(org_id, user_id),
            crate::sync::runtime::SqlWriteAction::Insert,
            org_membership_changed_fields(org_id, user_id, Some(role_id), Some(granted_by)),
            None,
        )?;
    }
    tx.commit().map_err(map_db)?;
    drop(conn);
    if n > 0 {
        crate::db::repository::bump_sync_permission_epoch(pool, org_id).map_err(map_db)?;
        // Drop the DB guard before touching the RBAC cache so a future cache
        // implementation that re-reads the DB cannot deadlock on the same pool.
        crate::services::rbac::PermissionMatrix::global().invalidate(user_id, org_id);
        // Membership rebinds can flip gate decisions (a role's permission set
        // implicitly affects gate eligibility). Flush the gate-check cache
        // wholesale — keyed by ctx_hash, no reverse index by user/org exists.
        crate::services::policy::GateCheckCache::global().invalidate_all();
    }
    Ok(n > 0)
}

pub fn remove_membership(pool: &DbPool, org_id: &str, user_id: &str) -> Result<bool> {
    let mut conn = pool.write().map_err(|e| OrgError::DbError(e.to_string()))?;
    let tx = conn.transaction().map_err(map_db)?;
    let n = tx
        .execute(
            "DELETE FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
            params![org_id, user_id],
        )
        .map_err(map_db)?;
    if n > 0 {
        record_core_capture_tx(
            &tx,
            org_id,
            crate::sync::core_registry::CoreSyncResourceKind::OrgMembership,
            org_membership_resource_id(org_id, user_id),
            crate::sync::runtime::SqlWriteAction::Delete,
            org_membership_changed_fields(org_id, user_id, None, None),
            None,
        )?;
    }
    tx.commit().map_err(map_db)?;
    drop(conn);
    if n > 0 {
        crate::db::repository::bump_sync_permission_epoch(pool, org_id).map_err(map_db)?;
        crate::services::rbac::PermissionMatrix::global().invalidate(user_id, org_id);
        // Membership rebinds can flip gate decisions (a role's permission set
        // implicitly affects gate eligibility). Flush the gate-check cache
        // wholesale — keyed by ctx_hash, no reverse index by user/org exists.
        crate::services::policy::GateCheckCache::global().invalidate_all();
    }
    Ok(n > 0)
}

pub fn list_memberships_for_user(
    pool: &DbPool,
    user_id: &str,
) -> Result<Vec<(Organization, Role)>> {
    let conn = pool.read().map_err(|e| OrgError::DbError(e.to_string()))?;
    let mut stmt = conn
        .prepare(
            "SELECT o.org_id, o.name, o.slug, o.contact_email, o.dpo_contact, \
                    o.retention_policy_json, o.status, o.created_at, \
                    r.role_id, r.name, r.permissions_json \
             FROM org_memberships m \
             JOIN organizations o ON o.org_id = m.org_id \
             JOIN roles r ON r.role_id = m.role_id \
             WHERE m.user_id = ?1 \
             ORDER BY o.created_at ASC, o.org_id ASC",
        )
        .map_err(map_db)?;
    let rows = stmt
        .query_map(params![user_id], |row| {
            let org = Organization {
                org_id: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                contact_email: row.get(3)?,
                dpo_contact: row.get(4)?,
                retention_policy_json: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
            };
            let role_id: String = row.get(8)?;
            let role_name: String = row.get(9)?;
            let perms_json: String = row.get(10)?;
            Ok((org, role_id, role_name, perms_json))
        })
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        let (org, role_id, role_name, perms_json) = r.map_err(map_db)?;
        let permissions = parse_permissions(&perms_json)?;
        out.push((
            org,
            Role {
                role_id,
                name: role_name,
                permissions,
            },
        ));
    }
    Ok(out)
}

pub fn list_memberships_for_org(pool: &DbPool, org_id: &str) -> Result<Vec<(String, Role)>> {
    let conn = pool.read().map_err(|e| OrgError::DbError(e.to_string()))?;
    let mut stmt = conn
        .prepare(
            "SELECT m.user_id, r.role_id, r.name, r.permissions_json \
             FROM org_memberships m \
             JOIN roles r ON r.role_id = m.role_id \
             WHERE m.org_id = ?1 \
             ORDER BY m.granted_at ASC, m.user_id ASC",
        )
        .map_err(map_db)?;
    let rows = stmt
        .query_map(params![org_id], |row| {
            let user_id: String = row.get(0)?;
            let role_id: String = row.get(1)?;
            let role_name: String = row.get(2)?;
            let perms_json: String = row.get(3)?;
            Ok((user_id, role_id, role_name, perms_json))
        })
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        let (user_id, role_id, role_name, perms_json) = r.map_err(map_db)?;
        let permissions = parse_permissions(&perms_json)?;
        out.push((
            user_id,
            Role {
                role_id,
                name: role_name,
                permissions,
            },
        ));
    }
    Ok(out)
}

pub fn get_user_role_in_org(pool: &DbPool, user_id: &str, org_id: &str) -> Result<Option<Role>> {
    let conn = pool.read().map_err(|e| OrgError::DbError(e.to_string()))?;
    let row = conn
        .query_row(
            "SELECT r.role_id, r.name, r.permissions_json \
             FROM org_memberships m \
             JOIN roles r ON r.role_id = m.role_id \
             WHERE m.user_id = ?1 AND m.org_id = ?2",
            params![user_id, org_id],
            |row| {
                let role_id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let perms_json: String = row.get(2)?;
                Ok((role_id, name, perms_json))
            },
        )
        .optional()
        .map_err(map_db)?;
    match row {
        Some((role_id, name, perms_json)) => Ok(Some(Role {
            role_id,
            name,
            permissions: parse_permissions(&perms_json)?,
        })),
        None => Ok(None),
    }
}

pub fn list_roles(pool: &DbPool) -> Result<Vec<Role>> {
    let conn = pool.read().map_err(|e| OrgError::DbError(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT role_id, name, permissions_json FROM roles ORDER BY name ASC")
        .map_err(map_db)?;
    let rows = stmt
        .query_map([], |row| {
            let role_id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let perms_json: String = row.get(2)?;
            Ok((role_id, name, perms_json))
        })
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        let (role_id, name, perms_json) = r.map_err(map_db)?;
        out.push(Role {
            role_id,
            name,
            permissions: parse_permissions(&perms_json)?,
        });
    }
    Ok(out)
}

fn parse_permissions(perms_json: &str) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(perms_json)
        .map_err(|e| OrgError::DbError(format!("invalid permissions_json: {}", e)))
}

// Re-export the public patch type under the documented name. We define the
// struct here (rather than in mod.rs) so the repository layer owns the
// physical column mapping; mod.rs re-exports for callers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OrgPatch {
    pub name: Option<String>,
    pub contact_email: Option<Option<String>>,
    pub dpo_contact: Option<Option<String>>,
    pub retention_policy_json: Option<Option<String>>,
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use crate::sync::core_capture::load_core_write_capture;
    use crate::sync::ledger::FieldValue;
    use crate::sync::runtime::SqlWriteAction;
    use tempfile::TempDir;

    fn open_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("org_test.db");
        let pool = crate::db::init(&path).expect("init DB");
        (dir, pool)
    }

    fn capture_id_for_resource(pool: &DbPool, resource_type: &str, resource_id: &str) -> String {
        let conn = pool.read().expect("read");
        conn.query_row(
            "SELECT capture_id FROM __tentaflow_core_sync_captures \
             WHERE resource_type = ?1 AND resource_id = ?2 \
             ORDER BY created_at_ms DESC LIMIT 1",
            params![resource_type, resource_id],
            |row| row.get(0),
        )
        .expect("capture id")
    }

    #[test]
    fn create_organization_assigns_uuid_and_now() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "Acme", "acme", None, None, None, None).unwrap();
        assert_eq!(org.name, "Acme");
        assert_eq!(org.slug, "acme");
        assert_eq!(org.status, "active");
        // UUIDv4 dash format is 36 chars.
        assert_eq!(org.org_id.len(), 36);
        // created_at follows our ISO-8601 with Z.
        assert!(org.created_at.ends_with('Z'));
        assert!(org.created_at.contains('T'));
    }

    #[test]
    fn create_organization_rejects_slug_conflict() {
        let (_d, pool) = open_pool();
        create_organization(&pool, "Acme", "acme", None, None, None, None).unwrap();
        let err = create_organization(&pool, "Acme2", "acme", None, None, None, None).unwrap_err();
        assert!(matches!(err, OrgError::SlugConflict(s) if s == "acme"));
    }

    #[test]
    fn get_organization_returns_none_for_unknown() {
        let (_d, pool) = open_pool();
        assert!(get_organization(&pool, "ghost").unwrap().is_none());
    }

    #[test]
    fn list_organizations_filters_by_status() {
        let (_d, pool) = open_pool();
        let a = create_organization(&pool, "A", "a", None, None, None, None).unwrap();
        let b = create_organization(&pool, "B", "b", None, None, None, None).unwrap();
        delete_organization(&pool, &b.org_id).unwrap();
        let active = list_organizations(&pool, Some("active")).unwrap();
        // org-default seed + a (b is deleted).
        let ids: Vec<&str> = active.iter().map(|o| o.org_id.as_str()).collect();
        assert!(ids.contains(&a.org_id.as_str()));
        assert!(!ids.contains(&b.org_id.as_str()));
        let deleted = list_organizations(&pool, Some("deleted")).unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].org_id, b.org_id);
    }

    #[test]
    fn update_organization_patches_only_provided_fields() {
        let (_d, pool) = open_pool();
        let org =
            create_organization(&pool, "Old", "oldslug", Some("a@x"), None, None, None).unwrap();
        let patch = OrgPatch {
            name: Some("New".to_string()),
            ..Default::default()
        };
        assert!(update_organization(&pool, &org.org_id, &patch).unwrap());
        let got = get_organization(&pool, &org.org_id).unwrap().unwrap();
        assert_eq!(got.name, "New");
        assert_eq!(got.slug, "oldslug");
        assert_eq!(got.contact_email.as_deref(), Some("a@x"));
    }

    #[test]
    fn update_organization_double_option_sets_to_null() {
        let (_d, pool) = open_pool();
        let org = create_organization(
            &pool,
            "T",
            "t",
            Some("contact@x"),
            Some("dpo@x"),
            None,
            None,
        )
        .unwrap();
        let patch = OrgPatch {
            contact_email: Some(None),
            ..Default::default()
        };
        assert!(update_organization(&pool, &org.org_id, &patch).unwrap());
        let got = get_organization(&pool, &org.org_id).unwrap().unwrap();
        assert!(got.contact_email.is_none());
        assert_eq!(got.dpo_contact.as_deref(), Some("dpo@x"));
    }

    #[test]
    fn delete_organization_soft_deletes() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "X", "x", None, None, None, None).unwrap();
        assert!(delete_organization(&pool, &org.org_id).unwrap());
        let got = get_organization(&pool, &org.org_id).unwrap().unwrap();
        assert_eq!(got.status, "deleted");
        // Second delete is a no-op (status already 'deleted').
        assert!(!delete_organization(&pool, &org.org_id).unwrap());
    }

    #[test]
    fn add_membership_idempotent() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "Org", "org", None, None, None, None).unwrap();
        let roles = list_roles(&pool).unwrap();
        let viewer = roles.iter().find(|r| r.name == "org_viewer").unwrap();
        assert!(add_membership(&pool, &org.org_id, "user-1", &viewer.role_id, "admin").unwrap());
        // Second call is a no-op (INSERT OR IGNORE returns 0 affected).
        assert!(!add_membership(&pool, &org.org_id, "user-1", &viewer.role_id, "admin").unwrap());
    }

    #[test]
    fn create_organization_records_core_capture() {
        let (_d, pool) = open_pool();
        let actor_id = crate::db::repository::create_user_account(
            &pool,
            "org-actor",
            "hash",
            "Org Actor",
            "org-actor@example.com",
        )
        .unwrap();
        let org = create_organization(
            &pool,
            "Captured",
            "captured",
            Some("captured@example.com"),
            None,
            None,
            Some(actor_id.as_str()),
        )
        .unwrap();
        let capture_id = capture_id_for_resource(&pool, "core.organization", &org.org_id);
        let conn = pool.read().expect("read");
        let capture = load_core_write_capture(&conn, &capture_id)
            .expect("load capture")
            .expect("capture");

        assert_eq!(capture.action, SqlWriteAction::Insert);
        assert_eq!(capture.actor_user_id, Some(actor_id));
        assert_eq!(
            capture.changed_fields.get("name"),
            Some(&FieldValue::String("Captured".to_string()))
        );
    }

    #[test]
    fn add_membership_records_core_capture() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "Org2", "org2", None, None, None, None).unwrap();
        let role = list_roles(&pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_viewer")
            .unwrap();
        add_membership(&pool, &org.org_id, "user-2", &role.role_id, "admin").unwrap();
        let resource_id = org_membership_resource_id(&org.org_id, "user-2");
        let capture_id = capture_id_for_resource(&pool, "core.org_membership", &resource_id);
        let conn = pool.read().expect("read");
        let capture = load_core_write_capture(&conn, &capture_id)
            .expect("load capture")
            .expect("capture");

        assert_eq!(capture.action, SqlWriteAction::Insert);
        assert_eq!(
            capture.changed_fields.get("role_id"),
            Some(&FieldValue::String(role.role_id))
        );
    }

    #[test]
    fn add_membership_bumps_sync_permission_epoch() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "Org3", "org3", None, None, None, None).unwrap();
        let role = list_roles(&pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_viewer")
            .unwrap();
        let before = crate::db::repository::get_sync_permission_epoch(&pool, &org.org_id).unwrap();
        assert!(add_membership(&pool, &org.org_id, "user-3", &role.role_id, "admin").unwrap());
        let after = crate::db::repository::get_sync_permission_epoch(&pool, &org.org_id).unwrap();

        assert!(after > before);
    }

    #[test]
    fn remove_membership_returns_false_for_missing() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "Org", "org", None, None, None, None).unwrap();
        assert!(!remove_membership(&pool, &org.org_id, "user-1").unwrap());
    }

    #[test]
    fn list_memberships_for_user_joins_organizations() {
        let (_d, pool) = open_pool();
        let o1 = create_organization(&pool, "O1", "o1", None, None, None, None).unwrap();
        let o2 = create_organization(&pool, "O2", "o2", None, None, None, None).unwrap();
        let admin_role = list_roles(&pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_admin")
            .unwrap();
        let viewer_role = list_roles(&pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_viewer")
            .unwrap();
        add_membership(&pool, &o1.org_id, "u-1", &admin_role.role_id, "boot").unwrap();
        add_membership(&pool, &o2.org_id, "u-1", &viewer_role.role_id, "boot").unwrap();
        add_membership(&pool, &o2.org_id, "u-2", &admin_role.role_id, "boot").unwrap();

        let rows = list_memberships_for_user(&pool, "u-1").unwrap();
        assert_eq!(rows.len(), 2);
        let by_org: std::collections::HashMap<_, _> = rows
            .iter()
            .map(|(o, r)| (o.org_id.clone(), r.name.clone()))
            .collect();
        assert_eq!(
            by_org.get(&o1.org_id).map(String::as_str),
            Some("org_admin")
        );
        assert_eq!(
            by_org.get(&o2.org_id).map(String::as_str),
            Some("org_viewer")
        );
    }

    #[test]
    fn get_user_role_in_org_returns_role_with_permissions_parsed() {
        let (_d, pool) = open_pool();
        let org = create_organization(&pool, "O", "o", None, None, None, None).unwrap();
        let admin_role = list_roles(&pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == "org_admin")
            .unwrap();
        add_membership(&pool, &org.org_id, "u-1", &admin_role.role_id, "boot").unwrap();
        let role = get_user_role_in_org(&pool, "u-1", &org.org_id)
            .unwrap()
            .expect("role");
        assert_eq!(role.name, "org_admin");
        assert!(role.permissions.contains(&"org.admin".to_string()));
        assert!(role.permissions.contains(&"rbac.elevate".to_string()));
    }

    #[test]
    fn list_roles_returns_5_preseed_roles_after_migration() {
        let (_d, pool) = open_pool();
        let roles = list_roles(&pool).unwrap();
        assert_eq!(roles.len(), 5);
        let names: std::collections::HashSet<_> = roles.iter().map(|r| r.name.as_str()).collect();
        for expected in [
            "org_admin",
            "org_operator",
            "org_viewer",
            "dpo",
            "supervisor",
        ] {
            assert!(names.contains(expected), "missing role {}", expected);
        }
    }
}
