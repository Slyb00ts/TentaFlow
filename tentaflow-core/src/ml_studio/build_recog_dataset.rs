// ===== File: ml_studio/build_recog_dataset.rs — build COCO dataset from raw media =====
//
// Recognition projects train on COCO directories. Until now the only way to get
// one was to point at a pre-built `coco_path` on disk. This module lets the user
// upload many raw files (images + video), stages them server-side per project,
// then builds a `<coco_dir>/train/` with copied/decoded images, real image
// dimensions and an empty-annotation `_annotations.coco.json` (the fixed 17 ADR
// categories). Annotations are filled later by auto-label + the manual editor.
//
// Building decodes HEIC and runs ffmpeg per video — for hundreds of clips this is
// MINUTES of work, so (like RF-DETR training in `train_recognition.rs`) it runs as
// an async background job: the request returns a build id immediately and the UI
// polls `BuildProgress`. One build per project at a time; an overall timeout and
// hard server-side caps (fps range, file count, total/per-video frame count) guard
// against runaway work. The dataset is built into a temp dir and atomically renamed
// into place only after `_annotations.coco.json` is written, and the DB row is
// created BEFORE staging is cleared so a failed registration leaves the originals
// for retry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Canonical 17 detection categories (id order matters: it is baked into the
/// COCO file and every model trained from it). Kept here as the single source.
const CATEGORIES: &[&str] = &[
    "tablica_adr",
    "tablica_rejestracyjna",
    "nalepka_2.1",
    "nalepka_2.2",
    "nalepka_2.3",
    "nalepka_3",
    "nalepka_4.1",
    "nalepka_4.2",
    "nalepka_4.3",
    "nalepka_5.1",
    "nalepka_5.2",
    "nalepka_6.1",
    "nalepka_8",
    "nalepka_9",
    "znak_srodowiskowy",
    "termometr",
    "nalepka_nieznana",
];

// Server-side caps — UI caps are bypassable, so these are the real limits enforced
// in Rust before any tool runs.
const FPS_MIN: u32 = 1;
const FPS_MAX: u32 = 30;
/// Max staged source files consumed by a single build.
const MAX_SOURCE_FILES: usize = 2000;
/// Max images a single build may produce across all sources (copies + frames).
const MAX_TOTAL_FRAMES: u64 = 200_000;
/// Max frames extracted from ONE video clip (also bounded by the %04d pattern).
const MAX_FRAMES_PER_VIDEO: u64 = 9999;
/// Overall wall-clock budget for one build.
const BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// Per-file budget for an external media tool (heif-convert/ffmpeg). A corrupt or
/// adversarial file must never hang a build (which would also pin the per-project
/// build slot forever), so each subprocess is wrapped in the OS `timeout` command.
const PER_FILE_TIMEOUT_SECS: u64 = 600;

// Staging quota — a per-project cap enforced as uploads complete, plus a TTL that
// reaps stale staging dirs (mirrors the chunked-upload TTL in dispatch).
const STAGING_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const STAGING_MAX_FILES: usize = 4000;
const STAGING_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Staging root for uploaded raw media, per project. Lives under the cache dir so
/// large transient uploads follow the cache override (e.g. a non-root disk) and
/// never bloat the backed-up data dir before the dataset is actually built.
pub fn staging_dir(project_id: &str) -> PathBuf {
    crate::paths::cache_dir()
        .join("ml-recog-staging")
        .join(sanitize_component(project_id))
}

/// Destination root for a built `coco_path` dataset. Uses the unique build id (not
/// the user-supplied name) as the on-disk directory so two builds with names that
/// sanitize to the same component never collide; the display name stays metadata
/// on the dataset row.
fn dataset_dir(project_id: &str, build_id: &str) -> PathBuf {
    crate::paths::data_dir()
        .join("ml-studio")
        .join("recog-datasets")
        .join(sanitize_component(project_id))
        .join(sanitize_component(build_id))
}

/// Keeps a user-supplied id/name safe as a single path component (no traversal,
/// no separators). Non-alphanumerics collapse to `_`.
fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    }
}

/// Reaps staging dirs untouched for longer than `STAGING_TTL`. Best-effort; called
/// before each stage write so abandoned uploads do not pin disk forever.
fn reap_stale_staging() {
    let root = crate::paths::cache_dir().join("ml-recog-staging");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Newest mtime across the dir's files is its "last touch".
        let last = newest_mtime(&path);
        if let Some(last) = last {
            if now
                .duration_since(last)
                .map(|d| d > STAGING_TTL)
                .unwrap_or(false)
            {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

fn newest_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if let Ok(meta) = entry.metadata() {
            if let Ok(m) = meta.modified() {
                newest = Some(newest.map_or(m, |cur| cur.max(m)));
            }
        }
    }
    newest
}

/// Per-project lock serializing the staging quota check-then-write, so two
/// concurrent completed uploads cannot both read the same usage and write past
/// the cap. Poison-tolerant: a panic elsewhere must not wedge staging.
fn staging_lock(project_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(project_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Current total bytes + file count staged for a project.
fn staging_usage(dir: &Path) -> (u64, usize) {
    let mut bytes = 0u64;
    let mut count = 0usize;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    bytes += meta.len();
                    count += 1;
                }
            }
        }
    }
    (bytes, count)
}

/// Writes one fully-received staged file to `staging_dir(project_id)`. Returns
/// the on-disk path. Filenames are sanitized to a basename and de-duplicated so
/// two uploads with the same name cannot clobber each other. Rejects the write if
/// it would push the project over the staging quota (bytes or file count).
pub fn stage_file(project_id: &str, filename: &str, bytes: &[u8]) -> Result<PathBuf> {
    reap_stale_staging();
    let dir = staging_dir(project_id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create staging dir {}", dir.display()))?;

    // Serialize quota check + write per project against concurrent uploads.
    let lock = staging_lock(project_id);
    let _quota_guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    let (used_bytes, used_files) = staging_usage(&dir);
    if used_files + 1 > STAGING_MAX_FILES {
        anyhow::bail!(
            "limit plików w poczekalni projektu osiągnięty ({} plików) — zbuduj dataset, aby zwolnić miejsce",
            STAGING_MAX_FILES
        );
    }
    if used_bytes.saturating_add(bytes.len() as u64) > STAGING_MAX_BYTES {
        anyhow::bail!(
            "limit rozmiaru poczekalni projektu osiągnięty ({} GB) — zbuduj dataset, aby zwolnić miejsce",
            STAGING_MAX_BYTES / (1024 * 1024 * 1024)
        );
    }

    let base = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload");
    let safe = sanitize_filename(base);
    let dest = unique_path(&dir, &safe);
    std::fs::write(&dest, bytes)
        .with_context(|| format!("write staged file {}", dest.display()))?;
    Ok(dest)
}

/// Sanitizes a basename, preserving the extension (needed to dispatch on type).
fn sanitize_filename(name: &str) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !e.is_empty() => (s, Some(e)),
        _ => (name, None),
    };
    let safe_stem = sanitize_component(stem);
    match ext {
        Some(e) => format!(
            "{}.{}",
            safe_stem,
            sanitize_component(e).to_ascii_lowercase()
        ),
        None => safe_stem,
    }
}

/// Returns a path inside `dir` for `name`, appending `_N` before the extension
/// until the path is free.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !e.is_empty() => (s.to_string(), format!(".{}", e)),
        _ => (name.to_string(), String::new()),
    };
    let mut n = 1u32;
    loop {
        let candidate = dir.join(format!("{}_{}{}", stem, n, ext));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Live progress of an async build, polled by the UI (mirrors the `DatasetSyncProgress`
/// pattern). `status` is "running" | "succeeded" | "failed".
#[derive(Clone, Debug)]
pub struct BuildProgress {
    pub status: String,
    pub files_total: u64,
    pub files_done: u64,
    pub frames_extracted: u64,
    pub dataset_id: Option<String>,
    pub image_count: u64,
    pub category_count: u32,
    pub error: Option<String>,
}

impl Default for BuildProgress {
    fn default() -> Self {
        BuildProgress {
            status: "running".to_string(),
            files_total: 0,
            files_done: 0,
            frames_extracted: 0,
            dataset_id: None,
            image_count: 0,
            category_count: 0,
            error: None,
        }
    }
}

static BUILD_PROGRESS: OnceLock<Mutex<std::collections::HashMap<String, BuildProgress>>> =
    OnceLock::new();

fn progress_map() -> &'static Mutex<std::collections::HashMap<String, BuildProgress>> {
    BUILD_PROGRESS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn set_progress(build_id: &str, p: BuildProgress) {
    if let Ok(mut m) = progress_map().lock() {
        m.insert(build_id.to_string(), p);
    }
}

fn update_progress(build_id: &str, f: impl FnOnce(&mut BuildProgress)) {
    if let Ok(mut m) = progress_map().lock() {
        if let Some(p) = m.get_mut(build_id) {
            f(p);
        }
    }
}

/// Current progress of a build (None when the build id is unknown).
pub fn build_progress(build_id: &str) -> Option<BuildProgress> {
    progress_map().lock().ok()?.get(build_id).cloned()
}

// Per-project guard: only one build may run at a time for a given project. A second
// concurrent request for the same project is rejected.
static ACTIVE_BUILDS: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn active_builds() -> &'static Mutex<std::collections::HashSet<String>> {
    ACTIVE_BUILDS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Tries to claim the build slot for a project. Returns false if a build is already
/// running for it.
fn try_claim_project(project_id: &str) -> bool {
    if let Ok(mut s) = active_builds().lock() {
        s.insert(project_id.to_string())
    } else {
        false
    }
}

fn release_project(project_id: &str) {
    if let Ok(mut s) = active_builds().lock() {
        s.remove(project_id);
    }
}

/// Starts an async build of a COCO dataset from the project's staged media. Returns
/// the build id used for status polling, or an error if a build is already running
/// for this project or `fps` is out of the allowed range. The build registers the
/// dataset in the DB on success; the heavy work (HEIC decode, ffmpeg) runs off the
/// RPC thread.
pub fn spawn_build(
    project_id: String,
    owner_user_id: String,
    dataset_name: String,
    fps: u32,
    source_dir: Option<String>,
) -> Result<String> {
    if fps < FPS_MIN || fps > FPS_MAX {
        anyhow::bail!("fps musi być w zakresie {}..={}", FPS_MIN, FPS_MAX);
    }
    // A non-empty server folder path replaces staging as the media source; the
    // folder must exist before a build is queued so the user gets an immediate error.
    let source_dir = source_dir.filter(|s| !s.trim().is_empty());
    match &source_dir {
        Some(dir) => {
            let path = Path::new(dir.trim());
            if !path.is_dir() {
                anyhow::bail!(
                    "folder źródłowy nie istnieje lub nie jest katalogiem: {}",
                    dir.trim()
                );
            }
        }
        None => {
            let staging = staging_dir(&project_id);
            if !staging.is_dir() || staging_usage(&staging).1 == 0 {
                anyhow::bail!("brak wgranych plików do zbudowania datasetu");
            }
        }
    }
    if !try_claim_project(&project_id) {
        anyhow::bail!("budowa datasetu dla tego projektu już trwa — poczekaj na jej zakończenie");
    }

    let build_id = uuid::Uuid::new_v4().to_string();
    set_progress(&build_id, BuildProgress::default());

    let build_id_task = build_id.clone();
    tokio::spawn(async move {
        let bid = build_id_task.clone();
        let pid = project_id.clone();
        // Heavy fs/tool work is blocking — keep it off the async worker.
        let result = tokio::task::spawn_blocking(move || {
            run_build(
                &build_id_task,
                &project_id,
                &owner_user_id,
                &dataset_name,
                fps,
                source_dir.as_deref(),
            )
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(build_id = %bid, error = %err, "recog dataset build failed");
                update_progress(&bid, |p| {
                    p.status = "failed".to_string();
                    p.error = Some(err.to_string());
                });
            }
            Err(join_err) => {
                tracing::warn!(build_id = %bid, error = %join_err, "recog dataset build task panicked");
                update_progress(&bid, |p| {
                    p.status = "failed".to_string();
                    p.error = Some(format!("build task failed: {}", join_err));
                });
            }
        }
        release_project(&pid);
    });

    Ok(build_id)
}

/// Synchronous build body run inside `spawn_blocking`. Builds into a fresh temp dir
/// and atomically renames into the final location only after every source is
/// processed AND `_annotations.coco.json` is written; registers the dataset row
/// BEFORE clearing staging so a failed registration leaves originals for retry.
fn run_build(
    build_id: &str,
    project_id: &str,
    owner_user_id: &str,
    dataset_name: &str,
    fps: u32,
    source_dir: Option<&str>,
) -> Result<()> {
    let deadline = Instant::now() + BUILD_TIMEOUT;
    let staging = staging_dir(project_id);
    // When a server folder is given it is the media source (scanned recursively);
    // otherwise the per-project staging dir is used (and cleared on success).
    let use_staging = source_dir.is_none();

    let mut sources: Vec<PathBuf> = Vec::new();
    match source_dir {
        Some(dir) => {
            let root = Path::new(dir.trim());
            if !root.is_dir() {
                anyhow::bail!(
                    "folder źródłowy nie istnieje lub nie jest katalogiem: {}",
                    dir.trim()
                );
            }
            gather_media_recursive(root, &mut sources)?;
        }
        None => {
            if !staging.is_dir() {
                anyhow::bail!("brak wgranych plików do zbudowania datasetu");
            }
            for entry in std::fs::read_dir(&staging)? {
                let path = entry?.path();
                if !path.is_file() {
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Android/photo apps leave soft-deleted shadows; never ingest them.
                if name.starts_with(".trashed-") {
                    continue;
                }
                sources.push(path);
            }
        }
    }
    sources.sort();
    if sources.len() > MAX_SOURCE_FILES {
        anyhow::bail!(
            "za dużo plików źródłowych ({}, limit {}) — podziel budowę na mniejsze partie",
            sources.len(),
            MAX_SOURCE_FILES
        );
    }
    let files_total = sources.len() as u64;
    update_progress(build_id, |p| p.files_total = files_total);

    // Build into a fresh temp dir alongside the final location; rename only on full
    // success so a crash/abort never leaves a half-written dataset (and a retry
    // never mixes in stale images).
    let coco_dir = dataset_dir(project_id, build_id);
    if let Some(parent) = coco_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dataset parent {}", parent.display()))?;
    }
    let tmp_dir = coco_dir.with_extension("building");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    let train_dir = tmp_dir.join("train");
    std::fs::create_dir_all(&train_dir)
        .with_context(|| format!("create train dir {}", train_dir.display()))?;

    let mut frames_total: u64 = 0;
    let mut done: u64 = 0;
    for src in &sources {
        if Instant::now() >= deadline {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            anyhow::bail!(
                "budowa przekroczyła limit czasu ({}h)",
                BUILD_TIMEOUT.as_secs() / 3600
            );
        }
        let produced = process_source(src, &train_dir, fps, MAX_TOTAL_FRAMES - frames_total)
            .with_context(|| format!("przetwarzanie pliku {}", src.display()))?;
        frames_total += produced;
        if frames_total > MAX_TOTAL_FRAMES {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            anyhow::bail!(
                "budowa wygenerowałaby ponad {} obrazów — zmniejsz fps lub liczbę plików",
                MAX_TOTAL_FRAMES
            );
        }
        done += 1;
        let frames_snapshot = frames_total;
        update_progress(build_id, |p| {
            p.files_done = done;
            p.frames_extracted = frames_snapshot;
        });
    }

    let image_count = write_coco_annotations(&train_dir)?;
    if image_count == 0 {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("po przetworzeniu nie powstał żaden obraz (sprawdź wgrane pliki)");
    }

    // Atomic publish: rename the fully-built temp dir into the final location.
    let _ = std::fs::remove_dir_all(&coco_dir);
    std::fs::rename(&tmp_dir, &coco_dir).with_context(|| {
        format!(
            "publish dataset {} -> {}",
            tmp_dir.display(),
            coco_dir.display()
        )
    })?;

    // Register the dataset row BEFORE clearing staging. If registration fails the
    // built files are removed and staging is left intact for a retry.
    let coco_path = coco_dir.to_string_lossy().to_string();
    let prof =
        crate::dispatch::ml_studio::profile_coco_dir(&coco_dir).with_context(|| "profil COCO")?;
    let profile_json = serde_json::to_string(&prof)?;
    let category_count = CATEGORIES.len() as u32;

    let dataset = match crate::ml_studio::repository::create_dataset(
        owner_user_id,
        project_id,
        dataset_name,
        "coco_path",
        image_count,
        category_count,
        &profile_json,
        coco_path.as_bytes(),
    ) {
        Ok(d) => d,
        Err(err) => {
            // Built files are orphaned without a DB row — remove them; staging stays.
            let _ = std::fs::remove_dir_all(&coco_dir);
            anyhow::bail!("rejestracja datasetu w bazie nieudana: {}", err);
        }
    };

    // Staging consumed only after the dataset is durably registered — and never
    // for a server-folder build, where the source is the user's own data.
    if use_staging {
        let _ = std::fs::remove_dir_all(&staging);
    }

    update_progress(build_id, |p| {
        p.status = "succeeded".to_string();
        p.image_count = image_count;
        p.category_count = category_count;
        p.dataset_id = Some(dataset.dataset_id.clone());
        p.files_done = files_total;
    });
    Ok(())
}

/// Media extensions accepted from a server source folder (lowercased match).
const SOURCE_DIR_EXTS: &[&str] = &["jpg", "jpeg", "png", "heic", "mp4", "mov"];

/// Recursively collects media files from a server source folder into `out`.
/// Skips any file whose name starts with `.trashed-` (photo-app soft-delete
/// shadows) and prunes any directory component named exactly `Archiwum`
/// (case-sensitive) so archived material is never ingested. Symlinks are not
/// followed: `read_dir` + `is_dir`/`is_file` traverse only real entries.
fn gather_media_recursive(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("read dir {}", dir.display()))?;
        for entry in entries {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                // Skip whole subtrees named exactly `Archiwum`.
                if name == "Archiwum" {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !path.is_file() {
                continue;
            }
            if name.starts_with(".trashed-") {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if SOURCE_DIR_EXTS.contains(&ext.as_str()) {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Dispatches a single staged file into `train_dir` by extension. Returns the
/// number of images produced (1 for an image, N frames for a video). `frame_budget`
/// caps how many frames this source may add (the remaining global allowance).
fn process_source(src: &Path, train_dir: &Path, fps: u32, frame_budget: u64) -> Result<u64> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("img")
        .to_string();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" => {
            if frame_budget == 0 {
                anyhow::bail!("przekroczono globalny limit obrazów");
            }
            let dest = unique_path(train_dir, &format!("{}.{}", stem, ext));
            std::fs::copy(src, &dest)
                .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
            Ok(1)
        }
        "heic" => {
            if frame_budget == 0 {
                anyhow::bail!("przekroczono globalny limit obrazów");
            }
            let dest = unique_path(train_dir, &format!("{}.jpg", stem));
            // Wrapped in OS `timeout` so a bad file can't hang the build.
            let status = std::process::Command::new("timeout")
                .arg(PER_FILE_TIMEOUT_SECS.to_string())
                .arg("heif-convert")
                .args(["-q", "90"])
                .arg(src)
                .arg(&dest)
                .status()
                .with_context(|| "uruchomienie heif-convert (czy zainstalowany?)")?;
            if !status.success() {
                if status.code() == Some(124) {
                    anyhow::bail!("heif-convert przekroczył limit czasu dla {}", src.display());
                }
                anyhow::bail!("heif-convert zwrócił błąd dla {}", src.display());
            }
            Ok(1)
        }
        "mp4" | "mov" => {
            // Per-video frame cap: min of the hard per-clip limit and the remaining
            // global budget. ffmpeg `-frames:v` bounds extraction server-side.
            let cap = MAX_FRAMES_PER_VIDEO.min(frame_budget);
            if cap == 0 {
                anyhow::bail!("przekroczono globalny limit obrazów");
            }
            // Pattern uses %04d → up to 9999 frames per clip; start at 1.
            let pattern = train_dir.join(format!("{}_f%04d.jpg", stem));
            // Wrapped in OS `timeout` so a corrupt clip can't hang the build.
            let status = std::process::Command::new("timeout")
                .arg(PER_FILE_TIMEOUT_SECS.to_string())
                .arg("ffmpeg")
                .arg("-y")
                .arg("-i")
                .arg(src)
                .args(["-vf", &format!("fps={}", fps)])
                .args(["-frames:v", &cap.to_string()])
                .args(["-q:v", "2"])
                .args(["-start_number", "1"])
                .arg(&pattern)
                .status()
                .with_context(|| "uruchomienie ffmpeg (czy zainstalowany?)")?;
            if !status.success() {
                if status.code() == Some(124) {
                    anyhow::bail!("ffmpeg przekroczył limit czasu dla {}", src.display());
                }
                anyhow::bail!("ffmpeg zwrócił błąd dla {}", src.display());
            }
            // Count the frames actually written for this clip's prefix.
            let prefix = format!("{}_f", stem);
            let mut produced = 0u64;
            for entry in std::fs::read_dir(train_dir)?.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(".jpg") {
                    produced += 1;
                }
            }
            Ok(produced)
        }
        other => {
            anyhow::bail!("nieobsługiwany typ pliku: .{}", other);
        }
    }
}

/// Writes `_annotations.coco.json` describing every jpg/png in `train_dir`, with
/// real image dimensions, the fixed 17 categories and an empty annotation list.
/// Returns the number of images recorded.
fn write_coco_annotations(train_dir: &Path) -> Result<u64> {
    let mut images = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(train_dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            files.push(path);
        }
    }
    files.sort();

    let mut next_id: i64 = 1;
    for path in &files {
        let (w, h) = image::image_dimensions(path)
            .with_context(|| format!("odczyt wymiarów obrazu {}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        images.push(serde_json::json!({
            "id": next_id,
            "file_name": file_name,
            "width": w,
            "height": h,
        }));
        next_id += 1;
    }

    let categories: Vec<serde_json::Value> = CATEGORIES
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            serde_json::json!({
                "id": (idx + 1) as i64,
                "name": name,
                "supercategory": "none",
            })
        })
        .collect();

    let coco = serde_json::json!({
        "categories": categories,
        "images": images,
        "annotations": [],
    });

    let annot_path = train_dir.join("_annotations.coco.json");
    std::fs::write(&annot_path, serde_json::to_vec_pretty(&coco)?)
        .with_context(|| format!("zapis {}", annot_path.display()))?;
    Ok(files.len() as u64)
}
