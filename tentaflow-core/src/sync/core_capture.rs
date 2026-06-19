// =============================================================================
// Plik: sync/core_capture.rs
// Opis: Binary capture zapisow core SQLite przygotowujacy Flow Builder, userow,
//       grupy i role do replikacji przez Sync Ledger.
// =============================================================================

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use super::core_registry::{descriptor_for_kind, CoreSyncResourceKind};
use super::ledger::{BaselineEpoch, FieldValue, HybridLogicalTimestamp};
use super::runtime::SqlWriteAction;

static CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreWriteCapture {
    pub capture_id: String,
    pub org_id: String,
    pub table_name: String,
    pub resource_type: String,
    pub resource_id: String,
    pub primary_key: String,
    pub action: SqlWriteAction,
    pub changed_fields: BTreeMap<String, FieldValue>,
    pub actor_user_id: Option<String>,
    /// HLC stamp minted inside the write transaction. The drained ledger
    /// operation reuses this exact timestamp so the originating order survives,
    /// and the receiver's HLC-LWW comparison sees a stable, pre-commit instant.
    pub hlc: HybridLogicalTimestamp,
    /// Locally-active baseline epoch at capture time. The drained operation
    /// inherits it so a post-cutover reset cannot silently mix epochs.
    pub epoch: BaselineEpoch,
    pub created_at_ms: i64,
}

impl CoreWriteCapture {
    /// Builds a capture. `hlc` and `epoch` are mandatory and minted by the
    /// caller inside the same SQLite transaction as the write, so a capture can
    /// never exist without the timestamp the ledger operation will carry.
    pub fn new(
        kind: CoreSyncResourceKind,
        org_id: impl Into<String>,
        resource_id: impl Into<String>,
        action: SqlWriteAction,
        changed_fields: BTreeMap<String, FieldValue>,
        actor_user_id: Option<String>,
        hlc: HybridLogicalTimestamp,
        epoch: BaselineEpoch,
    ) -> Self {
        let descriptor = descriptor_for_kind(kind);
        let org_id = org_id.into();
        let resource_id = resource_id.into();
        let created_at_ms = super::runtime::now_ms();
        let capture_id = stable_capture_id(
            &org_id,
            descriptor.table_name,
            descriptor.resource_type,
            &resource_id,
            action,
            created_at_ms,
            &changed_fields,
        );
        Self {
            capture_id,
            org_id,
            table_name: descriptor.table_name.to_string(),
            resource_type: descriptor.resource_type.to_string(),
            resource_id,
            primary_key: descriptor.primary_key_column.to_string(),
            action,
            changed_fields,
            actor_user_id,
            hlc,
            epoch,
            created_at_ms,
        }
    }
}

pub fn record_core_write_capture(
    tx: &rusqlite::Transaction<'_>,
    capture: &CoreWriteCapture,
) -> Result<()> {
    let changed_fields_blob = encode_changed_fields(&capture.changed_fields)?;
    tx.execute(
        "INSERT INTO __tentaflow_core_sync_captures \
         (capture_id, org_id, table_name, resource_type, resource_id, primary_key, action, \
          changed_fields_blob, actor_user_id, hlc_wall, hlc_logical, hlc_node, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            &capture.capture_id,
            &capture.org_id,
            &capture.table_name,
            &capture.resource_type,
            &capture.resource_id,
            &capture.primary_key,
            capture.action.as_str(),
            changed_fields_blob,
            capture.actor_user_id,
            capture.hlc.wall_time_ms,
            capture.hlc.logical,
            &capture.hlc.node_id,
            capture.created_at_ms,
        ],
    )?;
    Ok(())
}

pub fn load_core_write_capture(
    conn: &rusqlite::Connection,
    capture_id: &str,
) -> Result<Option<CoreWriteCapture>> {
    conn.query_row(
        "SELECT capture_id, org_id, table_name, resource_type, resource_id, primary_key, action, \
                changed_fields_blob, actor_user_id, hlc_wall, hlc_logical, hlc_node, created_at_ms \
         FROM __tentaflow_core_sync_captures WHERE capture_id = ?1",
        rusqlite::params![capture_id],
        |row| {
            let action_raw: String = row.get(6)?;
            let changed_fields_blob: Vec<u8> = row.get(7)?;
            let action = SqlWriteAction::from_str(&action_raw)
                .map_err(|e| rusqlite_decode_error(e.to_string()))?;
            let changed_fields = decode_changed_fields(&changed_fields_blob)
                .map_err(|e| rusqlite_decode_error(e.to_string()))?;
            let hlc = HybridLogicalTimestamp {
                wall_time_ms: row.get(9)?,
                logical: row.get::<_, i64>(10)? as u32,
                node_id: row.get(11)?,
            };
            Ok(CoreWriteCapture {
                capture_id: row.get(0)?,
                org_id: row.get(1)?,
                table_name: row.get(2)?,
                resource_type: row.get(3)?,
                resource_id: row.get(4)?,
                primary_key: row.get(5)?,
                action,
                changed_fields,
                actor_user_id: row.get(8)?,
                hlc,
                // Resolved from the live ledger epoch when the operation is
                // built at drain time; the capture row does not persist it.
                epoch: BaselineEpoch {
                    counter: 0,
                    origin_node: String::new(),
                },
                created_at_ms: row.get(12)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn drain_pending_core_captures(pool: &crate::db::DbPool, limit: usize) -> Result<usize> {
    drain_pending_core_captures_with(pool, limit, |capture| {
        super::runtime::record_core_capture(capture).map(|opt| opt.map(|record| record.op_id))
    })
}

/// Drains pending core captures through an explicit recorder. `drain_pending_core_captures`
/// records via the global runtime; the baseline reset path records through the
/// same `SyncRuntime` it just bumped, so the re-seed cannot land in a different
/// runtime than the one that advanced the epoch.
pub fn drain_pending_core_captures_with<F>(
    pool: &crate::db::DbPool,
    limit: usize,
    mut record: F,
) -> Result<usize>
where
    F: FnMut(CoreWriteCapture) -> super::ledger::LedgerResult<Option<super::ledger::OperationId>>,
{
    let captures = {
        let conn = pool
            .read()
            .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
        load_pending_core_captures(&conn, limit)?
    };
    let mut drained = 0usize;
    for capture in captures {
        match record(capture.clone()) {
            Ok(Some(op_id)) => {
                let mut conn = pool
                    .write()
                    .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
                mark_core_capture_status(
                    &mut conn,
                    &capture.capture_id,
                    "ledgered",
                    Some(op_id.to_hex()),
                    None,
                )?;
                drained += 1;
            }
            Ok(None) => break,
            Err(e) => {
                let mut conn = pool
                    .write()
                    .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
                mark_core_capture_status(
                    &mut conn,
                    &capture.capture_id,
                    "error",
                    None,
                    Some(&e.to_string()),
                )?;
            }
        }
    }
    Ok(drained)
}

/// Empties the core capture journal so a baseline reset starts from a clean
/// slate before re-seeding the present SQLite snapshot. The historical journal
/// is intentionally discarded: after the v54 ALTER its rows carry zeroed HLCs,
/// which LWW cannot order, and the reset replicates current state rather than
/// the write history. Returns the number of rows removed.
pub fn clear_core_capture_journal(pool: &crate::db::DbPool) -> Result<usize> {
    let conn = pool
        .write()
        .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
    let removed = conn.execute("DELETE FROM __tentaflow_core_sync_captures", [])?;
    Ok(removed)
}

fn load_pending_core_captures(
    conn: &rusqlite::Connection,
    limit: usize,
) -> Result<Vec<CoreWriteCapture>> {
    let mut stmt = conn.prepare_cached(
        "SELECT capture_id FROM __tentaflow_core_sync_captures \
         WHERE status IN ('pending','error') \
         ORDER BY created_at_ms ASC, capture_id ASC LIMIT ?1",
    )?;
    let capture_ids = stmt
        .query_map(rusqlite::params![limit as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut captures = Vec::with_capacity(capture_ids.len());
    for capture_id in capture_ids {
        if let Some(capture) = load_core_write_capture(conn, &capture_id)? {
            captures.push(capture);
        }
    }
    Ok(captures)
}

fn mark_core_capture_status(
    conn: &mut rusqlite::Connection,
    capture_id: &str,
    status: &str,
    operation_id: Option<String>,
    error_message: Option<&str>,
) -> Result<()> {
    let ledgered_at_ms = if status == "ledgered" {
        Some(super::runtime::now_ms())
    } else {
        None
    };
    conn.execute(
        "UPDATE __tentaflow_core_sync_captures \
         SET status = ?2, operation_id = COALESCE(?3, operation_id), \
             error_message = ?4, ledgered_at_ms = ?5 \
         WHERE capture_id = ?1",
        rusqlite::params![
            capture_id,
            status,
            operation_id,
            error_message,
            ledgered_at_ms,
        ],
    )?;
    Ok(())
}

fn rusqlite_decode_error(message: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        message,
    )))
}

fn encode_changed_fields(fields: &BTreeMap<String, FieldValue>) -> Result<Vec<u8>> {
    crate::sync::ledger::encode(fields).map_err(Into::into)
}

fn decode_changed_fields(bytes: &[u8]) -> Result<BTreeMap<String, FieldValue>> {
    crate::sync::ledger::decode(bytes).map_err(Into::into)
}

fn stable_capture_id(
    org_id: &str,
    table_name: &str,
    resource_type: &str,
    resource_id: &str,
    action: SqlWriteAction,
    created_at_ms: i64,
    changed_fields: &BTreeMap<String, FieldValue>,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(org_id.as_bytes());
    hasher.update([0]);
    hasher.update(table_name.as_bytes());
    hasher.update([0]);
    hasher.update(resource_type.as_bytes());
    hasher.update([0]);
    hasher.update(resource_id.as_bytes());
    hasher.update([0]);
    hasher.update(action.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(created_at_ms.to_le_bytes());
    hasher.update(
        CAPTURE_SEQUENCE
            .fetch_add(1, Ordering::Relaxed)
            .to_le_bytes(),
    );
    if let Ok(fields) = encode_changed_fields(changed_fields) {
        hasher.update(fields);
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::tempdir;

    #[test]
    fn migration_creates_core_capture_table() {
        let dir = tempdir().expect("tempdir");
        let db = db::init(&dir.path().join("core.db")).expect("db init");
        let conn = db.read().expect("db lock");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='__tentaflow_core_sync_captures'",
                [],
                |row| row.get(0),
            )
            .expect("table exists query");

        assert_eq!(count, 1);
    }

    #[test]
    fn record_core_capture_round_trips_binary_fields() {
        let dir = tempdir().expect("tempdir");
        let db = db::init(&dir.path().join("core.db")).expect("db init");
        let mut conn = db.write().expect("db lock");
        let tx = conn.transaction().expect("begin tx");
        // `actor_user_id` is FK-bound to `user_accounts(id)`, so the referenced
        // user must exist before the capture row is inserted.
        tx.execute(
            "INSERT INTO user_accounts (id, username, password_hash) VALUES ('user-uuid', 'user-uuid', 'h')",
            [],
        )
        .expect("seed actor user");
        let mut fields = BTreeMap::new();
        fields.insert(
            "name".to_string(),
            FieldValue::String("Pipeline sprzedaży".to_string()),
        );
        fields.insert("version".to_string(), FieldValue::I64(3));
        let hlc = HybridLogicalTimestamp {
            wall_time_ms: 1_765_000_000_000,
            logical: 4,
            node_id: "node_a".to_string(),
        };
        let mut capture = CoreWriteCapture::new(
            CoreSyncResourceKind::Flow,
            "org-default",
            "42",
            SqlWriteAction::Update,
            fields,
            Some("user-uuid".to_string()),
            hlc,
            BaselineEpoch {
                counter: 2,
                origin_node: "node_a".to_string(),
            },
        );

        record_core_write_capture(&tx, &capture).expect("record capture");
        tx.commit().expect("commit");

        let loaded = load_core_write_capture(&conn, &capture.capture_id)
            .expect("load capture")
            .expect("capture exists");

        // The capture row does not persist the epoch (it is resolved at drain
        // time), so normalise it before comparing the remaining fields.
        capture.epoch = BaselineEpoch {
            counter: 0,
            origin_node: String::new(),
        };
        assert_eq!(loaded, capture);
    }
}
