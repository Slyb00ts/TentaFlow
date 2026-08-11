// =============================================================================
// File: vision/onnx_cv.rs — generic ONNX runner for registered vision models
// =============================================================================
//
// Runner silnika `onnx-cv`: serwuje DYNAMICZNE modele ONNX z rejestru
// `vision_models` (wytrenowane w ML Studio albo zaimportowane) bez rekompilacji
// binarki. Kontrakty v1: `detect`/`rfdetr` (wyjścia `dets`+`labels`, postprocess
// współdzielony z wbudowanym detektorem ADR) oraz `classify`/`softmax`
// (logits → softmax → argmax, semantyka jak `classifier_stan`). Wbudowane
// silniki (rfdetr-adr / nalepka-stan / plate-ocr / onnx-ocr / apple-ocr) NIE
// przechodzą przez ten runner — rejestr obsługuje wyłącznie modele dynamiczne.
//
// Sesje ort są cache'owane per `model_name` z limitem LRU
// (`[vision] onnx_cv_max_models`, domyślnie 4); sha256 pliku jest
// weryfikowane przy pierwszym załadowaniu (mismatch = odmowa). Każdy model ma
// własny podkatalog engine-cache TensorRT (`trt-cache/<model_name>/`), żeby
// plany silników różnych grafów się nie mieszały. Wpis modelu trzyma PULĘ N
// niezależnych sesji (`[vision] onnx_cv_sessions_per_model`, domyślnie 1)
// wybieranych round-robin — `Session::run` wymaga `&mut self`, więc dopiero
// kilka sesji tego samego grafu pozwala na równoległe forwardy na GPU.

#![cfg(feature = "vision-ort")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use tracing::info;

use crate::db::repository::VisionModelRow;
use crate::paths;
use crate::services::runtime::local_cv::CvFrameLocal;
use crate::vision::ort_common::SessionPool;
use tentaflow_protocol::{CameraCvResult, CvDetection};

// The resident-model LRU cap comes from `[vision] onnx_cv_max_models` (each
// entry owns a whole [`SessionPool`], so the resident VRAM ceiling is
// `max_models * sessions_per_model` model copies). How many independent ort
// sessions each resident model keeps comes from
// `[vision] onnx_cv_sessions_per_model`: `ort::Session::run` takes `&mut self`,
// so one shared session serializes every forward of that model; N sessions
// (each its own `&mut`, same ONNX graph) checked out round-robin let N forwards
// run concurrently on the GPU. Default 1 is byte-identical to the historical
// single-`Mutex<Session>` behavior.

/// Parsed-once pool size from `[vision] onnx_cv_sessions_per_model`, clamped to
/// `1..=ort_common::MAX_SESSIONS_PER_MODEL`.
fn sessions_per_model() -> usize {
    static N: OnceLock<usize> = OnceLock::new();
    *N.get_or_init(|| {
        crate::vision::ort_common::pool_size(
            crate::vision::settings::get().onnx_cv_sessions_per_model,
        )
    })
}

/// Parsed `preprocess_json`: `{resolution, mean, std, layout}`. Mean/std
/// default to the ImageNet transform (the training default for both RF-DETR
/// and the timm classifiers); layout accepts only `nchw`.
#[derive(Debug, Clone, Deserialize)]
struct PreprocessSpec {
    resolution: u32,
    #[serde(default = "imagenet_mean")]
    mean: [f32; 3],
    #[serde(default = "imagenet_std")]
    std: [f32; 3],
    #[serde(default = "default_layout")]
    layout: String,
}

fn imagenet_mean() -> [f32; 3] {
    [0.485, 0.456, 0.406]
}
fn imagenet_std() -> [f32; 3] {
    [0.229, 0.224, 0.225]
}
fn default_layout() -> String {
    "nchw".to_string()
}

impl PreprocessSpec {
    fn parse(preprocess_json: &str) -> Result<Self> {
        let spec: PreprocessSpec =
            serde_json::from_str(preprocess_json).context("parse preprocess_json")?;
        if !(32..=4096).contains(&spec.resolution) {
            bail!(
                "preprocess resolution {} out of range (32..=4096)",
                spec.resolution
            );
        }
        if !spec.layout.eq_ignore_ascii_case("nchw") {
            bail!(
                "unsupported preprocess layout '{}' (only nchw)",
                spec.layout
            );
        }
        if spec.std.iter().any(|v| *v <= 0.0) {
            bail!("preprocess std must be positive");
        }
        Ok(spec)
    }
}

/// Output contract of a registered model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Contract {
    Rfdetr,
    Softmax,
}

/// One resident model: a pool of ort sessions + everything postprocess needs.
struct CachedModel {
    pool: Arc<SessionPool>,
    classes: Vec<String>,
    pre: PreprocessSpec,
    contract: Contract,
    default_threshold: Option<f32>,
    /// sha256 the session was built from; a registry update (new file hash)
    /// invalidates the cache entry and forces a reload.
    sha256: String,
}

/// Pure LRU bookkeeping over model names, separated from session loading so
/// the eviction policy is unit-testable without ort/GPU.
struct LruIndex {
    /// Most-recently-used last.
    order: Vec<String>,
    cap: usize,
}

impl LruIndex {
    fn new(cap: usize) -> Self {
        Self {
            order: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Marks `name` as most-recently-used and returns the names to evict to
    /// stay within `cap` (oldest first). The returned names are already
    /// removed from the index.
    fn touch(&mut self, name: &str) -> Vec<String> {
        self.order.retain(|n| n != name);
        self.order.push(name.to_string());
        let mut evicted = Vec::new();
        while self.order.len() > self.cap {
            evicted.push(self.order.remove(0));
        }
        evicted
    }

    fn remove(&mut self, name: &str) {
        self.order.retain(|n| n != name);
    }
}

struct SessionCache {
    models: HashMap<String, Arc<CachedModel>>,
    lru: LruIndex,
}

fn cache() -> &'static Mutex<SessionCache> {
    static CACHE: OnceLock<Mutex<SessionCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let cap = crate::vision::settings::get().onnx_cv_max_models.max(1);
        Mutex::new(SessionCache {
            models: HashMap::new(),
            lru: LruIndex::new(cap),
        })
    })
}

fn lock_cache() -> std::sync::MutexGuard<'static, SessionCache> {
    cache().lock().unwrap_or_else(|e| e.into_inner())
}

/// Per-model load locks (singleflight): concurrent first uses of the same
/// model serialize on one lock, so exactly one session build runs (waiters
/// block, then hit the cache) — no duplicate VRAM spikes and no races on the
/// per-model TRT cache directory. Entries are one `Arc<Mutex<()>>` per model
/// name ever loaded — bounded and tiny, so they are never pruned.
fn load_lock_for(model_name: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        guard
            .entry(model_name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// Hex-encoded sha256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Loads (or returns the cached) session for a registry row. Verifies the
/// sha256 of the model bytes on first load and builds the session FROM THOSE
/// VERIFIED BYTES (`commit_from_memory`) — hashing and loading can never see
/// different file contents. A mismatch refuses the model instead of serving
/// unverified weights. Row updates (different sha256) drop the stale entry
/// and reload. First loads of the same model are singleflighted per name.
fn get_or_load(row: &VisionModelRow) -> Result<Arc<CachedModel>> {
    let cache_hit = |guard: &mut SessionCache| -> Option<Arc<CachedModel>> {
        let existing = guard.models.get(&row.model_name)?;
        // A registry update (new hash) OR a poisoned pool (a panicked forward)
        // invalidates the cached entry and forces a fresh rebuild.
        if existing.sha256 != row.sha256 || existing.pool.is_poisoned() {
            guard.models.remove(&row.model_name);
            guard.lru.remove(&row.model_name);
            return None;
        }
        let existing = Arc::clone(existing);
        let evicted = guard.lru.touch(&row.model_name);
        for name in evicted {
            guard.models.remove(&name);
            info!("[onnx-cv] evicted session '{}' (LRU cap)", name);
        }
        Some(existing)
    };

    if let Some(existing) = cache_hit(&mut lock_cache()) {
        return Ok(existing);
    }

    // Singleflight: only one thread builds the session; late arrivals block
    // here and then find the freshly inserted entry in the cache re-check.
    let model_lock = load_lock_for(&row.model_name);
    let _load_guard = model_lock.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = cache_hit(&mut lock_cache()) {
        return Ok(existing);
    }

    let contract = match row.output_contract.as_str() {
        "rfdetr" => Contract::Rfdetr,
        "softmax" => Contract::Softmax,
        other => bail!("unknown output_contract '{other}'"),
    };
    let classes: Vec<String> =
        serde_json::from_str(&row.classes_json).context("parse classes_json")?;
    if classes.is_empty() {
        bail!("vision model '{}' has no classes", row.model_name);
    }
    let pre = PreprocessSpec::parse(&row.preprocess_json)
        .with_context(|| format!("vision model '{}'", row.model_name))?;

    let model_path = paths::vision_models_dir().join(&row.file_name);
    let bytes =
        std::fs::read(&model_path).with_context(|| format!("read {}", model_path.display()))?;
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(&row.sha256) {
        bail!(
            "vision model '{}' sha256 mismatch: file {} has {}, registry expects {} — \
             refusing to load",
            row.model_name,
            model_path.display(),
            actual,
            row.sha256
        );
    }

    crate::vision::ort_common::ensure_ort_dylib();
    let trt_cache_root = paths::vision_models_dir()
        .join("trt-cache")
        .join(&row.model_name);
    // TRT shape profile pins one engine over the batch range this runner
    // actually drives: detect batches frames per run, classify is always a
    // single crop. The input name must come from the graph itself (registry
    // models name inputs arbitrarily); when it cannot be read we build without
    // a profile (lazy per-batch-size engines — today's behavior).
    let trt_profile = crate::vision::ort_common::onnx_first_input_name(&bytes).map(|input_name| {
        let (opt_batch, max_batch) = match contract {
            Contract::Rfdetr => (8, 8),
            Contract::Softmax => (1, 1),
        };
        crate::vision::ort_common::TrtShapeProfile {
            input_name,
            min_batch: 1,
            opt_batch,
            max_batch,
            channels: 3,
            height: pre.resolution,
            width: pre.resolution,
        }
    });
    if trt_profile.is_none() {
        tracing::warn!(
            "[onnx-cv] '{}': could not read the first ONNX input name — building without a TensorRT shape profile",
            row.model_name
        );
    }
    // Build the whole pool under the SINGLE load-lock critical section, spread
    // round-robin across the configured GPU set (session i → device
    // `gpus[i % gpus.len()]`; default `[0]` = single GPU, unchanged). Each session
    // gets its OWN per-(device,session) engine-cache subdir
    // (`trt-cache/<model>/s<i>/` on device 0, `d<dev>_s<i>` elsewhere): the lazy
    // (unprofiled) TRT path compiles engines on the FIRST FORWARD, which happens
    // after this lock is released, so once sessions run concurrently (N>1) they
    // must never share a cache dir, and a device-`d` session must never reuse a
    // plan built for another device. The profiled detector path works identically
    // per dir; it just serializes one engine copy into each.
    let n_sessions = sessions_per_model();
    let gpus = crate::vision::ort_common::vision_gpu_set();
    let mut sessions = Vec::with_capacity(n_sessions);
    for i in 0..n_sessions {
        let device_id = gpus[i % gpus.len()];
        let session_cache = trt_cache_root.join(crate::vision::ort_common::session_cache_subdir(
            device_id, i,
        ));
        sessions.push(
            crate::vision::ort_common::build_ort_session_from_memory(
                &bytes,
                &session_cache,
                trt_profile.as_ref(),
                device_id,
                // Dynamic CV models (incl. OCR/classifier heads like nalepka-stan):
                // FP32 by default so fp16 rounding can't corrupt reads. See `ocr_fp16`.
                crate::vision::ort_common::ocr_fp16(),
            )
            .map_err(|e| anyhow!("onnx-cv session slot {i} on GPU device {device_id}: {e:#}"))?,
        );
    }
    drop(bytes);
    let pool = Arc::new(SessionPool::new(&row.model_name, sessions));
    info!(
        "[onnx-cv] loaded '{}' ({} classes, contract {:?}, {}px, pool={} session(s))",
        row.model_name,
        classes.len(),
        contract,
        pre.resolution,
        pool.len()
    );

    let loaded = Arc::new(CachedModel {
        pool,
        classes,
        pre,
        contract,
        default_threshold: row.default_threshold.map(|v| v as f32),
        sha256: row.sha256.clone(),
    });

    let mut guard = lock_cache();
    guard
        .models
        .insert(row.model_name.clone(), Arc::clone(&loaded));
    let evicted = guard.lru.touch(&row.model_name);
    for name in evicted {
        guard.models.remove(&name);
        info!("[onnx-cv] evicted session '{}' (LRU cap)", name);
    }
    Ok(loaded)
}

/// Writes one RGB24 frame into slot `bi` of a flat NCHW buffer using the
/// model's preprocess spec (stretch-resize, /255, per-channel normalize) —
/// the same transform shape as the fixed detector/classifier.
fn fill_frame_spec(
    data: &mut [f32],
    bi: usize,
    rgb: &[u8],
    w: u32,
    h: u32,
    pre: &PreprocessSpec,
) -> Result<()> {
    let res = pre.resolution as usize;
    let resized = crate::vision::resize::resize_rgb(rgb, w, h, pre.resolution, pre.resolution)
        .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
    let plane = res * res;
    let base = bi * 3 * plane;
    for y in 0..res {
        for x in 0..res {
            let p = (y * res + x) * 3;
            for c in 0..3 {
                let v = resized[p + c] as f32 / 255.0;
                data[base + c * plane + y * res + x] = (v - pre.mean[c]) / pre.std[c];
            }
        }
    }
    Ok(())
}

/// Detection on a batch of frames with an rfdetr-contract model. Blocking
/// (session.run) — call through `spawn_blocking`.
fn detect_blocking(
    model: &CachedModel,
    frames: &[CvFrameLocal],
    threshold: Option<f32>,
) -> Result<Vec<Vec<CvDetection>>> {
    if model.contract != Contract::Rfdetr {
        bail!(
            "detect requires the rfdetr contract, model registers {:?}",
            model.contract
        );
    }
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let res = model.pre.resolution as usize;
    let n = frames.len();
    let num_classes = model.classes.len();

    let mut data = vec![0f32; n * 3 * res * res];
    for (bi, f) in frames.iter().enumerate() {
        fill_frame_spec(&mut data, bi, &f.data, f.width, f.height, &model.pre)?;
    }
    let input = ndarray::Array4::from_shape_vec((n, 3, res, res), data)
        .map_err(|e| anyhow!("onnx-cv: build tensor [{n},3,{res},{res}]: {e}"))?;

    // The forward runs on the session's dedicated thread (see `SessionPool::run`),
    // which owns the `ort::Session`; only the OWNED tensors + derived dims cross
    // back, so no per-thread CUDA resources accumulate on this caller thread.
    let (dets_owned, labels_owned, queries, label_dim) = model.pool.run(move |session| {
        let value = ort::value::Value::from_array(input)
            .map_err(|e| anyhow!("onnx-cv: Value::from_array: {e}"))?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("onnx-cv: model has no inputs"))?;
        let outputs = session
            .run(ort::inputs! { input_name => value })
            .map_err(|e| anyhow!("onnx-cv: session.run: {e}"))?;

        // RF-DETR export contract: `dets [N,queries,4]` (cxcywh) + `labels
        // [N,queries,label_dim]` — identical shape validation to the fixed
        // detector, so a wrong graph fails loudly instead of slicing garbage.
        let (dets_shape, dets_v) = outputs["dets"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("onnx-cv: extract dets: {e}"))?;
        let (labels_shape, labels_v) = outputs["labels"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("onnx-cv: extract labels: {e}"))?;
        if dets_shape.len() != 3 || labels_shape.len() != 3 {
            bail!(
                "onnx-cv: unexpected output rank dets {dets_shape:?} / labels {labels_shape:?}"
            );
        }
        let queries = dets_shape[1] as usize;
        let label_dim = labels_shape[2] as usize;
        if dets_shape[0] as usize != n || dets_shape[2] != 4 {
            bail!("onnx-cv: unexpected dets shape {dets_shape:?}, expected [{n}, queries, 4]");
        }
        if labels_shape[0] as usize != n || labels_shape[1] as usize != queries {
            bail!(
                "onnx-cv: unexpected labels shape {labels_shape:?}, expected [{n}, {queries}, label_dim]"
            );
        }
        if label_dim <= num_classes {
            bail!("labels dim {label_dim} must exceed class count {num_classes} (background slot)");
        }
        if dets_v.len() < n * queries * 4 || labels_v.len() < n * queries * label_dim {
            bail!("onnx-cv: output buffers shorter than declared shape");
        }
        Ok((dets_v.to_vec(), labels_v.to_vec(), queries, label_dim))
    })?;

    let effective_threshold = threshold.or(model.default_threshold);
    let mut results = Vec::with_capacity(n);
    for bi in 0..n {
        let dets_off = bi * queries * 4;
        let labels_off = bi * queries * label_dim;
        let dets_slice = &dets_owned[dets_off..dets_off + queries * 4];
        let labels_slice = &labels_owned[labels_off..labels_off + queries * label_dim];
        let per_frame = crate::vision::rfdetr_post::postprocess_image(
            dets_slice,
            labels_slice,
            queries,
            label_dim,
            &model.classes,
            effective_threshold,
        )
        .into_iter()
        .map(|d| CvDetection {
            klasa: d.klasa,
            bbox: d.bbox,
            score: d.score,
        })
        .collect();
        results.push(per_frame);
    }
    Ok(results)
}

/// Single-crop classification with a softmax-contract model. Blocking.
fn classify_blocking(model: &CachedModel, crop: &CvFrameLocal) -> Result<(String, f32)> {
    if model.contract != Contract::Softmax {
        bail!(
            "classify requires the softmax contract, model registers {:?}",
            model.contract
        );
    }
    let res = model.pre.resolution as usize;
    let num_classes = model.classes.len();

    let mut data = vec![0f32; 3 * res * res];
    fill_frame_spec(
        &mut data,
        0,
        &crop.data,
        crop.width,
        crop.height,
        &model.pre,
    )?;
    let input = ndarray::Array4::from_shape_vec((1, 3, res, res), data)
        .map_err(|e| anyhow!("onnx-cv: build tensor [1,3,{res},{res}]: {e}"))?;

    // Forward + extraction run on the session's dedicated thread; only the owned
    // logits cross back (see `SessionPool::run`).
    let logits = model.pool.run(move |session| {
        let value = ort::value::Value::from_array(input)
            .map_err(|e| anyhow!("onnx-cv: Value::from_array: {e}"))?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("onnx-cv: model has no inputs"))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow!("onnx-cv: model has no outputs"))?;
        let outputs = session
            .run(ort::inputs! { input_name => value })
            .map_err(|e| anyhow!("onnx-cv: session.run: {e}"))?;
        let (shape, logits) = outputs[output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("onnx-cv: extract logits: {e}"))?;
        if shape.len() != 2 || shape[0] != 1 || shape[1] as usize != num_classes {
            bail!("onnx-cv: classifier output shape {shape:?} != [1, {num_classes}]");
        }
        Ok(logits[..num_classes].to_vec())
    })?;
    let (best_idx, score) = softmax_argmax(&logits);
    Ok((model.classes[best_idx].clone(), score))
}

/// Softmax + argmax over one logits row. Numerically stable (max-shifted).
/// Returns `(argmax_index, softmax_probability_of_argmax)`.
fn softmax_argmax(logits: &[f32]) -> (usize, f32) {
    let mut best_idx = 0usize;
    let mut best = f32::NEG_INFINITY;
    for (i, &l) in logits.iter().enumerate() {
        if l > best {
            best = l;
            best_idx = i;
        }
    }
    let denom: f32 = logits.iter().map(|&l| (l - best).exp()).sum();
    let prob = if denom > 0.0 { 1.0 / denom } else { 0.0 };
    (best_idx, prob)
}

/// Async entry used by the `onnx-cv` branch of `LocalCameraCvHandler`. Loads
/// the session lazily (spawn_blocking), runs the operation matching the
/// registered contract, and maps mismatched ops to clear errors.
pub async fn execute(
    row: VisionModelRow,
    op: crate::services::runtime::local_cv::CameraCvOpLocal,
) -> std::result::Result<CameraCvResult, String> {
    use crate::services::runtime::local_cv::CameraCvOpLocal;

    match op {
        CameraCvOpLocal::Detect { frames, threshold } => {
            if row.op != "detect" {
                return Err(format!(
                    "vision model '{}' registers op '{}', not detect",
                    row.model_name, row.op
                ));
            }
            let per_frame = tokio::task::spawn_blocking(move || {
                let model = get_or_load(&row)?;
                detect_blocking(&model, &frames, threshold)
            })
            .await
            .map_err(|e| format!("onnx-cv detect task: {e}"))?
            .map_err(|e| format!("onnx-cv detect: {e:#}"))?;
            Ok(CameraCvResult::Detections { per_frame })
        }
        CameraCvOpLocal::ClassifyState { crop } => {
            if row.op != "classify" {
                return Err(format!(
                    "vision model '{}' registers op '{}', not classify",
                    row.model_name, row.op
                ));
            }
            let (label, score) = tokio::task::spawn_blocking(move || {
                let model = get_or_load(&row)?;
                classify_blocking(&model, &crop)
            })
            .await
            .map_err(|e| format!("onnx-cv classify task: {e}"))?
            .map_err(|e| format!("onnx-cv classify: {e:#}"))?;
            tracing::debug!("[onnx-cv] classify → '{}' ({:.3})", label, score);
            Ok(CameraCvResult::Labels { stan: vec![label] })
        }
        CameraCvOpLocal::Ocr { .. } => Err(
            "onnx-cv registry models do not support OCR — use the fixed OCR engines".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LRU cap: touching a fourth name with cap=3 evicts the least recently
    /// used one; re-touching refreshes recency.
    #[test]
    fn lru_touch_evicts_oldest() {
        let mut lru = LruIndex::new(3);
        assert!(lru.touch("a").is_empty());
        assert!(lru.touch("b").is_empty());
        assert!(lru.touch("c").is_empty());
        // Refresh "a" — "b" becomes the oldest.
        assert!(lru.touch("a").is_empty());
        let evicted = lru.touch("d");
        assert_eq!(evicted, vec!["b".to_string()]);
        assert_eq!(lru.order, vec!["c", "a", "d"]);
    }

    /// cap=0 clamps to 1 so at least the active session stays resident.
    #[test]
    fn lru_cap_clamps_to_one() {
        let mut lru = LruIndex::new(0);
        assert!(lru.touch("a").is_empty());
        assert_eq!(lru.touch("b"), vec!["a".to_string()]);
    }

    #[test]
    fn lru_remove_drops_entry() {
        let mut lru = LruIndex::new(2);
        lru.touch("a");
        lru.touch("b");
        lru.remove("a");
        assert!(lru.touch("c").is_empty());
    }

    #[test]
    fn preprocess_spec_defaults_and_validation() {
        let spec = PreprocessSpec::parse(r#"{"resolution":560}"#).unwrap();
        assert_eq!(spec.resolution, 560);
        assert_eq!(spec.mean, imagenet_mean());
        assert_eq!(spec.std, imagenet_std());
        assert!(PreprocessSpec::parse(r#"{"resolution":0}"#).is_err());
        assert!(PreprocessSpec::parse(r#"{"resolution":224,"layout":"nhwc"}"#).is_err());
        assert!(PreprocessSpec::parse(r#"{"resolution":224,"std":[0.0,0.2,0.2]}"#).is_err());
    }

    #[test]
    fn softmax_argmax_is_stable_and_normalized() {
        let (idx, p) = softmax_argmax(&[1.0, 3.0, 2.0]);
        assert_eq!(idx, 1);
        assert!(p > 0.5 && p < 1.0);
        // Huge logits must not overflow to NaN.
        let (idx, p) = softmax_argmax(&[1000.0, 999.0]);
        assert_eq!(idx, 0);
        assert!(p.is_finite() && p > 0.5);
    }
}
