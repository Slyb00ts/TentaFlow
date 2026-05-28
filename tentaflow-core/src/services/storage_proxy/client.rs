// =============================================================================
// File: services/storage_proxy/client.rs — requester side for central storage.
// =============================================================================

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;
use dashmap::DashMap;
use serde_json::Value as JsonValue;
use thiserror::Error;
use tokio::sync::oneshot;
use uuid::Uuid;

use tentaflow_protocol::mesh::{
    StorageProxyRequestKind, StorageProxyRequestPayload, StorageProxyResponseKind,
    StorageProxyResponsePayload, StorageValueWire,
};

use crate::mesh::iroh_manager::IrohMeshManager;

pub const DEFAULT_STORAGE_PROXY_TIMEOUT: Duration = Duration::from_secs(10);

static STORAGE_PROXY_CLIENT: OnceLock<Arc<StorageProxyClient>> = OnceLock::new();

pub fn storage_proxy_client() -> &'static Arc<StorageProxyClient> {
    STORAGE_PROXY_CLIENT.get_or_init(|| Arc::new(StorageProxyClient::new()))
}

#[derive(Debug, Error)]
pub enum StorageProxyError {
    #[error("storage proxy request timed out after {0:?}")]
    Timeout(Duration),
    #[error("failed to encode StorageProxyRequest: {0}")]
    Encode(String),
    #[error("failed to send StorageProxyRequest to {peer}: {source}")]
    Send {
        peer: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("pending storage proxy waiter dropped before response arrived")]
    OneshotDropped,
    #[error("authority returned {code}: {message}")]
    Remote { code: String, message: String },
    #[error("authority returned unexpected storage proxy response")]
    UnexpectedResponse,
}

pub struct StorageProxyClient {
    pending: Arc<DashMap<String, oneshot::Sender<StorageProxyResponsePayload>>>,
}

impl StorageProxyClient {
    fn new() -> Self {
        Self {
            pending: Arc::new(DashMap::new()),
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn register(&self, request_id: &str) -> oneshot::Receiver<StorageProxyResponsePayload> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(request_id.to_string(), tx);
        rx
    }

    fn cancel(&self, request_id: &str) {
        self.pending.remove(request_id);
    }

    pub fn handle_response(&self, payload: StorageProxyResponsePayload) {
        if let Some((_, tx)) = self.pending.remove(&payload.request_id) {
            let _ = tx.send(payload);
        }
    }
}

pub async fn remote_sql_exec(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<(u64, i64), StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::SqlExec { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::SqlExec {
            rows_affected,
            last_insert_id,
        } => Ok((rows_affected, last_insert_id)),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

pub async fn remote_sql_query(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<JsonValue, StorageProxyError> {
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::SqlRows { columns, rows } => Ok(serde_json::json!({
            "columns": columns,
            "rows": rows.into_iter().map(row_to_json).collect::<Vec<_>>(),
        })),
        StorageProxyResponseKind::SqlOne { row } => Ok(serde_json::json!({
            "row": row.map(row_to_json).map(JsonValue::Array).unwrap_or(JsonValue::Null),
        })),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

pub async fn remote_kv_get(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::KvGet { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::KvValue { value } => Ok(value),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

pub async fn remote_kv_set(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<u64, StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::KvSet { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    remote_kv_write(iroh, authority_node_id, request, timeout).await
}

pub async fn remote_kv_delete(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<u64, StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::KvDelete { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    remote_kv_write(iroh, authority_node_id, request, timeout).await
}

pub async fn remote_kv_list(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<Vec<String>, StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::KvList { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::KvKeys { keys } => Ok(keys),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

pub async fn remote_blob_get_chunk(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<(String, String, u64, u64, Vec<u8>), StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::BlobGetChunk { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::BlobChunk {
            sha256,
            mime,
            size_bytes,
            offset,
            bytes,
        } => Ok((sha256, mime, size_bytes, offset, bytes)),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

pub async fn remote_blob_put_chunk(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<(String, String, bool, u32), StorageProxyError> {
    if !matches!(request.kind, StorageProxyRequestKind::BlobPutChunk { .. }) {
        return Err(StorageProxyError::UnexpectedResponse);
    }
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::BlobWrite {
            blob_id,
            sha256,
            complete,
            received_chunks,
        } => Ok((blob_id, sha256, complete, received_chunks)),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

async fn remote_kv_write(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<u64, StorageProxyError> {
    let response = send_and_wait(iroh, authority_node_id, request, timeout).await?;
    match response.kind {
        StorageProxyResponseKind::KvWrite { rows_affected } => Ok(rows_affected),
        StorageProxyResponseKind::Error { code, message } => {
            Err(StorageProxyError::Remote { code, message })
        }
        _ => Err(StorageProxyError::UnexpectedResponse),
    }
}

async fn send_and_wait(
    iroh: &IrohMeshManager,
    authority_node_id: &str,
    mut request: StorageProxyRequestPayload,
    timeout: Duration,
) -> Result<StorageProxyResponsePayload, StorageProxyError> {
    let client = storage_proxy_client();
    request.request_id = format!("sp-{}", Uuid::new_v4());
    let request_id = request.request_id.clone();
    let rx = client.register(&request_id);
    let bytes = crate::mesh::cbor::encode(&request).map_err(|e| {
        client.cancel(&request_id);
        StorageProxyError::Encode(e.to_string())
    })?;
    if let Err(e) = iroh
        .send_storage_proxy_request(authority_node_id, &bytes)
        .await
    {
        client.cancel(&request_id);
        return Err(StorageProxyError::Send {
            peer: authority_node_id.to_string(),
            source: e,
        });
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => {
            client.cancel(&request_id);
            Err(StorageProxyError::OneshotDropped)
        }
        Err(_) => {
            client.cancel(&request_id);
            Err(StorageProxyError::Timeout(timeout))
        }
    }
}

pub fn json_to_wire(value: &JsonValue) -> StorageValueWire {
    match value {
        JsonValue::Null => StorageValueWire::Null,
        JsonValue::Bool(v) => StorageValueWire::Bool(*v),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                StorageValueWire::I64(i)
            } else {
                StorageValueWire::F64(n.as_f64().unwrap_or_default())
            }
        }
        JsonValue::String(s) => StorageValueWire::Text(s.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => StorageValueWire::Text(value.to_string()),
    }
}

fn row_to_json(row: Vec<StorageValueWire>) -> Vec<JsonValue> {
    row.into_iter().map(wire_to_json).collect()
}

pub fn wire_to_json(value: StorageValueWire) -> JsonValue {
    match value {
        StorageValueWire::Null => JsonValue::Null,
        StorageValueWire::Bool(v) => JsonValue::Bool(v),
        StorageValueWire::I64(v) => JsonValue::from(v),
        StorageValueWire::F64(v) => JsonValue::from(v),
        StorageValueWire::Text(v) => JsonValue::String(v),
        StorageValueWire::Bytes(v) => {
            JsonValue::String(base64::engine::general_purpose::STANDARD.encode(v))
        }
    }
}
