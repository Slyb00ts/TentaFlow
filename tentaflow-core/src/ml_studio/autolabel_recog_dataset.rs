// ===== File: ml_studio/autolabel_recog_dataset.rs — RF-DETR batch auto-label =====
//
// Runs the IN-CORE RF-DETR ADR detector over every image of a recognition COCO
// dataset and writes the detections back into `_annotations.coco.json` as editable
// COCO annotations — a "starting point" the user corrects in the existing editor.
// No external service, no HTTP, no model deploy: the detector is the same always-on
// camera-CV path (`vision::detector_rfdetr`), loaded ONCE per job.
//
// Decoding + per-image inference is MINUTES of work for a large dataset, so (like
// `build_recog_dataset`) it runs as an async background job: the request returns a
// job id immediately and the UI polls `AutolabelProgress`. One auto-label per
// dataset at a time. `mode` is "only_empty" (default — never overwrite manual
// corrections) or "overwrite" (replace every image's annotations).
//
// The detector is feature-gated behind `inference-vision-gpu` (mirroring the camera
// CV path). When the feature is off, `spawn_autolabel` returns a clear error instead
// of failing to compile.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Live progress of an async auto-label job, polled by the UI. `status` is
/// "running" | "succeeded" | "failed". `project_id`/`owner_user_id` are stored so
/// the status handler can authorize the caller against the job's project (a job id
/// alone must not expose progress to an unrelated user).
#[derive(Clone, Debug)]
pub struct AutolabelProgress {
    pub status: String,
    pub images_total: u64,
    pub images_done: u64,
    pub detections: u64,
    /// Detections dropped because their class name is not among the dataset's COCO
    /// categories. A fully-mismatched category set finishes with 0 written and a
    /// non-zero count here, so the UI can hint at the cause instead of "0 detections".
    pub skipped_unknown: u64,
    pub project_id: String,
    pub owner_user_id: String,
    pub error: Option<String>,
}

impl Default for AutolabelProgress {
    fn default() -> Self {
        AutolabelProgress {
            status: "running".to_string(),
            images_total: 0,
            images_done: 0,
            detections: 0,
            skipped_unknown: 0,
            project_id: String::new(),
            owner_user_id: String::new(),
            error: None,
        }
    }
}

static PROGRESS: OnceLock<Mutex<std::collections::HashMap<String, AutolabelProgress>>> =
    OnceLock::new();

fn progress_map() -> &'static Mutex<std::collections::HashMap<String, AutolabelProgress>> {
    PROGRESS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn set_progress(job_id: &str, p: AutolabelProgress) {
    if let Ok(mut m) = progress_map().lock() {
        m.insert(job_id.to_string(), p);
    }
}

fn update_progress(job_id: &str, f: impl FnOnce(&mut AutolabelProgress)) {
    if let Ok(mut m) = progress_map().lock() {
        if let Some(p) = m.get_mut(job_id) {
            f(p);
        }
    }
}

/// Current progress of an auto-label job (None when the job id is unknown).
pub fn autolabel_progress(job_id: &str) -> Option<AutolabelProgress> {
    progress_map().lock().ok()?.get(job_id).cloned()
}

// Per-dataset guard shared by auto-label AND recording-import: only one such job
// may run at a time for a given dataset, because both republish the same
// `_annotations.coco.json` and must never interleave. A second concurrent request
// for the same dataset (of either kind) is rejected.
static ACTIVE: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn active() -> &'static Mutex<std::collections::HashSet<String>> {
    ACTIVE.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

pub(crate) fn try_claim_dataset(dataset_id: &str) -> bool {
    if let Ok(mut s) = active().lock() {
        s.insert(dataset_id.to_string())
    } else {
        false
    }
}

pub(crate) fn release_dataset(dataset_id: &str) {
    if let Ok(mut s) = active().lock() {
        s.remove(dataset_id);
    }
}

/// Starts an async auto-label of a COCO dataset's `train/` split with the in-core
/// RF-DETR detector. `dataset_dir` is the resolved on-disk `coco_path` root.
/// Returns the job id used for status polling, or an error if a job is already
/// running for this dataset, the threshold/mode are invalid, or the vision feature
/// is not compiled in.
pub fn spawn_autolabel(
    dataset_id: String,
    project_id: String,
    owner_user_id: String,
    dataset_dir: PathBuf,
    threshold: f64,
    mode: String,
) -> Result<String> {
    // The RF-DETR detector hard-drops detections with score <= 0.5 internally
    // (vision::detector_rfdetr::SCORE_THRESHOLD), so a threshold below 0.5 can never
    // yield anything extra. Clamp to that floor to keep the knob honest.
    if !(0.5..=1.0).contains(&threshold) {
        anyhow::bail!("próg musi być w zakresie 0.5..=1.0");
    }
    let mode = match mode.as_str() {
        "only_empty" | "overwrite" => mode,
        other => anyhow::bail!("nieznany tryb: {} (oczekiwano only_empty|overwrite)", other),
    };

    #[cfg(not(feature = "inference-vision-gpu"))]
    {
        let _ = (
            dataset_id,
            project_id,
            owner_user_id,
            dataset_dir,
            threshold,
            mode,
        );
        anyhow::bail!(
            "auto-etykietowanie wymaga wbudowanego detektora wizyjnego (feature inference-vision-gpu) — niedostępne w tej kompilacji"
        );
    }

    #[cfg(feature = "inference-vision-gpu")]
    {
        let train_dir = dataset_dir.join("train");
        let annot_path = train_dir.join("_annotations.coco.json");
        if !annot_path.is_file() {
            anyhow::bail!("dataset nie zawiera train/_annotations.coco.json");
        }
        if !try_claim_dataset(&dataset_id) {
            anyhow::bail!(
                "auto-etykietowanie tego datasetu już trwa — poczekaj na jego zakończenie"
            );
        }

        let job_id = uuid::Uuid::new_v4().to_string();
        set_progress(
            &job_id,
            AutolabelProgress {
                project_id: project_id.clone(),
                owner_user_id: owner_user_id.clone(),
                ..AutolabelProgress::default()
            },
        );

        let job_id_task = job_id.clone();
        tokio::spawn(async move {
            let jid = job_id_task.clone();
            let did = dataset_id.clone();
            // Decode + GPU inference is blocking — keep it off the async worker.
            let result = tokio::task::spawn_blocking(move || {
                run_autolabel(&job_id_task, &train_dir, threshold as f32, &mode)
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(job_id = %jid, error = %err, "recog dataset auto-label failed");
                    update_progress(&jid, |p| {
                        p.status = "failed".to_string();
                        p.error = Some(err.to_string());
                    });
                }
                Err(join_err) => {
                    tracing::warn!(job_id = %jid, error = %join_err, "recog dataset auto-label task panicked");
                    update_progress(&jid, |p| {
                        p.status = "failed".to_string();
                        p.error = Some(format!("auto-label task failed: {}", join_err));
                    });
                }
            }
            release_dataset(&did);
        });

        Ok(job_id)
    }
}

/// Synchronous auto-label body run inside `spawn_blocking`. Loads the RF-DETR
/// detector ONCE, then for each image in the COCO file decodes it to RGB8, runs the
/// detector and writes the surviving detections (`score >= threshold`) into the COCO
/// `annotations` array. `category_id = class_id + 1` (the 17 RF-DETR classes map to
/// COCO categories 1..=17 in the same order). The COCO file is rewritten atomically
/// (temp + rename), preserving `categories`/`images`; only `annotations` change.
#[cfg(feature = "inference-vision-gpu")]
fn run_autolabel(job_id: &str, train_dir: &Path, threshold: f32, mode: &str) -> Result<()> {
    use crate::vision::detector_rfdetr::RfDetrDetector;

    let annot_path = train_dir.join("_annotations.coco.json");
    let buf =
        std::fs::read(&annot_path).with_context(|| format!("odczyt {}", annot_path.display()))?;
    let coco: Value = serde_json::from_slice(&buf)
        .with_context(|| format!("parsowanie {}", annot_path.display()))?;

    // Map RF-DETR class name → COCO category_id via the dataset's own categories.
    // The detector's class index order matches the canonical 17-class order, which
    // is exactly how categories were seeded (id = class_id + 1), so we resolve the
    // category id by class NAME (robust even if a future dataset renumbers).
    let name_to_cat: std::collections::HashMap<String, i64> = coco
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

    // Images keyed by COCO image id, plus the ordered list to iterate.
    let images: Vec<(i64, String)> = coco
        .get("images")
        .and_then(|i| i.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|im| {
                    Some((
                        im.get("id")?.as_i64()?,
                        im.get("file_name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let images_total = images.len() as u64;
    update_progress(job_id, |p| p.images_total = images_total);
    if images.is_empty() {
        anyhow::bail!("dataset nie zawiera obrazów");
    }

    // In only_empty mode, images that already carry annotations are skipped so manual
    // corrections are never overwritten.
    let mut existing_counts: std::collections::HashMap<i64, u32> = std::collections::HashMap::new();
    if let Some(anns) = coco.get("annotations").and_then(|a| a.as_array()) {
        for a in anns {
            if let Some(iid) = a.get("image_id").and_then(|v| v.as_i64()) {
                *existing_counts.entry(iid).or_insert(0) += 1;
            }
        }
    }

    let detector = RfDetrDetector::load().context("ładowanie detektora RF-DETR")?;

    // Detections produced per image id during inference. Published only after a FRESH
    // re-read of the COCO file below, so a manual save made DURING this (minutes-long)
    // job is never clobbered by the stale start-of-job snapshot.
    let mut produced: std::collections::HashMap<i64, Vec<Value>> = std::collections::HashMap::new();

    let mut total_dets: u64 = 0;
    let mut skipped_unknown: u64 = 0;
    let mut done: u64 = 0;
    for (image_id, file_name) in &images {
        done += 1;
        // In only_empty mode skip images that already had annotations at job start —
        // running the detector on them would be wasted work (they are kept regardless
        // on the fresh re-read below).
        let skip = mode != "overwrite" && existing_counts.get(image_id).copied().unwrap_or(0) > 0;
        if !skip {
            let img_path = train_dir.join(file_name);
            // A single unreadable/corrupt image must not abort the whole job — log and
            // skip it (it stays without auto-labels, editable by hand).
            match decode_rgb(&img_path) {
                Ok((rgb, w, h)) => match detector.detect(&rgb, w, h) {
                    Ok(dets) => {
                        for d in dets {
                            if d.score < threshold {
                                continue;
                            }
                            let Some(&cat_id) = name_to_cat.get(&d.klasa) else {
                                skipped_unknown += 1;
                                continue;
                            };
                            // The detector returns a NORMALIZED bbox [x, y, w, h] in
                            // 0..1; COCO stores pixels in the source image.
                            let bx = (d.bbox[0] * w as f32).round().max(0.0) as i64;
                            let by = (d.bbox[1] * h as f32).round().max(0.0) as i64;
                            let bw = (d.bbox[2] * w as f32).round().max(0.0) as i64;
                            let bh = (d.bbox[3] * h as f32).round().max(0.0) as i64;
                            // `score` + `predicted` mark the box as a model prediction so
                            // the annotation editor renders it dashed with a confidence
                            // label until a human accepts it.
                            produced.entry(*image_id).or_default().push(json!({
                                "image_id": image_id,
                                "category_id": cat_id,
                                "bbox": [bx, by, bw, bh],
                                "area": bw * bh,
                                "iscrowd": 0,
                                "score": d.score,
                                "predicted": true,
                            }));
                            total_dets += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(image = %img_path.display(), error = %e, "auto-label detect failed")
                    }
                },
                Err(e) => {
                    tracing::warn!(image = %img_path.display(), error = %e, "auto-label decode failed")
                }
            }
        }
        let dets_snapshot = total_dets;
        let skipped_snapshot = skipped_unknown;
        update_progress(job_id, |p| {
            p.images_done = done;
            p.detections = dets_snapshot;
            p.skipped_unknown = skipped_snapshot;
        });
    }

    // Re-read the CURRENT file just before publishing so any manual annotation saved
    // during this job is preserved (we merge against the fresh state, not the stale
    // start-of-job snapshot). categories/images come from the fresh read too.
    let fresh_buf = std::fs::read(&annot_path)
        .with_context(|| format!("ponowny odczyt {}", annot_path.display()))?;
    let mut fresh: Value = serde_json::from_slice(&fresh_buf)
        .with_context(|| format!("ponowne parsowanie {}", annot_path.display()))?;

    // Images that currently (FRESH) have at least one annotation. In only_empty mode
    // we keep ALL current annotations and only add detector boxes for images that have
    // zero annotations in the fresh file.
    let mut fresh_counts: std::collections::HashMap<i64, u32> = std::collections::HashMap::new();
    if let Some(anns) = fresh.get("annotations").and_then(|a| a.as_array()) {
        for a in anns {
            if let Some(iid) = a.get("image_id").and_then(|v| v.as_i64()) {
                *fresh_counts.entry(iid).or_insert(0) += 1;
            }
        }
    }

    // New annotation ids start above the current maximum in the FRESH file so kept ids
    // (including any saved mid-job) never collide with the boxes we add.
    let mut next_ann_id: i64 = fresh
        .get("annotations")
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|a| a.get("id").and_then(|v| v.as_i64()))
                .max()
        })
        .unwrap_or(0)
        + 1;

    // Build the published annotation set. overwrite: start empty (replace is the
    // intended behavior). only_empty: keep every current annotation untouched.
    let mut new_anns: Vec<Value> = if mode == "overwrite" {
        Vec::new()
    } else {
        fresh
            .get("annotations")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
    };

    // Stable image order so assigned ids are deterministic across runs.
    let mut produced_ids: Vec<i64> = produced.keys().copied().collect();
    produced_ids.sort_unstable();
    for image_id in produced_ids {
        // only_empty: only fill images that are empty in the fresh file. overwrite:
        // every image's detector boxes are added (the array started empty).
        if mode != "overwrite" && fresh_counts.get(&image_id).copied().unwrap_or(0) > 0 {
            continue;
        }
        if let Some(boxes) = produced.remove(&image_id) {
            for mut b in boxes {
                if let Some(obj) = b.as_object_mut() {
                    obj.insert("id".to_string(), json!(next_ann_id));
                }
                new_anns.push(b);
                next_ann_id += 1;
            }
        }
    }

    if let Some(arr) = fresh.get_mut("annotations").and_then(|a| a.as_array_mut()) {
        *arr = new_anns;
    } else {
        fresh
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("COCO nie jest obiektem"))?
            .insert("annotations".to_string(), Value::Array(new_anns));
    }

    // Atomic publish: temp + rename so a crash never leaves a half-written COCO file.
    let tmp = annot_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&fresh)?)
        .with_context(|| format!("zapis {}", tmp.display()))?;
    std::fs::rename(&tmp, &annot_path)
        .with_context(|| format!("publikacja {}", annot_path.display()))?;

    update_progress(job_id, |p| {
        p.status = "succeeded".to_string();
        p.images_done = images_total;
        p.detections = total_dets;
        p.skipped_unknown = skipped_unknown;
    });
    Ok(())
}

/// Decodes an image file to a tightly packed RGB8 buffer + its dimensions.
#[cfg(feature = "inference-vision-gpu")]
fn decode_rgb(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    let dyn_img = image::open(path).with_context(|| format!("dekodowanie {}", path.display()))?;
    let rgb = dyn_img.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    Ok((rgb.into_raw(), w, h))
}
