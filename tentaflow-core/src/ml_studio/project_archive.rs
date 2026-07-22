// ===== File: ml_studio/project_archive.rs — project export/import as a self-contained ZIP =====
//
// Moves a whole ML Studio project between nodes that are NOT in the same mesh: the
// mesh transfer path (`mesh_artifact.rs`) needs a paired peer, this one produces a
// single file the user copies by hand (USB, scp, S3) and imports on the far side.
//
// The archive is self-contained: DB rows AND the on-disk COCO datasets AND (opt-in)
// the trained model artifacts. Everything a project needs to be re-openable and
// re-trainable on a node that has never seen it.
//
// Archive layout:
//
//   manifest.json                      version, exported_at, source_node_id, project
//                                      meta, contents inventory, per-file sha256,
//                                      and the list of artifact dirs found MISSING
//   db/projects.json                   the single project row
//   db/datasets.json                   dataset rows; `raw_data` is REPLACED by a
//                                      relative archive path (see below)
//   db/schemas.json                    per-project class/attribute definitions
//   db/lookup_dicts.json               OCR lookup dictionaries referenced by schemas
//   db/models.json                     only when `include_models`
//   db/training_runs.json              only when `include_history`
//   db/metrics_history.json            only when `include_history`
//   datasets/<dataset_id>/<split>/...  images + `_annotations.coco.json`, VERBATIM
//   datasets/<dataset_id>/raw.bin      inline (blob) dataset payload
//   artifacts/<kind>/<run_id>/...      checkpoints/ONNX, only when `include_models`
//
// `datasets.raw_data` for `kind = "coco_path"` holds an ABSOLUTE filesystem path of
// the exporting node. That path is meaningless on the importing node, so the export
// rewrites it to the relative in-archive location and the import writes back a fresh
// ABSOLUTE path under the destination node's own `recog-datasets/` root.
//
// The COCO files carry non-standard fields that the annotation editor depends on
// (`approved` per image, `attributes`/`score`/`predicted` per annotation). They are
// copied byte-for-byte on export and field-for-field on merge — never normalized.
//
// Real projects reach ~3k images / 1.9 GB before model checkpoints, so both
// directions stream: entries are copied through a fixed-size buffer straight to/from
// the zip on disk, never buffered whole in RAM. Both directions also run as async
// background jobs with an in-memory progress map the UI polls, mirroring
// `autolabel_recog_dataset.rs`.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Archive format version. Bumped only on a breaking layout change; an import
/// refuses anything it does not understand rather than guessing.
pub const ARCHIVE_VERSION: u32 = 1;

const MANIFEST_ENTRY: &str = "manifest.json";
const COCO_FILE: &str = "_annotations.coco.json";

/// Artifact roots under `paths::ml_artifacts_dir()`; a training run may have a
/// directory below either of them depending on which trainer produced it.
const ARTIFACT_KINDS: [&str; 2] = ["recog", "classifier"];

// Import guards against a hostile/corrupt archive. The caps are generous enough for
// the largest real project (~3k images, 1.9 GB, plus multi-GB checkpoints) yet still
// bound the damage a zip bomb can do: a 32 GiB write budget and 400k entries.
const MAX_IMPORT_ENTRIES: usize = 400_000;
const MAX_IMPORT_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// The manifest is parsed before any cap on the rest of the archive is known, so it
/// gets its own bound (one entry with sha256 costs ~150 B; 64 MiB covers 400k files).
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
/// Copy buffer for zip <-> disk streaming. Constant RAM regardless of file size.
const COPY_BUF: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// One archived file with its size and content hash. The import verifies each hash
/// after extraction, so a truncated copy fails loudly instead of producing a project
/// with silently corrupt images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// An artifact directory the export expected (a training run exists in the DB) but
/// did not find on disk. Recorded so the archive never claims a completeness it does
/// not have; the import surfaces these in the preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingArtifact {
    pub kind: String,
    pub run_id: String,
    pub expected_path: String,
}

/// Per-dataset inventory, kept in the manifest so `preview` is cheap: it answers
/// "how many images / which classes" without scanning the whole archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetInventory {
    pub dataset_id: String,
    pub name: String,
    pub kind: String,
    pub image_count: u64,
    pub annotation_count: u64,
    pub category_names: Vec<String>,
    pub bytes: u64,
}

/// Project identity as it existed on the exporting node. `owner_user_id`/`org_id`
/// are informational only — the import always re-owns to the importing identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub project_type: String,
    pub owner_user_id: String,
    pub org_id: String,
}

/// What the archive actually contains, independent of what was requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveContents {
    pub datasets: Vec<DatasetInventory>,
    pub schema_count: u64,
    pub lookup_dict_count: u64,
    pub model_count: u64,
    pub training_run_count: u64,
    pub metric_count: u64,
    pub includes_models: bool,
    pub includes_history: bool,
}

/// Parsed `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub version: u32,
    pub exported_at: String,
    pub source_node_id: String,
    pub project: ProjectMeta,
    pub contents: ArchiveContents,
    pub files: Vec<FileEntry>,
    pub missing_artifacts: Vec<MissingArtifact>,
}

// ---------------------------------------------------------------------------
// Public options / results
// ---------------------------------------------------------------------------

/// Export knobs. Datasets, schemas and lookup dicts are always included — without
/// them the archive is not a project.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExportOptions {
    /// Include `models` rows and their on-disk artifact directories.
    pub include_models: bool,
    /// Include `training_runs` + `metrics_history` (the training curves).
    pub include_history: bool,
}

/// Outcome of a finished export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSummary {
    pub archive_path: String,
    pub archive_bytes: u64,
    pub dataset_count: u64,
    pub image_count: u64,
    pub annotation_count: u64,
    pub model_count: u64,
    pub missing_artifacts: Vec<MissingArtifact>,
}

/// What the user sees before committing to an import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPreview {
    pub version: u32,
    pub exported_at: String,
    pub source_node_id: String,
    pub project_name: String,
    pub project_type: String,
    pub datasets: Vec<DatasetInventory>,
    pub classes: Vec<String>,
    pub has_models: bool,
    pub has_history: bool,
    pub total_uncompressed_bytes: u64,
    pub missing_artifacts: Vec<MissingArtifact>,
}

/// Where the archive's content lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImportMode {
    /// Create a brand-new project owned by the importing user.
    NewProject { name_override: Option<String> },
    /// Append the archive's images/annotations into an existing COCO dataset.
    MergeInto {
        project_id: String,
        dataset_id: String,
    },
}

/// Outcome of a finished import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSummary {
    pub project_id: String,
    pub project_name: String,
    pub dataset_count: u64,
    pub images_imported: u64,
    pub images_skipped_duplicate: u64,
    pub annotations_imported: u64,
    pub models_imported: u64,
    pub training_runs_imported: u64,
    pub missing_artifacts: Vec<MissingArtifact>,
}

// ---------------------------------------------------------------------------
// Background job progress
// ---------------------------------------------------------------------------

/// Live progress of an export/import job, polled by the UI. `status` is
/// "running" | "succeeded" | "failed". `project_id`/`owner_user_id` are stored so the
/// status handler can authorize the caller — a bare job id must not expose progress
/// to an unrelated user. For a `NewProject` import `project_id` stays empty until the
/// new project is registered.
#[derive(Clone, Debug, Default)]
pub struct ArchiveJobProgress {
    pub status: String,
    /// Coarse stage: "collecting" | "writing" | "extracting" | "verifying" |
    /// "registering".
    pub phase: String,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub project_id: String,
    pub owner_user_id: String,
    pub error: Option<String>,
}

impl ArchiveJobProgress {
    fn started(phase: &str, project_id: &str, owner_user_id: &str) -> Self {
        ArchiveJobProgress {
            status: "running".to_string(),
            phase: phase.to_string(),
            project_id: project_id.to_string(),
            owner_user_id: owner_user_id.to_string(),
            ..ArchiveJobProgress::default()
        }
    }
}

static PROGRESS: OnceLock<Mutex<HashMap<String, ArchiveJobProgress>>> = OnceLock::new();

fn progress_map() -> &'static Mutex<HashMap<String, ArchiveJobProgress>> {
    PROGRESS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_progress(job_id: &str, p: ArchiveJobProgress) {
    if let Ok(mut m) = progress_map().lock() {
        m.insert(job_id.to_string(), p);
    }
}

fn update_progress(job_id: &str, f: impl FnOnce(&mut ArchiveJobProgress)) {
    if let Ok(mut m) = progress_map().lock() {
        if let Some(p) = m.get_mut(job_id) {
            f(p);
        }
    }
}

/// Current progress of an export or import job (None when the job id is unknown).
pub fn job_progress(job_id: &str) -> Option<ArchiveJobProgress> {
    progress_map().lock().ok()?.get(job_id).cloned()
}

// Serializes jobs that would otherwise race on the same resource: one export per
// project, one import per destination (target dataset, or source archive path).
static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn active() -> &'static Mutex<HashSet<String>> {
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn try_claim(key: &str) -> bool {
    match active().lock() {
        Ok(mut s) => s.insert(key.to_string()),
        Err(_) => false,
    }
}

fn release(key: &str) {
    if let Ok(mut s) = active().lock() {
        s.remove(key);
    }
}

// ---------------------------------------------------------------------------
// Serialized DB rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectRow {
    project_id: String,
    name: String,
    description: String,
    project_type: String,
    status: String,
    owner_user_id: String,
    org_id: String,
    created_at: String,
    updated_at: String,
}

/// How a dataset's payload was archived. `raw_data` is never carried inline in the
/// JSON — it is either a directory tree or a blob file inside the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum RawDataKind {
    /// `datasets/<id>/` holds the COCO directory; `raw_data` is a path on import.
    CocoDir,
    /// `datasets/<id>/raw.bin` holds the original blob bytes.
    Blob,
    /// The row had no `raw_data`.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DatasetRow {
    dataset_id: String,
    project_id: String,
    name: String,
    kind: String,
    row_count: i64,
    column_count: i64,
    profile_json: String,
    created_at: String,
    raw_data_kind: RawDataKind,
    /// Relative in-archive location of the payload; empty for `RawDataKind::None`.
    raw_data_archive_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchemaRow {
    schema_id: String,
    project_id: String,
    json: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LookupDictRow {
    dict_id: String,
    project_id: String,
    name: String,
    rows_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelRow {
    model_id: String,
    project_id: String,
    name: String,
    framework: String,
    base_model: String,
    metrics_json: String,
    status: String,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrainingRunRow {
    run_id: String,
    project_id: String,
    model_id: Option<String>,
    status: String,
    config_json: String,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetricRow {
    run_id: String,
    step: i64,
    metric_key: String,
    metric_value: f64,
    ts: String,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Root of the destination node's COCO datasets, mirroring the layout
/// `build_recog_dataset` writes to (`<data>/ml-studio/recog-datasets/<proj>/<build>`).
fn recog_dataset_dir(project_id: &str, build_id: &str) -> PathBuf {
    crate::paths::data_dir()
        .join("ml-studio")
        .join("recog-datasets")
        .join(sanitize_component(project_id))
        .join(sanitize_component(build_id))
}

/// On-disk artifact directory of one training run.
fn artifact_dir(kind: &str, project_id: &str, run_id: &str) -> PathBuf {
    crate::paths::ml_artifacts_dir()
        .join(sanitize_component(kind))
        .join(sanitize_component(project_id))
        .join(sanitize_component(run_id))
}

/// Keeps a user-supplied id/name safe as a single path component (no traversal, no
/// separators). Same rule as `build_recog_dataset::sanitize_component`.
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

/// Recursively lists files under `root`, returning `(relative_slash_path, abs_path)`
/// sorted by relative path so two exports of an unchanged tree produce byte-identical
/// entry ordering.
fn walk_sorted(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    fn rec(root: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(cur)
            .with_context(|| format!("odczyt katalogu {}", cur.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        entries.sort();
        for p in entries {
            // Symlinks are not followed: an exported archive must describe real bytes
            // under the dataset root, not wander outside it.
            let meta = std::fs::symlink_metadata(&p)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                rec(root, &p, out)?;
            } else if meta.is_file() {
                let rel = p
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_string();
                out.push((rel, p));
            }
        }
        Ok(())
    }
    rec(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("otwarcie {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Starts an async export of `project_id` into `dest_zip`. Returns the job id used
/// for progress polling. Rejected when an export of the same project is already
/// running or the caller is not a member of the project.
pub fn spawn_export(
    project_id: String,
    owner_user_id: String,
    opts: ExportOptions,
    dest_zip: PathBuf,
) -> Result<String> {
    if super::repository::member_role(&project_id, &owner_user_id)?.is_none() {
        bail!("brak dostępu do projektu");
    }
    let key = format!("export:{project_id}");
    if !try_claim(&key) {
        bail!("eksport tego projektu już trwa — poczekaj na jego zakończenie");
    }

    let job_id = uuid::Uuid::new_v4().to_string();
    set_progress(
        &job_id,
        ArchiveJobProgress::started("collecting", &project_id, &owner_user_id),
    );

    let job_task = job_id.clone();
    tokio::spawn(async move {
        let jid = job_task.clone();
        // Hashing and copying gigabytes is blocking work — keep it off the async worker.
        let result = tokio::task::spawn_blocking(move || {
            run_export(&job_task, &project_id, opts, &dest_zip)
        })
        .await;
        match result {
            Ok(Ok(summary)) => {
                update_progress(&jid, |p| {
                    p.status = "succeeded".to_string();
                    p.phase = "writing".to_string();
                    p.files_done = p.files_total;
                    p.bytes_done = summary.archive_bytes;
                });
            }
            Ok(Err(err)) => {
                tracing::warn!(job_id = %jid, error = %err, "ml studio project export failed");
                update_progress(&jid, |p| {
                    p.status = "failed".to_string();
                    p.error = Some(err.to_string());
                });
            }
            Err(join_err) => {
                tracing::warn!(job_id = %jid, error = %join_err, "ml studio project export panicked");
                update_progress(&jid, |p| {
                    p.status = "failed".to_string();
                    p.error = Some(format!("export task failed: {}", join_err));
                });
            }
        }
        release(&key);
    });

    Ok(job_id)
}

fn run_export(
    job_id: &str,
    project_id: &str,
    opts: ExportOptions,
    dest_zip: &Path,
) -> Result<ExportSummary> {
    export_into(Some(job_id), project_id, opts, dest_zip)
}

/// Writes a self-contained archive of `project_id` to `dest_zip`. Authorization is
/// the caller's job (`spawn_export` checks membership).
///
/// The archive is built into `<dest_zip>.tmp` and renamed into place only after the
/// central directory is flushed, so an interrupted export never leaves a truncated
/// file that still looks like a valid zip.
pub fn build_export(
    project_id: &str,
    opts: ExportOptions,
    dest_zip: &Path,
) -> Result<ExportSummary> {
    export_into(None, project_id, opts, dest_zip)
}

fn export_into(
    job_id: Option<&str>,
    project_id: &str,
    opts: ExportOptions,
    dest_zip: &Path,
) -> Result<ExportSummary> {
    let project = load_project_row(project_id)?
        .ok_or_else(|| anyhow::anyhow!("projekt {project_id} nie istnieje"))?;
    let datasets = load_dataset_rows(project_id)?;
    let schemas = load_schema_rows(project_id)?;
    let dicts = load_lookup_dict_rows(project_id)?;
    let models = if opts.include_models {
        load_model_rows(project_id)?
    } else {
        Vec::new()
    };
    let runs = if opts.include_history || opts.include_models {
        // Model artifacts live under a RUN id, so exporting models without their runs
        // would archive checkpoint directories nothing could ever point back at.
        load_training_run_rows(project_id)?
    } else {
        Vec::new()
    };
    let metrics = if opts.include_history {
        load_metric_rows(&runs)?
    } else {
        Vec::new()
    };

    if let Some(parent) = dest_zip.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = tmp_sibling(dest_zip);
    if tmp_path.exists() {
        std::fs::remove_file(&tmp_path)?;
    }
    let file = std::fs::File::create(&tmp_path)
        .with_context(|| format!("utworzenie {}", tmp_path.display()))?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));

    let mut files: Vec<FileEntry> = Vec::new();
    let mut inventory: Vec<DatasetInventory> = Vec::new();
    let mut missing: Vec<MissingArtifact> = Vec::new();
    let mut total_images: u64 = 0;
    let mut total_annotations: u64 = 0;

    // Datasets first: they are the bulk of the work and drive the progress bar.
    let mut archived_datasets: Vec<DatasetRow> = Vec::new();
    if let Some(jid) = job_id {
        // Pre-count so the UI shows a real denominator from the first tick instead of a
        // bar that grows its own total as it goes.
        let mut planned_files: u64 = 0;
        let mut planned_bytes: u64 = 0;
        for ds in &datasets {
            if let Some(dir) = ds.source_dir.as_ref() {
                for (_, abs) in walk_sorted(dir)? {
                    planned_files += 1;
                    planned_bytes += std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        update_progress(jid, |p| {
            p.phase = "writing".to_string();
            p.files_total = planned_files;
            p.bytes_total = planned_bytes;
        });
    }

    for ds in &datasets {
        let mut row = ds.row.clone();
        let mut bytes = 0u64;
        let mut image_count = 0u64;
        let mut annotation_count = 0u64;
        let mut category_names: Vec<String> = Vec::new();

        match ds.kind_of_payload() {
            RawDataKind::CocoDir => {
                let dir = ds.source_dir.as_ref().context(
                    "wewnętrzny błąd eksportu: zbiór rozpoznany jako katalog COCO nie ma ścieżki źródłowej",
                )?;
                let prefix = format!("datasets/{}", ds.row.dataset_id);
                for (rel, abs) in walk_sorted(dir)? {
                    let arch = format!("{prefix}/{rel}");
                    let entry = write_stored_file(&mut zip, &arch, &abs)?;
                    bytes += entry.size;
                    if rel.ends_with(COCO_FILE) {
                        let (imgs, anns, cats) = coco_counts(&abs)?;
                        image_count += imgs;
                        annotation_count += anns;
                        for c in cats {
                            if !category_names.contains(&c) {
                                category_names.push(c);
                            }
                        }
                    }
                    files.push(entry);
                }
                row.raw_data_kind = RawDataKind::CocoDir;
                row.raw_data_archive_path = prefix;
            }
            RawDataKind::Blob => {
                let blob = ds.raw_blob.as_ref().context(
                    "wewnętrzny błąd eksportu: zbiór rozpoznany jako blob nie ma danych",
                )?;
                let arch = format!("datasets/{}/raw.bin", ds.row.dataset_id);
                let entry = write_bytes(&mut zip, &arch, blob, false)?;
                bytes += entry.size;
                files.push(entry);
                row.raw_data_kind = RawDataKind::Blob;
                row.raw_data_archive_path = arch;
            }
            RawDataKind::None => {
                row.raw_data_kind = RawDataKind::None;
                row.raw_data_archive_path = String::new();
            }
        }

        total_images += image_count;
        total_annotations += annotation_count;
        inventory.push(DatasetInventory {
            dataset_id: row.dataset_id.clone(),
            name: row.name.clone(),
            kind: row.kind.clone(),
            image_count,
            annotation_count,
            category_names,
            bytes,
        });
        archived_datasets.push(row);

        if let Some(jid) = job_id {
            let done = files.len() as u64;
            let bytes: u64 = files.iter().map(|f| f.size).sum();
            update_progress(jid, |p| {
                p.files_done = done;
                p.bytes_done = bytes;
            });
        }
    }

    // Model artifacts. A run whose directory is gone (pruned cache, moved disk) is
    // recorded as missing rather than silently omitted.
    if opts.include_models {
        for run in &runs {
            for kind in ARTIFACT_KINDS {
                let dir = artifact_dir(kind, project_id, &run.run_id);
                if !dir.is_dir() {
                    continue;
                }
                let entries = walk_sorted(&dir)?;
                if entries.is_empty() {
                    missing.push(MissingArtifact {
                        kind: kind.to_string(),
                        run_id: run.run_id.clone(),
                        expected_path: dir.to_string_lossy().to_string(),
                    });
                    continue;
                }
                for (rel, abs) in entries {
                    let arch = format!("artifacts/{kind}/{}/{rel}", run.run_id);
                    let entry = write_stored_file(&mut zip, &arch, &abs)?;
                    files.push(entry);
                }
            }
            // A finished run that produced a model but has no artifact dir under any
            // known kind cannot be re-served after import — say so in the manifest.
            let has_any = ARTIFACT_KINDS
                .iter()
                .any(|k| artifact_dir(k, project_id, &run.run_id).is_dir());
            if !has_any && run.model_id.is_some() {
                missing.push(MissingArtifact {
                    kind: "recog".to_string(),
                    run_id: run.run_id.clone(),
                    expected_path: artifact_dir("recog", project_id, &run.run_id)
                        .to_string_lossy()
                        .to_string(),
                });
            }
        }
        if let Some(jid) = job_id {
            let done = files.len() as u64;
            update_progress(jid, |p| p.files_done = done);
        }
    }

    // DB rows last (before the manifest) so their JSON already reflects the rewritten
    // dataset paths decided above.
    files.push(write_json(&mut zip, "db/projects.json", &vec![&project])?);
    files.push(write_json(&mut zip, "db/datasets.json", &archived_datasets)?);
    files.push(write_json(&mut zip, "db/schemas.json", &schemas)?);
    files.push(write_json(&mut zip, "db/lookup_dicts.json", &dicts)?);
    if opts.include_models {
        files.push(write_json(&mut zip, "db/models.json", &models)?);
    }
    if opts.include_history || opts.include_models {
        files.push(write_json(&mut zip, "db/training_runs.json", &runs)?);
    }
    if opts.include_history {
        files.push(write_json(&mut zip, "db/metrics_history.json", &metrics)?);
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = ArchiveManifest {
        version: ARCHIVE_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        source_node_id: crate::sync::runtime::local_node_id().unwrap_or_default(),
        project: ProjectMeta {
            project_id: project.project_id.clone(),
            name: project.name.clone(),
            description: project.description.clone(),
            project_type: project.project_type.clone(),
            owner_user_id: project.owner_user_id.clone(),
            org_id: project.org_id.clone(),
        },
        contents: ArchiveContents {
            datasets: inventory,
            schema_count: schemas.len() as u64,
            lookup_dict_count: dicts.len() as u64,
            model_count: models.len() as u64,
            training_run_count: runs.len() as u64,
            metric_count: metrics.len() as u64,
            includes_models: opts.include_models,
            includes_history: opts.include_history,
        },
        files,
        missing_artifacts: missing.clone(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    write_bytes(&mut zip, MANIFEST_ENTRY, &manifest_bytes, true)?;

    let mut inner = zip.finish()?;
    inner.flush()?;
    drop(inner);

    std::fs::rename(&tmp_path, dest_zip)
        .with_context(|| format!("publikacja {}", dest_zip.display()))?;
    let archive_bytes = std::fs::metadata(dest_zip)?.len();

    Ok(ExportSummary {
        archive_path: dest_zip.to_string_lossy().to_string(),
        archive_bytes,
        dataset_count: archived_datasets.len() as u64,
        image_count: total_images,
        annotation_count: total_annotations,
        model_count: models.len() as u64,
        missing_artifacts: missing,
    })
}

fn tmp_sibling(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive.zip".to_string());
    dest.with_file_name(format!("{name}.tmp"))
}

type ZipOut = zip::ZipWriter<std::io::BufWriter<std::fs::File>>;

fn stored_options() -> zip::write::FileOptions<'static, ()> {
    // Datasets are JPEG/PNG and checkpoints are already-compressed tensors: deflating
    // them burns CPU for ~0% gain on multi-GB archives. `large_file` emits zip64
    // headers so a >4 GiB archive stays readable.
    zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true)
}

fn deflated_options() -> zip::write::FileOptions<'static, ()> {
    zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated)
}

/// Streams `src` into the archive through a fixed buffer, hashing as it goes so the
/// file is read exactly once regardless of size.
fn write_stored_file(zip: &mut ZipOut, arch_path: &str, src: &Path) -> Result<FileEntry> {
    zip.start_file(arch_path.to_string(), stored_options())?;
    let mut f =
        std::fs::File::open(src).with_context(|| format!("otwarcie {}", src.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        zip.write_all(&buf[..n])?;
        size += n as u64;
    }
    Ok(FileEntry {
        path: arch_path.to_string(),
        size,
        sha256: hex(&hasher.finalize()),
    })
}

fn write_bytes(
    zip: &mut ZipOut,
    arch_path: &str,
    bytes: &[u8],
    deflate: bool,
) -> Result<FileEntry> {
    let opts = if deflate {
        deflated_options()
    } else {
        stored_options()
    };
    zip.start_file(arch_path.to_string(), opts)?;
    zip.write_all(bytes)?;
    Ok(FileEntry {
        path: arch_path.to_string(),
        size: bytes.len() as u64,
        sha256: sha256_bytes(bytes),
    })
}

fn write_json<T: Serialize>(zip: &mut ZipOut, arch_path: &str, value: &T) -> Result<FileEntry> {
    let bytes = serde_json::to_vec(value)?;
    write_bytes(zip, arch_path, &bytes, true)
}

/// Image count, annotation count and category names of one COCO file.
fn coco_counts(path: &Path) -> Result<(u64, u64, Vec<String>)> {
    let buf = std::fs::read(path).with_context(|| format!("odczyt {}", path.display()))?;
    let coco: Value = serde_json::from_slice(&buf)
        .with_context(|| format!("parsowanie {}", path.display()))?;
    let images = coco
        .get("images")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0);
    let anns = coco
        .get("annotations")
        .and_then(|v| v.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0);
    let cats = coco
        .get("categories")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok((images, anns, cats))
}

// ---------------------------------------------------------------------------
// Export: DB reads
// ---------------------------------------------------------------------------

/// A dataset row plus the payload the export must physically archive.
struct DatasetSource {
    row: DatasetRow,
    /// Resolved COCO directory for `kind = "coco_path"` when it exists on disk.
    source_dir: Option<PathBuf>,
    /// Inline payload for every other kind.
    raw_blob: Option<Vec<u8>>,
}

impl DatasetSource {
    fn kind_of_payload(&self) -> RawDataKind {
        if self.source_dir.is_some() {
            RawDataKind::CocoDir
        } else if self.raw_blob.as_ref().map(|b| !b.is_empty()).unwrap_or(false) {
            RawDataKind::Blob
        } else {
            RawDataKind::None
        }
    }
}

fn load_project_row(project_id: &str) -> Result<Option<ProjectRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    conn.query_row(
        "SELECT project_id, name, description, project_type, status, owner_user_id, org_id, \
                created_at, updated_at \
         FROM projects WHERE project_id = ?1",
        params![project_id],
        |r| {
            Ok(ProjectRow {
                project_id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                project_type: r.get(3)?,
                status: r.get(4)?,
                owner_user_id: r.get(5)?,
                org_id: r.get(6)?,
                created_at: r.get(7)?,
                updated_at: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_dataset_rows(project_id: &str) -> Result<Vec<DatasetSource>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT dataset_id, project_id, name, kind, row_count, column_count, profile_json, \
                created_at, raw_data \
         FROM datasets WHERE project_id = ?1 ORDER BY dataset_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        let raw: Option<Vec<u8>> = r.get(8)?;
        Ok((
            DatasetRow {
                dataset_id: r.get(0)?,
                project_id: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                row_count: r.get(4)?,
                column_count: r.get(5)?,
                profile_json: r.get(6)?,
                created_at: r.get(7)?,
                raw_data_kind: RawDataKind::None,
                raw_data_archive_path: String::new(),
            },
            raw,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (row, raw) = row?;
        if row.kind == "coco_path" {
            let path = raw
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).to_string())
                .unwrap_or_default();
            let dir = PathBuf::from(path.trim());
            let source_dir = if !path.trim().is_empty() && dir.is_dir() {
                Some(dir)
            } else {
                None
            };
            if source_dir.is_none() {
                tracing::warn!(
                    dataset_id = %row.dataset_id,
                    "coco_path dataset directory missing on disk — exported without files"
                );
            }
            out.push(DatasetSource {
                row,
                source_dir,
                raw_blob: None,
            });
        } else {
            out.push(DatasetSource {
                row,
                source_dir: None,
                raw_blob: raw,
            });
        }
    }
    Ok(out)
}

fn load_schema_rows(project_id: &str) -> Result<Vec<SchemaRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT schema_id, project_id, json, updated_at FROM schemas \
         WHERE project_id = ?1 ORDER BY schema_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(SchemaRow {
            schema_id: r.get(0)?,
            project_id: r.get(1)?,
            json: r.get(2)?,
            updated_at: r.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_lookup_dict_rows(project_id: &str) -> Result<Vec<LookupDictRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT dict_id, project_id, name, rows_json FROM lookup_dicts \
         WHERE project_id = ?1 ORDER BY dict_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(LookupDictRow {
            dict_id: r.get(0)?,
            project_id: r.get(1)?,
            name: r.get(2)?,
            rows_json: r.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_model_rows(project_id: &str) -> Result<Vec<ModelRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT model_id, project_id, name, framework, base_model, metrics_json, status, created_at \
         FROM models WHERE project_id = ?1 ORDER BY model_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(ModelRow {
            model_id: r.get(0)?,
            project_id: r.get(1)?,
            name: r.get(2)?,
            framework: r.get(3)?,
            base_model: r.get(4)?,
            metrics_json: r.get(5)?,
            status: r.get(6)?,
            created_at: r.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_training_run_rows(project_id: &str) -> Result<Vec<TrainingRunRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT run_id, project_id, model_id, status, config_json, started_at, finished_at \
         FROM training_runs WHERE project_id = ?1 ORDER BY run_id",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(TrainingRunRow {
            run_id: r.get(0)?,
            project_id: r.get(1)?,
            model_id: r.get(2)?,
            status: r.get(3)?,
            config_json: r.get(4)?,
            started_at: r.get(5)?,
            finished_at: r.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_metric_rows(runs: &[TrainingRunRow]) -> Result<Vec<MetricRow>> {
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT run_id, step, metric_key, metric_value, ts FROM metrics_history \
         WHERE run_id = ?1 ORDER BY id",
    )?;
    let mut out = Vec::new();
    for run in runs {
        let rows = stmt.query_map(params![run.run_id], |r| {
            Ok(MetricRow {
                run_id: r.get(0)?,
                step: r.get(1)?,
                metric_key: r.get(2)?,
                metric_value: r.get(3)?,
                ts: r.get(4)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Import: manifest + preview
// ---------------------------------------------------------------------------

/// Reads and validates only `manifest.json`. Cheap: no dataset entry is touched.
pub fn read_manifest(zip_path: &Path) -> Result<ArchiveManifest> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("otwarcie archiwum {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| anyhow::anyhow!("plik nie jest poprawnym archiwum ZIP: {e}"))?;
    let manifest = read_manifest_from(&mut archive)?;
    Ok(manifest)
}

fn read_manifest_from<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<ArchiveManifest> {
    let mut entry = archive
        .by_name(MANIFEST_ENTRY)
        .map_err(|_| anyhow::anyhow!("archiwum bez {MANIFEST_ENTRY} — to nie jest eksport projektu ML Studio"))?;
    if entry.size() > MAX_MANIFEST_BYTES {
        bail!("manifest przekracza dopuszczalny rozmiar {MAX_MANIFEST_BYTES} B");
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    drop(entry);

    let manifest: ArchiveManifest =
        serde_json::from_slice(&buf).context("parsowanie manifest.json")?;
    if manifest.version != ARCHIVE_VERSION {
        bail!(
            "nieobsługiwana wersja archiwum {} (ta instancja obsługuje {})",
            manifest.version,
            ARCHIVE_VERSION
        );
    }
    Ok(manifest)
}

/// Human-facing summary shown before the user commits to an import.
pub fn preview(zip_path: &Path) -> Result<ImportPreview> {
    let m = read_manifest(zip_path)?;
    let mut classes: Vec<String> = Vec::new();
    for ds in &m.contents.datasets {
        for c in &ds.category_names {
            if !classes.contains(c) {
                classes.push(c.clone());
            }
        }
    }
    classes.sort();
    Ok(ImportPreview {
        version: m.version,
        exported_at: m.exported_at,
        source_node_id: m.source_node_id,
        project_name: m.project.name,
        project_type: m.project.project_type,
        datasets: m.contents.datasets,
        classes,
        has_models: m.contents.includes_models && m.contents.model_count > 0,
        has_history: m.contents.includes_history,
        total_uncompressed_bytes: m.files.iter().map(|f| f.size).sum(),
        missing_artifacts: m.missing_artifacts,
    })
}

// ---------------------------------------------------------------------------
// Import: extraction
// ---------------------------------------------------------------------------

/// Validated, contained relative path of an archive entry. Rejects absolute paths,
/// `..` traversal and symlink entries — a hostile archive must not be able to write
/// a single byte outside the staging directory.
fn safe_entry_path<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<Option<PathBuf>> {
    // Unix mode 0xA000 is S_IFLNK: a symlink entry's "content" is a target path, and
    // materializing it would let a later entry write through it outside the staging dir.
    if let Some(mode) = entry.unix_mode() {
        if mode & 0xF000 == 0xA000 {
            bail!("archiwum zawiera dowiązanie symboliczne: {}", entry.name());
        }
    }
    let name = entry.name().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    // `enclosed_name` is the containment primitive: it returns None for absolute
    // paths, `..` components and Windows drive/UNC prefixes.
    let rel = entry
        .enclosed_name()
        .ok_or_else(|| anyhow::anyhow!("niebezpieczna ścieżka w archiwum: {name}"))?;
    if rel.components().count() == 0 {
        return Ok(None);
    }
    Ok(Some(rel))
}

/// Extracts the whole archive into `staging`, enforcing the entry-count and total
/// uncompressed-size caps and verifying every file's sha256 against the manifest.
fn extract_all(
    zip_path: &Path,
    staging: &Path,
    manifest: &ArchiveManifest,
    job_id: Option<&str>,
) -> Result<()> {
    let expected: HashMap<&str, &FileEntry> =
        manifest.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let total_bytes: u64 = manifest.files.iter().map(|f| f.size).sum();
    if total_bytes > MAX_IMPORT_BYTES {
        bail!(
            "archiwum deklaruje {total_bytes} B po rozpakowaniu — limit to {MAX_IMPORT_BYTES} B"
        );
    }
    if let Some(jid) = job_id {
        update_progress(jid, |p| {
            p.phase = "extracting".to_string();
            p.files_total = manifest.files.len() as u64;
            p.bytes_total = total_bytes;
        });
    }

    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| anyhow::anyhow!("plik nie jest poprawnym archiwum ZIP: {e}"))?;
    if archive.len() > MAX_IMPORT_ENTRIES {
        bail!(
            "archiwum ma {} wpisów — limit to {MAX_IMPORT_ENTRIES}",
            archive.len()
        );
    }

    let mut written: u64 = 0;
    let mut done: u64 = 0;
    let mut buf = vec![0u8; COPY_BUF];
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = safe_entry_path(&entry)? else {
            continue;
        };
        let out_path = staging.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path)?);
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            written += n as u64;
            // The declared total is checked up front, but a lying manifest must not be
            // able to make us write past the cap either.
            if written > MAX_IMPORT_BYTES {
                bail!("rozpakowane dane przekroczyły limit {MAX_IMPORT_BYTES} B");
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            size += n as u64;
        }
        out.flush()?;
        drop(out);

        if rel_str != MANIFEST_ENTRY {
            let Some(want) = expected.get(rel_str.as_str()) else {
                bail!("wpis {rel_str} nie występuje w manifescie");
            };
            let got = hex(&hasher.finalize());
            if got != want.sha256 || size != want.size {
                bail!("uszkodzony wpis {rel_str}: niezgodna suma kontrolna");
            }
        }

        done += 1;
        if let Some(jid) = job_id {
            update_progress(jid, |p| {
                p.files_done = done;
                p.bytes_done = written;
            });
        }
    }

    // Every manifest entry must have been present: a stripped archive would otherwise
    // import as a project with silently missing images.
    for f in &manifest.files {
        if !staging.join(&f.path).is_file() {
            bail!("archiwum niekompletne — brak {}", f.path);
        }
    }
    Ok(())
}

fn read_db_json<T: for<'de> Deserialize<'de>>(staging: &Path, name: &str) -> Result<Vec<T>> {
    let path = staging.join("db").join(name);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let buf = std::fs::read(&path)?;
    serde_json::from_slice(&buf).with_context(|| format!("parsowanie db/{name}"))
}

// ---------------------------------------------------------------------------
// Import: public entry points
// ---------------------------------------------------------------------------

/// Starts an async import. Returns the job id used for progress polling.
pub fn spawn_import(
    zip_path: PathBuf,
    mode: ImportMode,
    owner_user_id: String,
    org_id: String,
) -> Result<String> {
    let (key, target_project) = match &mode {
        ImportMode::NewProject { .. } => {
            (format!("import:{}", zip_path.display()), String::new())
        }
        ImportMode::MergeInto {
            project_id,
            dataset_id,
        } => {
            if super::repository::member_role(project_id, &owner_user_id)?.is_none() {
                bail!("brak dostępu do projektu docelowego");
            }
            (
                format!("import:{project_id}:{dataset_id}"),
                project_id.clone(),
            )
        }
    };
    if !try_claim(&key) {
        bail!("import do tego celu już trwa — poczekaj na jego zakończenie");
    }

    let job_id = uuid::Uuid::new_v4().to_string();
    set_progress(
        &job_id,
        ArchiveJobProgress::started("extracting", &target_project, &owner_user_id),
    );

    let job_task = job_id.clone();
    tokio::spawn(async move {
        let jid = job_task.clone();
        let result = tokio::task::spawn_blocking(move || {
            run_import(Some(&job_task), &zip_path, mode, &owner_user_id, &org_id)
        })
        .await;
        match result {
            Ok(Ok(summary)) => {
                update_progress(&jid, |p| {
                    p.status = "succeeded".to_string();
                    p.phase = "registering".to_string();
                    p.project_id = summary.project_id.clone();
                    p.files_done = p.files_total;
                });
            }
            Ok(Err(err)) => {
                tracing::warn!(job_id = %jid, error = %err, "ml studio project import failed");
                update_progress(&jid, |p| {
                    p.status = "failed".to_string();
                    p.error = Some(err.to_string());
                });
            }
            Err(join_err) => {
                tracing::warn!(job_id = %jid, error = %join_err, "ml studio project import panicked");
                update_progress(&jid, |p| {
                    p.status = "failed".to_string();
                    p.error = Some(format!("import task failed: {}", join_err));
                });
            }
        }
        release(&key);
    });

    Ok(job_id)
}

/// Imports an archive produced by `build_export`. Authorization is the caller's job
/// (`spawn_import` checks membership for the merge target).
///
/// Files are always extracted into a staging directory on the destination filesystem
/// and moved into place BEFORE any DB row is written, so a failed import never leaves
/// rows pointing at a directory that does not exist.
pub fn import_archive(
    zip_path: &Path,
    mode: ImportMode,
    owner_user_id: &str,
    org_id: &str,
) -> Result<ImportSummary> {
    run_import(None, zip_path, mode, owner_user_id, org_id)
}

fn run_import(
    job_id: Option<&str>,
    zip_path: &Path,
    mode: ImportMode,
    owner_user_id: &str,
    org_id: &str,
) -> Result<ImportSummary> {
    let manifest = read_manifest(zip_path)?;

    // Staging lives under the datasets root so the later rename into place is a
    // same-filesystem move (an atomic publish, not a multi-GB copy).
    let staging_root = crate::paths::data_dir().join("ml-studio").join(".import");
    std::fs::create_dir_all(&staging_root)?;
    let staging = staging_root.join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&staging)?;

    let outcome = (|| -> Result<ImportSummary> {
        extract_all(zip_path, &staging, &manifest, job_id)?;
        if let Some(jid) = job_id {
            update_progress(jid, |p| p.phase = "registering".to_string());
        }
        match mode {
            ImportMode::NewProject { name_override } => import_as_new_project(
                &staging,
                &manifest,
                name_override.as_deref(),
                owner_user_id,
                org_id,
            ),
            ImportMode::MergeInto {
                project_id,
                dataset_id,
            } => merge_into_dataset(&staging, &manifest, &project_id, &dataset_id),
        }
    })();

    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

// ---------------------------------------------------------------------------
// Import: new project
// ---------------------------------------------------------------------------

fn import_as_new_project(
    staging: &Path,
    manifest: &ArchiveManifest,
    name_override: Option<&str>,
    owner_user_id: &str,
    org_id: &str,
) -> Result<ImportSummary> {
    let datasets: Vec<DatasetRow> = read_db_json(staging, "datasets.json")?;
    let schemas: Vec<SchemaRow> = read_db_json(staging, "schemas.json")?;
    let dicts: Vec<LookupDictRow> = read_db_json(staging, "lookup_dicts.json")?;
    let models: Vec<ModelRow> = read_db_json(staging, "models.json")?;
    let runs: Vec<TrainingRunRow> = read_db_json(staging, "training_runs.json")?;
    let metrics: Vec<MetricRow> = read_db_json(staging, "metrics_history.json")?;

    let new_project_id = uuid::Uuid::new_v4().to_string();
    let wanted = name_override
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| manifest.project.name.clone());
    let name = unique_project_name(org_id, &wanted)?;

    // Id remaps. Every incoming id is replaced: reusing the source ids would collide
    // with an earlier import of the same archive on this node.
    let mut dataset_ids: HashMap<String, String> = HashMap::new();
    let mut dict_ids: HashMap<String, String> = HashMap::new();
    let mut model_ids: HashMap<String, String> = HashMap::new();
    let mut run_ids: HashMap<String, String> = HashMap::new();
    for d in &datasets {
        dataset_ids.insert(d.dataset_id.clone(), uuid::Uuid::new_v4().to_string());
    }
    for d in &dicts {
        dict_ids.insert(d.dict_id.clone(), uuid::Uuid::new_v4().to_string());
    }
    for m in &models {
        model_ids.insert(m.model_id.clone(), uuid::Uuid::new_v4().to_string());
    }
    for r in &runs {
        run_ids.insert(r.run_id.clone(), uuid::Uuid::new_v4().to_string());
    }

    // Move dataset directories into place first. `raw_data` for a COCO dataset is the
    // ABSOLUTE destination path — the source node's path is meaningless here.
    let mut dataset_paths: HashMap<String, Vec<u8>> = HashMap::new();
    let mut moved_dirs: Vec<PathBuf> = Vec::new();
    let mut images_imported: u64 = 0;
    let mut annotations_imported: u64 = 0;

    let publish = (|| -> Result<()> {
        for d in &datasets {
            let new_id = &dataset_ids[&d.dataset_id];
            match d.raw_data_kind {
                RawDataKind::CocoDir => {
                    let src = staging.join(&d.raw_data_archive_path);
                    if !src.is_dir() {
                        bail!("archiwum bez katalogu datasetu {}", d.raw_data_archive_path);
                    }
                    for (rel, abs) in walk_sorted(&src)? {
                        if rel.ends_with(COCO_FILE) {
                            let (imgs, anns, _) = coco_counts(&abs)?;
                            images_imported += imgs;
                            annotations_imported += anns;
                        }
                    }
                    let build_id = uuid::Uuid::new_v4().to_string();
                    let dest = recog_dataset_dir(&new_project_id, &build_id);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(&src, &dest).with_context(|| {
                        format!("przeniesienie datasetu do {}", dest.display())
                    })?;
                    moved_dirs.push(dest.clone());
                    dataset_paths.insert(
                        new_id.clone(),
                        dest.to_string_lossy().to_string().into_bytes(),
                    );
                }
                RawDataKind::Blob => {
                    let src = staging.join(&d.raw_data_archive_path);
                    let bytes = std::fs::read(&src)
                        .with_context(|| format!("odczyt {}", src.display()))?;
                    dataset_paths.insert(new_id.clone(), bytes);
                }
                RawDataKind::None => {}
            }
        }

        // Artifacts follow the same rule: on disk before any row references them.
        for kind in ARTIFACT_KINDS {
            let kind_root = staging.join("artifacts").join(kind);
            if !kind_root.is_dir() {
                continue;
            }
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&kind_root)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            entries.sort();
            for src in entries {
                let old_run = src
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let Some(new_run) = run_ids.get(&old_run) else {
                    continue;
                };
                let dest = artifact_dir(kind, &new_project_id, new_run);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if dest.exists() {
                    std::fs::remove_dir_all(&dest)?;
                }
                std::fs::rename(&src, &dest).with_context(|| {
                    format!("przeniesienie artefaktow do {}", dest.display())
                })?;
                moved_dirs.push(dest);
            }
        }
        Ok(())
    })();

    if let Err(e) = publish {
        for d in &moved_dirs {
            let _ = std::fs::remove_dir_all(d);
        }
        return Err(e);
    }

    // Rows go in as one transaction; if it fails, the files we just moved are removed
    // so the node is left exactly as it was.
    let insert = insert_new_project_rows(
        &new_project_id,
        &name,
        manifest,
        owner_user_id,
        org_id,
        &datasets,
        &dataset_ids,
        &dataset_paths,
        &schemas,
        &dicts,
        &dict_ids,
        &models,
        &model_ids,
        &runs,
        &run_ids,
        &metrics,
    );
    if let Err(e) = insert {
        for d in &moved_dirs {
            let _ = std::fs::remove_dir_all(d);
        }
        return Err(e);
    }

    Ok(ImportSummary {
        project_id: new_project_id,
        project_name: name,
        dataset_count: datasets.len() as u64,
        images_imported,
        images_skipped_duplicate: 0,
        annotations_imported,
        models_imported: models.len() as u64,
        training_runs_imported: runs.len() as u64,
        missing_artifacts: manifest.missing_artifacts.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_new_project_rows(
    new_project_id: &str,
    name: &str,
    manifest: &ArchiveManifest,
    owner_user_id: &str,
    org_id: &str,
    datasets: &[DatasetRow],
    dataset_ids: &HashMap<String, String>,
    dataset_paths: &HashMap<String, Vec<u8>>,
    schemas: &[SchemaRow],
    dicts: &[LookupDictRow],
    dict_ids: &HashMap<String, String>,
    models: &[ModelRow],
    model_ids: &HashMap<String, String>,
    runs: &[TrainingRunRow],
    run_ids: &HashMap<String, String>,
    metrics: &[MetricRow],
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    let tx = conn.unchecked_transaction()?;

    tx.execute(
        "INSERT INTO projects \
             (project_id, name, description, project_type, status, owner_user_id, org_id) \
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6)",
        params![
            new_project_id,
            name,
            manifest.project.description,
            manifest.project.project_type,
            owner_user_id,
            org_id
        ],
    )?;
    tx.execute(
        "INSERT INTO project_members (project_id, user_id, role, status, invited_by) \
         VALUES (?1, ?2, 'owner', 'active', ?2)",
        params![new_project_id, owner_user_id],
    )?;

    for d in datasets {
        let new_id = &dataset_ids[&d.dataset_id];
        let raw: Option<&Vec<u8>> = dataset_paths.get(new_id);
        tx.execute(
            "INSERT INTO datasets \
                 (dataset_id, project_id, name, kind, row_count, column_count, profile_json, raw_data) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                new_id,
                new_project_id,
                d.name,
                d.kind,
                d.row_count,
                d.column_count,
                d.profile_json,
                raw.map(|b| b.as_slice())
            ],
        )?;
    }

    for d in dicts {
        tx.execute(
            "INSERT INTO lookup_dicts (dict_id, project_id, name, rows_json) \
             VALUES (?1, ?2, ?3, ?4)",
            params![dict_ids[&d.dict_id], new_project_id, d.name, d.rows_json],
        )?;
    }

    for s in schemas {
        // Schemas reference lookup dictionaries by id (OCR attribute config); those ids
        // were just remapped, so the stored JSON has to follow or the attribute panel
        // would point at a dictionary of a different project.
        let remapped = remap_dict_ids(&s.json, dict_ids)?;
        tx.execute(
            "INSERT INTO schemas (schema_id, project_id, json) VALUES (?1, ?2, ?3)",
            params![
                uuid::Uuid::new_v4().to_string(),
                new_project_id,
                remapped
            ],
        )?;
    }

    for m in models {
        tx.execute(
            "INSERT INTO models \
                 (model_id, project_id, name, framework, base_model, metrics_json, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                model_ids[&m.model_id],
                new_project_id,
                m.name,
                m.framework,
                m.base_model,
                m.metrics_json,
                m.status
            ],
        )?;
    }

    for r in runs {
        let mapped_model = r
            .model_id
            .as_ref()
            .and_then(|id| model_ids.get(id).cloned());
        tx.execute(
            "INSERT INTO training_runs \
                 (run_id, project_id, model_id, status, config_json, started_at, finished_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_ids[&r.run_id],
                new_project_id,
                mapped_model,
                r.status,
                r.config_json,
                r.started_at,
                r.finished_at
            ],
        )?;
    }

    for m in metrics {
        let Some(run) = run_ids.get(&m.run_id) else {
            continue;
        };
        tx.execute(
            "INSERT INTO metrics_history (run_id, step, metric_key, metric_value, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run, m.step, m.metric_key, m.metric_value, m.ts],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Finds a free project name inside the org. `projects` has UNIQUE(org_id, name), so
/// an unmodified name would otherwise fail the whole import.
fn unique_project_name(org_id: &str, wanted: &str) -> Result<String> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut candidate = wanted.to_string();
    let mut suffix = 2u32;
    loop {
        let taken: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM projects WHERE org_id = ?1 AND name = ?2",
                params![org_id, candidate],
                |r| r.get(0),
            )
            .optional()?;
        if taken.is_none() {
            return Ok(candidate);
        }
        candidate = format!("{wanted} ({suffix})");
        suffix += 1;
        if suffix > 1000 {
            bail!("nie udało się znaleźć wolnej nazwy projektu dla '{wanted}'");
        }
    }
}

/// Rewrites every `lookup_dict_id` value found anywhere in a schema JSON tree. The
/// walk is structural rather than shape-aware so a schema gaining new nesting does
/// not silently start importing broken dictionary references.
fn remap_dict_ids(schema_json: &str, dict_ids: &HashMap<String, String>) -> Result<String> {
    let mut value: Value =
        serde_json::from_str(schema_json).context("parsowanie schematu projektu")?;
    fn walk(v: &mut Value, map: &HashMap<String, String>) {
        match v {
            Value::Object(obj) => {
                for (k, child) in obj.iter_mut() {
                    if k == "lookup_dict_id" || k == "dict_id" {
                        if let Some(old) = child.as_str() {
                            if let Some(new) = map.get(old) {
                                *child = Value::String(new.clone());
                                continue;
                            }
                        }
                    }
                    walk(child, map);
                }
            }
            Value::Array(arr) => {
                for child in arr.iter_mut() {
                    walk(child, map);
                }
            }
            _ => {}
        }
    }
    walk(&mut value, dict_ids);
    Ok(serde_json::to_string(&value)?)
}

// ---------------------------------------------------------------------------
// Import: merge into an existing dataset
// ---------------------------------------------------------------------------

fn merge_into_dataset(
    staging: &Path,
    manifest: &ArchiveManifest,
    project_id: &str,
    dataset_id: &str,
) -> Result<ImportSummary> {
    let target_dir = resolve_coco_dataset_dir(project_id, dataset_id)?;
    let target_train = target_dir.join("train");
    let target_coco_path = target_train.join(COCO_FILE);
    if !target_coco_path.is_file() {
        bail!("dataset docelowy nie ma train/{COCO_FILE}");
    }

    let mut target: Value = serde_json::from_slice(&std::fs::read(&target_coco_path)?)
        .with_context(|| format!("parsowanie {}", target_coco_path.display()))?;

    let target_categories: HashMap<String, i64> = target
        .get("categories")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some((c.get("name")?.as_str()?.to_string(), c.get("id")?.as_i64()?))
                })
                .collect()
        })
        .unwrap_or_default();
    if target_categories.is_empty() {
        bail!("dataset docelowy nie ma kategorii COCO");
    }

    // Content hashes of what the target already holds: an image that is byte-identical
    // to one already present is a re-import, not a new sample.
    let mut existing_hashes: HashSet<String> = HashSet::new();
    let mut existing_names: HashMap<String, String> = HashMap::new();
    for (rel, abs) in walk_sorted(&target_train)? {
        if rel == COCO_FILE {
            continue;
        }
        let h = sha256_file(&abs)?;
        existing_hashes.insert(h.clone());
        existing_names.insert(rel, h);
    }

    let mut next_image_id = max_id(&target, "images") + 1;
    let mut next_ann_id = max_id(&target, "annotations") + 1;

    let mut new_images: Vec<Value> = Vec::new();
    let mut new_anns: Vec<Value> = Vec::new();
    let mut copies: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut imported: u64 = 0;
    let mut skipped: u64 = 0;

    let datasets_root = staging.join("datasets");
    let mut source_dirs: Vec<PathBuf> = if datasets_root.is_dir() {
        std::fs::read_dir(&datasets_root)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("train").join(COCO_FILE).is_file())
            .collect()
    } else {
        Vec::new()
    };
    source_dirs.sort();
    if source_dirs.is_empty() {
        bail!("archiwum nie zawiera żadnego datasetu COCO do scalenia");
    }

    for src_dataset in &source_dirs {
        let src_train = src_dataset.join("train");
        let src_coco: Value =
            serde_json::from_slice(&std::fs::read(src_train.join(COCO_FILE))?)
                .context("parsowanie COCO z archiwum")?;

        // Categories are matched BY NAME. A class the target does not know cannot be
        // remapped to any id, and guessing would silently mislabel every box that uses
        // it — so the merge refuses and names the offenders.
        let src_categories: Vec<(i64, String)> = src_coco
            .get("categories")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        Some((c.get("id")?.as_i64()?, c.get("name")?.as_str()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let unknown: Vec<String> = src_categories
            .iter()
            .map(|(_, n)| n.clone())
            .filter(|n| !target_categories.contains_key(n))
            .collect();
        if !unknown.is_empty() {
            bail!(
                "dataset docelowy nie ma klas obecnych w archiwum: {}",
                unknown.join(", ")
            );
        }
        let cat_remap: HashMap<i64, i64> = src_categories
            .iter()
            .filter_map(|(id, name)| target_categories.get(name).map(|t| (*id, *t)))
            .collect();

        let empty = Vec::new();
        let src_images = src_coco
            .get("images")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        let src_anns = src_coco
            .get("annotations")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);

        // Annotations grouped by their source image id, so each image carries its own
        // boxes through the id remap.
        let mut by_image: HashMap<i64, Vec<&Value>> = HashMap::new();
        for a in src_anns {
            if let Some(iid) = a.get("image_id").and_then(|v| v.as_i64()) {
                by_image.entry(iid).or_default().push(a);
            }
        }

        for im in src_images {
            let Some(old_id) = im.get("id").and_then(|v| v.as_i64()) else {
                continue;
            };
            let Some(file_name) = im.get("file_name").and_then(|v| v.as_str()) else {
                continue;
            };
            let src_file = src_train.join(file_name);
            if !src_file.is_file() {
                bail!("archiwum deklaruje obraz {file_name}, którego nie ma w datasecie");
            }
            let hash = sha256_file(&src_file)?;
            if existing_hashes.contains(&hash) {
                skipped += 1;
                continue;
            }

            // Same name, different bytes: the incoming file gets a content-derived
            // suffix. An existing image is never overwritten.
            let final_name = if existing_names.contains_key(file_name) {
                suffixed_name(file_name, &hash)
            } else {
                file_name.to_string()
            };

            let mut image = im.clone();
            if let Some(obj) = image.as_object_mut() {
                obj.insert("id".to_string(), json!(next_image_id));
                obj.insert("file_name".to_string(), json!(final_name));
            }
            new_images.push(image);
            copies.push((src_file, target_train.join(&final_name)));
            existing_hashes.insert(hash.clone());
            existing_names.insert(final_name, hash);

            for a in by_image.get(&old_id).into_iter().flatten() {
                let mut ann = (*a).clone();
                let Some(obj) = ann.as_object_mut() else {
                    continue;
                };
                let old_cat = obj.get("category_id").and_then(|v| v.as_i64()).unwrap_or(0);
                let Some(new_cat) = cat_remap.get(&old_cat) else {
                    bail!("adnotacja wskazuje nieznaną kategorię {old_cat}");
                };
                obj.insert("id".to_string(), json!(next_ann_id));
                obj.insert("image_id".to_string(), json!(next_image_id));
                obj.insert("category_id".to_string(), json!(*new_cat));
                next_ann_id += 1;
                new_anns.push(ann);
            }

            next_image_id += 1;
            imported += 1;
        }
    }

    // Image bytes land before the COCO file is republished: an interrupted merge then
    // leaves unreferenced files (harmless, re-merge dedupes them) rather than COCO
    // records pointing at images that never arrived.
    for (src, dest) in &copies {
        std::fs::copy(src, dest)
            .with_context(|| format!("kopiowanie {} -> {}", src.display(), dest.display()))?;
    }

    let annotations_imported = new_anns.len() as u64;
    append_array(&mut target, "images", new_images)?;
    append_array(&mut target, "annotations", new_anns)?;

    let tmp = target_coco_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&target)?)?;
    std::fs::rename(&tmp, &target_coco_path)
        .with_context(|| format!("publikacja {}", target_coco_path.display()))?;

    Ok(ImportSummary {
        project_id: project_id.to_string(),
        project_name: manifest.project.name.clone(),
        dataset_count: source_dirs.len() as u64,
        images_imported: imported,
        images_skipped_duplicate: skipped,
        annotations_imported,
        models_imported: 0,
        training_runs_imported: 0,
        missing_artifacts: manifest.missing_artifacts.clone(),
    })
}

/// Resolves the on-disk COCO root of an existing dataset, refusing non-`coco_path`
/// datasets (there is nothing to merge images into).
fn resolve_coco_dataset_dir(project_id: &str, dataset_id: &str) -> Result<PathBuf> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let row: Option<(String, String, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT project_id, kind, raw_data FROM datasets WHERE dataset_id = ?1",
            params![dataset_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((owner_project, kind, raw)) = row else {
        bail!("dataset {dataset_id} nie istnieje");
    };
    if owner_project != project_id {
        bail!("dataset {dataset_id} nie należy do projektu {project_id}");
    }
    if kind != "coco_path" {
        bail!("scalanie obsługiwane tylko dla datasetów COCO (kind = coco_path)");
    }
    let path = raw
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    let dir = PathBuf::from(path.trim());
    if !dir.is_dir() {
        bail!("katalog datasetu docelowego nie istnieje: {}", dir.display());
    }
    Ok(dir)
}

fn max_id(coco: &Value, key: &str) -> i64 {
    coco.get(key)
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|e| e.get("id").and_then(|v| v.as_i64()))
                .max()
        })
        .unwrap_or(0)
}

fn append_array(coco: &mut Value, key: &str, mut items: Vec<Value>) -> Result<()> {
    if let Some(arr) = coco.get_mut(key).and_then(|v| v.as_array_mut()) {
        arr.append(&mut items);
        return Ok(());
    }
    coco.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("COCO nie jest obiektem"))?
        .insert(key.to_string(), Value::Array(items));
    Ok(())
}

/// `photo.jpg` + hash -> `photo.a1b2c3d4.jpg`. Keeps the extension so decoders still
/// recognize the file.
fn suffixed_name(file_name: &str, hash: &str) -> String {
    let short = &hash[..8.min(hash.len())];
    match file_name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}.{short}.{ext}"),
        None => format!("{file_name}.{short}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// `paths` overrides and the ML Studio pool are process-global, so archive tests
    /// run one at a time against one shared temp home.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct TestEnv {
        _guard: MutexGuard<'static, ()>,
        _home: tempfile::TempDir,
    }

    fn setup() -> TestEnv {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("temp home");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(home.path().join("data").to_string_lossy().to_string()),
        );
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Cache,
            Some(home.path().join("cache").to_string_lossy().to_string()),
        );
        std::env::set_var("TENTAFLOW_CACHE_DIR", home.path().join("cache"));
        std::fs::create_dir_all(crate::paths::data_dir()).expect("data dir");
        // The pool is a global OnceLock: the first test to run opens the database and
        // every later one reuses it, so each test works on freshly created project ids.
        let _ = super::super::db::init(&crate::paths::data_dir().join("ml_studio.db"));
        TestEnv {
            _guard: guard,
            _home: home,
        }
    }

    fn exec(sql: &str, p: &[&dyn rusqlite::ToSql]) {
        let pool = super::super::db::pool().expect("pool");
        let conn = pool.write().expect("write");
        conn.execute(sql, p).expect("exec");
    }

    /// A 1x1 PNG-ish payload: the archive never decodes images, only hashes/copies
    /// them, so distinct byte strings are enough to exercise dedupe and collisions.
    fn image_bytes(tag: &str) -> Vec<u8> {
        format!("PNGDATA-{tag}").into_bytes()
    }

    fn coco_doc(images: Vec<Value>, anns: Vec<Value>, cats: &[&str]) -> Value {
        json!({
            "images": images,
            "annotations": anns,
            "categories": cats.iter().enumerate()
                .map(|(i, n)| json!({"id": i as i64 + 1, "name": n}))
                .collect::<Vec<_>>(),
        })
    }

    /// Creates a project with one `coco_path` dataset holding two annotated images,
    /// one of which carries `approved` + per-box `attributes`.
    fn seed_project(name: &str) -> (String, String, PathBuf) {
        let project_id = uuid::Uuid::new_v4().to_string();
        let dataset_id = uuid::Uuid::new_v4().to_string();
        let build_id = uuid::Uuid::new_v4().to_string();
        let dir = recog_dataset_dir(&project_id, &build_id);
        let train = dir.join("train");
        std::fs::create_dir_all(&train).expect("train dir");
        std::fs::write(train.join("a.png"), image_bytes("a")).expect("a.png");
        std::fs::write(train.join("b.png"), image_bytes("b")).expect("b.png");

        let coco = coco_doc(
            vec![
                json!({"id": 1, "file_name": "a.png", "width": 4, "height": 4, "approved": true}),
                json!({"id": 2, "file_name": "b.png", "width": 4, "height": 4}),
            ],
            vec![
                json!({
                    "id": 1, "image_id": 1, "category_id": 1,
                    "bbox": [0, 0, 2, 2], "area": 4, "iscrowd": 0,
                    "attributes": {"stan": "czysta"},
                }),
                json!({
                    "id": 2, "image_id": 2, "category_id": 2,
                    "bbox": [1, 1, 2, 2], "area": 4, "iscrowd": 0,
                    "score": 0.87, "predicted": true,
                }),
            ],
            &["tablica_adr", "termometr"],
        );
        std::fs::write(train.join(COCO_FILE), serde_json::to_vec(&coco).unwrap())
            .expect("coco");

        exec(
            "INSERT INTO projects (project_id, name, description, project_type, owner_user_id, org_id) \
             VALUES (?1, ?2, '', 'recognition', 'user-src', 'org-src')",
            &[&project_id, &name],
        );
        exec(
            "INSERT INTO project_members (project_id, user_id, role, status, invited_by) \
             VALUES (?1, 'user-src', 'owner', 'active', 'user-src')",
            &[&project_id],
        );
        exec(
            "INSERT INTO datasets (dataset_id, project_id, name, kind, row_count, raw_data) \
             VALUES (?1, ?2, 'zbior', 'coco_path', 2, ?3)",
            &[
                &dataset_id,
                &project_id,
                &dir.to_string_lossy().to_string().into_bytes(),
            ],
        );

        let dict_id = uuid::Uuid::new_v4().to_string();
        exec(
            "INSERT INTO lookup_dicts (dict_id, project_id, name, rows_json) \
             VALUES (?1, ?2, 'kody', '[{\"kod\":\"33\"}]')",
            &[&dict_id, &project_id],
        );
        let schema = json!({
            "classes": [
                {"name": "tablica_adr", "attributes": [
                    {"key": "stan", "type": "enum", "values": ["czysta", "brudna"]},
                    {"key": "kod", "type": "ocr", "lookup_dict_id": dict_id},
                ]}
            ]
        });
        exec(
            "INSERT INTO schemas (schema_id, project_id, json) VALUES (?1, ?2, ?3)",
            &[
                &uuid::Uuid::new_v4().to_string(),
                &project_id,
                &serde_json::to_string(&schema).unwrap(),
            ],
        );

        (project_id, dataset_id, dir)
    }

    fn read_target_coco(project_id: &str, dataset_id: &str) -> Value {
        let dir = resolve_coco_dataset_dir(project_id, dataset_id).expect("dataset dir");
        serde_json::from_slice(&std::fs::read(dir.join("train").join(COCO_FILE)).unwrap())
            .unwrap()
    }

    #[test]
    fn round_trip_preserves_approved_and_attributes() {
        let _env = setup();
        let (project_id, _dataset_id, _dir) = seed_project("Cysterny RT");

        let zip_path = crate::paths::data_dir().join("rt.zip");
        let summary = build_export(
            &project_id,
            ExportOptions {
                include_models: false,
                include_history: false,
            },
            &zip_path,
        )
        .expect("export");
        assert_eq!(summary.dataset_count, 1);
        assert_eq!(summary.image_count, 2);
        assert_eq!(summary.annotation_count, 2);
        assert!(zip_path.is_file(), "archive must exist after export");
        assert!(
            !tmp_sibling(&zip_path).exists(),
            "temp file must be renamed away"
        );

        let imported = import_archive(
            &zip_path,
            ImportMode::NewProject {
                name_override: None,
            },
            "user-dst",
            "org-dst",
        )
        .expect("import");
        assert_eq!(imported.images_imported, 2);
        assert_eq!(imported.annotations_imported, 2);
        assert_ne!(imported.project_id, project_id, "fresh project id");

        // The new project owns a new dataset whose COCO kept the editor's fields.
        let pool = super::super::db::pool().unwrap();
        let new_dataset: String = {
            let conn = pool.read().unwrap();
            conn.query_row(
                "SELECT dataset_id FROM datasets WHERE project_id = ?1",
                params![imported.project_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        let coco = read_target_coco(&imported.project_id, &new_dataset);
        let img_a = coco["images"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["file_name"] == "a.png")
            .expect("a.png survived");
        assert_eq!(img_a["approved"], json!(true), "approved must round-trip");
        let ann_attr = coco["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a.get("attributes").is_some())
            .expect("attributes survived");
        assert_eq!(ann_attr["attributes"]["stan"], json!("czysta"));
        let ann_pred = coco["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a.get("predicted").is_some())
            .expect("predicted survived");
        assert_eq!(ann_pred["score"], json!(0.87));

        // Owner/org are the IMPORTING identity, not the source's.
        let conn = pool.read().unwrap();
        let (owner, org): (String, String) = conn
            .query_row(
                "SELECT owner_user_id, org_id FROM projects WHERE project_id = ?1",
                params![imported.project_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(owner, "user-dst");
        assert_eq!(org, "org-dst");
    }

    #[test]
    fn import_rewrites_raw_data_to_local_path() {
        let _env = setup();
        let (project_id, _dataset_id, source_dir) = seed_project("Cysterny RAW");
        let zip_path = crate::paths::data_dir().join("raw.zip");
        build_export(
            &project_id,
            ExportOptions {
                include_models: false,
                include_history: false,
            },
            &zip_path,
        )
        .expect("export");

        let imported = import_archive(
            &zip_path,
            ImportMode::NewProject {
                name_override: Some("Cysterny RAW kopia".to_string()),
            },
            "user-dst",
            "org-dst",
        )
        .expect("import");

        let pool = super::super::db::pool().unwrap();
        let conn = pool.read().unwrap();
        let raw: Vec<u8> = conn
            .query_row(
                "SELECT raw_data FROM datasets WHERE project_id = ?1",
                params![imported.project_id],
                |r| r.get(0),
            )
            .unwrap();
        let path = PathBuf::from(String::from_utf8(raw).unwrap());
        assert!(path.is_absolute(), "raw_data must stay an absolute path");
        assert_ne!(path, source_dir, "must not point at the exporter's dir");
        assert!(
            path.starts_with(
                crate::paths::data_dir()
                    .join("ml-studio")
                    .join("recog-datasets")
                    .join(sanitize_component(&imported.project_id))
            ),
            "raw_data must point under this node's own recog-datasets root: {}",
            path.display()
        );
        assert!(
            path.join("train").join(COCO_FILE).is_file(),
            "the rewritten path must actually hold the dataset"
        );
    }

    #[test]
    fn merge_dedupes_identical_images_by_content() {
        let _env = setup();
        let (src_project, _src_dataset, _) = seed_project("Merge zrodlo");
        let (dst_project, dst_dataset, _) = seed_project("Merge cel");

        let zip_path = crate::paths::data_dir().join("merge.zip");
        build_export(
            &src_project,
            ExportOptions {
                include_models: false,
                include_history: false,
            },
            &zip_path,
        )
        .expect("export");

        // Both projects were seeded with byte-identical images, so every incoming
        // image is a duplicate and nothing may be appended.
        let merged = import_archive(
            &zip_path,
            ImportMode::MergeInto {
                project_id: dst_project.clone(),
                dataset_id: dst_dataset.clone(),
            },
            "user-dst",
            "org-dst",
        )
        .expect("merge");
        assert_eq!(merged.images_imported, 0);
        assert_eq!(merged.images_skipped_duplicate, 2);
        let coco = read_target_coco(&dst_project, &dst_dataset);
        assert_eq!(coco["images"].as_array().unwrap().len(), 2);
        assert_eq!(coco["annotations"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn merge_appends_new_images_with_remapped_ids() {
        let _env = setup();
        let (src_project, src_dataset, src_dir) = seed_project("Merge nowe zrodlo");
        let (dst_project, dst_dataset, _) = seed_project("Merge nowy cel");

        // Give the source a distinct third image, plus a name collision with different
        // bytes so both the append and the rename path are exercised.
        let src_train = src_dir.join("train");
        std::fs::write(src_train.join("c.png"), image_bytes("c")).unwrap();
        std::fs::write(src_train.join("a.png"), image_bytes("a-different")).unwrap();
        let mut coco: Value =
            serde_json::from_slice(&std::fs::read(src_train.join(COCO_FILE)).unwrap()).unwrap();
        coco["images"].as_array_mut().unwrap().push(
            json!({"id": 3, "file_name": "c.png", "width": 4, "height": 4, "approved": true}),
        );
        coco["annotations"].as_array_mut().unwrap().push(json!({
            "id": 3, "image_id": 3, "category_id": 2,
            "bbox": [0, 0, 1, 1], "area": 1, "iscrowd": 0,
            "attributes": {"stan": "brudna"},
        }));
        std::fs::write(src_train.join(COCO_FILE), serde_json::to_vec(&coco).unwrap()).unwrap();
        let _ = src_dataset;

        let zip_path = crate::paths::data_dir().join("merge2.zip");
        build_export(
            &src_project,
            ExportOptions {
                include_models: false,
                include_history: false,
            },
            &zip_path,
        )
        .expect("export");

        let merged = import_archive(
            &zip_path,
            ImportMode::MergeInto {
                project_id: dst_project.clone(),
                dataset_id: dst_dataset.clone(),
            },
            "user-dst",
            "org-dst",
        )
        .expect("merge");
        assert_eq!(merged.images_imported, 2, "a.png (changed) + c.png");
        assert_eq!(merged.images_skipped_duplicate, 1, "b.png is identical");

        let target = read_target_coco(&dst_project, &dst_dataset);
        let images = target["images"].as_array().unwrap();
        assert_eq!(images.len(), 4);
        // Ids must be unique after the remap.
        let ids: HashSet<i64> = images.iter().map(|i| i["id"].as_i64().unwrap()).collect();
        assert_eq!(ids.len(), images.len(), "image ids must not collide");
        let ann_ids: HashSet<i64> = target["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ann_ids.len(), target["annotations"].as_array().unwrap().len());

        // The colliding name was rewritten, the original left untouched.
        let renamed = images
            .iter()
            .find(|i| {
                let n = i["file_name"].as_str().unwrap();
                n != "a.png" && n.starts_with("a.") && n.ends_with(".png")
            })
            .expect("colliding a.png was renamed");
        let renamed_path = resolve_coco_dataset_dir(&dst_project, &dst_dataset)
            .unwrap()
            .join("train")
            .join(renamed["file_name"].as_str().unwrap());
        assert_eq!(
            std::fs::read(&renamed_path).unwrap(),
            image_bytes("a-different"),
            "renamed file must hold the INCOMING bytes"
        );
        let original_path = renamed_path.with_file_name("a.png");
        assert_eq!(
            std::fs::read(&original_path).unwrap(),
            image_bytes("a"),
            "existing image must never be overwritten"
        );

        // Merged rows kept their editor fields.
        assert_eq!(renamed["approved"], json!(true));
        let merged_ann = target["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["attributes"]["stan"] == json!("brudna"))
            .expect("attributes preserved on merge");
        assert_eq!(merged_ann["category_id"], json!(2), "matched by NAME");
    }

    #[test]
    fn merge_rejects_unknown_category() {
        let _env = setup();
        let (src_project, _src_dataset, src_dir) = seed_project("Kategoria zrodlo");
        let (dst_project, dst_dataset, _) = seed_project("Kategoria cel");

        let src_train = src_dir.join("train");
        let mut coco: Value =
            serde_json::from_slice(&std::fs::read(src_train.join(COCO_FILE)).unwrap()).unwrap();
        coco["categories"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": 99, "name": "nalepka_nieznana"}));
        std::fs::write(src_train.join(COCO_FILE), serde_json::to_vec(&coco).unwrap()).unwrap();

        let zip_path = crate::paths::data_dir().join("cat.zip");
        build_export(
            &src_project,
            ExportOptions {
                include_models: false,
                include_history: false,
            },
            &zip_path,
        )
        .expect("export");

        let err = import_archive(
            &zip_path,
            ImportMode::MergeInto {
                project_id: dst_project,
                dataset_id: dst_dataset,
            },
            "user-dst",
            "org-dst",
        )
        .expect_err("unknown category must abort the merge");
        let msg = err.to_string();
        assert!(
            msg.contains("nalepka_nieznana"),
            "error must name the mismatched class, got: {msg}"
        );
    }

    #[test]
    fn manifest_version_is_rejected_when_unknown() {
        let _env = setup();
        let zip_path = crate::paths::data_dir().join("badver.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let manifest = json!({
            "version": ARCHIVE_VERSION + 7,
            "exported_at": "2026-07-21T00:00:00Z",
            "source_node_id": "node",
            "project": {
                "project_id": "p", "name": "n", "description": "",
                "project_type": "recognition", "owner_user_id": "u", "org_id": "o",
            },
            "contents": {
                "datasets": [], "schema_count": 0, "lookup_dict_count": 0,
                "model_count": 0, "training_run_count": 0, "metric_count": 0,
                "includes_models": false, "includes_history": false,
            },
            "files": [],
            "missing_artifacts": [],
        });
        zip.start_file(MANIFEST_ENTRY.to_string(), deflated_options())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        zip.finish().unwrap();

        let err = read_manifest(&zip_path).expect_err("newer version must be rejected");
        assert!(
            err.to_string().contains("wersja archiwum"),
            "error must explain the version mismatch, got: {err}"
        );
    }

    #[test]
    fn zip_slip_entry_is_rejected() {
        let _env = setup();
        let zip_path = crate::paths::data_dir().join("slip.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let evil = "../../escaped.txt";
        let payload = b"pwned".to_vec();
        let manifest = ArchiveManifest {
            version: ARCHIVE_VERSION,
            exported_at: "2026-07-21T00:00:00Z".to_string(),
            source_node_id: "node".to_string(),
            project: ProjectMeta {
                project_id: "p".to_string(),
                name: "Slip".to_string(),
                description: String::new(),
                project_type: "recognition".to_string(),
                owner_user_id: "u".to_string(),
                org_id: "o".to_string(),
            },
            contents: ArchiveContents {
                datasets: Vec::new(),
                schema_count: 0,
                lookup_dict_count: 0,
                model_count: 0,
                training_run_count: 0,
                metric_count: 0,
                includes_models: false,
                includes_history: false,
            },
            files: vec![FileEntry {
                path: evil.to_string(),
                size: payload.len() as u64,
                sha256: sha256_bytes(&payload),
            }],
            missing_artifacts: Vec::new(),
        };
        zip.start_file(evil.to_string(), stored_options()).unwrap();
        zip.write_all(&payload).unwrap();
        zip.start_file(MANIFEST_ENTRY.to_string(), deflated_options())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        zip.finish().unwrap();

        let staging = crate::paths::data_dir().join("slip-staging");
        std::fs::create_dir_all(&staging).unwrap();
        let escaped = crate::paths::data_dir().join("escaped.txt");
        let err = extract_all(&zip_path, &staging, &manifest, None)
            .expect_err("traversal entry must abort extraction");
        assert!(
            err.to_string().contains("niebezpieczna ścieżka")
                || err.to_string().contains("nie występuje w manifescie"),
            "unexpected error: {err}"
        );
        assert!(
            !escaped.exists(),
            "traversal must not write outside the staging dir"
        );
    }
}
