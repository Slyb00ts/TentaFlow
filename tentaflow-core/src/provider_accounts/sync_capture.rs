// ===== File: provider_accounts/sync_capture.rs — account writes → Sync Ledger =====
//
// Registering a table in `sync/core_registry.rs` only declares that it MAY
// travel; a row reaches the outbox because a write captured it. This module is
// the single place that turns a provider-account write into a core capture, so
// the `(resource_id, changed_fields)` shape a peer materializes cannot drift
// between the create path, the grant editor and the node matrix.
//
// Every capture runs inside the SAME transaction as the write it describes:
// the HLC is minted there, and a capture committed without its row (or a row
// committed without its capture) would replicate a state that never existed.
//
// `provider_account_credentials` has NO capture here yet, and the omission is
// the feature's security boundary rather than an oversight: the material is
// sealed with the PER-NODE `SettingsCipher` key, so it can only travel
// decrypted with the receiver re-encrypting (the `SharedSettingSecret` model),
// and it must reach only the nodes flagged `receives_accounts`. Registering it
// half-done would replicate a secret to every trusted node. `provider_account_sessions`
// and `provider_account_node_state` never travel at all: the first is runtime
// state like `flow_executions`, the second is what one node measured about its
// own copy.

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

/// Ledger scope of every capture here. This is the PARTITION org, not the
/// account's own organisation: `ensure_default_core_sync_policies` seeds its
/// policies under the default org, and a capture minted under any other org
/// would find no policy, resolve to zero targets and never leave the outbox.
/// The row's real `org_id` travels in the fields, and that is what the receiver
/// writes. Same decision as Code Studio and Flow Builder.
const CAPTURE_ORG_ID: &str = crate::services::org::DEFAULT_ORG_ID;

struct AccountCaptureRow {
    org_id: String,
    engine_id: String,
    display_name: String,
    scope: String,
    owner_user_id: Option<String>,
    credential_kind: String,
    provider_subject: Option<String>,
    plan_label: Option<String>,
    home_node_id: Option<String>,
    home_lost_node_id: Option<String>,
    status: String,
    created_by: String,
    created_at: String,
    credential_revoked_revision: i64,
    max_sessions: i64,
}

/// Captures the account as it now stands: present → a full-row Insert (the
/// receiver upserts it), gone → a Delete tombstone. Reading the row back
/// instead of taking the caller's fields is deliberate — a status change, a
/// rename and a login all replicate through ONE shape, so a peer never has to
/// reconstruct a partial update.
pub fn capture_account(tx: &rusqlite::Transaction<'_>, account_id: &str) -> Result<()> {
    let row = tx
        .query_row(
            "SELECT org_id, engine_id, display_name, scope, owner_user_id, credential_kind, \
                    provider_subject, plan_label, home_node_id, home_lost_node_id, status, \
                    created_by, created_at, credential_revoked_revision, max_sessions \
             FROM provider_accounts WHERE account_id = ?1",
            rusqlite::params![account_id],
            |row| {
                Ok(AccountCaptureRow {
                    org_id: row.get(0)?,
                    engine_id: row.get(1)?,
                    display_name: row.get(2)?,
                    scope: row.get(3)?,
                    owner_user_id: row.get(4)?,
                    credential_kind: row.get(5)?,
                    provider_subject: row.get(6)?,
                    plan_label: row.get(7)?,
                    home_node_id: row.get(8)?,
                    home_lost_node_id: row.get(9)?,
                    status: row.get(10)?,
                    created_by: row.get(11)?,
                    created_at: row.get(12)?,
                    credential_revoked_revision: row.get(13)?,
                    max_sessions: row.get(14)?,
                })
            },
        )
        .optional()
        .map_err(|e| anyhow!("provider account sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    let action = match &row {
        Some(row) => {
            fields.insert("org_id".to_string(), text(&row.org_id));
            fields.insert("engine_id".to_string(), text(&row.engine_id));
            fields.insert("display_name".to_string(), text(&row.display_name));
            fields.insert("scope".to_string(), text(&row.scope));
            fields.insert(
                "owner_user_id".to_string(),
                opt_text(row.owner_user_id.as_deref()),
            );
            fields.insert("credential_kind".to_string(), text(&row.credential_kind));
            // The provider identity travels because it is what makes two
            // accounts of the same engine distinguishable — and because the
            // materializer REFUSES a value that differs from a stored non-NULL
            // one (§C.4): a different identity under the same account id is a
            // mistake, never a rename.
            fields.insert(
                "provider_subject".to_string(),
                opt_text(row.provider_subject.as_deref()),
            );
            fields.insert(
                "plan_label".to_string(),
                opt_text(row.plan_label.as_deref()),
            );
            // Which node refreshes the credential is an account-wide fact, so
            // it replicates; whether a node HOLDS the credential is per-node
            // truth and stays in `provider_account_node_state`.
            fields.insert(
                "home_node_id".to_string(),
                opt_text(row.home_node_id.as_deref()),
            );
            // WHICH node's deletion took the home away, when the home is gone.
            // It travels for the reason the marker exists at all: a peer that
            // only learns "this account has no home" cannot tell an account
            // that was never homed from one that is still running its sessions
            // on a machine no longer in the registry.
            fields.insert(
                "home_lost_node_id".to_string(),
                opt_text(row.home_lost_node_id.as_deref()),
            );
            fields.insert("status".to_string(), text(&row.status));
            fields.insert("created_by".to_string(), text(&row.created_by));
            fields.insert("created_at".to_string(), text(&row.created_at));
            // The material never travels on the ledger, so this mark is how a
            // node holding a copy of a CLEARED credential learns to drop it.
            // The receiver takes the HIGHER of the two — a revocation is not
            // undone by an older row arriving late.
            fields.insert(
                "credential_revoked_revision".to_string(),
                FieldValue::I64(row.credential_revoked_revision),
            );
            // The limit is an administrator's decision about the ACCOUNT, taken
            // once from any node, so it replicates with the row. What it is
            // compared against — the open sessions — does not: those are this
            // node's runtime state.
            fields.insert(
                "max_sessions".to_string(),
                FieldValue::I64(row.max_sessions),
            );
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::ProviderAccount,
        CAPTURE_ORG_ID,
        account_id.to_string(),
        action,
        fields,
        // No actor is bound to the capture: these tables carry no FK to
        // `user_accounts` (see migration 156), so binding an actor the capture
        // journal cannot resolve would fail an otherwise legal write. Who acted
        // is in `audit_log`, and `created_by` / `granted_by` travel in the
        // fields. Same decision as Code Studio.
        None,
    )?;
    Ok(())
}

/// Captures a grant as it now stands. A row that is gone is captured as a
/// Delete tombstone, so a revocation replicates instead of being silently
/// undone by the older grant still travelling somewhere in the mesh.
pub fn capture_grant(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    subject_type: &str,
    subject_id: &str,
) -> Result<()> {
    let resource_id = composite_resource_id(&[account_id, subject_type, subject_id]);
    let row = tx
        .query_row(
            "SELECT granted_by, granted_at FROM provider_account_grants \
             WHERE account_id = ?1 AND subject_type = ?2 AND subject_id = ?3",
            rusqlite::params![account_id, subject_type, subject_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| anyhow!("provider account sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("account_id".to_string(), text(account_id));
    fields.insert("subject_type".to_string(), text(subject_type));
    fields.insert("subject_id".to_string(), text(subject_id));
    let action = match &row {
        Some((granted_by, granted_at)) => {
            fields.insert("granted_by".to_string(), text(granted_by));
            fields.insert("granted_at".to_string(), text(granted_at));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::ProviderAccountGrant,
        CAPTURE_ORG_ID,
        resource_id,
        action,
        fields,
        None,
    )?;
    Ok(())
}

/// Captures a runtime node as it now stands. The flag is an administrator's
/// fleet-wide decision made from any node, which is why it replicates at all —
/// what a node MEASURED about itself does not.
pub fn capture_runtime_node(tx: &rusqlite::Transaction<'_>, node_id: &str) -> Result<()> {
    let row = tx
        .query_row(
            "SELECT receives_accounts, updated_by, updated_at FROM agent_runtime_nodes \
             WHERE node_id = ?1",
            rusqlite::params![node_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? != 0,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| anyhow!("provider account sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("node_id".to_string(), text(node_id));
    let action = match &row {
        Some((receives_accounts, updated_by, updated_at)) => {
            fields.insert(
                "receives_accounts".to_string(),
                FieldValue::Bool(*receives_accounts),
            );
            fields.insert("updated_by".to_string(), opt_text(updated_by.as_deref()));
            fields.insert("updated_at".to_string(), text(updated_at));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::AgentRuntimeNode,
        CAPTURE_ORG_ID,
        node_id.to_string(),
        action,
        fields,
        None,
    )?;
    Ok(())
}

/// Captures one cell of the node matrix. `install_state` is per-node truth
/// reported by the node that owns the row — it replicates so N01 is a fleet
/// view, and only the owning node writes its own rows (enforced in the handler,
/// which is where the node's identity is known, not in the materializer).
pub fn capture_runtime_engine(
    tx: &rusqlite::Transaction<'_>,
    node_id: &str,
    engine_id: &str,
) -> Result<()> {
    let resource_id = composite_resource_id(&[node_id, engine_id]);
    let row = tx
        .query_row(
            "SELECT install_state, version, installed_at, last_error FROM agent_runtime_engines \
             WHERE node_id = ?1 AND engine_id = ?2",
            rusqlite::params![node_id, engine_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| anyhow!("provider account sync capture: {e}"))?;

    let mut fields = BTreeMap::new();
    fields.insert("node_id".to_string(), text(node_id));
    fields.insert("engine_id".to_string(), text(engine_id));
    let action = match &row {
        Some((install_state, version, installed_at, last_error)) => {
            fields.insert("install_state".to_string(), text(install_state));
            fields.insert("version".to_string(), opt_text(version.as_deref()));
            fields.insert(
                "installed_at".to_string(),
                opt_text(installed_at.as_deref()),
            );
            fields.insert("last_error".to_string(), opt_text(last_error.as_deref()));
            SqlWriteAction::Insert
        }
        None => SqlWriteAction::Delete,
    };

    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        Kind::AgentRuntimeEngine,
        CAPTURE_ORG_ID,
        resource_id,
        action,
        fields,
        None,
    )?;
    Ok(())
}
