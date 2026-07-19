// =============================================================================
// File: storage.rs
// Purpose: Admin-side binary protocol for Ustawienia → Magazyn danych:
//          storage-category overview (paths + sizes + disk), directory
//          browsing for the picker tree, folder creation, and data
//          migration (live move with service pause / boot-time pending
//          move for restart-only categories). Packed into a single
//          `StorageAdminPayload` inner enum so the whole surface burns one
//          `MessageBody` discriminant slot (same pack pattern as
//          camera.rs / profiling.rs).
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// One storage category row for the settings screen.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageCategoryInfo {
    /// Stable key = the `*_dir` setting name (`models_dir`, `data_dir`, ...).
    pub key: String,
    /// Currently effective absolute path.
    pub path: String,
    /// Default path under the shared root (shown when the user resets).
    pub default_path: String,
    /// Whether a non-default override is active.
    pub overridden: bool,
    /// Recursive size in bytes (computed server-side, best-effort).
    pub size_bytes: u64,
    /// True = the category migrates live; false = change applies on restart.
    pub live_migratable: bool,
    /// Pending boot migration target, if one is scheduled (restart categories).
    pub pending_path: Option<String>,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageOverviewResponse {
    pub categories: Vec<StorageCategoryInfo>,
    /// Shared root (`tentaflow_home()`), for display.
    pub root: String,
    /// Total bytes of the filesystem holding the shared root.
    pub disk_total_bytes: u64,
    /// Available bytes of that filesystem.
    pub disk_available_bytes: u64,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageBrowseRequest {
    /// Absolute directory to list. Empty = filesystem roots ("/" plus the
    /// shared root's parents on Unix; drive letters on Windows).
    pub path: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageDirEntry {
    pub name: String,
    pub path: String,
    /// True when the directory contains at least one subdirectory.
    pub has_children: bool,
    /// True when the server process can create files inside.
    pub writable: bool,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageBrowseResponse {
    pub path: String,
    pub entries: Vec<StorageDirEntry>,
    /// Free bytes on the filesystem holding `path`.
    pub free_bytes: u64,
    /// Total bytes on the filesystem holding `path`.
    pub total_bytes: u64,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageCreateDirRequest {
    /// Existing parent directory.
    pub parent: String,
    /// New directory name (single component, validated server-side).
    pub name: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageCreateDirResponse {
    pub path: String,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageMigrateRequest {
    /// Category key (`models_dir`, `blobs_dir`, ...).
    pub key: String,
    /// New absolute directory.
    pub new_path: String,
    /// True = move existing data; false = just switch the path.
    pub move_data: bool,
}

#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct StorageMigrateResponse {
    /// How the request was handled:
    /// - "live"     — background job started; watch progress via the deploy
    ///                log stream using `job_id`.
    /// - "switched" — path switched without moving data (effective now).
    /// - "boot"     — pending move recorded; applies at next start.
    pub mode: String,
    /// Progress-stream id for `mode == "live"` (deployment log stream).
    pub job_id: Option<String>,
}

/// Wewnętrzny enum spinający całą powierzchnię Magazynu danych w jeden
/// wariant `MessageBody::StorageAdminBody`.
#[derive(SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum StorageAdminPayload {
    OverviewRequest,
    OverviewResponse(StorageOverviewResponse),
    BrowseRequest(StorageBrowseRequest),
    BrowseResponse(StorageBrowseResponse),
    CreateDirRequest(StorageCreateDirRequest),
    CreateDirResponse(StorageCreateDirResponse),
    MigrateRequest(StorageMigrateRequest),
    MigrateResponse(StorageMigrateResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn storage_admin_roundtrip() {
        let body = MessageBody::StorageAdminBody(StorageAdminPayload::MigrateRequest(
            StorageMigrateRequest {
                key: "models_dir".into(),
                new_path: "/mnt/storage/tentaflow-models".into(),
                move_data: true,
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(body, decoded);

        let resp = MessageBody::StorageAdminBody(StorageAdminPayload::OverviewResponse(
            StorageOverviewResponse {
                categories: vec![StorageCategoryInfo {
                    key: "models_dir".into(),
                    path: "/data/models".into(),
                    default_path: "/data/models".into(),
                    overridden: false,
                    size_bytes: 42,
                    live_migratable: true,
                    pending_path: None,
                }],
                root: "/data".into(),
                disk_total_bytes: 100,
                disk_available_bytes: 50,
            },
        ));
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded: MessageBody = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(resp, decoded);
    }
}
