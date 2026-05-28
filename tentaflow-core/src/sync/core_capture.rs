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

use super::core_registry::{CoreSyncResourceKind, descriptor_for_kind};
use super::ledger::FieldValue;
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
    pub actor_user_id: Option<i64>,
    pub created_at_ms: i64,
}

impl CoreWriteCapture {
    pub fn new(
        kind: CoreSyncResourceKind,
        org_id: impl Into<String>,
        resource_id: impl Into<String>,
        action: SqlWriteAction,
        changed_fields: BTreeMap<String, FieldValue>,
        actor_user_id: Option<i64>,
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
          changed_fields_blob, actor_user_id, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                changed_fields_blob, actor_user_id, created_at_ms \
         FROM __tentaflow_core_sync_captures WHERE capture_id = ?1",
        rusqlite::params![capture_id],
        |row| {
            let action_raw: String = row.get(6)?;
            let changed_fields_blob: Vec<u8> = row.get(7)?;
            let action = SqlWriteAction::from_str(&action_raw)
                .map_err(|e| rusqlite_decode_error(e.to_string()))?;
            let changed_fields = decode_changed_fields(&changed_fields_blob)
                .map_err(|e| rusqlite_decode_error(e.to_string()))?;
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
                created_at_ms: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn drain_pending_core_captures(pool: &crate::db::DbPool, limit: usize) -> Result<usize> {
    let captures = {
        let conn = pool
            .lock()
            .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
        load_pending_core_captures(&conn, limit)?
    };
    let mut drained = 0usize;
    for capture in captures {
        match super::runtime::record_core_capture(capture.clone()) {
            Ok(Some(record)) => {
                let mut conn = pool
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
                mark_core_capture_status(
                    &mut conn,
                    &capture.capture_id,
                    "ledgered",
                    Some(record.op_id.to_hex()),
                    None,
                )?;
                drained += 1;
            }
            Ok(None) => break,
            Err(e) => {
                let mut conn = pool
                    .lock()
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
    hasher.update(CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed).to_le_bytes());
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
        let conn = db.lock().expect("db lock");
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
        let mut conn = db.lock().expect("db lock");
        let tx = conn.transaction().expect("begin tx");
        let mut fields = BTreeMap::new();
        fields.insert(
            "name".to_string(),
            FieldValue::String("Pipeline sprzedaży".to_string()),
        );
        fields.insert("version".to_string(), FieldValue::I64(3));
        let capture = CoreWriteCapture::new(
            CoreSyncResourceKind::Flow,
            "org-default",
            "42",
            SqlWriteAction::Update,
            fields,
            Some(1),
        );

        record_core_write_capture(&tx, &capture).expect("record capture");
        tx.commit().expect("commit");

        let loaded = load_core_write_capture(&conn, &capture.capture_id)
            .expect("load capture")
            .expect("capture exists");

        assert_eq!(loaded, capture);
    }
}
