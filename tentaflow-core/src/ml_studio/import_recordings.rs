// ===== File: ml_studio/import_recordings.rs — import TentaVision frames into a dataset =====
//
// Takes recordings picked in TentaVision, decodes them to frames at a chosen fps
// and APPENDS those frames to an EXISTING recognition COCO dataset, optionally
// pre-labeled by the in-core vision models (RF-DETR boxes → per-crop state
// classification → ADR placard code). The human then only corrects predictions
// instead of drawing every box from scratch.
//
// The dataset is append-only here: existing images, their `approved` flag and
// their `attributes` are never read-modified-written. New images land with
// `approved: false` (unreviewed by definition) and new annotations are marked
// `predicted: true` + `score`, which is what makes the editor render them dashed.
//
// Decoding hundreds of clips and running three model stages per frame is MINUTES
// of work, so this runs as a background job (job id returned immediately, UI polls
// `ImportProgress`) under the SAME per-dataset guard as auto-label — the two jobs
// both republish `_annotations.coco.json` and must never interleave.
//
// The vision stages are feature-gated behind `inference-vision-gpu` (mirroring
// `autolabel_recog_dataset`): with the feature off, an import WITHOUT auto-label
// still works, and requesting auto-label returns a clear error.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::build_recog_dataset::{FPS_MAX, FPS_MIN};

/// Max recordings consumed by a single import job. UI caps are bypassable, so this
/// is the real limit enforced before any decoding starts.
const MAX_RECORDINGS_PER_JOB: usize = 500;
/// Max frames one import job may append across all its recordings.
const MAX_IMPORT_FRAMES: u64 = 50_000;
/// Overall wall-clock budget for one import job.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// Model stages are skipped for boxes smaller than this — a 1–3 px crop carries no
/// signal and only wastes a forward pass.
#[cfg(feature = "inference-vision-gpu")]
const MIN_CROP_PX: i64 = 4;

/// What to do when an imported frame's name is already used in the target dataset.
/// Overwriting is deliberately NOT an option: an existing image may already carry
/// approved human work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionPolicy {
    /// Append `_1`, `_2`, … until the name is free.
    Suffix,
    /// Drop the frame and report it in the per-recording outcome.
    Skip,
}

impl FromStr for CollisionPolicy {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "suffix" => Ok(CollisionPolicy::Suffix),
            "skip" => Ok(CollisionPolicy::Skip),
            other => anyhow::bail!("nieznana polityka kolizji: {other} (oczekiwano suffix|skip)"),
        }
    }
}

/// Everything one import job needs. `dataset_dir` is the resolved on-disk
/// `coco_path` root (the caller has already authorized the user against the
/// project); `recording_refs` are TentaVision `recordings.ref` values.
#[derive(Debug, Clone)]
pub struct ImportRecordingsSpec {
    pub dataset_id: String,
    pub project_id: String,
    pub owner_user_id: String,
    pub dataset_dir: PathBuf,
    pub recording_refs: Vec<String>,
    pub fps: u32,
    pub autolabel: bool,
    pub collision: CollisionPolicy,
}

/// Outcome of one source recording, so the UI can show what actually happened
/// rather than a single aggregate number.
#[derive(Clone, Debug, Default)]
pub struct RecordingOutcome {
    pub recording_ref: String,
    /// Frames appended to the dataset (already collision-resolved).
    pub frames: u64,
    /// Annotations written for those frames.
    pub detections: u64,
    /// Frames dropped by `CollisionPolicy::Skip`.
    pub skipped_frames: u64,
    /// Set when the whole recording was rejected; carries the human-readable reason.
    pub skipped: Option<String>,
}

/// Live progress of an async import job, polled by the UI. `status` is
/// "running" | "succeeded" | "failed"; `phase` is "extracting" | "labeling" |
/// "publishing". `project_id`/`owner_user_id` let the status handler authorize the
/// caller — a job id alone must not expose progress to an unrelated user.
#[derive(Clone, Debug)]
pub struct ImportProgress {
    pub status: String,
    pub phase: String,
    pub recordings_total: u64,
    pub recordings_done: u64,
    pub frames_extracted: u64,
    pub frames_labeled: u64,
    pub images_added: u64,
    pub detections: u64,
    pub project_id: String,
    pub owner_user_id: String,
    pub outcomes: Vec<RecordingOutcome>,
    pub error: Option<String>,
}

impl Default for ImportProgress {
    fn default() -> Self {
        ImportProgress {
            status: "running".to_string(),
            phase: "extracting".to_string(),
            recordings_total: 0,
            recordings_done: 0,
            frames_extracted: 0,
            frames_labeled: 0,
            images_added: 0,
            detections: 0,
            project_id: String::new(),
            owner_user_id: String::new(),
            outcomes: Vec::new(),
            error: None,
        }
    }
}

static PROGRESS: OnceLock<Mutex<HashMap<String, ImportProgress>>> = OnceLock::new();

fn progress_map() -> &'static Mutex<HashMap<String, ImportProgress>> {
    PROGRESS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_progress(job_id: &str, p: ImportProgress) {
    if let Ok(mut m) = progress_map().lock() {
        m.insert(job_id.to_string(), p);
    }
}

fn update_progress(job_id: &str, f: impl FnOnce(&mut ImportProgress)) {
    if let Ok(mut m) = progress_map().lock() {
        if let Some(p) = m.get_mut(job_id) {
            f(p);
        }
    }
}

/// Current progress of an import job (None when the job id is unknown).
pub fn import_progress(job_id: &str) -> Option<ImportProgress> {
    progress_map().lock().ok()?.get(job_id).cloned()
}

/// Starts an async import of TentaVision recordings into an existing recognition
/// dataset. Returns the job id used for status polling, or an error when the spec
/// violates a server-side cap, another job already holds the dataset, or auto-label
/// was requested without the vision feature compiled in.
pub fn spawn_import_recordings(spec: ImportRecordingsSpec) -> Result<String> {
    validate_spec(&spec)?;

    #[cfg(not(feature = "camera"))]
    {
        anyhow::bail!(
            "import nagrań wymaga modułu kamer (feature camera) — niedostępny w tej kompilacji"
        );
    }

    #[cfg(feature = "camera")]
    {
        let train_dir = spec.dataset_dir.join("train");
        let annot_path = train_dir.join("_annotations.coco.json");
        if !annot_path.is_file() {
            anyhow::bail!("dataset nie zawiera train/_annotations.coco.json");
        }
        let pool = crate::db::global_pool().ok_or_else(|| {
            anyhow::anyhow!("baza główna niedostępna — nie można odczytać nagrań")
        })?;

        // Shared with auto-label: both jobs republish the same COCO file, so a
        // dataset may host only one of them at a time.
        if !super::autolabel_recog_dataset::try_claim_dataset(&spec.dataset_id) {
            anyhow::bail!(
                "inne zadanie tego datasetu już trwa (import lub auto-etykietowanie) — poczekaj na jego zakończenie"
            );
        }

        let job_id = uuid::Uuid::new_v4().to_string();
        set_progress(
            &job_id,
            ImportProgress {
                project_id: spec.project_id.clone(),
                owner_user_id: spec.owner_user_id.clone(),
                recordings_total: spec.recording_refs.len() as u64,
                ..ImportProgress::default()
            },
        );

        let job_id_task = job_id.clone();
        tokio::spawn(async move {
            let jid = job_id_task.clone();
            let did = spec.dataset_id.clone();
            // ffmpeg + decode + GPU inference are blocking — keep them off the
            // async worker.
            let result =
                tokio::task::spawn_blocking(move || run_import(&job_id_task, &spec, &pool)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(job_id = %jid, error = %err, "recording import failed");
                    update_progress(&jid, |p| {
                        p.status = "failed".to_string();
                        p.error = Some(err.to_string());
                    });
                }
                Err(join_err) => {
                    tracing::warn!(job_id = %jid, error = %join_err, "recording import task panicked");
                    update_progress(&jid, |p| {
                        p.status = "failed".to_string();
                        p.error = Some(format!("import task failed: {}", join_err));
                    });
                }
            }
            super::autolabel_recog_dataset::release_dataset(&did);
        });

        Ok(job_id)
    }
}

/// Server-side spec validation, run before anything is claimed or spawned.
fn validate_spec(spec: &ImportRecordingsSpec) -> Result<()> {
    if !(FPS_MIN..=FPS_MAX).contains(&spec.fps) {
        anyhow::bail!("fps musi być w zakresie {FPS_MIN}..={FPS_MAX}");
    }
    if spec.recording_refs.is_empty() {
        anyhow::bail!("nie wybrano żadnego nagrania");
    }
    if spec.recording_refs.len() > MAX_RECORDINGS_PER_JOB {
        anyhow::bail!("wybrano za dużo nagrań (limit {MAX_RECORDINGS_PER_JOB})");
    }
    #[cfg(not(feature = "inference-vision-gpu"))]
    if spec.autolabel {
        anyhow::bail!(
            "auto-etykietowanie wymaga wbudowanego detektora wizyjnego (feature inference-vision-gpu) — niedostępne w tej kompilacji"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// True iff `canonical` is inside the recordings tree. Composed of a strict prefix
/// check against the canonical base and a scan for the `.tentaflow/recordings`
/// directory pair that `services::recording::storage::camera_subdir` hard-codes;
/// BOTH must agree, so a planted `/elsewhere/.tentaflow/recordings/blob` fails the
/// prefix check and a DB-tampered `/etc/passwd` fails the segment scan.
///
/// `base` is `None` only when the recordings root cannot be canonicalised (it does
/// not exist yet); the segment scan alone still rejects every traversal vector.
fn path_is_contained(canonical: &Path, base: Option<&Path>) -> bool {
    if let Some(base) = base {
        if !canonical.starts_with(base) {
            return false;
        }
    }
    let mut comps = canonical
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .peekable();
    while let Some(c) = comps.next() {
        if c == ".tentaflow" && comps.peek() == Some(&"recordings") {
            return true;
        }
    }
    false
}

/// Resolves a `recordings.file_path` to a canonical path that is proven to live
/// inside the recordings tree. The DB row is NOT trusted: symlinks are rejected
/// BEFORE canonicalize (which would silently resolve them — the recorder never
/// writes symlinks, so one here means the row was tampered with).
#[cfg(feature = "camera")]
fn validated_recording_path(file_path: &str) -> Result<PathBuf> {
    let meta = std::fs::symlink_metadata(file_path)
        .with_context(|| format!("plik nagrania niedostępny: {file_path}"))?;
    if meta.file_type().is_symlink() {
        anyhow::bail!("ścieżka nagrania jest dowiązaniem symbolicznym — odrzucona");
    }
    let canonical = std::fs::canonicalize(file_path)
        .with_context(|| format!("kanonizacja ścieżki nagrania: {file_path}"))?;
    let base = crate::services::recording::recording_base_dir()
        .ok()
        .and_then(|b| std::fs::canonicalize(b).ok());
    if !path_is_contained(&canonical, base.as_deref()) {
        anyhow::bail!("ścieżka nagrania leży poza katalogiem nagrań — odrzucona");
    }
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// COCO helpers (pure — unit tested without models or a GPU)
// ---------------------------------------------------------------------------

/// Highest `id` in a COCO array, or 0 when there is none. Existing datasets have a
/// SPARSE id space (deleted boxes leave gaps), so new ids must start above the
/// maximum, never at `len()`.
fn max_id(items: &[Value]) -> i64 {
    items
        .iter()
        .filter_map(|v| v.get("id").and_then(|v| v.as_i64()))
        .max()
        .unwrap_or(0)
}

/// Converts a detector bbox (NORMALIZED 0..1 xywh) to absolute pixel xywh clamped
/// to the frame. Clamping the CORNERS (not the width/height) is what makes a box
/// running off an edge get truncated instead of shifted inwards. `None` when the
/// clamped box has no area — a degenerate box is not a usable annotation.
#[cfg(any(feature = "inference-vision-gpu", test))]
fn norm_bbox_to_pixels(bbox: [f32; 4], w: u32, h: u32) -> Option<[i64; 4]> {
    if w == 0 || h == 0 || !bbox.iter().all(|v| v.is_finite()) {
        return None;
    }
    let (fw, fh) = (w as f32, h as f32);
    let x0 = (bbox[0] * fw).round().clamp(0.0, fw) as i64;
    let y0 = (bbox[1] * fh).round().clamp(0.0, fh) as i64;
    let x1 = ((bbox[0] + bbox[2]) * fw).round().clamp(0.0, fw) as i64;
    let y1 = ((bbox[1] + bbox[3]) * fh).round().clamp(0.0, fh) as i64;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some([x0, y0, x1 - x0, y1 - y0])
}

/// Picks a dataset-unique file name for an imported frame. `taken` holds every name
/// already used (COCO `images` + everything on disk in `train/` + names handed out
/// earlier in this job), so the returned name can never overwrite existing work.
/// `None` = the caller's policy is `Skip` and the name was taken.
fn unique_frame_name(
    desired: &str,
    taken: &HashSet<String>,
    policy: CollisionPolicy,
) -> Option<String> {
    if !taken.contains(desired) {
        return Some(desired.to_string());
    }
    if policy == CollisionPolicy::Skip {
        return None;
    }
    let (stem, ext) = match desired.rsplit_once('.') {
        Some((s, e)) if !e.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (desired.to_string(), String::new()),
    };
    let mut n = 1u32;
    loop {
        let candidate = format!("{stem}_{n}{ext}");
        if !taken.contains(&candidate) {
            return Some(candidate);
        }
        n += 1;
    }
}

/// Frame file name that cannot collide with existing dataset images and stays
/// traceable to its source: `rec_<ref>_f0001.<ext>`. Non-alphanumerics in the ref
/// collapse so the name is always a safe single path component. `ext` follows the
/// bytes actually copied (a PNG snapshot must not be published as `.jpg`).
#[cfg(any(feature = "camera", test))]
fn frame_file_name(recording_ref: &str, index: u64, ext: &str) -> String {
    let safe: String = recording_ref
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("rec_{safe}_f{index:04}.{ext}")
}

/// Attribute surface a project's schema declares for one class. Only attributes
/// listed here may be written, so an auto-label never invents a key the editor
/// cannot render.
#[cfg(any(feature = "inference-vision-gpu", test))]
#[derive(Debug, Clone, Default)]
struct ClassAttrs {
    /// Allowed values of the `stan` list attribute; empty when the class has none.
    stan_values: Vec<String>,
    /// Whether the class declares the `kod` OCR attribute.
    has_kod: bool,
}

#[cfg(any(feature = "inference-vision-gpu", test))]
impl ClassAttrs {
    /// Keeps a predicted state label only when the schema declares `stan` AND the
    /// label is one of its values — the editor renders `stan` as a select over that
    /// list, so a value outside it would be unselectable.
    fn accept_stan(&self, labels: &[String]) -> Option<String> {
        labels
            .iter()
            .find(|l| self.stan_values.iter().any(|v| v == *l))
            .cloned()
    }
}

/// Parses the project's recognition schema into per-class attribute surfaces.
/// A missing/empty schema yields an empty map, which disables attribute writing
/// entirely rather than guessing.
#[cfg(any(feature = "inference-vision-gpu", test))]
fn parse_schema(schema_json: &str) -> HashMap<String, ClassAttrs> {
    let Ok(root) = serde_json::from_str::<Value>(schema_json) else {
        return HashMap::new();
    };
    let Some(classes) = root.get("classes").and_then(|c| c.as_array()) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for class in classes {
        let Some(name) = class.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let mut attrs = ClassAttrs::default();
        if let Some(list) = class.get("attributes").and_then(|a| a.as_array()) {
            for attr in list {
                match attr.get("name").and_then(|n| n.as_str()) {
                    Some("stan") => {
                        attrs.stan_values = attr
                            .get("list")
                            .and_then(|l| l.get("values"))
                            .and_then(|v| v.as_array())
                            .map(|vals| {
                                vals.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                    Some("kod") => attrs.has_kod = true,
                    _ => {}
                }
            }
        }
        out.insert(name.to_string(), attrs);
    }
    out
}

/// Trims a `snap_adr` result (`"33/1203 opis materiału"`) down to the bare
/// `kemler/UN` code the dataset stores in the `kod` attribute.
#[cfg(any(feature = "inference-vision-gpu", test))]
fn adr_code_only(snapped: &str) -> String {
    snapped
        .split_once(' ')
        .map(|(code, _)| code.to_string())
        .unwrap_or_else(|| snapped.to_string())
}

// ---------------------------------------------------------------------------
// Job body
// ---------------------------------------------------------------------------

/// One extracted frame waiting to be appended, with its predicted boxes. Class
/// NAMES (not ids) are carried so the COCO category ids can be resolved against
/// the FRESH file at publish time.
struct PendingFrame {
    /// Index into the job's outcome list — attributes counts back to its recording.
    outcome_idx: usize,
    source: PathBuf,
    desired_name: String,
    width: u32,
    height: u32,
    /// Carried per frame so `publish` needs no reference to the job spec.
    collision: CollisionPolicy,
    boxes: Vec<PendingBox>,
}

struct PendingBox {
    class_name: String,
    bbox: [i64; 4],
    score: f32,
    attributes: serde_json::Map<String, Value>,
}

/// Removes the job's scratch directory on drop, so an early `?` never leaks the
/// extracted frames onto the cache disk.
#[cfg(feature = "camera")]
struct ScratchDir(PathBuf);

#[cfg(feature = "camera")]
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Synchronous import body run inside `spawn_blocking`.
#[cfg(feature = "camera")]
fn run_import(job_id: &str, spec: &ImportRecordingsSpec, pool: &crate::db::DbPool) -> Result<()> {
    let started = std::time::Instant::now();
    let train_dir = spec.dataset_dir.join("train");
    let annot_path = train_dir.join("_annotations.coco.json");

    let scratch = ScratchDir(
        crate::paths::cache_dir()
            .join("ml-recog-import")
            .join(job_id),
    );
    std::fs::create_dir_all(&scratch.0)
        .with_context(|| format!("utworzenie katalogu roboczego {}", scratch.0.display()))?;

    let mut outcomes: Vec<RecordingOutcome> = Vec::with_capacity(spec.recording_refs.len());
    let mut pending: Vec<PendingFrame> = Vec::new();
    let mut frames_extracted: u64 = 0;

    for (idx, recording_ref) in spec.recording_refs.iter().enumerate() {
        if started.elapsed() > IMPORT_TIMEOUT {
            anyhow::bail!("import przekroczył limit czasu");
        }
        outcomes.push(RecordingOutcome {
            recording_ref: recording_ref.clone(),
            ..RecordingOutcome::default()
        });
        let budget = MAX_IMPORT_FRAMES.saturating_sub(frames_extracted);
        if budget == 0 {
            outcomes[idx].skipped = Some(format!(
                "osiągnięto limit {MAX_IMPORT_FRAMES} klatek na jedno zadanie"
            ));
            continue;
        }

        match extract_recording(
            pool,
            recording_ref,
            spec.fps,
            spec.collision,
            &scratch.0,
            idx,
            budget,
        ) {
            Ok(frames) => {
                frames_extracted += frames.len() as u64;
                pending.extend(frames);
            }
            // `{:#}` keeps the whole context chain, so the UI shows the real cause
            // ("plik nagrania niedostępny: …") instead of just the outermost line.
            Err(reason) => outcomes[idx].skipped = Some(format!("{reason:#}")),
        }

        let done = idx as u64 + 1;
        let snapshot = outcomes.clone();
        update_progress(job_id, |p| {
            p.recordings_done = done;
            p.frames_extracted = frames_extracted;
            p.outcomes = snapshot;
        });
    }

    if pending.is_empty() {
        let snapshot = outcomes.clone();
        update_progress(job_id, |p| {
            p.status = "succeeded".to_string();
            p.phase = "publishing".to_string();
            p.outcomes = snapshot;
        });
        return Ok(());
    }

    // `validate_spec` already rejected auto-label when the vision feature is off, so
    // a build without it never reaches the model stages.
    #[cfg(feature = "inference-vision-gpu")]
    if spec.autolabel {
        update_progress(job_id, |p| p.phase = "labeling".to_string());
        let schema = parse_schema(&super::repository::schema_get(&spec.project_id)?);
        label_frames(job_id, &mut pending, &schema, started)?;
    }

    update_progress(job_id, |p| p.phase = "publishing".to_string());
    publish(job_id, &train_dir, &annot_path, pending, &mut outcomes)
}

/// Resolves ONE recording ref and turns it into extracted frames on disk.
/// Snapshots are accepted as a single image (a snapshot IS one frame of interest —
/// the recorder captures it at the best OCR read), so `fps` does not apply to them;
/// segments are decoded with the shared ffmpeg helper.
#[cfg(feature = "camera")]
fn extract_recording(
    pool: &crate::db::DbPool,
    recording_ref: &str,
    fps: u32,
    collision: CollisionPolicy,
    scratch: &Path,
    outcome_idx: usize,
    frame_budget: u64,
) -> Result<Vec<PendingFrame>> {
    let row = crate::db::repository::get_recording_by_ref(pool, recording_ref)
        .with_context(|| format!("odczyt nagrania {recording_ref}"))?
        .ok_or_else(|| anyhow::anyhow!("nagranie nie istnieje lub zostało usunięte"))?;
    // The lookup already filters `purged_at IS NULL`; asserting it here keeps the
    // invariant local, so widening that query can never silently import purged media.
    if row.purged_at.is_some() {
        anyhow::bail!("nagranie zostało usunięte");
    }
    let src = validated_recording_path(&row.file_path)?;

    let dir = scratch.join(format!("r{outcome_idx}"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("utworzenie katalogu {}", dir.display()))?;

    let produced: Vec<PathBuf> = match row.kind.as_str() {
        "snapshot" => {
            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jpg")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
                anyhow::bail!("nieobsługiwany format migawki: .{ext}");
            }
            let dest = dir.join(format!("snap.{ext}"));
            std::fs::copy(&src, &dest)
                .with_context(|| format!("kopiowanie migawki {}", src.display()))?;
            vec![dest]
        }
        "segment" => {
            super::build_recog_dataset::extract_video_frames(&src, &dir, fps, "seg", frame_budget)?;
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jpg"))
                .collect();
            // ffmpeg's %04d numbering is lexicographically ordered, so sorting the
            // paths restores temporal order for the appended frames.
            files.sort();
            files
        }
        other => anyhow::bail!("nieobsługiwany rodzaj nagrania: {other}"),
    };

    if produced.is_empty() {
        anyhow::bail!("nagranie nie dało żadnej klatki");
    }

    let mut out = Vec::with_capacity(produced.len());
    for (i, path) in produced.into_iter().enumerate() {
        // Dimensions come from the PRODUCED jpeg: the recordings table has NULL
        // width/height for every segment, and ffprobe on a truncated fragment
        // reports the full nominal clip anyway.
        let (width, height) = match image::image_dimensions(&path) {
            Ok(dims) => dims,
            Err(e) => {
                tracing::warn!(frame = %path.display(), error = %e, "import: unreadable frame skipped");
                continue;
            }
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("jpg")
            .to_ascii_lowercase();
        out.push(PendingFrame {
            outcome_idx,
            desired_name: frame_file_name(recording_ref, i as u64 + 1, &ext),
            source: path,
            width,
            height,
            collision,
            boxes: Vec::new(),
        });
    }
    if out.is_empty() {
        anyhow::bail!("żadnej klatki nie udało się odczytać");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Auto-label stages
// ---------------------------------------------------------------------------

/// ADR placard reader. `vision::adr_ocr` is a non-Apple module (on macOS/iOS the
/// OCR stack is Apple Vision, which carries no ADR row model), so there the `kod`
/// stage has no engine and imported placards simply get no code attribute.
#[cfg(feature = "inference-vision-gpu")]
struct AdrReader {
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    engine: Option<std::sync::Arc<crate::vision::adr_ocr::AdrOcr>>,
}

#[cfg(feature = "inference-vision-gpu")]
impl AdrReader {
    fn load() -> Self {
        AdrReader {
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            engine: crate::vision::adr_ocr::get(),
        }
    }

    /// Reads a placard crop and snaps the UN number to the ADR catalog, returning
    /// the bare `kemler/UN` code the dataset stores.
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    fn read_code(&self, crop: &[u8], cw: u32, ch: u32) -> Option<String> {
        let (_kemler, un) = self.engine.as_ref()?.read_adr(crop, cw, ch)?;
        crate::vision::adr::snap_adr(&un).map(|s| adr_code_only(&s))
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn read_code(&self, _crop: &[u8], _cw: u32, _ch: u32) -> Option<String> {
        None
    }
}

/// Runs the three model stages over every pending frame. Each model is loaded ONCE
/// for the whole job. A frame that fails to decode or infer keeps zero boxes and
/// stays editable by hand — one bad frame must never abort the import.
#[cfg(feature = "inference-vision-gpu")]
fn label_frames(
    job_id: &str,
    pending: &mut [PendingFrame],
    schema: &HashMap<String, ClassAttrs>,
    started: std::time::Instant,
) -> Result<()> {
    use std::sync::Arc;

    use crate::vision::classifier_stan::StateClassifier;
    use crate::vision::detector_rfdetr::RfDetrDetector;

    let detector = RfDetrDetector::load().context("ładowanie detektora RF-DETR")?;
    let classifier = StateClassifier::load().context("ładowanie klasyfikatora stanu")?;
    let adr = AdrReader::load();

    let mut labeled: u64 = 0;
    let mut detections: u64 = 0;
    for frame in pending.iter_mut() {
        if started.elapsed() > IMPORT_TIMEOUT {
            anyhow::bail!("import przekroczył limit czasu");
        }
        labeled += 1;
        let rgb = match image::open(&frame.source) {
            Ok(img) => img.to_rgb8(),
            Err(e) => {
                tracing::warn!(frame = %frame.source.display(), error = %e, "import: decode failed");
                continue;
            }
        };
        let (w, h) = (rgb.width(), rgb.height());
        let rgb = rgb.into_raw();

        let dets = match detector.detect(&rgb, w, h) {
            Ok(dets) => dets,
            Err(e) => {
                tracing::warn!(frame = %frame.source.display(), error = %e, "import: detect failed");
                continue;
            }
        };

        // Stage 2/3 inputs: one crop per surviving box, cut once and reused by both
        // the state classifier and the ADR reader.
        let mut kept: Vec<(usize, [i64; 4], f32, String)> = Vec::new();
        let mut crops: Vec<(Arc<[u8]>, u32, u32)> = Vec::new();
        for det in dets {
            let Some(bbox) = norm_bbox_to_pixels(det.bbox, w, h) else {
                continue;
            };
            let (cw, ch) = (bbox[2] as u32, bbox[3] as u32);
            let crop = crop_rgb(&rgb, w, bbox[0] as u32, bbox[1] as u32, cw, ch);
            kept.push((crops.len(), bbox, det.score, det.klasa));
            crops.push((Arc::from(crop.into_boxed_slice()), cw, ch));
        }
        if kept.is_empty() {
            continue;
        }

        // One batched forward for the whole frame instead of one per box.
        let states = match classifier.classify_batch(&crops) {
            Ok(states) => states,
            Err(e) => {
                tracing::warn!(frame = %frame.source.display(), error = %e, "import: state classify failed");
                vec![Vec::new(); crops.len()]
            }
        };

        for (crop_idx, bbox, score, class_name) in kept {
            let class_attrs = schema.get(&class_name);
            let mut attributes = serde_json::Map::new();
            let big_enough = bbox[2] >= MIN_CROP_PX && bbox[3] >= MIN_CROP_PX;

            if big_enough {
                if let Some(attrs) = class_attrs {
                    if let Some(stan) = states
                        .get(crop_idx)
                        .and_then(|labels| attrs.accept_stan(labels))
                    {
                        attributes.insert("stan".to_string(), json!(stan));
                    }
                    // ADR codes only exist on placards, and only when the project's
                    // schema actually declares the `kod` attribute for that class.
                    if attrs.has_kod && class_name == "tablica_adr" {
                        let (crop, cw, ch) = &crops[crop_idx];
                        if let Some(code) = adr.read_code(crop, *cw, *ch) {
                            attributes.insert("kod".to_string(), json!(code));
                        }
                    }
                }
            }

            frame.boxes.push(PendingBox {
                class_name,
                bbox,
                score,
                attributes,
            });
            detections += 1;
        }

        update_progress(job_id, |p| {
            p.frames_labeled = labeled;
            p.detections = detections;
        });
    }
    Ok(())
}

/// Extracts an RGB24 rectangle from a tightly packed RGB frame (stride = w*3).
/// Coordinates are already clamped to the frame by `norm_bbox_to_pixels`.
#[cfg(feature = "inference-vision-gpu")]
fn crop_rgb(frame: &[u8], frame_w: u32, x0: u32, y0: u32, cw: u32, ch: u32) -> Vec<u8> {
    let stride = frame_w as usize * 3;
    let mut out = Vec::with_capacity(cw as usize * ch as usize * 3);
    for row in 0..ch as usize {
        let start = (y0 as usize + row) * stride + x0 as usize * 3;
        out.extend_from_slice(&frame[start..start + cw as usize * 3]);
    }
    out
}

// ---------------------------------------------------------------------------
// Publish
// ---------------------------------------------------------------------------

/// Copies the extracted frames into `train/` and appends them to the COCO file.
///
/// The COCO file is read FRESH here (not at job start) so a manual save made during
/// this minutes-long job survives: existing `images`, their `approved` flag and all
/// existing `annotations` with their `attributes` are carried over untouched, and
/// only new entries are appended. Publishing is temp + rename so a crash never
/// leaves a half-written dataset index.
fn publish(
    job_id: &str,
    train_dir: &Path,
    annot_path: &Path,
    pending: Vec<PendingFrame>,
    outcomes: &mut [RecordingOutcome],
) -> Result<()> {
    let buf =
        std::fs::read(annot_path).with_context(|| format!("odczyt {}", annot_path.display()))?;
    let mut coco: Value = serde_json::from_slice(&buf)
        .with_context(|| format!("parsowanie {}", annot_path.display()))?;

    let name_to_cat: HashMap<String, i64> = coco
        .get("categories")
        .and_then(|c| c.as_array())
        .map(|cats| {
            cats.iter()
                .filter_map(|c| {
                    Some((c.get("name")?.as_str()?.to_string(), c.get("id")?.as_i64()?))
                })
                .collect()
        })
        .unwrap_or_default();
    if name_to_cat.is_empty() {
        anyhow::bail!("COCO bez kategorii — uruchom najpierw budowę datasetu");
    }

    let images = coco
        .get("images")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();
    let annotations = coco
        .get("annotations")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();

    // Fresh, non-colliding id spaces: existing ids are sparse, so start above the max.
    let mut next_image_id = max_id(&images) + 1;
    let mut next_ann_id = max_id(&annotations) + 1;

    // Every name already in use: the COCO index AND everything on disk (a file the
    // index does not know about would still be overwritten by a plain copy).
    let mut taken: HashSet<String> = images
        .iter()
        .filter_map(|im| im.get("file_name").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect();
    for entry in std::fs::read_dir(train_dir)?.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            taken.insert(name.to_string());
        }
    }

    let mut new_images: Vec<Value> = Vec::new();
    let mut new_anns: Vec<Value> = Vec::new();
    let mut images_added: u64 = 0;
    let mut detections: u64 = 0;

    for frame in pending {
        let outcome = &mut outcomes[frame.outcome_idx];
        let Some(name) = unique_frame_name(&frame.desired_name, &taken, frame.collision) else {
            outcome.skipped_frames += 1;
            continue;
        };
        let dest = train_dir.join(&name);
        if let Err(e) = std::fs::copy(&frame.source, &dest) {
            tracing::warn!(frame = %frame.source.display(), error = %e, "import: frame copy failed");
            outcome.skipped_frames += 1;
            continue;
        }
        taken.insert(name.clone());

        let image_id = next_image_id;
        next_image_id += 1;
        // Imported frames are unreviewed by definition — never `approved`.
        new_images.push(json!({
            "id": image_id,
            "file_name": name,
            "width": frame.width,
            "height": frame.height,
            "approved": false,
        }));
        images_added += 1;
        outcome.frames += 1;

        for b in frame.boxes {
            let Some(&category_id) = name_to_cat.get(&b.class_name) else {
                continue;
            };
            let mut ann = json!({
                "id": next_ann_id,
                "image_id": image_id,
                "category_id": category_id,
                "bbox": b.bbox,
                "area": b.bbox[2] * b.bbox[3],
                "iscrowd": 0,
                "score": b.score,
                "predicted": true,
            });
            if !b.attributes.is_empty() {
                ann["attributes"] = Value::Object(b.attributes);
            }
            new_anns.push(ann);
            next_ann_id += 1;
            detections += 1;
            outcome.detections += 1;
        }
    }

    let root = coco
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("COCO nie jest obiektem"))?;
    let mut merged_images = images;
    merged_images.extend(new_images);
    root.insert("images".to_string(), Value::Array(merged_images));
    let mut merged_anns = annotations;
    merged_anns.extend(new_anns);
    root.insert("annotations".to_string(), Value::Array(merged_anns));

    let tmp = annot_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&coco)?)
        .with_context(|| format!("zapis {}", tmp.display()))?;
    std::fs::rename(&tmp, annot_path)
        .with_context(|| format!("publikacja {}", annot_path.display()))?;

    let snapshot = outcomes.to_vec();
    update_progress(job_id, |p| {
        p.status = "succeeded".to_string();
        p.images_added = images_added;
        p.detections = detections;
        p.outcomes = snapshot;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_converts_to_absolute_pixels() {
        let px = norm_bbox_to_pixels([0.25, 0.5, 0.5, 0.25], 800, 400).unwrap();
        assert_eq!(px, [200, 200, 400, 100]);
    }

    #[test]
    fn bbox_clamps_at_frame_edges() {
        // Box running off the right/bottom edge is TRUNCATED, not shifted inwards.
        let px = norm_bbox_to_pixels([0.9, 0.9, 0.5, 0.5], 100, 100).unwrap();
        assert_eq!(px, [90, 90, 10, 10]);
        // Negative origin is clamped to 0 and the width shrinks accordingly.
        let px = norm_bbox_to_pixels([-0.2, -0.1, 0.5, 0.5], 100, 100).unwrap();
        assert_eq!(px, [0, 0, 30, 40]);
        // Fully outside / degenerate boxes are dropped.
        assert!(norm_bbox_to_pixels([1.2, 0.1, 0.3, 0.3], 100, 100).is_none());
        assert!(norm_bbox_to_pixels([0.1, 0.1, 0.0, 0.3], 100, 100).is_none());
        assert!(norm_bbox_to_pixels([f32::NAN, 0.1, 0.3, 0.3], 100, 100).is_none());
    }

    #[test]
    fn new_ids_start_above_a_sparse_maximum() {
        let items = vec![json!({"id": 3}), json!({"id": 14062}), json!({"id": 7})];
        assert_eq!(max_id(&items) + 1, 14063);
        assert_eq!(max_id(&[]) + 1, 1);
    }

    #[test]
    fn collision_is_suffixed_or_skipped_never_overwritten() {
        let mut taken = HashSet::new();
        taken.insert("rec_clip_a_f0001.jpg".to_string());
        taken.insert("rec_clip_a_f0001_1.jpg".to_string());

        let name =
            unique_frame_name("rec_clip_a_f0001.jpg", &taken, CollisionPolicy::Suffix).unwrap();
        assert_eq!(name, "rec_clip_a_f0001_2.jpg");
        assert!(!taken.contains(&name));

        assert!(unique_frame_name("rec_clip_a_f0001.jpg", &taken, CollisionPolicy::Skip).is_none());
        assert_eq!(
            unique_frame_name("fresh.jpg", &taken, CollisionPolicy::Skip).unwrap(),
            "fresh.jpg"
        );
    }

    #[test]
    fn frame_names_are_traceable_and_path_safe() {
        assert_eq!(
            frame_file_name("clip_9f1a/../etc", 7, "jpg"),
            "rec_clip_9f1a____etc_f0007.jpg"
        );
        // A PNG snapshot keeps its real extension.
        assert_eq!(frame_file_name("snap_a", 1, "png"), "rec_snap_a_f0001.png");
    }

    #[test]
    fn append_preserves_existing_approved_and_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let train = dir.path().join("train");
        std::fs::create_dir_all(&train).unwrap();
        let annot = train.join("_annotations.coco.json");

        let existing = json!({
            "categories": [{"id": 1, "name": "tablica_adr"}],
            "images": [{"id": 41, "file_name": "1.jpg", "width": 640, "height": 480, "approved": true}],
            "annotations": [{
                "id": 14062, "image_id": 41, "category_id": 1,
                "bbox": [484, 261, 130, 121], "area": 15730, "iscrowd": 0,
                "attributes": {"stan": "czysta", "kod": "33/1203"}
            }],
        });
        std::fs::write(&annot, serde_json::to_vec(&existing).unwrap()).unwrap();

        // A real JPEG so the copy has content; dimensions come from PendingFrame.
        let src = dir.path().join("frame.jpg");
        image::RgbImage::new(8, 8).save(&src).unwrap();

        let pending = vec![PendingFrame {
            outcome_idx: 0,
            source: src,
            desired_name: "rec_clip_a_f0001.jpg".to_string(),
            width: 1920,
            height: 1080,
            collision: CollisionPolicy::Suffix,
            boxes: vec![PendingBox {
                class_name: "tablica_adr".to_string(),
                bbox: [10, 20, 30, 40],
                score: 0.87,
                attributes: [("stan".to_string(), json!("brudna"))]
                    .into_iter()
                    .collect(),
            }],
        }];
        let mut outcomes = vec![RecordingOutcome {
            recording_ref: "clip_a".to_string(),
            ..RecordingOutcome::default()
        }];
        publish("job", &train, &annot, pending, &mut outcomes).unwrap();

        let out: Value = serde_json::from_slice(&std::fs::read(&annot).unwrap()).unwrap();
        let images = out["images"].as_array().unwrap();
        let anns = out["annotations"].as_array().unwrap();

        // Existing work survives byte-for-byte.
        assert_eq!(images[0], existing["images"][0]);
        assert_eq!(anns[0], existing["annotations"][0]);

        // The appended image is unreviewed and its ids clear the sparse maximum.
        assert_eq!(images[1]["approved"], json!(false));
        assert_eq!(images[1]["width"], json!(1920));
        assert_eq!(images[1]["id"], json!(42));
        assert_eq!(anns[1]["id"], json!(14063));
        assert_eq!(anns[1]["image_id"], json!(42));
        assert_eq!(anns[1]["predicted"], json!(true));
        assert_eq!(anns[1]["area"], json!(1200));
        assert_eq!(anns[1]["attributes"]["stan"], json!("brudna"));
        assert!(train.join("rec_clip_a_f0001.jpg").is_file());
        assert_eq!(outcomes[0].frames, 1);
        assert_eq!(outcomes[0].detections, 1);
    }

    #[test]
    fn publish_never_overwrites_an_image_already_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let train = dir.path().join("train");
        std::fs::create_dir_all(&train).unwrap();
        let annot = train.join("_annotations.coco.json");
        std::fs::write(
            &annot,
            serde_json::to_vec(&json!({
                "categories": [{"id": 1, "name": "tablica_adr"}],
                "images": [],
                "annotations": [],
            }))
            .unwrap(),
        )
        .unwrap();
        // An orphan file the COCO index does not know about must still be respected.
        std::fs::write(train.join("rec_clip_a_f0001.jpg"), b"original").unwrap();

        let src = dir.path().join("frame.jpg");
        image::RgbImage::new(8, 8).save(&src).unwrap();
        let pending = vec![PendingFrame {
            outcome_idx: 0,
            source: src,
            desired_name: "rec_clip_a_f0001.jpg".to_string(),
            width: 8,
            height: 8,
            collision: CollisionPolicy::Suffix,
            boxes: Vec::new(),
        }];
        let mut outcomes = vec![RecordingOutcome::default()];
        publish("job", &train, &annot, pending, &mut outcomes).unwrap();

        assert_eq!(
            std::fs::read(train.join("rec_clip_a_f0001.jpg")).unwrap(),
            b"original"
        );
        assert!(train.join("rec_clip_a_f0001_1.jpg").is_file());
    }

    #[test]
    fn attributes_are_written_only_when_the_schema_declares_them() {
        let schema = parse_schema(
            &json!({"classes": [
                {"name": "tablica_adr", "attributes": [
                    {"name": "kod", "type": "ocr"},
                    {"name": "stan", "type": "list", "list": {"values": ["czysta", "brudna"]}}
                ]},
                {"name": "termometr", "attributes": []}
            ]})
            .to_string(),
        );

        let adr = schema.get("tablica_adr").unwrap();
        assert!(adr.has_kod);
        assert_eq!(
            adr.accept_stan(&["brudna".to_string()]),
            Some("brudna".to_string())
        );
        // A predicted label outside the schema's value list is not written — the
        // editor renders `stan` as a select over exactly those values.
        assert_eq!(adr.accept_stan(&["uszkodzona".to_string()]), None);

        let termometr = schema.get("termometr").unwrap();
        assert!(!termometr.has_kod);
        assert_eq!(termometr.accept_stan(&["czysta".to_string()]), None);

        // A class absent from the schema gets no attributes at all.
        assert!(schema.get("nalepka_9").is_none());
        // A missing schema disables attribute writing rather than guessing.
        assert!(parse_schema("{}").is_empty());
    }

    #[test]
    fn adr_snap_result_is_reduced_to_the_bare_code() {
        assert_eq!(adr_code_only("33/1203 aceton"), "33/1203");
        assert_eq!(adr_code_only("30/1202"), "30/1202");
    }

    #[test]
    fn path_traversal_file_paths_are_rejected() {
        let base = Path::new("/home/u/.tentaflow/recordings");
        assert!(path_is_contained(
            Path::new("/home/u/.tentaflow/recordings/cam1/segments/clip_a.mp4"),
            Some(base)
        ));
        // Classic DB-tamper vector.
        assert!(!path_is_contained(Path::new("/etc/passwd"), Some(base)));
        // Planted tree that satisfies the segment scan but lies outside the base.
        assert!(!path_is_contained(
            Path::new("/tmp/evil/.tentaflow/recordings/blob"),
            Some(base)
        ));
        // Sibling directory sharing the base's prefix as a string.
        assert!(!path_is_contained(
            Path::new("/home/u/.tentaflow/recordings-evil/clip.mp4"),
            Some(base)
        ));
        // Base unresolvable: the segment scan alone still rejects the traversal.
        assert!(!path_is_contained(Path::new("/etc/passwd"), None));
    }

    fn spec(fps: u32) -> ImportRecordingsSpec {
        ImportRecordingsSpec {
            dataset_id: "d".to_string(),
            project_id: "p".to_string(),
            owner_user_id: "u".to_string(),
            dataset_dir: PathBuf::from("/nonexistent"),
            recording_refs: vec!["clip_a".to_string()],
            fps,
            autolabel: false,
            collision: CollisionPolicy::Suffix,
        }
    }

    #[test]
    fn fps_out_of_range_is_rejected() {
        assert!(validate_spec(&spec(0)).is_err());
        assert!(validate_spec(&spec(FPS_MAX + 1)).is_err());
        assert!(validate_spec(&spec(1)).is_ok());
        assert!(validate_spec(&spec(10)).is_ok());
    }

    #[test]
    fn recording_selection_caps_are_enforced() {
        let mut s = spec(5);
        s.recording_refs.clear();
        assert!(validate_spec(&s).is_err());
        s.recording_refs = (0..MAX_RECORDINGS_PER_JOB + 1)
            .map(|i| i.to_string())
            .collect();
        assert!(validate_spec(&s).is_err());
    }

    #[test]
    fn collision_policy_parses_from_the_wire() {
        assert_eq!(
            "suffix".parse::<CollisionPolicy>().unwrap(),
            CollisionPolicy::Suffix
        );
        assert_eq!(
            "skip".parse::<CollisionPolicy>().unwrap(),
            CollisionPolicy::Skip
        );
        assert!("overwrite".parse::<CollisionPolicy>().is_err());
    }
}
