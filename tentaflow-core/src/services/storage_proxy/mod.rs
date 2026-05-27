// =============================================================================
// File: services/storage_proxy/mod.rs — binary mesh proxy for central-only storage.
// =============================================================================

mod client;
mod server;

pub use client::{
    DEFAULT_STORAGE_PROXY_TIMEOUT, StorageProxyClient, StorageProxyError, json_to_wire,
    remote_blob_get_chunk, remote_blob_put_chunk, remote_kv_delete, remote_kv_get,
    remote_kv_list, remote_kv_set, remote_sql_exec, remote_sql_query, storage_proxy_client,
    wire_to_json,
};
pub use server::handle_request;
