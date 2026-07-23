// =============================================================================
// File: mesh/recordings_pull.rs
// Purpose: INITIATOR (node A) side of cross-node camera recordings pull. A asks a
//          PAIRED node B for its recordings list, then pulls selected recording
//          files from B over the mesh. Mirrors the `WebResearch` mesh-command
//          round trip (list) and the `MlArtifactPushTo` file-stream (pull):
//            - `list_remote`  → `MeshCommandType::CameraRecordingsList`  (~30s);
//            - `pull_remote`  → `MeshCommandType::CameraRecordingPull` + files
//               arriving over ALPN_ARTIFACT into a dedicated temp dir.
//          The DB-facing B side lives in `mesh::command_executor` (behind the
//          same trust gate as every other mesh command); the file transport +
//          path-containment validation live in `ml_studio::mesh_artifact`.
// =============================================================================

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::mesh::iroh_manager::IrohMeshManager;

/// One recording as advertised by a remote node. Timestamps are unix
/// MILLISECONDS on the wire (the recordings table stores seconds; both sides
/// convert at the boundary). Serialized as JSON inside the mesh command payload
/// so this crate stays free of the recordings DB types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRecordingItem {
    pub recording_ref: String,
    pub kind: String,
    pub camera_id: String,
    pub created_at_ms: i64,
    pub duration_ms: Option<i64>,
    pub file_size_bytes: i64,
    pub plate_text: Option<String>,
    pub adr_text: Option<String>,
}

/// Filters for a remote recordings listing. `limit` is clamped by the receiver.
/// Timestamps are unix MILLISECONDS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRecordingFilters {
    pub camera_id: Option<String>,
    pub date_from_ms: Option<i64>,
    pub date_to_ms: Option<i64>,
    pub limit: u32,
}

/// Hard cap on how many recordings one pull may request. Bounds the number of
/// ALPN_ARTIFACT streams a single command can trigger on the source node.
pub const MAX_REFS_PER_PULL: usize = 64;

/// Round-trip budget for the list command (mirrors other read-only mesh commands).
const LIST_TIMEOUT_SECS: u64 = 30;

/// Budget for the pull command. The source node streams every file synchronously
/// before returning the confirmation, so this covers the full transfer of up to
/// `MAX_REFS_PER_PULL` clips (each bounded by `mesh_artifact::MAX_RECORDING_BYTES`).
const PULL_TIMEOUT_SECS: u64 = 1800;

/// After the pull command confirms, the files have already been stored by the
/// local ALPN_ARTIFACT accept loop. This short bounded poll only absorbs the
/// last-writer ordering between B's confirmation and A's own store task.
const FILE_SETTLE_TIMEOUT: Duration = Duration::from_secs(15);
const FILE_SETTLE_POLL: Duration = Duration::from_millis(200);

/// Startup-wired context so the free-function interface (no `ctx`) can reach the
/// mesh manager, matching the `robot_dispatch` global-context pattern.
#[derive(Clone)]
pub struct RecordingsPullContext {
    pub iroh: Arc<IrohMeshManager>,
    pub local_node_id: String,
}

static PULL_CTX: OnceLock<RwLock<Option<RecordingsPullContext>>> = OnceLock::new();

fn pull_ctx_cell() -> &'static RwLock<Option<RecordingsPullContext>> {
    PULL_CTX.get_or_init(|| RwLock::new(None))
}

/// Install the global recordings-pull context (startup wiring). Replaces any
/// prior value so a re-init does not leave a stale node id behind.
pub fn set_context(ctx: RecordingsPullContext) {
    *pull_ctx_cell().write() = Some(ctx);
}

fn context() -> anyhow::Result<RecordingsPullContext> {
    pull_ctx_cell()
        .read()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("recordings pull context not initialized"))
}

/// A: ask paired node B for its recordings list (mesh command, ~30s).
pub async fn list_remote(
    node_id: &str,
    filters: RemoteRecordingFilters,
) -> anyhow::Result<Vec<RemoteRecordingItem>> {
    use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};
    let ctx = context()?;
    let filters_json = serde_json::to_string(&filters)
        .map_err(|e| anyhow::anyhow!("serialize recordings filters: {e}"))?;
    let resp = ctx
        .iroh
        .send_command_and_wait(
            node_id,
            MeshCommandType::CameraRecordingsList { filters_json },
            LIST_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| anyhow::anyhow!("recordings list command to {node_id}: {e}"))?;
    if !resp.ok {
        anyhow::bail!(
            "recordings list on {node_id} failed: {}",
            resp.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    match resp.payload {
        MeshCommandResponsePayload::CameraRecordingsListResult { recordings_json } => {
            serde_json::from_str::<Vec<RemoteRecordingItem>>(&recordings_json)
                .map_err(|e| anyhow::anyhow!("parse remote recordings list: {e}"))
        }
        _ => anyhow::bail!("unexpected payload for CameraRecordingsList"),
    }
}

/// A: pull selected recording files from B into a local temp dir; returns
/// `(ref, local_path, item)` per pulled file. Files land under
/// `paths::mesh_recordings_pull_dir()`; the caller deletes them after import.
///
/// Metadata is resolved from a listing of B (up to 1000 newest recordings). Refs
/// the caller selected from an earlier `list_remote` are therefore resolvable;
/// a ref absent from that listing (purged / outside the newest window) is
/// reported as an error rather than returned without metadata.
pub async fn pull_remote(
    node_id: &str,
    refs: &[String],
) -> anyhow::Result<Vec<(String, PathBuf, RemoteRecordingItem)>> {
    use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};
    let ctx = context()?;
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    if refs.len() > MAX_REFS_PER_PULL {
        anyhow::bail!(
            "pull requests {} recordings, max is {}",
            refs.len(),
            MAX_REFS_PER_PULL
        );
    }

    // Resolve metadata for the requested refs from B's listing.
    let listing = list_remote(
        node_id,
        RemoteRecordingFilters {
            camera_id: None,
            date_from_ms: None,
            date_to_ms: None,
            limit: 1000,
        },
    )
    .await?;
    let mut by_ref: std::collections::HashMap<String, RemoteRecordingItem> = listing
        .into_iter()
        .map(|i| (i.recording_ref.clone(), i))
        .collect();
    let missing: Vec<&String> = refs.iter().filter(|r| !by_ref.contains_key(*r)).collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "recordings not found on {node_id}: {}",
            missing
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let pull_dir = crate::paths::mesh_recordings_pull_dir();
    std::fs::create_dir_all(&pull_dir)
        .map_err(|e| anyhow::anyhow!("create recordings pull dir: {e}"))?;

    // Ask B to stream the files back to us; B returns which refs it streamed.
    let resp = ctx
        .iroh
        .send_command_and_wait(
            node_id,
            MeshCommandType::CameraRecordingPull {
                recording_refs: refs.to_vec(),
                target_node_id: ctx.local_node_id.clone(),
            },
            PULL_TIMEOUT_SECS,
        )
        .await
        .map_err(|e| anyhow::anyhow!("recordings pull command to {node_id}: {e}"))?;
    if !resp.ok {
        anyhow::bail!(
            "recordings pull on {node_id} failed: {}",
            resp.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    let pulled_refs = match resp.payload {
        MeshCommandResponsePayload::CameraRecordingPullResult { pulled_refs } => pulled_refs,
        _ => anyhow::bail!("unexpected payload for CameraRecordingPull"),
    };

    // Files travelled over ALPN_ARTIFACT and were stored as `<ref>.<ext>` by the
    // local accept loop. Resolve each confirmed ref to its landed file.
    let mut out = Vec::with_capacity(pulled_refs.len());
    for rec_ref in &pulled_refs {
        let Some(item) = by_ref.remove(rec_ref) else {
            anyhow::bail!("node {node_id} streamed an unrequested ref: {rec_ref}");
        };
        let path = await_landed_file(&pull_dir, rec_ref).await?;
        out.push((rec_ref.clone(), path, item));
    }
    Ok(out)
}

/// Polls `pull_dir` for a file named `<ref>` or `<ref>.<ext>` until it appears or
/// the settle timeout elapses. The confirmation already implies the store
/// completed; this only tolerates the last-writer race between the two tasks.
async fn await_landed_file(pull_dir: &std::path::Path, rec_ref: &str) -> anyhow::Result<PathBuf> {
    let deadline = tokio::time::Instant::now() + FILE_SETTLE_TIMEOUT;
    loop {
        if let Some(p) = find_landed_file(pull_dir, rec_ref) {
            return Ok(p);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("pulled recording {rec_ref} never landed in {}", pull_dir.display());
        }
        tokio::time::sleep(FILE_SETTLE_POLL).await;
    }
}

/// B side: validate a recording's on-disk `file_path` before streaming it to the
/// puller. Mirrors the containment guard of `api/recording.rs`: reject symlinks
/// BEFORE canonicalize, then require the canonical path to live under
/// `canonical(paths::recordings_dir())` AND traverse a `.tentaflow/recordings`
/// pair. The DB path is NOT trusted blindly — a tampered `/etc/passwd` or a
/// planted blob outside the base is rejected. `max_bytes` caps a hostile/huge
/// file. Returns the validated canonical path.
pub async fn validate_local_recording_path(
    file_path: &str,
    max_bytes: i64,
) -> anyhow::Result<PathBuf> {
    match tokio::fs::symlink_metadata(file_path).await {
        Ok(m) if m.file_type().is_symlink() => {
            anyhow::bail!("recording path is a symlink: {file_path}")
        }
        Ok(_) => {}
        Err(e) => anyhow::bail!("recording path stat failed ({file_path}): {e}"),
    }
    let canonical = tokio::fs::canonicalize(file_path)
        .await
        .map_err(|e| anyhow::anyhow!("recording path canonicalize failed ({file_path}): {e}"))?;
    if !path_within_recordings_base(&canonical).await {
        anyhow::bail!("recording path is outside the recordings base: {file_path}");
    }
    let meta = tokio::fs::metadata(&canonical)
        .await
        .map_err(|e| anyhow::anyhow!("recording metadata failed: {e}"))?;
    if !meta.is_file() {
        anyhow::bail!("recording path is not a regular file: {file_path}");
    }
    if max_bytes >= 0 && meta.len() > max_bytes as u64 {
        anyhow::bail!(
            "recording exceeds pull size cap: {} B > {} B",
            meta.len(),
            max_bytes
        );
    }
    Ok(canonical)
}

/// True iff `canonical` lives under the canonical recordings base AND traverses a
/// `.tentaflow/recordings` directory pair — identical to `api/recording.rs`.
async fn path_within_recordings_base(canonical: &std::path::Path) -> bool {
    if let Ok(canonical_base) = tokio::fs::canonicalize(crate::paths::recordings_dir()).await {
        return canonical.starts_with(&canonical_base)
            && path_traverses_recordings_dir(canonical);
    }
    path_traverses_recordings_dir(canonical)
}

/// True iff the canonical path contains a `.tentaflow/recordings` pair somewhere
/// in its parent chain (the layout hard-coded by the recorder).
fn path_traverses_recordings_dir(canonical: &std::path::Path) -> bool {
    let mut comps = canonical
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .peekable();
    while let Some(c) = comps.next() {
        if c == ".tentaflow" {
            if let Some(&next) = comps.peek() {
                if next == "recordings" {
                    return true;
                }
            }
        }
    }
    false
}

/// Finds a landed file whose stem equals `rec_ref` (`<ref>` or `<ref>.<ext>`).
fn find_landed_file(pull_dir: &std::path::Path, rec_ref: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(pull_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Stored as `<ref>` (no ext) or `<ref>.<ext>`.
        if name == rec_ref || name.strip_prefix(rec_ref).is_some_and(|r| r.starts_with('.')) {
            return Some(path);
        }
    }
    None
}
