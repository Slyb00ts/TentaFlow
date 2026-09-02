// ===== File: code_studio/sync_capture.rs — registry writes → Sync Ledger =====
//
// Registering a table in `sync/core_registry.rs` only declares that it MAY
// travel; a row reaches the outbox because a write captured it. This module is
// the single place that turns a Code Studio registry write into a core capture,
// so the `(resource_id, changed_fields)` shape a peer materializes can never
// drift between the create path, the mirror and the allowlist editor.
//
// Every capture must run inside the SAME transaction as the write it describes:
// the HLC is minted there, and a capture committed without its row (or a row
// committed without its capture) would replicate a state that never existed.
//
// The vault (`code_workspace_secrets`, `code_agent_credentials`) has no capture
// here and never will — its material is encrypted with the per-node
// SettingsCipher key (plan §5.2). It does not even share this database: the
// vault and the saga state live in the instance content DB (`code_studio::db`).

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use rusqlite::OptionalExtension;

use crate::sync::core_registry::CoreSyncResourceKind as Kind;
use crate::sync::ledger::FieldValue;
use crate::sync::resource_id::composite_resource_id;
use crate::sync::runtime::SqlWriteAction;

fn text(value: &str) -> FieldValue {
    FieldValue::String(value.to_string())
}

fn opt_text(value: Option<&str>) -> FieldValue {
    value.map(text).unwrap_or(FieldValue::Null)
}

fn opt_i64(value: Option<i64>) -> FieldValue {
    value.map(FieldValue::I64).unwrap_or(FieldValue::Null)
}

/// Ledger scope of every Code Studio capture. This is the PARTITION org, not
/// the workspace's own organisation: `ensure_default_core_sync_policies` seeds
/// its policies under the default org, and a capture minted under any other org
/// would find no policy, resolve to zero targets and never leave the outbox.
/// Flow Builder, skills, agents and RBAC are captured the same way. The row's
/// real `org_id` travels in the fields, and that is what the receiver writes.
const CAPTURE_ORG_ID: &str = crate::services::org::DEFAULT_ORG_ID;

/// Captures the CURRENT full row of a workspace as an Insert (the receiver
/// upserts it). Reading the row back instead of taking the caller's fields is
/// deliberate: status changes, branch discovery and quota edits all replicate
/// through one shape, so a peer never has to reconstruct a partial update.
pub fn capture_workspace(tx: &rusqlite::Transaction<'_>, workspace_id: &str) -> Result<()> {
    let row = tx
        .query_row(
            "SELECT org_id, owner_user_id, name, slug, node_id, exec_mode, container_image, \
                    egress_enforcement, repo_kind, repo_url, repo_auth_kind, secret_ref, \
                    ssh_host_fingerprint, default_branch, target_branch, autonomy_ceiling, \
                    egress_policy, index_enabled, quota_disk_bytes, quota_sessions, status, \
                    status_detail, created_at \
             FROM code_workspaces WHERE id = ?1",
            rusqlite::params![workspace_id],
            |row| {
                Ok(WorkspaceCaptureRow {
                    org_id: row.get(0)?,
                    owner_user_id: row.get(1)?,
                    name: row.get(2)?,
                    slug: row.get(3)?,
                    node_id: row.get(4)?,
                    exec_mode: row.get(5)?,
                    container_image: row.get(6)?,
                    egress_enforcement: row.get(7)?,
                    repo_kind: row.get(8)?,
                    repo_url: row.get(9)?,
                    repo_auth_kind: row.get(10)?,
                    secret_ref: row.get(11)?,
                    ssh_host_fingerprint: row.get(12)?,
                    default_branch: row.get(13)?,
                    target_branch: row.get(14)?,
                    autonomy_ceiling: row.get(15)?,
                    egress_policy: row.get(16)?,
                    index_enabled: row.get::<_, i64>(17)? != 0,
                    quota_disk_bytes: row.get(18)?,
                    quota_sessions: row.get(19)?,
                    status: row.get(20)?,
                    status_detail: row.get(21)?,
                    created_at: row.get(22)?,
                })
            },
        )
        .optional()
        .map_err(|e| anyhow!("code_studio sync capture: {e}"))?;
    let Some(row) = row else {
        return Ok(());
    };

    let mut fields = BTreeMap::new();
    fields.insert("org_id".to_string(), text(&row.org_id));
    fields.insert("owner_user_id".to_string(), text(&row.owner_user_id));
    fields.insert("name".to_string(), text(&row.name));
    fields.insert("slug".to_string(), text(&row.slug));
    // The owner node travels with the row: that is how every other node knows
    // it may render this workspace but not run it.
    fields.insert("node_id".to_string(), text(&row.node_id));
    fields.insert("exec_mode".to_string(), text(&row.exec_mode));
    fields.insert(
        "container_image".to_string(),
        opt_text(row.container_image.as_deref()),
    );
    fields.insert(
        "egress_enforcement".to_string(),
        text(&row.egress_enforcement),
    );
    fields.insert("repo_kind".to_string(), text(&row.repo_kind));
    fields.insert("repo_url".to_string(), opt_text(row.repo_url.as_deref()));
    fields.insert(
        "repo_auth_kind".to_string(),
        opt_text(row.repo_auth_kind.as_deref()),
    );
    // A HANDLE into the node-local vault, never the material behind it (§5.2).
    fields.insert(
        "secret_ref".to_string(),
        opt_text(row.secret_ref.as_deref()),
    );
    fields.insert(
        "ssh_host_fingerprint".to_string(),
        opt_text(row.ssh_host_fingerprint.as_deref()),
    );
    fields.insert(
        "default_branch".to_string(),
        opt_text(row.default_branch.as_deref()),
    );
    fields.insert(
        "target_branch".to_string(),
        opt_text(row.target_branch.as_deref()),
    );
    fields.insert("autonomy_ceiling".to_string(), text(&row.autonomy_ceiling));
    fields.insert("egress_policy".to_string(), text(&row.egress_policy));
    fields.insert(
        "index_enabled".to_string(),
        FieldValue::Bool(row.index_enabled),
    );
    // Quotas belong to the workspace, not to a node, so they replicate; they are
    // still ENFORCED on the owner node, which is the only one that opens
    // sessions or holds the tree.
    fields.insert(
        "quota_disk_bytes".to_string(),
        opt_i64(row.quota_disk_bytes),
    );
    fields.insert("quota_sessions".to_string(), opt_i64(row.quota_sessions));
    fields.insert("status".to_string(), text(&row.status));
    fields.insert(
        "status_detail".to_string(),
        opt_text(row.status_detail.as_deref()),
    );
    fields.insert("created_at".to_string(), text(&row.created_at));

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::CodeWorkspace,
        CAPTURE_ORG_ID,
        workspace_id.to_string(),
        SqlWriteAction::Insert,
        fields,
        // No actor is bound to the capture on purpose: migration 125 dropped the
        // Code Studio FKs to `user_accounts` (a synced row may name an account a
        // node has not materialized yet), but the capture journal keeps one, so
        // binding an unknown actor there would fail an otherwise legal write. The
        // acting user is already in `audit_log`, and `added_by` / `created_by` /
        // `granted_by` travel in the fields.
        None,
    )
}

struct WorkspaceCaptureRow {
    org_id: String,
    owner_user_id: String,
    name: String,
    slug: String,
    node_id: String,
    exec_mode: String,
    container_image: Option<String>,
    egress_enforcement: String,
    repo_kind: String,
    repo_url: Option<String>,
    repo_auth_kind: Option<String>,
    secret_ref: Option<String>,
    ssh_host_fingerprint: Option<String>,
    default_branch: Option<String>,
    target_branch: Option<String>,
    autonomy_ceiling: String,
    egress_policy: String,
    index_enabled: bool,
    quota_disk_bytes: Option<i64>,
    quota_sessions: Option<i64>,
    status: String,
    status_detail: Option<String>,
    created_at: String,
}

/// Captures a membership as it now stands. A row that is no longer there is
/// captured as a Delete tombstone, so a removal replicates instead of being
/// silently undone by the older grant still travelling somewhere in the mesh.
pub fn capture_member(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    user_id: &str,
) -> Result<()> {
    let resource_id = composite_resource_id(&[workspace_id, user_id]);
    let row = tx
        .query_row(
            "SELECT role, added_by, added_at FROM code_workspace_members \
             WHERE workspace_id = ?1 AND user_id = ?2",
            rusqlite::params![workspace_id, user_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| anyhow!("code_studio sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("workspace_id".to_string(), text(workspace_id));
    fields.insert("user_id".to_string(), text(user_id));
    let action = match &row {
        Some((role, added_by, added_at)) => {
            fields.insert("role".to_string(), text(role));
            // The project mirror recognises its own grants by `added_by`
            // (`project:<project_id>`), so it must survive the trip or the mirror
            // on another node would treat a mirrored member as a manual one.
            fields.insert("added_by".to_string(), text(added_by));
            fields.insert("added_at".to_string(), text(added_at));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::CodeWorkspaceMember,
        CAPTURE_ORG_ID,
        resource_id,
        action,
        fields,
        None,
    )
}

/// Captures a creator grant as it now stands (present → Insert, gone → Delete).
pub fn capture_creator_grant(
    tx: &rusqlite::Transaction<'_>,
    org_id: &str,
    user_id: &str,
) -> Result<()> {
    let resource_id = composite_resource_id(&[org_id, user_id]);
    let row = tx
        .query_row(
            "SELECT granted_by, created_at FROM code_workspace_creator_grants \
             WHERE org_id = ?1 AND user_id = ?2",
            rusqlite::params![org_id, user_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| anyhow!("code_studio sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("org_id".to_string(), text(org_id));
    fields.insert("user_id".to_string(), text(user_id));
    let action = match &row {
        Some((granted_by, created_at)) => {
            fields.insert("granted_by".to_string(), text(granted_by));
            fields.insert("created_at".to_string(), text(created_at));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::CodeWorkspaceCreatorGrant,
        CAPTURE_ORG_ID,
        resource_id,
        action,
        fields,
        None,
    )
}

/// Captures a workspace↔project link as it now stands.
pub fn capture_project_link(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    project_id: &str,
) -> Result<()> {
    let resource_id = composite_resource_id(&[workspace_id, project_id]);
    let row = tx
        .query_row(
            "SELECT linked_by, created_at FROM code_workspace_project_links \
             WHERE workspace_id = ?1 AND project_id = ?2",
            rusqlite::params![workspace_id, project_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| anyhow!("code_studio sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("workspace_id".to_string(), text(workspace_id));
    fields.insert("project_id".to_string(), text(project_id));
    let action = match &row {
        Some((linked_by, created_at)) => {
            fields.insert("linked_by".to_string(), text(linked_by));
            fields.insert("created_at".to_string(), text(created_at));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::CodeWorkspaceProjectLink,
        CAPTURE_ORG_ID,
        resource_id,
        action,
        fields,
        None,
    )
}

/// Captures a standing capability grant as it now stands. The identity is the
/// UNIQUE triple; the table's AUTOINCREMENT `id` is node-local and stays behind.
pub fn capture_allowlist_entry(
    tx: &rusqlite::Transaction<'_>,
    workspace_id: &str,
    capability: &str,
    pattern: &str,
) -> Result<()> {
    let resource_id = composite_resource_id(&[workspace_id, capability, pattern]);
    let row = tx
        .query_row(
            "SELECT created_by, created_at FROM code_workspace_allowlist \
             WHERE workspace_id = ?1 AND capability = ?2 AND pattern = ?3",
            rusqlite::params![workspace_id, capability, pattern],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| anyhow!("code_studio sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("workspace_id".to_string(), text(workspace_id));
    fields.insert("capability".to_string(), text(capability));
    fields.insert("pattern".to_string(), text(pattern));
    let action = match &row {
        Some((created_by, created_at)) => {
            fields.insert("created_by".to_string(), text(created_by));
            fields.insert("created_at".to_string(), text(created_at));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::CodeWorkspaceAllowlist,
        CAPTURE_ORG_ID,
        resource_id,
        action,
        fields,
        None,
    )
}
