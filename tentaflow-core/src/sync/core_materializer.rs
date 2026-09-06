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
            // `sync_nodes` is deliberately NOT here. Whole-row last-writer-wins
            // would let a node freeze its entire registry row — including the
            // operator flag it may not write — simply by describing itself with
            // a clock from the future, and an administrator's later decision
            // would then be dropped silently as "stale". `apply_sync_node`
            // versions the one revocation-bearing field on its own instead; see
            // `OPERATOR_VERSION_RESOURCE_TYPE`.
            | CoreSyncResourceKind::UserGroup
            | CoreSyncResourceKind::SyncPolicy
            | CoreSyncResourceKind::SyncResourceAcl
            | CoreSyncResourceKind::SyncUserOrgProfile
            | CoreSyncResourceKind::SharedSettingSecret
            | CoreSyncResourceKind::AddonInstance
            | CoreSyncResourceKind::AddonConfig
            // The app-permission matrix is revocation-bearing: an admin's deny
            // (or a row removal) must never be resurrected by a stale allow
            // that took the long way round — same reason as resource_permissions.
            | CoreSyncResourceKind::AddonPermission
            | CoreSyncResourceKind::AddonPermissionDefault
            | CoreSyncResourceKind::AddonVisibility
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
            // TentaVM (plan §6.1): "LWW stays as the ORDER, not as the
            // authorization". Ownership decides who may write a registry row;
            // this decides which of the owner's own writes — and which restated
            // row after a baseline reset — is the later one. Without it a
            // re-seeded copy of an old host row could overwrite a newer one that
            // took a shorter path through the mesh.
            | CoreSyncResourceKind::VmHost
            | CoreSyncResourceKind::VmConnector
            | CoreSyncResourceKind::VmConnectorSecretGrant
            | CoreSyncResourceKind::VmHostGpu
            | CoreSyncResourceKind::VmStoragePool
            | CoreSyncResourceKind::VmNetwork
            | CoreSyncResourceKind::VmImage
            | CoreSyncResourceKind::VmImageLocation
            | CoreSyncResourceKind::VmHostGrant
            | CoreSyncResourceKind::VmInstanceSetting
            | CoreSyncResourceKind::VmGuest
            | CoreSyncResourceKind::VmGuestMember
            | CoreSyncResourceKind::VmGuestDisk
            | CoreSyncResourceKind::VmGuestNic
            | CoreSyncResourceKind::VmGuestDevice
            | CoreSyncResourceKind::VmSnapshot
            | CoreSyncResourceKind::VmJob
            | CoreSyncResourceKind::VmTag
            | CoreSyncResourceKind::VmAccessRequest
            | CoreSyncResourceKind::VmAccessDecision
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

    // Only an operation whose author had a title earns a place in the LWW
    // order. Arms with no ownership rule of their own are authorized by the
    // descriptor alone, so they keep the old behaviour.
    let mut authorized = true;
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
        CoreSyncResourceKind::AddonPermission => apply_addon_permission(&tx, operation)?,
        CoreSyncResourceKind::AddonPermissionDefault => {
            apply_addon_permission_default(&tx, operation)?
        }
        CoreSyncResourceKind::AddonVisibility => apply_addon_visibility(&tx, operation)?,
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
        // Eighteen tables, one arm: what differs between them is who owns a row,
        // and that is data (`tentavm_registry::OwnerRule`), not eighteen copies
        // of the same SQL. See `sync/tentavm_registry.rs`.
        //
        // This is the one arm that reports whether the author had a TITLE, not
        // just how many rows moved — see the `authorized` flag below.
        CoreSyncResourceKind::VmHost
        | CoreSyncResourceKind::VmConnector
        | CoreSyncResourceKind::VmConnectorSecretGrant
        | CoreSyncResourceKind::VmHostGpu
        | CoreSyncResourceKind::VmStoragePool
        | CoreSyncResourceKind::VmNetwork
        | CoreSyncResourceKind::VmImage
        | CoreSyncResourceKind::VmImageLocation
        | CoreSyncResourceKind::VmHostGrant
        | CoreSyncResourceKind::VmInstanceSetting
        | CoreSyncResourceKind::VmGuest
        | CoreSyncResourceKind::VmGuestMember
        | CoreSyncResourceKind::VmGuestDisk
        | CoreSyncResourceKind::VmGuestNic
        | CoreSyncResourceKind::VmGuestDevice
        | CoreSyncResourceKind::VmSnapshot
        | CoreSyncResourceKind::VmJob
        | CoreSyncResourceKind::VmTag
        | CoreSyncResourceKind::VmAccessRequest
        | CoreSyncResourceKind::VmAccessDecision => {
            let applied = crate::sync::tentavm_registry::apply(&tx, descriptor, operation)?;
            authorized = applied.authorized();
            applied.rows()
        }
    };

    if lww_tracked && authorized {
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

/// Stamps a resource's position in the LWW order.
///
/// The slot is meant to move FORWARD ONLY: it is what `is_newer_than_stored`
/// compares an incoming operation against, so a stamp that moves it backwards
/// makes an already-rejected operation acceptable again. This function does not
/// enforce that today — every caller happens to stamp a timestamp it just
/// minted — and making it enforce the rule touches ~50 resource kinds at once,
/// which is why it is a step of its own rather than a line here. Until then:
/// stamp with the clock you just took, never with one you received.
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
                  category, disambiguation_json, icon, runtime, wasm_size_bytes, license, show_in_catalog, \
                  admin_only) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22) \
                 ON CONFLICT(addon_id) DO UPDATE SET \
                 name=excluded.name, display_name=excluded.display_name, version=excluded.version, \
                 package_id=excluded.package_id, package_version=excluded.package_version, \
                 description=excluded.description, author=excluded.author, platforms=excluded.platforms, \
                 manifest_json=excluded.manifest_json, is_enabled=excluded.is_enabled, \
                 is_system=excluded.is_system, skill_md=excluded.skill_md, keywords_json=excluded.keywords_json, \
                 category=excluded.category, disambiguation_json=excluded.disambiguation_json, \
                 icon=excluded.icon, runtime=excluded.runtime, wasm_size_bytes=excluded.wasm_size_bytes, \
                 license=excluded.license, show_in_catalog=excluded.show_in_catalog, \
                 admin_only=excluded.admin_only, \
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
                    field_bool_or(operation, "admin_only", false)?,
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
                "addon_permission_defaults",
                "addon_visibility",
                "addon_permission_catalog",
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

fn apply_addon_permission(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let addon_id = field_string(operation, "addon_id")?;
    let subject_type = field_string(operation, "subject_type")?;
    let subject_id = field_string(operation, "subject_id")?;
    let permission_id = field_string(operation, "permission_id")?;
    // Same composite-id binding as resource_permissions: an op whose fields
    // encode a different rule than its LWW slot claims must not apply.
    let expected_id = crate::sync::resource_id::composite_resource_id(&[
        &addon_id,
        &subject_type,
        &subject_id,
        &permission_id,
    ]);
    if expected_id != operation.body.resource_id {
        return Err(SyncLedgerError::Runtime(format!(
            "addon_permission composite id mismatch: body={}, fields={}",
            operation.body.resource_id, expected_id
        )));
    }
    match operation.body.action {
        ActionType::Insert | ActionType::Update => {
            let grant_mode = field_string(operation, "grant_mode")?;
            // `updated_by` stays NULL on receivers: the FK to user_accounts has
            // no cross-partition ordering guarantee, and last-edit attribution
            // is origin-local UX, not authorization state.
            tx.execute(
                "INSERT INTO addon_permissions \
                 (addon_id, subject_type, subject_id, permission_id, granted, grant_mode, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now')) \
                 ON CONFLICT(addon_id, subject_type, subject_id, permission_id) DO UPDATE SET \
                   granted = excluded.granted, \
                   grant_mode = excluded.grant_mode, \
                   updated_at = datetime('now')",
                rusqlite::params![
                    addon_id,
                    subject_type,
                    subject_id,
                    permission_id,
                    (grant_mode == "allow") as i64,
                    grant_mode,
                ],
            )
            .map_err(sql_error)
        }
        ActionType::Delete => tx
            .execute(
                "DELETE FROM addon_permissions \
                 WHERE addon_id = ?1 AND subject_type = ?2 AND subject_id = ?3 AND permission_id = ?4",
                rusqlite::params![addon_id, subject_type, subject_id, permission_id],
            )
            .map_err(sql_error),
    }
}

fn apply_addon_permission_default(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let addon_id = field_string(operation, "addon_id")?;
    let permission_id = field_string(operation, "permission_id")?;
    let expected_id =
        crate::sync::resource_id::composite_resource_id(&[&addon_id, &permission_id]);
    if expected_id != operation.body.resource_id {
        return Err(SyncLedgerError::Runtime(format!(
            "addon_permission_default composite id mismatch: body={}, fields={}",
            operation.body.resource_id, expected_id
        )));
    }
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                "INSERT INTO addon_permission_defaults (addon_id, permission_id, grant_mode) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(addon_id, permission_id) DO UPDATE SET \
                   grant_mode = excluded.grant_mode, \
                   updated_at = datetime('now')",
                rusqlite::params![
                    addon_id,
                    permission_id,
                    field_string(operation, "grant_mode")?,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM addon_permission_defaults \
                 WHERE addon_id = ?1 AND permission_id = ?2",
                rusqlite::params![addon_id, permission_id],
            )
            .map_err(sql_error),
    }
}

fn apply_addon_visibility(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let addon_id = field_string(operation, "addon_id")?;
    let group_id = field_string(operation, "group_id")?;
    let expected_id = crate::sync::resource_id::composite_resource_id(&[&addon_id, &group_id]);
    if expected_id != operation.body.resource_id {
        return Err(SyncLedgerError::Runtime(format!(
            "addon_visibility composite id mismatch: body={}, fields={}",
            operation.body.resource_id, expected_id
        )));
    }
    match operation.body.action {
        ActionType::Insert | ActionType::Update => tx
            .execute(
                // The WHERE EXISTS guard keeps a missing user_groups parent (its
                // Delete tombstone raced ahead of this op) from tripping the FK
                // and poisoning the drain: the group is gone, so its visibility
                // row is correctly skipped (CASCADE removed it on the origin too).
                "INSERT INTO addon_visibility (addon_id, group_id, visible) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM user_groups WHERE id = ?2) \
                 ON CONFLICT(addon_id, group_id) DO UPDATE SET \
                   visible = excluded.visible",
                rusqlite::params![
                    addon_id,
                    group_id,
                    field_bool_or(operation, "visible", true)? as i64,
                ],
            )
            .map_err(sql_error),
        ActionType::Delete => tx
            .execute(
                "DELETE FROM addon_visibility WHERE addon_id = ?1 AND group_id = ?2",
                rusqlite::params![addon_id, group_id],
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

/// The `sync_nodes` columns the ORGANIZATION decides, and which therefore
/// replicate to every node, the one they describe included.
///
/// Everything else on that row is knowledge a node has about itself first-hand:
/// its keys (`public_key`, `public_key_type`), whether this node trusts it
/// (`trust_status`), how it participates in sync (`sync_profile`) and whose it
/// is (`owner_user_id`). The two sets are complementary **by construction** —
/// `is_organizational_node_field` is the only predicate, and a column added to
/// `sync_nodes` later falls on the protected side without anyone remembering to
/// put it there.
///
/// Why the protected side matters: `list_permission_filtered_sync_targets`
/// selects on `trust_status = 'trusted' AND sync_profile IN (...)`, so a peer
/// that could write either of those on THIS node's row would switch this node's
/// synchronization off. The baseline importer already refuses to overwrite this
/// row (`core_baseline.rs`, `if n.node_id == local_node_id { continue }`); the
/// operation path had no such rule.
const ORGANIZATIONAL_NODE_FIELDS: &[&str] = &["operator", "node_kind", "display_name"];

fn is_organizational_node_field(key: &str) -> bool {
    ORGANIZATIONAL_NODE_FIELDS.contains(&key)
}

/// The subset of the organizational fields a node may assert about ITSELF.
/// `operator` is missing on purpose: self-description exists so `node_kind`
/// becomes worth reading, not so a peer can promote itself.
const SELF_DESCRIBED_NODE_FIELDS: &[&str] = &["display_name", "node_kind"];

/// How much of a registry row one operation is allowed to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeWriteScope {
    /// Somebody else's row: every column the operation carries.
    FullRow,
    /// THIS node's own row: the organizational fields only. The rest is dropped
    /// rather than refused, because a reseed replays whole rows and a terminal
    /// conflict on every peer is a worse answer than ignoring the half of the
    /// row the wire has no business stating.
    OrganizationalOnly,
}

/// Version slot for the one `sync_nodes` field that carries a revocation.
///
/// Taking a node off the operator list must not be undone by an older
/// `operator = 1` that reached this node the long way round — the reason
/// `resource_permissions` is last-writer-wins. Versioning the FIELD rather than
/// the row keeps that guarantee while denying a self-describing peer any way to
/// touch the slot: only an operator's operation ever carries `operator`, so only
/// an operator's clock ever moves it.
pub(crate) const OPERATOR_VERSION_RESOURCE_TYPE: &str = "core.sync_node.operator";

/// What this operation states about the operator flag, or `None` when it states
/// nothing about it.
///
/// **The only reading of that field in this module**, and the reason it exists
/// as a function: the version-slot check, the floor and the upsert used to
/// answer this question three different ways, and an `Insert` that simply
/// omitted the column meant "no statement" to two of them and "clear the flag"
/// to the third. One operation from the wire then emptied the operator list
/// without ever naming the field. Absence means "not stating it" everywhere now,
/// the upsert included — see `apply_peer_node_row`.
fn stated_operator(operation: &SyncOperation) -> LedgerResult<Option<bool>> {
    optional_present_bool(operation, "operator")
}

/// `node_kind` is a device hint a peer states about itself, so an unknown value
/// degrades to the column's default instead of tripping the SQL CHECK (which
/// would turn the inbox entry into a terminal conflict carrying raw SQL). Same
/// treatment, same reason, as `skill_status_or_active`.
fn node_kind_or_unknown(kind: String) -> String {
    match kind.as_str() {
        "unknown" | "phone" | "tablet" | "laptop" | "desktop" | "server" | "shared"
        | "authority" => kind,
        _ => "unknown".to_string(),
    }
}

/// `trust_status`, `sync_profile` and `public_key_type` cannot be guessed the
/// way a device kind can — reject with our own message so the conflict reason is
/// readable, not a raw `SQLITE_CONSTRAINT`. Same shape as `check_skill_source`.
fn check_node_enum_field(field: &str, value: &str, allowed: &[&str]) -> LedgerResult<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(SyncLedgerError::Runtime(format!(
            "replicated sync node has invalid {field}: '{value}'"
        )))
    }
}

fn check_node_row_enums(operation: &SyncOperation) -> LedgerResult<()> {
    for (field, allowed) in [
        (
            "trust_status",
            &["untrusted", "pending", "trusted", "revoked"][..],
        ),
        (
            "sync_profile",
            &["standard", "limited", "authority", "storage_only", "ephemeral"][..],
        ),
        ("public_key_type", &["ed25519", "secp256k1"][..]),
    ] {
        if let Some(value) = optional_present_string(operation, field)? {
            check_node_enum_field(field, &value, allowed)?;
        }
    }
    Ok(())
}

/// Is `node_id` on the organization's operator list, as THIS node currently
/// knows it? Read inside the apply transaction, so it sees every operation
/// already materialized ahead of this one.
pub(crate) fn node_is_operator(tx: &rusqlite::Transaction<'_>, node_id: &str) -> LedgerResult<bool> {
    let flag: Option<i64> = tx
        .query_row(
            "SELECT operator FROM sync_nodes WHERE node_id = ?1",
            rusqlite::params![node_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    Ok(flag.unwrap_or(0) != 0)
}

/// Which node this installation is, read from the database inside the apply
/// transaction rather than from the process-global sync runtime.
///
/// The global works in production and is invisible in tests — a unit test never
/// initializes it, so a rule written against it silently evaporates and no
/// mutation of that call site can be caught. `sync::runtime::init` records the
/// same id in `settings` (`LOCAL_NODE_ID_SETTING`) precisely so this decision can
/// be made from data every test can seed.
fn local_node_id(tx: &rusqlite::Transaction<'_>) -> LedgerResult<Option<String>> {
    tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![crate::db::repository::LOCAL_NODE_ID_SETTING],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(sql_error)
}

/// How many nodes this registry currently counts as operators — the same
/// question, in the same words, that the local admin edit and the baseline
/// importer ask (`repository::count_operator_nodes`).
fn operator_count(tx: &rusqlite::Transaction<'_>) -> LedgerResult<i64> {
    crate::db::repository::count_operator_nodes(tx).map_err(sql_error)
}

/// The operator list must never empty out from the wire.
///
/// With zero operators no `core.sync_node` and no `core.node_user_assignment`
/// operation can be authorized on any node ever again, and the registry stops
/// converging until a person edits every node by hand. Deferrable rather than
/// terminal: a promotion queued behind this demotion makes it legal.
fn check_operator_floor(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    let target = operation.body.resource_id.as_str();
    let removes_the_flag = match operation.body.action {
        ActionType::Delete => true,
        _ => stated_operator(operation)? == Some(false),
    };
    if !removes_the_flag || !node_is_operator(tx, target)? {
        return Ok(());
    }
    if operator_count(tx)? <= 1 {
        return Err(SyncLedgerError::DeferredOrdering(format!(
            "node '{target}' is the last operator in this registry, so the wire may not remove it"
        )));
    }
    Ok(())
}

/// Provenance gate for `sync_nodes`. Answers WHO may write and HOW MUCH.
///
/// Two ways in, and no third: the operation comes from a node on the operator
/// list, or it is a node describing itself within `SELF_DESCRIBED_NODE_FIELDS`.
/// Whichever it is, an operation about THIS node's own row is narrowed to the
/// organizational fields — the wire does not get to state this node's keys, its
/// trust or its sync profile.
///
/// Until this existed, `actor_node_id` was not consulted at all — any trusted
/// peer could write any node's row, `node_kind` included, which is exactly why
/// `node_kind` could not be believed.
///
/// A refusal for "the author is not an operator here" is DEFERRABLE, not
/// terminal: the operation that puts the author on the list may still be behind
/// this one in the inbox. A self-description reaching outside its field set can
/// never become valid, so that one is terminal.
fn authorize_sync_node_origin(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
    local_node_id: Option<&str>,
) -> LedgerResult<NodeWriteScope> {
    let actor = operation.body.actor_node_id.as_str();
    let target = operation.body.resource_id.as_str();
    if !node_is_operator(tx, actor)? {
        if actor != target {
            return Err(SyncLedgerError::DeferredOrdering(format!(
                "node '{actor}' may not write the registry row of node '{target}': \
                 it is not on the operator list"
            )));
        }
        if !matches!(operation.body.action, ActionType::Update) {
            return Err(SyncLedgerError::Runtime(format!(
                "node '{actor}' may only describe itself with an update, not {:?}",
                operation.body.action
            )));
        }
        for key in operation.body.changed_fields.keys() {
            if !SELF_DESCRIBED_NODE_FIELDS.contains(&key.as_str()) {
                return Err(SyncLedgerError::Runtime(format!(
                    "node '{actor}' may not assert '{key}' about itself"
                )));
            }
        }
    }
    check_operator_floor(tx, operation)?;
    if local_node_id == Some(target) {
        if matches!(operation.body.action, ActionType::Delete) {
            return Err(SyncLedgerError::Runtime(format!(
                "node '{actor}' may not delete this node's own registry row"
            )));
        }
        return Ok(NodeWriteScope::OrganizationalOnly);
    }
    Ok(NodeWriteScope::FullRow)
}

/// True when this `sync_nodes` operation asks for exactly what the row already
/// holds.
///
/// One question, one implementation: `operation_changes_nothing` compares the
/// columns the operation names against the columns the table has, so a column
/// added to `sync_nodes` later is compared without anybody adding it to a second
/// list. The list this used to carry was that second list, and a write touching
/// only a column missing from it would have been dropped as a no-op.
fn sync_node_operation_changes_nothing(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<bool> {
    operation_changes_nothing(
        tx,
        "sync_nodes",
        &["node_id"],
        std::slice::from_ref(&operation.body.resource_id),
        operation,
    )
}

/// A registry row may only be CREATED for a node this installation actually
/// knows: one it has paired with (`trusted_nodes`) or itself.
///
/// Without it an operator could put an invented node into the registry with
/// `operator = 1` — a node nobody can reach, that nobody can unpair, and that
/// counts towards the operator floor forever. The rule is about creation only:
/// an existing row keeps working after an unpair, which is what lets the prune
/// path (`repository::delete_sync_node`) be the one thing that removes it.
///
/// Deferrable, not terminal: pairing hands the joiner the whole trusted set
/// (`net/iroh/pairing.rs`), but a registry operation about a third node can
/// still arrive before that set does.
fn require_known_node(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    if matches!(operation.body.action, ActionType::Delete) {
        // A delete creates nothing. Refusing one for a node we never had would
        // only turn an already-harmless tombstone into a conflict.
        return Ok(());
    }
    let target = operation.body.resource_id.as_str();
    let known: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_nodes WHERE node_id = ?1) \
                 OR EXISTS(SELECT 1 FROM trusted_nodes WHERE node_id = ?1 AND is_active = 1) \
                 OR EXISTS(SELECT 1 FROM settings WHERE key = ?2 AND value = ?1)",
            rusqlite::params![target, crate::db::repository::LOCAL_NODE_ID_SETTING],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if known {
        Ok(())
    } else {
        Err(SyncLedgerError::DeferredOrdering(format!(
            "no registry row may be created for node '{target}': this node has never paired with it"
        )))
    }
}

fn apply_sync_node(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let changes_nothing = sync_node_operation_changes_nothing(tx, operation)?;
    let local = local_node_id(tx)?;
    let scope = match authorize_sync_node_origin(tx, operation, local.as_deref()) {
        Ok(scope) => scope,
        // An operation that asks for exactly what the row already holds
        // exercises no authority, so it needs none: it is ignored rather than
        // refused. `reseed_core_state_from_current_rows` replays a whole-row
        // `Insert` for every node on every baseline reset, and most of those
        // rows every peer already agrees with — refusing them would fill every
        // inbox with conflicts about rows nobody was changing. Ignoring also
        // keeps it away from the version slot below, where an unauthorized peer
        // could otherwise pin the order with a clock from the future while
        // "changing nothing".
        Err(_) if changes_nothing => return Ok(0),
        Err(error) => return Err(error),
    };
    check_node_row_enums(operation)?;
    require_known_node(tx, operation)?;

    // The operator flag carries a revocation, so it is ordered by its own clock
    // (see `OPERATOR_VERSION_RESOURCE_TYPE`). The slot advances on every
    // AUTHORIZED statement about the flag, a statement that restates the value
    // already held included: it is still a real author saying something at a
    // real time, and skipping it leaves a hole in the order that a later-arriving
    // older operation walks straight through. An operation that lost this race is
    // dropped whole — it is one administrator decision, and applying the device
    // kind out of a decision the mesh has already superseded would record half of
    // something that no longer holds.
    let carries_operator = stated_operator(operation)?.is_some();
    if carries_operator {
        if !incoming_hlc_wins(
            tx,
            OPERATOR_VERSION_RESOURCE_TYPE,
            &operation.body.resource_id,
            &operation.body.hlc_timestamp,
        )? {
            return Ok(0);
        }
        upsert_resource_version(
            tx,
            OPERATOR_VERSION_RESOURCE_TYPE,
            &operation.body.resource_id,
            &operation.body.hlc_timestamp,
        )?;
    }
    if changes_nothing {
        return Ok(0);
    }

    match scope {
        NodeWriteScope::OrganizationalOnly => apply_own_node_row(tx, operation),
        NodeWriteScope::FullRow => apply_peer_node_row(tx, operation),
    }
}

/// This node's own registry row: only the organizational fields are taken from
/// the wire, whatever the operation says about the rest.
///
/// `Insert` and `Update` mean the same thing here — merge what the organization
/// decided — because a reseed states the whole row as an `Insert` and this node's
/// row always exists already (`ensure_local_node_in_sync_identity` at boot).
fn apply_own_node_row(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    let dropped: Vec<&String> = operation
        .body
        .changed_fields
        .keys()
        .filter(|key| !is_organizational_node_field(key))
        .collect();
    if !dropped.is_empty() {
        tracing::warn!(
            actor = %operation.body.actor_node_id,
            fields = ?dropped,
            "core sync: ignoring a peer's statement about this node's own identity fields"
        );
    }
    let display_name = optional_present_string(operation, "display_name")?;
    let node_kind = optional_present_string(operation, "node_kind")?.map(node_kind_or_unknown);
    let operator = stated_operator(operation)?;
    if display_name.is_none() && node_kind.is_none() && operator.is_none() {
        return Ok(0);
    }
    tx.execute(
        "UPDATE sync_nodes SET \
         display_name = COALESCE(?2, display_name), node_kind = COALESCE(?3, node_kind), \
         operator = COALESCE(?4, operator) \
         WHERE node_id = ?1",
        rusqlite::params![
            operation.body.resource_id,
            display_name,
            node_kind,
            operator,
        ],
    )
    .map_err(sql_error)
    .and_then(require_existing(operation))
}

/// Somebody else's registry row.
///
/// `Insert` is an upsert, and every column it carries is written **by presence**:
/// a field the operation does not name is left where it is on an existing row,
/// and takes the column default on a genuinely new one. It used to substitute a
/// default instead, which turned an `Insert` about a display name into a silent
/// `operator = 0`, `trust_status = 'untrusted'` and `sync_profile = 'standard'`
/// for a row that said otherwise. `owner_user_id` needs `nullable_update_string`
/// rather than `optional_present_string` to say it: that column is nullable, so
/// "not named" and "named as null" are two different statements and only the
/// second one clears it. `public_key` stays required: an `Insert` that introduces
/// a node has to say which node.
fn apply_peer_node_row(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    match operation.body.action {
        ActionType::Insert => tx
            .execute(
                "INSERT INTO sync_nodes \
                 (node_id, public_key, public_key_type, display_name, node_kind, trust_status, owner_user_id, sync_profile, operator) \
                 VALUES (?1, ?2, COALESCE(?3, 'ed25519'), COALESCE(?4, ''), COALESCE(?5, 'unknown'), \
                         COALESCE(?6, 'untrusted'), ?8, COALESCE(?9, 'standard'), COALESCE(?10, 0)) \
                 ON CONFLICT(node_id) DO UPDATE SET \
                 public_key = ?2, public_key_type = COALESCE(?3, sync_nodes.public_key_type), \
                 display_name = COALESCE(?4, sync_nodes.display_name), \
                 node_kind = COALESCE(?5, sync_nodes.node_kind), \
                 trust_status = COALESCE(?6, sync_nodes.trust_status), \
                 owner_user_id = CASE WHEN ?7 THEN ?8 ELSE sync_nodes.owner_user_id END, \
                 sync_profile = COALESCE(?9, sync_nodes.sync_profile), \
                 operator = COALESCE(?10, sync_nodes.operator)",
                rusqlite::params![
                    operation.body.resource_id,
                    field_string(operation, "public_key")?,
                    optional_present_string(operation, "public_key_type")?,
                    optional_present_string(operation, "display_name")?,
                    optional_present_string(operation, "node_kind")?.map(node_kind_or_unknown),
                    optional_present_string(operation, "trust_status")?,
                    nullable_update_string(operation, "owner_user_id")?.0,
                    nullable_update_string(operation, "owner_user_id")?.1,
                    optional_present_string(operation, "sync_profile")?,
                    stated_operator(operation)?,
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
                 sync_profile = COALESCE(?9, sync_profile), \
                 operator = COALESCE(?10, operator) \
                 WHERE node_id = ?1",
                rusqlite::params![
                    operation.body.resource_id,
                    optional_present_string(operation, "public_key")?,
                    optional_present_string(operation, "public_key_type")?,
                    optional_present_string(operation, "display_name")?,
                    optional_present_string(operation, "node_kind")?.map(node_kind_or_unknown),
                    optional_present_string(operation, "trust_status")?,
                    nullable_update_string(operation, "owner_user_id")?.0,
                    nullable_update_string(operation, "owner_user_id")?.1,
                    optional_present_string(operation, "sync_profile")?,
                    stated_operator(operation)?,
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

/// Provenance gate for `user_identity_keys`.
///
/// Until this existed the table had NO origin rule at all: any trusted peer
/// could overwrite the identity key of any user, and the key is what an
/// administrator's decision will be signed with (plan §6.1, step 15). Publishing
/// a key for somebody else is the whole attack — the verifier only asks whether
/// an active key of that user signed the message, so adding one is as good as
/// replacing one.
///
/// The rule is the one plan §6.1 gives for the other identity-registry table,
/// `node_user_assignments`: the organization's operator nodes write it. That is
/// what the mesh can check today; binding a key to the person it belongs to
/// needs the signature step 15 brings, and this gate is what step 15 widens.
///
/// Deferrable, for the same reason as `sync_nodes`: the operation that puts the
/// author on the operator list may still be queued behind this one.
fn authorize_user_identity_key_origin(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    let actor = operation.body.actor_node_id.as_str();
    if node_is_operator(tx, actor)? {
        return Ok(());
    }
    Err(SyncLedgerError::DeferredOrdering(format!(
        "node '{actor}' may not write user identity keys: it is not on the operator list"
    )))
}

fn apply_user_identity_key(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    // A restatement of a key everybody already agrees on exercises no authority,
    // so it needs none — `reseed_core_state_from_current_rows` replays this whole
    // table after every baseline reset, from whichever node happens to reset.
    if let Err(error) = authorize_user_identity_key_origin(tx, operation) {
        if operation_changes_nothing(
            tx,
            "user_identity_keys",
            &["key_id"],
            std::slice::from_ref(&operation.body.resource_id),
            operation,
        )? {
            return Ok(0);
        }
        return Err(error);
    }
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

/// Provenance gate for `node_user_assignments`.
///
/// An assignment says which person a node acts for, so an unchecked one lets any
/// trusted peer point any node at any user. Only the organization's operator
/// nodes write it. Deferrable for the same reason as `sync_nodes`: the operation
/// that puts the author on the list may still be queued behind this one.
fn authorize_node_user_assignment_origin(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<()> {
    let actor = operation.body.actor_node_id.as_str();
    if node_is_operator(tx, actor)? {
        return Ok(());
    }
    Err(SyncLedgerError::DeferredOrdering(format!(
        "node '{actor}' may not write node/user assignments: it is not on the operator list"
    )))
}

fn apply_node_user_assignment(
    tx: &rusqlite::Transaction<'_>,
    operation: &SyncOperation,
) -> LedgerResult<usize> {
    authorize_node_user_assignment_origin(tx, operation)?;
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

pub(crate) fn field_string(operation: &SyncOperation, key: &str) -> LedgerResult<String> {
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

pub(crate) fn optional_present_string(
    operation: &SyncOperation,
    key: &str,
) -> LedgerResult<Option<String>> {
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

pub(crate) fn optional_present_i64(operation: &SyncOperation, key: &str) -> LedgerResult<Option<i64>> {
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

/// One column of a table as the database itself describes it. Read from
/// `PRAGMA table_info`, never from a list somebody has to remember to update:
/// the arms that build SQL from an operation's fields use this both as the
/// allow-list of writable identifiers and as the answer to "may this column be
/// left unstated on a new row".
#[derive(Debug, Clone)]
pub(crate) struct ColumnInfo {
    pub name: String,
    pub not_null: bool,
    pub has_default: bool,
}

/// The columns of `table_name`, in schema order.
///
/// `table_name` always comes from a `CoreSyncDescriptor`, so it is a compiled-in
/// identifier and never wire input — the wire only ever names FIELDS, which are
/// then checked against what this returns.
pub(crate) fn table_columns(
    tx: &rusqlite::Transaction<'_>,
    table_name: &str,
) -> LedgerResult<Vec<ColumnInfo>> {
    let mut stmt = tx
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .map_err(sql_error)?;
    let columns = stmt
        .query_map([], |row| {
            Ok(ColumnInfo {
                name: row.get::<_, String>(1)?,
                not_null: row.get::<_, i64>(3)? != 0,
                has_default: row.get::<_, Option<String>>(4)?.is_some(),
            })
        })
        .map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)?;
    if columns.is_empty() {
        return Err(SyncLedgerError::Runtime(format!(
            "core sync table has no columns: {table_name}"
        )));
    }
    Ok(columns)
}

/// True when every column this operation states already holds the value it asks
/// for — including the case where it states nothing but the key.
///
/// An operation that changes nothing exercises no authority, so it needs none,
/// and every arm that has an origin rule uses this to say so. It is not a
/// convenience: `reseed_core_state_from_current_rows` restates every row this
/// node holds after a baseline reset, most of them written by somebody else and
/// already agreed on by the receiver. Refusing those would turn one reset into a
/// terminal conflict per row, on rows nobody was changing.
///
/// Only columns that exist are compared. The ledger envelope adds `capture_id`
/// to every operation's fields, and a peer on a newer schema states columns this
/// node does not have yet; neither says anything about this row's contents.
pub(crate) fn operation_changes_nothing(
    tx: &rusqlite::Transaction<'_>,
    table_name: &str,
    key_columns: &[&str],
    key_values: &[String],
    operation: &SyncOperation,
) -> LedgerResult<bool> {
    if matches!(operation.body.action, ActionType::Delete) {
        return Ok(false);
    }
    let stated: Vec<String> = table_columns(tx, table_name)?
        .into_iter()
        .map(|column| column.name)
        .filter(|name| {
            !key_columns.contains(&name.as_str())
                && operation.body.changed_fields.contains_key(name)
        })
        .collect();
    let where_clause = key_columns
        .iter()
        .enumerate()
        .map(|(index, key)| format!("\"{key}\" = ?{}", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let selection = if stated.is_empty() {
        "1".to_string()
    } else {
        stated
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let sql = format!("SELECT {selection} FROM {table_name} WHERE {where_clause}");
    let held: Option<Vec<rusqlite::types::Value>> = tx
        .query_row(&sql, rusqlite::params_from_iter(key_values), |row| {
            (0..stated.len())
                .map(|index| row.get::<_, rusqlite::types::Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .optional()
        .map_err(sql_error)?;
    // A row that is not here cannot already agree with anything.
    let Some(held) = held else {
        return Ok(false);
    };
    for (name, value) in stated.iter().zip(held.iter()) {
        let incoming = operation
            .body
            .changed_fields
            .get(name)
            .expect("column was selected because the operation states it");
        if !field_equals_stored(incoming, value) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Does a wire field state exactly the value the column holds? A boolean and the
/// 0/1 SQLite stores for it are the same statement; anything whose types do not
/// line up is a change by definition.
fn field_equals_stored(incoming: &FieldValue, held: &rusqlite::types::Value) -> bool {
    use rusqlite::types::Value;
    match (incoming, held) {
        (FieldValue::Null, Value::Null) => true,
        (FieldValue::Bool(incoming), Value::Integer(held)) => i64::from(*incoming) == *held,
        (FieldValue::I64(incoming), Value::Integer(held)) => incoming == held,
        (FieldValue::U64(incoming), Value::Integer(held)) => {
            i64::try_from(*incoming).map(|value| value == *held).unwrap_or(false)
        }
        (FieldValue::String(incoming), Value::Text(held)) => incoming == held,
        (FieldValue::Decimal(incoming), Value::Text(held)) => incoming == held,
        (FieldValue::Decimal(incoming), Value::Real(held)) => incoming == &held.to_string(),
        _ => false,
    }
}

pub(crate) fn sql_error(error: rusqlite::Error) -> SyncLedgerError {
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

    /// Builds a core-sync operation for one of the app-permission matrix kinds,
    /// with the composite resource_id computed the way the capture site does.
    fn matrix_operation(
        resource_type: &str,
        table_name: &str,
        primary_key: &str,
        id_parts: &[&str],
        action: ActionType,
        fields: BTreeMap<String, FieldValue>,
    ) -> SyncOperation {
        let mut op = rollup_operation(
            &crate::sync::resource_id::composite_resource_id(id_parts),
            fields,
        );
        op.body.partition_id = PartitionId::new("core/org/default/addons").unwrap();
        op.body.resource_type = resource_type.to_string();
        op.body.table_name = table_name.to_string();
        op.body.primary_key = primary_key.to_string();
        op.body.action = action;
        op
    }

    /// A core-sync operation about one node's registry row, authored by
    /// `actor`. `resource_id == actor` is a node describing itself.
    fn node_operation(
        resource_id: &str,
        actor: &str,
        action: ActionType,
        fields: BTreeMap<String, FieldValue>,
    ) -> SyncOperation {
        let mut op = rollup_operation(resource_id, fields);
        op.body.partition_id = PartitionId::new("core/org/default/identity").unwrap();
        op.body.resource_type = "core.sync_node".to_string();
        op.body.table_name = "sync_nodes".to_string();
        op.body.primary_key = "node_id".to_string();
        op.body.action = action;
        op.body.actor_node_id = actor.to_string();
        op.body.hlc_timestamp.node_id = actor.to_string();
        op
    }

    fn seed_node(tx: &rusqlite::Transaction<'_>, node_id: &str, operator: bool) {
        tx.execute(
            "INSERT INTO sync_nodes (node_id, public_key, display_name, node_kind, trust_status, operator) \
             VALUES (?1, 'pk', '', 'unknown', 'untrusted', ?2)",
            rusqlite::params![node_id, operator],
        )
        .unwrap();
    }

    /// A node this installation has paired with. Creating a REGISTRY row for a
    /// node requires it (`require_known_node`), so a test about anything else
    /// that introduces a new node has to say the node exists first.
    fn seed_trusted_node(tx: &rusqlite::Transaction<'_>, node_id: &str) {
        tx.execute(
            "INSERT INTO trusted_nodes (node_id, public_key, is_active) VALUES (?1, 'pk', 1)",
            rusqlite::params![node_id],
        )
        .unwrap();
    }

    fn node_exists(tx: &rusqlite::Transaction<'_>, node_id: &str) -> bool {
        tx.query_row(
            "SELECT 1 FROM sync_nodes WHERE node_id = ?1",
            rusqlite::params![node_id],
            |_| Ok(true),
        )
        .optional()
        .unwrap()
        .unwrap_or(false)
    }

    fn node_row(tx: &rusqlite::Transaction<'_>, node_id: &str) -> (String, bool, String) {
        tx.query_row(
            "SELECT node_kind, operator, trust_status FROM sync_nodes WHERE node_id = ?1",
            rusqlite::params![node_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    /// The same operation, stamped with an explicit wall clock. The operator
    /// flag is ordered by its own slot, so two writes to one node's flag must
    /// carry distinct timestamps or the second is (correctly) dropped as stale.
    fn at_wall_time(mut op: SyncOperation, wall_time_ms: i64) -> SyncOperation {
        op.body.hlc_timestamp.wall_time_ms = wall_time_ms;
        op
    }

    fn field_map(pairs: &[(&str, FieldValue)]) -> BTreeMap<String, FieldValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// A registry row for a node this installation has never paired with must
    /// not be creatable from the wire (U3 of the step 5 review). Without the
    /// rule an operator could seed the registry with an invented node carrying
    /// `operator = 1`: unreachable, unpairable, and counting towards the
    /// operator floor forever.
    #[test]
    fn no_registry_row_is_created_for_a_node_this_installation_never_paired_with() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_local_node_id(&tx, "node-me");
        seed_node(&tx, "node-op", true);

        let invent = node_operation(
            "node-ghost",
            "node-op",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("operator", FieldValue::Bool(true)),
            ]),
        );
        let refused = apply_sync_node(&tx, &invent).expect_err("an unknown node has no row");
        assert!(
            matches!(refused, SyncLedgerError::DeferredOrdering(_)),
            "pairing may still be behind the operation: {refused:?}"
        );
        assert!(
            !node_exists(&tx, "node-ghost"),
            "the invented node must not be in the registry"
        );

        // Once this node has actually paired with it, the same operation lands.
        seed_trusted_node(&tx, "node-ghost");
        assert_eq!(
            apply_sync_node(&tx, &at_wall_time(invent, 2_000)).unwrap(),
            1
        );
        assert!(node_exists(&tx, "node-ghost"));
    }

    /// `user_identity_keys` had NO origin rule at all: any trusted peer could
    /// publish or replace the identity key of any user, which is the key an
    /// administrator's decisions will be signed with (step 15). Publishing one
    /// for somebody else IS the attack — the verifier only asks whether an active
    /// key of that user signed, so adding a key is as good as replacing one.
    #[test]
    fn user_identity_keys_are_written_only_by_operator_nodes() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-op", true);
        seed_node(&tx, "node-plain", false);
        tx.execute(
            "INSERT INTO user_accounts (id, username, password_hash, display_name, role) \
             VALUES ('u-1', 'key-owner', 'x', 'Key Owner', 'admin')",
            [],
        )
        .unwrap();

        let key_op = |actor: &str, public_key: &str| {
            let mut op = rollup_operation(
                "key-1",
                field_map(&[
                    ("user_id", FieldValue::String("u-1".into())),
                    ("key_type", FieldValue::String("ed25519".into())),
                    ("public_key", FieldValue::String(public_key.into())),
                ]),
            );
            op.body.resource_type = "core.user_identity_key".to_string();
            op.body.table_name = "user_identity_keys".to_string();
            op.body.primary_key = "key_id".to_string();
            op.body.action = ActionType::Insert;
            op.body.actor_node_id = actor.to_string();
            op
        };

        let refused = apply_user_identity_key(&tx, &key_op("node-plain", "forged"))
            .expect_err("a peer may not publish somebody's identity key");
        assert!(
            matches!(refused, SyncLedgerError::DeferredOrdering(_)),
            "the author's promotion may still be behind it: {refused:?}"
        );

        assert_eq!(
            apply_user_identity_key(&tx, &key_op("node-op", "real")).unwrap(),
            1
        );
        let stored: String = tx
            .query_row(
                "SELECT public_key FROM user_identity_keys WHERE key_id = 'key-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "real");

        // The overwrite is the whole point: with the key in place, a non-operator
        // peer restating it differently is still refused.
        let overwrite = apply_user_identity_key(&tx, &key_op("node-plain", "forged"))
            .expect_err("a peer may not replace an identity key either");
        assert!(matches!(overwrite, SyncLedgerError::DeferredOrdering(_)));
        let unchanged: String = tx
            .query_row(
                "SELECT public_key FROM user_identity_keys WHERE key_id = 'key-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unchanged, "real");

        // And a restatement of the key everybody agrees on is ignored, not
        // refused: the reseed replays this table from whichever node resets.
        assert_eq!(
            apply_user_identity_key(&tx, &key_op("node-plain", "real")).unwrap(),
            0
        );
    }

    /// The provenance rule of `apply_sync_node`, all four ways through it: a
    /// stranger writing someone else's row, a node describing itself inside and
    /// outside its own field set, and an operator writing anything.
    ///
    /// Before the rule existed, every one of these applied — `actor_node_id` was
    /// not read at all, which is why `node_kind` could not be believed and why
    /// any trusted peer could have marked itself `trusted` in the registry.
    #[test]
    fn sync_node_writes_are_bound_to_their_author() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-a", false);
        seed_node(&tx, "node-b", false);
        seed_node(&tx, "node-op", true);

        // 1. A stranger writing someone else's row: refused, and DEFERRABLE —
        //    the op that puts the author on the operator list may still be
        //    behind this one in the inbox.
        let stranger = node_operation(
            "node-b",
            "node-a",
            ActionType::Update,
            field_map(&[("node_kind", FieldValue::String("server".into()))]),
        );
        match apply_sync_node(&tx, &stranger) {
            Err(SyncLedgerError::DeferredOrdering(message)) => {
                assert!(message.contains("operator list"), "message: {message}")
            }
            other => panic!("a stranger must not write another node's row: {other:?}"),
        }
        assert_eq!(node_row(&tx, "node-b").0, "unknown");

        // 2. A node describing itself, inside its field set: applied.
        let self_kind = node_operation(
            "node-a",
            "node-a",
            ActionType::Update,
            field_map(&[("node_kind", FieldValue::String("laptop".into()))]),
        );
        assert_eq!(apply_sync_node(&tx, &self_kind).unwrap(), 1);
        assert_eq!(node_row(&tx, "node-a").0, "laptop");

        // 3. The same node reaching past that set: refused, and TERMINAL —
        //    no later arrival can make "I am an operator, signed me" valid.
        for forbidden in [
            ("operator", FieldValue::Bool(true)),
            ("trust_status", FieldValue::String("trusted".into())),
            ("public_key", FieldValue::String("attacker".into())),
            ("sync_profile", FieldValue::String("authority".into())),
            ("owner_user_id", FieldValue::String("u1".into())),
        ] {
            let op = node_operation(
                "node-a",
                "node-a",
                ActionType::Update,
                field_map(&[forbidden.clone()]),
            );
            match apply_sync_node(&tx, &op) {
                Err(SyncLedgerError::Runtime(message)) => assert!(
                    message.contains(forbidden.0),
                    "message must name the field: {message}"
                ),
                other => panic!("self-assertion of '{}' must fail: {other:?}", forbidden.0),
            }
        }
        let (kind, operator, trust) = node_row(&tx, "node-a");
        assert_eq!((kind.as_str(), operator, trust.as_str()), ("laptop", false, "untrusted"));

        // 4. And it may not claim a whole row through an insert either — the
        //    upsert would carry every column at once, field set or no field set.
        let self_insert = node_operation(
            "node-a",
            "node-a",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("node_kind", FieldValue::String("server".into())),
                ("trust_status", FieldValue::String("trusted".into())),
            ]),
        );
        assert!(matches!(
            apply_sync_node(&tx, &self_insert),
            Err(SyncLedgerError::Runtime(_))
        ));
        assert_eq!(node_row(&tx, "node-a").2, "untrusted");

        // 5. An operator node writes any row, the operator flag included — that
        //    is how an admin's decision reaches the rest of the organization.
        let promote = node_operation(
            "node-a",
            "node-op",
            ActionType::Update,
            field_map(&[
                ("operator", FieldValue::Bool(true)),
                ("node_kind", FieldValue::String("server".into())),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &promote).unwrap(), 1);
        let (kind, operator, _) = node_row(&tx, "node-a");
        assert_eq!((kind.as_str(), operator), ("server", true));

        // 6. …and the node it just promoted is an operator from the next
        //    operation onward, read inside the transaction.
        let chained = node_operation(
            "node-b",
            "node-a",
            ActionType::Update,
            field_map(&[("operator", FieldValue::Bool(true))]),
        );
        assert_eq!(apply_sync_node(&tx, &chained).unwrap(), 1);
        assert!(node_row(&tx, "node-b").1);
    }

    /// Seeds this installation's own node id the way `runtime::init` does, so a
    /// test can exercise the "is this my row?" rule that production reads from
    /// the same place.
    fn seed_local_node_id(tx: &rusqlite::Transaction<'_>, node_id: &str) {
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![crate::db::repository::LOCAL_NODE_ID_SETTING, node_id],
        )
        .unwrap();
    }

    fn full_node_row(tx: &rusqlite::Transaction<'_>, node_id: &str) -> (String, String, String, bool, String) {
        tx.query_row(
            "SELECT public_key, trust_status, sync_profile, operator, node_kind \
             FROM sync_nodes WHERE node_id = ?1",
            rusqlite::params![node_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap()
    }

    /// Every column of `sync_nodes` is classified, and the classification is what
    /// decides whether the wire may state it about THIS node's row.
    ///
    /// Driven by the live schema (`PRAGMA table_info`) with the expected set
    /// spelled out here, so three things fail loudly instead of one: moving a
    /// column between the two sides, adding a column to `sync_nodes` without
    /// deciding which side it is on, and the guard disagreeing with the constant.
    #[test]
    fn every_registry_column_is_either_organizational_or_this_node_s_own() {
        const EXPECTED_ORGANIZATIONAL: &[&str] = &["operator", "node_kind", "display_name"];
        // The key and the columns SQLite maintains: never carried by an operation.
        const NOT_WRITABLE: &[&str] = &["node_id", "last_seen_at", "created_at", "updated_at"];

        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_local_node_id(&tx, "node-me");
        seed_node(&tx, "node-me", false);
        seed_node(&tx, "node-op", true);

        let columns: Vec<String> = {
            let mut stmt = tx.prepare("PRAGMA table_info(sync_nodes)").unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert!(
            columns.len() >= 9,
            "table_info returned nothing usable: {columns:?}"
        );

        let mut seen_organizational = Vec::new();
        for column in columns.iter().filter(|c| !NOT_WRITABLE.contains(&c.as_str())) {
            let organizational = EXPECTED_ORGANIZATIONAL.contains(&column.as_str());
            assert_eq!(
                is_organizational_node_field(column),
                organizational,
                "column '{column}' is classified differently than this test expects"
            );
            if organizational {
                seen_organizational.push(column.clone());
            }
            // An operator's write of a single column against our own row: an
            // organizational column reaches the row, anything else is dropped.
            let value = if column == "operator" {
                FieldValue::Bool(true)
            } else {
                FieldValue::String("zzz-probe".to_string())
            };
            let op = node_operation(
                "node-me",
                "node-op",
                ActionType::Update,
                field_map(&[(column.as_str(), value)]),
            );
            let scope = authorize_sync_node_origin(&tx, &op, Some("node-me")).unwrap();
            assert_eq!(
                scope,
                NodeWriteScope::OrganizationalOnly,
                "our own row must never be writable in full"
            );
        }
        let mut expected = EXPECTED_ORGANIZATIONAL.to_vec();
        expected.sort_unstable();
        seen_organizational.sort();
        assert_eq!(
            seen_organizational, expected,
            "the schema no longer carries exactly the organizational columns this rule names"
        );
    }

    /// The measured attack from the review: an operator elsewhere switching this
    /// node's synchronization off through `sync_profile` (the second half of the
    /// clause `list_permission_filtered_sync_targets` selects on), and pointing
    /// this node at a user of their choosing through `owner_user_id`.
    ///
    /// Both are dropped rather than refused: a reseed states whole rows, and a
    /// terminal conflict on every peer is a worse answer than ignoring the half
    /// of the row the wire has no business stating.
    #[test]
    fn no_peer_rewrites_this_node_s_own_key_trust_profile_or_owner() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_local_node_id(&tx, "node-me");
        tx.execute(
            "INSERT INTO sync_nodes (node_id, public_key, node_kind, trust_status, sync_profile, operator) \
             VALUES ('node-me', 'mykey', 'server', 'trusted', 'authority', 1)",
            [],
        )
        .unwrap();
        seed_node(&tx, "node-op", true);
        tx.execute(
            "INSERT INTO user_accounts (id, username, email, password_hash, role) \
             VALUES ('u1', 'u1', 'u1@example.test', 'x', 'user')",
            [],
        )
        .unwrap();

        for (field, value) in [
            ("public_key", FieldValue::String("attacker".into())),
            ("public_key_type", FieldValue::String("secp256k1".into())),
            ("trust_status", FieldValue::String("revoked".into())),
            ("sync_profile", FieldValue::String("ephemeral".into())),
            ("owner_user_id", FieldValue::String("u1".into())),
        ] {
            let op = node_operation(
                "node-me",
                "node-op",
                ActionType::Update,
                field_map(&[(field, value)]),
            );
            apply_sync_node(&tx, &op).expect("a dropped field is not a conflict");
            let (key, trust, profile, _, _) = full_node_row(&tx, "node-me");
            assert_eq!(
                (key.as_str(), trust.as_str(), profile.as_str()),
                ("mykey", "trusted", "authority"),
                "'{field}' reached our own row"
            );
            let owner: Option<String> = tx
                .query_row(
                    "SELECT owner_user_id FROM sync_nodes WHERE node_id = 'node-me'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(owner, None, "'{field}' reached our own owner_user_id");
        }

        // Our own row cannot be deleted from the wire either.
        let delete = node_operation("node-me", "node-op", ActionType::Delete, BTreeMap::new());
        assert!(matches!(
            apply_sync_node(&tx, &delete),
            Err(SyncLedgerError::Runtime(_))
        ));

        // …while an organizational decision about this node still arrives.
        let demote = node_operation(
            "node-me",
            "node-op",
            ActionType::Update,
            field_map(&[
                ("operator", FieldValue::Bool(false)),
                ("node_kind", FieldValue::String("desktop".into())),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &demote).unwrap(), 1);
        let (_, _, _, operator, kind) = full_node_row(&tx, "node-me");
        assert_eq!((operator, kind.as_str()), (false, "desktop"));
    }

    /// The same rule read from the database, not from a process-global.
    ///
    /// `apply_sync_node` asks `settings` which node this installation is. Before
    /// that it asked `sync::runtime::local_node_id()`, which is `None` in every
    /// unit test — so the whole rule evaporated under test and no mutation of it
    /// could be caught. This test fails if the lookup stops finding the id.
    #[test]
    fn the_own_row_rule_reads_this_node_s_identity_from_the_database() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-me", false);
        seed_node(&tx, "node-op", true);

        let attack = || {
            node_operation(
                "node-me",
                "node-op",
                ActionType::Update,
                field_map(&[("trust_status", FieldValue::String("revoked".into()))]),
            )
        };

        // Without the setting this node has no idea which row is its own, so the
        // operation lands as an ordinary peer write — that is the state the rule
        // must not be tested in.
        assert_eq!(apply_sync_node(&tx, &attack()).unwrap(), 1);
        assert_eq!(full_node_row(&tx, "node-me").1, "revoked");

        tx.execute(
            "UPDATE sync_nodes SET trust_status = 'trusted' WHERE node_id = 'node-me'",
            [],
        )
        .unwrap();
        seed_local_node_id(&tx, "node-me");
        apply_sync_node(&tx, &attack()).expect("dropped, not refused");
        assert_eq!(
            full_node_row(&tx, "node-me").1,
            "trusted",
            "the rule must bite once this node knows which row is its own"
        );
    }

    /// A peer cannot pin its row against an administrator's decision by
    /// describing itself with a clock from the future.
    ///
    /// This is why `sync_nodes` is not whole-row last-writer-wins: `wall_time_ms`
    /// comes off the wire with no skew ceiling anywhere in the ledger, so a
    /// self-description at `i64::MAX` would set the row's version out of reach
    /// and every later demotion would be dropped as "stale" — silently, with the
    /// inbox entry marked applied.
    #[test]
    fn a_future_dated_self_description_cannot_pin_the_operator_flag() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-x", true);
        seed_node(&tx, "node-op", true);

        let freeze = at_wall_time(
            node_operation(
                "node-x",
                "node-x",
                ActionType::Update,
                field_map(&[("node_kind", FieldValue::String("laptop".into()))]),
            ),
            i64::MAX - 1,
        );
        assert_eq!(apply_sync_node(&tx, &freeze).unwrap(), 1);

        let demote = at_wall_time(
            node_operation(
                "node-x",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(false))]),
            ),
            1_800_000_000_000,
        );
        assert_eq!(apply_sync_node(&tx, &demote).unwrap(), 1);
        assert!(!full_node_row(&tx, "node-x").3, "the demotion must land");

        // The flag's own slot still orders operator writes, so a stale promotion
        // cannot resurrect the authority that was just taken away.
        let stale_promotion = at_wall_time(
            node_operation(
                "node-x",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(true))]),
            ),
            1_700_000_000_000,
        );
        assert_eq!(apply_sync_node(&tx, &stale_promotion).unwrap(), 0);
        assert!(!full_node_row(&tx, "node-x").3, "a stale promotion must lose");
    }

    /// Values outside a column's CHECK arrive from peers. A device kind degrades
    /// to the column default; the fields that cannot be guessed are refused with
    /// our own message. Neither may reach SQLite and come back as a terminal
    /// conflict carrying raw SQL — the rule `skill_status_or_active` and
    /// `check_skill_source` already state for replicated skills.
    #[test]
    fn out_of_set_registry_values_never_reach_the_sql_check() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-x", false);
        seed_node(&tx, "node-op", true);

        let kind = node_operation(
            "node-x",
            "node-x",
            ActionType::Update,
            field_map(&[("node_kind", FieldValue::String("toaster".into()))]),
        );
        assert_eq!(apply_sync_node(&tx, &kind).unwrap(), 1);
        assert_eq!(full_node_row(&tx, "node-x").4, "unknown");

        for (field, value) in [
            ("trust_status", "sideways"),
            ("sync_profile", "gaseous"),
            ("public_key_type", "rot13"),
        ] {
            let op = node_operation(
                "node-x",
                "node-op",
                ActionType::Update,
                field_map(&[(field, FieldValue::String(value.into()))]),
            );
            match apply_sync_node(&tx, &op) {
                Err(SyncLedgerError::Runtime(message)) => {
                    assert!(message.contains(field), "message: {message}");
                    assert!(
                        !message.contains("CHECK constraint"),
                        "the conflict reason must be ours, not SQLite's: {message}"
                    );
                }
                other => panic!("'{field}' = '{value}' must be refused by name: {other:?}"),
            }
        }

        // An insert from an operator gets the same treatment.
        seed_trusted_node(&tx, "node-new");
        let insert = node_operation(
            "node-new",
            "node-op",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("node_kind", FieldValue::String("toaster".into())),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &insert).unwrap(), 1);
        assert_eq!(full_node_row(&tx, "node-new").4, "unknown");
    }

    /// `reseed_core_state_from_current_rows` replays a whole-row `Insert` for
    /// every node on every baseline reset, this node's own row included. An
    /// operation that asks for what the row already holds exercises no authority,
    /// so it needs none and must not leave a conflict behind — otherwise every
    /// reseed poisons every peer's inbox with rows nobody was changing.
    #[test]
    fn a_reseed_of_rows_everybody_agrees_on_is_a_no_op() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_local_node_id(&tx, "node-me");
        tx.execute(
            "INSERT INTO sync_nodes (node_id, public_key, public_key_type, display_name, \
             node_kind, trust_status, owner_user_id, sync_profile, operator) \
             VALUES ('node-me', 'mykey', 'ed25519', '', 'server', 'trusted', NULL, 'authority', 1)",
            [],
        )
        .unwrap();
        seed_node(&tx, "node-plain", false);

        let reseed_row = |node_id: &str, key: &str, kind: &str, trust: &str, profile: &str, operator: bool| {
            node_operation(
                node_id,
                "node-stranger",
                ActionType::Insert,
                field_map(&[
                    ("public_key", FieldValue::String(key.into())),
                    ("public_key_type", FieldValue::String("ed25519".into())),
                    ("display_name", FieldValue::String("".into())),
                    ("node_kind", FieldValue::String(kind.into())),
                    ("trust_status", FieldValue::String(trust.into())),
                    ("owner_user_id", FieldValue::Null),
                    ("sync_profile", FieldValue::String(profile.into())),
                    ("operator", FieldValue::Bool(operator)),
                ]),
            )
        };

        // Our own row, restated exactly: no write, no conflict — even though the
        // author is a stranger who could not otherwise touch the registry.
        let own = reseed_row("node-me", "mykey", "server", "trusted", "authority", true);
        assert_eq!(apply_sync_node(&tx, &own).unwrap(), 0);

        // Somebody else's row, restated exactly: same answer.
        let other = reseed_row("node-plain", "pk", "unknown", "untrusted", "standard", false);
        assert_eq!(apply_sync_node(&tx, &other).unwrap(), 0);

        // A reseed that actually disagrees about our own row still cannot state
        // our key or our trust, and still is not a conflict. Authored by an
        // operator, because a stranger would not get past the provenance gate at
        // all and this assertion is about the field rule, not about the author.
        seed_node(&tx, "node-op", true);
        let mut disagreeing =
            reseed_row("node-me", "theirkey", "laptop", "revoked", "ephemeral", true);
        disagreeing.body.actor_node_id = "node-op".to_string();
        disagreeing.body.hlc_timestamp.node_id = "node-op".to_string();
        apply_sync_node(&tx, &disagreeing).expect("dropped, not refused");
        let (key, trust, profile, operator, kind) = full_node_row(&tx, "node-me");
        assert_eq!(
            (key.as_str(), trust.as_str(), profile.as_str(), operator, kind.as_str()),
            ("mykey", "trusted", "authority", true, "laptop"),
            "only the organizational half of the reseed may land on our own row"
        );

        // A stranger still cannot change somebody else's row for real.
        let real_change = reseed_row("node-plain", "pk", "server", "trusted", "standard", true);
        assert!(matches!(
            apply_sync_node(&tx, &real_change),
            Err(SyncLedgerError::DeferredOrdering(_))
        ));
    }

    /// An `Insert` that does not name a column must not decide it.
    ///
    /// The arm is an upsert, and it used to substitute the column default for a
    /// field the operation omitted — so an `Insert` about a display name silently
    /// wrote `operator = 0`, `trust_status = 'untrusted'` and
    /// `sync_profile = 'standard'` over a row that said otherwise. Two further
    /// readings of the same field (the version slot and the floor) meanwhile
    /// agreed that such an operation says nothing about the flag, which is how
    /// one operation from the wire emptied the operator list without ever naming
    /// it.
    #[test]
    fn an_insert_decides_only_the_columns_it_names() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO sync_nodes (node_id, public_key, public_key_type, display_name, \
             node_kind, trust_status, sync_profile, operator) \
             VALUES ('node-op', 'pk', 'ed25519', 'Op', 'server', 'trusted', 'authority', 1)",
            [],
        )
        .unwrap();
        assert_eq!(operator_count(&tx).unwrap(), 1);

        // The only legal author with one operator left is that operator itself.
        let partial = node_operation(
            "node-op",
            "node-op",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("display_name", FieldValue::String("Renamed".into())),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &partial).unwrap(), 1);
        let (key, trust, profile, operator, kind) = full_node_row(&tx, "node-op");
        assert_eq!(
            (key.as_str(), trust.as_str(), profile.as_str(), operator, kind.as_str()),
            ("pk", "trusted", "authority", true, "server"),
            "an unnamed column must keep its value"
        );
        assert_eq!(
            operator_count(&tx).unwrap(),
            1,
            "an insert that never named the operator flag must not empty the list"
        );
        let renamed: String = tx
            .query_row(
                "SELECT display_name FROM sync_nodes WHERE node_id = 'node-op'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(renamed, "Renamed", "the column it DID name must land");

        // …and the same operation cannot walk past the version slot either: an
        // explicit promotion at 9000 stands against a later insert dated 100.
        // A second operator first, so the refusal below comes from the ORDER and
        // not from the floor — two rules, two tests.
        seed_node(&tx, "node-anchor", true);
        let promote = at_wall_time(
            node_operation(
                "node-op",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(true))]),
            ),
            9000,
        );
        apply_sync_node(&tx, &promote).unwrap();
        let stale = at_wall_time(
            node_operation(
                "node-op",
                "node-op",
                ActionType::Insert,
                field_map(&[
                    ("public_key", FieldValue::String("pk".into())),
                    ("operator", FieldValue::Bool(false)),
                ]),
            ),
            100,
        );
        assert_eq!(apply_sync_node(&tx, &stale).unwrap(), 0);
        assert!(full_node_row(&tx, "node-op").3, "a stale insert must lose the slot");

        // A genuinely new row still gets the column defaults.
        seed_trusted_node(&tx, "node-new");
        let fresh = node_operation(
            "node-new",
            "node-anchor",
            ActionType::Insert,
            field_map(&[("public_key", FieldValue::String("pk2".into()))]),
        );
        assert_eq!(apply_sync_node(&tx, &fresh).unwrap(), 1);
        let (_, trust, profile, operator, kind) = full_node_row(&tx, "node-new");
        assert_eq!(
            (trust.as_str(), profile.as_str(), operator, kind.as_str()),
            ("untrusted", "standard", false, "unknown")
        );
    }

    /// `owner_user_id` is nullable, so "the operation did not name it" and "the
    /// operation named it as null" are two different statements — and only the
    /// second one clears it.
    ///
    /// The `Update` arm has always told them apart; the `Insert` arm collapsed
    /// both into `None` and wiped the column on every operation that happened not
    /// to mention it. `reseed_core_state_from_current_rows` sends whole-row
    /// inserts, so the day this column gets a consumer the loss would arrive
    /// without a sound.
    #[test]
    fn an_insert_tells_an_unnamed_owner_from_an_owner_named_as_null() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-op", true);
        tx.execute(
            "INSERT INTO user_accounts (id, username, email, password_hash, role) \
             VALUES ('u1', 'u1', 'u1@example.test', 'x', 'user')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO sync_nodes (node_id, public_key, owner_user_id) \
             VALUES ('node-x', 'pk', 'u1')",
            [],
        )
        .unwrap();

        let owner = |tx: &rusqlite::Transaction<'_>| -> Option<String> {
            tx.query_row(
                "SELECT owner_user_id FROM sync_nodes WHERE node_id = 'node-x'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Names public_key and display_name, says nothing about the owner.
        let silent = node_operation(
            "node-x",
            "node-op",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("display_name", FieldValue::String("Renamed".into())),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &silent).unwrap(), 1);
        assert_eq!(
            owner(&tx),
            Some("u1".to_string()),
            "an insert that never named the owner must not clear it"
        );

        // Names it as null: that IS a statement, and it clears the column.
        let explicit = node_operation(
            "node-x",
            "node-op",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("owner_user_id", FieldValue::Null),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &explicit).unwrap(), 1);
        assert_eq!(owner(&tx), None, "an owner named as null must be cleared");

        // …and naming a value sets it.
        let named = node_operation(
            "node-x",
            "node-op",
            ActionType::Insert,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("owner_user_id", FieldValue::String("u1".into())),
            ]),
        );
        assert_eq!(apply_sync_node(&tx, &named).unwrap(), 1);
        assert_eq!(owner(&tx), Some("u1".to_string()));
    }

    /// A statement about the operator flag takes its place in the order even when
    /// it restates the value already held.
    ///
    /// Without it the order has holes: a demotion that arrives at a moment when
    /// the row is already demoted is dropped without a trace, and a promotion
    /// older than that demotion then wins and resurrects the authority.
    #[test]
    fn a_restated_operator_value_still_advances_the_order() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-x", true);
        seed_node(&tx, "node-op", true);
        seed_node(&tx, "node-op2", true);

        let demote = |author: &str, wall: i64| {
            at_wall_time(
                node_operation(
                    "node-x",
                    author,
                    ActionType::Update,
                    field_map(&[("operator", FieldValue::Bool(false))]),
                ),
                wall,
            )
        };
        assert_eq!(apply_sync_node(&tx, &demote("node-op", 1000)).unwrap(), 1);
        assert!(!full_node_row(&tx, "node-x").3);

        // Restates `operator = false` — writes nothing, but it is a real author
        // speaking at a real time, so the order moves to 3000.
        assert_eq!(apply_sync_node(&tx, &demote("node-op2", 3000)).unwrap(), 0);

        let promote = at_wall_time(
            node_operation(
                "node-x",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(true))]),
            ),
            2000,
        );
        assert_eq!(
            apply_sync_node(&tx, &promote).unwrap(),
            0,
            "a promotion older than the last statement must not resurrect the flag"
        );
        assert!(!full_node_row(&tx, "node-x").3);

        // An UNAUTHORIZED no-op must not move the order — otherwise a stranger
        // pins it with a clock from the future while "changing nothing".
        seed_node(&tx, "node-stranger", false);
        let stranger = demote("node-stranger", i64::MAX - 1);
        assert_eq!(apply_sync_node(&tx, &stranger).unwrap(), 0);
        let after = at_wall_time(
            node_operation(
                "node-x",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(true))]),
            ),
            4000,
        );
        assert_eq!(
            apply_sync_node(&tx, &after).unwrap(),
            1,
            "a stranger's no-op must not have taken the slot"
        );
    }

    /// Every organizational field reaches this node's own row.
    ///
    /// The classification is one constant and `apply_own_node_row` writes the
    /// same three columns in SQL — two spellings of one list, which is the shape
    /// that produced the first critical finding of this step at a larger scale.
    /// Driven by the constant, so a fourth entry that never reaches the statement
    /// fails here instead of disappearing silently.
    #[test]
    fn every_organizational_field_reaches_this_node_s_own_row() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_local_node_id(&tx, "node-me");
        seed_node(&tx, "node-me", false);
        seed_node(&tx, "node-op", true);

        for (index, field) in ORGANIZATIONAL_NODE_FIELDS.iter().enumerate() {
            let (value, expected) = match *field {
                "operator" => (FieldValue::Bool(true), "1".to_string()),
                "node_kind" => (FieldValue::String("desktop".into()), "desktop".to_string()),
                other => (
                    FieldValue::String(format!("value-{other}")),
                    format!("value-{other}"),
                ),
            };
            let op = at_wall_time(
                node_operation(
                    "node-me",
                    "node-op",
                    ActionType::Update,
                    field_map(&[(field, value)]),
                ),
                1000 + index as i64,
            );
            assert_eq!(
                apply_sync_node(&tx, &op).unwrap(),
                1,
                "organizational field '{field}' did not reach our own row"
            );
            let held: String = tx
                .query_row(
                    &format!(
                        "SELECT CAST({field} AS TEXT) FROM sync_nodes WHERE node_id = 'node-me'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                held, expected,
                "organizational field '{field}' is in the constant but not in the statement"
            );
        }
    }

    /// Every writable column is compared before an operation is called a no-op.
    ///
    /// `sync_node_operation_changes_nothing` names its columns one by one, and a
    /// column it forgets makes an operation that changes only that column look
    /// like a no-op — a silently dropped write rather than a refusal. Driven by
    /// the live schema so a new column cannot slip past the comparison.
    #[test]
    fn a_change_to_any_writable_column_is_not_a_no_op() {
        const NOT_WRITABLE: &[&str] = &["node_id", "last_seen_at", "created_at", "updated_at"];
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO sync_nodes (node_id, public_key, public_key_type, display_name, \
             node_kind, trust_status, sync_profile, operator) \
             VALUES ('node-x', 'pk', 'ed25519', 'Name', 'server', 'trusted', 'authority', 1)",
            [],
        )
        .unwrap();

        let columns: Vec<String> = {
            let mut stmt = tx.prepare("PRAGMA table_info(sync_nodes)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        for column in columns.iter().filter(|c| !NOT_WRITABLE.contains(&c.as_str())) {
            let different = match column.as_str() {
                "operator" => FieldValue::Bool(false),
                "public_key_type" => FieldValue::String("secp256k1".into()),
                "trust_status" => FieldValue::String("revoked".into()),
                "sync_profile" => FieldValue::String("limited".into()),
                "node_kind" => FieldValue::String("laptop".into()),
                "owner_user_id" => FieldValue::String("someone".into()),
                _ => FieldValue::String("something-else".into()),
            };
            let op = node_operation(
                "node-x",
                "node-op",
                ActionType::Update,
                field_map(&[(column.as_str(), different)]),
            );
            assert!(
                !sync_node_operation_changes_nothing(&tx, &op).unwrap(),
                "a change to '{column}' is not compared, so it would be dropped as a no-op"
            );
        }

        // …and restating the whole row IS a no-op, so the comparison is not
        // trivially answering "false" to everything.
        let same = node_operation(
            "node-x",
            "node-op",
            ActionType::Update,
            field_map(&[
                ("public_key", FieldValue::String("pk".into())),
                ("public_key_type", FieldValue::String("ed25519".into())),
                ("display_name", FieldValue::String("Name".into())),
                ("node_kind", FieldValue::String("server".into())),
                ("trust_status", FieldValue::String("trusted".into())),
                ("owner_user_id", FieldValue::Null),
                ("sync_profile", FieldValue::String("authority".into())),
                ("operator", FieldValue::Bool(true)),
            ]),
        );
        assert!(sync_node_operation_changes_nothing(&tx, &same).unwrap());
    }

    /// The wire may not empty the operator list. With zero operators no registry
    /// operation can ever be authorized again on any node, and the only way back
    /// is a person editing every node by hand.
    #[test]
    fn the_wire_may_not_remove_the_last_operator() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-op", true);
        seed_node(&tx, "node-other", true);

        // Two operators: one may go, by demotion or by deletion.
        let demote_other = at_wall_time(
            node_operation(
                "node-other",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(false))]),
            ),
            10,
        );
        assert_eq!(apply_sync_node(&tx, &demote_other).unwrap(), 1);
        assert_eq!(operator_count(&tx).unwrap(), 1);

        // One left: neither self-demotion nor deletion may take it.
        for op in [
            node_operation(
                "node-op",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(false))]),
            ),
            node_operation("node-op", "node-op", ActionType::Delete, BTreeMap::new()),
        ] {
            match apply_sync_node(&tx, &op) {
                Err(SyncLedgerError::DeferredOrdering(message)) => {
                    assert!(message.contains("last operator"), "message: {message}")
                }
                other => panic!("the last operator must not be removable: {other:?}"),
            }
        }
        assert_eq!(operator_count(&tx).unwrap(), 1);

        // A promotion arriving first makes the demotion legal again.
        let promote = at_wall_time(
            node_operation(
                "node-other",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(true))]),
            ),
            20,
        );
        assert_eq!(apply_sync_node(&tx, &promote).unwrap(), 1);
        let demote_self = at_wall_time(
            node_operation(
                "node-op",
                "node-op",
                ActionType::Update,
                field_map(&[("operator", FieldValue::Bool(false))]),
            ),
            30,
        );
        assert_eq!(apply_sync_node(&tx, &demote_self).unwrap(), 1);
        assert_eq!(operator_count(&tx).unwrap(), 1);
    }

    /// An assignment says which person a node acts for. Only operator nodes
    /// write it — including for themselves, which is the case a "the author is
    /// the subject" exception would have quietly allowed.
    #[test]
    fn node_user_assignments_are_written_only_by_operator_nodes() {
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&db).unwrap();
        let tx = conn.transaction().unwrap();
        seed_node(&tx, "node-a", false);
        seed_node(&tx, "node-op", true);
        tx.execute(
            "INSERT INTO user_accounts (id, username, email, password_hash, role) \
             VALUES ('u1', 'u1', 'u1@example.test', 'x', 'user')",
            [],
        )
        .unwrap();

        let fields = field_map(&[
            ("node_id", FieldValue::String("node-a".into())),
            ("user_id", FieldValue::String("u1".into())),
            ("assignment_mode", FieldValue::String("primary".into())),
        ]);
        let mut from_self = rollup_operation("node-a|u1|primary", fields.clone());
        from_self.body.resource_type = "core.node_user_assignment".to_string();
        from_self.body.table_name = "node_user_assignments".to_string();
        from_self.body.actor_node_id = "node-a".to_string();
        assert!(matches!(
            apply_node_user_assignment(&tx, &from_self),
            Err(SyncLedgerError::DeferredOrdering(_))
        ));
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM node_user_assignments", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "a non-operator author must write nothing");

        let mut from_operator = from_self.clone();
        from_operator.body.actor_node_id = "node-op".to_string();
        assert_eq!(
            apply_node_user_assignment(&tx, &from_operator).unwrap(),
            1
        );
    }

    /// Replicated matrix rows (grant / default / visibility) must materialize as
    /// upserts, replay idempotently, honor the composite-id binding, and — for
    /// visibility — skip a row whose user_groups parent is gone instead of
    /// tripping the FK and poisoning the drain.
    #[test]
    fn addon_permission_matrix_ops_round_trip_through_materializer() {
        let target = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let mut conn = repository::acquire_for_baseline(&target).unwrap();
        conn.execute(
            "INSERT INTO user_groups (id, name) VALUES ('g1', 'Team')",
            [],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();

        // Per-subject grant: deny lands, replay overwrites (not doubles).
        let mut grant_fields = BTreeMap::new();
        grant_fields.insert("addon_id".to_string(), FieldValue::String("bench-1".into()));
        grant_fields.insert("subject_type".to_string(), FieldValue::String("user".into()));
        grant_fields.insert("subject_id".to_string(), FieldValue::String("u1".into()));
        grant_fields.insert(
            "permission_id".to_string(),
            FieldValue::String("benchmark.write".into()),
        );
        grant_fields.insert("grant_mode".to_string(), FieldValue::String("deny".into()));
        let grant_op = matrix_operation(
            "core.addon_permission",
            "addon_permissions",
            "addon_id,subject_type,subject_id,permission_id",
            &["bench-1", "user", "u1", "benchmark.write"],
            ActionType::Update,
            grant_fields.clone(),
        );
        assert_eq!(apply_addon_permission(&tx, &grant_op).unwrap(), 1);
        assert_eq!(apply_addon_permission(&tx, &grant_op).unwrap(), 1);
        let (granted, mode): (i64, String) = tx
            .query_row(
                "SELECT granted, grant_mode FROM addon_permissions \
                 WHERE addon_id = 'bench-1' AND subject_id = 'u1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((granted, mode.as_str()), (0, "deny"));

        // An op whose fields encode a different rule than its LWW slot is rejected.
        let mut forged = grant_op.clone();
        forged.body.resource_id = crate::sync::resource_id::composite_resource_id(&[
            "bench-1",
            "user",
            "u2",
            "benchmark.write",
        ]);
        assert!(apply_addon_permission(&tx, &forged).is_err());

        // Default flips manifest allow -> admin deny; Delete removes the row.
        let mut default_fields = BTreeMap::new();
        default_fields.insert("addon_id".to_string(), FieldValue::String("bench-1".into()));
        default_fields.insert(
            "permission_id".to_string(),
            FieldValue::String("benchmark.read".into()),
        );
        default_fields.insert("grant_mode".to_string(), FieldValue::String("deny".into()));
        let default_op = matrix_operation(
            "core.addon_permission_default",
            "addon_permission_defaults",
            "addon_id,permission_id",
            &["bench-1", "benchmark.read"],
            ActionType::Update,
            default_fields.clone(),
        );
        assert_eq!(apply_addon_permission_default(&tx, &default_op).unwrap(), 1);
        let default_delete = matrix_operation(
            "core.addon_permission_default",
            "addon_permission_defaults",
            "addon_id,permission_id",
            &["bench-1", "benchmark.read"],
            ActionType::Delete,
            default_fields,
        );
        assert_eq!(apply_addon_permission_default(&tx, &default_delete).unwrap(), 1);

        // Visibility upserts for an existing group...
        let mut vis_fields = BTreeMap::new();
        vis_fields.insert("addon_id".to_string(), FieldValue::String("bench-1".into()));
        vis_fields.insert("group_id".to_string(), FieldValue::String("g1".into()));
        vis_fields.insert("visible".to_string(), FieldValue::Bool(false));
        let vis_op = matrix_operation(
            "core.addon_visibility",
            "addon_visibility",
            "addon_id,group_id",
            &["bench-1", "g1"],
            ActionType::Update,
            vis_fields,
        );
        assert_eq!(apply_addon_visibility(&tx, &vis_op).unwrap(), 1);
        let visible: i64 = tx
            .query_row(
                "SELECT visible FROM addon_visibility WHERE addon_id = 'bench-1' AND group_id = 'g1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(visible, 0);

        // ...and is a clean 0-row skip (not an FK error) for a missing group.
        let mut ghost_fields = BTreeMap::new();
        ghost_fields.insert("addon_id".to_string(), FieldValue::String("bench-1".into()));
        ghost_fields.insert("group_id".to_string(), FieldValue::String("ghost".into()));
        ghost_fields.insert("visible".to_string(), FieldValue::Bool(true));
        let ghost_op = matrix_operation(
            "core.addon_visibility",
            "addon_visibility",
            "addon_id,group_id",
            &["bench-1", "ghost"],
            ActionType::Update,
            ghost_fields,
        );
        assert_eq!(apply_addon_visibility(&tx, &ghost_op).unwrap(), 0);
        tx.commit().unwrap();
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
