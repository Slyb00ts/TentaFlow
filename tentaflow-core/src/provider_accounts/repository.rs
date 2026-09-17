// ===== File: provider_accounts/repository.rs — CRUD over the account registry =====
//
// The registry lives in the main database (migration 156) and is carried by the
// Sync Ledger, so an account added on the desktop is usable from the phone.
//
// Four invariants this module enforces rather than documents:
//   1. every write that touches a REPLICATED row captures it in the SAME
//      transaction — a committed row without its capture replicates a state
//      that never existed;
//   2. `provider_subject` is immutable once set: a different provider identity
//      under the same account id is a mistake, never a rename;
//   3. a credential write is a revision CAS, never a blind overwrite — two
//      writers minting one revision must not silently pick a winner, because
//      the loser's token is the one the provider already rotated;
//   4. a `scope='user'` account answers to its owner and to nobody else. There
//      is no administrator bypass, and there is no grant table row that could
//      create one.

use std::collections::{BTreeSet, HashMap};

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

use super::sync_capture;
use super::{
    AccountFilter, AccountRecord, AccountUpdate, CredentialMeta, CredentialSummary,
    CredentialWrite, GrantInput, GrantRecord, NewAccount, NodeStateRecord, RuntimeEngineRecord,
    RuntimeNodeRecord, SessionRecord, ACCOUNT_SCOPES, ACCOUNT_STATUSES, CREDENTIAL_KINDS,
    GRANT_SUBJECT_TYPES, INSTALL_STATES, RUNTIME_STATES,
};
use crate::crypto::SettingsCipher;
use crate::db::DbPool;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("provider account db read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("provider account db write: {e}")
}

const ACCOUNT_COLS: &str = "account_id, org_id, engine_id, display_name, scope, owner_user_id, \
     credential_kind, provider_subject, plan_label, home_node_id, status, created_by, created_at, \
     updated_at";

fn read_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccountRecord> {
    Ok(AccountRecord {
        account_id: row.get(0)?,
        org_id: row.get(1)?,
        engine_id: row.get(2)?,
        display_name: row.get(3)?,
        scope: row.get(4)?,
        owner_user_id: row.get(5)?,
        credential_kind: row.get(6)?,
        provider_subject: row.get(7)?,
        plan_label: row.get(8)?,
        home_node_id: row.get(9)?,
        status: row.get(10)?,
        created_by: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn sha256_hex(material: &str) -> String {
    hex::encode(Sha256::digest(material.as_bytes()))
}

/// Identifies WHICH credential is stored without being able to reconstruct it —
/// the only way an auditor can tell a rotation from a rewrite of the same key.
/// Labelled with its algorithm so nobody mistakes it for a provider's own key
/// fingerprint, exactly as `code_studio::vault::fingerprint_of` does.
fn credential_fingerprint(material_sha256: &str) -> String {
    format!("sha256:{material_sha256}")
}

/// Which node this installation is, read from the row `sync::runtime::init`
/// writes. Taken from the database rather than the process-global runtime for
/// the reason the materializer states: a global is invisible to tests, so a
/// rule written against it silently evaporates there.
fn local_node_id_tx(tx: &rusqlite::Transaction<'_>) -> Result<Option<String>> {
    tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![crate::db::repository::LOCAL_NODE_ID_SETTING],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(read_err)
}

/// One audit row, written INSIDE the mutation's transaction.
///
/// The capture journal deliberately binds no actor (migration 156 carries no FK
/// to `user_accounts`, so a capture naming one could fail an otherwise legal
/// write), which makes `audit_log` the ONLY record of who acted on an account.
/// Joining the write's transaction is what keeps that record honest in both
/// directions: a rolled-back mutation leaves no entry claiming it happened, and
/// a committed one can never be missing its entry.
fn audit_tx(
    tx: &rusqlite::Transaction<'_>,
    actor: Option<&str>,
    action: &str,
    resource: &str,
    details: serde_json::Value,
) -> Result<()> {
    let node_id = local_node_id_tx(tx)?;
    crate::db::repository::log_audit_tx(
        tx,
        actor,
        None,
        action,
        Some(resource),
        Some(&details.to_string()),
        None,
        node_id.as_deref(),
    )
}

// =============================================================================
// Accounts
// =============================================================================

/// Validates what the column CHECKs cannot: that the engine exists in the
/// catalog, that the credential kind is one the engine supports, and that the
/// owner and the scope agree.
pub fn validate_new_account(new: &NewAccount) -> Result<()> {
    let engine = super::engine(&new.engine_id)
        .ok_or_else(|| anyhow!("unknown agent engine '{}'", new.engine_id))?;
    if !ACCOUNT_SCOPES.contains(&new.scope.as_str()) {
        return Err(anyhow!("account scope must be one of {ACCOUNT_SCOPES:?}"));
    }
    if !CREDENTIAL_KINDS.contains(&new.credential_kind.as_str()) {
        return Err(anyhow!(
            "account credential_kind must be one of {CREDENTIAL_KINDS:?}"
        ));
    }
    match new.credential_kind.as_str() {
        "api_key" if !engine.supports_api_key => {
            return Err(anyhow!(
                "engine '{}' authenticates through its own login only, not an API key",
                new.engine_id
            ));
        }
        "provider_login" if !engine.supports_login => {
            return Err(anyhow!("engine '{}' has no login flow", new.engine_id));
        }
        _ => {}
    }
    if new.display_name.trim().is_empty() {
        return Err(anyhow!("account display_name is required"));
    }
    // Mirrors the table CHECK, so the caller gets a sentence instead of
    // "CHECK constraint failed".
    if (new.scope == "user") != new.owner_user_id.is_some() {
        return Err(anyhow!(
            "a user account needs an owner and a global account must not have one"
        ));
    }
    Ok(())
}

/// Inserts an account and captures it for the ledger in one transaction.
pub fn create_account(db: &DbPool, new: &NewAccount) -> Result<AccountRecord> {
    validate_new_account(new)?;
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    tx.execute(
        "INSERT INTO provider_accounts \
           (account_id, org_id, engine_id, display_name, scope, owner_user_id, credential_kind, \
            status, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8)",
        params![
            new.account_id,
            new.org_id,
            new.engine_id,
            new.display_name,
            new.scope,
            new.owner_user_id,
            new.credential_kind,
            new.created_by,
        ],
    )
    .map_err(write_err)?;
    sync_capture::capture_account(&tx, &new.account_id)?;
    audit_tx(
        &tx,
        Some(&new.created_by),
        "provider_account.create",
        &new.account_id,
        serde_json::json!({
            "engine_id": new.engine_id,
            "scope": new.scope,
            "credential_kind": new.credential_kind,
            "owner_user_id": new.owner_user_id,
        }),
    )?;
    tx.commit().map_err(write_err)?;
    drop(conn);

    get_account(db, &new.account_id)?
        .ok_or_else(|| anyhow!("provider account vanished right after insert"))
}

pub fn get_account(db: &DbPool, account_id: &str) -> Result<Option<AccountRecord>> {
    let conn = db.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {ACCOUNT_COLS} FROM provider_accounts WHERE account_id = ?1"),
        params![account_id],
        read_account,
    )
    .optional()
    .map_err(read_err)
}

/// Every account of the org, filtered as A01 filters. Ordered by engine then
/// name so two nodes render the same list.
pub fn list_accounts(
    db: &DbPool,
    org_id: &str,
    filter: &AccountFilter,
) -> Result<Vec<AccountRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut sql = format!("SELECT {ACCOUNT_COLS} FROM provider_accounts WHERE org_id = ?1");
    let mut binds: Vec<String> = vec![org_id.to_string()];
    if let Some(engine_id) = filter.engine_id.as_deref() {
        binds.push(engine_id.to_string());
        sql.push_str(&format!(" AND engine_id = ?{}", binds.len()));
    }
    if let Some(scope) = filter.scope.as_deref() {
        binds.push(scope.to_string());
        sql.push_str(&format!(" AND scope = ?{}", binds.len()));
    }
    if let Some(query) = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        binds.push(format!("%{}%", query.to_lowercase()));
        sql.push_str(&format!(
            " AND (LOWER(display_name) LIKE ?{0} OR LOWER(COALESCE(provider_subject,'')) LIKE ?{0})",
            binds.len()
        ));
    }
    sql.push_str(" ORDER BY engine_id, display_name, account_id");
    let mut stmt = conn.prepare(&sql).map_err(read_err)?;
    let bind_refs: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(&bind_refs[..], read_account)
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// Applies the fields an administrator may edit. Returns false when the account
/// does not exist; an update that changes nothing still refreshes `updated_at`,
/// which is what tells the UI the row was touched.
pub fn update_account(
    db: &DbPool,
    account_id: &str,
    update: &AccountUpdate,
    actor: Option<&str>,
) -> Result<bool> {
    if let Some(status) = update.status.as_deref() {
        if !ACCOUNT_STATUSES.contains(&status) {
            return Err(anyhow!(
                "account status must be one of {ACCOUNT_STATUSES:?}"
            ));
        }
    }
    if let Some(name) = update.display_name.as_deref() {
        if name.trim().is_empty() {
            return Err(anyhow!("account display_name is required"));
        }
    }
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let changed = tx
        .execute(
            "UPDATE provider_accounts SET \
               display_name = COALESCE(?2, display_name), \
               status = COALESCE(?3, status), \
               plan_label = COALESCE(?4, plan_label), \
               home_node_id = COALESCE(?5, home_node_id), \
               updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
             WHERE account_id = ?1",
            params![
                account_id,
                update.display_name,
                update.status,
                update.plan_label,
                update.home_node_id,
            ],
        )
        .map_err(write_err)?;
    if changed > 0 {
        sync_capture::capture_account(&tx, account_id)?;
        audit_tx(
            &tx,
            actor,
            "provider_account.update",
            account_id,
            serde_json::json!({
                "display_name": update.display_name,
                "status": update.status,
                "plan_label": update.plan_label,
                "home_node_id": update.home_node_id,
            }),
        )?;
    }
    tx.commit().map_err(write_err)?;
    Ok(changed > 0)
}

/// Records the provider identity learned at a successful login.
///
/// Immutable once set: a value that differs from a stored non-NULL one is
/// refused. Two different provider identities under one account id mean the
/// account was logged into twice with different credentials, and silently
/// taking the newer one would leave every grant, session and agent pointing at
/// somebody else's subscription.
pub fn set_provider_subject(db: &DbPool, account_id: &str, subject: &str) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    apply_provider_subject_tx(&tx, account_id, subject)?;
    sync_capture::capture_account(&tx, account_id)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

fn apply_provider_subject_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    subject: &str,
) -> Result<()> {
    let stored: Option<Option<String>> = tx
        .query_row(
            "SELECT provider_subject FROM provider_accounts WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    let stored = stored.ok_or_else(|| anyhow!("provider account '{account_id}' does not exist"))?;
    match stored {
        Some(existing) if existing != subject => Err(anyhow!(
            "provider account '{account_id}' is already bound to a different provider identity"
        )),
        Some(_) => Ok(()),
        None => {
            tx.execute(
                "UPDATE provider_accounts SET provider_subject = ?2, \
                   updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE account_id = ?1",
                params![account_id, subject],
            )
            .map_err(write_err)?;
            Ok(())
        }
    }
}

/// Deletes an account. The FK cascade takes the grants, credential, sessions
/// and node state with it; each replicated child is captured as a tombstone
/// BEFORE the parent goes, so a peer removes the same set instead of keeping
/// orphaned grants.
pub fn delete_account(db: &DbPool, account_id: &str, actor: Option<&str>) -> Result<bool> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let removed = delete_account_tx(&tx, account_id, actor)?;
    tx.commit().map_err(write_err)?;
    Ok(removed)
}

/// The deletion itself, joinable to a caller's transaction so a user account
/// and the provider accounts that answer to it can go in ONE commit.
fn delete_account_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
    actor: Option<&str>,
) -> Result<bool> {
    let grants = grant_keys_tx(tx, account_id)?;
    let removed = tx
        .execute(
            "DELETE FROM provider_accounts WHERE account_id = ?1",
            params![account_id],
        )
        .map_err(write_err)?;
    if removed > 0 {
        for (subject_type, subject_id) in &grants {
            sync_capture::capture_grant(tx, account_id, subject_type, subject_id)?;
        }
        sync_capture::capture_account(tx, account_id)?;
        audit_tx(
            tx,
            actor,
            "provider_account.delete",
            account_id,
            serde_json::json!({ "grants_removed": grants.len() }),
        )?;
    }
    Ok(removed > 0)
}

// =============================================================================
// Lifecycle of the entities an account POINTS AT
//
// Migration 156 declares no foreign key to `user_accounts`, `user_groups` or
// `sync_nodes`: a replicated row can name an entity this node has not
// materialized yet, and a FK would refuse it (the v125 code_studio precedent).
// The cleanup a FK would have done therefore has to be performed explicitly,
// and it has to run in the SAME transaction as the removal that triggers it —
// a user deleted without their accounts leaves a credential nobody owns, and a
// commit that separates the two can be interrupted between them.
// =============================================================================

/// Removes the personal accounts of a user who is being deleted, plus every
/// grant naming them. The account's own children (credential, sessions, node
/// state, grants) go by the ON DELETE CASCADE of migration 156.
pub fn forget_user_tx(
    tx: &rusqlite::Transaction<'_>,
    user_id: &str,
    actor: Option<&str>,
) -> Result<usize> {
    let owned: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT account_id FROM provider_accounts \
                 WHERE scope = 'user' AND owner_user_id = ?1",
            )
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![user_id], |row| row.get::<_, String>(0))
            .map_err(read_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(read_err)?;
        rows
    };
    for account_id in &owned {
        delete_account_tx(tx, account_id, actor)?;
    }
    forget_grant_subject_tx(tx, "user", user_id)?;
    Ok(owned.len())
}

/// Removes every grant naming one subject — the cleanup a deleted user or group
/// would otherwise leave behind as an access rule pointing at nothing.
pub fn forget_grant_subject_tx(
    tx: &rusqlite::Transaction<'_>,
    subject_type: &str,
    subject_id: &str,
) -> Result<usize> {
    let accounts: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT account_id FROM provider_account_grants \
                 WHERE subject_type = ?1 AND subject_id = ?2",
            )
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![subject_type, subject_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(read_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(read_err)?;
        rows
    };
    for account_id in &accounts {
        tx.execute(
            "DELETE FROM provider_account_grants \
             WHERE account_id = ?1 AND subject_type = ?2 AND subject_id = ?3",
            params![account_id, subject_type, subject_id],
        )
        .map_err(write_err)?;
        sync_capture::capture_grant(tx, account_id, subject_type, subject_id)?;
    }
    Ok(accounts.len())
}

/// Removes what a node being deleted from the registry leaves behind: its row
/// in the agent-runtime matrix (engines cascade, and each is captured as its own
/// tombstone first), its per-account materialization state, and the `home_node_id`
/// of every account that pointed at it — an account whose home node is gone is
/// an account with no home, not an account homed on a node that does not exist.
pub fn forget_node_tx(tx: &rusqlite::Transaction<'_>, node_id: &str) -> Result<()> {
    let engines: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT engine_id FROM agent_runtime_engines WHERE node_id = ?1")
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![node_id], |row| row.get::<_, String>(0))
            .map_err(read_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(read_err)?;
        rows
    };
    let removed = tx
        .execute(
            "DELETE FROM agent_runtime_nodes WHERE node_id = ?1",
            params![node_id],
        )
        .map_err(write_err)?;
    if removed > 0 {
        for engine_id in &engines {
            sync_capture::capture_runtime_engine(tx, node_id, engine_id)?;
        }
        sync_capture::capture_runtime_node(tx, node_id)?;
    }
    // Node-local measurement of a node that is gone: no capture, because it
    // never travelled in the first place.
    tx.execute(
        "DELETE FROM provider_account_node_state WHERE node_id = ?1",
        params![node_id],
    )
    .map_err(write_err)?;

    let homed: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT account_id FROM provider_accounts WHERE home_node_id = ?1")
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![node_id], |row| row.get::<_, String>(0))
            .map_err(read_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(read_err)?;
        rows
    };
    for account_id in &homed {
        tx.execute(
            "UPDATE provider_accounts SET home_node_id = NULL, \
               updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE account_id = ?1",
            params![account_id],
        )
        .map_err(write_err)?;
        sync_capture::capture_account(tx, account_id)?;
    }
    Ok(())
}

// =============================================================================
// Grants
// =============================================================================

fn grant_keys_tx(
    tx: &rusqlite::Transaction<'_>,
    account_id: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = tx
        .prepare(
            "SELECT subject_type, subject_id FROM provider_account_grants WHERE account_id = ?1",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![account_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

pub fn list_grants(db: &DbPool, account_id: &str) -> Result<Vec<GrantRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT account_id, subject_type, subject_id, granted_by, granted_at \
             FROM provider_account_grants WHERE account_id = ?1 \
             ORDER BY subject_type, subject_id",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![account_id], |row| {
            Ok(GrantRecord {
                account_id: row.get(0)?,
                subject_type: row.get(1)?,
                subject_id: row.get(2)?,
                granted_by: row.get(3)?,
                granted_at: row.get(4)?,
            })
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// `account_id -> number of grant rows` for the accounts asked about. An
/// account without a grant is absent rather than zero, so a caller must treat a
/// missing key as "nobody was given access" and not as "unknown".
pub fn grant_counts(db: &DbPool, account_ids: &[String]) -> Result<HashMap<String, u32>> {
    let conn = db.read().map_err(read_err)?;
    count_by_account(
        &conn,
        account_ids,
        "SELECT account_id, COUNT(*) FROM provider_account_grants",
    )
}

/// Counts the rows of one child table per account, restricted to the accounts
/// the caller is about to render. Scoping matters because the single-account
/// window asks the same question as the whole list does: an unrestricted
/// `GROUP BY` would aggregate the installation's entire history to decorate one
/// row.
fn count_by_account(
    conn: &rusqlite::Connection,
    account_ids: &[String],
    select: &str,
) -> Result<HashMap<String, u32>> {
    let mut counts = HashMap::with_capacity(account_ids.len());
    crate::db::repository::lookup_in_chunks(
        conn,
        account_ids,
        |placeholders| format!("{select} WHERE account_id IN ({placeholders}) GROUP BY account_id"),
        |row| {
            let account_id: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            counts.insert(account_id, u32::try_from(count).unwrap_or(u32::MAX));
            Ok(())
        },
    )
    .map_err(read_err)?;
    Ok(counts)
}

/// Replaces the account's grants with exactly `desired`.
///
/// The replacement is a DIFF, not a truncate-and-reinsert: only the rows that
/// actually change are written and captured, so an untouched grant keeps its
/// `granted_by`/`granted_at` and does not replicate a spurious edit, while a
/// removed one travels as its own Delete tombstone. A truncate would emit a
/// tombstone plus an Insert for every row on every save, and a peer replaying
/// them out of order would briefly lose access it never lost.
pub fn set_grants(
    db: &DbPool,
    account_id: &str,
    desired: &[GrantInput],
    granted_by: &str,
) -> Result<Vec<GrantRecord>> {
    let mut wanted: BTreeSet<(String, String)> = BTreeSet::new();
    for grant in desired {
        if !GRANT_SUBJECT_TYPES.contains(&grant.subject_type.as_str()) {
            return Err(anyhow!(
                "grant subject_type must be one of {GRANT_SUBJECT_TYPES:?}"
            ));
        }
        // 'org' means the whole organisation, so it has no subject of its own
        // and the empty string is its only legal id — otherwise two spellings
        // of "everybody" would be two different primary keys.
        let subject_id = if grant.subject_type == "org" {
            String::new()
        } else {
            if grant.subject_id.trim().is_empty() {
                return Err(anyhow!("a {} grant needs a subject", grant.subject_type));
            }
            grant.subject_id.clone()
        };
        wanted.insert((grant.subject_type.clone(), subject_id));
    }

    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let account: Option<(String, String)> = tx
        .query_row(
            "SELECT scope, org_id FROM provider_accounts WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(read_err)?;
    let (scope, org_id) =
        account.ok_or_else(|| anyhow!("provider account '{account_id}' does not exist"))?;
    if scope != "global" {
        // Plan §2.1: a user account is usable by its owner alone. A grant row
        // on one would be an access path the owner never agreed to.
        return Err(anyhow!("only a global account can be shared"));
    }
    for (subject_type, subject_id) in &wanted {
        validate_grant_subject_tx(&tx, &org_id, subject_type, subject_id)?;
    }

    let current: BTreeSet<(String, String)> = grant_keys_tx(&tx, account_id)?.into_iter().collect();
    let removed: Vec<&(String, String)> = current.difference(&wanted).collect();
    let added: Vec<&(String, String)> = wanted.difference(&current).collect();
    for key in &removed {
        tx.execute(
            "DELETE FROM provider_account_grants \
             WHERE account_id = ?1 AND subject_type = ?2 AND subject_id = ?3",
            params![account_id, key.0, key.1],
        )
        .map_err(write_err)?;
        sync_capture::capture_grant(&tx, account_id, &key.0, &key.1)?;
    }
    for key in &added {
        tx.execute(
            "INSERT INTO provider_account_grants (account_id, subject_type, subject_id, granted_by) \
             VALUES (?1, ?2, ?3, ?4)",
            params![account_id, key.0, key.1, granted_by],
        )
        .map_err(write_err)?;
        sync_capture::capture_grant(&tx, account_id, &key.0, &key.1)?;
    }
    if !added.is_empty() || !removed.is_empty() {
        audit_tx(
            &tx,
            Some(granted_by),
            "provider_account.grants_set",
            account_id,
            serde_json::json!({
                "added": added.iter().map(|k| subject_label(k)).collect::<Vec<_>>(),
                "removed": removed.iter().map(|k| subject_label(k)).collect::<Vec<_>>(),
            }),
        )?;
    }
    tx.commit().map_err(write_err)?;
    drop(conn);
    list_grants(db, account_id)
}

fn subject_label(key: &(String, String)) -> String {
    if key.1.is_empty() {
        key.0.clone()
    } else {
        format!("{}:{}", key.0, key.1)
    }
}

/// A grant may only name a subject that EXISTS and belongs to the account's
/// organisation. Two reasons, and the second is why this is not merely tidy:
/// an unknown id would be rendered verbatim by the detail window (Core resolves
/// names, and an unresolvable one falls back to the raw id), and a grant naming
/// somebody outside the org would be an access path the org never granted.
///
/// A group carries no organisation of its own (`user_groups` has no `org_id`),
/// so the boundary for a group grant is enforced where it can be: the effective
/// -grant SQL admits a member only if THEY are in the account's org.
fn validate_grant_subject_tx(
    tx: &rusqlite::Transaction<'_>,
    org_id: &str,
    subject_type: &str,
    subject_id: &str,
) -> Result<()> {
    let known = match subject_type {
        "user" => tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM user_accounts u \
                   JOIN org_memberships m ON m.user_id = u.id \
                   WHERE u.id = ?1 AND m.org_id = ?2)",
                params![subject_id, org_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(read_err)?,
        "group" => tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM user_groups WHERE id = ?1)",
                params![subject_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(read_err)?,
        // 'org' is the account's own organisation, which the account row proves.
        _ => 1,
    };
    if known == 0 {
        return Err(anyhow!(
            "grant subject {subject_type} '{subject_id}' is not a member of this organisation"
        ));
    }
    Ok(())
}

/// The SQL that decides whether a user reaches a GLOBAL account: they must be a
/// member of the account's organisation, and a grant must name them directly, a
/// group they belong to, or the organisation as a whole. Written once because
/// the list and the single-account check must never disagree — a screen that
/// shows an account the runtime then refuses is worse than one that shows
/// nothing.
///
/// The membership requirement is factored OUT of the individual arms on
/// purpose. `user_groups` has no organisation of its own (a group is
/// installation-wide), so a group grant can only be bounded by the org of the
/// person reaching through it; making that the common condition means a user
/// and a group grant cannot end up with different boundaries.
const GRANTED_TO_USER: &str = "EXISTS (SELECT 1 FROM org_memberships m \
         WHERE m.user_id = ?1 AND m.org_id = provider_accounts.org_id) \
   AND EXISTS (SELECT 1 FROM provider_account_grants g \
      WHERE g.account_id = provider_accounts.account_id AND ( \
         (g.subject_type = 'user' AND g.subject_id = ?1) \
      OR (g.subject_type = 'group' AND g.subject_id IN \
            (SELECT group_id FROM group_members WHERE user_id = ?1)) \
      OR g.subject_type = 'org'))";

/// Every account the user may run an agent on IN this organisation: their own
/// `scope='user'` accounts plus the global ones granted to them. An
/// administrator gets no extra reach here — administering an account is not
/// using it.
///
/// The organisation is a parameter rather than a filter the caller applies
/// afterwards, because the same rule has to hold for the runtime resolver,
/// which reaches this with nothing but a user and an engine.
pub fn list_accounts_for_user(
    db: &DbPool,
    org_id: &str,
    user_id: &str,
    engine_id: Option<&str>,
) -> Result<Vec<AccountRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut sql = format!(
        "SELECT {ACCOUNT_COLS} FROM provider_accounts \
         WHERE org_id = ?2 AND (owner_user_id = ?1 OR (scope = 'global' AND {GRANTED_TO_USER}))"
    );
    let mut binds: Vec<String> = vec![user_id.to_string(), org_id.to_string()];
    if let Some(engine_id) = engine_id {
        binds.push(engine_id.to_string());
        sql.push_str(&format!(" AND engine_id = ?{}", binds.len()));
    }
    sql.push_str(" ORDER BY engine_id, display_name, account_id");
    let mut stmt = conn.prepare(&sql).map_err(read_err)?;
    let bind_refs: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|b| b as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(&bind_refs[..], read_account)
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// Whether this one account is reachable by this user, by the same rule the
/// list uses.
pub fn user_may_use_account(
    db: &DbPool,
    org_id: &str,
    account_id: &str,
    user_id: &str,
) -> Result<bool> {
    let conn = db.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM provider_accounts WHERE account_id = ?3 \
               AND org_id = ?2 \
               AND (owner_user_id = ?1 OR (scope = 'global' AND {GRANTED_TO_USER})))"
        ),
        params![user_id, org_id, account_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|found| found != 0)
    .map_err(read_err)
}

// =============================================================================
// Credentials
// =============================================================================

pub fn credential_summary(db: &DbPool, account_id: &str) -> Result<Option<CredentialSummary>> {
    let conn = db.read().map_err(read_err)?;
    conn.query_row(
        "SELECT account_id, revision, material_sha256, provider_subject, expires_at, \
                refreshed_at, refreshed_by_node \
         FROM provider_account_credentials WHERE account_id = ?1",
        params![account_id],
        |row| {
            Ok(CredentialSummary {
                account_id: row.get(0)?,
                revision: row.get(1)?,
                material_sha256: row.get(2)?,
                provider_subject: row.get(3)?,
                expires_at: row.get(4)?,
                refreshed_at: row.get(5)?,
                refreshed_by_node: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(read_err)
}

/// `account_id -> (revision, expires_at)` for those of the given accounts that
/// HAVE a credential. An absent key is revision 0 — never logged in — which is
/// what tells the list apart from an account whose login merely expired.
pub fn credential_revisions(
    db: &DbPool,
    account_ids: &[String],
) -> Result<HashMap<String, (i64, Option<String>)>> {
    let conn = db.read().map_err(read_err)?;
    let mut out = HashMap::with_capacity(account_ids.len());
    crate::db::repository::lookup_in_chunks(
        &conn,
        account_ids,
        |placeholders| {
            format!(
                "SELECT account_id, revision, expires_at FROM provider_account_credentials \
                 WHERE account_id IN ({placeholders})"
            )
        },
        |row| {
            out.insert(row.get::<_, String>(0)?, (row.get(1)?, row.get(2)?));
            Ok(())
        },
    )
    .map_err(read_err)?;
    Ok(out)
}

/// Stores a credential claimed to BE `expected_revision`.
///
/// This is the CAS the mesh needs, expressed exactly as the materializer
/// expresses it: a strictly newer revision wins, the same revision with the
/// same material is a replay, and the same revision with DIFFERENT material is
/// a conflict that leaves the row untouched and moves the account to
/// `needs_login`. There is no rule that picks a winner between two writers who
/// both minted revision N, because the one that loses is a token the provider
/// has already rotated.
pub fn set_credential(
    db: &DbPool,
    cipher: &SettingsCipher,
    account_id: &str,
    expected_revision: i64,
    material: &str,
    meta: &CredentialMeta,
) -> Result<CredentialWrite> {
    write_credential(
        db,
        cipher,
        account_id,
        Some(expected_revision),
        material,
        meta,
    )
}

/// Stores a credential this node is minting itself: an administrator entering
/// an API key, or the home node finishing a login. The revision is read and
/// incremented inside the SAME transaction as the write, so two concurrent
/// local writers cannot both mint the same one.
pub fn mint_credential(
    db: &DbPool,
    cipher: &SettingsCipher,
    account_id: &str,
    material: &str,
    meta: &CredentialMeta,
) -> Result<CredentialWrite> {
    write_credential(db, cipher, account_id, None, material, meta)
}

fn write_credential(
    db: &DbPool,
    cipher: &SettingsCipher,
    account_id: &str,
    expected_revision: Option<i64>,
    material: &str,
    meta: &CredentialMeta,
) -> Result<CredentialWrite> {
    if material.trim().is_empty() {
        return Err(anyhow!("credential material is empty"));
    }
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let account_exists: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM provider_accounts WHERE account_id = ?1)",
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found != 0)
        .map_err(read_err)?;
    if !account_exists {
        return Err(anyhow!("provider account '{account_id}' does not exist"));
    }

    let stored: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision, material_sha256 FROM provider_account_credentials \
             WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(read_err)?;
    let local_revision = stored.as_ref().map(|(rev, _)| *rev).unwrap_or(0);
    let sha = sha256_hex(material);
    let revision = expected_revision.unwrap_or(local_revision + 1);

    if revision < local_revision {
        tx.commit().map_err(write_err)?;
        return Ok(CredentialWrite::Stale {
            revision: local_revision,
        });
    }
    if revision == local_revision {
        let same_material = stored
            .as_ref()
            .is_some_and(|(_, stored_sha)| stored_sha == &sha);
        if same_material {
            tx.commit().map_err(write_err)?;
            return Ok(CredentialWrite::Unchanged {
                revision: local_revision,
            });
        }
        set_status_tx(&tx, account_id, "needs_login")?;
        sync_capture::capture_account(&tx, account_id)?;
        // A conflict is the one credential outcome an operator has to act on —
        // two writers minted one revision and the account is now locked out —
        // so it is recorded even though nothing was written.
        audit_tx(
            &tx,
            meta.actor.as_deref(),
            "provider_account.credential_conflict",
            account_id,
            serde_json::json!({
                "revision": local_revision,
                "kept_fingerprint": stored
                    .as_ref()
                    .map(|(_, stored_sha)| credential_fingerprint(stored_sha)),
                "rejected_fingerprint": credential_fingerprint(&sha),
            }),
        )?;
        tx.commit().map_err(write_err)?;
        return Ok(CredentialWrite::Conflict {
            revision: local_revision,
        });
    }

    let material_enc = cipher
        .encrypt_bound(material, &super::credential_context(account_id))
        .map_err(|e| write_err(format!("seal credential: {e}")))?;
    tx.execute(
        "INSERT INTO provider_account_credentials \
           (account_id, revision, material_enc, material_sha256, provider_subject, expires_at, \
            refreshed_at, refreshed_by_node) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?7) \
         ON CONFLICT(account_id) DO UPDATE SET \
            revision = excluded.revision, material_enc = excluded.material_enc, \
            material_sha256 = excluded.material_sha256, \
            provider_subject = excluded.provider_subject, expires_at = excluded.expires_at, \
            refreshed_at = excluded.refreshed_at, refreshed_by_node = excluded.refreshed_by_node",
        params![
            account_id,
            revision,
            material_enc,
            sha,
            meta.provider_subject,
            meta.expires_at,
            meta.refreshed_by_node,
        ],
    )
    .map_err(write_err)?;
    if let Some(subject) = meta.provider_subject.as_deref() {
        apply_provider_subject_tx(&tx, account_id, subject)?;
    }
    // A stored credential is what makes an account usable; a disabled one stays
    // disabled, because disabling is an administrator's decision and not a
    // consequence of somebody logging in.
    set_status_tx(&tx, account_id, "active")?;
    sync_capture::capture_account(&tx, account_id)?;
    audit_tx(
        &tx,
        meta.actor.as_deref(),
        "provider_account.credential_set",
        account_id,
        serde_json::json!({
            "revision": revision,
            "fingerprint": credential_fingerprint(&sha),
            // A rotation and a first login look identical afterwards; only the
            // write knows which it was.
            "rotated": stored.is_some(),
            "refreshed_by_node": meta.refreshed_by_node,
            "expires_at": meta.expires_at,
        }),
    )?;
    tx.commit().map_err(write_err)?;
    Ok(CredentialWrite::Applied { revision })
}

/// The decrypted material, or `None` when the account has no credential on this
/// node. Decryption is bound to the account id: a ciphertext moved from another
/// account's row fails the AEAD tag instead of decrypting under a borrowed
/// identity.
pub fn credential_material(
    db: &DbPool,
    cipher: &SettingsCipher,
    account_id: &str,
) -> Result<Option<String>> {
    let conn = db.read().map_err(read_err)?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT material_enc FROM provider_account_credentials WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(read_err)?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let plain = cipher
        .decrypt_bound(&stored, &super::credential_context(account_id))
        .map_err(|e| read_err(format!("open credential: {e}")))?;
    Ok(Some(plain.value))
}

/// Removes the credential and leaves the account asking for a new one.
pub fn clear_credential(db: &DbPool, account_id: &str, actor: Option<&str>) -> Result<bool> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let dropped: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision, material_sha256 FROM provider_account_credentials \
             WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(read_err)?;
    let removed = tx
        .execute(
            "DELETE FROM provider_account_credentials WHERE account_id = ?1",
            params![account_id],
        )
        .map_err(write_err)?;
    if let Some((revision, sha)) = dropped {
        set_status_tx(&tx, account_id, "needs_login")?;
        sync_capture::capture_account(&tx, account_id)?;
        audit_tx(
            &tx,
            actor,
            "provider_account.credential_clear",
            account_id,
            serde_json::json!({
                "revision": revision,
                "fingerprint": credential_fingerprint(&sha),
            }),
        )?;
    }
    tx.commit().map_err(write_err)?;
    Ok(removed > 0)
}

/// Moves the account's status, except out of `disabled`: that one is an
/// administrator's decision and no credential event may undo it.
fn set_status_tx(tx: &rusqlite::Transaction<'_>, account_id: &str, status: &str) -> Result<()> {
    tx.execute(
        "UPDATE provider_accounts SET status = ?2, \
           updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
         WHERE account_id = ?1 AND status <> 'disabled'",
        params![account_id, status],
    )
    .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Sessions (runtime state, never replicated)
// =============================================================================

pub fn list_sessions(db: &DbPool, account_id: &str) -> Result<Vec<SessionRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT account_id, session_id, user_id, agent_id, workspace_id, node_id, \
                    vendor_session_id, started_at, last_used_at \
             FROM provider_account_sessions WHERE account_id = ?1 ORDER BY started_at, session_id",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![account_id], |row| {
            Ok(SessionRecord {
                account_id: row.get(0)?,
                session_id: row.get(1)?,
                user_id: row.get(2)?,
                agent_id: row.get(3)?,
                workspace_id: row.get(4)?,
                node_id: row.get(5)?,
                vendor_session_id: row.get(6)?,
                started_at: row.get(7)?,
                last_used_at: row.get(8)?,
            })
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// `account_id -> open sessions` for the accounts asked about. Only accounts
/// with at least one session appear.
pub fn session_counts(db: &DbPool, account_ids: &[String]) -> Result<HashMap<String, u32>> {
    let conn = db.read().map_err(read_err)?;
    count_by_account(
        &conn,
        account_ids,
        "SELECT account_id, COUNT(*) FROM provider_account_sessions",
    )
}

/// When each account was last USED by this one user, which is what their own
/// account list shows. Accounts they never ran are absent rather than zero.
pub fn last_used_by_user(db: &DbPool, user_id: &str) -> Result<HashMap<String, String>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT account_id, MAX(last_used_at) FROM provider_account_sessions \
             WHERE user_id = ?1 AND last_used_at IS NOT NULL GROUP BY account_id",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows.into_iter().collect())
}

/// Records or refreshes a session. `vendor_session_id` is bound to
/// (account, user, agent, workspace) by the table's partial unique index, so
/// the same user resuming the same agent in the same workspace reattaches the
/// provider's own conversation instead of starting a new one.
pub fn upsert_session(db: &DbPool, session: &SessionRecord) -> Result<()> {
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO provider_account_sessions \
           (account_id, session_id, user_id, agent_id, workspace_id, node_id, vendor_session_id, \
            last_used_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(account_id, session_id) DO UPDATE SET \
            vendor_session_id = excluded.vendor_session_id, \
            last_used_at = excluded.last_used_at",
        params![
            session.account_id,
            session.session_id,
            session.user_id,
            session.agent_id,
            session.workspace_id,
            session.node_id,
            session.vendor_session_id,
            session.last_used_at,
        ],
    )
    .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Per-node state (node-local, never replicated)
// =============================================================================

pub fn list_node_states(db: &DbPool, account_id: &str) -> Result<Vec<NodeStateRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT account_id, node_id, applied_revision, runtime_state, last_error, updated_at \
             FROM provider_account_node_state WHERE account_id = ?1 ORDER BY node_id",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map(params![account_id], |row| {
            Ok(NodeStateRecord {
                account_id: row.get(0)?,
                node_id: row.get(1)?,
                applied_revision: row.get(2)?,
                runtime_state: row.get(3)?,
                last_error: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// `node_id -> how many accounts that node has actually materialized`, which is
/// the number the node matrix shows. An account that is merely REPLICATED to a
/// node is not counted: the row exists there with `applied_revision = 0` until
/// the node writes the credential out, and counting it would promise a login
/// that would fail.
pub fn materialized_account_counts(db: &DbPool) -> Result<HashMap<String, u32>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT node_id, COUNT(*) FROM provider_account_node_state \
             WHERE applied_revision > 0 AND runtime_state = 'ready' GROUP BY node_id",
        )
        .map_err(read_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    Ok(rows
        .into_iter()
        .map(|(node_id, count)| (node_id, u32::try_from(count).unwrap_or(u32::MAX)))
        .collect())
}

pub fn set_node_state(
    db: &DbPool,
    account_id: &str,
    node_id: &str,
    applied_revision: i64,
    runtime_state: &str,
    last_error: Option<&str>,
) -> Result<()> {
    if !RUNTIME_STATES.contains(&runtime_state) {
        return Err(anyhow!("runtime_state must be one of {RUNTIME_STATES:?}"));
    }
    let conn = db.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO provider_account_node_state \
           (account_id, node_id, applied_revision, runtime_state, last_error) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(account_id, node_id) DO UPDATE SET \
            applied_revision = excluded.applied_revision, \
            runtime_state = excluded.runtime_state, last_error = excluded.last_error, \
            updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
        params![
            account_id,
            node_id,
            applied_revision,
            runtime_state,
            last_error
        ],
    )
    .map_err(write_err)?;
    Ok(())
}

// =============================================================================
// Agent runtime nodes and engines
// =============================================================================

pub fn list_runtime_nodes(db: &DbPool) -> Result<Vec<RuntimeNodeRecord>> {
    let conn = db.read().map_err(read_err)?;
    let mut stmt = conn
        .prepare(
            "SELECT node_id, receives_accounts, updated_by, updated_at FROM agent_runtime_nodes \
             ORDER BY node_id",
        )
        .map_err(read_err)?;
    let mut nodes = stmt
        .query_map([], |row| {
            Ok(RuntimeNodeRecord {
                node_id: row.get(0)?,
                receives_accounts: row.get::<_, i64>(1)? != 0,
                updated_by: row.get(2)?,
                updated_at: row.get(3)?,
                engines: Vec::new(),
            })
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    drop(stmt);

    let mut stmt = conn
        .prepare(
            "SELECT node_id, engine_id, install_state, version, installed_at, last_error \
             FROM agent_runtime_engines ORDER BY node_id, engine_id",
        )
        .map_err(read_err)?;
    let engines = stmt
        .query_map([], |row| {
            Ok(RuntimeEngineRecord {
                node_id: row.get(0)?,
                engine_id: row.get(1)?,
                install_state: row.get(2)?,
                version: row.get(3)?,
                installed_at: row.get(4)?,
                last_error: row.get(5)?,
            })
        })
        .map_err(read_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(read_err)?;
    for engine in engines {
        if let Some(node) = nodes.iter_mut().find(|n| n.node_id == engine.node_id) {
            node.engines.push(engine);
        }
    }
    Ok(nodes)
}

/// Flips whether a node may hold account credentials. The row is created on
/// first use: a node is in this table because somebody made a decision about
/// it, not because it exists.
///
/// Turning the flag OFF on a node that carries nothing else removes the row
/// instead of storing a `false`. The table's only job is to record decisions,
/// and "no decision" and "decided no" mean the same thing to every reader of
/// it — keeping the row would replicate a fact that never changes anything and
/// would make the node matrix grow one permanent line per node ever toggled.
pub fn set_receives_accounts(
    db: &DbPool,
    node_id: &str,
    enabled: bool,
    updated_by: Option<&str>,
) -> Result<()> {
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let has_engines: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_runtime_engines WHERE node_id = ?1)",
            params![node_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found != 0)
        .map_err(read_err)?;
    if !enabled && !has_engines {
        tx.execute(
            "DELETE FROM agent_runtime_nodes WHERE node_id = ?1",
            params![node_id],
        )
        .map_err(write_err)?;
    } else {
        tx.execute(
            "INSERT INTO agent_runtime_nodes (node_id, receives_accounts, updated_by) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(node_id) DO UPDATE SET receives_accounts = excluded.receives_accounts, \
                updated_by = excluded.updated_by, \
                updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            params![node_id, i64::from(enabled), updated_by],
        )
        .map_err(write_err)?;
    }
    sync_capture::capture_runtime_node(&tx, node_id)?;
    audit_tx(
        &tx,
        updated_by,
        "agent_runtime.receives_accounts",
        node_id,
        serde_json::json!({ "enabled": enabled }),
    )?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

/// Records what one engine looks like on THIS node. The runtime node row is
/// created alongside, because the engine row's FK needs it and a node that has
/// an engine installed is by definition a node somebody set up.
///
/// The node is not a parameter: an engine row is a measurement, and a node can
/// only measure itself. Letting a caller name the node would let this
/// installation assert an install state for a peer, which the materializer on
/// that peer then refuses — the row would exist here and nowhere else.
pub fn set_engine_state(
    db: &DbPool,
    engine_id: &str,
    install_state: &str,
    version: Option<&str>,
    last_error: Option<&str>,
) -> Result<()> {
    if super::engine(engine_id).is_none() {
        return Err(anyhow!("unknown agent engine '{engine_id}'"));
    }
    if !INSTALL_STATES.contains(&install_state) {
        return Err(anyhow!("install_state must be one of {INSTALL_STATES:?}"));
    }
    let mut conn = db.write().map_err(write_err)?;
    let tx = conn.transaction().map_err(write_err)?;
    let node_id = local_node_id_tx(&tx)?
        .ok_or_else(|| anyhow!("this installation has no node identity yet"))?;
    let node_id = node_id.as_str();
    let created = tx
        .execute(
            "INSERT OR IGNORE INTO agent_runtime_nodes (node_id) VALUES (?1)",
            params![node_id],
        )
        .map_err(write_err)?;
    tx.execute(
        "INSERT INTO agent_runtime_engines \
           (node_id, engine_id, install_state, version, installed_at, last_error) \
         VALUES (?1, ?2, ?3, ?4, \
           CASE WHEN ?3 = 'installed' THEN strftime('%Y-%m-%dT%H:%M:%SZ','now') END, ?5) \
         ON CONFLICT(node_id, engine_id) DO UPDATE SET \
            install_state = excluded.install_state, version = excluded.version, \
            installed_at = COALESCE(excluded.installed_at, agent_runtime_engines.installed_at), \
            last_error = excluded.last_error",
        params![node_id, engine_id, install_state, version, last_error],
    )
    .map_err(write_err)?;
    if created > 0 {
        sync_capture::capture_runtime_node(&tx, node_id)?;
    }
    sync_capture::capture_runtime_engine(&tx, node_id, engine_id)?;
    tx.commit().map_err(write_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("db")
    }

    fn cipher() -> SettingsCipher {
        SettingsCipher::new(&[7u8; 32])
    }

    fn seed_user(db: &DbPool, user_id: &str) {
        let conn = db.write().expect("db");
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts \
               (id, username, password_hash, display_name, is_active, is_admin, role) \
             VALUES (?1, ?1, 'x', ?1, 1, 0, 'user')",
            params![user_id],
        )
        .expect("seed user");
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES ('org-default', ?1, 'role-org-viewer', datetime('now'), 'test')",
            params![user_id],
        )
        .expect("seed membership");
    }

    fn seed_group(db: &DbPool, group_id: &str, members: &[&str]) {
        let conn = db.write().expect("db");
        conn.execute(
            "INSERT OR IGNORE INTO user_groups (id, name) VALUES (?1, ?1)",
            params![group_id],
        )
        .expect("seed group");
        for member in members {
            conn.execute(
                "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?1, ?2)",
                params![group_id, member],
            )
            .expect("seed membership");
        }
    }

    fn global_account(db: &DbPool, account_id: &str) -> AccountRecord {
        create_account(
            db,
            &NewAccount {
                account_id: account_id.to_string(),
                org_id: "org-default".into(),
                engine_id: "claude-code".into(),
                display_name: format!("Account {account_id}"),
                scope: "global".into(),
                owner_user_id: None,
                credential_kind: "api_key".into(),
                created_by: "admin".into(),
            },
        )
        .expect("create account")
    }

    fn user_account(db: &DbPool, account_id: &str, owner: &str) -> AccountRecord {
        create_account(
            db,
            &NewAccount {
                account_id: account_id.to_string(),
                org_id: "org-default".into(),
                engine_id: "codex".into(),
                display_name: format!("Account {account_id}"),
                scope: "user".into(),
                owner_user_id: Some(owner.to_string()),
                credential_kind: "provider_login".into(),
                created_by: owner.to_string(),
            },
        )
        .expect("create account")
    }

    fn grant(subject_type: &str, subject_id: &str) -> GrantInput {
        GrantInput {
            subject_type: subject_type.into(),
            subject_id: subject_id.into(),
        }
    }

    fn ids(accounts: &[AccountRecord]) -> Vec<&str> {
        accounts.iter().map(|a| a.account_id.as_str()).collect()
    }

    /// A direct user grant, a group grant and an org-wide grant all reach the
    /// same user, and none of them reaches somebody outside them.
    #[test]
    fn a_grant_resolves_through_user_group_and_org_subjects() {
        let db = pool();
        for user in ["alice", "bob", "carol", "outsider"] {
            seed_user(&db, user);
        }
        seed_group(&db, "platform", &["bob"]);
        // The outsider is a real account but not a member of the org, so the
        // org-wide grant must miss them.
        db.write()
            .unwrap()
            .execute("DELETE FROM org_memberships WHERE user_id = 'outsider'", [])
            .unwrap();

        global_account(&db, "direct");
        global_account(&db, "by-group");
        global_account(&db, "by-org");
        set_grants(&db, "direct", &[grant("user", "alice")], "admin").unwrap();
        set_grants(&db, "by-group", &[grant("group", "platform")], "admin").unwrap();
        set_grants(&db, "by-org", &[grant("org", "")], "admin").unwrap();

        assert_eq!(
            ids(&list_accounts_for_user(&db, "alice", None).unwrap()),
            vec!["by-org", "direct"]
        );
        assert_eq!(
            ids(&list_accounts_for_user(&db, "bob", None).unwrap()),
            vec!["by-group", "by-org"]
        );
        assert_eq!(
            ids(&list_accounts_for_user(&db, "carol", None).unwrap()),
            vec!["by-org"]
        );
        assert!(list_accounts_for_user(&db, "outsider", None)
            .unwrap()
            .is_empty());

        // The single-account check must agree with the list, row for row.
        for (user, account, expected) in [
            ("alice", "direct", true),
            ("alice", "by-group", false),
            ("bob", "by-group", true),
            ("carol", "direct", false),
            ("outsider", "by-org", false),
        ] {
            assert_eq!(
                user_may_use_account(&db, account, user).unwrap(),
                expected,
                "{user} -> {account}"
            );
        }
    }

    /// A user account belongs to its owner alone: no grant can be written on
    /// it, and nobody else sees it.
    #[test]
    fn a_user_account_answers_to_its_owner_only() {
        let db = pool();
        seed_user(&db, "alice");
        seed_user(&db, "bob");
        user_account(&db, "alice-codex", "alice");

        assert_eq!(
            ids(&list_accounts_for_user(&db, "alice", None).unwrap()),
            vec!["alice-codex"]
        );
        assert!(list_accounts_for_user(&db, "bob", None).unwrap().is_empty());
        let refused = set_grants(&db, "alice-codex", &[grant("user", "bob")], "admin")
            .expect_err("a user account cannot be shared");
        assert!(refused.to_string().contains("global"), "{refused}");
    }

    /// The full replace touches only what changed: an untouched grant keeps its
    /// `granted_by`, a removed one is gone, a new one is there.
    #[test]
    fn setting_grants_diffs_instead_of_rewriting_every_row() {
        let db = pool();
        for user in ["alice", "bob", "carol"] {
            seed_user(&db, user);
        }
        global_account(&db, "shared");
        set_grants(
            &db,
            "shared",
            &[grant("user", "alice"), grant("user", "bob")],
            "first-admin",
        )
        .unwrap();

        let after = set_grants(
            &db,
            "shared",
            &[grant("user", "alice"), grant("user", "carol")],
            "second-admin",
        )
        .unwrap();

        let subjects: Vec<(&str, &str)> = after
            .iter()
            .map(|g| (g.subject_id.as_str(), g.granted_by.as_str()))
            .collect();
        assert_eq!(
            subjects,
            vec![("alice", "first-admin"), ("carol", "second-admin")],
            "alice was never re-granted, so her attribution stands"
        );
    }

    /// An 'org' grant has exactly one spelling, whatever the caller sends.
    #[test]
    fn an_org_grant_is_keyed_by_the_empty_subject() {
        let db = pool();
        seed_user(&db, "alice");
        global_account(&db, "shared");
        set_grants(&db, "shared", &[grant("org", "org-default")], "admin").unwrap();
        let grants = list_grants(&db, "shared").unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].subject_id, "");
        assert!(user_may_use_account(&db, "shared", "alice").unwrap());
    }

    /// Revision CAS: newer wins, a replay is a no-op, and two writers claiming
    /// one revision for different material leave the stored row alone.
    #[test]
    fn a_credential_conflict_leaves_the_stored_material_untouched() {
        let db = pool();
        let cipher = cipher();
        global_account(&db, "shared");

        let first = mint_credential(
            &db,
            &cipher,
            "shared",
            "key-one",
            &CredentialMeta::default(),
        )
        .unwrap();
        assert_eq!(first, CredentialWrite::Applied { revision: 1 });
        assert_eq!(
            get_account(&db, "shared").unwrap().unwrap().status,
            "active"
        );

        let replay = set_credential(
            &db,
            &cipher,
            "shared",
            1,
            "key-one",
            &CredentialMeta::default(),
        )
        .unwrap();
        assert_eq!(replay, CredentialWrite::Unchanged { revision: 1 });

        let conflict = set_credential(
            &db,
            &cipher,
            "shared",
            1,
            "key-two",
            &CredentialMeta::default(),
        )
        .unwrap();
        assert_eq!(conflict, CredentialWrite::Conflict { revision: 1 });
        assert_eq!(
            credential_material(&db, &cipher, "shared")
                .unwrap()
                .unwrap(),
            "key-one",
            "a conflict must never overwrite the material"
        );
        assert_eq!(
            get_account(&db, "shared").unwrap().unwrap().status,
            "needs_login",
            "a conflict asks a human, it does not guess"
        );

        let stale = set_credential(
            &db,
            &cipher,
            "shared",
            0,
            "key-three",
            &CredentialMeta::default(),
        )
        .unwrap();
        assert_eq!(stale, CredentialWrite::Stale { revision: 1 });
        assert_eq!(
            credential_material(&db, &cipher, "shared")
                .unwrap()
                .unwrap(),
            "key-one"
        );

        let newer = set_credential(
            &db,
            &cipher,
            "shared",
            2,
            "key-three",
            &CredentialMeta::default(),
        )
        .unwrap();
        assert_eq!(newer, CredentialWrite::Applied { revision: 2 });
        assert_eq!(
            credential_material(&db, &cipher, "shared")
                .unwrap()
                .unwrap(),
            "key-three"
        );
    }

    /// The ciphertext is bound to the account it was written for: moved onto
    /// another account's row it fails the AEAD tag instead of decrypting.
    #[test]
    fn a_credential_moved_to_another_account_does_not_decrypt() {
        let db = pool();
        let cipher = cipher();
        global_account(&db, "account-a");
        global_account(&db, "account-b");
        mint_credential(
            &db,
            &cipher,
            "account-a",
            "secret-of-a",
            &CredentialMeta::default(),
        )
        .unwrap();

        let stolen: String = db
            .read()
            .unwrap()
            .query_row(
                "SELECT material_enc FROM provider_account_credentials WHERE account_id = 'account-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO provider_account_credentials \
                   (account_id, revision, material_enc, material_sha256, refreshed_at) \
                 VALUES ('account-b', 1, ?1, 'copied', 'now')",
                params![stolen],
            )
            .unwrap();

        assert_eq!(
            credential_material(&db, &cipher, "account-a")
                .unwrap()
                .unwrap(),
            "secret-of-a"
        );
        let error = credential_material(&db, &cipher, "account-b")
            .expect_err("a relocated ciphertext must not open");
        assert!(error.to_string().contains("open credential"), "{error}");
    }

    /// Clearing a credential asks for a new login and removes the material.
    #[test]
    fn clearing_a_credential_asks_for_a_new_login() {
        let db = pool();
        let cipher = cipher();
        global_account(&db, "shared");
        mint_credential(&db, &cipher, "shared", "key", &CredentialMeta::default()).unwrap();
        assert!(clear_credential(&db, "shared").unwrap());
        assert!(credential_material(&db, &cipher, "shared")
            .unwrap()
            .is_none());
        assert_eq!(
            get_account(&db, "shared").unwrap().unwrap().status,
            "needs_login"
        );
        assert!(!clear_credential(&db, "shared").unwrap());
    }

    /// The provider identity is written once. A second, different one is
    /// refused — under one account id it would mean a different subscription.
    #[test]
    fn a_provider_subject_is_immutable_once_set() {
        let db = pool();
        global_account(&db, "shared");
        set_provider_subject(&db, "shared", "team@example.com").unwrap();
        // Idempotent for the same identity.
        set_provider_subject(&db, "shared", "team@example.com").unwrap();
        let error = set_provider_subject(&db, "shared", "other@example.com")
            .expect_err("a second identity must be refused");
        assert!(
            error.to_string().contains("different provider identity"),
            "{error}"
        );
        assert_eq!(
            get_account(&db, "shared")
                .unwrap()
                .unwrap()
                .provider_subject
                .unwrap(),
            "team@example.com"
        );

        // The same rule applies when the identity arrives with a credential.
        let cipher = cipher();
        let error = mint_credential(
            &db,
            &cipher,
            "shared",
            "key",
            &CredentialMeta {
                provider_subject: Some("other@example.com".into()),
                ..CredentialMeta::default()
            },
        )
        .expect_err("a credential cannot rebind the identity");
        assert!(
            error.to_string().contains("different provider identity"),
            "{error}"
        );
    }

    /// A disabled account stays disabled: storing a credential is not a way
    /// around an administrator's decision.
    #[test]
    fn a_disabled_account_is_not_reactivated_by_a_credential() {
        let db = pool();
        let cipher = cipher();
        global_account(&db, "shared");
        update_account(
            &db,
            "shared",
            &AccountUpdate {
                status: Some("disabled".into()),
                ..AccountUpdate::default()
            },
        )
        .unwrap();
        mint_credential(&db, &cipher, "shared", "key", &CredentialMeta::default()).unwrap();
        assert_eq!(
            get_account(&db, "shared").unwrap().unwrap().status,
            "disabled"
        );
    }

    /// Engines that authenticate through their own login only cannot hold an
    /// organisation API key.
    #[test]
    fn an_engine_without_an_api_key_mode_refuses_one() {
        let db = pool();
        let error = create_account(
            &db,
            &NewAccount {
                account_id: "grok".into(),
                org_id: "org-default".into(),
                engine_id: "grok-build".into(),
                display_name: "Grok".into(),
                scope: "global".into(),
                owner_user_id: None,
                credential_kind: "api_key".into(),
                created_by: "admin".into(),
            },
        )
        .expect_err("grok has no API-key mode");
        assert!(error.to_string().contains("API key"), "{error}");
    }

    /// The node matrix carries its engines, and flipping the flag creates the
    /// node row on first use.
    #[test]
    fn the_runtime_matrix_records_nodes_and_their_engines() {
        let db = pool();
        set_engine_state(
            &db,
            "helios",
            "claude-code",
            "installed",
            Some("2.1.0"),
            None,
        )
        .unwrap();
        set_engine_state(&db, "helios", "codex", "error", None, Some("no artifact")).unwrap();
        set_receives_accounts(&db, "helios", true, Some("admin")).unwrap();

        let nodes = list_runtime_nodes(&db).unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].receives_accounts);
        assert_eq!(nodes[0].engines.len(), 2);
        let claude = &nodes[0].engines[0];
        assert_eq!(claude.engine_id, "claude-code");
        assert_eq!(claude.install_state, "installed");
        assert!(claude.installed_at.is_some());
        assert_eq!(
            nodes[0].engines[1].last_error.as_deref(),
            Some("no artifact")
        );
    }

    /// Deleting an account takes its grants, credential and sessions with it.
    #[test]
    fn deleting_an_account_removes_everything_hanging_off_it() {
        let db = pool();
        let cipher = cipher();
        seed_user(&db, "alice");
        global_account(&db, "shared");
        set_grants(&db, "shared", &[grant("user", "alice")], "admin").unwrap();
        mint_credential(&db, &cipher, "shared", "key", &CredentialMeta::default()).unwrap();
        upsert_session(
            &db,
            &SessionRecord {
                account_id: "shared".into(),
                session_id: "s1".into(),
                user_id: "alice".into(),
                agent_id: None,
                workspace_id: None,
                node_id: "helios".into(),
                vendor_session_id: None,
                started_at: String::new(),
                last_used_at: None,
            },
        )
        .unwrap();

        assert!(delete_account(&db, "shared").unwrap());
        assert!(get_account(&db, "shared").unwrap().is_none());
        assert!(list_grants(&db, "shared").unwrap().is_empty());
        assert!(list_sessions(&db, "shared").unwrap().is_empty());
        assert!(credential_summary(&db, "shared").unwrap().is_none());
        assert!(!delete_account(&db, "shared").unwrap());
    }
}
