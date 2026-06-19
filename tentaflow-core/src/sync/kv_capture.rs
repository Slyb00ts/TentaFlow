// =============================================================================
// Plik: sync/kv_capture.rs
// Opis: Trwaly capture zapisow addonowego KV przed wyslaniem do Sync Ledger.
//       Zapisy pozostaja w SQLite do czasu potwierdzonego appendu do ledgera.
// =============================================================================

use std::io;

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvWriteCapture {
    pub capture_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub instance_id: String,
    pub key: String,
    pub value: Option<Vec<u8>>,
    pub actor_user_id: Option<String>,
    pub created_at_ms: i64,
}

impl KvWriteCapture {
    pub fn new(
        org_id: impl Into<String>,
        addon_id: impl Into<String>,
        instance_id: impl Into<String>,
        key: impl Into<String>,
        value: Option<Vec<u8>>,
        actor_user_id: Option<String>,
    ) -> Self {
        let org_id = org_id.into();
        let addon_id = addon_id.into();
        let instance_id = instance_id.into();
        let key = key.into();
        let created_at_ms = super::runtime::now_ms();
        let capture_id = stable_capture_id(
            &org_id,
            &addon_id,
            &instance_id,
            &key,
            value.as_deref(),
            created_at_ms,
        );
        Self {
            capture_id,
            org_id,
            addon_id,
            instance_id,
            key,
            value,
            actor_user_id,
            created_at_ms,
        }
    }
}

pub fn record_kv_write_capture(
    tx: &rusqlite::Transaction<'_>,
    capture: &KvWriteCapture,
) -> Result<()> {
    let action = if capture.value.is_some() {
        "set"
    } else {
        "delete"
    };
    tx.execute(
        "INSERT INTO __tentaflow_kv_sync_captures \
         (capture_id, org_id, addon_id, instance_id, storage_key, action, storage_value, \
          actor_user_id, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            &capture.capture_id,
            &capture.org_id,
            &capture.addon_id,
            &capture.instance_id,
            &capture.key,
            action,
            capture.value.as_deref(),
            capture.actor_user_id,
            capture.created_at_ms,
        ],
    )?;
    Ok(())
}

pub fn load_kv_write_capture(
    conn: &rusqlite::Connection,
    capture_id: &str,
) -> Result<Option<KvWriteCapture>> {
    conn.query_row(
        "SELECT capture_id, org_id, addon_id, instance_id, storage_key, action, storage_value, \
                actor_user_id, created_at_ms \
         FROM __tentaflow_kv_sync_captures WHERE capture_id = ?1",
        rusqlite::params![capture_id],
        |row| {
            let action: String = row.get(5)?;
            let value: Option<Vec<u8>> = row.get(6)?;
            let value = match action.as_str() {
                "set" => value,
                "delete" => None,
                other => return Err(rusqlite_decode_error(format!("unknown kv action: {other}"))),
            };
            Ok(KvWriteCapture {
                capture_id: row.get(0)?,
                org_id: row.get(1)?,
                addon_id: row.get(2)?,
                instance_id: row.get(3)?,
                key: row.get(4)?,
                value,
                actor_user_id: row.get(7)?,
                created_at_ms: row.get(8)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn ledger_kv_capture_now(pool: &crate::db::DbPool, capture: &KvWriteCapture) -> Result<()> {
    match super::runtime::record_kv_capture(capture.clone()) {
        Ok(Some(record)) => {
            let mut conn = pool
                .write()
                .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
            mark_kv_capture_status(
                &mut conn,
                &capture.capture_id,
                "ledgered",
                Some(record.op_id.to_hex()),
                None,
            )?;
        }
        Ok(None) => {}
        Err(e) => {
            let mut conn = pool
                .write()
                .map_err(|lock| anyhow::anyhow!("Blad blokady bazy: {}", lock))?;
            mark_kv_capture_status(
                &mut conn,
                &capture.capture_id,
                "error",
                None,
                Some(&e.to_string()),
            )?;
        }
    }
    Ok(())
}

pub fn drain_pending_kv_captures(pool: &crate::db::DbPool, limit: usize) -> Result<usize> {
    let captures = {
        let conn = pool
            .read()
            .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
        load_pending_kv_captures(&conn, limit)?
    };
    let mut drained = 0usize;
    for capture in captures {
        match super::runtime::record_kv_capture(capture.clone()) {
            Ok(Some(record)) => {
                let mut conn = pool
                    .write()
                    .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
                mark_kv_capture_status(
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
                    .write()
                    .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
                mark_kv_capture_status(
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

fn load_pending_kv_captures(
    conn: &rusqlite::Connection,
    limit: usize,
) -> Result<Vec<KvWriteCapture>> {
    let mut stmt = conn.prepare_cached(
        "SELECT capture_id FROM __tentaflow_kv_sync_captures \
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
        if let Some(capture) = load_kv_write_capture(conn, &capture_id)? {
            captures.push(capture);
        }
    }
    Ok(captures)
}

fn mark_kv_capture_status(
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
        "UPDATE __tentaflow_kv_sync_captures \
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

fn stable_capture_id(
    org_id: &str,
    addon_id: &str,
    instance_id: &str,
    key: &str,
    value: Option<&[u8]>,
    created_at_ms: i64,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(org_id.as_bytes());
    hasher.update([0]);
    hasher.update(addon_id.as_bytes());
    hasher.update([0]);
    hasher.update(instance_id.as_bytes());
    hasher.update([0]);
    hasher.update(key.as_bytes());
    hasher.update([0]);
    hasher.update(created_at_ms.to_le_bytes());
    hasher.update([0]);
    match value {
        Some(bytes) => {
            hasher.update([1]);
            hasher.update(bytes);
        }
        None => hasher.update([0]),
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::tempdir;

    #[test]
    fn migration_creates_kv_capture_table() {
        let dir = tempdir().expect("tempdir");
        let db = db::init(&dir.path().join("core.db")).expect("db init");
        let conn = db.read().expect("db lock");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='__tentaflow_kv_sync_captures'",
                [],
                |row| row.get(0),
            )
            .expect("table exists query");

        assert_eq!(count, 1);
    }

    #[test]
    fn record_kv_capture_round_trips_binary_value() {
        let dir = tempdir().expect("tempdir");
        let db = db::init(&dir.path().join("core.db")).expect("db init");
        let mut conn = db.write().expect("db lock");
        // actor_user_id has an FK to user_accounts(id); seed the referenced row.
        conn.execute(
            "INSERT INTO user_accounts (id, username, password_hash) VALUES ('1','test-user','x')",
            [],
        )
        .expect("seed actor user");
        let tx = conn.transaction().expect("begin tx");
        let capture = KvWriteCapture::new(
            "org-default",
            "kv-addon",
            "inst-1",
            "settings/theme",
            Some(vec![0, 1, 2, 255]),
            Some("1".to_string()),
        );

        record_kv_write_capture(&tx, &capture).expect("record capture");
        tx.commit().expect("commit");

        let loaded = load_kv_write_capture(&conn, &capture.capture_id)
            .expect("load capture")
            .expect("capture exists");

        assert_eq!(loaded, capture);
    }
}
