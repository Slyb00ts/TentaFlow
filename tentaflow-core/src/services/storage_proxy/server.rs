// =============================================================================
// File: services/storage_proxy/server.rs — authority side for central storage.
// =============================================================================

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tentaflow_protocol::mesh::{
    StorageProxyRequestKind, StorageProxyRequestPayload, StorageProxyResponseKind,
    StorageProxyResponsePayload, StorageValueWire,
};

use crate::addon::storage_sql_exec::{exec_for_addon, query_for_addon, query_one_for_addon};
use crate::db::{repository, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;

const MAX_BLOB_PROXY_CHUNK_BYTES: u32 = 1024 * 1024;

pub async fn handle_request(
    iroh: Arc<IrohMeshManager>,
    db: DbPool,
    local_node_id: String,
    from_node_id: String,
    payload: StorageProxyRequestPayload,
) {
    let blob_root = crate::paths::tentaflow_home().to_path_buf();
    let response =
        execute_request_with_blob_root(&db, &local_node_id, &from_node_id, payload, &blob_root);
    match crate::mesh::cbor::encode(&response) {
        Ok(bytes) => {
            if let Err(e) = iroh
                .send_storage_proxy_response(&from_node_id, &bytes)
                .await
            {
                tracing::warn!(peer = %from_node_id, "StorageProxyResponse send failed: {}", e);
            }
        }
        Err(e) => tracing::warn!(peer = %from_node_id, "StorageProxyResponse encode failed: {}", e),
    }
}

#[cfg(test)]
fn execute_request(
    db: &DbPool,
    local_node_id: &str,
    from_node_id: &str,
    payload: StorageProxyRequestPayload,
) -> StorageProxyResponsePayload {
    let blob_root = crate::paths::tentaflow_home().to_path_buf();
    execute_request_with_blob_root(db, local_node_id, from_node_id, payload, &blob_root)
}

fn execute_request_with_blob_root(
    db: &DbPool,
    local_node_id: &str,
    from_node_id: &str,
    payload: StorageProxyRequestPayload,
    blob_root: &Path,
) -> StorageProxyResponsePayload {
    let request_id = payload.request_id.clone();
    let response = match validate_authority(db, local_node_id, &payload) {
        Ok(()) => {
            let proxy_action = match &payload.kind {
                StorageProxyRequestKind::SqlExec { .. } => "write",
                StorageProxyRequestKind::SqlQuery { .. } => "read",
                StorageProxyRequestKind::KvGet { .. } => "read",
                StorageProxyRequestKind::KvSet { .. } => "write",
                StorageProxyRequestKind::KvDelete { .. } => "write",
                StorageProxyRequestKind::KvList { .. } => "read",
                StorageProxyRequestKind::BlobGetChunk { .. } => "read",
                StorageProxyRequestKind::BlobPutChunk { .. } => "write",
            };
            if let Err(message) = validate_proxy_access(db, from_node_id, &payload, proxy_action) {
                return error_response(request_id, local_node_id, message);
            }
            match payload.kind {
                StorageProxyRequestKind::SqlExec { query, params } => {
                    let params = params.into_iter().map(wire_to_json).collect::<Vec<_>>();
                    match exec_for_addon(
                        &payload.org_id,
                        &payload.addon_id,
                        &query,
                        &params,
                        payload.actor_user_id,
                    ) {
                        Ok((rows_affected, last_insert_id)) => StorageProxyResponseKind::SqlExec {
                            rows_affected,
                            last_insert_id,
                        },
                        Err(e) => StorageProxyResponseKind::Error {
                            code: e.kind().to_string(),
                            message: e.to_string(),
                        },
                    }
                }
                StorageProxyRequestKind::SqlQuery {
                    query,
                    params,
                    one,
                    limit,
                } => {
                    let params = params.into_iter().map(wire_to_json).collect::<Vec<_>>();
                    let result = if one {
                        query_one_for_addon(&payload.org_id, &payload.addon_id, &query, &params)
                    } else {
                        query_for_addon(
                            &payload.org_id,
                            &payload.addon_id,
                            &query,
                            &params,
                            limit.map(|v| v as usize),
                        )
                    };
                    match result {
                        Ok(value) => sql_json_to_response(value, one),
                        Err(e) => StorageProxyResponseKind::Error {
                            code: e.kind().to_string(),
                            message: e.to_string(),
                        },
                    }
                }
                StorageProxyRequestKind::KvGet { instance_id, key } => {
                    match kv_get(db, &payload.addon_id, &instance_id, &key) {
                        Ok(value) => StorageProxyResponseKind::KvValue { value },
                        Err(e) => StorageProxyResponseKind::Error {
                            code: "kv_error".to_string(),
                            message: e.to_string(),
                        },
                    }
                }
                StorageProxyRequestKind::KvSet {
                    instance_id,
                    key,
                    value,
                } => match kv_set(
                    db,
                    &payload.org_id,
                    &payload.addon_id,
                    &instance_id,
                    &key,
                    value,
                    payload.actor_user_id,
                ) {
                    Ok(rows_affected) => StorageProxyResponseKind::KvWrite { rows_affected },
                    Err(e) => StorageProxyResponseKind::Error {
                        code: "kv_error".to_string(),
                        message: e.to_string(),
                    },
                },
                StorageProxyRequestKind::KvDelete { instance_id, key } => match kv_delete(
                    db,
                    &payload.org_id,
                    &payload.addon_id,
                    &instance_id,
                    &key,
                    payload.actor_user_id,
                ) {
                    Ok(rows_affected) => StorageProxyResponseKind::KvWrite { rows_affected },
                    Err(e) => StorageProxyResponseKind::Error {
                        code: "kv_error".to_string(),
                        message: e.to_string(),
                    },
                },
                StorageProxyRequestKind::KvList {
                    instance_id,
                    prefix,
                } => match kv_list(db, &payload.addon_id, &instance_id, prefix.as_deref()) {
                    Ok(keys) => StorageProxyResponseKind::KvKeys { keys },
                    Err(e) => StorageProxyResponseKind::Error {
                        code: "kv_error".to_string(),
                        message: e.to_string(),
                    },
                },
                StorageProxyRequestKind::BlobGetChunk {
                    sha256,
                    offset,
                    length,
                } => match blob_get_chunk(blob_root, &sha256, offset, length) {
                    Ok((mime, size_bytes, bytes)) => StorageProxyResponseKind::BlobChunk {
                        sha256,
                        mime,
                        size_bytes,
                        offset,
                        bytes,
                    },
                    Err(e) => StorageProxyResponseKind::Error {
                        code: "blob_error".to_string(),
                        message: e.to_string(),
                    },
                },
                StorageProxyRequestKind::BlobPutChunk {
                    blob_id,
                    sha256,
                    mime,
                    size_bytes,
                    chunk_index,
                    chunk_count,
                    chunk_sha256,
                    bytes,
                } => match blob_put_chunk(
                    db,
                    blob_root,
                    &payload.org_id,
                    &blob_id,
                    &sha256,
                    &mime,
                    size_bytes,
                    chunk_index,
                    chunk_count,
                    &chunk_sha256,
                    bytes,
                    payload.actor_user_id,
                ) {
                    Ok((complete, received_chunks)) => StorageProxyResponseKind::BlobWrite {
                        blob_id,
                        sha256,
                        complete,
                        received_chunks,
                    },
                    Err(e) => StorageProxyResponseKind::Error {
                        code: "blob_error".to_string(),
                        message: e.to_string(),
                    },
                },
            }
        }
        Err(message) => StorageProxyResponseKind::Error {
            code: "authority_denied".to_string(),
            message,
        },
    };
    tracing::debug!(
        peer = %from_node_id,
        request_id = %request_id,
        "StorageProxyRequest handled"
    );
    StorageProxyResponsePayload {
        request_id,
        from_node_id: local_node_id.to_string(),
        kind: response,
    }
}

fn error_response(
    request_id: String,
    local_node_id: &str,
    message: String,
) -> StorageProxyResponsePayload {
    StorageProxyResponsePayload {
        request_id,
        from_node_id: local_node_id.to_string(),
        kind: StorageProxyResponseKind::Error {
            code: "authority_denied".to_string(),
            message,
        },
    }
}

fn validate_authority(
    db: &DbPool,
    local_node_id: &str,
    payload: &StorageProxyRequestPayload,
) -> Result<(), String> {
    let policy = repository::get_effective_sync_policy(
        db,
        &payload.org_id,
        &payload.addon_id,
        &payload.resource_type,
        &payload.resource_id,
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "missing central storage policy".to_string())?;
    if !policy.is_enabled {
        return Err("central storage policy disabled".to_string());
    }
    if !policy.mode.is_authority_backed() {
        return Err(format!(
            "policy mode {} is not authority-backed",
            policy.mode
        ));
    }
    if policy.authority_node_id.as_deref() != Some(local_node_id) {
        return Err("local node is not authority for this resource".to_string());
    }
    Ok(())
}

fn validate_proxy_access(
    db: &DbPool,
    from_node_id: &str,
    payload: &StorageProxyRequestPayload,
    node_action: &str,
) -> Result<(), String> {
    let node_decision = repository::can_node_access_sync_resource(
        db,
        from_node_id,
        &payload.org_id,
        &payload.addon_id,
        &payload.resource_type,
        &payload.resource_id,
        node_action,
    )
    .map_err(|e| e.to_string())?;
    if node_decision.allowed {
        return Ok(());
    }
    Err(format!(
        "node {from_node_id} cannot use {node_action}: {}",
        node_decision.reason
    ))
}

fn sql_json_to_response(value: JsonValue, one: bool) -> StorageProxyResponseKind {
    if one {
        let row = value
            .get("row")
            .and_then(|row| row.as_array())
            .map(|row| row.iter().map(json_to_wire).collect::<Vec<_>>());
        return StorageProxyResponseKind::SqlOne { row };
    }
    let columns = value
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|columns| {
            columns
                .iter()
                .filter_map(|column| column.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rows = value
        .get("rows")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_array())
                .map(|row| row.iter().map(json_to_wire).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    StorageProxyResponseKind::SqlRows { columns, rows }
}

fn json_to_wire(value: &JsonValue) -> StorageValueWire {
    crate::services::storage_proxy::json_to_wire(value)
}

fn wire_to_json(value: StorageValueWire) -> JsonValue {
    crate::services::storage_proxy::wire_to_json(value)
}

fn kv_get(
    db: &DbPool,
    addon_id: &str,
    instance_id: &str,
    key: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let conn = db
        .read()
        .map_err(|e| anyhow::anyhow!("storage proxy kv db lock: {e}"))?;
    let value = conn
        .query_row(
            "SELECT storage_value FROM addon_storage \
             WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
            rusqlite::params![addon_id, instance_id, key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

fn kv_set(
    db: &DbPool,
    org_id: &str,
    addon_id: &str,
    instance_id: &str,
    key: &str,
    value: Vec<u8>,
    actor_user_id: Option<String>,
) -> anyhow::Result<u64> {
    let capture = crate::sync::kv_capture::KvWriteCapture::new(
        org_id,
        addon_id,
        instance_id,
        key,
        Some(value.clone()),
        actor_user_id,
    );
    let mut conn = db
        .write()
        .map_err(|e| anyhow::anyhow!("storage proxy kv db lock: {e}"))?;
    let tx = conn.transaction()?;
    let rows = tx.execute(
        "INSERT OR REPLACE INTO addon_storage \
         (addon_id, instance_id, storage_key, storage_value, value_size_bytes, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        rusqlite::params![addon_id, instance_id, key, &value, value.len() as i64],
    )?;
    crate::sync::kv_capture::record_kv_write_capture(&tx, &capture)?;
    tx.commit()?;
    crate::sync::kv_capture::ledger_kv_capture_now(db, &capture)?;
    Ok(rows as u64)
}

fn kv_delete(
    db: &DbPool,
    org_id: &str,
    addon_id: &str,
    instance_id: &str,
    key: &str,
    actor_user_id: Option<String>,
) -> anyhow::Result<u64> {
    let capture = crate::sync::kv_capture::KvWriteCapture::new(
        org_id,
        addon_id,
        instance_id,
        key,
        None,
        actor_user_id,
    );
    let mut conn = db
        .write()
        .map_err(|e| anyhow::anyhow!("storage proxy kv db lock: {e}"))?;
    let tx = conn.transaction()?;
    let rows = tx.execute(
        "DELETE FROM addon_storage WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key = ?3",
        rusqlite::params![addon_id, instance_id, key],
    )?;
    crate::sync::kv_capture::record_kv_write_capture(&tx, &capture)?;
    tx.commit()?;
    crate::sync::kv_capture::ledger_kv_capture_now(db, &capture)?;
    Ok(rows as u64)
}

fn kv_list(
    db: &DbPool,
    addon_id: &str,
    instance_id: &str,
    prefix: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let conn = db
        .read()
        .map_err(|e| anyhow::anyhow!("storage proxy kv db lock: {e}"))?;
    let keys = if let Some(prefix) = prefix {
        let like_pattern = format!("{prefix}%");
        let mut stmt = conn.prepare(
            "SELECT storage_key FROM addon_storage \
             WHERE addon_id = ?1 AND instance_id = ?2 AND storage_key LIKE ?3 \
             ORDER BY storage_key",
        )?;
        let rows = stmt
            .query_map(
                rusqlite::params![addon_id, instance_id, like_pattern],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    } else {
        let mut stmt = conn.prepare(
            "SELECT storage_key FROM addon_storage \
             WHERE addon_id = ?1 AND instance_id = ?2 \
             ORDER BY storage_key",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![addon_id, instance_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    Ok(keys)
}

fn blob_get_chunk(
    root: &Path,
    sha256: &str,
    offset: u64,
    length: u32,
) -> anyhow::Result<(String, u64, Vec<u8>)> {
    validate_blob_sha(sha256)?;
    if length == 0 {
        anyhow::bail!("blob chunk length must be greater than zero");
    }
    if length > MAX_BLOB_PROXY_CHUNK_BYTES {
        anyhow::bail!("blob chunk length exceeds proxy limit");
    }
    let path = blob_path_for_sha(root, sha256);
    let mut file = std::fs::File::open(&path)?;
    let size_bytes = file.metadata()?.len();
    if offset > size_bytes {
        anyhow::bail!("blob offset is outside file");
    }
    let readable = (offset.saturating_add(length as u64)).min(size_bytes) - offset;
    let mut data = vec![0u8; readable as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut data)?;
    Ok(("application/octet-stream".to_string(), size_bytes, data))
}

fn blob_put_chunk(
    db: &DbPool,
    root: &Path,
    org_id: &str,
    blob_id: &str,
    sha256: &str,
    mime: &str,
    size_bytes: u64,
    chunk_index: u32,
    chunk_count: u32,
    chunk_sha256: &str,
    bytes: Vec<u8>,
    actor_user_id: Option<String>,
) -> anyhow::Result<(bool, u32)> {
    validate_blob_sha(sha256)?;
    validate_blob_sha(chunk_sha256)?;
    if blob_id.is_empty() || blob_id.len() > 128 {
        anyhow::bail!("invalid blob id");
    }
    if mime.is_empty() || mime.len() > 128 {
        anyhow::bail!("invalid blob mime");
    }
    if chunk_count == 0 || chunk_index >= chunk_count {
        anyhow::bail!("invalid blob chunk index");
    }
    if bytes.is_empty() || bytes.len() > MAX_BLOB_PROXY_CHUNK_BYTES as usize {
        anyhow::bail!("blob chunk size exceeds proxy limit");
    }
    if chunk_count as u64 * u64::from(MAX_BLOB_PROXY_CHUNK_BYTES) < size_bytes {
        anyhow::bail!("blob chunk count cannot cover declared size");
    }
    crate::sync::storage_monitor::ensure_large_blob_allowed(size_bytes)?;
    if digest_hex(&bytes) != chunk_sha256 {
        anyhow::bail!("blob chunk sha256 mismatch");
    }

    let upload_dir = blob_upload_dir(root, sha256);
    std::fs::create_dir_all(&upload_dir)?;
    let chunk_path = upload_dir.join(format!("{chunk_index:016}.part"));
    let tmp_path = upload_dir.join(format!(
        "{chunk_index:016}.tmp-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &chunk_path)?;

    let received_chunks = count_blob_chunks(&upload_dir)?;
    if received_chunks < chunk_count {
        return Ok((false, received_chunks));
    }

    let final_path = blob_path_for_sha(root, sha256);
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_final = final_path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    {
        let mut out = std::fs::File::create(&tmp_final)?;
        for idx in 0..chunk_count {
            let chunk = std::fs::read(upload_dir.join(format!("{idx:016}.part")))?;
            std::io::Write::write_all(&mut out, &chunk)?;
        }
        std::io::Write::flush(&mut out)?;
        out.sync_all()?;
    }
    let final_bytes = std::fs::read(&tmp_final)?;
    if final_bytes.len() as u64 != size_bytes {
        let _ = std::fs::remove_file(&tmp_final);
        anyhow::bail!("blob size mismatch");
    }
    if digest_hex(&final_bytes) != sha256 {
        let _ = std::fs::remove_file(&tmp_final);
        anyhow::bail!("blob sha256 mismatch");
    }
    match std::fs::rename(&tmp_final, &final_path) {
        Ok(()) => {}
        Err(e) if final_path.exists() && file_sha_matches(&final_path, sha256)? => {
            let _ = std::fs::remove_file(&tmp_final);
            tracing::debug!("blob proxy reused existing blob after rename race: {}", e);
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_final);
            return Err(e.into());
        }
    }
    let capture = crate::sync::blob_capture::BlobWriteCapture::new(
        org_id,
        blob_id,
        sha256,
        mime,
        size_bytes,
        final_path.to_string_lossy().to_string(),
        actor_user_id,
    );
    {
        let conn = db
            .write()
            .map_err(|e| anyhow::anyhow!("storage proxy blob db lock: {e}"))?;
        crate::sync::blob_capture::record_blob_write_capture(&conn, &capture)?;
    }
    crate::sync::blob_capture::ledger_blob_capture_now(db, &capture)?;
    let _ = std::fs::remove_dir_all(&upload_dir);
    Ok((true, chunk_count))
}

fn count_blob_chunks(upload_dir: &Path) -> anyhow::Result<u32> {
    let mut count = 0u32;
    for entry in std::fs::read_dir(upload_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("part")
        {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn blob_path_for_sha(root: &Path, sha256: &str) -> PathBuf {
    root.join("blobs")
        .join(&sha256[0..2])
        .join(&sha256[2..4])
        .join(format!("{sha256}.bin"))
}

fn blob_upload_dir(root: &Path, sha256: &str) -> PathBuf {
    root.join("sync").join("blob-proxy-uploads").join(sha256)
}

fn validate_blob_sha(sha256: &str) -> anyhow::Result<()> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("invalid blob sha256");
    }
    Ok(())
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn file_sha_matches(path: &Path, expected_sha: &str) -> anyhow::Result<bool> {
    let bytes = std::fs::read(path)?;
    Ok(digest_hex(&bytes) == expected_sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> DbPool {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::db::init(&dir.path().join("test.db")).expect("db")
    }

    fn seed_central_kv_access(db: &DbPool) {
        repository::upsert_sync_node_identity(
            db,
            "authority-node",
            "authority-pub",
            "ed25519",
            "Authority",
            "authority",
            "trusted",
            None,
            "authority",
        )
        .expect("authority node");
        repository::upsert_sync_node_identity(
            db,
            "client-node",
            "client-pub",
            "ed25519",
            "Client",
            "laptop",
            "trusted",
            None,
            "standard",
        )
        .expect("client node");
        repository::upsert_sync_policy(
            db,
            "kv-policy",
            crate::services::org::DEFAULT_ORG_ID,
            "kv-addon",
            Some("addon.kv"),
            Some("kv-resource"),
            "authority_write",
            Some("authority-node"),
            None,
            true,
        )
        .expect("policy");
        repository::grant_sync_explicit_share(
            db,
            crate::services::org::DEFAULT_ORG_ID,
            "kv-addon",
            "addon.kv",
            "kv-resource",
            "node",
            "client-node",
            "read",
            None,
        )
        .expect("read share");
        repository::grant_sync_explicit_share(
            db,
            crate::services::org::DEFAULT_ORG_ID,
            "kv-addon",
            "addon.kv",
            "kv-resource",
            "node",
            "client-node",
            "write",
            None,
        )
        .expect("write share");
    }

    fn seed_central_blob_access(db: &DbPool) {
        repository::upsert_sync_node_identity(
            db,
            "authority-node",
            "authority-pub",
            "ed25519",
            "Authority",
            "authority",
            "trusted",
            None,
            "authority",
        )
        .expect("authority node");
        repository::upsert_sync_node_identity(
            db,
            "client-node",
            "client-pub",
            "ed25519",
            "Client",
            "laptop",
            "trusted",
            None,
            "standard",
        )
        .expect("client node");
        repository::upsert_sync_policy(
            db,
            "blob-policy",
            crate::services::org::DEFAULT_ORG_ID,
            "core",
            Some("core.blob"),
            Some("blob-resource"),
            "authority_write",
            Some("authority-node"),
            None,
            true,
        )
        .expect("policy");
        for action in ["read", "write"] {
            repository::grant_sync_explicit_share(
                db,
                crate::services::org::DEFAULT_ORG_ID,
                "core",
                "core.blob",
                "blob-resource",
                "node",
                "client-node",
                action,
                None,
            )
            .expect("share");
        }
    }

    #[test]
    fn central_kv_proxy_writes_and_reads_on_authority() {
        let db = test_db();
        seed_central_kv_access(&db);

        let set = execute_request(
            &db,
            "authority-node",
            "client-node",
            StorageProxyRequestPayload {
                request_id: "set-1".to_string(),
                from_node_id: "client-node".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                addon_id: "kv-addon".to_string(),
                resource_type: "addon.kv".to_string(),
                resource_id: "kv-resource".to_string(),
                actor_user_id: None,
                kind: StorageProxyRequestKind::KvSet {
                    instance_id: "inst".to_string(),
                    key: "name".to_string(),
                    value: b"value".to_vec(),
                },
            },
        );
        assert!(matches!(
            set.kind,
            StorageProxyResponseKind::KvWrite { rows_affected: 1 }
        ));

        let get = execute_request(
            &db,
            "authority-node",
            "client-node",
            StorageProxyRequestPayload {
                request_id: "get-1".to_string(),
                from_node_id: "client-node".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                addon_id: "kv-addon".to_string(),
                resource_type: "addon.kv".to_string(),
                resource_id: "kv-resource".to_string(),
                actor_user_id: None,
                kind: StorageProxyRequestKind::KvGet {
                    instance_id: "inst".to_string(),
                    key: "name".to_string(),
                },
            },
        );
        match get.kind {
            StorageProxyResponseKind::KvValue { value } => {
                assert_eq!(value, Some(b"value".to_vec()));
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn central_blob_proxy_writes_chunks_and_reads_range_on_authority() {
        let db = test_db();
        let root = tempfile::tempdir().expect("blob root");
        seed_central_blob_access(&db);

        let bytes = b"central-only blob payload".to_vec();
        let sha256 = digest_hex(&bytes);
        let first = bytes[..12].to_vec();
        let second = bytes[12..].to_vec();
        let first_sha = digest_hex(&first);
        let second_sha = digest_hex(&second);

        let first_response = execute_request_with_blob_root(
            &db,
            "authority-node",
            "client-node",
            StorageProxyRequestPayload {
                request_id: "blob-put-1".to_string(),
                from_node_id: "client-node".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                addon_id: "core".to_string(),
                resource_type: "core.blob".to_string(),
                resource_id: "blob-resource".to_string(),
                actor_user_id: None,
                kind: StorageProxyRequestKind::BlobPutChunk {
                    blob_id: "blob-1".to_string(),
                    sha256: sha256.clone(),
                    mime: "application/octet-stream".to_string(),
                    size_bytes: bytes.len() as u64,
                    chunk_index: 0,
                    chunk_count: 2,
                    chunk_sha256: first_sha,
                    bytes: first,
                },
            },
            root.path(),
        );
        assert!(matches!(
            first_response.kind,
            StorageProxyResponseKind::BlobWrite {
                complete: false,
                received_chunks: 1,
                ..
            }
        ));

        let second_response = execute_request_with_blob_root(
            &db,
            "authority-node",
            "client-node",
            StorageProxyRequestPayload {
                request_id: "blob-put-2".to_string(),
                from_node_id: "client-node".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                addon_id: "core".to_string(),
                resource_type: "core.blob".to_string(),
                resource_id: "blob-resource".to_string(),
                actor_user_id: None,
                kind: StorageProxyRequestKind::BlobPutChunk {
                    blob_id: "blob-1".to_string(),
                    sha256: sha256.clone(),
                    mime: "application/octet-stream".to_string(),
                    size_bytes: bytes.len() as u64,
                    chunk_index: 1,
                    chunk_count: 2,
                    chunk_sha256: second_sha,
                    bytes: second,
                },
            },
            root.path(),
        );
        assert!(matches!(
            second_response.kind,
            StorageProxyResponseKind::BlobWrite {
                complete: true,
                received_chunks: 2,
                ..
            }
        ));

        let get = execute_request_with_blob_root(
            &db,
            "authority-node",
            "client-node",
            StorageProxyRequestPayload {
                request_id: "blob-get".to_string(),
                from_node_id: "client-node".to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                addon_id: "core".to_string(),
                resource_type: "core.blob".to_string(),
                resource_id: "blob-resource".to_string(),
                actor_user_id: None,
                kind: StorageProxyRequestKind::BlobGetChunk {
                    sha256,
                    offset: 8,
                    length: 10,
                },
            },
            root.path(),
        );
        match get.kind {
            StorageProxyResponseKind::BlobChunk { bytes: got, .. } => {
                assert_eq!(got, b"only blob ".to_vec());
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
