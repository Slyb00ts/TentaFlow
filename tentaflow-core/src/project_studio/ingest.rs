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
use sha2::{Digest, Sha256};

use super::repository;
use crate::api::openai::types::{EmbeddingInput, EmbeddingRequest};
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::routing::router::Router;
use crate::services::document::extract::{
    classify_source, split_into_chunks, SourceKind, CHUNK_OVERLAP_CHARS, CHUNK_SIZE_CHARS,
};
use crate::services::vector::backend::{Field, FieldSpec, Metric, UpsertItem};
use crate::services::vector::error::VectorError;
use tentaflow_sdk_spec::{FieldType, FieldValue};

/// Embeddings model alias resolved by the platform (same alias the RAG addon
/// uses, so one embedding space serves both).
pub const EMBEDDINGS_ALIAS: &str = "rag-embeddings";

/// Vector namespace holding the project's knowledge chunks.
pub const VECTOR_NAMESPACE: &str = "passages";

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

/// Flags a live run for cancellation. `false` = this process does not own it.
pub fn signal_cancel(job_id: &str) -> bool {
    INGEST_CANCEL.signal(job_id)
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
/// job without a cancel-registry entry has no live task behind it (the
/// process restarted mid-job), so it is closed as failed — otherwise pollers
/// and the delete paths would wait on it forever.
pub fn recover_orphaned_jobs(pool: &DbPool) {
    let Ok(jobs) = repository::running_job_ids(pool) else {
        return;
    };
    for job_id in jobs {
        if INGEST_CANCEL.is_registered(&job_id) {
            continue;
        }
        tracing::warn!(job_id, "marking orphaned ingest job as failed");
        let _ = repository::finish_ingest_job(pool, &job_id, "failed", "interrupted by restart");
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
pub enum WorkPayload {
    /// Content-addressed blob `<dir_path>/files/<sha256>`.
    Blob,
    /// Single public web page, fetched through the web_research SSRF guard.
    Url(String),
}

/// One file to process in an ingest job.
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

/// Everything a job needs, captured before the spawn.
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

/// Starts the ingest job for an already-created `ingest_jobs` row: registers
/// the cancel token, opens the log_bus channel BEFORE spawning (the response
/// returns to the frontend before the task runs — an immediate stream
/// subscribe must find the channel) and spawns the pipeline with a panic
/// guard so the finalizers (job status, End, close, unregister) always run.
pub fn start_job(task: IngestTask) {
    let job_id = task.job_id.clone();
    let cancel = register_cancel(&job_id);
    let tx = log_bus::sender_for(&job_id);

    tokio::spawn(async move {
        let project_pool = task.project_pool.clone();
        let job_id_task = task.job_id.clone();

        // Wspolbieznosc ogranicza teraz JEDNA bramka w `execute_ingest`
        // (`services::ingest_gate`), przez ktora przechodzi tez addon RAG — a
        // liczy ona DOKUMENTY, czyli to, co realnie ogranicza pamiec. Wlasny
        // semafor na poziomie zadania byl drugim, niezaleznym limitem tego samego
        // zasobu i przepuszczal jeden dokument z kazdego zadania i tak.
        //
        // Zadanie anulowane, zanim tknelo jakikolwiek plik, konczy sie terminalnie
        // bez przetwarzania — na tym opieraja sie sciezki kasowania — bo `run_job`
        // sprawdza flage przed kazdym plikiem.
        if cancel.load(Ordering::Relaxed) {
            let _ = repository::finish_ingest_job(&project_pool, &job_id_task, "cancelled", "");
        } else {
            if crate::services::ingest_gate::would_wait() {
                emit_line(
                    &tx,
                    &job_id_task,
                    "log",
                    "queue",
                    "waiting for a free ingest slot".to_string(),
                    0,
                );
            }
            let run = {
                let tx = tx.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move { run_job(task, tx, cancel).await })
            };
            if let Err(join_err) = run.await {
                if join_err.is_panic() {
                    let _ = repository::finish_ingest_job(
                        &project_pool,
                        &job_id_task,
                        "failed",
                        "ingest task panicked",
                    );
                }
            }
        }

        // Terminal from the DB row — the single source of truth the polling
        // endpoint also reads (success | failed | cancelled).
        let (status, error) = match repository::get_ingest_job(&project_pool, &job_id_task) {
            Ok(Some(job)) => (job.status, job.error),
            _ => ("failed".to_string(), "job record missing".to_string()),
        };
        let _ = tx.send(BusMessage::End {
            deploy_id: job_id_task.clone(),
            final_status: status,
            image_tag: String::new(),
            container_name: String::new(),
            error_message: error,
            duration_ms: 0,
        });
        // Give live subscribers a moment to drain End before the channel dies.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        log_bus::close(&job_id_task);
        unregister_cancel(&job_id_task);
    });
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
    let mut options = serde_json::Map::new();
    options.insert(
        "doc_id".to_string(),
        serde_json::Value::String(work.file_id.clone()),
    );
    options.insert(
        "source_id".to_string(),
        serde_json::Value::String(source_id.to_string()),
    );
    options.insert(
        "path".to_string(),
        serde_json::Value::String(work.path.clone()),
    );

    let token = tokio_util::sync::CancellationToken::new();
    let request = crate::services::runtime::executor::IngestRequest {
        model: INGEST_FLOW_MODEL.to_string(),
        document_bytes: bytes,
        mime: work.mime.clone(),
        options,
        vector_home: Some(dir_path.join("vectors")),
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
}
