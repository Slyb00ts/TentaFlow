// =============================================================================
// Plik: sync/core_materializer.rs
// Opis: Bezpieczne aplikowanie odebranych operacji Core Sync do glownej SQLite.
// =============================================================================

use super::core_registry::{descriptor_for_table, CoreSyncResourceKind, CORE_SYNC_ADDON_ID};
use super::ledger::{
    ActionType, FieldValue, HybridLogicalTimestamp, LedgerResult, SyncLedgerError, SyncOperation,
};
use crate::db::DbPool;
use rusqlite::OptionalExtension;
use std::sync::Arc;

/// Resource kinds that multiple nodes may edit concurrently. Their writes go
/// through HLC last-writer-wins: an incoming operation is applied only when its
/// HLC strictly exceeds the version recorded in `core_resource_versions`. The
/// remaining kinds are either insert-only (group_members, org_memberships) or
/// keyed so that concurrent edits never collide, so they bypass LWW.
fn is_lww_tracked(kind: CoreSyncResourceKind) -> bool {
    matches!(
        kind,
        CoreSyncResourceKind::Flow
            | CoreSyncResourceKind::UserAccount
            | CoreSyncResourceKind::Organization
            | CoreSyncResourceKind::Role
            | CoreSyncResourceKind::UserGroup
            | CoreSyncResourceKind::SyncPolicy
            | CoreSyncResourceKind::SyncResourceAcl
            | CoreSyncResourceKind::SyncUserOrgProfile
            | CoreSyncResourceKind::AddonInstance
            | CoreSyncResourceKind::AddonConfig
    )
}

pub fn apply_core_operation(
    pool: &DbPool,
    settings_cipher: &Arc<crate::crypto::SettingsCipher>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
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
    // Fold the incoming HLC into the local clock so the next locally-minted
    // operation is strictly later than anything we have observed from the mesh.
    crate::sync::runtime::observe_core_hlc(&operation.body.hlc_timestamp);

    let lww_tracked = is_lww_tracked(descriptor.kind);
    let mut conn = pool
        .lock()
        .map_err(|e| SyncLedgerError::Runtime(format!("Blad blokady bazy: {e}")))?;
    let tx = conn
        .transaction()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;

    // HLC last-writer-wins gate: drop stale concurrent edits before touching the
    // target row, so a slower-but-older write never clobbers a newer one.
    if lww_tracked
        && !incoming_hlc_wins(
            &tx,
            &operation.body.resource_type,
            &operation.body.resource_id,
            &operation.body.hlc_timestamp,
        )?
    {
        tx.commit()
            .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
        return Ok(0);
    }

    let rows = match descriptor.kind {
        CoreSyncResourceKind::Organization => apply_organization(&tx, operation)?,
        CoreSyncResourceKind::UserAccount => apply_user_account(&tx, operation)?,
        CoreSyncResourceKind::UserGroup => apply_user_group(&tx, operation)?,
        CoreSyncResourceKind::GroupMember => apply_group_member(&tx, operation)?,
        CoreSyncResourceKind::Role => apply_role(&tx, operation)?,
        CoreSyncResourceKind::OrgMembership => apply_org_membership(&tx, operation)?,
        CoreSyncResourceKind::SyncNode => apply_sync_node(&tx, operation)?,
        CoreSyncResourceKind::UserIdentityKey => apply_user_identity_key(&tx, operation)?,
        CoreSyncResourceKind::NodeUserAssignment => apply_node_user_assignment(&tx, operation)?,
        CoreSyncResourceKind::SyncUserOrgProfile => apply_sync_user_org_profile(&tx, operation)?,
        CoreSyncResourceKind::Flow => apply_flow(&tx, operation)?,
        CoreSyncResourceKind::FlowModelBinding => apply_flow_model_binding(&tx, operation)?,
        CoreSyncResourceKind::SyncPolicy => apply_sync_policy(&tx, operation)?,
        CoreSyncResourceKind::SyncResourceAcl => apply_sync_resource_acl(&tx, operation)?,
        CoreSyncResourceKind::SyncExplicitShare => apply_sync_explicit_share(&tx, operation)?,
        CoreSyncResourceKind::SharedSettingSecret => {
            apply_shared_setting_secret(&tx, settings_cipher, operation)?
        }
        CoreSyncResourceKind::AddonInstance => apply_addon_instance(&tx, operation)?,
        CoreSyncResourceKind::AddonConfig => apply_addon_config(&tx, operation)?,
    };

    if lww_tracked {
        upsert_resource_version(
            &tx,
            &operation.body.resource_type,
            &operation.body.resource_id,
            &operation.body.hlc_timestamp,
        )?;
    }

    tx.commit()
        .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?;
    Ok(rows)
}

/// Returns true when `incoming` strictly exceeds the HLC currently recorded for
/// `(resource_type, resource_id)` in `core_resource_versions`. A missing row
/// (never-seen resource) always wins. Comparison uses the total HLC order from
/// phase A (wall, logical, node_id tie-break).
fn incoming_hlc_wins(
    tx: &rusqlite::Transaction<'_>,
    resource_type: &str,
    resource_id: &str,
    incoming: &HybridLogicalTimestamp,
) -> LedgerResult<bool> {
    let existing: Option<(i64, i64, String)> = tx
        .query_row(
            "SELECT hlc_wall, hlc_logical, hlc_node FROM core_resource_versions \
             WHERE resource_type = ?1 AND resource_id = ?2",
            rusqlite::params![resource_type, resource_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql_error)?;
    match existing {
        None => Ok(true),
        Some((wall, logical, node)) => {
            let current = HybridLogicalTimestamp {
                wall_time_ms: wall,
                logical: logical as u32,
                node_id: node,
            };
            Ok(*incoming > current)
        }
    }
}

fn upsert_resource_version(
    tx: &rusqlite::Transaction<'_>,
    resource_type: &str,
    resource_id: &str,
    hlc: &HybridLogicalTimestamp,
) -> LedgerResult<()> {
    tx.execute(
        "INSERT INTO core_resource_versions (resource_type, resource_id, hlc_wall, hlc_logical, hlc_node) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(resource_type, resource_id) DO UPDATE SET \
         hlc_wall = excluded.hlc_wall, hlc_logical = excluded.hlc_logical, hlc_node = excluded.hlc_node",
        rusqlite::params![
            resource_type,
            resource_id,
            hlc.wall_time_ms,
            hlc.logical as i64,
            hlc.node_id,
        ],
    )
    .map(|_| ())
    .map_err(sql_error)
}

fn apply_shared_setting_secret(
    tx: &rusqlite::Transaction<'_>,
    settings_cipher: &crate::crypto::SettingsCipher,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let key = field_string(operation, "key")?;
    // Same resource kind carries both allowlisted secrets (re-encrypted per node)
    // and allowlisted non-secret fleet config (plaintext). Anything else is
    // refused — a node never materializes a setting it has not opted into.
    let is_secret = crate::db::repository::is_shared_secret_setting_key(&key);
    let is_shared = crate::db::repository::is_shared_setting_key(&key);
    if !is_secret && !is_shared {
        return Err(SyncLedgerError::Runtime(format!(
            "setting is not syncable: {key}"
        )));
    }
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let value = field_string(operation, "value")?;
            // Secrets re-encrypt with THIS node's cipher; non-secrets stay plain.
            let stored = if is_secret {
                settings_cipher
                    .encrypt(&value)
                    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))?
            } else {
                value
            };
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
                rusqlite::params![key, stored],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "UPDATE settings SET value = '', updated_at = datetime('now') WHERE key = ?1",
                rusqlite::params![key],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated installed-addon-instance op to the local `addons` table.
/// Insert carries the full row (the origin's package is identical here, but
/// carrying the row keeps the receiver from re-deriving anything in this tx).
/// Update covers the enable/disable toggle. The runtime is loaded/unloaded by a
/// post-commit reconcile (see sync runtime), NOT here — no wasmtime in a tx.
fn apply_addon_instance(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let addon_id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO addons \
                 (addon_id, name, display_name, version, package_id, package_version, description, \
                  author, platforms, manifest_json, is_enabled, is_system, skill_md, keywords_json, \
                  category, disambiguation_json, icon, runtime, wasm_size_bytes, license, show_in_catalog) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21) \
                 ON CONFLICT(addon_id) DO UPDATE SET \
                 name=excluded.name, display_name=excluded.display_name, version=excluded.version, \
                 package_id=excluded.package_id, package_version=excluded.package_version, \
                 description=excluded.description, author=excluded.author, platforms=excluded.platforms, \
                 manifest_json=excluded.manifest_json, is_enabled=excluded.is_enabled, \
                 is_system=excluded.is_system, skill_md=excluded.skill_md, keywords_json=excluded.keywords_json, \
                 category=excluded.category, disambiguation_json=excluded.disambiguation_json, \
                 icon=excluded.icon, runtime=excluded.runtime, wasm_size_bytes=excluded.wasm_size_bytes, \
                 license=excluded.license, show_in_catalog=excluded.show_in_catalog, \
                 updated_at=datetime('now')",
                rusqlite::params![
                    addon_id,
                    field_string(operation, "name")?,
                    field_string_or(operation, "display_name", "")?,
                    field_string(operation, "version")?,
                    field_string_or(operation, "package_id", "")?,
                    field_string_or(operation, "package_version", "")?,
                    field_string_or(operation, "description", "")?,
                    field_string_or(operation, "author", "")?,
                    field_string_or(operation, "platforms", "all")?,
                    field_string(operation, "manifest_json")?,
                    field_bool_or(operation, "is_enabled", true)?,
                    field_bool_or(operation, "is_system", false)?,
                    field_optional_string(operation, "skill_md")?,
                    field_string_or(operation, "keywords_json", "[]")?,
                    field_string_or(operation, "category", "")?,
                    field_string_or(operation, "disambiguation_json", "[]")?,
                    field_optional_string(operation, "icon")?,
                    field_string_or(operation, "runtime", "wasmtime")?,
                    field_i64_or(operation, "wasm_size_bytes", 0)?,
                    field_string_or(operation, "license", "")?,
                    field_bool_or(operation, "show_in_catalog", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE addons SET is_enabled = ?2, updated_at = datetime('now') WHERE addon_id = ?1",
                rusqlite::params![addon_id, field_bool_or(operation, "is_enabled", true)?],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => {
            // Purge scoped rows too (no FKs) so a later reinstall of the same
            // addon_id never inherits stale synced config/limits/rules. Mirrors
            // the local uninstall cleanup. Per-instance SQLite file + data dir
            // are removed by the reconcile hook (it has fs access).
            for table in [
                "addon_storage",
                "addon_permissions",
                "addon_secrets",
                "addon_resource_limits",
                "addon_config",
                "addon_network_rules",
                "addon_migrations_applied",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE addon_id = ?1"),
                    rusqlite::params![addon_id],
                )
                .ok();
            }
            tx.execute(
                "DELETE FROM addons WHERE addon_id = ?1",
                rusqlite::params![addon_id],
            )
            .map_err(sql_error)
        }
    }
}

/// Apply a replicated NON-secret addon-config row. Components travel in the
/// fields (`addon_id`, `key`, `value`); the resource id is `addon_id:key` for
/// per-key LWW. Always stored with is_secret=0 (secret rows never sync). No FK on
/// `addon_config`, so a row arriving before its instance is a harmless orphan
/// until the instance lands.
fn apply_addon_config(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let addon_id = field_string(operation, "addon_id")?;
    let key = field_string(operation, "key")?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO addon_config (addon_id, key, value, is_secret, updated_at) \
                 VALUES (?1, ?2, ?3, 0, datetime('now')) \
                 ON CONFLICT(addon_id, key) DO UPDATE SET \
                    value = excluded.value, is_secret = 0, updated_at = datetime('now')",
                rusqlite::params![addon_id, key, field_string(operation, "value")?],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM addon_config WHERE addon_id = ?1 AND key = ?2",
                rusqlite::params![addon_id, key],
            )
            .map_err(sql_error),
    }
}

fn apply_organization(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO organizations \
                 (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now')) \
                 ON CONFLICT(org_id) DO UPDATE SET \
                 name = excluded.name, slug = excluded.slug, contact_email = excluded.contact_email, \
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
    let id = &operation.body.resource_id;
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
    let id = &operation.body.resource_id;
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
    let group_id = field_string(operation, "group_id")?;
    let user_id = field_string(operation, "user_id")?;
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

fn apply_sync_node(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO sync_nodes \
                 (node_id, public_key, public_key_type, display_name, node_kind, trust_status, owner_user_id, sync_profile) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(node_id) DO UPDATE SET \
                 public_key = excluded.public_key, public_key_type = excluded.public_key_type, \
                 display_name = excluded.display_name, node_kind = excluded.node_kind, trust_status = excluded.trust_status, \
                 owner_user_id = excluded.owner_user_id, sync_profile = excluded.sync_profile",
                rusqlite::params![
                    operation.body.resource_id,
                    field_string(operation, "public_key")?,
                    field_string_or(operation, "public_key_type", "ed25519")?,
                    field_string_or(operation, "display_name", "")?,
                    field_string_or(operation, "node_kind", "unknown")?,
                    field_string_or(operation, "trust_status", "untrusted")?,
                    optional_present_string(operation, "owner_user_id")?,
                    field_string_or(operation, "sync_profile", "standard")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE sync_nodes SET \
                 public_key = COALESCE(?2, public_key), public_key_type = COALESCE(?3, public_key_type), \
                 display_name = COALESCE(?4, display_name), node_kind = COALESCE(?5, node_kind), \
                 trust_status = COALESCE(?6, trust_status), \
                 owner_user_id = CASE WHEN ?7 THEN ?8 ELSE owner_user_id END, \
                 sync_profile = COALESCE(?9, sync_profile) \
                 WHERE node_id = ?1",
                rusqlite::params![
                    operation.body.resource_id,
                    optional_present_string(operation, "public_key")?,
                    optional_present_string(operation, "public_key_type")?,
                    optional_present_string(operation, "display_name")?,
                    optional_present_string(operation, "node_kind")?,
                    optional_present_string(operation, "trust_status")?,
                    nullable_update_string(operation, "owner_user_id")?.0,
                    nullable_update_string(operation, "owner_user_id")?.1,
                    optional_present_string(operation, "sync_profile")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM sync_nodes WHERE node_id = ?1",
                rusqlite::params![operation.body.resource_id],
            )
            .map_err(sql_error),
    }
}

fn apply_user_identity_key(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO user_identity_keys (key_id, user_id, key_type, public_key, purpose, status, revoked_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(key_id) DO UPDATE SET \
                 user_id = excluded.user_id, key_type = excluded.key_type, public_key = excluded.public_key, \
                 purpose = excluded.purpose, status = excluded.status, revoked_at = excluded.revoked_at",
                rusqlite::params![
                    operation.body.resource_id,
                    field_string(operation, "user_id")?,
                    field_string(operation, "key_type")?,
                    field_string(operation, "public_key")?,
                    field_string_or(operation, "purpose", "sync")?,
                    field_string_or(operation, "status", "active")?,
                    field_optional_string(operation, "revoked_at")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE user_identity_keys SET \
                 status = COALESCE(?2, status), revoked_at = CASE WHEN ?3 THEN ?4 ELSE revoked_at END \
                 WHERE key_id = ?1",
                rusqlite::params![
                    operation.body.resource_id,
                    optional_present_string(operation, "status")?,
                    nullable_update_string(operation, "revoked_at")?.0,
                    nullable_update_string(operation, "revoked_at")?.1,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM user_identity_keys WHERE key_id = ?1",
                rusqlite::params![operation.body.resource_id],
            )
            .map_err(sql_error),
    }
}

fn apply_node_user_assignment(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let node_id = field_string(operation, "node_id")?;
    let user_id = field_string(operation, "user_id")?;
    let assignment_mode = field_string(operation, "assignment_mode")?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO node_user_assignments (node_id, user_id, assignment_mode, valid_until, created_by) \
                 VALUES (?1, ?2, ?3, NULL, ?4) \
                 ON CONFLICT(node_id, user_id, assignment_mode) DO UPDATE SET \
                 valid_until = NULL, created_by = excluded.created_by",
                rusqlite::params![
                    node_id,
                    user_id,
                    assignment_mode,
                    optional_present_string(operation, "created_by")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE node_user_assignments SET valid_until = CASE WHEN ?4 THEN ?5 ELSE valid_until END \
                 WHERE node_id = ?1 AND user_id = ?2 AND assignment_mode = ?3",
                rusqlite::params![
                    node_id,
                    user_id,
                    assignment_mode,
                    nullable_update_string(operation, "valid_until")?.0,
                    nullable_update_string(operation, "valid_until")?.1,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM node_user_assignments WHERE node_id = ?1 AND user_id = ?2 AND assignment_mode = ?3",
                rusqlite::params![node_id, user_id, assignment_mode],
            )
            .map_err(sql_error),
    }
}

fn apply_sync_user_org_profile(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let org_id = field_string(operation, "org_id")?;
    let user_id = field_string(operation, "user_id")?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO sync_user_org_profiles \
                 (org_id, user_id, department_id, manager_user_id, is_department_manager) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(org_id, user_id) DO UPDATE SET \
                 department_id = excluded.department_id, manager_user_id = excluded.manager_user_id, \
                 is_department_manager = excluded.is_department_manager",
                rusqlite::params![
                    org_id,
                    user_id,
                    field_optional_string(operation, "department_id")?,
                    optional_present_string(operation, "manager_user_id")?,
                    field_bool_or(operation, "is_department_manager", false)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE sync_user_org_profiles SET \
                 department_id = CASE WHEN ?3 THEN ?4 ELSE department_id END, \
                 manager_user_id = CASE WHEN ?5 THEN ?6 ELSE manager_user_id END, \
                 is_department_manager = COALESCE(?7, is_department_manager) \
                 WHERE org_id = ?1 AND user_id = ?2",
                rusqlite::params![
                    org_id,
                    user_id,
                    nullable_update_string(operation, "department_id")?.0,
                    nullable_update_string(operation, "department_id")?.1,
                    nullable_update_string(operation, "manager_user_id")?.0,
                    nullable_update_string(operation, "manager_user_id")?.1,
                    optional_present_bool(operation, "is_department_manager")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM sync_user_org_profiles WHERE org_id = ?1 AND user_id = ?2",
                rusqlite::params![org_id, user_id],
            )
            .map_err(sql_error),
    }
}

fn apply_flow(tx: &rusqlite::Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
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

fn apply_flow_model_binding(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO flow_model_bindings (id, flow_id, model_pattern, priority) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(id) DO UPDATE SET \
                 flow_id = excluded.flow_id, model_pattern = excluded.model_pattern, priority = excluded.priority",
                rusqlite::params![
                    id,
                    field_string(operation, "flow_id")?,
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
                    optional_present_string(operation, "flow_id")?,
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

fn apply_sync_policy(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO sync_policies \
                 (policy_id, org_id, addon_id, resource_type, resource_id, mode, authority_node_id, retention_days, is_enabled) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(org_id, addon_id, resource_type, resource_id) DO UPDATE SET \
                 policy_id = excluded.policy_id, mode = excluded.mode, authority_node_id = excluded.authority_node_id, \
                 retention_days = excluded.retention_days, is_enabled = excluded.is_enabled",
                rusqlite::params![
                    operation.body.resource_id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "addon_id")?,
                    field_string_or(operation, "resource_type", "")?,
                    field_string_or(operation, "resource_id", "")?,
                    field_string(operation, "mode")?,
                    field_optional_string(operation, "authority_node_id")?,
                    optional_present_i64(operation, "retention_days")?,
                    field_bool_or(operation, "is_enabled", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE sync_policies SET \
                 mode = COALESCE(?2, mode), \
                 authority_node_id = CASE WHEN ?3 THEN ?4 ELSE authority_node_id END, \
                 retention_days = CASE WHEN ?5 THEN ?6 ELSE retention_days END, \
                 is_enabled = COALESCE(?7, is_enabled) \
                 WHERE policy_id = ?1",
                rusqlite::params![
                    operation.body.resource_id,
                    optional_present_string(operation, "mode")?,
                    nullable_update_string(operation, "authority_node_id")?.0,
                    nullable_update_string(operation, "authority_node_id")?.1,
                    nullable_update_i64(operation, "retention_days")?.0,
                    nullable_update_i64(operation, "retention_days")?.1,
                    optional_present_bool(operation, "is_enabled")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM sync_policies WHERE policy_id = ?1",
                rusqlite::params![operation.body.resource_id],
            )
            .map_err(sql_error),
    }
}

fn apply_sync_resource_acl(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let org_id = field_string(operation, "org_id")?;
    let addon_id = field_string(operation, "addon_id")?;
    let resource_type = field_string(operation, "resource_type")?;
    let resource_id = field_string(operation, "resource_id")?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO sync_resource_acl \
                 (org_id, addon_id, resource_type, resource_id, owner_user_id, assigned_user_id, department_id, manager_user_id, visibility_scope) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(org_id, addon_id, resource_type, resource_id) DO UPDATE SET \
                 owner_user_id = excluded.owner_user_id, assigned_user_id = excluded.assigned_user_id, \
                 department_id = excluded.department_id, manager_user_id = excluded.manager_user_id, visibility_scope = excluded.visibility_scope",
                rusqlite::params![
                    org_id,
                    addon_id,
                    resource_type,
                    resource_id,
                    optional_present_string(operation, "owner_user_id")?,
                    optional_present_string(operation, "assigned_user_id")?,
                    field_optional_string(operation, "department_id")?,
                    optional_present_string(operation, "manager_user_id")?,
                    field_string_or(operation, "visibility_scope", "assigned")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE sync_resource_acl SET \
                 owner_user_id = CASE WHEN ?5 THEN ?6 ELSE owner_user_id END, \
                 assigned_user_id = CASE WHEN ?7 THEN ?8 ELSE assigned_user_id END, \
                 department_id = CASE WHEN ?9 THEN ?10 ELSE department_id END, \
                 manager_user_id = CASE WHEN ?11 THEN ?12 ELSE manager_user_id END, \
                 visibility_scope = COALESCE(?13, visibility_scope) \
                 WHERE org_id = ?1 AND addon_id = ?2 AND resource_type = ?3 AND resource_id = ?4",
                rusqlite::params![
                    org_id,
                    addon_id,
                    resource_type,
                    resource_id,
                    nullable_update_string(operation, "owner_user_id")?.0,
                    nullable_update_string(operation, "owner_user_id")?.1,
                    nullable_update_string(operation, "assigned_user_id")?.0,
                    nullable_update_string(operation, "assigned_user_id")?.1,
                    nullable_update_string(operation, "department_id")?.0,
                    nullable_update_string(operation, "department_id")?.1,
                    nullable_update_string(operation, "manager_user_id")?.0,
                    nullable_update_string(operation, "manager_user_id")?.1,
                    optional_present_string(operation, "visibility_scope")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM sync_resource_acl \
                 WHERE org_id = ?1 AND addon_id = ?2 AND resource_type = ?3 AND resource_id = ?4",
                rusqlite::params![org_id, addon_id, resource_type, resource_id],
            )
            .map_err(sql_error),
    }
}

fn apply_sync_explicit_share(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let org_id = field_string(operation, "org_id")?;
    let addon_id = field_string(operation, "addon_id")?;
    let resource_type = field_string(operation, "resource_type")?;
    let resource_id = field_string(operation, "resource_id")?;
    let subject_type = field_string(operation, "subject_type")?;
    let subject_id = field_string(operation, "subject_id")?;
    let action = field_string(operation, "action")?;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO sync_explicit_shares \
                 (org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action, granted_by, revoked_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL) \
                 ON CONFLICT(org_id, addon_id, resource_type, resource_id, subject_type, subject_id, action) DO UPDATE SET \
                 granted_by = excluded.granted_by, granted_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), revoked_at = NULL",
                rusqlite::params![
                    org_id,
                    addon_id,
                    resource_type,
                    resource_id,
                    subject_type,
                    subject_id,
                    action,
                    optional_present_string(operation, "granted_by")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => tx
            .execute(
                "UPDATE sync_explicit_shares SET revoked_at = CASE WHEN ?8 THEN ?9 ELSE revoked_at END \
                 WHERE org_id = ?1 AND addon_id = ?2 AND resource_type = ?3 AND resource_id = ?4 \
                 AND subject_type = ?5 AND subject_id = ?6 AND action = ?7",
                rusqlite::params![
                    org_id,
                    addon_id,
                    resource_type,
                    resource_id,
                    subject_type,
                    subject_id,
                    action,
                    nullable_update_string(operation, "revoked_at")?.0,
                    nullable_update_string(operation, "revoked_at")?.1,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation)),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM sync_explicit_shares \
                 WHERE org_id = ?1 AND addon_id = ?2 AND resource_type = ?3 AND resource_id = ?4 \
                 AND subject_type = ?5 AND subject_id = ?6 AND action = ?7",
                rusqlite::params![
                    org_id,
                    addon_id,
                    resource_type,
                    resource_id,
                    subject_type,
                    subject_id,
                    action,
                ],
            )
            .map_err(sql_error),
    }
}

/// UPDATE matched no row: the INSERT that creates this resource has not been
/// materialized yet (causal-ordering gap), not a data conflict. Surface it as a
/// deferred-ordering error so the inbox keeps the entry retryable until the
/// prerequisite INSERT lands via push or repair.
fn require_existing(operation: &SyncOperation) -> impl FnOnce(usize) -> LedgerResult<usize> + '_ {
    |rows| {
        if rows == 0 {
            Err(SyncLedgerError::DeferredOrdering(format!(
                "core sync target row not found: {}/{}",
                operation.body.resource_type, operation.body.resource_id
            )))
        } else {
            Ok(rows)
        }
    }
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

fn nullable_update_i64(operation: &SyncOperation, key: &str) -> LedgerResult<(bool, Option<i64>)> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::I64(value)) => Ok((true, Some(*value))),
        Some(FieldValue::U64(value)) => i64::try_from(*value)
            .map(|value| (true, Some(value)))
            .map_err(|e| SyncLedgerError::Runtime(format!("invalid i64 field {key}: {e}"))),
        Some(FieldValue::Null) => Ok((true, None)),
        None => Ok((false, None)),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid nullable i64 type: {key}"
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
