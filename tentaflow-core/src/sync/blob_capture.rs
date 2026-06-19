// =============================================================================
// Plik: sync/blob_capture.rs
// Opis: Trwaly capture plikow BlobStore do replikacji przez Sync Ledger.
//       Capture trzyma metadane i sciezke, a runtime weryfikuje hash pliku.
// =============================================================================

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlobWriteCapture {
    pub capture_id: String,
    pub org_id: String,
    pub blob_id: String,
    pub sha256: String,
    pub mime: String,
    pub size_bytes: u64,
    pub file_path: String,
    pub actor_user_id: Option<String>,
    pub created_at_ms: i64,
}

impl BlobWriteCapture {
    pub fn new(
        org_id: impl Into<String>,
        blob_id: impl Into<String>,
        sha256: impl Into<String>,
        mime: impl Into<String>,
        size_bytes: u64,
        file_path: impl Into<String>,
        actor_user_id: Option<String>,
    ) -> Self {
        let org_id = org_id.into();
        let blob_id = blob_id.into();
        let sha256 = sha256.into();
        let mime = mime.into();
        let file_path = file_path.into();
        let created_at_ms = super::runtime::now_ms();
        let capture_id = stable_capture_id(&org_id, &sha256, &mime, size_bytes, created_at_ms);
        Self {
            capture_id,
            org_id,
            blob_id,
            sha256,
            mime,
            size_bytes,
            file_path,
            actor_user_id,
            created_at_ms,
        }
    }
}

pub fn record_blob_write_capture(
    conn: &rusqlite::Connection,
    capture: &BlobWriteCapture,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO __tentaflow_blob_sync_captures \
         (capture_id, org_id, blob_id, sha256, mime, size_bytes, file_path, actor_user_id, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            &capture.capture_id,
            &capture.org_id,
            &capture.blob_id,
            &capture.sha256,
            &capture.mime,
            capture.size_bytes as i64,
            &capture.file_path,
            capture.actor_user_id,
            capture.created_at_ms,
        ],
    )?;
    Ok(())
}

pub fn load_blob_write_capture(
    conn: &rusqlite::Connection,
    capture_id: &str,
) -> Result<Option<BlobWriteCapture>> {
    conn.query_row(
        "SELECT capture_id, org_id, blob_id, sha256, mime, size_bytes, file_path, actor_user_id, created_at_ms \
         FROM __tentaflow_blob_sync_captures WHERE capture_id = ?1",
        rusqlite::params![capture_id],
        |row| {
            let size_bytes: i64 = row.get(5)?;
            Ok(BlobWriteCapture {
                capture_id: row.get(0)?,
                org_id: row.get(1)?,
                blob_id: row.get(2)?,
                sha256: row.get(3)?,
                mime: row.get(4)?,
                size_bytes: size_bytes.max(0) as u64,
                file_path: row.get(6)?,
                actor_user_id: row.get(7)?,
                created_at_ms: row.get(8)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn ledger_blob_capture_now(pool: &crate::db::DbPool, capture: &BlobWriteCapture) -> Result<()> {
    match super::runtime::record_blob_capture(capture.clone()) {
        Ok(Some(record)) => {
            let mut conn = pool
                .write()
                .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
            mark_blob_capture_status(
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
            mark_blob_capture_status(
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

pub fn drain_pending_blob_captures(pool: &crate::db::DbPool, limit: usize) -> Result<usize> {
    let captures = {
        let conn = pool
            .read()
            .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
        load_pending_blob_captures(&conn, limit)?
    };
    let mut drained = 0usize;
    for capture in captures {
        match super::runtime::record_blob_capture(capture.clone()) {
            Ok(Some(record)) => {
                let mut conn = pool
                    .write()
                    .map_err(|e| anyhow::anyhow!("Blad blokady bazy: {}", e))?;
                mark_blob_capture_status(
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
                mark_blob_capture_status(
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

fn load_pending_blob_captures(
    conn: &rusqlite::Connection,
    limit: usize,
) -> Result<Vec<BlobWriteCapture>> {
    let mut stmt = conn.prepare_cached(
        "SELECT capture_id FROM __tentaflow_blob_sync_captures \
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
        if let Some(capture) = load_blob_write_capture(conn, &capture_id)? {
            captures.push(capture);
        }
    }
    Ok(captures)
}

fn mark_blob_capture_status(
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
        "UPDATE __tentaflow_blob_sync_captures \
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

fn stable_capture_id(
    org_id: &str,
    sha256: &str,
    mime: &str,
    size_bytes: u64,
    created_at_ms: i64,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(org_id.as_bytes());
    hasher.update([0]);
    hasher.update(sha256.as_bytes());
    hasher.update([0]);
    hasher.update(mime.as_bytes());
    hasher.update([0]);
    hasher.update(size_bytes.to_le_bytes());
    hasher.update([0]);
    hasher.update(created_at_ms.to_le_bytes());
    hex::encode(hasher.finalize())
}
