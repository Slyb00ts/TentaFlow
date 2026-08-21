// ===== File: project_studio/ingest.rs — chunked uploads + knowledge-source ingest jobs =====
//
// Three responsibilities:
//   1. Chunked file uploads: an in-memory accumulator streams chunks into a
//      part file under `<dir_path>/files/`, finalizes to a content-addressed
//      blob `files/<sha256>` and remembers filename/mime metadata until
//      `SourceCreate` consumes the refs (TTL-bounded).
//   2. Ingest jobs: extract → chunk → embed → vector-store pipeline per file,
//      run as a spawned task with its own cancel registry, progress re-emitted
//      over `log_bus` (key = job_id) for the ingest stream handler, and
//      per-file progress committed in one transaction per file.
//   3. Vector-store plumbing shared with KbSearch and the delete paths
//      (deterministic ref ids, cleanup-then-reingest, scope `ps-<project_id>`,
//      namespace `passages` under `<dir_path>/vectors`).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::repository;
use crate::api::openai::types::{EmbeddingInput, EmbeddingRequest};
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::routing::router::Router;
use crate::services::document::extract::{
    classify_source, split_into_chunks, SourceKind, CHUNK_OVERLAP_CHARS, CHUNK_SIZE_CHARS,
};
use crate::services::ingest_jobs;
use crate::services::vector::backend::{Field, FieldSpec, Metric, UpsertItem};
use crate::services::vector::error::VectorError;
use tentaflow_sdk_spec::{FieldType, FieldValue};

/// Embeddings model alias resolved by the platform (same alias the RAG addon
/// uses, so one embedding space serves both).
pub const EMBEDDINGS_ALIAS: &str = "rag-embeddings";

/// Vector namespace holding the project's knowledge chunks.
pub const VECTOR_NAMESPACE: &str = "passages";

/// Whether a project ingest builds a knowledge graph. Off by owner decision —
/// extraction costs one extra LLM pass per document, so a project opts in rather
/// than paying for it silently. Flipping this to a per-project setting is a
/// one-line change here; the node itself needs nothing (Projects already pass a
/// `graph_home`, which is the structural gate).
const PROJECT_GRAPH_EXTRACTION_DEFAULT: bool = false;

/// Graph collection holding the project's knowledge graph — the same collection
/// name the RAG retrieval nodes read, so a project graph is queryable by the
/// existing graph nodes without a second naming scheme.
pub const GRAPH_COLLECTION: &str = "kg_active";

/// Chunks per embeddings request — bounds request size and lets cancel react
/// between batches on large files.
const EMBED_BATCH: usize = 16;

/// Vector scope for a project — a pseudo addon id, so the per-tenant quota and
/// registry rows in `addon_vector_namespaces` apply unchanged.
pub fn vector_scope(project_id: &str) -> String {
    format!("ps-{project_id}")
}

// =============================================================================
// Global ingest concurrency gate
// =============================================================================

// =============================================================================
// Cancel registry (module-local mirror of dispatch/benchmark.rs)
// =============================================================================

/// Anulowanie biezacych zadan tego procesu. Wspolny typ z
/// `services::cancel_registry` — kazda z trzech kopii tej mapy miala wlasna
/// implementacje, a ta w Project Studio wprost nazywala sie lustrem benchmarku.
static INGEST_CANCEL: crate::services::cancel_registry::CancelRegistry =
    crate::services::cancel_registry::CancelRegistry::new();

fn register_cancel(job_id: &str) -> Arc<AtomicBool> {
    INGEST_CANCEL.register(job_id)
}

fn unregister_cancel(job_id: &str) {
    INGEST_CANCEL.unregister(job_id)
}

/// Cancels a job wherever it currently is. `false` = nothing to cancel (the
/// job is already terminal, or was never queued here).
///
/// Three places have to agree, because a job now has three lives: waiting in
/// the queue, claimed by a worker, and running inside this process.
///   * waiting  — the queue row is removed here and now and the job is closed
///     as cancelled, because the callers that cancel (project delete, source
///     delete) then WAIT for a terminal status, and leaving the row for a busy
///     worker would make that wait depend on unrelated ingest.
///   * claimed  — the request is persisted on the row; the worker's next
///     heartbeat turns it into its local flag.
///   * running here — the in-memory token is tripped, which is what reaches a
///     job already inside a file (the flow cancel path).
pub fn signal_cancel(job_id: &str) -> bool {
    let outcome = ingest_jobs::pool()
        .and_then(|pool| ingest_jobs::request_cancel(&pool, job_id))
        .unwrap_or_else(|e| {
            tracing::error!(job_id, error = %e, "ingest queue cancel failed");
            ingest_jobs::CancelOutcome::Unknown
        });
    let signalled = INGEST_CANCEL.signal(job_id);
    match outcome {
        ingest_jobs::CancelOutcome::Dequeued(payload_json) => {
            close_dequeued_job(&payload_json);
            true
        }
        ingest_jobs::CancelOutcome::Signalled => true,
        ingest_jobs::CancelOutcome::Unknown => signalled,
    }
}

/// Closes a job cancelled before any worker claimed it: nothing will ever run
/// it, so the terminal record is written right here, in the same shape a worker
/// would have produced.
fn close_dequeued_job(payload_json: &str) {
    let Ok(payload) = serde_json::from_str::<JobPayload>(payload_json) else {
        return;
    };
    let Ok(project_pool) = super::project_db::open(&payload.project_id) else {
        return;
    };
    close_job_as_cancelled(&project_pool, &payload);
}

fn close_job_as_cancelled(project_pool: &DbPool, payload: &JobPayload) {
    // The source follows the JOB row: if the job was already terminal, this
    // cancel lost the race and must not relabel a source the finished run left
    // `ready`.
    if repository::finish_ingest_job(project_pool, &payload.job_id, "cancelled", "").unwrap_or(false)
    {
        let _ = repository::set_source_status(project_pool, &payload.source_id, "cancelled", "");
    }
    let tx = log_bus::sender_for(&payload.job_id);
    emit_end_and_close(&tx, &payload.job_id, "cancelled", "");
    unregister_cancel(&payload.job_id);
}

// =============================================================================
// Chunked upload accumulator
// =============================================================================

pub const MAX_UPLOAD_CHUNK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_UPLOAD_FILE_BYTES: u64 = 64 * 1024 * 1024;
const UPLOAD_TTL_MS: i64 = 30 * 60 * 1000;

/// (org_id, user_id, project_id, upload_id) — a chunk stream is private to
/// the uploader; another user cannot append to or finalize it.
type UploadKey = (String, String, String, String);

struct PendingUpload {
    filename: String,
    mime: String,
    total_chunks: u32,
    next_seq: u32,
    received_bytes: u64,
    part_path: PathBuf,
    last_touch_ms: i64,
}

/// Metadata of a finalized blob, kept until `SourceCreate` consumes the
/// `file_ref` (or the TTL sweeps it). Keyed like uploads but by sha256.
#[derive(Clone)]
pub struct FileMeta {
    pub filename: String,
    pub mime: String,
    pub size_bytes: u64,
    finalized_at_ms: i64,
}

fn pending_uploads() -> &'static DashMap<UploadKey, PendingUpload> {
    static MAP: OnceLock<DashMap<UploadKey, PendingUpload>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

fn finalized_files() -> &'static DashMap<UploadKey, FileMeta> {
    static MAP: OnceLock<DashMap<UploadKey, FileMeta>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Drops uploads/finalized metadata older than the TTL, removing orphan part
/// files. Called lazily from `accept_upload_chunk` — no dedicated task.
fn sweep_expired_uploads() {
    let cutoff = now_ms() - UPLOAD_TTL_MS;
    let stale: Vec<UploadKey> = pending_uploads()
        .iter()
        .filter(|e| e.value().last_touch_ms < cutoff)
        .map(|e| e.key().clone())
        .collect();
    for key in stale {
        if let Some((_, up)) = pending_uploads().remove(&key) {
            let _ = std::fs::remove_file(&up.part_path);
        }
    }
    let stale: Vec<UploadKey> = finalized_files()
        .iter()
        .filter(|e| e.value().finalized_at_ms < cutoff)
        .map(|e| e.key().clone())
        .collect();
    for key in stale {
        finalized_files().remove(&key);
    }
}

fn validate_upload_id(upload_id: &str) -> Result<()> {
    if upload_id.is_empty() || upload_id.len() > 128 {
        return Err(anyhow!("invalid upload_id"));
    }
    // The on-disk part name is a hash of the full upload key, so the id never
    // touches the filesystem directly — the charset bound only keeps the wire
    // value and accumulator keys sane.
    if !upload_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(anyhow!("invalid upload_id"));
    }
    Ok(())
}

/// Part-file name derived from the FULL upload key. Hashing (org, user,
/// project, upload_id) together prevents two users who happen to pick the
/// same upload_id from appending into each other's part file.
fn part_file_name(key: &UploadKey) -> String {
    let mut hasher = Sha256::new();
    for segment in [&key.0, &key.1, &key.2, &key.3] {
        hasher.update(segment.as_bytes());
        hasher.update(b"|");
    }
    format!(".upload-{}.part", hex::encode(hasher.finalize()))
}

pub enum UploadOutcome {
    Buffered {
        received_chunks: u32,
        received_bytes: u64,
    },
    Finalized {
        sha256: String,
        received_chunks: u32,
        size_bytes: u64,
    },
}

/// Accepts one upload chunk (blocking disk IO — call from `spawn_blocking`).
/// Chunks must arrive in order; `seq == 0` (re)starts the stream. The final
/// chunk hashes the part file and renames it to `files/<sha256>`.
#[allow(clippy::too_many_arguments)]
pub fn accept_upload_chunk(
    org_id: &str,
    user_id: &str,
    project_id: &str,
    dir_path: &Path,
    upload_id: &str,
    filename: &str,
    mime: &str,
    seq: u32,
    total_chunks: u32,
    bytes: &[u8],
) -> Result<UploadOutcome> {
    sweep_expired_uploads();
    validate_upload_id(upload_id)?;
    if total_chunks == 0 {
        return Err(anyhow!("total_chunks must be >= 1"));
    }
    if seq >= total_chunks {
        return Err(anyhow!("seq out of range"));
    }
    if bytes.len() > MAX_UPLOAD_CHUNK_BYTES {
        return Err(anyhow!("chunk exceeds 4 MiB limit"));
    }
    let filename = filename.trim();
    if filename.is_empty() {
        return Err(anyhow!("filename required"));
    }

    let key: UploadKey = (
        org_id.to_string(),
        user_id.to_string(),
        project_id.to_string(),
        upload_id.to_string(),
    );
    let files_dir = dir_path.join("files");
    std::fs::create_dir_all(&files_dir)?;
    let part_path = files_dir.join(part_file_name(&key));

    if seq == 0 {
        // Restarting an upload id discards any previous partial stream.
        if let Some((_, old)) = pending_uploads().remove(&key) {
            let _ = std::fs::remove_file(&old.part_path);
        }
        std::fs::write(&part_path, bytes)?;
        pending_uploads().insert(
            key.clone(),
            PendingUpload {
                filename: filename.to_string(),
                mime: mime.to_string(),
                total_chunks,
                next_seq: 1,
                received_bytes: bytes.len() as u64,
                part_path: part_path.clone(),
                last_touch_ms: now_ms(),
            },
        );
    } else {
        let mut entry = pending_uploads()
            .get_mut(&key)
            .ok_or_else(|| anyhow!("unknown upload_id (expired or never started)"))?;
        if entry.total_chunks != total_chunks || seq != entry.next_seq {
            return Err(anyhow!("out-of-order upload chunk"));
        }
        if entry.received_bytes + bytes.len() as u64 > MAX_UPLOAD_FILE_BYTES {
            let part = entry.part_path.clone();
            drop(entry);
            pending_uploads().remove(&key);
            let _ = std::fs::remove_file(&part);
            return Err(anyhow!("file exceeds 64 MiB limit"));
        }
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&entry.part_path)?;
        f.write_all(bytes)?;
        entry.next_seq = seq + 1;
        entry.received_bytes += bytes.len() as u64;
        entry.last_touch_ms = now_ms();
    }

    let (received_chunks, received_bytes, complete, meta_filename, meta_mime) = {
        let entry = pending_uploads()
            .get(&key)
            .ok_or_else(|| anyhow!("upload state lost"))?;
        (
            entry.next_seq,
            entry.received_bytes,
            entry.next_seq == entry.total_chunks,
            entry.filename.clone(),
            entry.mime.clone(),
        )
    };

    if !complete {
        return Ok(UploadOutcome::Buffered {
            received_chunks,
            received_bytes,
        });
    }

    // Final chunk: hash the part file streaming, rename to the blob name.
    let sha256 = {
        use std::io::Read;
        let mut file = std::fs::File::open(&part_path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hex::encode(hasher.finalize())
    };
    let blob_path = files_dir.join(&sha256);
    if blob_path.exists() {
        // Same content already stored (dedup) — drop the fresh copy.
        let _ = std::fs::remove_file(&part_path);
    } else {
        std::fs::rename(&part_path, &blob_path)?;
    }
    pending_uploads().remove(&key);
    finalized_files().insert(
        (
            org_id.to_string(),
            user_id.to_string(),
            project_id.to_string(),
            sha256.clone(),
        ),
        FileMeta {
            filename: meta_filename,
            mime: meta_mime,
            size_bytes: received_bytes,
            finalized_at_ms: now_ms(),
        },
    );
    Ok(UploadOutcome::Finalized {
        sha256,
        received_chunks,
        size_bytes: received_bytes,
    })
}

/// Metadata of a finalized upload, looked up by `SourceCreate` when it turns
/// `file_refs` into `source_files` rows. Not consumed — the TTL sweeper
/// removes it.
pub fn finalized_meta(
    org_id: &str,
    user_id: &str,
    project_id: &str,
    sha256: &str,
) -> Option<FileMeta> {
    finalized_files()
        .get(&(
            org_id.to_string(),
            user_id.to_string(),
            project_id.to_string(),
            sha256.to_string(),
        ))
        .map(|m| m.clone())
}

/// How old an on-disk `.upload-*.part` file must be before the GC removes it.
/// Longer than the accumulator TTL, so only parts the in-memory sweeper
/// already forgot (e.g. after a restart) are touched.
const PART_FILE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// How old an unreferenced `files/<sha256>` blob must be before the GC removes
/// it. 24 h leaves ample room for the upload → SourceCreate window (the
/// finalized-meta TTL is 30 min) without racing an in-flight wizard.
const ORPHAN_BLOB_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Best-effort GC of the project `files/` directory, run when a project pool
/// is freshly opened: stale part files (aborted uploads, restarts) and blobs
/// that neither a `source_files` row nor any attachments_json entry (cases,
/// run items, run steps, tasks — risk F.6) references any more. Never fails
/// the open.
pub fn cleanup_files_dir(pool: &DbPool, dir_path: &Path) {
    let files_dir = dir_path.join("files");
    let Ok(entries) = std::fs::read_dir(&files_dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    // One pass over the reference tables instead of a per-blob count query.
    // A read failure disables blob GC for this run — never treat a blob as
    // orphaned on uncertainty (stale part cleanup still proceeds).
    let referenced = repository::referenced_blob_sha256s(pool).ok();
    let mut removed = 0u32;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        let name = entry.file_name().to_string_lossy().to_string();
        let stale_part =
            name.starts_with(".upload-") && name.ends_with(".part") && age > PART_FILE_MAX_AGE;
        let orphan_blob = name.len() == 64
            && name.bytes().all(|b| b.is_ascii_hexdigit())
            && age > ORPHAN_BLOB_MAX_AGE
            && referenced.as_ref().is_some_and(|set| !set.contains(&name));
        if (stale_part || orphan_blob) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            dir = %files_dir.display(),
            removed,
            "project files GC removed stale upload parts / unreferenced blobs"
        );
    }
}

/// Startup recovery, run when a project pool is freshly opened: a 'running'
/// job the QUEUE no longer holds has no future — either its worker died
/// mid-job or it was never enqueued — so it is closed as failed, otherwise
/// pollers and the delete paths would wait on it forever.
///
/// The queue is the authority, not the in-process cancel registry: a job that
/// outlived a restart is still queued here and has not started yet, and killing
/// it because no task of THIS process holds it would defeat the whole point of
/// persisting it.
///
/// THIS FUNCTION OWNS THE TERMINAL WRITE for an orphan — the job row AND the
/// source it was indexing. A source left at `indexing` is a document that is
/// forever mid-ingest in the UI, and no other caller is in a position to close
/// it: the write is guarded (`finish_ingest_job` only lands on a row still
/// `running`), so it happens exactly once, and whoever calls first is the one
/// that lands it. A second writer elsewhere would have to guess whether the
/// guard had already fired for it, which is precisely how the source used to be
/// left behind.
pub fn recover_orphaned_jobs(pool: &DbPool) {
    let Ok(jobs) = repository::running_jobs(pool) else {
        return;
    };
    let Ok(queue) = ingest_jobs::pool() else {
        return;
    };
    for (job_id, source_id) in jobs {
        if ingest_jobs::is_pending(&queue, &job_id).unwrap_or(true) {
            continue;
        }
        if repository::finish_ingest_job(pool, &job_id, "failed", "interrupted by restart")
            .unwrap_or(false)
        {
            tracing::warn!(job_id, "marked orphaned ingest job as failed");
            let _ = repository::set_source_status(
                pool,
                &source_id,
                "error",
                "interrupted by restart",
            );
        }
    }
}

// =============================================================================
// Embeddings
// =============================================================================

/// Embeds `texts` through the platform executor under the shared
/// `rag-embeddings` alias. Order of the returned vectors matches `texts`.
pub async fn embed_texts(router: &Router, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let executor = router
        .executor()
        .ok_or_else(|| anyhow!("model runtime executor not available"))?;
    let expected = texts.len();
    let input = if expected == 1 {
        EmbeddingInput::Single(texts.into_iter().next().expect("len checked"))
    } else {
        EmbeddingInput::Multiple(texts)
    };
    let request = EmbeddingRequest {
        model: EMBEDDINGS_ALIAS.to_string(),
        input,
        encoding_format: None,
        dimensions: None,
        user: None,
        extra: serde_json::Map::new(),
    };
    // §2.5 — project ingest / retrieval embeddings run on behalf of a project,
    // not of the user who happened to trigger the job.
    let mut rctx = crate::services::runtime::context::ExecutionContext::new(
        None,
        crate::flow_engine::dispatcher::FlowOrigin::Project,
        crate::flow_engine::dispatcher::FlowActor::system_component("project_studio"),
    );
    let response = executor
        .execute_embeddings(request, &mut rctx)
        .await
        .map_err(|e| anyhow!("embeddings ({EMBEDDINGS_ALIAS}): {e}"))?;
    let mut data = response.data;
    data.sort_by_key(|d| d.index);
    if data.len() != expected {
        return Err(anyhow!(
            "embeddings returned {} vectors for {expected} inputs",
            data.len()
        ));
    }
    Ok(data.into_iter().map(|d| d.embedding).collect())
}

// =============================================================================
// Vector store plumbing
// =============================================================================

/// Metadata schema of the `passages` namespace. The same names feed `KbHit`
/// mapping in the dispatcher.
pub fn passage_field_specs() -> Vec<FieldSpec> {
    vec![
        FieldSpec {
            name: "doc_id".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "chunk_index".to_string(),
            field_type: FieldType::Int,
            indexed: true,
        },
        FieldSpec {
            name: "text".to_string(),
            field_type: FieldType::Str,
            indexed: false,
        },
        FieldSpec {
            name: "source_id".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "path".to_string(),
            field_type: FieldType::Str,
            indexed: false,
        },
        FieldSpec {
            name: "location".to_string(),
            field_type: FieldType::Str,
            indexed: false,
        },
    ]
}

/// Deletes every vector of `doc_id` (= file_id) from the project's passage
/// namespace. A missing namespace means there is nothing to clean. Delete
/// paths have no real embedding at hand, so the probe is the zero vector.
pub fn delete_file_vectors(
    core_db: &DbPool,
    org_id: &str,
    project_id: &str,
    file_id: &str,
) -> Result<()> {
    let mgr = crate::services::vector_namespace_manager(core_db);
    let scope = vector_scope(project_id);
    let backend = match mgr.get(org_id, &scope, VECTOR_NAMESPACE) {
        Ok(b) => b,
        Err(VectorError::NamespaceNotFound { .. }) => return Ok(()),
        Err(e) => return Err(anyhow!("vector namespace open: {e}")),
    };
    crate::services::vector::doc_vectors::delete_doc_vectors(&*backend, file_id, None)
        .map_err(|e| anyhow!("vector cleanup: {e}"))
}

/// Drops every namespace of the project scope `ps-<project_id>` (registry row
/// + on-disk data). Returns how many namespaces were removed.
pub fn drop_project_namespaces(core_db: &DbPool, org_id: &str, project_id: &str) -> Result<u32> {
    let scope = vector_scope(project_id);
    let namespaces: Vec<String> = {
        let conn = core_db.read().map_err(|e| anyhow!("core db read: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT namespace FROM addon_vector_namespaces WHERE org_id = ?1 AND addon_id = ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![org_id, scope], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mgr = crate::services::vector_namespace_manager(core_db);
    let mut dropped = 0u32;
    for ns in namespaces {
        mgr.delete_namespace(org_id, &scope, &ns)
            .map_err(|e| anyhow!("drop namespace '{ns}': {e}"))?;
        dropped += 1;
    }
    Ok(dropped)
}

/// Graph sibling of [`delete_file_vectors`]: soft-deletes every entity and
/// relation `graph_extract` attributed to `file_id` (its `provenance.doc_id`).
/// Returns `(nodes, edges)` removed. A build with no graph backend, and a
/// project that never had a graph collection, both removed nothing.
pub fn delete_file_graph(
    core_db: &DbPool,
    org_id: &str,
    project_id: &str,
    file_id: &str,
) -> Result<(u64, u64)> {
    #[cfg(feature = "graph")]
    {
        // Same instance scope as the vector side — one project, one pseudo addon
        // id, so quota and registry rows line up across both stores.
        crate::services::graph_manager(core_db)
            .delete_document_in(org_id, &vector_scope(project_id), GRAPH_COLLECTION, file_id)
            .map_err(|e| anyhow!("graph cleanup: {e}"))
    }
    #[cfg(not(feature = "graph"))]
    {
        let _ = (core_db, org_id, project_id, file_id);
        Ok((0, 0))
    }
}

/// Graph sibling of [`drop_project_namespaces`]: drops every graph collection of
/// the project scope (registry rows + on-disk Cozo files) at project teardown.
pub fn drop_project_graph(core_db: &DbPool, org_id: &str, project_id: &str) -> Result<()> {
    #[cfg(feature = "graph")]
    {
        crate::services::graph_manager(core_db)
            .delete_all_for_addon(org_id, &vector_scope(project_id))
            .map_err(|e| anyhow!("drop project graph: {e}"))
    }
    #[cfg(not(feature = "graph"))]
    {
        let _ = (core_db, org_id, project_id);
        Ok(())
    }
}

/// One extracted chunk ready for embedding + storage.
struct ChunkText {
    index: u64,
    text: String,
    location: String,
}

/// Writes all chunks of one file: cleanup-then-reingest (old vectors of the
/// doc removed first, so a shrinking re-ingest leaves no orphans) + rollback
/// of every ref on batch failure — no partial file ingest, mirroring the
/// `store` node adapter semantics.
#[allow(clippy::too_many_arguments)]
fn store_chunks_blocking(
    core_db: &DbPool,
    org_id: &str,
    project_id: &str,
    dir_path: &Path,
    source_id: &str,
    file_id: &str,
    path: &str,
    chunks: &[ChunkText],
    vectors: &[Vec<f32>],
) -> Result<()> {
    debug_assert_eq!(chunks.len(), vectors.len());
    let dim = vectors
        .first()
        .map(|v| v.len() as u32)
        .ok_or_else(|| anyhow!("no vectors to store"))?;

    let mgr = crate::services::vector_namespace_manager(core_db);
    let scope = vector_scope(project_id);
    // Cleanup-then-reingest with a REAL embedding as the search probe — the
    // zero vector degenerates under cosine, a fresh vector of the same doc
    // lands near the old ones and maximises cleanup recall (store.rs pattern).
    match mgr.get(org_id, &scope, VECTOR_NAMESPACE) {
        Ok(backend) => crate::services::vector::doc_vectors::delete_doc_vectors(
            &*backend,
            file_id,
            Some(vectors[0].as_slice()),
        )?,
        Err(VectorError::NamespaceNotFound { .. }) => {}
        Err(e) => return Err(anyhow!("vector namespace open: {e}")),
    }
    let specs = passage_field_specs();
    let vectors_dir = dir_path.join("vectors");
    // Ensure the namespace exists AT the project directory before the quota
    // path (`upsert_batch_with_quota` → `get_or_create`) would create it in
    // the default addon tree.
    mgr.get_or_create_at(
        org_id,
        &scope,
        VECTOR_NAMESPACE,
        dim,
        Metric::Cosine,
        &specs,
        false,
        &vectors_dir,
    )
    .map_err(|e| anyhow!("vector namespace create: {e}"))?;

    let mut fields_per_chunk: Vec<Vec<Field>> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        fields_per_chunk.push(vec![
            Field {
                name: "doc_id".to_string(),
                value: FieldValue::Str(file_id.to_string()),
            },
            Field {
                name: "chunk_index".to_string(),
                value: FieldValue::Int(chunk.index as i64),
            },
            Field {
                name: "text".to_string(),
                value: FieldValue::Str(chunk.text.clone()),
            },
            Field {
                name: "source_id".to_string(),
                value: FieldValue::Str(source_id.to_string()),
            },
            Field {
                name: "path".to_string(),
                value: FieldValue::Str(path.to_string()),
            },
            Field {
                name: "location".to_string(),
                value: FieldValue::Str(chunk.location.clone()),
            },
        ]);
    }
    let items: Vec<UpsertItem<'_>> = chunks
        .iter()
        .zip(vectors.iter())
        .zip(fields_per_chunk.iter())
        .map(|((chunk, vector), fields)| UpsertItem {
            ref_id: crate::services::vector::doc_vectors::ref_id_for(file_id, chunk.index),
            vector,
            fields: fields.as_slice(),
            sparse: None,
        })
        .collect();

    if let Err(e) = mgr.upsert_batch_with_quota(
        org_id,
        &scope,
        VECTOR_NAMESPACE,
        dim,
        Metric::Cosine,
        &specs,
        false,
        &items,
        None,
    ) {
        // No partial ingest: best-effort delete of every ref of this file.
        if let Ok(backend) = mgr.get(org_id, &scope, VECTOR_NAMESPACE) {
            for item in &items {
                let _ = backend.delete(item.ref_id);
            }
        }
        return Err(anyhow!("vector batch upsert: {e}"));
    }
    Ok(())
}

// =============================================================================
// Extraction
// =============================================================================

/// File extensions treated as source code — chunked line-based with a path
/// header so the embedding carries file identity and line provenance.
const CODE_EXTS: &[&str] = &[
    "rs",
    "py",
    "js",
    "ts",
    "tsx",
    "jsx",
    "java",
    "cs",
    "cpp",
    "cc",
    "c",
    "h",
    "hpp",
    "go",
    "rb",
    "php",
    "swift",
    "kt",
    "kts",
    "scala",
    "sql",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "yaml",
    "yml",
    "toml",
    "ini",
    "css",
    "scss",
    "html",
    "xml",
    "proto",
    "gradle",
    "cmake",
    "dockerfile",
];

fn code_ext(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    CODE_EXTS.iter().find(|e| **e == ext).copied()
}

/// Whether a blob may be shown by the text-only preview endpoint: source code
/// by extension, or anything the classifier recognises as plain text.
pub fn is_text_preview(path: &str, mime: &str, bytes: &[u8]) -> bool {
    code_ext(path).is_some() || classify_source(mime, bytes) == SourceKind::Text
}

// =============================================================================
// Code-tree ingestion (git / zip sources)
// =============================================================================

/// Per-file size cap of a code source. Bigger files are generated artifacts,
/// minified bundles or binaries — never useful knowledge chunks (risk R8).
pub const MAX_TREE_FILE_BYTES: u64 = 512 * 1024;
/// Entry cap of one code tree.
pub const MAX_TREE_FILES: usize = 5_000;
/// Total ingested-bytes budget of one code tree.
pub const MAX_TREE_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// Directories never walked: VCS metadata plus dependency/build output that
/// would blow the entry budget without carrying project knowledge.
const SKIPPED_TREE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "bin",
    "obj",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".gradle",
    ".next",
    ".nuxt",
    ".idea",
    ".vscode",
    "vendor",
    "coverage",
];

/// One file of a code tree, already content-addressed into the project's blob
/// store so the regular ingest pipeline can process it unchanged.
#[derive(Debug, Clone)]
pub struct CollectedFile {
    pub rel_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub mime: String,
}

fn tree_mime(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "csv" => "text/csv",
        _ => "text/plain",
    }
}

/// Walks an extracted/cloned source tree, keeps the text and source-code files
/// within the budgets and writes each one into `<dir_path>/files/<sha256>`.
/// Returns the collected files in stable path order. Blocking IO — call from
/// `spawn_blocking`.
pub fn collect_tree_files(root: &Path, dir_path: &Path) -> Result<Vec<CollectedFile>> {
    let files_dir = dir_path.join("files");
    std::fs::create_dir_all(&files_dir)?;
    let mut out: Vec<CollectedFile> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // symlink_metadata: a symlink is never followed — a repo may point
            // one at /etc and the walk must not read through it.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if !SKIPPED_TREE_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            if !meta.is_file() || meta.len() > MAX_TREE_FILE_BYTES {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel_path = rel.to_string_lossy().replace('\\', "/");
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if !is_text_preview(&rel_path, "", &bytes) {
                continue;
            }
            if out.len() >= MAX_TREE_FILES {
                return Err(anyhow!(
                    "source tree exceeds {MAX_TREE_FILES} indexable files"
                ));
            }
            total_bytes += bytes.len() as u64;
            if total_bytes > MAX_TREE_TOTAL_BYTES {
                return Err(anyhow!(
                    "source tree exceeds the {MAX_TREE_TOTAL_BYTES} byte budget"
                ));
            }
            let sha256 = hex::encode(Sha256::digest(&bytes));
            let blob_path = files_dir.join(&sha256);
            if !blob_path.exists() {
                std::fs::write(&blob_path, &bytes)?;
            }
            out.push(CollectedFile {
                mime: tree_mime(&rel_path).to_string(),
                size_bytes: bytes.len() as u64,
                sha256,
                rel_path,
            });
        }
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

/// Writes one generated text document (e.g. the api_spec endpoint digest) into
/// the project's blob store and returns its collected-file descriptor.
pub fn store_generated_text(
    dir_path: &Path,
    rel_path: &str,
    content: &str,
) -> Result<CollectedFile> {
    let files_dir = dir_path.join("files");
    std::fs::create_dir_all(&files_dir)?;
    let bytes = content.as_bytes();
    let sha256 = hex::encode(Sha256::digest(bytes));
    let blob_path = files_dir.join(&sha256);
    if !blob_path.exists() {
        std::fs::write(&blob_path, bytes)?;
    }
    Ok(CollectedFile {
        rel_path: rel_path.to_string(),
        sha256,
        size_bytes: bytes.len() as u64,
        mime: tree_mime(rel_path).to_string(),
    })
}

/// Difference between the file set recorded in `source_files` and the current
/// tree, used by the git refresh: only these paths are re-embedded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TreeDelta {
    /// Paths present only in the new tree.
    pub added: Vec<String>,
    /// Paths whose content hash changed.
    pub changed: Vec<String>,
    /// Paths that vanished from the tree (their rows + vectors are dropped).
    pub removed: Vec<String>,
}

impl TreeDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Computes the delta between the stored `(path, sha256)` pairs and the freshly
/// collected tree.
pub fn diff_tree(stored: &HashMap<String, String>, current: &[CollectedFile]) -> TreeDelta {
    let mut delta = TreeDelta::default();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for file in current {
        seen.insert(file.rel_path.as_str());
        match stored.get(&file.rel_path) {
            None => delta.added.push(file.rel_path.clone()),
            Some(sha) if *sha != file.sha256 => delta.changed.push(file.rel_path.clone()),
            Some(_) => {}
        }
    }
    for path in stored.keys() {
        if !seen.contains(path.as_str()) {
            delta.removed.push(path.clone());
        }
    }
    delta.added.sort();
    delta.changed.sort();
    delta.removed.sort();
    delta
}

/// Line-based chunker for code files: each chunk is prefixed with a
/// `// <path>:<start>-<end>` header line and reports its line range as the
/// human-readable location ("l. 10–42").
fn chunk_code(path: &str, content: &str) -> Vec<ChunkText> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start_line = 1usize;
    let mut line_no = 0usize;
    for line in &lines {
        line_no += 1;
        if !buf.is_empty() && buf.chars().count() + line.chars().count() + 1 > CHUNK_SIZE_CHARS {
            let end_line = line_no - 1;
            out.push(ChunkText {
                index: out.len() as u64,
                text: format!("// {path}:{start_line}-{end_line}\n{buf}"),
                location: format!("l. {start_line}–{end_line}"),
            });
            buf.clear();
            start_line = line_no;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        // Oversized single line: hard-cut so no chunk exceeds the embedding
        // context budget.
        if line.chars().count() > CHUNK_SIZE_CHARS {
            buf.push_str(&line.chars().take(CHUNK_SIZE_CHARS).collect::<String>());
        } else {
            buf.push_str(line);
        }
    }
    if !buf.trim().is_empty() {
        let end_line = line_no.max(start_line);
        out.push(ChunkText {
            index: out.len() as u64,
            text: format!("// {path}:{start_line}-{end_line}\n{buf}"),
            location: format!("l. {start_line}–{end_line}"),
        });
    }
    out
}

fn prose_chunks(text: &str) -> Vec<ChunkText> {
    split_into_chunks(text, CHUNK_SIZE_CHARS, CHUNK_OVERLAP_CHARS)
        .into_iter()
        .enumerate()
        .map(|(i, text)| ChunkText {
            index: i as u64,
            text,
            location: String::new(),
        })
        .collect()
}

enum ExtractOutcome {
    Chunks(Vec<ChunkText>),
    Skip(String),
}

/// Where the bytes of one work item come from.
#[derive(Serialize, Deserialize)]
pub enum WorkPayload {
    /// Content-addressed blob `<dir_path>/files/<sha256>`.
    Blob,
    /// Single public web page, fetched through the web_research SSRF guard.
    Url(String),
}

/// One file to process in an ingest job.
#[derive(Serialize, Deserialize)]
pub struct FileWork {
    pub file_id: String,
    pub path: String,
    pub sha256: String,
    pub mime: String,
    pub payload: WorkPayload,
}

/// Blocking extraction of one work item into chunks. Office formats go
/// through the pure-Rust extractors, PDFs use the native pdfium text layer;
/// scans/images are skipped until the vision pipeline is callable from a
/// native core job.
fn extract_file(dir_path: &Path, work: &FileWork) -> Result<ExtractOutcome> {
    match &work.payload {
        WorkPayload::Url(url) => {
            let request = crate::web_research::types::ReadUrlRequest {
                url: url.clone(),
                max_chars: 200_000,
                mode: Default::default(),
                user_id: None,
            };
            let page = crate::web_research::reader::read_url(&request)
                .map_err(|e| anyhow!("url fetch: {e}"))?;
            if page.text.trim().is_empty() {
                return Ok(ExtractOutcome::Skip("page has no extractable text".into()));
            }
            Ok(ExtractOutcome::Chunks(prose_chunks(&page.text)))
        }
        // Tu trafiaja juz TYLKO pliki kodu — `process_file` kieruje kazdy inny blob
        // do wspolnego flow-ingestu. Proza, office, PDF, skany i obrazy sa jego
        // sprawa; ta sciezka zostaje wylacznie dla chunkowania po liniach z
        // zakresem linii jako `location`, ktorego wezel `chunk` nie odtwarza.
        WorkPayload::Blob => {
            let blob_path = dir_path.join("files").join(&work.sha256);
            let bytes =
                std::fs::read(&blob_path).map_err(|e| anyhow!("blob {} read: {e}", work.sha256))?;
            let content = String::from_utf8_lossy(&bytes);
            Ok(ExtractOutcome::Chunks(chunk_code(&work.path, &content)))
        }
    }
}

// =============================================================================
// Job runner
// =============================================================================

/// Everything a job needs, captured at the call site. `core_db` and `router`
/// are process-wide handles and do NOT travel through the queue — they are
/// published for the workers (see [`WorkerRuntime`]); the rest is the durable
/// payload.
pub struct IngestTask {
    pub core_db: DbPool,
    pub router: Arc<Router>,
    pub project_pool: DbPool,
    pub org_id: String,
    pub project_id: String,
    pub dir_path: PathBuf,
    pub source_id: String,
    pub job_id: String,
    pub files: Vec<FileWork>,
}

fn emit_line(
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
    kind: &str,
    phase: &str,
    line: String,
    progress_pct: u32,
) {
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: job_id.to_string(),
        kind: kind.to_string(),
        line,
        phase: phase.to_string(),
        progress_pct,
        ts_ms: log_bus::now_ms(),
    }));
}

/// Everything a queued job needs to run in a process that never saw the
/// request that created it. Serialized into the durable queue, so the fields
/// are data only: the runtime handles (`core_db`, `router`) are process-wide
/// and come from [`WorkerRuntime`].
#[derive(Serialize, Deserialize)]
struct JobPayload {
    org_id: String,
    project_id: String,
    dir_path: PathBuf,
    source_id: String,
    job_id: String,
    files: Vec<FileWork>,
}

/// Handles a queue worker cannot obtain from a request context — a job
/// recovered after a restart has no session behind it. Published by whichever
/// runs first: the startup wiring (`start_workers`) or the first enqueue in an
/// embedding that has no startup hook.
struct WorkerRuntime {
    core_db: DbPool,
    router: Arc<Router>,
}

static WORKER_RUNTIME: OnceLock<WorkerRuntime> = OnceLock::new();
static WORKERS_STARTED: AtomicBool = AtomicBool::new(false);

/// Woken on every enqueue so a waiting worker starts immediately instead of at
/// the next poll.
fn wake() -> &'static tokio::sync::Notify {
    static WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();
    WAKE.get_or_init(tokio::sync::Notify::new)
}

/// Safety net behind the wake signal: a job enqueued by ANOTHER process run —
/// i.e. one that outlived a restart — has no live notifier behind it.
const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(30);

fn publish_runtime(core_db: DbPool, router: Arc<Router>) {
    let _ = WORKER_RUNTIME.set(WorkerRuntime { core_db, router });
}

/// Clears the queue of every job it was still holding for a process run that no
/// longer exists, and drives the per-project recovery for each one. Startup
/// only: a row claimed by THIS run is supervised by definition, and the queue's
/// own `reconcile_orphans` never touches it.
///
/// It does NOT write the terminal job or source status itself. Deleting the
/// queue row is what turns the job into an orphan, and `recover_orphaned_jobs`
/// is the single owner of what that means for the project — it also runs from
/// `project_db::open`'s fresh-open hook, so a project this queue sweep never
/// names still gets recovered. Writing the row here as well produced the bug
/// this ordering exists to prevent: the open below fires that hook, the hook's
/// guarded write lands, and a second guarded write here then returned `false`
/// and skipped the source, leaving the document at `indexing` forever.
///
/// The explicit call after `open` is therefore load-bearing rather than
/// belt-and-braces: `open` only runs the hook on a FRESH open, and a project
/// already in the pool cache would otherwise be swept in the queue and left
/// untouched in its own database.
pub fn reconcile_orphans() {
    let Ok(pool) = ingest_jobs::pool() else {
        return;
    };
    let orphans = match ingest_jobs::reconcile_orphans(&pool, ingest_jobs::QUEUE_PROJECT_STUDIO) {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "ingest queue reconciliation failed");
            return;
        }
    };
    for job in orphans {
        let Ok(payload) = serde_json::from_str::<JobPayload>(&job.payload_json) else {
            continue;
        };
        let Ok(project_pool) = super::project_db::open(&payload.project_id) else {
            continue;
        };
        tracing::warn!(job_id = %payload.job_id, "closing an ingest job orphaned by a restart");
        // An orphaned QUEUE row does not prove the job failed: the worker
        // writes the project row terminal and only then deletes the queue row,
        // so a crash in between leaves exactly this state behind a SUCCESS.
        // The guarded write inside decides; a recorded success survives.
        recover_orphaned_jobs(&project_pool);
    }
}

/// How many jobs may be IN PROGRESS at once in this process. It equals
/// `MAX_CONCURRENT_DOCUMENT_INGESTS` only because both are "two"; they bound
/// different things and this one is a genuine second limit — the gate in
/// `execute_ingest` counts DOCUMENTS across Project Studio and the RAG addon,
/// so a worker CAN sit idle holding a job while the addon holds both document
/// permits. A job waiting for a worker is reported as queued (`start_job`),
/// which is the only honest thing to say about it.
const WORKERS: usize = crate::services::ingest_gate::MAX_CONCURRENT_DOCUMENT_INGESTS;

/// Delay before a worker task that died is replaced, so a worker panicking on
/// every pass degrades to a slow retry instead of a hot loop.
const WORKER_RESTART_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Starts the queue workers for this process.
pub fn start_workers(core_db: DbPool, router: Arc<Router>) {
    publish_runtime(core_db, router);
    if WORKERS_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    for _ in 0..WORKERS {
        tokio::spawn(supervise_worker());
    }
}

/// Keeps one worker slot filled. `worker_loop` never returns, so its task
/// ending at all means it panicked somewhere the per-job guard does not cover
/// (the claim, the wake, the select). Without this the pool would shrink
/// silently — one worker per panic — until nothing ingested for the life of the
/// process, and `WORKERS_STARTED` would block any restart.
async fn supervise_worker() {
    loop {
        match tokio::spawn(worker_loop()).await {
            Ok(()) => return,
            Err(e) if e.is_cancelled() => return,
            Err(_) => {
                tracing::error!("ingest worker task panicked; restarting the worker");
                tokio::time::sleep(WORKER_RESTART_DELAY).await;
            }
        }
    }
}

async fn worker_loop() {
    loop {
        // Armed BEFORE the claim attempt, so an enqueue that lands while we are
        // draining cannot be missed between "queue empty" and "waiting".
        // `enable` is what arms it: `notify_waiters` only reaches futures that
        // are already registered, and a `Notified` registers when first polled.
        let notified = wake().notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        loop {
            let Ok(pool) = ingest_jobs::pool() else {
                break;
            };
            retry_undeleted(&pool);
            match ingest_jobs::claim(&pool, ingest_jobs::QUEUE_PROJECT_STUDIO) {
                Ok(Some(job)) => run_supervised(job).await,
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "ingest queue claim failed");
                    break;
                }
            }
        }
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(IDLE_POLL) => {}
        }
    }
}

/// Runs `fut` on its own task so a panic inside it is captured instead of
/// unwinding into the caller. `false` = it panicked.
async fn supervised<F>(job_id: &str, stage: &str, fut: F) -> bool
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    match tokio::spawn(fut).await {
        Ok(()) => true,
        Err(e) if e.is_cancelled() => false,
        Err(_) => {
            tracing::error!(job_id, stage, "ingest job panicked");
            false
        }
    }
}

/// Supervises the WHOLE of `run_claimed`, not just the pipeline: opening the
/// project database, reading back the terminal row and closing the log channel
/// are as able to panic as the pipeline is, and such a panic used to escape
/// into `worker_loop` and kill the worker for the life of the process.
async fn run_supervised(job: crate::services::ingest_jobs::QueuedJob) {
    let job_id = job.job_id.clone();
    let payload_json = job.payload_json.clone();
    if supervised(&job_id, "job", run_claimed(job)).await {
        return;
    }
    // The recovery runs supervised too: it opens the same project database
    // that may be what panicked.
    let recovery_id = job_id.clone();
    supervised(&job_id, "panic recovery", async move {
        close_panicked_job(&recovery_id, &payload_json);
    })
    .await;
}

/// Accounts for a job whose `run_claimed` panicked. A panic must not leave the
/// job merely swallowed: the queue row goes (nothing will run it again, and a
/// `running` row owned by a LIVE instance is invisible to reconciliation), the
/// project row is closed if the pipeline had not closed it already, and the
/// stream is ended with whatever the row actually says.
fn close_panicked_job(job_id: &str, payload_json: &str) {
    if let Ok(pool) = ingest_jobs::pool() {
        finish_queue_row(&pool, job_id);
    }
    let mut status = "failed".to_string();
    let mut error = PANIC_ERROR.to_string();
    if let Ok(payload) = serde_json::from_str::<JobPayload>(payload_json) {
        if let Ok(project_pool) = super::project_db::open(&payload.project_id) {
            if repository::finish_ingest_job(&project_pool, job_id, "failed", PANIC_ERROR)
                .unwrap_or(false)
            {
                let _ = repository::set_source_status(
                    &project_pool,
                    &payload.source_id,
                    "error",
                    PANIC_ERROR,
                );
            }
            // The panic may have happened AFTER the pipeline wrote a real
            // terminal status; the bus must report that one, not a made-up
            // failure.
            if let Ok(Some(row)) = repository::get_ingest_job(&project_pool, job_id) {
                status = row.status;
                error = row.error;
            }
        }
    }
    let tx = log_bus::sender_for(job_id);
    emit_end_and_close(&tx, job_id, &status, &error);
    unregister_cancel(job_id);
}

const PANIC_ERROR: &str = "ingest worker panicked";

/// Queue rows whose job is already accounted for in its project but whose
/// deletion failed. Discarding that error would make the row immortal: `claim`
/// only takes `queued`, `is_pending` keeps answering yes, and nothing in this
/// process would ever look at the row again.
fn undeleted() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static UNDELETED: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        OnceLock::new();
    UNDELETED.get_or_init(Default::default)
}

fn undeleted_lock() -> std::sync::MutexGuard<'static, std::collections::HashSet<String>> {
    undeleted()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Deletes the queue row of a job that is already recorded in its project. A
/// failure is REMEMBERED rather than retried on the spot — the worker loop
/// tries again on its next pass, which bounds the retry to the loop's own
/// cadence instead of spinning on a file that is not writable.
fn finish_queue_row(pool: &DbPool, job_id: &str) {
    match ingest_jobs::finish(pool, job_id) {
        Ok(()) => {
            undeleted_lock().remove(job_id);
        }
        Err(e) => {
            tracing::error!(
                job_id,
                error = %e,
                "ingest queue row not deleted; retrying on the next worker pass"
            );
            undeleted_lock().insert(job_id.to_string());
        }
    }
}

/// One delete attempt per remembered row per worker pass.
fn retry_undeleted(pool: &DbPool) {
    let pending: Vec<String> = undeleted_lock().iter().cloned().collect();
    for job_id in pending {
        finish_queue_row(pool, &job_id);
    }
}

/// Panic injected into `run_claimed` OUTSIDE the pipeline, so the guard that is
/// supposed to keep a worker alive through a finalizer panic can be exercised
/// at all. Consumed by the first job that reads it.
#[cfg(test)]
static PANIC_AFTER_PROJECT_OPEN: AtomicBool = AtomicBool::new(false);

/// Runs one claimed job. Every step is inside the supervision of
/// [`run_supervised`], so a panic here ends the job rather than the worker.
async fn run_claimed(job: crate::services::ingest_jobs::QueuedJob) {
    let Ok(pool) = ingest_jobs::pool() else {
        return;
    };
    let payload: JobPayload = match serde_json::from_str(&job.payload_json) {
        Ok(p) => p,
        Err(e) => {
            // Nothing can ever run this row; leaving it would make it a job
            // claimed and released forever.
            tracing::error!(job_id = %job.job_id, error = %e, "unreadable ingest job payload");
            finish_queue_row(&pool, &job.job_id);
            return;
        }
    };
    let job_id = payload.job_id.clone();
    let cancel = register_cancel(&job_id);
    let tx = log_bus::sender_for(&job_id);
    // Closes the claim window: a cancel that landed between the claim and the
    // registration above exists ONLY in the queue row.
    if !matches!(
        ingest_jobs::heartbeat(&pool, &job_id),
        Ok(ingest_jobs::JobLiveness::Running)
    ) {
        cancel.store(true, Ordering::Relaxed);
    }

    let project_pool = match super::project_db::open(&payload.project_id) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(job_id = %job_id, error = %e, "ingest job project unavailable");
            finish_queue_row(&pool, &job_id);
            emit_end_and_close(&tx, &job_id, "failed", &format!("project unavailable: {e}"));
            unregister_cancel(&job_id);
            return;
        }
    };

    #[cfg(test)]
    if PANIC_AFTER_PROJECT_OPEN.swap(false, Ordering::SeqCst) {
        panic!("finalizer blew up");
    }

    // Wspolbieznosc ogranicza JEDNA bramka w `execute_ingest`
    // (`services::ingest_gate`), przez ktora przechodzi tez addon RAG — a
    // liczy ona DOKUMENTY, czyli to, co realnie ogranicza pamiec.
    //
    // Zadanie anulowane, zanim tknelo jakikolwiek plik, konczy sie terminalnie
    // bez przetwarzania — na tym opieraja sie sciezki kasowania — bo `run_job`
    // sprawdza flage przed kazdym plikiem.
    if cancel.load(Ordering::Relaxed) {
        let _ = repository::finish_ingest_job(&project_pool, &job_id, "cancelled", "");
    } else {
        if crate::services::ingest_gate::would_wait() {
            emit_line(
                &tx,
                &job_id,
                "log",
                "queue",
                "waiting for a free ingest slot".to_string(),
                0,
            );
        }
        let task = IngestTask {
            core_db: runtime_core_db(),
            router: runtime_router(),
            project_pool: project_pool.clone(),
            org_id: payload.org_id,
            project_id: payload.project_id,
            dir_path: payload.dir_path,
            source_id: payload.source_id,
            job_id: job_id.clone(),
            files: payload.files,
        };
        let tx_run = tx.clone();
        let cancel_run = cancel.clone();
        run_guarded(&project_pool, &job_id, async move {
            run_job(task, tx_run, cancel_run).await
        })
        .await;
    }

    // Terminal from the DB row — the single source of truth the polling
    // endpoint also reads (success | failed | cancelled).
    let (status, error) = match repository::get_ingest_job(&project_pool, &job_id) {
        Ok(Some(job)) => (job.status, job.error),
        _ => ("failed".to_string(), "job record missing".to_string()),
    };
    // The queue row goes only AFTER the project row is terminal: a missing
    // queue row must always mean the job is accounted for elsewhere.
    finish_queue_row(&pool, &job_id);
    emit_end_and_close(&tx, &job_id, &status, &error);
    unregister_cancel(&job_id);
}

/// Runs the pipeline supervised and turns a panic in it into a terminal job
/// status — a panicking job must end FAILED, never stay `running` forever.
async fn run_guarded<F>(project_pool: &DbPool, job_id: &str, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if !supervised(job_id, "pipeline", fut).await {
        let _ = repository::finish_ingest_job(project_pool, job_id, "failed", "ingest task panicked");
    }
}

/// Runtime handles for a claimed job. A worker only runs after
/// `start_workers`/`start_job` published them, so the expectation holds.
fn runtime_core_db() -> DbPool {
    WORKER_RUNTIME
        .get()
        .expect("ingest worker runtime published before any job is claimed")
        .core_db
        .clone()
}

fn runtime_router() -> Arc<Router> {
    WORKER_RUNTIME
        .get()
        .expect("ingest worker runtime published before any job is claimed")
        .router
        .clone()
}

/// Emits the terminal bus message and closes the channel. The 100 ms pause
/// gives live subscribers a chance to drain `End` first; a subscriber that
/// still misses it reconciles through `IngestStatusRequest`.
fn emit_end_and_close(
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
    status: &str,
    error: &str,
) {
    let _ = tx.send(BusMessage::End {
        deploy_id: job_id.to_string(),
        final_status: status.to_string(),
        image_tag: String::new(),
        container_name: String::new(),
        error_message: error.to_string(),
        duration_ms: 0,
    });
    let job_id = job_id.to_string();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                log_bus::close(&job_id);
            });
        }
        Err(_) => log_bus::close(&job_id),
    }
}

/// Persists the job and returns; a worker drains the queue. The job outlives
/// the process that accepted it, which is the whole point — before this it was
/// a `tokio::spawn` that a restart dropped silently.
///
/// The log_bus channel is opened BEFORE returning (the response reaches the
/// frontend before any worker picks the job up — an immediate stream subscribe
/// must find the channel), and the cancel token is NOT registered here: a
/// cancel arriving while the job waits is persisted in the queue row instead,
/// which is the only representation that survives a restart.
pub fn start_job(task: IngestTask) {
    let IngestTask {
        core_db,
        router,
        project_pool,
        org_id,
        project_id,
        dir_path,
        source_id,
        job_id,
        files,
    } = task;
    publish_runtime(core_db, router);
    let payload = JobPayload {
        org_id,
        project_id,
        dir_path,
        source_id: source_id.clone(),
        job_id: job_id.clone(),
        files,
    };
    let tx = log_bus::sender_for(&job_id);

    let queued = ingest_jobs::pool().and_then(|pool| {
        let json = serde_json::to_string(&payload)?;
        ingest_jobs::enqueue(&pool, ingest_jobs::QUEUE_PROJECT_STUDIO, &job_id, &json)
    });
    match queued {
        Ok(()) => {
            wake().notify_waiters();
            announce_queue_wait(&tx, &job_id);
        }
        Err(e) => {
            tracing::error!(job_id = %job_id, error = %e, "ingest job could not be queued");
            let message = format!("ingest queue unavailable: {e}");
            let _ = repository::finish_ingest_job(&project_pool, &job_id, "failed", &message);
            let _ = repository::set_source_status(&project_pool, &source_id, "error", &message);
            emit_end_and_close(&tx, &job_id, "failed", &message);
        }
    }
}

/// Reports a job that will WAIT for a worker. Before the queue, a job that
/// could not start said so through the document gate; now it can also sit in
/// the queue with nobody working on it, and saying nothing would leave the
/// stream blank until a worker got to it.
///
/// It reports only what the queue can prove — how many jobs are outstanding
/// ahead of this one — and never that the job is running: fewer outstanding
/// jobs than workers means one is free, so there is no wait worth announcing.
fn announce_queue_wait(tx: &tokio::sync::broadcast::Sender<BusMessage>, job_id: &str) {
    let Ok(pool) = ingest_jobs::pool() else {
        return;
    };
    // A failed read is not an empty queue. Defaulting to 0 turned "the queue
    // could not be read" into the positive claim "nothing is ahead of you", so
    // the one case where the depth is worth knowing was the one case that
    // silently reported no wait. Unknown depth says nothing and is logged.
    let ahead = match ingest_jobs::jobs_ahead(&pool, ingest_jobs::QUEUE_PROJECT_STUDIO, job_id) {
        Ok(ahead) => ahead,
        Err(e) => {
            tracing::warn!(job_id = %job_id, error = %e, "ingest queue depth unreadable");
            return;
        }
    };
    if (ahead as usize) < WORKERS {
        return;
    }
    emit_line(
        tx,
        job_id,
        "log",
        "queue",
        format!("queued behind {ahead} ingest job(s)"),
        0,
    );
}

async fn run_job(
    task: IngestTask,
    tx: tokio::sync::broadcast::Sender<BusMessage>,
    cancel: Arc<AtomicBool>,
) {
    let IngestTask {
        core_db,
        router,
        project_pool,
        org_id,
        project_id,
        dir_path,
        source_id,
        job_id,
        files,
    } = task;

    let _ = repository::set_source_status(&project_pool, &source_id, "indexing", "");
    emit_line(
        &tx,
        &job_id,
        "phase",
        "extract",
        format!("indexing {} file(s)", files.len()),
        0,
    );

    let total = files.len().max(1) as u32;
    let mut done = 0u32;
    let mut ready = 0u32;
    let mut failed = 0u32;
    let mut cancelled = false;

    for work in files {
        // One beat per file: it records that this worker is alive AND reports a
        // cancel that arrived through the queue (another process, or the window
        // between the claim and the local registration).
        if let Ok(queue) = ingest_jobs::pool() {
            if !matches!(
                ingest_jobs::heartbeat(&queue, &job_id),
                Ok(ingest_jobs::JobLiveness::Running)
            ) {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let _ = repository::mark_file_indexing(&project_pool, &work.file_id);
        emit_line(
            &tx,
            &job_id,
            "file",
            "extract",
            work.path.clone(),
            done * 100 / total,
        );

        let outcome = process_file(
            &core_db,
            &router,
            &org_id,
            &project_id,
            &dir_path,
            &source_id,
            &work,
            &cancel,
        )
        .await;
        done += 1;
        let progress = done * 100 / total;
        match outcome {
            FileResult::Ready(chunks) => {
                ready += 1;
                let _ = repository::record_file_progress(
                    &project_pool,
                    &job_id,
                    &work.file_id,
                    "ready",
                    "",
                    chunks,
                    chunks,
                );
                emit_line(
                    &tx,
                    &job_id,
                    "progress",
                    "store",
                    format!("{} — {} chunks", work.path, chunks),
                    progress,
                );
            }
            FileResult::Skipped(reason) => {
                let _ = repository::record_file_progress(
                    &project_pool,
                    &job_id,
                    &work.file_id,
                    "skipped",
                    &reason,
                    0,
                    0,
                );
                emit_line(
                    &tx,
                    &job_id,
                    "log",
                    "extract",
                    format!("{} skipped: {}", work.path, reason),
                    progress,
                );
            }
            FileResult::Error(message) => {
                failed += 1;
                let _ = repository::record_file_progress(
                    &project_pool,
                    &job_id,
                    &work.file_id,
                    "error",
                    &message,
                    0,
                    0,
                );
                emit_line(
                    &tx,
                    &job_id,
                    "log",
                    "extract",
                    format!("{} failed: {}", work.path, message),
                    progress,
                );
            }
            FileResult::Cancelled => {
                cancelled = true;
                break;
            }
        }
    }

    // Cancel keeps everything already ingested; the job and source just stop.
    let (job_status, job_error, source_status, source_error) = if cancelled {
        ("cancelled", String::new(), "cancelled", String::new())
    } else if ready == 0 && failed > 0 {
        let msg = format!("{failed} file(s) failed");
        ("failed", msg.clone(), "error", msg)
    } else {
        ("success", String::new(), "ready", String::new())
    };
    let _ = repository::finish_ingest_job(&project_pool, &job_id, job_status, &job_error);
    let _ = repository::set_source_status(&project_pool, &source_id, source_status, &source_error);
}

enum FileResult {
    /// Stored; carries the chunk count.
    Ready(u32),
    Skipped(String),
    Error(String),
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
/// Published-name platformowego flow-ingestu (seed rdzenia). Ten sam flow obsluguje
/// addon RAG — rozne sa tylko scope zapisu i katalog przestrzeni wektorowej.
const INGEST_FLOW_MODEL: &str = "core:rag-ingest";

/// `IngestRequest.options` for one file. They are copied verbatim into
/// `envelope.meta`, so this carries only vector METADATA and the graph toggle —
/// never a decision about WHERE anything is written (that rides on the request's
/// `vector_home` / `graph_home` fields, which no node can rewrite).
fn ingest_flow_options(
    file_id: &str,
    source_id: &str,
    path: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut options = serde_json::Map::new();
    options.insert(
        "doc_id".to_string(),
        serde_json::Value::String(file_id.to_string()),
    );
    options.insert(
        "source_id".to_string(),
        serde_json::Value::String(source_id.to_string()),
    );
    options.insert(
        "path".to_string(),
        serde_json::Value::String(path.to_string()),
    );
    // Projects DO establish a graph home (`<project>/graph` on the request), so
    // the structural gate in `graph_extract` is open here — this is the caller
    // the node is aimed at. Extraction is nevertheless off by owner decision:
    // building a knowledge graph costs an extra LLM pass over every ingested
    // file, so it is opt-in, not something a project silently starts paying for.
    // Stamped explicitly because the shared key defaults to ON when absent.
    options.insert(
        "graph_enabled".to_string(),
        serde_json::Value::Bool(PROJECT_GRAPH_EXTRACTION_DEFAULT),
    );
    options
}

/// Ingestuje JEDEN plik przez wspolny flow zamiast wlasnej sciezki
/// extract -> chunk -> embed -> store.
///
/// Tozsamosc pisania jedzie w `ExecutionContext` (scope `ps-<project_id>`), a nie w
/// opcjach: `options` sa przepisywane wprost do `envelope.meta`, ktore moze nadpisac
/// kazdy wezel. `doc_id`/`source_id`/`path` ida przez meta swiadomie — to metadane
/// wektora, ktore wezel `store` ma zapisac, a nie decyzje o tym, GDZIE pisze.
///
/// Anulowanie: `execute_ingest` nie widzi naszej flagi, wiec pilnujemy jej obok i
/// przewracamy token. Bez tego zatrzymany job dalej mielilby model do konca pliku.
async fn ingest_file_via_flow(
    router: &Arc<Router>,
    org_id: &str,
    project_id: &str,
    dir_path: &Path,
    source_id: &str,
    work: &FileWork,
    bytes: Vec<u8>,
    cancel: &Arc<AtomicBool>,
) -> FileResult {
    let Some(executor) = router.executor() else {
        return FileResult::Error("model runtime executor not available".to_string());
    };
    let options = ingest_flow_options(&work.file_id, source_id, &work.path);

    let token = tokio_util::sync::CancellationToken::new();
    let request = crate::services::runtime::executor::IngestRequest {
        model: INGEST_FLOW_MODEL.to_string(),
        document_bytes: bytes,
        mime: work.mime.clone(),
        options,
        vector_home: Some(dir_path.join("vectors")),
        graph_home: Some(dir_path.join("graph")),
        cancel_token: Some(token.clone()),
        flow_depth: 0,
    };

    // §2.5 — the acting identity is the project: an ingest job outlives the
    // session that queued it, so the project id is the honest actor.
    let mut rctx = crate::services::runtime::context::ExecutionContext::new(
        None,
        crate::flow_engine::dispatcher::FlowOrigin::Project,
        crate::flow_engine::dispatcher::FlowActor::system_component(format!(
            "project:{project_id}"
        )),
    );
    rctx.addon_id = Some(vector_scope(project_id));
    rctx.org_id = Some(org_id.to_string());

    let fut = executor.execute_ingest(request, &mut rctx);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            result = &mut fut => {
                return match result {
                    Ok(response) => {
                        if cancel.load(Ordering::Relaxed) {
                            FileResult::Cancelled
                        } else if response.chunks == 0 {
                            FileResult::Skipped("no text content".to_string())
                        } else {
                            FileResult::Ready(response.chunks)
                        }
                    }
                    Err(e) => {
                        if cancel.load(Ordering::Relaxed) {
                            FileResult::Cancelled
                        } else {
                            FileResult::Error(e.to_string())
                        }
                    }
                };
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                if cancel.load(Ordering::Relaxed) {
                    token.cancel();
                }
            }
        }
    }
}

async fn process_file(
    core_db: &DbPool,
    router: &Arc<Router>,
    org_id: &str,
    project_id: &str,
    dir_path: &Path,
    source_id: &str,
    work: &FileWork,
    cancel: &Arc<AtomicBool>,
) -> FileResult {
    // Pliki binarne, ktore umie wspolny flow (proza / office / PDF / skan /
    // obraz), ida przez NIEGO — jedna implementacja parsowania, chunkingu,
    // embeddingu i zapisu, wspolna z addonem RAG. Natywnie zostaja tylko dwa
    // przypadki bez odpowiednika we flow: kod (chunkowany po liniach, z zakresem
    // linii jako `location`) i zrodla URL (trigger flow-ingestu przyjmuje wylacznie
    // blob binarny).
    if matches!(work.payload, WorkPayload::Blob) && code_ext(&work.path).is_none() {
        let blob_path = dir_path.join("files").join(&work.sha256);
        let bytes = match tokio::task::spawn_blocking(move || std::fs::read(&blob_path)).await {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => return FileResult::Error(format!("blob read: {e}")),
            Err(_) => return FileResult::Error("blob read task panicked".to_string()),
        };
        return ingest_file_via_flow(
            router, org_id, project_id, dir_path, source_id, work, bytes, cancel,
        )
        .await;
    }

    // Extraction is CPU/IO heavy (pdfium, zip, blocking HTTP) — off the
    // async worker.
    let extracted = {
        let dir = dir_path.to_path_buf();
        let work_clone = FileWork {
            file_id: work.file_id.clone(),
            path: work.path.clone(),
            sha256: work.sha256.clone(),
            mime: work.mime.clone(),
            payload: match &work.payload {
                WorkPayload::Blob => WorkPayload::Blob,
                WorkPayload::Url(u) => WorkPayload::Url(u.clone()),
            },
        };
        match tokio::task::spawn_blocking(move || extract_file(&dir, &work_clone)).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => return FileResult::Error(e.to_string()),
            Err(_) => return FileResult::Error("extraction task panicked".to_string()),
        }
    };

    let chunks = match extracted {
        ExtractOutcome::Skip(reason) => return FileResult::Skipped(reason),
        ExtractOutcome::Chunks(chunks) if chunks.is_empty() => {
            return FileResult::Skipped("no text content".to_string())
        }
        ExtractOutcome::Chunks(chunks) => chunks,
    };

    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
    for batch in chunks.chunks(EMBED_BATCH) {
        if cancel.load(Ordering::Relaxed) {
            return FileResult::Cancelled;
        }
        let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
        match embed_texts(router, texts).await {
            Ok(mut vecs) => vectors.append(&mut vecs),
            Err(e) => return FileResult::Error(e.to_string()),
        }
    }

    let store_result = {
        let core_db = core_db.clone();
        let org_id = org_id.to_string();
        let project_id = project_id.to_string();
        let dir = dir_path.to_path_buf();
        let src = source_id.to_string();
        let file_id = work.file_id.clone();
        let path = work.path.clone();
        let chunk_count = chunks.len() as u32;
        match tokio::task::spawn_blocking(move || {
            store_chunks_blocking(
                &core_db,
                &org_id,
                &project_id,
                &dir,
                &src,
                &file_id,
                &path,
                &chunks,
                &vectors,
            )
            .map(|_| chunk_count)
        })
        .await
        {
            Ok(r) => r,
            Err(_) => Err(anyhow!("vector store task panicked")),
        }
    };
    match store_result {
        Ok(count) => FileResult::Ready(count),
        Err(e) => FileResult::Error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Publishes ONE process-wide queue for the whole test binary, exactly as
    /// startup does — `start_job`, `signal_cancel` and `recover_orphaned_jobs`
    /// read the global handle, and a per-test pool would not exercise them.
    /// The guard makes queue tests exclusive: they assert on WHICH job a claim
    /// returns, and a parallel test's job in the same queue would decide that
    /// for them. The queue is drained before each one for the same reason.
    fn exclusive_queue() -> (std::sync::MutexGuard<'static, ()>, DbPool) {
        static QUEUE: OnceLock<(tempfile::TempDir, DbPool)> = OnceLock::new();
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_dir, pool) = QUEUE.get_or_init(|| {
            let dir = tempfile::tempdir().expect("queue tempdir");
            let pool = ingest_jobs::init(&dir.path().join("jobs.db")).expect("queue init");
            (dir, pool)
        });
        while let Some(job) = ingest_jobs::claim(pool, ingest_jobs::QUEUE_PROJECT_STUDIO)
            .expect("drain claim")
        {
            ingest_jobs::finish(pool, &job.job_id).expect("drain finish");
        }
        (guard, pool.clone())
    }

    /// A project database with one source and one job row, WITHOUT the central
    /// registry — enough for everything that writes a terminal job status.
    fn project_db_with_job(dir: &Path, job_id: &str) -> DbPool {
        std::fs::create_dir_all(dir.join("files")).expect("files dir");
        let (pool, _) = super::super::project_db::open_pool_at(dir).expect("project db");
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO sources (source_id, kind, name, created_by) \
                 VALUES ('src-1', 'document', 'Spec', 'tester')",
                [],
            )
            .expect("source");
        }
        repository::create_ingest_job(&pool, job_id, "src-1", 1, "tester").expect("job row");
        pool
    }

    fn task_for(
        state: &Arc<crate::dispatch::state::AppState>,
        dir: &Path,
        project_pool: &DbPool,
        project_id: &str,
        job_id: &str,
    ) -> IngestTask {
        IngestTask {
            core_db: state.db.clone(),
            router: state.router.clone(),
            project_pool: project_pool.clone(),
            org_id: "org-1".to_string(),
            project_id: project_id.to_string(),
            dir_path: dir.to_path_buf(),
            source_id: "src-1".to_string(),
            job_id: job_id.to_string(),
            files: vec![FileWork {
                file_id: "f-1".to_string(),
                path: "spec.txt".to_string(),
                sha256: "deadbeef".to_string(),
                mime: "text/plain".to_string(),
                payload: WorkPayload::Blob,
            }],
        }
    }

    /// R1: the job is persisted by `start_job`, outlives the process that
    /// accepted it (the startup reconciliation leaves it alone) and is still
    /// there to be claimed and run afterwards.
    #[test]
    fn a_queued_job_outlives_the_process_that_accepted_it() {
        let (_guard, queue) = exclusive_queue();
        let state = crate::dispatch::state::AppState::for_test();
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_id = format!("p-{}", uuid::Uuid::new_v4());
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        let project_pool = project_db_with_job(tmp.path(), &job_id);

        start_job(task_for(&state, tmp.path(), &project_pool, &project_id, &job_id));
        assert!(
            ingest_jobs::is_pending(&queue, &job_id).expect("pending"),
            "start_job must persist the job instead of only spawning it"
        );

        // Startup after a restart: reconciliation must not touch work that has
        // not started, and the project row must stay open for it.
        reconcile_orphans();
        recover_orphaned_jobs(&project_pool);
        let row = repository::get_ingest_job(&project_pool, &job_id)
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "running", "a queued job is still outstanding");

        let claimed = ingest_jobs::claim(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO)
            .expect("claim")
            .expect("the job survived");
        assert_eq!(claimed.job_id, job_id);
        let payload: JobPayload = serde_json::from_str(&claimed.payload_json).expect("payload");
        assert_eq!(payload.project_id, project_id);
        assert_eq!(payload.files.len(), 1);
        assert_eq!(payload.dir_path, tmp.path());
        ingest_jobs::finish(&queue, &job_id).expect("finish");
    }

    /// A `running` project row whose job the queue no longer holds belongs to a
    /// worker that died; one still in the queue does not.
    #[test]
    fn recovery_closes_only_jobs_the_queue_no_longer_holds() {
        let (_guard, queue) = exclusive_queue();
        let tmp = tempfile::tempdir().expect("tempdir");
        let queued_id = format!("job-{}", uuid::Uuid::new_v4());
        let project_pool = project_db_with_job(tmp.path(), &queued_id);
        let dead_id = format!("job-{}", uuid::Uuid::new_v4());
        repository::create_ingest_job(&project_pool, &dead_id, "src-1", 1, "tester").expect("row");
        ingest_jobs::enqueue(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO, &queued_id, "{}")
            .expect("enqueue");

        recover_orphaned_jobs(&project_pool);

        let queued = repository::get_ingest_job(&project_pool, &queued_id)
            .expect("read")
            .expect("row");
        assert_eq!(queued.status, "running", "queued work must not be killed");
        let dead = repository::get_ingest_job(&project_pool, &dead_id)
            .expect("read")
            .expect("row");
        assert_eq!(dead.status, "failed");
        assert_eq!(dead.error, "interrupted by restart");
        ingest_jobs::finish(&queue, &queued_id).expect("finish");
    }

    /// Cancellation of a job nobody has claimed: it leaves the queue at once
    /// (the delete paths wait for a terminal status) and the project row and its
    /// source are closed as cancelled.
    #[test]
    fn cancelling_a_waiting_job_closes_it_without_running_it() {
        let (_guard, queue) = exclusive_queue();
        let state = crate::dispatch::state::AppState::for_test();
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_id = format!("p-{}", uuid::Uuid::new_v4());
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        let project_pool = project_db_with_job(tmp.path(), &job_id);
        start_job(task_for(&state, tmp.path(), &project_pool, &project_id, &job_id));

        // `signal_cancel` resolves the project through the central registry;
        // the terminal write itself is what this asserts.
        let outcome = ingest_jobs::request_cancel(&queue, &job_id).expect("cancel");
        let ingest_jobs::CancelOutcome::Dequeued(payload_json) = outcome else {
            panic!("a job nobody claimed must leave the queue");
        };
        let payload: JobPayload = serde_json::from_str(&payload_json).expect("payload");
        close_job_as_cancelled(&project_pool, &payload);

        assert!(
            ingest_jobs::claim(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO)
                .expect("claim")
                .is_none(),
            "a cancelled job must never be handed to a worker"
        );
        let row = repository::get_ingest_job(&project_pool, &job_id)
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "cancelled");
        assert!(row.finished_at.is_some());
        let source = repository::get_source(&project_pool, "src-1")
            .expect("read")
            .expect("source");
        assert_eq!(source.status, "cancelled");
    }

    /// A cancel that lands on a CLAIMED job is persisted on the row, because the
    /// worker may be in another process; the worker sees it at its next beat.
    #[test]
    fn cancelling_a_claimed_job_reaches_it_through_the_queue() {
        let (_guard, queue) = exclusive_queue();
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        ingest_jobs::enqueue(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO, &job_id, "{}")
            .expect("enqueue");
        ingest_jobs::claim(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO)
            .expect("claim")
            .expect("job");
        assert_eq!(
            ingest_jobs::request_cancel(&queue, &job_id).expect("cancel"),
            ingest_jobs::CancelOutcome::Signalled
        );
        assert_eq!(
            ingest_jobs::heartbeat(&queue, &job_id).expect("beat"),
            ingest_jobs::JobLiveness::CancelRequested
        );
        ingest_jobs::finish(&queue, &job_id).expect("finish");
    }

    /// A panicking pipeline must leave the job FAILED. Without the guard the row
    /// would stay `running` and every poller and delete path would wait on it.
    #[tokio::test]
    async fn a_panicking_job_is_recorded_as_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        let project_pool = project_db_with_job(tmp.path(), &job_id);

        run_guarded(&project_pool, &job_id, async {
            panic!("ingest blew up");
        })
        .await;

        let row = repository::get_ingest_job(&project_pool, &job_id)
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "failed");
        assert_eq!(row.error, "ingest task panicked");
    }

    /// A project that exists BOTH in the central registry and on disk, so
    /// `project_db::open` — the call the worker path makes, having no request
    /// context to inherit a pool from — resolves it exactly as in production.
    /// Returns the project id, its directory and the sha of one empty code
    /// blob the pipeline can run on.
    fn registered_project(root: &Path) -> (String, PathBuf, String) {
        let _ = super::super::db::init(&root.join("projects.db"));
        let project_id = format!("p-{}", uuid::Uuid::new_v4());
        let dir = root.join(&project_id);
        std::fs::create_dir_all(dir.join("files")).expect("project dir");
        repository::create_project(
            &project_id,
            "org-1",
            &format!("Projekt {project_id}"),
            "",
            "knowledge",
            "[\"knowledge\"]",
            "tester",
            &dir.to_string_lossy(),
            &[],
        )
        .expect("registry row");
        let pool = super::super::project_db::open(&project_id).expect("project db");
        repository::create_source(&pool, "src-1", "document", "Spec", "{}", "tester")
            .expect("source");
        // An EMPTY source file: the real pipeline extracts it, finds no text
        // and truthfully skips it, so the job completes without a model
        // runtime instead of being faked past the pipeline.
        let sha = hex::encode(Sha256::digest(b""));
        std::fs::write(dir.join("files").join(&sha), b"").expect("blob");
        (project_id, dir, sha)
    }

    /// Registers one more job (row + file) on an already registered project.
    fn queued_task(
        state: &Arc<crate::dispatch::state::AppState>,
        project_id: &str,
        dir: &Path,
        sha: &str,
        job_id: &str,
        path: &str,
    ) -> IngestTask {
        let pool = super::super::project_db::open(project_id).expect("project db");
        let file_id =
            repository::upsert_source_file(&pool, "src-1", path, sha, 0, "text/x-rust")
                .expect("file row");
        repository::create_ingest_job(&pool, job_id, "src-1", 1, "tester").expect("job row");
        IngestTask {
            core_db: state.db.clone(),
            router: state.router.clone(),
            project_pool: pool,
            org_id: "org-1".to_string(),
            project_id: project_id.to_string(),
            dir_path: dir.to_path_buf(),
            source_id: "src-1".to_string(),
            job_id: job_id.to_string(),
            files: vec![FileWork {
                file_id,
                path: path.to_string(),
                sha256: sha.to_string(),
                mime: "text/x-rust".to_string(),
                payload: WorkPayload::Blob,
            }],
        }
    }

    /// Waits for the project row to leave `running`. The re-notify covers the
    /// window in which a worker has not yet registered for the wake signal —
    /// production closes it with `IDLE_POLL`, which no test wants to sit
    /// through.
    async fn await_terminal(project_id: &str, job_id: &str) -> super::super::models::IngestJobRecord {
        for _ in 0..600 {
            let pool = super::super::project_db::open(project_id).expect("project db");
            if let Ok(Some(row)) = repository::get_ingest_job(&pool, job_id) {
                if row.status != "running" {
                    return row;
                }
            }
            wake().notify_waiters();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the job never reached a terminal status");
    }

    /// R1 end to end: `start_job` persists the job, it survives the project
    /// pool being closed and reopened plus a full startup reconciliation, and
    /// the REAL worker path (`worker_loop` → `run_claimed` → `run_job`) carries
    /// it to a terminal project row and clears the queue.
    #[tokio::test]
    async fn a_persisted_job_is_completed_by_the_worker_after_a_reopen() {
        let (_guard, queue) = exclusive_queue();
        let state = crate::dispatch::state::AppState::for_test();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());
        let job_id = format!("job-{}", uuid::Uuid::new_v4());

        start_job(queued_task(
            &state,
            &project_id,
            &dir,
            &sha,
            &job_id,
            "main.rs",
        ));
        assert!(
            ingest_jobs::is_pending(&queue, &job_id).expect("pending"),
            "start_job must persist the job instead of only spawning it"
        );

        // The process that accepted the request is gone: its project pool is
        // closed, and the next one starts with reconciliation and recovery.
        super::super::project_db::close(&project_id);
        reconcile_orphans();
        let reopened = super::super::project_db::open(&project_id).expect("reopen");
        recover_orphaned_jobs(&reopened);
        assert_eq!(
            repository::get_ingest_job(&reopened, &job_id)
                .expect("read")
                .expect("row")
                .status,
            "running",
            "work that has not started must survive the restart intact"
        );

        let worker = tokio::spawn(worker_loop());
        let row = await_terminal(&project_id, &job_id).await;
        worker.abort();

        assert_eq!(row.status, "success", "error: {}", row.error);
        assert_eq!(row.files_done, 1, "the pipeline really ran the file");
        assert!(row.finished_at.is_some());
        assert!(
            !ingest_jobs::is_pending(&queue, &job_id).expect("pending"),
            "a completed job must leave the queue"
        );
        let source = repository::get_source(&reopened, "src-1")
            .expect("read")
            .expect("source");
        assert_eq!(source.status, "ready");
    }

    /// `signal_cancel` is the entry point the dispatch layer calls, and it has
    /// to handle both lives of a job: waiting in the queue (removed and closed
    /// here and now) and claimed by a worker (persisted on the row, so a worker
    /// in another process still sees it).
    #[test]
    fn signal_cancel_closes_a_waiting_job_and_flags_a_claimed_one() {
        let (_guard, queue) = exclusive_queue();
        let state = crate::dispatch::state::AppState::for_test();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());

        let waiting = format!("job-{}", uuid::Uuid::new_v4());
        start_job(queued_task(
            &state, &project_id, &dir, &sha, &waiting, "waiting.rs",
        ));
        assert!(signal_cancel(&waiting), "a queued job is cancellable");
        assert!(
            !ingest_jobs::is_pending(&queue, &waiting).expect("pending"),
            "a cancelled job must never be handed to a worker"
        );
        let project_pool = super::super::project_db::open(&project_id).expect("project db");
        let row = repository::get_ingest_job(&project_pool, &waiting)
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "cancelled");
        assert_eq!(
            repository::get_source(&project_pool, "src-1")
                .expect("read")
                .expect("source")
                .status,
            "cancelled"
        );

        // Claimed: the worker may live in another process, so the request is
        // persisted on the row rather than resolved here.
        let claimed = format!("job-{}", uuid::Uuid::new_v4());
        start_job(queued_task(
            &state, &project_id, &dir, &sha, &claimed, "claimed.rs",
        ));
        ingest_jobs::claim(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO)
            .expect("claim")
            .expect("job");
        assert!(signal_cancel(&claimed), "a claimed job is cancellable");
        assert_eq!(
            ingest_jobs::heartbeat(&queue, &claimed).expect("beat"),
            ingest_jobs::JobLiveness::CancelRequested,
            "the worker learns about the cancel at its next beat"
        );
        assert_eq!(
            repository::get_ingest_job(&project_pool, &claimed)
                .expect("read")
                .expect("row")
                .status,
            "running",
            "a claimed job is closed by its worker, not by the cancel"
        );
        ingest_jobs::finish(&queue, &claimed).expect("finish");
    }

    /// A panic OUTSIDE the pipeline — in `run_claimed` itself, where the
    /// project database, the terminal read and the stream close live — must not
    /// take the worker down with it. The job is recorded, the queue row goes,
    /// and the SAME worker completes the next job.
    #[tokio::test]
    async fn a_panic_in_a_finalizer_records_the_job_and_spares_the_worker() {
        let (_guard, queue) = exclusive_queue();
        let state = crate::dispatch::state::AppState::for_test();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());

        let worker = tokio::spawn(worker_loop());

        let exploding = format!("job-{}", uuid::Uuid::new_v4());
        PANIC_AFTER_PROJECT_OPEN.store(true, Ordering::SeqCst);
        start_job(queued_task(
            &state, &project_id, &dir, &sha, &exploding, "boom.rs",
        ));
        let row = await_terminal(&project_id, &exploding).await;
        assert_eq!(row.status, "failed");
        assert_eq!(row.error, PANIC_ERROR, "the panic is recorded, not swallowed");
        assert!(
            !ingest_jobs::is_pending(&queue, &exploding).expect("pending"),
            "a panicked job must not stay claimed forever"
        );

        // The proof that the worker is alive: it claims and completes the next
        // job. Before the guard, one panic ended ingest for the process.
        let after = format!("job-{}", uuid::Uuid::new_v4());
        start_job(queued_task(
            &state, &project_id, &dir, &sha, &after, "after.rs",
        ));
        let next = await_terminal(&project_id, &after).await;
        worker.abort();
        assert_eq!(next.status, "success", "error: {}", next.error);
    }

    /// Leaves behind exactly what a worker killed mid-ingest leaves: a project
    /// job row still `running`, the source it was writing still `indexing`, and
    /// a queue row claimed by a process run that no longer exists.
    fn orphan_mid_ingest(queue: &DbPool, project_id: &str, dir: &Path, sha: &str, job_id: &str) {
        let pool = super::super::project_db::open(project_id).expect("project db");
        let file_id =
            repository::upsert_source_file(&pool, "src-1", "spec.rs", sha, 0, "text/x-rust")
                .expect("file row");
        repository::create_ingest_job(&pool, job_id, "src-1", 1, "tester").expect("job row");
        repository::set_source_status(&pool, "src-1", "indexing", "").expect("source indexing");
        let payload = serde_json::to_string(&JobPayload {
            org_id: "org-1".to_string(),
            project_id: project_id.to_string(),
            dir_path: dir.to_path_buf(),
            source_id: "src-1".to_string(),
            job_id: job_id.to_string(),
            files: vec![FileWork {
                file_id,
                path: "spec.rs".to_string(),
                sha256: sha.to_string(),
                mime: "text/x-rust".to_string(),
                payload: WorkPayload::Blob,
            }],
        })
        .expect("payload");
        let conn = queue.write().expect("write");
        conn.execute(
            "INSERT INTO ingest_jobs (job_id, queue, payload_json, status, owner_instance, \
             enqueued_at_ms, claimed_at_ms) \
             VALUES (?1, ?2, ?3, 'running', 'gone-instance', 1, 1)",
            rusqlite::params![job_id, ingest_jobs::QUEUE_PROJECT_STUDIO, payload],
        )
        .expect("orphan row");
    }

    /// Asserts the ONE outcome an orphan may end in: the job failed with the
    /// restart reason and the source it was indexing failed with it too.
    fn assert_closed_by_restart(pool: &DbPool, job_id: &str) {
        let row = repository::get_ingest_job(pool, job_id)
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "failed", "error: {}", row.error);
        assert_eq!(row.error, "interrupted by restart");
        assert!(row.finished_at.is_some());
        let source = repository::get_source(pool, "src-1")
            .expect("read")
            .expect("source");
        assert_eq!(
            source.status, "error",
            "a source left at `indexing` is a document forever mid-ingest"
        );
        assert_eq!(source.error, "interrupted by restart");
    }

    /// The crash shape the reconciliation exists for, with the project pool
    /// CLOSED — so `project_db::open` inside the sweep is a fresh one and fires
    /// its recovery hook. That hook used to close the job first, after which the
    /// sweep's own guarded write found nothing left to guard and skipped the
    /// source: the job ended `failed` and the document stayed `indexing` for the
    /// life of the installation.
    #[test]
    fn a_restart_never_leaves_a_source_stuck_indexing() {
        let (_guard, queue) = exclusive_queue();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        orphan_mid_ingest(&queue, &project_id, &dir, &sha, &job_id);

        // The process that owned the job is gone, and so is its project pool.
        super::super::project_db::close(&project_id);
        reconcile_orphans();

        // Read the project file through a plain pool, NOT `project_db::open`:
        // that call would fire the fresh-open recovery hook a second time and
        // do the sweep's work for it, hiding a sweep that skipped the project.
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("project db");
        assert_closed_by_restart(&pool, &job_id);
        assert!(
            !ingest_jobs::is_pending(&queue, &job_id).expect("pending"),
            "the orphaned queue row is cleared"
        );
    }

    /// The same orphan on a project pool this process ALREADY holds open — the
    /// case the fresh-open hook does not cover, because there is no fresh open.
    /// The sweep must recover it through its own explicit call, or the queue row
    /// disappears while the project database still claims the job is running.
    #[test]
    fn reconciliation_recovers_an_orphan_on_an_already_open_project() {
        let (_guard, queue) = exclusive_queue();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        orphan_mid_ingest(&queue, &project_id, &dir, &sha, &job_id);

        let pool = super::super::project_db::open(&project_id).expect("project db");
        reconcile_orphans();

        assert_closed_by_restart(&pool, &job_id);
        assert!(
            !ingest_jobs::is_pending(&queue, &job_id).expect("pending"),
            "the orphaned queue row is cleared"
        );
    }

    /// The crash window: the worker wrote the terminal project row and died
    /// before deleting its queue row. The next startup sees a `running` row of
    /// a dead instance — and must NOT rewrite a recorded success as
    /// "interrupted by restart".
    #[test]
    fn reconciliation_never_rewrites_a_recorded_success() {
        let (_guard, queue) = exclusive_queue();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());
        let job_id = format!("job-{}", uuid::Uuid::new_v4());
        let project_pool = super::super::project_db::open(&project_id).expect("project db");
        let file_id = repository::upsert_source_file(&project_pool, "src-1", "done.rs", &sha, 0, "text/x-rust")
            .expect("file row");
        repository::create_ingest_job(&project_pool, &job_id, "src-1", 1, "tester").expect("job row");

        // What the worker had already done before the crash.
        assert!(
            repository::finish_ingest_job(&project_pool, &job_id, "success", "").expect("finish"),
            "the first terminal write lands"
        );
        repository::set_source_status(&project_pool, "src-1", "ready", "").expect("source");

        // What the crash left behind: a claimed row of a process run that is
        // gone, holding the very payload the job succeeded with.
        let payload = serde_json::to_string(&JobPayload {
            org_id: "org-1".to_string(),
            project_id: project_id.clone(),
            dir_path: dir.clone(),
            source_id: "src-1".to_string(),
            job_id: job_id.clone(),
            files: vec![FileWork {
                file_id,
                path: "done.rs".to_string(),
                sha256: sha.clone(),
                mime: "text/x-rust".to_string(),
                payload: WorkPayload::Blob,
            }],
        })
        .expect("payload");
        {
            let conn = queue.write().expect("write");
            conn.execute(
                "INSERT INTO ingest_jobs (job_id, queue, payload_json, status, owner_instance, \
                 enqueued_at_ms, claimed_at_ms) VALUES (?1, ?2, ?3, 'running', 'gone-instance', 1, 1)",
                rusqlite::params![job_id, ingest_jobs::QUEUE_PROJECT_STUDIO, payload],
            )
            .expect("orphan row");
        }

        reconcile_orphans();
        recover_orphaned_jobs(&project_pool);

        let row = repository::get_ingest_job(&project_pool, &job_id)
            .expect("read")
            .expect("row");
        assert_eq!(row.status, "success", "a recorded success survives recovery");
        assert_eq!(row.error, "");
        assert_eq!(
            repository::get_source(&project_pool, "src-1")
                .expect("read")
                .expect("source")
                .status,
            "ready",
            "the source must not be relabelled either"
        );
        assert!(
            !ingest_jobs::is_pending(&queue, &job_id).expect("pending"),
            "the stale queue row is still cleared"
        );
    }

    /// A job that has to WAIT for a worker says so on its own stream. Before the
    /// queue, a job that could not start reported it through the document gate;
    /// with workers bounded, a job can now wait in the QUEUE instead and used to
    /// emit nothing at all. The line states what the queue can prove — the work
    /// ahead of it — and claims no progress.
    #[test]
    fn a_job_queued_behind_others_reports_the_wait() {
        let (_guard, queue) = exclusive_queue();
        let state = crate::dispatch::state::AppState::for_test();
        let tmp = tempfile::tempdir().expect("tempdir");
        let (project_id, dir, sha) = registered_project(tmp.path());

        // Enough outstanding work that no worker could be free.
        for i in 0..WORKERS {
            let busy = format!("job-{}", uuid::Uuid::new_v4());
            start_job(queued_task(
                &state,
                &project_id,
                &dir,
                &sha,
                &busy,
                &format!("busy{i}.rs"),
            ));
        }

        let waiting = format!("job-{}", uuid::Uuid::new_v4());
        let mut rx = log_bus::sender_for(&waiting).subscribe();
        start_job(queued_task(
            &state, &project_id, &dir, &sha, &waiting, "waiting.rs",
        ));

        let mut lines = Vec::new();
        while let Ok(BusMessage::Line(line)) = rx.try_recv() {
            lines.push(line);
        }
        assert!(
            lines.iter().any(|l| l.phase == "queue"
                && l.line == format!("queued behind {WORKERS} ingest job(s)")),
            "a job waiting for a worker must say so; got {} line(s)",
            lines.len()
        );
        assert!(
            lines.iter().all(|l| l.progress_pct == 0),
            "a queued job has made no progress to report"
        );

        while let Some(job) = ingest_jobs::claim(&queue, ingest_jobs::QUEUE_PROJECT_STUDIO)
            .expect("drain claim")
        {
            ingest_jobs::finish(&queue, &job.job_id).expect("drain finish");
        }
    }

    #[test]
    fn ref_id_matches_store_adapter_deterministics() {
        // Same doc + index → same id; different index/doc → different id;
        // never zero (reserved by zvec).
        let a = crate::services::vector::doc_vectors::ref_id_for("doc-1", 0);
        assert_eq!(
            a,
            crate::services::vector::doc_vectors::ref_id_for("doc-1", 0)
        );
        assert_ne!(
            a,
            crate::services::vector::doc_vectors::ref_id_for("doc-1", 1)
        );
        assert_ne!(
            a,
            crate::services::vector::doc_vectors::ref_id_for("doc-2", 0)
        );
        assert_ne!(crate::services::vector::doc_vectors::ref_id_for("", 0), 0);
    }

    #[test]
    fn chunk_code_headers_carry_path_and_line_ranges() {
        let content = (1..=200)
            .map(|i| format!("let x{i} = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_code("src/lib.rs", &content);
        assert!(!chunks.is_empty());
        assert!(chunks[0].text.starts_with("// src/lib.rs:1-"));
        assert!(chunks[0].location.starts_with("l. 1–"));
        for c in &chunks {
            assert!(
                c.text.chars().count() <= CHUNK_SIZE_CHARS + 64,
                "header + body bounded"
            );
        }
    }

    #[test]
    fn upload_accumulator_orders_limits_and_finalizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();

        // Out-of-order start (seq=1 without seq=0) is rejected.
        assert!(accept_upload_chunk(
            "org",
            "u1",
            "p1",
            dir,
            "up1",
            "a.txt",
            "text/plain",
            1,
            2,
            b"x"
        )
        .is_err());

        // Two ordered chunks finalize to a content-addressed blob.
        let r0 = accept_upload_chunk(
            "org",
            "u1",
            "p1",
            dir,
            "up1",
            "a.txt",
            "text/plain",
            0,
            2,
            b"hello ",
        )
        .expect("chunk 0");
        assert!(matches!(
            r0,
            UploadOutcome::Buffered {
                received_chunks: 1,
                ..
            }
        ));
        let r1 = accept_upload_chunk(
            "org",
            "u1",
            "p1",
            dir,
            "up1",
            "a.txt",
            "text/plain",
            1,
            2,
            b"world",
        )
        .expect("chunk 1");
        let sha = match r1 {
            UploadOutcome::Finalized {
                sha256, size_bytes, ..
            } => {
                assert_eq!(size_bytes, 11);
                sha256
            }
            _ => panic!("expected finalized"),
        };
        let blob = dir.join("files").join(&sha);
        assert_eq!(std::fs::read(&blob).expect("blob"), b"hello world");
        let meta = finalized_meta("org", "u1", "p1", &sha).expect("meta");
        assert_eq!(meta.filename, "a.txt");
        assert_eq!(meta.size_bytes, 11);

        // Same upload_id from two different users must not collide: the part
        // file name hashes the full (org, user, project, upload_id) key.
        for (user, c0) in [("ua", b"aaa"), ("ub", b"bbb")] {
            let r = accept_upload_chunk(
                "org",
                user,
                "p1",
                dir,
                "shared",
                "s.txt",
                "text/plain",
                0,
                2,
                c0,
            )
            .expect("chunk 0");
            assert!(matches!(r, UploadOutcome::Buffered { .. }));
        }
        let sha_a = match accept_upload_chunk(
            "org",
            "ua",
            "p1",
            dir,
            "shared",
            "s.txt",
            "text/plain",
            1,
            2,
            b"111",
        )
        .expect("ua finalize")
        {
            UploadOutcome::Finalized { sha256, .. } => sha256,
            _ => panic!("expected finalized"),
        };
        let sha_b = match accept_upload_chunk(
            "org",
            "ub",
            "p1",
            dir,
            "shared",
            "s.txt",
            "text/plain",
            1,
            2,
            b"222",
        )
        .expect("ub finalize")
        {
            UploadOutcome::Finalized { sha256, .. } => sha256,
            _ => panic!("expected finalized"),
        };
        assert_ne!(sha_a, sha_b);
        assert_eq!(
            std::fs::read(dir.join("files").join(&sha_a)).expect("blob a"),
            b"aaa111"
        );
        assert_eq!(
            std::fs::read(dir.join("files").join(&sha_b)).expect("blob b"),
            b"bbb222"
        );

        // Traversal in upload_id is rejected before any FS access.
        assert!(accept_upload_chunk(
            "org",
            "u1",
            "p1",
            dir,
            "../evil",
            "a.txt",
            "text/plain",
            0,
            1,
            b"x"
        )
        .is_err());
        // Oversized single chunk is rejected.
        let big = vec![0u8; MAX_UPLOAD_CHUNK_BYTES + 1];
        assert!(accept_upload_chunk(
            "org",
            "u1",
            "p1",
            dir,
            "up2",
            "b.bin",
            "application/octet-stream",
            0,
            1,
            &big
        )
        .is_err());
    }
    /// The owner decision for Projects: a project ingest does NOT build a
    /// knowledge graph. The shared `graph_enabled` key means ON when absent, so
    /// this must be stamped explicitly — a missing key silently turns an extra
    /// LLM pass on for every ingested file.
    #[test]
    fn project_ingest_options_turn_graph_extraction_off() {
        let options = super::ingest_flow_options("file-1", "src-1", "docs/a.pdf");
        assert_eq!(
            options.get("graph_enabled"),
            Some(&serde_json::Value::Bool(super::PROJECT_GRAPH_EXTRACTION_DEFAULT)),
            "Projects ingest must state the graph decision, not stay silent — an \
             absent key means ON in the shared node"
        );
        assert!(
            !super::PROJECT_GRAPH_EXTRACTION_DEFAULT,
            "the recorded owner decision is graph OFF by default in Projects"
        );
        assert_eq!(
            options.get("doc_id").and_then(|v| v.as_str()),
            Some("file-1")
        );
        assert_eq!(
            options.get("source_id").and_then(|v| v.as_str()),
            Some("src-1")
        );
        assert_eq!(
            options.get("path").and_then(|v| v.as_str()),
            Some("docs/a.pdf")
        );
    }
}
