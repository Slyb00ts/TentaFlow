// ===== File: tentavm/policy.rs — writing environment policy to the registry ===
//
// Two tables of plan §4.1 hold policy an administrator decides rather than
// state a machine reports: `vm_host_grants` (who may do what, and where) and
// `vm_instance_settings` (how the environment behaves). Both are
// `OwnerRule::Organization` in `sync/tentavm_registry.rs`, which is plan §6.1's
// answer for a row nobody owns: it replicates from an OPERATOR node and from
// nowhere else, because the "administrator signature" arm of §6.1 does not
// exist (step 15 — `user_identity_keys` holds no key material and there is no
// signing function).
//
// That one fact decides the shape of this module, and it is worth stating
// before the code rather than after:
//
//   a policy write from a NON-operator node is REFUSED here, not written.
//
// The alternative — write locally, let every peer's `apply` arm refuse the
// operation — is not "degraded replication". It is a grant that exists on one
// node and on no other: node A believes a user holds `manage`, the rest of the
// fleet believes they hold nothing, and the two never converge because the
// operation is terminally refused rather than deferred. That is a split of the
// authorization itself, which is exactly what `06-granty-wymagania.md` W1
// forbids for the access-request row; the argument does not become weaker when
// the row is the grant instead of the request for one.
//
// The cost of refusing, named: an administrator sitting at a node the fleet has
// not marked `operator` cannot edit grants or settings from it, and is told to
// use an operator node. A fresh install is not affected — a node writes its own
// `sync_nodes` row with `operator = 1` (`repository.rs`, the bootstrap of the
// operator list), so the single-node case works from the first boot.

use anyhow::{anyhow, Result};

use crate::db::DbPool;
use crate::sync::core_registry::CoreSyncResourceKind as Kind;

/// One row of the desired grant matrix of a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRow {
    pub subject_kind: String,
    pub subject_id: String,
    pub role: String,
}

/// Is `node_id` on the organization's operator list?
///
/// The same question `sync::tentavm_registry::authorize_organization` asks of
/// an INCOMING operation, asked here of the outgoing one. Asking it in both
/// directions is the point: a write this node would refuse from a peer is a
/// write it must not mint either, and the answer comes from the same column.
pub fn node_is_operator(main_db: &DbPool, node_id: &str) -> Result<bool> {
    let conn = main_db
        .read()
        .map_err(|e| anyhow!("tentavm policy: main db lock: {e}"))?;
    let flag: Option<i64> = conn
        .query_row(
            "SELECT operator FROM sync_nodes WHERE node_id = ?1",
            rusqlite::params![node_id],
            |row| row.get(0),
        )
        .ok();
    Ok(flag.unwrap_or(0) != 0)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Replaces the grant matrix of ONE host with `desired`, and captures every row
/// that actually moved.
///
/// The matrix is sent whole (`HostGrantsSetRequest` documents it as "the
/// complete desired grant set of one host — rows absent from `grants` are
/// removed"), so this is a set difference, not a patch: rows that are gone from
/// `desired` are deleted and captured as tombstones, rows whose role changed are
/// updated, rows that are identical are left alone and mint nothing.
///
/// "Left alone mints nothing" is not an optimization. A capture is an operation
/// in the replicated ledger; minting one for a row nobody changed would make
/// every save of an unchanged screen a fleet-wide write, and step 7 already had
/// to fix exactly that shape on `ensure_local_host`.
///
/// Returns how many rows changed.
pub fn set_host_grants(
    main_db: &DbPool,
    instance_id: &str,
    org_id: &str,
    host_id: &str,
    granted_by: &str,
    local_node_id: &str,
    desired: &[GrantRow],
) -> Result<usize> {
    let now = now();
    let mut conn = main_db
        .write()
        .map_err(|e| anyhow!("tentavm policy: main db lock: {e}"))?;
    // One transaction for the rows AND their captures: the ledger mints the HLC
    // inside it, so a capture committed without its row would publish a state
    // that never existed on this node.
    let tx = conn.transaction()?;

    let existing: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT subject_kind, subject_id FROM vm_host_grants \
             WHERE instance_id = ?1 AND org_id = ?2 AND host_id = ?3",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![instance_id, org_id, host_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut changed = 0usize;
    for (subject_kind, subject_id) in &existing {
        if desired
            .iter()
            .any(|row| &row.subject_kind == subject_kind && &row.subject_id == subject_id)
        {
            continue;
        }
        let removed = tx.execute(
            "DELETE FROM vm_host_grants \
             WHERE instance_id = ?1 AND org_id = ?2 AND host_id = ?3 \
               AND subject_kind = ?4 AND subject_id = ?5",
            rusqlite::params![instance_id, org_id, host_id, subject_kind, subject_id],
        )?;
        if removed > 0 {
            changed += 1;
            crate::sync::tentavm_registry::capture_row(
                &tx,
                Kind::VmHostGrant,
                &[instance_id, host_id, subject_kind, subject_id],
            )?;
        }
    }

    for row in desired {
        // Guarded so a save that changes nothing writes nothing: `created_at`
        // and `granted_by` of an untouched row keep their original attribution,
        // which is what makes the column an audit trail rather than a record of
        // who last pressed Save.
        let written = tx.execute(
            "INSERT INTO vm_host_grants \
                (instance_id, host_id, subject_kind, subject_id, org_id, role, \
                 granted_by, created_at, updated_at, updated_by_node) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9) \
             ON CONFLICT(instance_id, host_id, subject_kind, subject_id) DO UPDATE SET \
                 role = excluded.role, \
                 granted_by = excluded.granted_by, \
                 updated_at = excluded.updated_at, \
                 updated_by_node = excluded.updated_by_node \
             WHERE vm_host_grants.role <> excluded.role",
            rusqlite::params![
                instance_id,
                host_id,
                row.subject_kind,
                row.subject_id,
                org_id,
                row.role,
                granted_by,
                now,
                local_node_id
            ],
        )?;
        if written > 0 {
            changed += 1;
            crate::sync::tentavm_registry::capture_row(
                &tx,
                Kind::VmHostGrant,
                &[instance_id, host_id, &row.subject_kind, &row.subject_id],
            )?;
        }
    }

    tx.commit()?;
    Ok(changed)
}

/// Writes the environment's settings document, one `vm_instance_settings` row
/// per key, and captures the keys that moved.
///
/// Keys absent from `values` are left alone rather than deleted: the settings
/// screen sends the whole record and `settings_to_rows` states every key it
/// knows, so an absent key means "this build does not know that key" — and
/// deleting a key a NEWER node wrote would let an older node silently reset a
/// setting it cannot render.
pub fn set_instance_settings(
    main_db: &DbPool,
    instance_id: &str,
    org_id: &str,
    local_node_id: &str,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<usize> {
    let now = now();
    let mut conn = main_db
        .write()
        .map_err(|e| anyhow!("tentavm policy: main db lock: {e}"))?;
    let tx = conn.transaction()?;
    let mut changed = 0usize;
    for (key, value) in values {
        let written = tx.execute(
            "INSERT INTO vm_instance_settings \
                (instance_id, key, org_id, value, created_at, updated_at, updated_by_node) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6) \
             ON CONFLICT(instance_id, key) DO UPDATE SET \
                 value = excluded.value, \
                 updated_at = excluded.updated_at, \
                 updated_by_node = excluded.updated_by_node \
             WHERE vm_instance_settings.value <> excluded.value",
            rusqlite::params![instance_id, key, org_id, value, now, local_node_id],
        )?;
        if written > 0 {
            changed += 1;
            crate::sync::tentavm_registry::capture_row(
                &tx,
                Kind::VmInstanceSetting,
                &[instance_id, key],
            )?;
        }
    }
    tx.commit()?;
    Ok(changed)
}
