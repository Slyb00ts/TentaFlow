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
            | CoreSyncResourceKind::Skill
            | CoreSyncResourceKind::SkillFile
            | CoreSyncResourceKind::Agent
            | CoreSyncResourceKind::UserAccount
            | CoreSyncResourceKind::Organization
            | CoreSyncResourceKind::Role
            | CoreSyncResourceKind::UserGroup
            | CoreSyncResourceKind::SyncPolicy
            | CoreSyncResourceKind::SyncResourceAcl
            | CoreSyncResourceKind::SyncUserOrgProfile
            | CoreSyncResourceKind::SharedSettingSecret
            | CoreSyncResourceKind::AddonInstance
            | CoreSyncResourceKind::AddonConfig
            | CoreSyncResourceKind::ApiKey
            | CoreSyncResourceKind::ResourcePermission
            | CoreSyncResourceKind::PiiRule
            | CoreSyncResourceKind::ComplianceDataCategory
            | CoreSyncResourceKind::ComplianceProcessingActivity
            | CoreSyncResourceKind::ComplianceLegalBasis
            | CoreSyncResourceKind::ComplianceRetentionPolicy
            | CoreSyncResourceKind::ComplianceProcessor
            | CoreSyncResourceKind::TokenUsageDaily
            | CoreSyncResourceKind::TokenQuota
            | CoreSyncResourceKind::TokenLease
            | CoreSyncResourceKind::ModelMetricsRollup
            | CoreSyncResourceKind::ModelPricing
            | CoreSyncResourceKind::CameraCvPipeline
            | CoreSyncResourceKind::VisionModel
            // Code Studio: every one of these is revocable, and a revocation must
            // not be resurrected by a stale grant that took the long way round.
            // LWW makes the Delete tombstone win over the older Insert, the same
            // reason `resource_permissions` is tracked.
            | CoreSyncResourceKind::CodeWorkspace
            | CoreSyncResourceKind::CodeWorkspaceMember
            | CoreSyncResourceKind::CodeWorkspaceCreatorGrant
            | CoreSyncResourceKind::CodeWorkspaceProjectLink
            | CoreSyncResourceKind::CodeWorkspaceAllowlist
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
        .write()
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
        CoreSyncResourceKind::Skill => apply_skill(&tx, operation)?,
        CoreSyncResourceKind::SkillFile => apply_skill_file(&tx, operation)?,
        CoreSyncResourceKind::Agent => apply_agent(&tx, operation)?,
        CoreSyncResourceKind::SyncPolicy => apply_sync_policy(&tx, operation)?,
        CoreSyncResourceKind::SyncResourceAcl => apply_sync_resource_acl(&tx, operation)?,
        CoreSyncResourceKind::SyncExplicitShare => apply_sync_explicit_share(&tx, operation)?,
        CoreSyncResourceKind::SharedSettingSecret => {
            apply_shared_setting_secret(&tx, settings_cipher, operation)?
        }
        CoreSyncResourceKind::AddonInstance => apply_addon_instance(&tx, operation)?,
        CoreSyncResourceKind::AddonConfig => apply_addon_config(&tx, operation)?,
        CoreSyncResourceKind::ApiKey => apply_api_key(&tx, operation)?,
        CoreSyncResourceKind::ResourcePermission => apply_resource_permission(&tx, operation)?,
        CoreSyncResourceKind::FlowVersion => apply_flow_version(&tx, operation)?,
        CoreSyncResourceKind::PiiRule => apply_pii_rule(&tx, operation)?,
        CoreSyncResourceKind::ComplianceDataCategory => {
            apply_compliance_data_category(&tx, operation)?
        }
        CoreSyncResourceKind::ComplianceProcessingActivity => {
            apply_compliance_processing_activity(&tx, operation)?
        }
        CoreSyncResourceKind::ComplianceLegalBasis => apply_compliance_legal_basis(&tx, operation)?,
        CoreSyncResourceKind::ComplianceRetentionPolicy => {
            apply_compliance_retention_policy(&tx, operation)?
        }
        CoreSyncResourceKind::ComplianceProcessor => apply_compliance_processor(&tx, operation)?,
        CoreSyncResourceKind::TokenUsageDaily => apply_token_usage_daily(&tx, operation)?,
        CoreSyncResourceKind::TokenQuota => apply_token_quota(&tx, operation)?,
        CoreSyncResourceKind::TokenLease => apply_token_lease(&tx, operation)?,
        CoreSyncResourceKind::ModelMetricsRollup => apply_model_metrics_rollup(&tx, operation)?,
        CoreSyncResourceKind::ModelPricing => apply_model_pricing(&tx, operation)?,
        CoreSyncResourceKind::CameraCvPipeline => apply_camera_cv_pipeline(&tx, operation)?,
        CoreSyncResourceKind::VisionModel => apply_vision_model(&tx, operation)?,
        CoreSyncResourceKind::CodeWorkspace => apply_code_workspace(&tx, operation)?,
        CoreSyncResourceKind::CodeWorkspaceMember => apply_code_workspace_member(&tx, operation)?,
        CoreSyncResourceKind::CodeWorkspaceCreatorGrant => {
            apply_code_workspace_creator_grant(&tx, operation)?
        }
        CoreSyncResourceKind::CodeWorkspaceProjectLink => {
            apply_code_workspace_project_link(&tx, operation)?
        }
        CoreSyncResourceKind::CodeWorkspaceAllowlist => {
            apply_code_workspace_allowlist(&tx, operation)?
        }
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

    // A synced flow edit/delete/status change must drop the FlowDispatcher's
    // compiled-flow cache (same guarantee the local save handlers give), else a
    // remote change is masked by a cached compile until the TTL. Post-commit +
    // best-effort: `None` before the dispatcher exists just means nothing is
    // cached. `FlowModelBinding` affects model→flow resolution, so it counts too.
    if rows > 0
        && matches!(
            descriptor.kind,
            CoreSyncResourceKind::Flow | CoreSyncResourceKind::FlowModelBinding
        )
    {
        if let Some(d) = crate::flow_engine::dispatcher::global_flow_dispatcher() {
            d.invalidate_cache();
        }
    }
    Ok(rows)
}

/// Returns true when `incoming` strictly exceeds the HLC currently recorded for
/// `(resource_type, resource_id)` in `core_resource_versions`. A missing row
/// (never-seen resource) always wins. Comparison uses the total HLC order from
/// phase A (wall, logical, node_id tie-break).
///
/// Shared with the replicated `addon.kv` materializer (`runtime::apply_kv_operation`):
/// that path is the same concurrent-edit class as LWW-tracked core resources, so it
/// reuses this exact comparison instead of re-deriving HLC order.
pub(crate) fn incoming_hlc_wins(
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

pub(crate) fn upsert_resource_version(
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
            // The addon's materialized skill goes too (mirrors the local
            // uninstall cleanup). The origin also emits a core.skill Delete op,
            // which then applies as a harmless no-op here. FK cascade drops the
            // skill's reference files.
            tx.execute(
                "DELETE FROM skills WHERE source = 'addon' AND source_ref = ?1",
                rusqlite::params![addon_id],
            )
            .ok();
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

/// Apply a replicated external-app API key. Insert/Update are full-row upserts
/// keyed on `uid`; the replicated set carries the verifier (NEVER the raw key),
/// prefix, name, type, subject, rate limit and active flag. `last_used_at` is
/// node-local and intentionally absent from the synced fields, so the UPSERT must
/// preserve the existing local value rather than reset it to NULL (mirrors the
/// skills use_count/last_used_at preservation).
fn apply_api_key(tx: &rusqlite::Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let uid = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO api_keys \
                 (uid, key_verifier, key_prefix, name, key_type, subject_id, rate_limit_rps, is_active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(uid) DO UPDATE SET \
                 key_verifier = excluded.key_verifier, key_prefix = excluded.key_prefix, \
                 name = excluded.name, key_type = excluded.key_type, \
                 subject_id = excluded.subject_id, rate_limit_rps = excluded.rate_limit_rps, \
                 is_active = excluded.is_active",
                rusqlite::params![
                    uid,
                    field_string(operation, "key_verifier")?,
                    field_string(operation, "key_prefix")?,
                    field_string(operation, "name")?,
                    field_string(operation, "key_type")?,
                    field_optional_string(operation, "subject_id")?,
                    field_i64_or(operation, "rate_limit_rps", 0)?,
                    field_bool_or(operation, "is_active", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM api_keys WHERE uid = ?1",
                rusqlite::params![uid],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated resource ACL rule. The four key components travel in the
/// fields (`resource_type`, `resource_id`, `subject_type`, `subject_id`); the
/// resource_id is the length-prefixed composite for per-rule LWW. A `clear` on
/// the origin replicates as Delete (tombstone): the LWW gate in
/// `apply_core_operation` records the clear's HLC in `core_resource_versions`, so
/// a later-arriving but OLDER `allow` for the same rule loses the comparison and
/// is dropped — the cleared rule is never resurrected.
fn apply_resource_permission(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let resource_type = field_string(operation, "resource_type")?;
    let resource_id = field_string(operation, "resource_id")?;
    let subject_type = field_string(operation, "subject_type")?;
    let subject_id = field_string(operation, "subject_id")?;
    // Bind the LWW slot to the rule we actually write: the gate in
    // `apply_core_operation` keys on `operation.body.resource_id`, so a stale op
    // whose composite id points at rule C but whose fields encode rule B would
    // pass C's freshness check and then resurrect B. Reject any op whose fields
    // do not recompute to the composite id it claims.
    let expected_id = crate::sync::resource_id::composite_resource_id(&[
        &resource_type,
        &resource_id,
        &subject_type,
        &subject_id,
    ]);
    if expected_id != operation.body.resource_id {
        return Err(SyncLedgerError::Runtime(format!(
            "resource_permission composite id mismatch: body={}, fields={}",
            operation.body.resource_id, expected_id
        )));
    }
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO resource_permissions \
                 (resource_type, resource_id, subject_type, subject_id, access_level) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(resource_type, resource_id, subject_type, subject_id) \
                 DO UPDATE SET access_level = excluded.access_level",
                rusqlite::params![
                    resource_type,
                    resource_id,
                    subject_type,
                    subject_id,
                    field_string(operation, "access_level")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM resource_permissions \
                 WHERE resource_type = ?1 AND resource_id = ?2 \
                   AND subject_type = ?3 AND subject_id = ?4",
                rusqlite::params![resource_type, resource_id, subject_type, subject_id],
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

    // Seed-guard for system flows (ps-chat & co.). System flows are seeded
    // per-node with a fixed id and refreshed by the LOCAL seed, so the mesh is
    // never their source of truth: EVERY remote write (Insert/Update/Delete)
    // reaching a row locally marked is_system=1 is rejected — a synced write
    // would desync one node's copy of a platform resource and break the module
    // that depends on it (project chat dispatches ps-chat by this fixed id).
    // The wire is_system flag is never trusted either: no node legitimately
    // produces an is_system=true op (flow captures always send false, the seed
    // writes raw SQL without capture), so Insert/Update below coerce it to 0.
    let local_is_system: Option<bool> = tx
        .query_row(
            "SELECT is_system FROM flows WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok(row.get::<_, i64>(0)? != 0),
        )
        .optional()
        .map_err(sql_error)?;
    if local_is_system == Some(true) {
        tracing::warn!(
            flow_id = %id,
            action = ?operation.body.action,
            "sync: rejected remote write to a local system flow (seed-owned)"
        );
        return Ok(0);
    }

    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO flows \
                 (id, name, description, is_default, service_type, flow_json, status, published_model_name, is_system) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, description = excluded.description, is_default = excluded.is_default, \
                 service_type = excluded.service_type, flow_json = excluded.flow_json, \
                 status = excluded.status, published_model_name = excluded.published_model_name, \
                 is_system = excluded.is_system, \
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
                    false,
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
                 is_system = COALESCE(?12, is_system), \
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
                    false,
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

/// Remote skill fields are untrusted peer input: an out-of-set `status`
/// degrades to 'active' instead of tripping the SQL CHECK (which would turn
/// the inbox entry into a terminal conflict with a raw SQL message).
fn skill_status_or_active(status: String) -> String {
    match status.as_str() {
        "active" | "disabled" | "quarantine" | "archived" => status,
        _ => "active".to_string(),
    }
}

/// A `source` outside the CHECK set cannot be guessed — reject the operation
/// with our own message so the conflict reason is readable, not raw SQL.
fn check_skill_source(source: &str) -> LedgerResult<()> {
    if matches!(source, "user" | "addon" | "hub") {
        Ok(())
    } else {
        Err(SyncLedgerError::Runtime(format!(
            "replicated skill has invalid source: '{source}'"
        )))
    }
}

/// Mirrors the size cap every local write path enforces
/// (`repository::validate_skill_params`) so sync cannot smuggle oversized rows.
fn check_skill_content(content: &str) -> LedgerResult<()> {
    if content.chars().count() > crate::db::repository::SKILL_CONTENT_MAX_CHARS {
        Err(SyncLedgerError::Runtime(format!(
            "replicated skill content exceeds {} chars",
            crate::db::repository::SKILL_CONTENT_MAX_CHARS
        )))
    } else {
        Ok(())
    }
}

/// Apply a replicated skill row. Inserts are full-row upserts (the origin emits
/// every upsert as Insert with the complete synced field set). `use_count` /
/// `last_used_at` / `created_by` / `created_at` are node-local and never touched
/// here, so a synced edit cannot reset local usage stats. Source/status/content
/// are validated against the local write rules before touching the table.
fn apply_skill(tx: &rusqlite::Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert => {
            let source = field_string(operation, "source")?;
            check_skill_source(&source)?;
            let content = field_string(operation, "content")?;
            check_skill_content(&content)?;
            let status = skill_status_or_active(field_string_or(operation, "status", "active")?);
            tx.execute(
                "INSERT INTO skills \
                 (id, name, display_name, description, content, tags_json, category, source, source_ref, status, created_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, display_name = excluded.display_name, \
                 description = excluded.description, content = excluded.content, \
                 tags_json = excluded.tags_json, category = excluded.category, \
                 source = excluded.source, source_ref = excluded.source_ref, \
                 status = excluded.status, updated_at = datetime('now')",
                rusqlite::params![
                    id,
                    field_string(operation, "name")?,
                    field_optional_string(operation, "display_name")?,
                    field_string(operation, "description")?,
                    content,
                    field_string_or(operation, "tags_json", "[]")?,
                    field_optional_string(operation, "category")?,
                    source,
                    field_optional_string(operation, "source_ref")?,
                    status,
                    field_optional_string(operation, "created_by")?,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Update => {
            let content = optional_present_string(operation, "content")?;
            if let Some(content) = content.as_deref() {
                check_skill_content(content)?;
            }
            let status = optional_present_string(operation, "status")?.map(skill_status_or_active);
            let display_name = nullable_update_string(operation, "display_name")?;
            let category = nullable_update_string(operation, "category")?;
            tx.execute(
                "UPDATE skills SET \
                 name = COALESCE(?2, name), \
                 display_name = CASE WHEN ?3 THEN ?4 ELSE display_name END, \
                 description = COALESCE(?5, description), content = COALESCE(?6, content), \
                 tags_json = COALESCE(?7, tags_json), \
                 category = CASE WHEN ?8 THEN ?9 ELSE category END, \
                 status = COALESCE(?10, status), updated_at = datetime('now') \
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    optional_present_string(operation, "name")?,
                    display_name.0,
                    display_name.1,
                    optional_present_string(operation, "description")?,
                    content,
                    optional_present_string(operation, "tags_json")?,
                    category.0,
                    category.1,
                    status,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation))
        }
        ActionType::Delete => tx
            .execute("DELETE FROM skills WHERE id = ?1", rusqlite::params![id])
            .map_err(sql_error),
    }
}

/// Apply a replicated skill reference file. Components travel in the fields
/// (`skill_id`, `path`, `content`); the resource id is the composite
/// (skill_id, path) for per-file LWW. `skill_files` carries an FK to `skills`,
/// so a file landing before its skill is a causal-ordering gap — surfaced as
/// DeferredOrdering to keep the inbox entry retryable until the skill arrives.
fn apply_skill_file(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let skill_id = field_string(operation, "skill_id")?;
    let path = field_string(operation, "path")?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let skill_exists: bool = tx
                .query_row(
                    "SELECT 1 FROM skills WHERE id = ?1",
                    rusqlite::params![skill_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(sql_error)?
                .unwrap_or(false);
            if !skill_exists {
                return Err(SyncLedgerError::DeferredOrdering(format!(
                    "skill_files target skill not found: {skill_id}"
                )));
            }
            tx.execute(
                "INSERT INTO skill_files (skill_id, path, content) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(skill_id, path) DO UPDATE SET content = excluded.content",
                rusqlite::params![skill_id, path, field_string(operation, "content")?],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM skill_files WHERE skill_id = ?1 AND path = ?2",
                rusqlite::params![skill_id, path],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated agent row (Harness §3.3). Like skills, every local upsert
/// is emitted as a full-row Insert, so the Insert arm is the primary path; the
/// Update arm covers partial captures for completeness. `agent_runs` are runtime
/// state and never reach this materializer.
fn apply_agent(tx: &rusqlite::Transaction<'_>, operation: &SyncOperation) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO agents \
                 (id, name, display_name, description, system_prompt, model, tools_json, \
                  skills_json, params_json, max_iterations, timeout_secs, max_subagents, \
                  max_spawn_depth, flow_id, routable, is_enabled, on_child_complete) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
                 ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, display_name = excluded.display_name, \
                 description = excluded.description, system_prompt = excluded.system_prompt, \
                 model = excluded.model, tools_json = excluded.tools_json, \
                 skills_json = excluded.skills_json, params_json = excluded.params_json, \
                 max_iterations = excluded.max_iterations, timeout_secs = excluded.timeout_secs, \
                 max_subagents = excluded.max_subagents, max_spawn_depth = excluded.max_spawn_depth, \
                 flow_id = excluded.flow_id, routable = excluded.routable, \
                 is_enabled = excluded.is_enabled, on_child_complete = excluded.on_child_complete, \
                 updated_at = datetime('now')",
                rusqlite::params![
                    id,
                    field_string(operation, "name")?,
                    field_optional_string(operation, "display_name")?,
                    field_string(operation, "description")?,
                    field_optional_string(operation, "system_prompt")?,
                    field_optional_string(operation, "model")?,
                    field_string_or(operation, "tools_json", "[]")?,
                    field_string_or(operation, "skills_json", "{}")?,
                    field_string_or(operation, "params_json", "{}")?,
                    field_i64_or(operation, "max_iterations", 25)?,
                    field_i64_or(operation, "timeout_secs", 600)?,
                    field_i64_or(operation, "max_subagents", 0)?,
                    field_i64_or(operation, "max_spawn_depth", 1)?,
                    field_optional_string(operation, "flow_id")?,
                    field_bool_or(operation, "routable", true)?,
                    field_bool_or(operation, "is_enabled", true)?,
                    field_string_or(operation, "on_child_complete", "notify")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Update => {
            let display_name = nullable_update_string(operation, "display_name")?;
            let system_prompt = nullable_update_string(operation, "system_prompt")?;
            let model = nullable_update_string(operation, "model")?;
            let flow_id = nullable_update_string(operation, "flow_id")?;
            tx.execute(
                "UPDATE agents SET \
                 name = COALESCE(?2, name), \
                 display_name = CASE WHEN ?3 THEN ?4 ELSE display_name END, \
                 description = COALESCE(?5, description), \
                 system_prompt = CASE WHEN ?6 THEN ?7 ELSE system_prompt END, \
                 model = CASE WHEN ?8 THEN ?9 ELSE model END, \
                 tools_json = COALESCE(?10, tools_json), \
                 skills_json = COALESCE(?11, skills_json), \
                 params_json = COALESCE(?12, params_json), \
                 max_iterations = COALESCE(?13, max_iterations), \
                 timeout_secs = COALESCE(?14, timeout_secs), \
                 max_subagents = COALESCE(?15, max_subagents), \
                 max_spawn_depth = COALESCE(?16, max_spawn_depth), \
                 flow_id = CASE WHEN ?17 THEN ?18 ELSE flow_id END, \
                 routable = COALESCE(?19, routable), \
                 is_enabled = COALESCE(?20, is_enabled), \
                 on_child_complete = COALESCE(?21, on_child_complete), \
                 updated_at = datetime('now') \
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    optional_present_string(operation, "name")?,
                    display_name.0,
                    display_name.1,
                    optional_present_string(operation, "description")?,
                    system_prompt.0,
                    system_prompt.1,
                    model.0,
                    model.1,
                    optional_present_string(operation, "tools_json")?,
                    optional_present_string(operation, "skills_json")?,
                    optional_present_string(operation, "params_json")?,
                    optional_present_i64(operation, "max_iterations")?,
                    optional_present_i64(operation, "timeout_secs")?,
                    optional_present_i64(operation, "max_subagents")?,
                    optional_present_i64(operation, "max_spawn_depth")?,
                    flow_id.0,
                    flow_id.1,
                    optional_present_bool(operation, "routable")?,
                    optional_present_bool(operation, "is_enabled")?,
                    optional_present_string(operation, "on_child_complete")?,
                ],
            )
            .map_err(sql_error)
            .and_then(require_existing(operation))
        }
        ActionType::Delete => tx
            .execute("DELETE FROM agents WHERE id = ?1", rusqlite::params![id])
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

/// Apply a replicated flow_versions snapshot. The history is append-only, so
/// Insert and Update both upsert the full row; the id is the resource id. The
/// FK to `flows(id)` means a snapshot landing before its parent flow is a
/// causal-ordering gap — surfaced as DeferredOrdering so the inbox retries it
/// until the flow arrives (mirrors `apply_skill_file`).
fn apply_flow_version(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let flow_id = field_string(operation, "flow_id")?;
            let flow_exists: bool = tx
                .query_row(
                    "SELECT 1 FROM flows WHERE id = ?1",
                    rusqlite::params![flow_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(sql_error)?
                .unwrap_or(false);
            if !flow_exists {
                return Err(SyncLedgerError::DeferredOrdering(format!(
                    "flow_versions target flow not found: {flow_id}"
                )));
            }
            tx.execute(
                "INSERT INTO flow_versions \
                 (id, flow_id, version_num, name, description, status, created_by, flow_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(id) DO UPDATE SET \
                 flow_id = excluded.flow_id, version_num = excluded.version_num, \
                 name = excluded.name, description = excluded.description, \
                 status = excluded.status, created_by = excluded.created_by, \
                 flow_json = excluded.flow_json",
                rusqlite::params![
                    id,
                    flow_id,
                    field_i64_or(operation, "version_num", 0)?,
                    field_string(operation, "name")?,
                    field_optional_string(operation, "description")?,
                    field_string(operation, "status")?,
                    field_optional_string(operation, "created_by")?,
                    field_string(operation, "flow_json")?,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM flow_versions WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated PII redaction rule. Insert/Update are full-row upserts
/// keyed on the UUID `id`; `org_id` travels in the fields to satisfy the NOT
/// NULL column. `created_at` is node-local and preserved on conflict.
fn apply_pii_rule(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO pii_rules \
                 (id, org_id, name, category, pattern, replacement, is_active, priority, description, test_examples) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, category = excluded.category, pattern = excluded.pattern, \
                 replacement = excluded.replacement, is_active = excluded.is_active, \
                 priority = excluded.priority, description = excluded.description, \
                 test_examples = excluded.test_examples",
                rusqlite::params![
                    id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "name")?,
                    field_string(operation, "category")?,
                    field_string(operation, "pattern")?,
                    field_string_or(operation, "replacement", "[UKRYTY]")?,
                    field_bool_or(operation, "is_active", true)?,
                    field_i64_or(operation, "priority", 0)?,
                    field_optional_string(operation, "description")?,
                    field_optional_string(operation, "test_examples")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM pii_rules WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated compliance data-category row. `org_id` travels in the
/// fields; the receiver upserts the full synced config column set (runtime
/// timestamps stay node-local).
fn apply_compliance_data_category(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let category_id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO compliance_data_categories \
                 (category_id, org_id, slug, name_translations, description_translations, \
                  personal_data, sensitive_data, risk_class, source_scope, addon_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(category_id) DO UPDATE SET \
                 slug = excluded.slug, name_translations = excluded.name_translations, \
                 description_translations = excluded.description_translations, \
                 personal_data = excluded.personal_data, sensitive_data = excluded.sensitive_data, \
                 risk_class = excluded.risk_class, source_scope = excluded.source_scope, \
                 addon_id = excluded.addon_id",
                rusqlite::params![
                    category_id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "slug")?,
                    field_string_or(operation, "name_translations", "{}")?,
                    field_string_or(operation, "description_translations", "{}")?,
                    field_bool_or(operation, "personal_data", true)?,
                    field_bool_or(operation, "sensitive_data", false)?,
                    field_string_or(operation, "risk_class", "standard")?,
                    field_string_or(operation, "source_scope", "core")?,
                    field_optional_string(operation, "addon_id")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM compliance_data_categories WHERE category_id = ?1",
                rusqlite::params![category_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated compliance processing-activity row.
fn apply_compliance_processing_activity(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let activity_id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO compliance_processing_activities \
                 (activity_id, org_id, slug, name_translations, purpose_translations, \
                  controller_role, owner_user_id, system_scope, addon_id, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(activity_id) DO UPDATE SET \
                 slug = excluded.slug, name_translations = excluded.name_translations, \
                 purpose_translations = excluded.purpose_translations, \
                 controller_role = excluded.controller_role, owner_user_id = excluded.owner_user_id, \
                 system_scope = excluded.system_scope, addon_id = excluded.addon_id, \
                 status = excluded.status",
                rusqlite::params![
                    activity_id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "slug")?,
                    field_string_or(operation, "name_translations", "{}")?,
                    field_string_or(operation, "purpose_translations", "{}")?,
                    field_string_or(operation, "controller_role", "controller")?,
                    field_optional_string(operation, "owner_user_id")?,
                    field_string_or(operation, "system_scope", "core")?,
                    field_optional_string(operation, "addon_id")?,
                    field_string_or(operation, "status", "active")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM compliance_processing_activities WHERE activity_id = ?1",
                rusqlite::params![activity_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated compliance legal-basis row. The FK to activity/category
/// is satisfied by their own replicated rows; both are nullable here.
fn apply_compliance_legal_basis(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let legal_basis_id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO compliance_legal_basis \
                 (legal_basis_id, org_id, activity_id, category_id, basis_kind, basis_reference, \
                  description_translations, is_active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(legal_basis_id) DO UPDATE SET \
                 activity_id = excluded.activity_id, category_id = excluded.category_id, \
                 basis_kind = excluded.basis_kind, basis_reference = excluded.basis_reference, \
                 description_translations = excluded.description_translations, \
                 is_active = excluded.is_active",
                rusqlite::params![
                    legal_basis_id,
                    field_string(operation, "org_id")?,
                    field_optional_string(operation, "activity_id")?,
                    field_optional_string(operation, "category_id")?,
                    field_string(operation, "basis_kind")?,
                    field_string_or(operation, "basis_reference", "")?,
                    field_string_or(operation, "description_translations", "{}")?,
                    field_bool_or(operation, "is_active", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM compliance_legal_basis WHERE legal_basis_id = ?1",
                rusqlite::params![legal_basis_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated compliance retention-policy row.
fn apply_compliance_retention_policy(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let retention_policy_id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO compliance_retention_policies \
                 (retention_policy_id, org_id, slug, name_translations, scope_kind, category_id, \
                  retention_days, minimum_days, action_after_retention, is_default, is_active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT(retention_policy_id) DO UPDATE SET \
                 slug = excluded.slug, name_translations = excluded.name_translations, \
                 scope_kind = excluded.scope_kind, category_id = excluded.category_id, \
                 retention_days = excluded.retention_days, minimum_days = excluded.minimum_days, \
                 action_after_retention = excluded.action_after_retention, \
                 is_default = excluded.is_default, is_active = excluded.is_active",
                rusqlite::params![
                    retention_policy_id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "slug")?,
                    field_string_or(operation, "name_translations", "{}")?,
                    field_string(operation, "scope_kind")?,
                    field_optional_string(operation, "category_id")?,
                    field_i64_or(operation, "retention_days", 0)?,
                    field_i64_or(operation, "minimum_days", 0)?,
                    field_string_or(operation, "action_after_retention", "delete")?,
                    field_bool_or(operation, "is_default", false)?,
                    field_bool_or(operation, "is_active", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM compliance_retention_policies WHERE retention_policy_id = ?1",
                rusqlite::params![retention_policy_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated compliance processor row.
fn apply_compliance_processor(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let processor_id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO compliance_processors \
                 (processor_id, org_id, name, role, country, transfer_mechanism, dpa_reference, is_active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(processor_id) DO UPDATE SET \
                 name = excluded.name, role = excluded.role, country = excluded.country, \
                 transfer_mechanism = excluded.transfer_mechanism, dpa_reference = excluded.dpa_reference, \
                 is_active = excluded.is_active",
                rusqlite::params![
                    processor_id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "name")?,
                    field_string(operation, "role")?,
                    field_string_or(operation, "country", "")?,
                    field_string_or(operation, "transfer_mechanism", "")?,
                    field_string_or(operation, "dpa_reference", "")?,
                    field_bool_or(operation, "is_active", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM compliance_processors WHERE processor_id = ?1",
                rusqlite::params![processor_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated `token_usage_daily` row. Counters are single-writer (the
/// owning node), so the synced cumulative value is authoritative — we replace the
/// whole counter set. `updated_at` is a node-local watermark and is NOT synced,
/// so it is omitted: an INSERT keeps the column DEFAULT and an UPDATE leaves the
/// receiver's own watermark untouched.
fn apply_token_usage_daily(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO token_usage_daily \
                 (id, node_id, org_id, user_id, model_id, usage_day, \
                  prompt_tokens, completion_tokens, total_tokens, request_count, \
                  audio_ms, images, embedding_tokens) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                 ON CONFLICT(id) DO UPDATE SET \
                 node_id = excluded.node_id, org_id = excluded.org_id, \
                 user_id = excluded.user_id, model_id = excluded.model_id, \
                 usage_day = excluded.usage_day, prompt_tokens = excluded.prompt_tokens, \
                 completion_tokens = excluded.completion_tokens, \
                 total_tokens = excluded.total_tokens, request_count = excluded.request_count, \
                 audio_ms = excluded.audio_ms, images = excluded.images, \
                 embedding_tokens = excluded.embedding_tokens",
                rusqlite::params![
                    id,
                    field_string(operation, "node_id")?,
                    field_string(operation, "org_id")?,
                    field_string(operation, "user_id")?,
                    field_string(operation, "model_id")?,
                    field_string(operation, "usage_day")?,
                    field_i64_or(operation, "prompt_tokens", 0)?,
                    field_i64_or(operation, "completion_tokens", 0)?,
                    field_i64_or(operation, "total_tokens", 0)?,
                    field_i64_or(operation, "request_count", 0)?,
                    field_i64_or(operation, "audio_ms", 0)?,
                    field_i64_or(operation, "images", 0)?,
                    field_i64_or(operation, "embedding_tokens", 0)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM token_usage_daily WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated `token_quota` row. `created_at` is node-local and preserved
/// on UPSERT (omitted from both the INSERT column list and the conflict update).
fn apply_token_quota(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO token_quota \
                 (id, org_id, scope_type, subject_id, model_id, period, max_total_tokens, is_active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(id) DO UPDATE SET \
                 org_id = excluded.org_id, scope_type = excluded.scope_type, \
                 subject_id = excluded.subject_id, model_id = excluded.model_id, \
                 period = excluded.period, max_total_tokens = excluded.max_total_tokens, \
                 is_active = excluded.is_active",
                rusqlite::params![
                    id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "scope_type")?,
                    field_optional_string(operation, "subject_id")?,
                    field_optional_string(operation, "model_id")?,
                    field_string(operation, "period")?,
                    field_i64_or(operation, "max_total_tokens", 0)?,
                    field_bool_or(operation, "is_active", true)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM token_quota WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated `token_lease` row (coordinator-written). `created_at` is
/// node-local and preserved on UPSERT.
fn apply_token_lease(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO token_lease \
                 (id, org_id, quota_id, node_id, period_key, base_used, granted_tokens, \
                  coordinator_node_id, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(id) DO UPDATE SET \
                 org_id = excluded.org_id, quota_id = excluded.quota_id, \
                 node_id = excluded.node_id, period_key = excluded.period_key, \
                 base_used = excluded.base_used, granted_tokens = excluded.granted_tokens, \
                 coordinator_node_id = excluded.coordinator_node_id, \
                 expires_at = excluded.expires_at",
                rusqlite::params![
                    id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "quota_id")?,
                    field_string(operation, "node_id")?,
                    field_string(operation, "period_key")?,
                    field_i64_or(operation, "base_used", 0)?,
                    field_i64_or(operation, "granted_tokens", 0)?,
                    field_string(operation, "coordinator_node_id")?,
                    field_string(operation, "expires_at")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM token_lease WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated `model_metrics_rollup` row. Counters/histograms are
/// single-writer (the owning node), so the synced cumulative value is
/// authoritative — we replace the whole set. `updated_at` is a node-local
/// watermark and is NOT synced, so it is omitted (INSERT keeps DEFAULT, UPDATE
/// leaves the receiver's own watermark untouched). Mirrors `apply_token_usage_daily`.
fn apply_model_metrics_rollup(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO model_metrics_rollup \
                 (id, node_id, org_id, user_id, model_id, service_key, backend, modality, \
                  hour_bucket, histogram_version, request_count, success_count, error_count, \
                  prompt_tokens, completion_tokens, total_tokens, embedding_tokens, audio_ms, images, \
                  prefill_secs_sum, decode_secs_sum, e2e_latency_ms_sum, queue_ms_sum, \
                  ttft_b0, ttft_b1, ttft_b2, ttft_b3, ttft_b4, ttft_b5, ttft_b6, ttft_b7, ttft_b8, ttft_b9, \
                  ttft_sample_count, \
                  decode_tps_b0, decode_tps_b1, decode_tps_b2, decode_tps_b3, decode_tps_b4, \
                  decode_tps_b5, decode_tps_b6, decode_tps_b7, decode_tps_sample_count, \
                  e2e_b0, e2e_b1, e2e_b2, e2e_b3, e2e_b4, e2e_b5, e2e_b6, e2e_b7, e2e_b8, e2e_b9, \
                  e2e_sample_count, usage_missing_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
                  ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, \
                  ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, ?50, ?51, \
                  ?52, ?53, ?54, ?55) \
                 ON CONFLICT(id) DO UPDATE SET \
                 node_id = excluded.node_id, org_id = excluded.org_id, user_id = excluded.user_id, \
                 model_id = excluded.model_id, service_key = excluded.service_key, \
                 backend = excluded.backend, modality = excluded.modality, \
                 hour_bucket = excluded.hour_bucket, histogram_version = excluded.histogram_version, \
                 request_count = excluded.request_count, success_count = excluded.success_count, \
                 error_count = excluded.error_count, prompt_tokens = excluded.prompt_tokens, \
                 completion_tokens = excluded.completion_tokens, total_tokens = excluded.total_tokens, \
                 embedding_tokens = excluded.embedding_tokens, audio_ms = excluded.audio_ms, \
                 images = excluded.images, prefill_secs_sum = excluded.prefill_secs_sum, \
                 decode_secs_sum = excluded.decode_secs_sum, \
                 e2e_latency_ms_sum = excluded.e2e_latency_ms_sum, queue_ms_sum = excluded.queue_ms_sum, \
                 ttft_b0 = excluded.ttft_b0, ttft_b1 = excluded.ttft_b1, ttft_b2 = excluded.ttft_b2, \
                 ttft_b3 = excluded.ttft_b3, ttft_b4 = excluded.ttft_b4, ttft_b5 = excluded.ttft_b5, \
                 ttft_b6 = excluded.ttft_b6, ttft_b7 = excluded.ttft_b7, ttft_b8 = excluded.ttft_b8, \
                 ttft_b9 = excluded.ttft_b9, ttft_sample_count = excluded.ttft_sample_count, \
                 decode_tps_b0 = excluded.decode_tps_b0, decode_tps_b1 = excluded.decode_tps_b1, \
                 decode_tps_b2 = excluded.decode_tps_b2, decode_tps_b3 = excluded.decode_tps_b3, \
                 decode_tps_b4 = excluded.decode_tps_b4, decode_tps_b5 = excluded.decode_tps_b5, \
                 decode_tps_b6 = excluded.decode_tps_b6, decode_tps_b7 = excluded.decode_tps_b7, \
                 decode_tps_sample_count = excluded.decode_tps_sample_count, \
                 e2e_b0 = excluded.e2e_b0, e2e_b1 = excluded.e2e_b1, e2e_b2 = excluded.e2e_b2, \
                 e2e_b3 = excluded.e2e_b3, e2e_b4 = excluded.e2e_b4, e2e_b5 = excluded.e2e_b5, \
                 e2e_b6 = excluded.e2e_b6, e2e_b7 = excluded.e2e_b7, e2e_b8 = excluded.e2e_b8, \
                 e2e_b9 = excluded.e2e_b9, e2e_sample_count = excluded.e2e_sample_count, \
                 usage_missing_count = excluded.usage_missing_count",
                rusqlite::params![
                    id,
                    field_string(operation, "node_id")?,
                    field_string(operation, "org_id")?,
                    field_string(operation, "user_id")?,
                    field_string(operation, "model_id")?,
                    field_string(operation, "service_key")?,
                    field_string(operation, "backend")?,
                    field_string(operation, "modality")?,
                    field_string(operation, "hour_bucket")?,
                    field_i64_or(operation, "histogram_version", 1)?,
                    field_i64_or(operation, "request_count", 0)?,
                    field_i64_or(operation, "success_count", 0)?,
                    field_i64_or(operation, "error_count", 0)?,
                    field_i64_or(operation, "prompt_tokens", 0)?,
                    field_i64_or(operation, "completion_tokens", 0)?,
                    field_i64_or(operation, "total_tokens", 0)?,
                    field_i64_or(operation, "embedding_tokens", 0)?,
                    field_i64_or(operation, "audio_ms", 0)?,
                    field_i64_or(operation, "images", 0)?,
                    field_f64_or(operation, "prefill_secs_sum", 0.0)?,
                    field_f64_or(operation, "decode_secs_sum", 0.0)?,
                    field_i64_or(operation, "e2e_latency_ms_sum", 0)?,
                    field_i64_or(operation, "queue_ms_sum", 0)?,
                    field_i64_or(operation, "ttft_b0", 0)?,
                    field_i64_or(operation, "ttft_b1", 0)?,
                    field_i64_or(operation, "ttft_b2", 0)?,
                    field_i64_or(operation, "ttft_b3", 0)?,
                    field_i64_or(operation, "ttft_b4", 0)?,
                    field_i64_or(operation, "ttft_b5", 0)?,
                    field_i64_or(operation, "ttft_b6", 0)?,
                    field_i64_or(operation, "ttft_b7", 0)?,
                    field_i64_or(operation, "ttft_b8", 0)?,
                    field_i64_or(operation, "ttft_b9", 0)?,
                    field_i64_or(operation, "ttft_sample_count", 0)?,
                    field_i64_or(operation, "decode_tps_b0", 0)?,
                    field_i64_or(operation, "decode_tps_b1", 0)?,
                    field_i64_or(operation, "decode_tps_b2", 0)?,
                    field_i64_or(operation, "decode_tps_b3", 0)?,
                    field_i64_or(operation, "decode_tps_b4", 0)?,
                    field_i64_or(operation, "decode_tps_b5", 0)?,
                    field_i64_or(operation, "decode_tps_b6", 0)?,
                    field_i64_or(operation, "decode_tps_b7", 0)?,
                    field_i64_or(operation, "decode_tps_sample_count", 0)?,
                    field_i64_or(operation, "e2e_b0", 0)?,
                    field_i64_or(operation, "e2e_b1", 0)?,
                    field_i64_or(operation, "e2e_b2", 0)?,
                    field_i64_or(operation, "e2e_b3", 0)?,
                    field_i64_or(operation, "e2e_b4", 0)?,
                    field_i64_or(operation, "e2e_b5", 0)?,
                    field_i64_or(operation, "e2e_b6", 0)?,
                    field_i64_or(operation, "e2e_b7", 0)?,
                    field_i64_or(operation, "e2e_b8", 0)?,
                    field_i64_or(operation, "e2e_b9", 0)?,
                    field_i64_or(operation, "e2e_sample_count", 0)?,
                    field_i64_or(operation, "usage_missing_count", 0)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM model_metrics_rollup WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated `model_pricing` row. `updated_at` is node-local and
/// preserved on UPSERT (omitted from both the INSERT column list and the conflict
/// update). Mirrors `apply_token_quota`.
fn apply_model_pricing(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO model_pricing \
                 (id, org_id, model_id, prompt_per_1k, completion_per_1k, audio_per_min, image_each, \
                  embedding_per_1k) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(id) DO UPDATE SET \
                 org_id = excluded.org_id, model_id = excluded.model_id, \
                 prompt_per_1k = excluded.prompt_per_1k, \
                 completion_per_1k = excluded.completion_per_1k, \
                 audio_per_min = excluded.audio_per_min, image_each = excluded.image_each, \
                 embedding_per_1k = excluded.embedding_per_1k",
                rusqlite::params![
                    id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "model_id")?,
                    field_f64_or(operation, "prompt_per_1k", 0.0)?,
                    field_f64_or(operation, "completion_per_1k", 0.0)?,
                    field_f64_or(operation, "audio_per_min", 0.0)?,
                    field_f64_or(operation, "image_each", 0.0)?,
                    field_f64_or(operation, "embedding_per_1k", 0.0)?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM model_pricing WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Captures carry the full row (name, pipeline_json, is_default), so Insert and
/// Update are the same upsert. Timestamps are node-local (unix seconds), like
/// the `flows` materializer's `datetime('now')`.
fn apply_camera_cv_pipeline(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let pipeline_json = field_string(operation, "pipeline_json")?;
            // A malformed replicated pipeline must not land in the table the
            // engine reads from — skip the write (the sender's local validator
            // should have rejected it; a bad payload here means a buggy or
            // hostile peer). Alias existence is deliberately NOT checked:
            // `model_aliases` rows may replicate after the pipeline does.
            // The schema lives in `camera_ingest`, so a build without camera ingest
            // cannot validate it — and does not need to: nothing here runs the
            // pipeline, the row is inert. It is still written so the ledger
            // converges, and every node that does run cameras validates its own copy.
            #[cfg(feature = "camera")]
            {
                let structurally_valid = serde_json::from_str::<
                    crate::services::camera_ingest::cv_pipeline::CvPipeline,
                >(&pipeline_json)
                .map_err(|e| e.to_string())
                .and_then(|p| {
                    crate::services::camera_ingest::cv_pipeline::validate(&p)
                        .map_err(|e| e.to_string())
                });
                if let Err(err) = structurally_valid {
                    tracing::warn!(
                        "core sync: skipping invalid camera cv pipeline '{}' from node '{}': {}",
                        id,
                        operation.body.actor_node_id,
                        err
                    );
                    return Ok(0);
                }
            }
            // The default flag is seed-owned: only the fixed seed row may carry
            // is_default=1 (the partial unique index enforces at most one).
            // Any other replicated row is forced to 0 so two nodes can never
            // converge into a constraint violation.
            let is_default = id == crate::db::seed::CAMERA_CV_PIPELINE_ID
                && field_bool_or(operation, "is_default", false)?;
            // Pre-org-scope senders omit org_id — default them into the
            // single default org, matching the v110 column default.
            let org_id =
                field_string_or(operation, "org_id", crate::services::org::DEFAULT_ORG_ID)?;
            tx.execute(
                "INSERT INTO camera_cv_pipelines \
                 (id, name, pipeline_json, is_default, org_id, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, CAST(strftime('%s','now') AS INTEGER), \
                 CAST(strftime('%s','now') AS INTEGER)) \
                 ON CONFLICT(id) DO UPDATE SET \
                 name = excluded.name, pipeline_json = excluded.pipeline_json, \
                 is_default = excluded.is_default, org_id = excluded.org_id, \
                 updated_at = CAST(strftime('%s','now') AS INTEGER)",
                rusqlite::params![
                    id,
                    field_string(operation, "name")?,
                    pipeline_json,
                    is_default,
                    org_id,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => {
            // A replicated delete must win mesh-wide, but local cameras may
            // still point at the row — clear those references explicitly
            // (they fall back to the default pipeline) instead of leaving
            // dangling ids behind.
            let referencing: Vec<String> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT camera_id FROM cameras \
                         WHERE cv_pipeline_id = ?1 AND removed_at IS NULL",
                    )
                    .map_err(sql_error)?;
                let rows = stmt
                    .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map_err(sql_error)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(sql_error)?;
                rows
            };
            if !referencing.is_empty() {
                tracing::warn!(
                    "core sync: pipeline '{}' deleted by node '{}' while assigned to cameras [{}] — clearing to default",
                    id,
                    operation.body.actor_node_id,
                    referencing.join(", ")
                );
                tx.execute(
                    "UPDATE cameras SET cv_pipeline_id = NULL WHERE cv_pipeline_id = ?1",
                    rusqlite::params![id],
                )
                .map_err(sql_error)?;
            }
            tx.execute(
                "DELETE FROM camera_cv_pipelines WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error)
        }
    }
}

/// Apply a replicated `vision_models` row. Only the row METADATA replicates —
/// the ONNX file stays on the node that published it, and the local `onnx-cv`
/// service reconciler skips rows whose file is absent, so a metadata-only node
/// never advertises a model it cannot serve. Structural validation reuses the
/// exact checks local registration enforces (bad op/contract, path-traversing
/// file_name, malformed JSON) so a hostile peer cannot land a poisoned row.
fn apply_vision_model(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let model_name = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let op = field_string(operation, "op")?;
            let file_name = field_string(operation, "file_name")?;
            let sha256 = field_string(operation, "sha256")?;
            let classes_json = field_string_or(operation, "classes_json", "[]")?;
            let preprocess_json = field_string_or(operation, "preprocess_json", "{}")?;
            let output_contract = field_string(operation, "output_contract")?;
            let source = field_string(operation, "source")?;
            let default_threshold = match operation.body.changed_fields.get("default_threshold") {
                Some(FieldValue::Decimal(v)) => v.parse::<f64>().ok(),
                Some(FieldValue::I64(v)) => Some(*v as f64),
                Some(FieldValue::U64(v)) => Some(*v as f64),
                _ => None,
            };
            let org_id =
                field_string_or(operation, "org_id", crate::services::org::DEFAULT_ORG_ID)?;
            let project_id = field_optional_string(operation, "project_id")?;
            let source_model_id = field_optional_string(operation, "source_model_id")?;
            if let Err(err) = crate::db::repository::validate_vision_model_fields(
                model_name,
                &op,
                &file_name,
                &sha256,
                &classes_json,
                &preprocess_json,
                &output_contract,
                &source,
            ) {
                tracing::warn!(
                    "core sync: skipping invalid vision model '{}' from node '{}': {}",
                    model_name,
                    operation.body.actor_node_id,
                    err
                );
                return Ok(0);
            }
            // Org-guarded upsert (mirror of the local `register_vision_model`):
            // `model_name` is the PK, so a replicated row from ANOTHER org must
            // not overwrite an existing row — 0 changed rows means a cross-org
            // name collision, which we log and skip instead of hijacking.
            let changed = tx
                .execute(
                    "INSERT INTO vision_models \
                     (model_name, op, file_name, sha256, classes_json, preprocess_json, \
                      output_contract, source, default_threshold, org_id, project_id, \
                      source_model_id, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, \
                     CAST(strftime('%s','now') AS INTEGER), CAST(strftime('%s','now') AS INTEGER)) \
                     ON CONFLICT(model_name) DO UPDATE SET \
                     op = excluded.op, file_name = excluded.file_name, sha256 = excluded.sha256, \
                     classes_json = excluded.classes_json, \
                     preprocess_json = excluded.preprocess_json, \
                     output_contract = excluded.output_contract, source = excluded.source, \
                     default_threshold = excluded.default_threshold, \
                     project_id = excluded.project_id, \
                     source_model_id = excluded.source_model_id, \
                     updated_at = CAST(strftime('%s','now') AS INTEGER) \
                     WHERE vision_models.org_id = excluded.org_id",
                    rusqlite::params![
                        model_name,
                        op,
                        file_name,
                        sha256,
                        classes_json,
                        preprocess_json,
                        output_contract,
                        source,
                        default_threshold,
                        org_id,
                        project_id,
                        source_model_id,
                    ],
                )
                .map_err(sql_error)?;
            if changed == 0 {
                tracing::warn!(
                    "core sync: vision model '{}' from node '{}' collides with another org's \
                     row — skipped",
                    model_name,
                    operation.body.actor_node_id
                );
            }
            Ok(changed)
        }
        ActionType::Delete => {
            let org_id =
                field_string_or(operation, "org_id", crate::services::org::DEFAULT_ORG_ID)?;
            tx.execute(
                "DELETE FROM vision_models WHERE model_name = ?1 AND org_id = ?2",
                rusqlite::params![model_name, org_id],
            )
            .map_err(sql_error)
        }
    }
}

// =============================================================================
// Code Studio registry (plan §5.1)
// =============================================================================

/// Every satellite table carries an FK to `code_workspaces`. A satellite row
/// landing before its workspace is a causal-ordering gap, not a conflict, so it
/// stays retryable until the workspace op arrives.
fn require_code_workspace(tx: &rusqlite::Transaction<'_>, workspace_id: &str) -> LedgerResult<()> {
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM code_workspaces WHERE id = ?1",
            rusqlite::params![workspace_id],
            |_| Ok(true),
        )
        .optional()
        .map_err(sql_error)?
        .unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(SyncLedgerError::DeferredOrdering(format!(
            "code studio target workspace not found: {workspace_id}"
        )))
    }
}

/// Apply a replicated workspace row. Every local mutation is captured as a
/// full-row Insert, so the Insert arm is the primary path and a replicated edit
/// never depends on UPDATE-after-INSERT ordering.
///
/// `secret_ref` is a HANDLE into the node-local vault, never material (§5.2): on
/// a node that does not hold the secret it resolves to nothing, which is exactly
/// the `secret_missing` answer the plan asks for instead of a silent failure.
fn apply_code_workspace(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let id = &operation.body.resource_id;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO code_workspaces \
                 (id, org_id, owner_user_id, name, slug, node_id, exec_mode, container_image, \
                  egress_enforcement, repo_kind, repo_url, repo_auth_kind, secret_ref, \
                  ssh_host_fingerprint, default_branch, target_branch, autonomy_ceiling, \
                  egress_policy, index_enabled, quota_disk_bytes, quota_sessions, status, \
                  status_detail, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, datetime('now')) \
                 ON CONFLICT(id) DO UPDATE SET \
                    org_id = excluded.org_id, owner_user_id = excluded.owner_user_id, \
                    name = excluded.name, slug = excluded.slug, node_id = excluded.node_id, \
                    exec_mode = excluded.exec_mode, container_image = excluded.container_image, \
                    egress_enforcement = excluded.egress_enforcement, \
                    repo_kind = excluded.repo_kind, repo_url = excluded.repo_url, \
                    repo_auth_kind = excluded.repo_auth_kind, secret_ref = excluded.secret_ref, \
                    ssh_host_fingerprint = excluded.ssh_host_fingerprint, \
                    default_branch = excluded.default_branch, \
                    target_branch = excluded.target_branch, \
                    autonomy_ceiling = excluded.autonomy_ceiling, \
                    egress_policy = excluded.egress_policy, \
                    index_enabled = excluded.index_enabled, \
                    quota_disk_bytes = excluded.quota_disk_bytes, \
                    quota_sessions = excluded.quota_sessions, status = excluded.status, \
                    status_detail = excluded.status_detail, updated_at = datetime('now')",
                rusqlite::params![
                    id,
                    field_string(operation, "org_id")?,
                    field_string(operation, "owner_user_id")?,
                    field_string(operation, "name")?,
                    field_string(operation, "slug")?,
                    field_string(operation, "node_id")?,
                    field_string(operation, "exec_mode")?,
                    field_optional_string(operation, "container_image")?,
                    field_string(operation, "egress_enforcement")?,
                    field_string(operation, "repo_kind")?,
                    field_optional_string(operation, "repo_url")?,
                    field_optional_string(operation, "repo_auth_kind")?,
                    field_optional_string(operation, "secret_ref")?,
                    field_optional_string(operation, "ssh_host_fingerprint")?,
                    field_optional_string(operation, "default_branch")?,
                    field_optional_string(operation, "target_branch")?,
                    field_string(operation, "autonomy_ceiling")?,
                    field_string(operation, "egress_policy")?,
                    field_bool_or(operation, "index_enabled", false)?,
                    optional_present_i64(operation, "quota_disk_bytes")?,
                    optional_present_i64(operation, "quota_sessions")?,
                    field_string(operation, "status")?,
                    field_optional_string(operation, "status_detail")?,
                    field_string(operation, "created_at")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM code_workspaces WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated workspace membership. Identity is (workspace_id, user_id);
/// `added_by` travels because the project mirror recognises its own grants by it
/// (`ps:<project_id>`) and must not revoke a hand-made membership.
fn apply_code_workspace_member(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let workspace_id = field_string(operation, "workspace_id")?;
    let user_id = field_string(operation, "user_id")?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            require_code_workspace(tx, &workspace_id)?;
            tx.execute(
                "INSERT INTO code_workspace_members \
                 (workspace_id, user_id, role, added_by, added_at) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(workspace_id, user_id) DO UPDATE SET \
                    role = excluded.role, added_by = excluded.added_by",
                rusqlite::params![
                    workspace_id,
                    user_id,
                    field_string(operation, "role")?,
                    field_string(operation, "added_by")?,
                    field_string(operation, "added_at")?,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM code_workspace_members WHERE workspace_id = ?1 AND user_id = ?2",
                rusqlite::params![workspace_id, user_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated creator grant. It carries no FK, so it can land before any
/// workspace exists — which is the point: the right to CREATE one must reach a
/// node that holds nothing yet.
fn apply_code_workspace_creator_grant(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let org_id = field_string(operation, "org_id")?;
    let user_id = field_string(operation, "user_id")?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO code_workspace_creator_grants \
                 (org_id, user_id, granted_by, created_at) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(org_id, user_id) DO UPDATE SET granted_by = excluded.granted_by",
                rusqlite::params![
                    org_id,
                    user_id,
                    field_string(operation, "granted_by")?,
                    field_string(operation, "created_at")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM code_workspace_creator_grants WHERE org_id = ?1 AND user_id = ?2",
                rusqlite::params![org_id, user_id],
            )
            .map_err(sql_error),
    }
}

fn apply_code_workspace_project_link(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let workspace_id = field_string(operation, "workspace_id")?;
    let project_id = field_string(operation, "project_id")?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            require_code_workspace(tx, &workspace_id)?;
            tx.execute(
                "INSERT INTO code_workspace_project_links \
                 (workspace_id, project_id, linked_by, created_at) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(workspace_id, project_id) DO UPDATE SET \
                    linked_by = excluded.linked_by",
                rusqlite::params![
                    workspace_id,
                    project_id,
                    field_string(operation, "linked_by")?,
                    field_string(operation, "created_at")?,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM code_workspace_project_links \
                 WHERE workspace_id = ?1 AND project_id = ?2",
                rusqlite::params![workspace_id, project_id],
            )
            .map_err(sql_error),
    }
}

/// Apply a replicated standing capability grant. The local `id` is left to the
/// receiver's AUTOINCREMENT and never travels — identity is the UNIQUE triple
/// (workspace_id, capability, pattern), so the same grant converges to one row
/// on every node instead of colliding with a foreign rowid.
fn apply_code_workspace_allowlist(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let workspace_id = field_string(operation, "workspace_id")?;
    let capability = field_string(operation, "capability")?;
    let pattern = field_string(operation, "pattern")?;
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            require_code_workspace(tx, &workspace_id)?;
            tx.execute(
                "INSERT INTO code_workspace_allowlist \
                 (workspace_id, capability, pattern, created_by, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(workspace_id, capability, pattern) DO UPDATE SET \
                    created_by = excluded.created_by",
                rusqlite::params![
                    workspace_id,
                    capability,
                    pattern,
                    field_string(operation, "created_by")?,
                    field_string(operation, "created_at")?,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM code_workspace_allowlist \
                 WHERE workspace_id = ?1 AND capability = ?2 AND pattern = ?3",
                rusqlite::params![workspace_id, capability, pattern],
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

/// Decode a real (f64) field. Reals ride the wire as `FieldValue::Decimal` (an
/// exact decimal string), so parse it back; integer-typed inputs are accepted as
/// a convenience. A missing/null field yields `default`.
fn field_f64_or(operation: &SyncOperation, key: &str, default: f64) -> LedgerResult<f64> {
    match operation.body.changed_fields.get(key) {
        Some(FieldValue::Decimal(value)) => value
            .parse::<f64>()
            .map_err(|e| SyncLedgerError::Runtime(format!("invalid f64 field {key}: {e}"))),
        Some(FieldValue::I64(value)) => Ok(*value as f64),
        Some(FieldValue::U64(value)) => Ok(*value as f64),
        Some(FieldValue::Null) | None => Ok(default),
        _ => Err(SyncLedgerError::Runtime(format!(
            "core operation field has invalid f64 type: {key}"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{
        ModelMetricsCounters, ModelMetricsDims, ModelMetricsPerfSamples, ModelMetricsTimes,
        ModelMetricsTokens,
    };
    use crate::db::repository;
    use crate::services::org::DEFAULT_ORG_ID;
    use crate::sync::ledger::{BaselineEpoch, OperationId, PartitionId, SyncOperationBody};
    use std::collections::BTreeMap;

    fn rollup_operation(resource_id: &str, fields: BTreeMap<String, FieldValue>) -> SyncOperation {
        SyncOperation {
            op_id: OperationId::from_hash([7; 32]),
            operation_hash: [7; 32],
            body: SyncOperationBody {
                org_id: DEFAULT_ORG_ID.to_string(),
                partition_id: PartitionId::new("core/org/default/model_metrics").unwrap(),
                node_seq: 1,
                addon_id: CORE_SYNC_ADDON_ID.to_string(),
                resource_type: "core.model_metrics_rollup".to_string(),
                resource_id: resource_id.to_string(),
                table_name: "model_metrics_rollup".to_string(),
                primary_key: "id".to_string(),
                action: ActionType::Insert,
                changed_fields: fields,
                before_hash: None,
                after_hash: None,
                actor_user_id: String::new(),
                actor_device_id: "peer".to_string(),
                actor_node_id: "peer".to_string(),
                hlc_timestamp: HybridLogicalTimestamp {
                    wall_time_ms: 1,
                    logical: 0,
                    node_id: "peer".to_string(),
                },
                epoch: BaselineEpoch::default(),
                prev_node_hash: None,
                payload_hash: [0; 32],
                acl_snapshot_hash: [0; 32],
                policy_epoch: 0,
                encryption_info: None,
            },
            signature: Vec::new(),
        }
    }

    /// The rollup INSERT lists 55 columns by hand; a miscounted placeholder or
    /// param breaks EVERY replicated rollup at `prepare`. Round-trip a locally
    /// bumped row through `model_metrics_changed_fields` → `apply` on a second
    /// DB and compare column by column.
    #[test]
    fn model_metrics_rollup_round_trips_through_materializer() {
        let source = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        repository::bump_model_metrics_rollup(
            &source,
            &ModelMetricsDims {
                node_id: "peer",
                org_id: DEFAULT_ORG_ID,
                user_id: "u1",
                model_id: "qwen",
                service_key: "vllm/qwen",
                backend: "vllm",
                modality: "chat",
                hour_bucket: "2026-08-20T10:00:00Z",
                histogram_version: 1,
            },
            &ModelMetricsCounters {
                request_count: 3,
                success_count: 2,
                error_count: 1,
                usage_missing_count: 2,
            },
            &ModelMetricsTokens {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                embedding_tokens: 7,
                audio_ms: 0,
                images: 0,
            },
            &ModelMetricsTimes {
                e2e_latency_ms: 900,
                ..Default::default()
            },
            &ModelMetricsPerfSamples {
                ttft_ms: Some(120),
                decode_tps: Some(50.0),
                e2e_ms: Some(900),
            },
        )
        .unwrap();
        let original =
            repository::list_model_metrics_rollup(&source, DEFAULT_ORG_ID, &Default::default())
                .unwrap()
                .remove(0);

        let target = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        {
            let mut conn = repository::acquire_for_baseline(&target).unwrap();
            let tx = conn.transaction().unwrap();
            let operation = rollup_operation(
                &original.id,
                repository::model_metrics_changed_fields(&original),
            );
            assert_eq!(apply_model_metrics_rollup(&tx, &operation).unwrap(), 1);
            // Replaying the same operation must overwrite, not fail or double.
            assert_eq!(apply_model_metrics_rollup(&tx, &operation).unwrap(), 1);
            tx.commit().unwrap();
        }
        let rows =
            repository::list_model_metrics_rollup(&target, DEFAULT_ORG_ID, &Default::default())
                .unwrap();
        assert_eq!(rows.len(), 1);
        let got = &rows[0];
        assert_eq!(got.id, original.id);
        assert_eq!(got.node_id, "peer");
        assert_eq!(got.service_key, "vllm/qwen");
        assert_eq!(got.hour_bucket, "2026-08-20T10:00:00Z");
        assert_eq!(got.request_count, 3);
        assert_eq!(got.success_count, 2);
        assert_eq!(got.error_count, 1);
        assert_eq!(got.usage_missing_count, 2);
        assert_eq!(got.prompt_tokens, 100);
        assert_eq!(got.completion_tokens, 50);
        assert_eq!(got.total_tokens, 150);
        assert_eq!(got.embedding_tokens, 7);
        assert_eq!(got.e2e_latency_ms_sum, 900);
        assert_eq!(got.ttft_buckets, original.ttft_buckets);
        assert_eq!(got.ttft_sample_count, 1);
        assert_eq!(got.decode_tps_buckets, original.decode_tps_buckets);
        assert_eq!(got.decode_tps_sample_count, 1);
        assert_eq!(got.e2e_buckets, original.e2e_buckets);
        assert_eq!(got.e2e_sample_count, 1);
    }
}
