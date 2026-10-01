// ============ File: project_studio/media.rs — bounded attachment IO and durable video previews ============

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

#[cfg(feature = "camera")]
use anyhow::Context;
use anyhow::{anyhow, Result};
use dashmap::DashMap;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dispatch::HandlerContext;

pub fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn safe_directory(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(anyhow!("media directory is not a regular directory"))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !std::fs::symlink_metadata(&path)?.file_type().is_dir() {
                        return Err(anyhow!("media directory is not a regular directory"));
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}

pub fn open_options_regular(path: &Path, options: &mut OpenOptions) -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_file()) {
        return Err(anyhow!("media path is not a regular file"));
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(anyhow!("media path is not a regular file"));
    }
    Ok(file)
}

pub fn open_regular(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    open_options_regular(path, &mut options)
}

pub fn write_metadata<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("metadata has no parent directory"))?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = open_options_regular(&temp, &mut options)?;
        serde_json::to_writer(&mut file, value)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

pub fn hash_file(path: &Path, allowed: &dyn Fn() -> Result<()>) -> Result<String> {
    let mut file = open_regular(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        allowed()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn ensure_write_space(dir: &Path, bytes: u64) -> Result<()> {
    crate::sync::storage_monitor::ensure_large_blob_allowed(bytes)
        .map_err(|e| anyhow!("insufficient disk space: {e}"))?;
    let report = crate::sync::storage_monitor::report_for_root(dir)?;
    if report.level == crate::sync::storage_monitor::StoragePressureLevel::Critical
        || !report.can_accept_large_blob(bytes)
        || report.available_bytes.is_some_and(|free| free < bytes)
    {
        return Err(anyhow!(
            "insufficient disk space; free space and resume the upload"
        ));
    }
    Ok(())
}

pub fn read_range(path: &Path, offset: u64, max_bytes: u32) -> Result<(Vec<u8>, u64, bool)> {
    if max_bytes == 0 || max_bytes as usize > super::ingest::MAX_UPLOAD_CHUNK_BYTES {
        return Err(anyhow!("read length must be 1..4 MiB"));
    }
    let mut file = open_regular(path)?;
    let size = file.metadata()?.len();
    if offset > size {
        return Err(anyhow!("read offset exceeds attachment size"));
    }
    let length = (size - offset).min(max_bytes as u64) as usize;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0u8; length];
    file.read_exact(&mut bytes)?;
    Ok((bytes, size, offset + length as u64 == size))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PreviewState {
    pub status: String,
    pub error: String,
    pub total_size: u64,
    pub duration_ms: u64,
}

fn previews_dir(dir: &Path) -> Result<PathBuf> {
    let files = safe_directory(dir, "files")?;
    safe_directory(&files, ".previews")
}

pub fn read_preview_state(dir: &Path, sha: &str) -> Result<Option<PreviewState>> {
    let path = previews_dir(dir)?.join(format!("{sha}.json"));
    match open_regular(&path) {
        Ok(file) => {
            if file.metadata()?.len() > 16 * 1024 {
                return Err(anyhow!("preview metadata exceeds limit"));
            }
            Ok(Some(serde_json::from_reader(file)?))
        }
        Err(error)
            if !path.exists()
                && std::fs::symlink_metadata(&path).is_err()
                && error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn preview_path(dir: &Path, sha: &str) -> Result<PathBuf> {
    if !is_sha256(sha) {
        return Err(anyhow!("invalid attachment SHA-256"));
    }
    let state =
        read_preview_state(dir, sha)?.ok_or_else(|| anyhow!("preview has not been requested"))?;
    if state.status != "ready" {
        return Err(anyhow!("video preview is not ready"));
    }
    let path = previews_dir(dir)?.join(format!("{sha}.mp4"));
    if open_regular(&path)?.metadata()?.len() != state.total_size {
        return Err(anyhow!(
            "video preview file does not match completed metadata"
        ));
    }
    Ok(path)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AttachmentOwner {
    pub kind: tentaflow_protocol::project_studio::AttachmentOwnerKind,
    pub id: String,
    pub step_index: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct PreviewJob {
    org_id: String,
    user_id: String,
    project_id: String,
    owner: AttachmentOwner,
    sha256: String,
}

struct PreviewWorker {
    database: Weak<crate::db::Db>,
    state: std::sync::RwLock<Weak<crate::dispatch::AppState>>,
    namespace: String,
    wake: tokio::sync::Notify,
    running: AtomicBool,
}

fn workers() -> &'static DashMap<usize, Arc<PreviewWorker>> {
    static WORKERS: OnceLock<DashMap<usize, Arc<PreviewWorker>>> = OnceLock::new();
    WORKERS.get_or_init(DashMap::new)
}

struct WorkerLifetime(Arc<PreviewWorker>);
impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

fn worker_for(state: &Arc<crate::dispatch::AppState>) -> Result<Arc<PreviewWorker>> {
    crate::services::ingest_jobs::pool()?;
    let identity = Arc::as_ptr(&state.db) as usize;
    let worker = match workers().entry(identity) {
        dashmap::mapref::entry::Entry::Occupied(mut entry) => {
            if entry
                .get()
                .database
                .upgrade()
                .is_some_and(|db| Arc::ptr_eq(&db, &state.db))
            {
                let worker = entry.get().clone();
                let mut current = worker
                    .state
                    .write()
                    .map_err(|_| anyhow!("media state lock poisoned"))?;
                if current.upgrade().is_none() {
                    *current = Arc::downgrade(state);
                }
                drop(current);
                worker
            } else {
                let worker = make_worker(state)?;
                entry.insert(worker.clone());
                worker
            }
        }
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            let worker = make_worker(state)?;
            entry.insert(worker.clone());
            worker
        }
    };
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|e| anyhow!("video worker runtime unavailable: {e}"))?;
    if !worker.running.swap(true, Ordering::SeqCst) {
        if let Err(error) = restore_queue(&worker.namespace) {
            worker.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
        let worker = worker.clone();
        runtime.spawn(async move {
            let _lifetime = WorkerLifetime(worker.clone());
            loop {
                if worker.database.upgrade().is_none() { break; }
                let state = worker.state.read().ok().and_then(|state| state.upgrade());
                let Some(state) = state else {
                    tokio::select! { _ = worker.wake.notified() => {}, _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {} }
                    continue;
                };
                let queue = match crate::services::ingest_jobs::pool() {
                    Ok(queue) => queue,
                    Err(error) => { tracing::error!(%error, "media queue unavailable"); tokio::time::sleep(std::time::Duration::from_secs(2)).await; continue; }
                };
                match crate::services::ingest_jobs::claim(&queue, &worker.namespace) {
                    Ok(Some(job)) => {
                        static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
                        let permit = GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))).clone().acquire_owned().await;
                        if let Ok(_permit) = permit {
                            let queue_for_work = queue.clone();
                            let work = job.clone();
                            let result = tokio::task::spawn_blocking(move || process_preview_job(&state, &queue_for_work, &work)).await;
                            match result {
                                Ok(Ok(())) => {},
                                Ok(Err(error)) => tracing::warn!(%error, job_id=%job.job_id, "video preview job failed"),
                                Err(error) => { tracing::error!(%error, job_id=%job.job_id, "video preview worker failed"); let _ = record_preview_failure(&job, &format!("conversion worker failed: {error}")); },
                            }
                        } else { let _ = record_preview_failure(&job, "conversion gate closed"); }
                        if let Err(error) = crate::services::ingest_jobs::finish(&queue, &job) { tracing::error!(%error, "video preview queue completion failed"); }
                    }
                    Ok(None) => {
                        drop(state);
                        tokio::select! { _ = worker.wake.notified() => {}, _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {} }
                    }
                    Err(error) => { tracing::error!(%error, "video preview queue claim failed"); tokio::time::sleep(std::time::Duration::from_secs(2)).await; }
                }
            }
        });
    }
    worker.wake.notify_one();
    Ok(worker)
}

fn preview_lock(path: &Path) -> &'static std::sync::Mutex<()> {
    static LOCKS: OnceLock<[std::sync::Mutex<()>; 64]> = OnceLock::new();
    let index = path
        .to_string_lossy()
        .bytes()
        .fold(0usize, |n, b| n.wrapping_mul(31).wrapping_add(b as usize))
        % 64;
    &LOCKS.get_or_init(|| std::array::from_fn(|_| std::sync::Mutex::new(())))[index]
}

pub fn with_publication_gate<T>(action: impl FnOnce() -> Result<T>) -> Result<T> {
    let queue = crate::services::ingest_jobs::pool()?;
    crate::services::ingest_jobs::with_transaction(&queue, |_| action())
}

fn namespace(db: &crate::db::DbPool) -> Result<String> {
    let path: String = db.read()?.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    let identity = if path.is_empty() {
        format!("memory-{:p}", Arc::as_ptr(db))
    } else {
        path
    };
    Ok(format!(
        "project_media:{}",
        hex::encode(Sha256::digest(identity.as_bytes()))
    ))
}

fn make_worker(state: &Arc<crate::dispatch::AppState>) -> Result<Arc<PreviewWorker>> {
    Ok(Arc::new(PreviewWorker {
        database: Arc::downgrade(&state.db),
        state: std::sync::RwLock::new(Arc::downgrade(state)),
        namespace: namespace(&state.db)?,
        wake: tokio::sync::Notify::new(),
        running: AtomicBool::new(false),
    }))
}

pub fn start_workers(state: Arc<crate::dispatch::AppState>) -> Result<()> {
    recover_transfer_intents(&state)?;
    worker_for(&state)?;
    Ok(())
}

fn restore_queue(namespace: &str) -> Result<()> {
    let queue = crate::services::ingest_jobs::pool()?;
    let mut failures = Vec::new();
    for job in crate::services::ingest_jobs::reconcile_orphans(&queue, namespace)? {
        let outcome = if job.cancel_requested {
            record_preview_failure(&job, "conversion cancelled before restart")
        } else {
            crate::services::ingest_jobs::enqueue(&queue, namespace, &job.job_id, &job.payload_json)
        };
        if let Err(error) = outcome {
            let message = format!("conversion restart recovery failed: {error}");
            if let Err(record_error) = record_preview_failure(&job, &message) {
                tracing::error!(%record_error, job_id=%job.job_id, "media restart failure could not be persisted");
            }
            failures.push(message);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("; ")))
    }
}

fn record_preview_failure(
    job: &crate::services::ingest_jobs::QueuedJob,
    error: &str,
) -> Result<()> {
    let payload: PreviewJob = serde_json::from_str(&job.payload_json)?;
    let Some(project) = super::repository::get_project(&payload.org_id, &payload.project_id)?
    else {
        return Ok(());
    };
    let state = PreviewState {
        status: "error".into(),
        error: error.into(),
        total_size: 0,
        duration_ms: 0,
    };
    let dir = Path::new(&project.dir_path);
    let meta = previews_dir(dir)?.join(format!("{}.json", payload.sha256));
    let queue = crate::services::ingest_jobs::pool()?;
    crate::services::ingest_jobs::with_transaction(&queue, |conn| {
        if crate::services::ingest_jobs::job_in(conn, &job.job_id)?
            .is_some_and(|current| current.claim_token != job.claim_token)
            || crate::services::ingest_jobs::suspension_in(conn, &job.job_id)?
        {
            return Ok(());
        }
        let _guard = preview_lock(&meta)
            .lock()
            .map_err(|_| anyhow!("preview lock poisoned"))?;
        if preview_path(dir, &payload.sha256).is_ok() {
            return Ok(());
        }
        write_metadata(&meta, &state)
    })
}

fn require_preview_reference(
    state: &Arc<crate::dispatch::AppState>,
    pool: &crate::db::DbPool,
    payload: &PreviewJob,
) -> Result<()> {
    let actor = crate::db::repository::get_user_account_by_id(&state.db, &payload.user_id)?
        .ok_or_else(|| anyhow!("media actor account is unavailable"))?;
    if !actor.is_active {
        return Err(anyhow!("media actor account is disabled"));
    }
    let org = crate::services::rbac::resolve_org_context(
        &state.db,
        &payload.user_id,
        Some(&payload.org_id),
    )
    .map_err(|e| anyhow!(e))?;
    let ctx = HandlerContext {
        session: tentaflow_protocol::SessionAuth::Anonymous,
        correlation_id: 0,
        connection_id: 0,
        resume_secret: None,
        state: state.clone(),
        org_context: Some(org),
        origin: crate::dispatch::RequestOrigin::Local,
    };
    crate::dispatch::project_studio::require_attachment(
        &ctx,
        pool,
        &payload.project_id,
        &payload.owner,
        &payload.sha256,
    )
    .map_err(|error| anyhow!(error.message))?;
    Ok(())
}

fn validate_preview_access(
    state: &Arc<crate::dispatch::AppState>,
    pool: &crate::db::DbPool,
    payload: &PreviewJob,
) -> Result<()> {
    require_preview_reference(state, pool, payload)?;
    super::repository::project_write_admission(&payload.project_id)?;
    if payload.owner.kind == tentaflow_protocol::project_studio::AttachmentOwnerKind::Task {
        let published = super::repository::pending_task_transfers()?
            .iter()
            .any(|operation| {
                operation.phase == "published"
                    && operation.destination_project_id == payload.project_id
                    && operation.task_ids.contains(&payload.owner.id)
            });
        if published
            && super::repository::task_location(&payload.owner.id)?
                .is_some_and(|location| location.project_id == payload.project_id)
        {
            // Content is already canonical here; the remaining fence protects
            // source cleanup, not conversion of the destination's immutable blob.
            return Ok(());
        }
        super::repository::task_write_admission(&payload.project_id, &payload.owner.id)?;
    }
    Ok(())
}

fn replacement_preview_job(
    state: &Arc<crate::dispatch::AppState>,
    pool: &crate::db::DbPool,
    payload: &PreviewJob,
    actor: &str,
    excluded: &std::collections::HashSet<String>,
) -> Result<Option<PreviewJob>> {
    let owners = super::attachments::attachment_owners(pool, &payload.sha256, excluded)?;
    Ok(owners.into_iter().find_map(|owner| {
        [actor, payload.user_id.as_str()]
            .into_iter()
            .find_map(|user| {
                let candidate = PreviewJob {
                    org_id: payload.org_id.clone(),
                    project_id: payload.project_id.clone(),
                    sha256: payload.sha256.clone(),
                    user_id: user.into(),
                    owner: owner.clone(),
                };
                validate_preview_access(state, pool, &candidate)
                    .ok()
                    .map(|_| candidate)
            })
    }))
}

fn current_preview_job(
    state: &Arc<crate::dispatch::AppState>,
    pool: &crate::db::DbPool,
    conn: &rusqlite::Connection,
    job: &crate::services::ingest_jobs::QueuedJob,
    original: &PreviewJob,
) -> Result<Option<PreviewJob>> {
    if crate::services::ingest_jobs::heartbeat_in(conn, job)?
        != crate::services::ingest_jobs::JobLiveness::Running
    {
        return Err(anyhow!("video conversion was cancelled"));
    }
    let current = crate::services::ingest_jobs::job_in(conn, &job.job_id)?
        .ok_or_else(|| anyhow!("video conversion queue row is unavailable"))?;
    let payload: PreviewJob = serde_json::from_str(&current.payload_json)?;
    if payload.org_id != original.org_id
        || payload.project_id != original.project_id
        || payload.sha256 != original.sha256
    {
        return Err(anyhow!("video conversion source identity changed"));
    }
    require_preview_reference(state, pool, &payload)?;
    if payload.owner.kind == tentaflow_protocol::project_studio::AttachmentOwnerKind::Task {
        if let Some(operation) = super::repository::pending_task_transfers()?
            .into_iter()
            .find(|operation| {
                operation.source_project_id == payload.project_id
                    && matches!(operation.phase.as_str(), "prepared" | "copied")
                    && operation.task_ids.contains(&payload.owner.id)
            })
        {
            let excluded = operation.task_ids.iter().cloned().collect();
            if let Some(replacement) =
                replacement_preview_job(state, pool, &payload, &operation.actor_user_id, &excluded)?
            {
                if !crate::services::ingest_jobs::replace_payload(
                    conn,
                    &job.job_id,
                    &current.payload_json,
                    &serde_json::to_string(&replacement)?,
                )? {
                    return Err(anyhow!(
                        "shared media queue ownership changed during transfer"
                    ));
                }
                return Ok(Some(replacement));
            }
            let attachment = super::tasks::task_attachment_metadata(
                pool,
                &payload.owner.id,
                &payload.sha256,
            )?
            .ok_or_else(|| anyhow!("media transfer source attachment reference disappeared"))?;
            let intent = MediaTransferIntent {
                snapshot: TransferPreview {
                    sha256: payload.sha256.clone(),
                    original_size: attachment.size_bytes,
                    owner_task_id: payload.owner.id.clone(),
                    ready: None,
                    requeue: true,
                },
                original_payload: Some(current.payload_json),
                suspended: true,
                aborting: false,
                actor_user_id: operation.actor_user_id,
            };
            conn.execute("INSERT INTO project_media_transfers(queue,operation_id,project_id,sha256,intent_json) \
                VALUES (?1,?2,?3,?4,?5) ON CONFLICT(queue,operation_id,project_id,sha256) DO NOTHING",
                rusqlite::params![current.queue,operation.operation_id,payload.project_id,payload.sha256,serde_json::to_string(&intent)?])?;
            crate::services::ingest_jobs::suspend_in(conn, &job.job_id, &operation.operation_id)?;
            return Ok(None);
        }
    }
    validate_preview_access(state, pool, &payload)?;
    Ok(Some(payload))
}

fn process_preview_job(
    state: &Arc<crate::dispatch::AppState>,
    queue: &crate::db::DbPool,
    job: &crate::services::ingest_jobs::QueuedJob,
) -> Result<()> {
    let payload: PreviewJob = serde_json::from_str(&job.payload_json)?;
    if !is_sha256(&payload.sha256) {
        return Err(anyhow!("invalid queued media SHA-256"));
    }
    let project = super::repository::get_project(&payload.org_id, &payload.project_id)?
        .ok_or_else(|| anyhow!("media project no longer exists"))?;
    let pool = super::project_db::open(&payload.project_id)?;
    let dir = Path::new(&project.dir_path);
    let meta = previews_dir(dir)?.join(format!("{}.json", payload.sha256));
    let output = previews_dir(dir)?.join(format!("{}.mp4", payload.sha256));
    let allowed = || {
        crate::services::ingest_jobs::with_transaction(queue, |conn| {
            current_preview_job(state, &pool, conn, job, &payload)
        })?
        .ok_or_else(|| anyhow!("video conversion suspended for task transfer"))
        .map(|_| ())
    };
    let result = (|| -> Result<()> {
        let ready = crate::services::ingest_jobs::with_transaction(queue, |conn| {
            if current_preview_job(state, &pool, conn, job, &payload)?.is_none() {
                return Ok(None);
            }
            let _guard = preview_lock(&meta)
                .lock()
                .map_err(|_| anyhow!("preview lock poisoned"))?;
            if read_preview_state(dir, &payload.sha256)?
                .is_some_and(|state| state.status == "ready")
                && preview_path(dir, &payload.sha256).is_ok()
            {
                return Ok(Some(true));
            }
            write_metadata(
                &meta,
                &PreviewState {
                    status: "processing".into(),
                    error: String::new(),
                    total_size: 0,
                    duration_ms: 0,
                },
            )?;
            Ok(Some(false))
        })?
        .ok_or_else(|| anyhow!("video conversion suspended for task transfer"))?;
        if ready {
            return Ok(());
        }
        transcode(
            &dir.join("files").join(&payload.sha256),
            &output,
            &allowed,
            &|temp, size, duration| {
                crate::services::ingest_jobs::with_transaction(queue, |conn| {
                    if current_preview_job(state, &pool, conn, job, &payload)?.is_none() {
                        return Ok(false);
                    }
                    let _guard = preview_lock(&meta)
                        .lock()
                        .map_err(|_| anyhow!("preview lock poisoned"))?;
                    std::fs::rename(temp, &output)?;
                    write_metadata(
                        &meta,
                        &PreviewState {
                            status: "ready".into(),
                            error: String::new(),
                            total_size: size,
                            duration_ms: duration,
                        },
                    )?;
                    Ok(true)
                })?
                .then_some(())
                .ok_or_else(|| anyhow!("video publication suspended for task transfer"))
            },
        )?;
        Ok(())
    })();
    if let Err(error) = &result {
        crate::services::ingest_jobs::with_transaction(queue, |conn| {
            if !crate::services::ingest_jobs::job_in(conn, &job.job_id)?
                .is_some_and(|current| current.claim_token == job.claim_token)
                || crate::services::ingest_jobs::suspension_in(conn, &job.job_id)?
            {
                return Ok(());
            }
            let _guard = preview_lock(&meta)
                .lock()
                .map_err(|_| anyhow!("preview lock poisoned"))?;
            if read_preview_state(dir, &payload.sha256)?
                .is_some_and(|state| state.status == "ready")
                && preview_path(dir, &payload.sha256).is_ok()
            {
                return Ok(());
            }
            write_metadata(
                &meta,
                &PreviewState {
                    status: "error".into(),
                    error: error.to_string(),
                    total_size: 0,
                    duration_ms: 0,
                },
            )
        })?;
    }
    result
}

pub fn request_preview(
    ctx: &HandlerContext,
    project_id: &str,
    owner: &AttachmentOwner,
    sha: &str,
    retry: bool,
) -> Result<PreviewState> {
    let org = crate::dispatch::project_studio::require_read(ctx).map_err(|e| anyhow!(e.message))?;
    crate::dispatch::project_studio::require_project_access(ctx, org, project_id)
        .map_err(|e| anyhow!(e.message))?;
    let pool = super::project_db::open(project_id)?;
    let (project, attachment) =
        crate::dispatch::project_studio::require_attachment(ctx, &pool, project_id, owner, sha)
            .map_err(|e| anyhow!(e.message))?;
    if !attachment.mime.starts_with("video/") {
        return Err(anyhow!("attachment is not a video"));
    }
    let worker = worker_for(&ctx.state)?;
    let queue = crate::services::ingest_jobs::pool()?;
    let job_id = format!("{}:{project_id}:{sha}", worker.namespace);
    let dir = Path::new(&project.dir_path);
    let meta = previews_dir(dir)?.join(format!("{sha}.json"));
    let payload = PreviewJob {
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
        project_id: project_id.into(),
        owner: owner.clone(),
        sha256: sha.into(),
    };
    let outcome = crate::services::ingest_jobs::with_transaction(&queue, |conn| {
        crate::dispatch::project_studio::require_attachment(ctx, &pool, project_id, owner, sha)
            .map_err(|e| anyhow!(e.message))?;
        let _guard = preview_lock(&meta)
            .lock()
            .map_err(|_| anyhow!("preview lock poisoned"))?;
        if let Some(state) = read_preview_state(dir, sha)? {
            if state.status == "ready" && preview_path(dir, sha).is_ok() {
                return Ok(state);
            }
            if crate::services::ingest_jobs::job_in(conn, &job_id)?.is_some() {
                return Ok(state);
            }
            if !retry {
                if state.status == "error" {
                    return Ok(state);
                }
                let interrupted = PreviewState {
                    status: "error".into(), error: "conversion was interrupted or its cached file is unavailable; retry to resume".into(),
                    total_size: 0, duration_ms: 0,
                };
                write_metadata(&meta, &interrupted)?;
                return Ok(interrupted);
            }
        }
        validate_preview_access(&ctx.state, &pool, &payload)?;
        let queued = PreviewState {
            status: "queued".into(),
            error: String::new(),
            total_size: 0,
            duration_ms: 0,
        };
        crate::services::ingest_jobs::enqueue_in(
            &conn,
            &worker.namespace,
            &job_id,
            &serde_json::to_string(&payload)?,
        )?;
        write_metadata(&meta, &queued)?;
        Ok(queued)
    })?;
    worker.wake.notify_one();
    Ok(outcome)
}

pub fn cancel_project(core_db: &crate::db::DbPool, project_id: &str) -> Result<()> {
    let namespace = namespace(core_db)?;
    let jobs = crate::services::ingest_jobs::pool()?;
    crate::services::ingest_jobs::with_transaction(&jobs, |conn| {
        let ids = {
            let mut query =
                conn.prepare("SELECT job_id,payload_json FROM ingest_jobs WHERE queue=?1")?;
            let rows = query.query_map([&namespace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, json) in ids {
            let payload: PreviewJob = serde_json::from_str(&json)?;
            if payload.project_id == project_id {
                crate::services::ingest_jobs::request_cancel_in(conn, &id)?;
            }
        }
        Ok(())
    })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TransferPreview {
    pub sha256: String,
    pub original_size: u64,
    pub owner_task_id: String,
    pub ready: Option<ReadyPreview>,
    pub requeue: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ReadyPreview {
    pub sha256: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct MediaTransferIntent {
    snapshot: TransferPreview,
    original_payload: Option<String>,
    suspended: bool,
    aborting: bool,
    actor_user_id: String,
}

fn transfer_intent(
    conn: &rusqlite::Connection,
    queue: &str,
    project: &str,
    operation: &str,
    sha: &str,
) -> Result<Option<MediaTransferIntent>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT intent_json FROM project_media_transfers \
         WHERE queue=?1 AND operation_id=?2 AND project_id=?3 AND sha256=?4",
            rusqlite::params![queue, operation, project, sha],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

fn operation_intents(
    conn: &rusqlite::Connection,
    queue: &str,
    project: &str,
    operation: &str,
) -> Result<Vec<MediaTransferIntent>> {
    let mut query = conn.prepare(
        "SELECT intent_json FROM project_media_transfers \
        WHERE queue=?1 AND operation_id=?2 AND project_id=?3 ORDER BY sha256",
    )?;
    let rows = query.query_map(rusqlite::params![queue, operation, project], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| serde_json::from_str(&row?).map_err(Into::into))
        .collect()
}

#[derive(PartialEq, Eq)]
struct FileIdentity {
    size: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn file_identity(path: &Path) -> Result<FileIdentity> {
    let meta = open_regular(path)?.metadata()?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity {
        size: meta.len(),
        modified: meta.modified()?,
        #[cfg(unix)]
        device: meta.dev(),
        #[cfg(unix)]
        inode: meta.ino(),
    })
}

pub fn prepare_transfer(
    state: &Arc<crate::dispatch::AppState>,
    source: &super::models::ProjectRecord,
    source_pool: &crate::db::DbPool,
    operation_id: &str,
    actor: &str,
    attachments: &[(String, tentaflow_protocol::project_studio::AttachmentWire)],
) -> Result<Vec<TransferPreview>> {
    use crate::services::ingest_jobs as queue;
    uuid::Uuid::parse_str(operation_id)?;
    let queue_namespace = namespace(&state.db)?;
    let jobs = queue::pool()?;
    let excluded = attachments
        .iter()
        .map(|(task, _)| task.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut unique = std::collections::BTreeMap::new();
    for (task, attachment) in attachments {
        if !is_sha256(&attachment.sha256) {
            return Err(anyhow!("invalid transfer attachment SHA-256"));
        }
        unique
            .entry(attachment.sha256.clone())
            .or_insert((task, attachment));
    }
    let dir = Path::new(&source.dir_path);
    let mut snapshots = Vec::new();
    for (sha, (task, attachment)) in unique {
        let original = dir.join("files").join(&sha);
        let original_identity = file_identity(&original)?;
        if original_identity.size != attachment.size_bytes
            || hash_file(&original, &|| Ok(()))? != sha
        {
            return Err(anyhow!(
                "transfer attachment content differs from its reference"
            ));
        }
        let before = read_preview_state(dir, &sha)?;
        let ready = match &before {
            Some(meta) if meta.status == "ready" => {
                let path = preview_path(dir, &sha)?;
                let identity = file_identity(&path)?;
                let hash = hash_file(&path, &|| Ok(()))?;
                Some((
                    ReadyPreview {
                        sha256: hash,
                        size_bytes: meta.total_size,
                        duration_ms: meta.duration_ms,
                    },
                    identity,
                ))
            }
            _ => None,
        };
        let meta_path = previews_dir(dir)?.join(format!("{sha}.json"));
        let snapshot = queue::with_transaction(&jobs, |conn| {
            let _guard = preview_lock(&meta_path)
                .lock()
                .map_err(|_| anyhow!("preview lock poisoned"))?;
            if file_identity(&original)? != original_identity {
                return Err(anyhow!("attachment changed during transfer preparation"));
            }
            if let Some(intent) = transfer_intent(
                conn,
                &queue_namespace,
                &source.project_id,
                operation_id,
                &sha,
            )? {
                if intent.aborting {
                    return Err(anyhow!("media preparation is being rolled back"));
                }
                if intent.snapshot.original_size != original_identity.size
                    || !excluded.contains(&intent.snapshot.owner_task_id)
                {
                    return Err(anyhow!("media preparation differs from its durable intent"));
                }
                return Ok(intent.snapshot);
            }
            let current = read_preview_state(dir, &sha)?;
            if serde_json::to_string(&current)? != serde_json::to_string(&before)? {
                return Err(anyhow!(
                    "preview changed during transfer preparation; retry the operation"
                ));
            }
            if let Some((_, identity)) = &ready {
                if file_identity(&preview_path(dir, &sha)?)? != *identity {
                    return Err(anyhow!(
                        "completed preview changed during transfer preparation"
                    ));
                }
            }
            let job_id = format!("{}:{}:{sha}", queue_namespace, source.project_id);
            let pending = queue::job_in(conn, &job_id)?;
            let mut suspended = false;
            if let Some(job) = &pending {
                let payload: PreviewJob = serde_json::from_str(&job.payload_json)?;
                if payload.project_id != source.project_id
                    || payload.org_id != source.org_id
                    || payload.sha256 != sha
                {
                    return Err(anyhow!("source media queue identity is invalid"));
                }
                if payload.owner.kind
                    == tentaflow_protocol::project_studio::AttachmentOwnerKind::Task
                    && excluded.contains(&payload.owner.id)
                {
                    let replacement =
                        replacement_preview_job(state, source_pool, &payload, actor, &excluded)?;
                    if let Some(replacement) = replacement {
                        if !queue::replace_payload(
                            conn,
                            &job_id,
                            &job.payload_json,
                            &serde_json::to_string(&replacement)?,
                        )? {
                            return Err(anyhow!("preview queue ownership changed during transfer"));
                        }
                    } else {
                        queue::suspend_in(conn, &job_id, operation_id)?;
                        suspended = true;
                    }
                }
            }
            let snapshot = TransferPreview {
                sha256: sha.clone(),
                original_size: original_identity.size,
                owner_task_id: (*task).clone(),
                ready: ready.as_ref().map(|(ready, _)| ready.clone()),
                requeue: ready.is_none()
                    && (pending.is_some()
                        || current
                            .as_ref()
                            .is_some_and(|s| s.status == "queued" || s.status == "processing")),
            };
            let intent = MediaTransferIntent {
                snapshot: snapshot.clone(),
                original_payload: pending.map(|job| job.payload_json),
                suspended,
                aborting: false,
                actor_user_id: actor.into(),
            };
            conn.execute("INSERT INTO project_media_transfers(queue,operation_id,project_id,sha256,intent_json) \
                VALUES (?1,?2,?3,?4,?5)",rusqlite::params![queue_namespace,operation_id,source.project_id,sha,serde_json::to_string(&intent)?])?;
            Ok(snapshot)
        })?;
        snapshots.push(snapshot);
    }
    if let Some(worker) = workers().get(&(Arc::as_ptr(&state.db) as usize)) {
        worker.wake.notify_one();
    }
    Ok(snapshots)
}

pub fn abort_transfer(
    state: &Arc<crate::dispatch::AppState>,
    source: &super::models::ProjectRecord,
    source_pool: &crate::db::DbPool,
    operation_id: &str,
) -> Result<()> {
    use crate::services::ingest_jobs as queue;
    let queue_namespace = namespace(&state.db)?;
    let jobs = queue::pool()?;
    queue::with_transaction(&jobs, |conn| {
        for mut intent in
            operation_intents(conn, &queue_namespace, &source.project_id, operation_id)?
        {
            if !super::repository::task_location(&intent.snapshot.owner_task_id)?
                .is_some_and(|location| location.project_id == source.project_id)
            {
                return Err(anyhow!("published media transfer cannot be rolled back"));
            }
            if intent.suspended {
                if !super::tasks::attachment_referenced(
                    source_pool,
                    &intent.snapshot.owner_task_id,
                    &intent.snapshot.sha256,
                )? {
                    return Err(anyhow!(
                        "source media reference is unavailable during rollback"
                    ));
                }
                let payload = intent.original_payload.as_deref().ok_or_else(|| {
                    anyhow!("suspended media intent has no original queue payload")
                })?;
                let id = format!(
                    "{}:{}:{}",
                    queue_namespace, source.project_id, intent.snapshot.sha256
                );
                if let Some(current) = queue::job_in(conn, &id)? {
                    if current.payload_json != payload {
                        return Err(anyhow!("suspended source media payload changed"));
                    }
                } else {
                    queue::enqueue_in(conn, &queue_namespace, &id, payload)?;
                    queue::suspend_in(conn, &id, operation_id)?;
                }
                let dir = Path::new(&source.dir_path);
                let meta = previews_dir(dir)?.join(format!("{}.json", intent.snapshot.sha256));
                let _guard = preview_lock(&meta)
                    .lock()
                    .map_err(|_| anyhow!("preview lock poisoned"))?;
                if !read_preview_state(dir, &intent.snapshot.sha256)?
                    .is_some_and(|state| state.status == "ready")
                {
                    write_metadata(
                        &meta,
                        &PreviewState {
                            status: "queued".into(),
                            error: String::new(),
                            total_size: 0,
                            duration_ms: 0,
                        },
                    )?;
                }
            }
            intent.aborting = true;
            conn.execute(
                "UPDATE project_media_transfers SET intent_json=?5 \
                WHERE queue=?1 AND operation_id=?2 AND project_id=?3 AND sha256=?4",
                rusqlite::params![
                    queue_namespace,
                    operation_id,
                    source.project_id,
                    intent.snapshot.sha256,
                    serde_json::to_string(&intent)?
                ],
            )?;
        }
        Ok(())
    })
}

pub fn finish_transfer(
    state: &Arc<crate::dispatch::AppState>,
    source: &super::models::ProjectRecord,
    operation_id: &str,
) -> Result<()> {
    use crate::services::ingest_jobs as queue;
    let queue_namespace = namespace(&state.db)?;
    let jobs = queue::pool()?;
    let source_pool = super::project_db::open(&source.project_id)?;
    queue::with_transaction(&jobs, |conn| {
        for intent in operation_intents(conn, &queue_namespace, &source.project_id, operation_id)? {
            let location = super::repository::task_location(&intent.snapshot.owner_task_id)?
                .ok_or_else(|| anyhow!("media transfer canonical task location is unavailable"))?;
            let id = format!(
                "{}:{}:{}",
                queue_namespace, source.project_id, intent.snapshot.sha256
            );
            if intent.suspended {
                if location.project_id == source.project_id {
                    if !intent.aborting
                        || super::repository::pending_task_transfers()?
                            .iter()
                            .any(|operation| operation.operation_id == operation_id)
                    {
                        return Err(anyhow!(
                            "source media cannot resume before task rollback completes"
                        ));
                    }
                    queue::resume_in(conn, &id, operation_id)?;
                } else {
                    let payload: PreviewJob = serde_json::from_str(
                        intent
                            .original_payload
                            .as_deref()
                            .ok_or_else(|| anyhow!("media transfer lost original payload"))?,
                    )?;
                    let replacement = replacement_preview_job(
                        state,
                        &source_pool,
                        &payload,
                        &intent.actor_user_id,
                        &std::collections::HashSet::new(),
                    )?;
                    if let Some(replacement) = replacement {
                        let changed = conn.execute(
                            "UPDATE ingest_jobs SET payload_json=?3 \
                            WHERE job_id=?1 AND suspended_by=?2",
                            rusqlite::params![
                                id,
                                operation_id,
                                serde_json::to_string(&replacement)?
                            ],
                        )?;
                        if changed != 1 {
                            return Err(anyhow!("suspended shared media queue is unavailable"));
                        }
                        queue::resume_in(conn, &id, operation_id)?;
                    } else {
                        conn.execute(
                            "DELETE FROM ingest_jobs WHERE job_id=?1 AND suspended_by=?2",
                            rusqlite::params![id, operation_id],
                        )?;
                        let dir = Path::new(&source.dir_path);
                        let meta =
                            previews_dir(dir)?.join(format!("{}.json", intent.snapshot.sha256));
                        let _guard = preview_lock(&meta)
                            .lock()
                            .map_err(|_| anyhow!("preview lock poisoned"))?;
                        if !read_preview_state(dir, &intent.snapshot.sha256)?
                            .is_some_and(|state| state.status == "ready")
                        {
                            write_metadata(&meta,&PreviewState {status:"error".into(),
                                error:"task moved; any remaining source attachment owner may retry".into(),total_size:0,duration_ms:0})?;
                        }
                    }
                }
            }
        }
        conn.execute("DELETE FROM project_media_transfers WHERE queue=?1 AND operation_id=?2 AND project_id=?3",
            rusqlite::params![queue_namespace,operation_id,source.project_id])?;
        Ok(())
    })?;
    if let Some(worker) = workers().get(&(Arc::as_ptr(&state.db) as usize)) {
        worker.wake.notify_one();
    }
    Ok(())
}

fn recover_transfer_intents(state: &Arc<crate::dispatch::AppState>) -> Result<()> {
    let queue_namespace = namespace(&state.db)?;
    let jobs = crate::services::ingest_jobs::pool()?;
    let operations = {
        let conn = jobs
            .read()
            .map_err(|error| anyhow!("media transfer intents: {error}"))?;
        let mut query = conn.prepare(
            "SELECT DISTINCT operation_id,project_id FROM project_media_transfers WHERE queue=?1",
        )?;
        let rows = query.query_map([&queue_namespace], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let pending = super::repository::pending_task_transfers()?;
    for (operation_id, project_id) in operations {
        if pending
            .iter()
            .any(|operation| operation.operation_id == operation_id)
        {
            return Err(anyhow!(
                "task transfer must recover before media workers start"
            ));
        }
        let source = super::repository::project_record(&project_id)?
            .ok_or_else(|| anyhow!("media transfer source project is unavailable"))?;
        let pool = super::project_db::open(&project_id)?;
        let intents = {
            let conn = jobs
                .read()
                .map_err(|error| anyhow!("media intent read: {error}"))?;
            operation_intents(&conn, &queue_namespace, &project_id, &operation_id)?
        };
        let rolled_back = intents.iter().try_fold(true, |all, intent| {
            Ok::<_, anyhow::Error>(
                all && super::repository::task_location(&intent.snapshot.owner_task_id)?
                    .is_some_and(|location| location.project_id == project_id),
            )
        })?;
        if rolled_back {
            abort_transfer(state, &source, &pool, &operation_id)?;
        }
        finish_transfer(state, &source, &operation_id)?;
    }
    Ok(())
}

pub fn resume_transfer(
    state: &Arc<crate::dispatch::AppState>,
    destination: &super::models::ProjectRecord,
    destination_pool: &crate::db::DbPool,
    actor: &str,
    snapshots: &[TransferPreview],
) -> Result<()> {
    use crate::services::ingest_jobs as queue;
    let queue_namespace = namespace(&state.db)?;
    let jobs = queue::pool()?;
    let dir = Path::new(&destination.dir_path);
    for snapshot in snapshots {
        if !is_sha256(&snapshot.sha256) {
            return Err(anyhow!("invalid journal attachment SHA-256"));
        }
        let original = dir.join("files").join(&snapshot.sha256);
        let identity = file_identity(&original)?;
        if identity.size != snapshot.original_size
            || hash_file(&original, &|| Ok(()))? != snapshot.sha256
        {
            return Err(anyhow!(
                "destination attachment differs from transfer manifest"
            ));
        }
        let ready_identity = if let Some(ready) = &snapshot.ready {
            let path = previews_dir(dir)?.join(format!("{}.mp4", snapshot.sha256));
            let identity = file_identity(&path)?;
            if identity.size != ready.size_bytes || hash_file(&path, &|| Ok(()))? != ready.sha256 {
                return Err(anyhow!(
                    "destination preview differs from transfer manifest"
                ));
            }
            Some(identity)
        } else {
            None
        };
        let payload = PreviewJob {
            org_id: destination.org_id.clone(),
            project_id: destination.project_id.clone(),
            user_id: actor.into(),
            owner: AttachmentOwner {
                kind: tentaflow_protocol::project_studio::AttachmentOwnerKind::Task,
                id: snapshot.owner_task_id.clone(),
                step_index: None,
            },
            sha256: snapshot.sha256.clone(),
        };
        let meta = previews_dir(dir)?.join(format!("{}.json", snapshot.sha256));
        loop {
            let completed = match read_preview_state(dir, &snapshot.sha256)? {
                Some(current) if current.status == "ready" => {
                    let path = preview_path(dir, &snapshot.sha256)?;
                    let before = file_identity(&path)?;
                    let hash = hash_file(&path, &|| Ok(()))?;
                    if file_identity(&path)? != before {
                        return Err(anyhow!(
                            "completed destination preview changed while hashing"
                        ));
                    }
                    if snapshot.ready.as_ref().is_some_and(|ready| {
                        ready.sha256 != hash
                            || ready.size_bytes != before.size
                            || ready.duration_ms != current.duration_ms
                    }) {
                        return Err(anyhow!("completed preview differs from transfer manifest"));
                    }
                    Some((current, before))
                }
                _ => None,
            };
            let finished = queue::with_transaction(&jobs, |conn| {
                let _guard = preview_lock(&meta)
                    .lock()
                    .map_err(|_| anyhow!("preview lock poisoned"))?;
                if file_identity(&original)? != identity {
                    return Err(anyhow!("destination attachment identity changed"));
                }
                if let Some(current) = read_preview_state(dir, &snapshot.sha256)? {
                    if current.status == "ready" {
                        let Some((checked, checked_identity)) = &completed else {
                            // Hash the newly published pair outside the queue writer.
                            return Ok(false);
                        };
                        if serde_json::to_string(&current)? != serde_json::to_string(checked)?
                            || file_identity(&preview_path(dir, &snapshot.sha256)?)?
                                != *checked_identity
                        {
                            return Err(anyhow!("completed destination preview identity changed"));
                        }
                        return Ok(true);
                    }
                }
                if let Err(error) = validate_preview_access(state, destination_pool, &payload) {
                    write_metadata(
                        &meta,
                        &PreviewState {
                            status: "error".into(),
                            error: format!(
                                "transferred preview requires an authorized retry: {error}"
                            ),
                            total_size: 0,
                            duration_ms: 0,
                        },
                    )?;
                    return Ok(true);
                }
                if let Some(ready) = &snapshot.ready {
                    let path = previews_dir(dir)?.join(format!("{}.mp4", snapshot.sha256));
                    if Some(file_identity(&path)?) != ready_identity {
                        return Err(anyhow!("destination preview identity changed"));
                    }
                    write_metadata(
                        &meta,
                        &PreviewState {
                            status: "ready".into(),
                            error: String::new(),
                            total_size: ready.size_bytes,
                            duration_ms: ready.duration_ms,
                        },
                    )?;
                } else if snapshot.requeue {
                    let job_id = format!(
                        "{}:{}:{}",
                        queue_namespace, destination.project_id, snapshot.sha256
                    );
                    if queue::job_in(conn, &job_id)?.is_none() {
                        queue::enqueue_in(
                            conn,
                            &queue_namespace,
                            &job_id,
                            &serde_json::to_string(&payload)?,
                        )?;
                        write_metadata(
                            &meta,
                            &PreviewState {
                                status: "queued".into(),
                                error: String::new(),
                                total_size: 0,
                                duration_ms: 0,
                            },
                        )?;
                    }
                }
                Ok(true)
            })?;
            if finished {
                break;
            }
        }
    }
    if let Some(worker) = workers().get(&(Arc::as_ptr(&state.db) as usize)) {
        worker.wake.notify_one();
    }
    Ok(())
}

#[cfg(any(feature = "camera", test))]
fn validate_video_container(path: &Path) -> Result<()> {
    let mut file = open_regular(path)?;
    let mut header = [0u8; 512];
    let size = file.read(&mut header)?;
    let bytes = &header[..size];
    let iso = bytes.len() >= 12
        && [
            b"ftyp".as_slice(),
            b"moov".as_slice(),
            b"mdat".as_slice(),
            b"wide".as_slice(),
        ]
        .contains(&&bytes[4..8]);
    let avi = bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"AVI ";
    let ebml = bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]);
    let transport =
        bytes.len() > 376 && bytes[0] == 0x47 && bytes[188] == 0x47 && bytes[376] == 0x47;
    if !iso
        && !avi
        && !ebml
        && !transport
        && !bytes.starts_with(b"OggS")
        && !bytes.starts_with(b"FLV")
        && !bytes.starts_with(&[0, 0, 1, 0xba])
    {
        return Err(anyhow!(
            "unsupported local video container; network playlists are not accepted"
        ));
    }
    Ok(())
}

#[cfg(feature = "camera")]
fn transcode(
    source: &Path,
    output: &Path,
    allowed: &dyn Fn() -> Result<()>,
    publish: &dyn Fn(&Path, u64, u64) -> Result<()>,
) -> Result<(u64, u64)> {
    use gst::prelude::*;
    use gstreamer as gst;
    allowed()?;
    validate_video_container(source)?;
    ensure_write_space(
        source
            .parent()
            .ok_or_else(|| anyhow!("source has no parent"))?,
        4 * 1024 * 1024,
    )?;
    crate::services::gstreamer_runtime::prepare_runtime_environment();
    gst::init()?;
    let temp = output.with_extension("mp4.part");
    let faststart_temp = output.with_extension("mp4.mux");
    for path in [&temp, &faststart_temp] {
        if std::fs::symlink_metadata(path).is_ok() {
            std::fs::remove_file(path)?;
        }
    }
    let pipeline = gst::Pipeline::with_name("project-attachment-preview");
    let source_element = gst::ElementFactory::make("filesrc")
        .property(
            "location",
            source
                .to_str()
                .ok_or_else(|| anyhow!("invalid media path"))?,
        )
        .build()?;
    let decoder = gst::ElementFactory::make("decodebin").build()?;
    let video_queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 2u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .build()?;
    let convert = gst::ElementFactory::make("videoconvert").build()?;
    let scale = gst::ElementFactory::make("videoscale")
        .property("add-borders", true)
        .build()?;
    let dimensions = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", 1280i32)
                .field("height", 720i32)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        )
        .build()?;
    let encoder = gst::ElementFactory::make("x264enc")
        .property_from_str("speed-preset", "veryfast")
        .property_from_str("tune", "zerolatency")
        .property("bitrate", 2500u32)
        .property("key-int-max", 60u32)
        .property("threads", 2u32)
        .build()?;
    let parser = gst::ElementFactory::make("h264parse").build()?;
    let mux = gst::ElementFactory::make("mp4mux")
        .name("preview-mux")
        .property("faststart", true)
        .property(
            "faststart-file",
            faststart_temp
                .to_str()
                .ok_or_else(|| anyhow!("invalid preview path"))?,
        )
        .build()?;
    let sink = gst::ElementFactory::make("filesink")
        .property(
            "location",
            temp.to_str()
                .ok_or_else(|| anyhow!("invalid preview path"))?,
        )
        .build()?;
    pipeline.add_many([
        &source_element,
        &decoder,
        &video_queue,
        &convert,
        &scale,
        &dimensions,
        &encoder,
        &parser,
        &mux,
        &sink,
    ])?;
    source_element.link(&decoder)?;
    gst::Element::link_many([
        &video_queue,
        &convert,
        &scale,
        &dimensions,
        &encoder,
        &parser,
        &mux,
        &sink,
    ])?;
    let audio_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let audio_failure = audio_error.clone();
    let pipeline_weak = pipeline.downgrade();
    let video_weak = video_queue.downgrade();
    decoder.connect_pad_added(move |_, pad| {
        let Some(caps) = pad.current_caps() else {
            return;
        };
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if structure.name().starts_with("video/") {
            if let Some(queue) = video_weak.upgrade() {
                if let Some(sink) = queue.static_pad("sink").filter(|p| !p.is_linked()) {
                    let _ = pad.link(&sink);
                }
            }
        } else if structure.name().starts_with("audio/") {
            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };
            let audio = (|| -> Result<()> {
                if pipeline.by_name("preview-audio").is_some() {
                    return Ok(());
                }
                let queue = gst::ElementFactory::make("queue")
                    .name("preview-audio")
                    .property("max-size-buffers", 8u32)
                    .property("max-size-bytes", 0u32)
                    .property("max-size-time", 0u64)
                    .build()?;
                let convert = gst::ElementFactory::make("audioconvert").build()?;
                let resample = gst::ElementFactory::make("audioresample").build()?;
                let encoder = gst::ElementFactory::make("avenc_aac").build()?;
                let parser = gst::ElementFactory::make("aacparse").build()?;
                let mux = pipeline
                    .by_name("preview-mux")
                    .ok_or_else(|| anyhow!("preview mux unavailable"))?;
                pipeline.add_many([&queue, &convert, &resample, &encoder, &parser])?;
                gst::Element::link_many([&queue, &convert, &resample, &encoder, &parser, &mux])?;
                pad.link(
                    &queue
                        .static_pad("sink")
                        .ok_or_else(|| anyhow!("audio sink unavailable"))?,
                )?;
                for element in [&queue, &convert, &resample, &encoder, &parser] {
                    element.sync_state_with_parent()?;
                }
                Ok(())
            })();
            if let Err(error) = audio {
                if let Ok(mut failure) = audio_failure.lock() {
                    *failure = Some(error.to_string());
                }
            }
        }
    });
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow!("video pipeline has no bus"))?;
    let result = (|| -> Result<(u64, u64)> {
        allowed()?;
        pipeline.set_state(gst::State::Playing)?;
        let started = std::time::Instant::now();
        let mut last_space_check = started;
        loop {
            allowed()?;
            if let Some(error) = audio_error
                .lock()
                .map_err(|_| anyhow!("audio status lock poisoned"))?
                .as_ref()
            {
                return Err(anyhow!("video audio conversion failed: {error}"));
            }
            if started.elapsed() > std::time::Duration::from_secs(6 * 3600) {
                return Err(anyhow!("video conversion exceeded six hours"));
            }
            if last_space_check.elapsed() >= std::time::Duration::from_secs(2) {
                ensure_write_space(
                    output
                        .parent()
                        .ok_or_else(|| anyhow!("output has no parent"))?,
                    4 * 1024 * 1024,
                )?;
                last_space_check = std::time::Instant::now();
            }
            if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(250)) {
                match message.view() {
                    gst::MessageView::Eos(_) => break,
                    gst::MessageView::Error(error) => {
                        return Err(anyhow!("GStreamer conversion failed: {}", error.error()))
                    }
                    _ => {}
                }
            }
        }
        allowed()?;
        let duration_ms = pipeline
            .query_duration::<gst::ClockTime>()
            .map(|time| time.mseconds())
            .ok_or_else(|| anyhow!("video duration unavailable"))?;
        pipeline.set_state(gst::State::Null)?;
        let file = open_regular(&temp)?;
        let size = file.metadata()?.len();
        if size == 0 || duration_ms == 0 {
            return Err(anyhow!("conversion produced no playable video"));
        }
        file.sync_all()?;
        allowed()?;
        publish(&temp, size, duration_ms)?;
        Ok((size, duration_ms))
    })();
    let _ = pipeline.set_state(gst::State::Null);
    let _ = std::fs::remove_file(faststart_temp);
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result.context("video preview")
}

#[cfg(not(feature = "camera"))]
fn transcode(
    _source: &Path,
    _output: &Path,
    _allowed: &dyn Fn() -> Result<()>,
    _publish: &dyn Fn(&Path, u64, u64) -> Result<()>,
) -> Result<(u64, u64)> {
    Err(anyhow!(
        "this build does not include GStreamer video conversion"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_reads_seek_without_whole_file_buffers_and_reject_symlinks() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("original");
        let mut file = File::create(&path).expect("file");
        for value in 0..10u8 {
            file.write_all(&vec![value; 1024 * 1024])
                .expect("write block");
        }
        file.sync_all().expect("sync");
        let size = 10 * 1024 * 1024;
        let (bytes, total, eof) = read_range(&path, 7 * 1024 * 1024 - 3, 8).expect("seek");
        assert_eq!(total, size);
        assert_eq!(bytes, [6, 6, 6, 7, 7, 7, 7, 7]);
        assert!(!eof);
        let (bytes, _, eof) = read_range(&path, size - 5, 1024).expect("last");
        assert_eq!(bytes, [9; 5]);
        assert!(eof);
        assert!(read_range(&path, size, 1).expect("EOF").0.is_empty());
        assert!(read_range(&path, size + 1, 1).is_err());
        assert!(read_range(&path, 0, 0).is_err());
        assert!(read_range(&path, 0, 4 * 1024 * 1024 + 1).is_err());
        #[cfg(unix)]
        {
            let linked = root.path().join("linked");
            std::os::unix::fs::symlink(&path, &linked).expect("symlink");
            assert!(read_range(&linked, 0, 1).is_err());
            assert!(write_metadata(
                &linked,
                &PreviewState {
                    status: "error".into(),
                    error: "test".into(),
                    total_size: 0,
                    duration_ms: 0
                }
            )
            .is_ok());
            assert_eq!(
                std::fs::metadata(&path).expect("original preserved").len(),
                size
            );
        }
    }

    #[test]
    fn network_playlists_and_unrecognized_containers_are_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        for (name, bytes) in [
            (
                "playlist",
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nhttps://example.test/video.ts\n"
                    .as_slice(),
            ),
            ("unknown", b"not a video".as_slice()),
        ] {
            let path = root.path().join(name);
            std::fs::write(&path, bytes).expect("fixture");
            assert!(validate_video_container(&path).is_err());
        }
    }

    #[cfg(feature = "camera")]
    fn finish_pipeline(pipeline: &gstreamer::Pipeline) {
        use gstreamer::prelude::*;
        pipeline
            .set_state(gstreamer::State::Playing)
            .expect("playing");
        let bus = pipeline.bus().expect("bus");
        let result = loop {
            let message = bus
                .timed_pop(gstreamer::ClockTime::from_seconds(30))
                .expect("pipeline completed within 30 seconds");
            match message.view() {
                gstreamer::MessageView::Eos(_) => break Ok(()),
                gstreamer::MessageView::Error(error) => break Err(error.error().to_string()),
                _ => {}
            }
        };
        pipeline.set_state(gstreamer::State::Null).expect("stop");
        result.expect("real pipeline completed");
    }

    #[cfg(feature = "camera")]
    struct TransferMediaFixture {
        state: Arc<crate::dispatch::AppState>,
        actor: String,
        source: super::super::models::ProjectRecord,
        destination: super::super::models::ProjectRecord,
        source_pool: crate::db::DbPool,
        destination_pool: crate::db::DbPool,
        queue: crate::db::DbPool,
        namespace: String,
        job_id: String,
        payload: PreviewJob,
    }

    #[cfg(feature = "camera")]
    fn transfer_media_fixture() -> TransferMediaFixture {
        use gstreamer::prelude::*;
        let root = tempfile::tempdir().expect("actual media storage");
        let _ = super::super::db::init(&root.path().join("projects.db"));
        let state = crate::dispatch::AppState::for_test();
        crate::dispatch::app_gate::test_support::install_app(
            &state,
            "projekty",
            &["project_studio.read"],
        );
        let actor = crate::db::repository::create_user_account(
            &state.db,
            "media-owner",
            "hash",
            "Media owner",
            "owner@example.test",
        )
        .expect("actual actor");
        let org = crate::services::org::DEFAULT_ORG_ID;
        crate::services::org::add_membership(&state.db, org, &actor, "role-org-viewer", "test")
            .expect("actual organization");
        let project = |name: &str| {
            let id = uuid::Uuid::new_v4().to_string();
            let dir = root.path().join(&id);
            std::fs::create_dir_all(dir.join("files")).expect("blob directory");
            super::super::repository::create_project(
                &id,
                org,
                &format!("{name} {id}"),
                "",
                "custom",
                "[\"tasks\",\"tests\"]",
                &actor,
                &dir.to_string_lossy(),
                "",
                None,
                false,
                false,
                false,
                &[],
            )
            .expect("real project");
            let record = super::super::repository::project_record(&id)
                .expect("record")
                .expect("project");
            let pool = super::super::project_db::open(&id).expect("content");
            (record, pool)
        };
        let (source, source_pool) = project("Media source");
        let (destination, destination_pool) = project("Media destination");
        crate::services::gstreamer_runtime::prepare_runtime_environment();
        gstreamer::init().expect("GStreamer");
        let original = Path::new(&source.dir_path).join("files/recording.avi");
        let generator=gstreamer::parse::launch("avimux name=mux ! filesink name=output videotestsrc num-buffers=60 pattern=ball ! video/x-raw,format=I420,width=1280,height=720,framerate=30/1 ! queue max-size-buffers=2 ! mux.")
            .expect("actual AVI generator").downcast::<gstreamer::Pipeline>().expect("pipeline");
        generator
            .by_name("output")
            .expect("sink")
            .set_property("location", original.to_str().expect("path"));
        finish_pipeline(&generator);
        let sha = hash_file(&original, &|| Ok(())).expect("immutable original SHA");
        let size = std::fs::metadata(&original).expect("video").len();
        assert!(size > 64 * 1024 * 1024);
        std::fs::rename(
            &original,
            Path::new(&source.dir_path).join("files").join(&sha),
        )
        .expect("content-addressed original");
        let attachment = tentaflow_protocol::project_studio::AttachmentWire {
            sha256: sha.clone(),
            name: "recording.avi".into(),
            mime: "video/x-msvideo".into(),
            size_bytes: size,
        };
        let task = super::super::tasks::create_task(
            &source_pool,
            &super::super::tasks::TaskInput {
                task_type: "technical",
                title: "Actual recording",
                description_md: "",
                severity: "",
                priority: "medium",
                status: "todo",
                assigned_to: "",
                due_date: "",
                parent_task_id: None,
                links_json: "[]",
                attachments_json: &serde_json::to_string(&vec![attachment]).expect("reference"),
            },
            &actor,
        )
        .expect("real source task");
        super::super::task_index::sync_project(&source, &source_pool, 256)
            .expect("canonical source location");
        let payload = PreviewJob {
            org_id: org.into(),
            user_id: actor.clone(),
            project_id: source.project_id.clone(),
            owner: AttachmentOwner {
                kind: tentaflow_protocol::project_studio::AttachmentOwnerKind::Task,
                id: task.task_id,
                step_index: None,
            },
            sha256: sha.clone(),
        };
        let queue = crate::services::ingest_jobs::init(&root.path().join("jobs.db"))
            .expect("actual durable queue");
        let namespace = namespace(&state.db).expect("database namespace");
        let job_id = format!("{}:{}:{}", namespace, source.project_id, sha);
        crate::services::ingest_jobs::enqueue(
            &queue,
            &namespace,
            &job_id,
            &serde_json::to_string(&payload).expect("source provenance"),
        )
        .expect("persist queued conversion");
        write_metadata(
            &previews_dir(Path::new(&source.dir_path))
                .expect("previews")
                .join(format!("{sha}.json")),
            &PreviewState {
                status: "queued".into(),
                error: String::new(),
                total_size: 0,
                duration_ms: 0,
            },
        )
        .expect("actual queued metadata");
        std::fs::write(
            Path::new(&destination.dir_path).join("files").join(&sha),
            b"wrong immutable bytes",
        )
        .expect("real hardlink identity failure");
        std::mem::forget(root);
        TransferMediaFixture {
            state,
            actor,
            source,
            destination,
            source_pool,
            destination_pool,
            queue,
            namespace,
            job_id,
            payload,
        }
    }

    #[cfg(feature = "camera")]
    #[tokio::test]
    async fn failed_transfer_keeps_queued_video_across_queue_reopen() {
        use crate::services::ingest_jobs as queue;
        let f = transfer_media_fixture();
        assert!(super::super::task_transfer::transfer_task(
            &f.state,
            &f.source,
            &f.destination,
            &f.source_pool,
            &f.destination_pool,
            &f.payload.owner.id,
            &f.actor,
            false
        )
        .is_err());
        assert_eq!(
            super::super::repository::task_location(&f.payload.owner.id)
                .expect("canonical")
                .expect("task")
                .project_id,
            f.source.project_id
        );
        let pending = queue::job(&f.queue, &f.job_id)
            .expect("queue")
            .expect("original work retained");
        assert!(!pending.cancel_requested);
        assert_eq!(
            read_preview_state(Path::new(&f.source.dir_path), &f.payload.sha256)
                .expect("metadata")
                .expect("state")
                .status,
            "queued"
        );
        let path: String = f
            .queue
            .read()
            .expect("queue reader")
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |row| row.get(0),
            )
            .expect("actual jobs database path");
        let reopened = queue::open_pool_at(Path::new(&path)).expect("restart queue connection");
        let resumed = queue::claim(&reopened, &f.namespace)
            .expect("claim")
            .expect("durable queued work");
        process_preview_job(&f.state, &reopened, &resumed)
            .expect("real video conversion after rollback/reopen");
        assert_eq!(
            read_preview_state(Path::new(&f.source.dir_path), &f.payload.sha256)
                .expect("state")
                .expect("actual ready")
                .status,
            "ready"
        );
        assert!(
            preview_path(Path::new(&f.source.dir_path), &f.payload.sha256)
                .expect("actual MP4")
                .is_file()
        );
        queue::finish(&reopened, &resumed).expect("current claim completion");
    }

    #[cfg(feature = "camera")]
    #[tokio::test]
    async fn published_video_transfer_resumes_pending_and_preserves_completed_preview() {
        use crate::services::ingest_jobs as queue;
        for source_ready in [false, true] {
            let f = transfer_media_fixture();
            if source_ready {
                let claimed = queue::claim(&f.queue, &f.namespace)
                    .expect("claim source")
                    .expect("real queued video");
                process_preview_job(&f.state, &f.queue, &claimed).expect("actual source preview");
                queue::finish(&f.queue, &claimed).expect("source conversion finished");
            }
            let destination_dir = Path::new(&f.destination.dir_path);
            std::fs::remove_file(destination_dir.join("files").join(&f.payload.sha256))
                .expect("remove intentional hardlink fault");
            let metadata = previews_dir(destination_dir)
                .expect("actual destination cache")
                .join(format!("{}.json", f.payload.sha256));
            if !source_ready {
                std::fs::write(&metadata, b"{interrupted metadata")
                    .expect("real post-publication failure");
            }
            let outcome = super::super::task_transfer::transfer_task(
                &f.state,
                &f.source,
                &f.destination,
                &f.source_pool,
                &f.destination_pool,
                &f.payload.owner.id,
                &f.actor,
                false,
            );
            let snapshots = if source_ready {
                let outcome = outcome.expect("ready preview transfers successfully");
                let json: String = super::super::db::pool()
                    .expect("registry")
                    .read()
                    .expect("manifest reader")
                    .query_row(
                        "SELECT sha_manifest_json FROM task_transfer_journal WHERE operation_id=?1",
                        [&outcome.operation_id],
                        |row| row.get(0),
                    )
                    .expect("real cleaned transfer manifest");
                serde_json::from_str::<Vec<TransferPreview>>(&json).expect("ready manifest")
            } else {
                outcome.expect_err("malformed cache interrupts cleanup after publication");
                let journal = super::super::repository::pending_task_transfers()
                    .expect("actual durable journals")
                    .into_iter()
                    .find(|operation| operation.task_ids.contains(&f.payload.owner.id))
                    .expect("published cleanup is recoverable");
                assert_eq!(journal.phase, "published");
                assert_eq!(
                    super::super::repository::task_location(&f.payload.owner.id)
                        .expect("canonical route")
                        .expect("task")
                        .project_id,
                    f.destination.project_id
                );
                assert!(super::super::repository::task_write_admission(
                    &f.destination.project_id,
                    &f.payload.owner.id
                )
                .is_err());
                std::fs::remove_file(&metadata).expect("repair injected metadata failure");
                let snapshots =
                    serde_json::from_str::<Vec<TransferPreview>>(&journal.sha_manifest_json)
                        .expect("real pending transfer manifest");
                assert!(snapshots[0].ready.is_none() && snapshots[0].requeue);
                resume_transfer(
                    &f.state,
                    &f.destination,
                    &f.destination_pool,
                    &f.actor,
                    &snapshots,
                )
                .expect("published destination is admitted for media");
                let claimed = queue::claim(&f.queue, &f.namespace)
                    .expect("claim destination")
                    .expect("real destination conversion queued");
                let payload: PreviewJob =
                    serde_json::from_str(&claimed.payload_json).expect("destination owner");
                assert_eq!(payload.project_id, f.destination.project_id);
                process_preview_job(&f.state, &f.queue, &claimed)
                    .expect("actual destination conversion while cleanup fence remains");
                queue::finish(&f.queue, &claimed).expect("actual destination completion");
                snapshots
            };
            let mp4 = preview_path(destination_dir, &f.payload.sha256).expect("actual ready MP4");
            let ready_hash = hash_file(&mp4, &|| Ok(())).expect("actual completed hash");
            let ready_identity = file_identity(&mp4).expect("immutable ready identity");
            let ready_metadata = std::fs::read(&metadata).expect("actual ready metadata bytes");
            f.state
                .db
                .write()
                .expect("account writer")
                .execute(
                    "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                    [&f.actor],
                )
                .expect("current actor revoked after completion");
            resume_transfer(
                &f.state,
                &f.destination,
                &f.destination_pool,
                &f.actor,
                &snapshots,
            )
            .expect("repeated cleanup preserves completed immutable preview");
            assert_eq!(std::fs::read(&metadata).expect("metadata"), ready_metadata);
            assert!(file_identity(&mp4).expect("ready file") == ready_identity);
            assert_eq!(hash_file(&mp4, &|| Ok(())).expect("ready SHA"), ready_hash);
            assert!(queue::job(
                &f.queue,
                &format!(
                    "{}:{}:{}",
                    f.namespace, f.destination.project_id, f.payload.sha256
                )
            )
            .expect("destination queue")
            .is_none());
            f.state
                .db
                .write()
                .expect("account writer")
                .execute(
                    "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                    [&f.actor],
                )
                .expect("active recovery actor");
            super::super::task_transfer::recover_transfers(&f.state)
                .expect("actual published cleanup replays before exposure");
            assert!(super::super::repository::pending_task_transfers()
                .expect("durable journals")
                .iter()
                .all(|operation| !operation.task_ids.contains(&f.payload.owner.id)));
            assert_eq!(
                std::fs::read(&metadata).expect("ready cache"),
                ready_metadata
            );
        }
    }

    #[cfg(feature = "camera")]
    #[tokio::test]
    async fn prepared_video_rollback_releases_destination_writer_before_queue_resume() {
        use crate::services::ingest_jobs as queue;
        use gstreamer::prelude::*;
        let f = transfer_media_fixture();
        let destination_dir = Path::new(&f.destination.dir_path);
        let probe_path = destination_dir.join("files/probe.avi");
        let generator = gstreamer::parse::launch("avimux name=mux ! filesink name=output videotestsrc num-buffers=8 pattern=ball ! video/x-raw,format=I420,width=320,height=240,framerate=30/1 ! queue max-size-buffers=2 ! mux.")
            .expect("actual destination video").downcast::<gstreamer::Pipeline>().expect("pipeline");
        generator
            .by_name("output")
            .expect("sink")
            .set_property("location", probe_path.to_str().expect("path"));
        finish_pipeline(&generator);
        let probe_sha = hash_file(&probe_path, &|| Ok(())).expect("actual destination SHA");
        let attachment = tentaflow_protocol::project_studio::AttachmentWire {
            sha256: probe_sha.clone(),
            name: "probe.avi".into(),
            mime: "video/x-msvideo".into(),
            size_bytes: std::fs::metadata(&probe_path).expect("video").len(),
        };
        std::fs::rename(&probe_path, destination_dir.join("files").join(&probe_sha))
            .expect("actual destination immutable blob");
        let case_id = uuid::Uuid::new_v4().to_string();
        f.destination_pool.write().expect("case writer").execute(
            "INSERT INTO test_cases(case_id,kind,title,content_json,status,created_by,attachments_json) \
             VALUES (?1,'manual','Destination recording','{}','approved',?2,?3)",
            rusqlite::params![case_id,f.actor,serde_json::to_string(&vec![attachment]).expect("case reference")],
        ).expect("actual destination attachment owner");
        let operation = super::super::models::TaskTransferJournal {
            operation_id: uuid::Uuid::new_v4().to_string(),
            org_id: f.source.org_id.clone(),
            source_project_id: f.source.project_id.clone(),
            destination_project_id: f.destination.project_id.clone(),
            actor_user_id: f.actor.clone(),
            task_ids: vec![f.payload.owner.id.clone()],
            consent_wider_access: false,
            phase: "prepared".into(),
            sha_manifest_json: "[]".into(),
            key_map_json: "{}".into(),
            event_map_json: "{}".into(),
        };
        super::super::repository::prepare_task_transfer(&operation)
            .expect("actual durable preparation");
        prepare_transfer(
            &f.state,
            &f.source,
            &f.source_pool,
            &operation.operation_id,
            &f.actor,
            &[(
                f.payload.owner.id.clone(),
                super::super::tasks::task_attachment_metadata(
                    &f.source_pool,
                    &f.payload.owner.id,
                    &f.payload.sha256,
                )
                .expect("actual metadata")
                .expect("source reference"),
            )],
        )
        .expect("suspend real queued source before recovery");
        let probe = PreviewJob {
            org_id: f.destination.org_id.clone(),
            project_id: f.destination.project_id.clone(),
            user_id: f.actor.clone(),
            owner: AttachmentOwner {
                kind: tentaflow_protocol::project_studio::AttachmentOwnerKind::Case,
                id: case_id,
                step_index: None,
            },
            sha256: probe_sha.clone(),
        };
        let probe_id = format!("{}:{}:{}", f.namespace, f.destination.project_id, probe_sha);
        queue::enqueue(
            &f.queue,
            &f.namespace,
            &probe_id,
            &serde_json::to_string(&probe).expect("payload"),
        )
        .expect("actual destination preview queue");
        let claimed = queue::claim(&f.queue, &f.namespace)
            .expect("claim")
            .expect("destination job");
        assert_eq!(claimed.job_id, probe_id);
        let held_destination = f
            .destination_pool
            .write()
            .expect("actual destination writer");
        let state = f.state.clone();
        let (rollback_tx, rollback_rx) = std::sync::mpsc::channel();
        let rollback = std::thread::spawn(move || {
            rollback_tx
                .send(super::super::task_transfer::recover_transfers(&state))
                .expect("recovery result");
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let aborting = {
                let conn = f.queue.read().expect("intent reader");
                transfer_intent(
                    &conn,
                    &f.namespace,
                    &f.source.project_id,
                    &operation.operation_id,
                    &f.payload.sha256,
                )
                .expect("durable intent")
                .is_some_and(|intent| intent.aborting)
            };
            if aborting {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "real rollback did not reach destination cleanup"
            );
            std::thread::yield_now();
        }
        let state = f.state.clone();
        let jobs = f.queue.clone();
        let pool = f.destination_pool.clone();
        let probe_claim = claimed.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let result = queue::with_transaction(&jobs, |conn| {
                acquired_tx.send(()).expect("actual queue gate held");
                read_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("canonical abort committed");
                current_preview_job(&state, &pool, conn, &probe_claim, &probe)
            });
            assert!(
                done_tx.send(result).is_ok(),
                "actual destination read result"
            );
        });
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("destination worker holds jobs writer");
        drop(held_destination);
        while super::super::repository::pending_task_transfers()
            .expect("journal")
            .iter()
            .any(|pending| pending.operation_id == operation.operation_id)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "rollback did not commit canonical abort"
            );
            std::thread::yield_now();
        }
        read_tx
            .send(())
            .expect("current destination worker continues");
        let payload = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("destination reader must not wait behind rollback's queue wait")
            .expect("current authorized destination job")
            .expect("current owner");
        assert_eq!(payload.project_id, f.destination.project_id);
        rollback_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("actual rollback resumes after destination read")
            .expect("prepared recovery finished");
        reader.join().expect("destination worker completed");
        rollback.join().expect("rollback completed");
        queue::finish(&f.queue, &claimed).expect("current probe completion");
        assert!(queue::job(&f.queue, &f.job_id)
            .expect("source queue")
            .is_some_and(|job| !job.cancel_requested));
    }

    #[cfg(feature = "camera")]
    #[tokio::test]
    async fn running_video_rollback_preserves_retry_and_shared_case_owner() {
        use crate::services::ingest_jobs as queue;
        for shared in [false, true] {
            let f = transfer_media_fixture();
            let case_id = uuid::Uuid::new_v4().to_string();
            if shared {
                let raw = super::super::tasks::get_task(&f.source_pool, &f.payload.owner.id)
                    .expect("task")
                    .expect("row")
                    .attachments_json;
                f.source_pool.write().expect("case writer").execute("INSERT INTO test_cases(case_id,kind,title,content_json,status,created_by,attachments_json) \
                    VALUES (?1,'manual','Shared recording','{}','approved',?2,?3)",rusqlite::params![case_id,f.actor,raw]).expect("actual shared source owner");
            }
            let claimed = queue::claim(&f.queue, &f.namespace)
                .expect("claim")
                .expect("running conversion");
            let output = previews_dir(Path::new(&f.source.dir_path))
                .expect("preview directory")
                .join(format!("{}.mp4", f.payload.sha256));
            let part = output.with_extension("mp4.part");
            let mux = output.with_extension("mp4.mux");
            let observed = [part.clone(), mux.clone()];
            let state = f.state.clone();
            let source = f.source.clone();
            let destination = f.destination.clone();
            let source_pool = f.source_pool.clone();
            let destination_pool = f.destination_pool.clone();
            let actor = f.actor.clone();
            let task_id = f.payload.owner.id.clone();
            let failure = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                while !observed
                    .iter()
                    .any(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0))
                {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "actual conversion produced no partial output"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                super::super::task_transfer::transfer_task(
                    &state,
                    &source,
                    &destination,
                    &source_pool,
                    &destination_pool,
                    &task_id,
                    &actor,
                    false,
                )
                .expect_err("real hardlink mismatch aborts transfer");
            });
            let result = process_preview_job(&f.state, &f.queue, &claimed);
            failure.join().expect("transfer observer");
            if shared {
                result.expect("shared case conversion remains authorized during task fence");
                let current = queue::job(&f.queue, &f.job_id)
                    .expect("queue")
                    .expect("claim retained");
                let payload: PreviewJob =
                    serde_json::from_str(&current.payload_json).expect("current owner");
                assert_eq!(
                    payload.owner.kind,
                    tentaflow_protocol::project_studio::AttachmentOwnerKind::Case
                );
                assert_eq!(payload.owner.id, case_id);
                queue::finish(&f.queue, &claimed).expect("shared completion");
            } else {
                result.expect_err("exclusive conversion yields to the actual transfer fence");
                assert!(!part.exists() && !mux.exists());
                queue::finish(&f.queue, &claimed)
                    .expect("cancellation acknowledgement restores pending work");
                let retry = queue::claim(&f.queue, &f.namespace)
                    .expect("claim retry")
                    .expect("source pending survived rollback");
                assert_ne!(retry.claim_token, claimed.claim_token);
                queue::finish(&f.queue, &claimed).expect("late old finalizer cannot erase retry");
                process_preview_job(&f.state, &f.queue, &retry)
                    .expect("real source conversion retries after rollback");
                queue::finish(&f.queue, &retry).expect("actual retry completion");
            }
            assert_eq!(
                read_preview_state(Path::new(&f.source.dir_path), &f.payload.sha256)
                    .expect("state")
                    .expect("actual ready")
                    .status,
                "ready"
            );
            assert_eq!(
                hash_file(
                    &Path::new(&f.source.dir_path)
                        .join("files")
                        .join(&f.payload.sha256),
                    &|| Ok(())
                )
                .expect("original preserved"),
                f.payload.sha256
            );
        }
    }

    #[cfg(feature = "camera")]
    #[tokio::test]
    async fn actual_large_video_and_audio_convert_to_seekable_mp4_and_revocation_cleans_partial() {
        use gst::prelude::*;
        use gstreamer as gst;
        crate::services::gstreamer_runtime::prepare_runtime_environment();
        gst::init().expect("GStreamer");
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("recording.avi");
        let generator = gst::parse::launch("avimux name=mux ! filesink name=output videotestsrc num-buffers=90 pattern=ball ! video/x-raw,format=I420,width=1280,height=720,framerate=30/1 ! queue max-size-buffers=2 ! mux. audiotestsrc num-buffers=90 samplesperbuffer=1600 wave=sine ! audio/x-raw,rate=48000,channels=1 ! queue max-size-buffers=8 ! mux.")
            .expect("real raw video and audio fixture").downcast::<gst::Pipeline>().expect("pipeline");
        generator
            .by_name("output")
            .expect("sink")
            .set_property("location", source.to_str().expect("path"));
        finish_pipeline(&generator);
        assert!(std::fs::metadata(&source).expect("original").len() > 64 * 1024 * 1024);
        let original_sha = hash_file(&source, &|| Ok(())).expect("original SHA");
        let output = root.path().join("preview.mp4");
        let (size, duration) = transcode(&source, &output, &|| Ok(()), &|temp, _, _| {
            std::fs::rename(temp, &output)?;
            Ok(())
        })
        .expect("actual GStreamer conversion");
        assert!(size > 0);
        assert!(
            (2900..=3200).contains(&duration),
            "actual duration {duration}"
        );
        let (header, total, _) =
            read_range(&output, 0, 4 * 1024 * 1024).expect("bounded MP4 header");
        assert_eq!(total, size);
        let moov = header
            .windows(4)
            .position(|atom| atom == b"moov")
            .expect("seek metadata");
        let mdat = header
            .windows(4)
            .position(|atom| atom == b"mdat")
            .expect("media bytes");
        assert!(moov < mdat, "faststart seek metadata precedes media");
        let decoder = gst::parse::launch("filesrc name=input ! decodebin name=decode decode. ! queue ! videoconvert ! fakesink sync=false decode. ! queue ! audioconvert ! fakesink sync=false")
            .expect("verify actual video and audio decoding").downcast::<gst::Pipeline>().expect("decoder pipeline");
        decoder
            .by_name("input")
            .expect("source")
            .set_property("location", output.to_str().expect("path"));
        finish_pipeline(&decoder);
        let (tail, _, eof) = read_range(&output, size - 32, 32).expect("native Range seek tail");
        assert_eq!(tail.len(), 32);
        assert!(eof);
        let attempts = std::sync::atomic::AtomicU32::new(0);
        let cancelled = root.path().join("cancelled.mp4");
        let denied = transcode(
            &source,
            &cancelled,
            &|| {
                if attempts.fetch_add(1, Ordering::SeqCst) >= 5 {
                    Err(anyhow!("grant expired during conversion"))
                } else {
                    Ok(())
                }
            },
            &|temp, _, _| {
                std::fs::rename(temp, &cancelled)?;
                Ok(())
            },
        )
        .expect_err("conversion stops on current revocation");
        assert!(format!("{denied:#}").contains("grant expired"));
        assert!(!cancelled.exists());
        assert!(!cancelled.with_extension("mp4.part").exists());
        assert!(!cancelled.with_extension("mp4.mux").exists());
        assert_eq!(
            hash_file(&source, &|| Ok(())).expect("preserved original SHA"),
            original_sha
        );

        let state = crate::dispatch::AppState::for_test();
        crate::dispatch::app_gate::test_support::install_app(
            &state,
            "projekty",
            &["project_studio.read"],
        );
        let org_id = crate::services::org::DEFAULT_ORG_ID;
        let actor_id = crate::db::repository::create_user_account(
            &state.db,
            "media-worker",
            "hash",
            "Media worker",
            "media@example.test",
        )
        .expect("real active account");
        crate::services::org::add_membership(
            &state.db,
            org_id,
            &actor_id,
            "role-org-viewer",
            "test",
        )
        .expect("real organization membership");
        state
            .db
            .write()
            .expect("writer")
            .execute(
                "UPDATE user_accounts SET must_change_password = 0 WHERE id = ?1",
                [&actor_id],
            )
            .expect("valid actor session");
        super::super::db::init(&root.path().join("projects.db")).expect("project registry");
        let project_id = format!("account-preview-{}", uuid::Uuid::new_v4());
        let project_dir = root.path().join(&project_id);
        std::fs::create_dir_all(project_dir.join("files")).expect("project files");
        let original = project_dir.join("files").join(&original_sha);
        std::fs::copy(&source, &original).expect("actual content-addressed original");
        super::super::repository::create_project(
            &project_id,
            org_id,
            "Account preview",
            "",
            "custom",
            "[\"tasks\"]",
            &actor_id,
            &project_dir.to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("actual owned project");
        let pool = super::super::project_db::open(&project_id).expect("project content");
        let attachments =
            serde_json::to_string(&vec![tentaflow_protocol::project_studio::AttachmentWire {
                name: "recording.avi".into(),
                mime: "video/x-msvideo".into(),
                sha256: original_sha.clone(),
                size_bytes: std::fs::metadata(&original).expect("source").len(),
            }])
            .expect("exact attachment reference");
        let mutation = super::super::tasks::create_task(
            &pool,
            &super::super::tasks::TaskInput {
                task_type: "technical",
                title: "Account-bound recording",
                description_md: "Actual queued recording",
                severity: "",
                priority: "medium",
                status: "todo",
                assigned_to: "",
                due_date: "",
                parent_task_id: None,
                links_json: "[]",
                attachments_json: &attachments,
            },
            &actor_id,
        )
        .expect("actual task and event");
        let payload = PreviewJob {
            org_id: org_id.into(),
            user_id: actor_id.clone(),
            project_id: project_id.clone(),
            owner: AttachmentOwner {
                kind: tentaflow_protocol::project_studio::AttachmentOwnerKind::Task,
                id: mutation.task_id,
                step_index: None,
            },
            sha256: original_sha.clone(),
        };
        let queue = crate::services::ingest_jobs::init(&root.path().join("jobs.db"))
            .expect("durable queue");
        // Process-wide pools keep these paths until the test process ends.
        std::mem::forget(root);
        let namespace = make_worker(&state)
            .expect("actual database worker")
            .namespace
            .clone();
        let job_id = format!("account-job-{}", uuid::Uuid::new_v4());
        crate::services::ingest_jobs::enqueue(
            &queue,
            &namespace,
            &job_id,
            &serde_json::to_string(&payload).expect("provenance"),
        )
        .expect("persist actual actor and source");
        crate::services::ingest_jobs::claim(&queue, &namespace)
            .expect("claim")
            .expect("job");
        queue
            .write()
            .expect("writer")
            .execute(
                "UPDATE ingest_jobs SET owner_instance = 'prior-media-process' WHERE job_id = ?1",
                [&job_id],
            )
            .expect("interrupted claimed job");
        state
            .db
            .write()
            .expect("writer")
            .execute(
                "UPDATE user_accounts SET is_active = 0 WHERE id = ?1",
                [&actor_id],
            )
            .expect("disable before restart");
        restore_queue(&namespace).expect("restart actual queued provenance");
        let resumed = crate::services::ingest_jobs::claim(&queue, &namespace)
            .expect("claim resumed")
            .expect("actual recovered job");
        let denial = process_preview_job(&state, &queue, &resumed)
            .expect_err("disabled actor cannot start after restart");
        assert!(format!("{denial:#}").contains("account is disabled"));
        let failed = read_preview_state(&project_dir, &original_sha)
            .expect("metadata")
            .expect("honest terminal state");
        assert_eq!(failed.status, "error");
        assert_eq!(failed.total_size, 0);
        let destination = previews_dir(&project_dir)
            .expect("private previews")
            .join(format!("{original_sha}.mp4"));
        assert!(!destination.exists());
        crate::services::ingest_jobs::finish(&queue, &resumed).expect("close rejected job");

        state
            .db
            .write()
            .expect("writer")
            .execute(
                "UPDATE user_accounts SET is_active = 1 WHERE id = ?1",
                [&actor_id],
            )
            .expect("reactivate for a real subsequent conversion");
        let second_id = format!("running-account-job-{}", uuid::Uuid::new_v4());
        crate::services::ingest_jobs::enqueue(
            &queue,
            &namespace,
            &second_id,
            &serde_json::to_string(&payload).expect("provenance"),
        )
        .expect("explicit subsequent attempt");
        let running = crate::services::ingest_jobs::claim(&queue, &namespace)
            .expect("claim")
            .expect("job");
        let observer_db = state.db.clone();
        let observer_actor = actor_id.clone();
        let partial = destination.with_extension("mp4.part");
        let mux_partial = destination.with_extension("mp4.mux");
        let observed_partial = partial.clone();
        let observed_mux = mux_partial.clone();
        let disable = std::thread::spawn(move || -> Result<()> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                if [&observed_partial, &observed_mux]
                    .iter()
                    .any(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0))
                {
                    observer_db.write()?.execute(
                        "UPDATE user_accounts SET is_active = 0 WHERE id = ?1",
                        [&observer_actor],
                    )?;
                    return Ok(());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow!(
                        "the actual conversion never produced an observable partial"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        let denial = process_preview_job(&state, &queue, &running)
            .expect_err("account disabled during actual conversion stops work");
        disable
            .join()
            .expect("observer thread")
            .expect("disable after actual media bytes");
        assert!(format!("{denial:#}").contains("account is disabled"));
        assert_eq!(
            read_preview_state(&project_dir, &original_sha)
                .expect("metadata")
                .expect("honest error")
                .status,
            "error"
        );
        assert!(!destination.exists());
        assert!(!partial.exists());
        assert!(!mux_partial.exists());
        assert_eq!(
            hash_file(&original, &|| Ok(())).expect("preserved content-addressed SHA"),
            original_sha
        );
        crate::services::ingest_jobs::finish(&queue, &running).expect("close interrupted job");
    }

    #[tokio::test]
    async fn durable_conversion_queue_recovers_interrupted_work_and_uses_actual_db_identity() {
        let root = tempfile::tempdir().expect("tempdir");
        let queue =
            crate::services::ingest_jobs::init(&root.path().join("jobs.db")).expect("queue");
        let state_a = crate::dispatch::AppState::for_test();
        let state_b = crate::dispatch::AppState::for_test();
        let a = make_worker(&state_a).expect("first database namespace");
        let b = make_worker(&state_b).expect("second database namespace");
        assert_ne!(a.namespace, b.namespace);
        let id = format!("preview-{}", uuid::Uuid::new_v4());
        crate::services::ingest_jobs::enqueue(&queue, &a.namespace, &id, "{}")
            .expect("persist pending work");
        crate::services::ingest_jobs::claim(&queue, &a.namespace)
            .expect("claim")
            .expect("actual job");
        queue
            .write()
            .expect("writer")
            .execute(
                "UPDATE ingest_jobs SET owner_instance = 'previous-process' WHERE job_id = ?1",
                [&id],
            )
            .expect("interrupted prior process");
        restore_queue(&a.namespace).expect("restart restores actual queue");
        let resumed = crate::services::ingest_jobs::claim(&queue, &a.namespace)
            .expect("claim restored")
            .expect("restored job");
        assert_eq!(resumed.job_id, id);
        crate::services::ingest_jobs::finish(&queue, &resumed).expect("cleanup queue");
    }
}
