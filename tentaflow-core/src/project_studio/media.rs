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

fn preview_state(dir: &Path, sha: &str) -> Result<Option<PreviewState>> {
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
        preview_state(dir, sha)?.ok_or_else(|| anyhow!("preview has not been requested"))?;
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
        let worker = worker.clone();
        runtime.spawn(async move {
            let _lifetime = WorkerLifetime(worker.clone());
            if let Err(error) = restore_queue(&worker.namespace) { tracing::error!(%error, "media queue restart recovery failed"); }
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
                        if let Err(error) = crate::services::ingest_jobs::finish(&queue, &job.job_id) { tracing::error!(%error, "video preview queue completion failed"); }
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

fn save_preview_state(path: &Path, state: &PreviewState) -> Result<()> {
    let _guard = preview_lock(path)
        .lock()
        .map_err(|_| anyhow!("preview lock poisoned"))?;
    write_metadata(path, state)
}

fn make_worker(state: &Arc<crate::dispatch::AppState>) -> Result<Arc<PreviewWorker>> {
    let path: String = state.db.read()?.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    let identity = if path.is_empty() {
        format!("memory-{:p}", Arc::as_ptr(&state.db))
    } else {
        path
    };
    let namespace = format!(
        "project_media:{}",
        hex::encode(Sha256::digest(identity.as_bytes()))
    );
    Ok(Arc::new(PreviewWorker {
        database: Arc::downgrade(&state.db),
        state: std::sync::RwLock::new(Arc::downgrade(state)),
        namespace,
        wake: tokio::sync::Notify::new(),
        running: AtomicBool::new(false),
    }))
}

pub fn start_workers(state: Arc<crate::dispatch::AppState>) {
    if let Err(error) = worker_for(&state) {
        tracing::error!(%error, "project video worker startup failed");
    }
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
    save_preview_state(
        &previews_dir(Path::new(&project.dir_path))?.join(format!("{}.json", payload.sha256)),
        &state,
    )
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
    let dir = Path::new(&project.dir_path);
    if preview_state(dir, &payload.sha256)?.is_some_and(|state| state.status == "ready")
        && preview_path(dir, &payload.sha256).is_ok()
    {
        return Ok(());
    }
    let result = (|| -> Result<(u64, u64)> {
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
        let allowed = || {
            let actor = crate::db::repository::get_user_account_by_id(&state.db, &payload.user_id)?
                .ok_or_else(|| anyhow!("media actor account is unavailable"))?;
            if !actor.is_active {
                return Err(anyhow!("media actor account is disabled"));
            }
            if crate::services::ingest_jobs::heartbeat(queue, &job.job_id)?
                != crate::services::ingest_jobs::JobLiveness::Running
            {
                return Err(anyhow!("video conversion was cancelled"));
            }
            crate::dispatch::project_studio::require_attachment(
                &ctx,
                &payload.project_id,
                &payload.owner,
                &payload.sha256,
            )
            .map(|_| ())
            .map_err(|e| anyhow!(e.message))
        };
        allowed()?;
        let processing = PreviewState {
            status: "processing".into(),
            error: String::new(),
            total_size: 0,
            duration_ms: 0,
        };
        save_preview_state(
            &previews_dir(dir)?.join(format!("{}.json", payload.sha256)),
            &processing,
        )?;
        let output = previews_dir(dir)?.join(format!("{}.mp4", payload.sha256));
        let converted = transcode(&dir.join("files").join(&payload.sha256), &output, &allowed)?;
        if let Err(error) = allowed() {
            std::fs::remove_file(&output)?;
            return Err(error);
        }
        Ok(converted)
    })();
    let final_state = match &result {
        Ok((size, duration)) => PreviewState {
            status: "ready".into(),
            error: String::new(),
            total_size: *size,
            duration_ms: *duration,
        },
        Err(error) => PreviewState {
            status: "error".into(),
            error: error.to_string(),
            total_size: 0,
            duration_ms: 0,
        },
    };
    save_preview_state(
        &previews_dir(dir)?.join(format!("{}.json", payload.sha256)),
        &final_state,
    )?;
    result.map(|_| ())
}

pub fn request_preview(
    ctx: &HandlerContext,
    project_id: &str,
    owner: &AttachmentOwner,
    sha: &str,
    retry: bool,
) -> Result<PreviewState> {
    let (project, attachment) =
        crate::dispatch::project_studio::require_attachment(ctx, project_id, owner, sha)
            .map_err(|e| anyhow!(e.message))?;
    if !attachment.mime.starts_with("video/") {
        return Err(anyhow!("attachment is not a video"));
    }
    let worker = worker_for(&ctx.state)?;
    let queue = crate::services::ingest_jobs::pool()?;
    let job_id = format!("{}:{project_id}:{sha}", worker.namespace);
    let dir = Path::new(&project.dir_path);
    let meta = previews_dir(dir)?.join(format!("{sha}.json"));
    let _guard = preview_lock(&meta)
        .lock()
        .map_err(|_| anyhow!("preview lock poisoned"))?;
    let state = preview_state(dir, sha)?;
    if let Some(state) = &state {
        if state.status == "ready" && preview_path(dir, sha).is_ok() {
            return Ok(state.clone());
        }
        if crate::services::ingest_jobs::is_pending(&queue, &job_id)? {
            return Ok(state.clone());
        }
        if !retry {
            if state.status == "error" {
                return Ok(state.clone());
            }
            let interrupted = PreviewState {
                status: "error".into(),
                error:
                    "conversion was interrupted or its cached file is unavailable; retry to resume"
                        .into(),
                total_size: 0,
                duration_ms: 0,
            };
            write_metadata(&meta, &interrupted)?;
            return Ok(interrupted);
        }
    }
    let org = ctx
        .org_context
        .as_ref()
        .ok_or_else(|| anyhow!("media principal is unavailable"))?;
    let payload = PreviewJob {
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
        project_id: project_id.into(),
        owner: owner.clone(),
        sha256: sha.into(),
    };
    let queued = PreviewState {
        status: "queued".into(),
        error: String::new(),
        total_size: 0,
        duration_ms: 0,
    };
    write_metadata(&meta, &queued)?;
    if let Err(error) = crate::services::ingest_jobs::enqueue(
        &queue,
        &worker.namespace,
        &job_id,
        &serde_json::to_string(&payload)?,
    ) {
        if !crate::services::ingest_jobs::is_pending(&queue, &job_id)? {
            let failed = PreviewState {
                status: "error".into(),
                error: error.to_string(),
                total_size: 0,
                duration_ms: 0,
            };
            write_metadata(&meta, &failed)?;
            return Err(error);
        }
    }
    worker.wake.notify_one();
    Ok(queued)
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
fn transcode(source: &Path, output: &Path, allowed: &dyn Fn() -> Result<()>) -> Result<(u64, u64)> {
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
        std::fs::rename(&temp, output)?;
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
        let (size, duration) =
            transcode(&source, &output, &|| Ok(())).expect("actual GStreamer conversion");
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
        let denied = transcode(&source, &cancelled, &|| {
            if attempts.fetch_add(1, Ordering::SeqCst) >= 5 {
                Err(anyhow!("grant expired during conversion"))
            } else {
                Ok(())
            }
        })
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
        let failed = preview_state(&project_dir, &original_sha)
            .expect("metadata")
            .expect("honest terminal state");
        assert_eq!(failed.status, "error");
        assert_eq!(failed.total_size, 0);
        let destination = previews_dir(&project_dir)
            .expect("private previews")
            .join(format!("{original_sha}.mp4"));
        assert!(!destination.exists());
        crate::services::ingest_jobs::finish(&queue, &job_id).expect("close rejected job");

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
            preview_state(&project_dir, &original_sha)
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
        crate::services::ingest_jobs::finish(&queue, &second_id).expect("close interrupted job");
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
        crate::services::ingest_jobs::finish(&queue, &id).expect("cleanup queue");
    }
}
