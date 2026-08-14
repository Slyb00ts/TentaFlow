// =============================================================================
// File: vision/detector_rfdetr.rs — RF-DETR ADR detector (ort+CUDA / Burn)
// =============================================================================
//
// Always-on ADR (dangerous-goods placards / labels) detector for the ADR
// camera-CV PoC. Backend inferencji wybierany cfg/feature:
//   * `vision-ort` (ONNX Runtime, crate `ort`) → sesja ort + CUDA EP na
//     natywnej CUDA 13.3, model dynamic-batch `rfdetr-base.onnx` (prawdziwy
//     batching bez paddingu). To główna, docelowa ścieżka wydajności (~200 fps).
//   * inaczej → wendorowany `burn_rfdetr` (build-time ONNX→Burn codegen), wagi z
//     `rfdetr-base.bpk`, fixed-batch=8 (chunk + zerowy padding).
//
// Preprocessing i postprocessing są backend-agnostyczne i IDENTYCZNE dla obu
// ścieżek — tylko sam forward idzie przez ort albo Burn, więc współrzędne wyjścia
// pokrywają się co do bitu. Preprocessing mirroruje referencyjne `model.predict`
// 1:1: RGB → 560×560 bilinear STRETCH (no letterbox) → /255 → per-channel ImageNet
// normalize → NCHW f32 [N,3,560,560]. DETR head → per-query sigmoid + argmax over
// the 17 real classes (index 17 is the background/ignore slot), NO NMS.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
#[cfg(not(feature = "vision-ort"))]
use burn::tensor::{Tensor, TensorData};
#[cfg(not(feature = "vision-ort"))]
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::Deserialize;
use tracing::info;

use crate::paths;
use crate::services::detection_bus::Detection;
#[cfg(not(feature = "vision-ort"))]
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
#[cfg(not(feature = "vision-ort"))]
use crate::vision::burn_rfdetr::Model;

/// Square input resolution the exported RF-DETR graph expects. Public so the
/// camera ingest pipeline can GPU-scale its detect branch to exactly this size
/// and hit the [`fill_frame`] copy fast-path (single source of truth — the scale
/// target and the fast-path threshold can never drift apart).
pub const RESOLUTION: u32 = 560;

/// Nazwa tensora wejściowego w grafie ONNX (`[batch,3,560,560]`).
#[cfg(feature = "vision-ort")]
const INPUT_NAME: &str = "input";

// Detector session-pool size comes from `[vision] detector_sessions` (default
// 4). 4 pipelined batch-8 forwards measured ~1300 frames/s vs ~430 serialized
// on one GPU (cam_scale sweep) — ~48 cameras @25fps with per-frame p99 inside
// the 40 ms frame budget, vs ~16 at 1 session. Costs ~4×2.6 GB VRAM.

/// Przybliżony rozmiar rezydentny JEDNEJ sesji RF-DETR na GPU (do komunikatu
/// OOM). N sesji ≈ N×tyle VRAM — patrz fail-loud w [`RfDetrDetector::load`].
#[cfg(feature = "vision-ort")]
const DETECTOR_SESSION_VRAM_GB: f32 = 2.6;

/// Rozmiar batcha wkompilowany na stałe w wyeksportowany graf RF-DETR.
/// Model przyjmuje WYŁĄCZNIE wejście `[MODEL_BATCH,3,560,560]` — mniejszy lub
/// większy batch panikuje na stałych kształtach grafu. Klatki chunkujemy po
/// `MODEL_BATCH`, a niepełne chunki dopełniamy zerowym paddingiem.
pub const MODEL_BATCH: usize = 8;

/// Per-channel ImageNet normalization (matches the training transform).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

#[cfg(feature = "vision-ort")]
thread_local! {
    /// Reusable host input buffer for `detect_batch`. Grows to the largest
    /// batch this thread has seen and then stays resident, so the per-batch
    /// ~30 MB alloc + page-fault churn of a fresh `Vec` is paid once. Passed
    /// to ort as a BORROWED tensor (raw pointer, blocking run) — see the
    /// safety comment in `detect_batch`.
    static HOST_INPUT_SCRATCH: std::cell::RefCell<Vec<f32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// `rfdetr-classes.json` shape: `{ "classes": [...], "resolution": 560 }`.
#[derive(Debug, Deserialize)]
struct ClassesFile {
    classes: Vec<String>,
    #[allow(dead_code)]
    resolution: u32,
}

/// Loaded RF-DETR model + class-name table + backend device. `detect`/`detect_batch`
/// take `&self`: the ort path drives an internally-concurrent [`SessionPool`]
/// (interior mutability per session), and the Burn path's `Model::forward` is
/// already `&self` (the singleton still serializes forwards through its own Mutex
/// + the single Burn/wgpu thread).
pub struct RfDetrDetector {
    /// Pula sesji ONNX Runtime (TensorRT→CUDA→CPU) — ścieżka ort. Wewnętrznie
    /// współbieżna (round-robin `Mutex<Session>`), więc `detect_batch` bierze
    /// `&self` i wiele forwardów może biec równolegle na GPU. Budowana RAZ w `load`.
    #[cfg(feature = "vision-ort")]
    pool: crate::vision::ort_common::SessionPool,
    #[cfg(not(feature = "vision-ort"))]
    model: Model<VisionBackend>,
    #[cfg(not(feature = "vision-ort"))]
    device: VisionDevice,
    classes: Vec<String>,
}

impl RfDetrDetector {
    /// Builds the detector from the deploy-time model dir
    /// (`vision_models_dir()/rfdetr-{base.bpk,classes.json}`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let classes_path = dir.join("rfdetr-classes.json");

        let classes_bytes = std::fs::read(&classes_path)
            .with_context(|| format!("read {}", classes_path.display()))?;
        let parsed: ClassesFile = serde_json::from_slice(&classes_bytes)
            .with_context(|| format!("parse {}", classes_path.display()))?;
        if parsed.classes.is_empty() {
            bail!("rfdetr-classes.json has no classes");
        }

        // Ścieżka ort+CUDA: sesja ONNX Runtime na modelu dynamic-batch, tworzona
        // RAZ przy ładowaniu i reużywana przez wszystkie forwardy.
        #[cfg(feature = "vision-ort")]
        {
            let onnx_path = dir.join("rfdetr-base.onnx");
            if !onnx_path.exists() {
                bail!("RF-DETR ONNX missing: {}", onnx_path.display());
            }
            crate::vision::ort_common::ensure_ort_dylib();
            // Fixed 560x560 but VARIABLE batch (per-tick camera count): pin one
            // TRT engine over 1..=max_batch so the first inference of each new
            // batch size does not trigger a per-shape engine rebuild. On a large
            // GPU (B300) a wider batch amortizes fixed per-launch overhead, so the
            // profile ceiling and optimization point are tunable via `[vision]
            // opt_batch`/`max_batch` to measure and exploit cross-camera batching
            // beyond MODEL_BATCH. Changing these makes TRT rebuild the engine on
            // next load (profile mismatch).
            let vision = crate::vision::settings::get();
            let opt_batch = vision.opt_batch.filter(|&n| n >= 1).unwrap_or(MODEL_BATCH);
            let max_batch = vision
                .max_batch
                .filter(|&n| n >= opt_batch)
                .unwrap_or(opt_batch);
            let trt_profile = crate::vision::ort_common::TrtShapeProfile {
                input_name: INPUT_NAME.to_string(),
                min_batch: 1,
                opt_batch,
                max_batch,
                channels: 3,
                height: RESOLUTION,
                width: RESOLUTION,
            };
            let n = crate::vision::ort_common::pool_size(vision.detector_sessions);
            // Every pooled session is a full model copy on ITS GPU. The pool
            // builder degrades on a failure PAST slot 0 (keeps the sessions that
            // built, WARNs loudly) so a transient TRT engine-build failure can
            // never disable camera analysis outright; only slot 0 failing (nothing
            // usable) reaches this hard error, which names the pool size + per-GPU
            // VRAM math and the failing slot's CUDA device id.
            let gpus = crate::vision::ort_common::vision_gpu_set();
            let per_gpu = n.div_ceil(gpus.len());
            let pool = crate::vision::ort_common::build_session_pool_from_file(
                &onnx_path,
                &dir.join("trt-cache"),
                Some(&trt_profile),
                n,
                // Detector keeps FP16 — localization tolerates it, throughput matters.
                true,
            )
            .map_err(|e| {
                anyhow!(
                    "building RF-DETR ort session pool of {n} session(s) across {n_gpus} GPU(s) \
                     {gpus:?} failed: {e:#}. Each session is a full model copy \
                     (~{DETECTOR_SESSION_VRAM_GB} GB VRAM); sessions spread round-robin, so up to \
                     {per_gpu} land on one GPU needing ~{per_gpu_gb:.1} GB resident PER DEVICE — a \
                     failure of the FIRST session means nothing can run at all. Lower \
                     `[vision] detector_sessions` (currently {n}) or add GPUs via `[vision] gpus`.",
                    n_gpus = gpus.len(),
                    per_gpu_gb = per_gpu as f32 * DETECTOR_SESSION_VRAM_GB
                )
            })?;
            info!(
                "[rfdetr] loaded {} ({} classes, backend ort TensorRT→CUDA→CPU, pool={} session(s))",
                onnx_path.display(),
                parsed.classes.len(),
                pool.len()
            );
            Ok(Self {
                pool,
                classes: parsed.classes,
            })
        }

        // Ścieżka Burn: wagi `.bpk` na wybranym backendzie vision-*.
        #[cfg(not(feature = "vision-ort"))]
        {
            let weights_path = dir.join("rfdetr-base.bpk");
            if !weights_path.exists() {
                bail!("RF-DETR weights missing: {}", weights_path.display());
            }
            let device = burn_backend::device();
            let mut model = Model::<VisionBackend>::new(&device);
            let mut store = BurnpackStore::from_file(&weights_path)
                .with_from_adapter(burn_backend::BoolNativeToU32Adapter);
            model
                .load_from(&mut store)
                .map_err(|e| anyhow!("load RF-DETR weights {}: {e}", weights_path.display()))?;

            info!(
                "[rfdetr] loaded {} ({} classes, backend {})",
                weights_path.display(),
                parsed.classes.len(),
                std::any::type_name::<VisionBackend>()
            );
            Ok(Self {
                model,
                device,
                classes: parsed.classes,
            })
        }
    }

    /// Number of pooled ort sessions (each a full model copy on its GPU).
    /// The Burn path holds exactly one model instance. Reported over the
    /// vision-worker link as a heartbeat stat.
    pub fn pool_size(&self) -> usize {
        #[cfg(feature = "vision-ort")]
        {
            self.pool.len()
        }
        #[cfg(not(feature = "vision-ort"))]
        {
            1
        }
    }

    /// Single-frame convenience. Delegates to `detect_batch` (N=1) so there is
    /// exactly one preprocess + postprocess code path — a single live camera
    /// gets bit-identical results to the batched fleet path.
    pub fn detect(&self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<Detection>> {
        Ok(self
            .detect_batch(&[(rgb, w, h)], None)?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    /// Przetwarza N klatek jednym forwardem ort na modelu dynamic-batch.
    ///
    /// Graf ONNX ma dynamiczny wymiar batcha, więc budujemy tensor `[N,3,560,560]`
    /// dla N=`frames.len()` BEZ paddingu (inaczej niż fixed-batch=8 w Burn) i robimy
    /// pojedynczy `session.run`. Wyjścia `dets [N,queries,4]` (cxcywh) oraz
    /// `labels [N,queries,label_dim]` rozdzielamy per slot i postprocessujemy tą
    /// samą funkcją co ścieżka Burn — współrzędne są identyczne. Kolejność wyników
    /// == kolejność `frames`, długość wektora == `frames.len()`.
    #[cfg(feature = "vision-ort")]
    pub fn detect_batch(
        &self,
        frames: &[(&[u8], u32, u32)],
        threshold: Option<f32>,
    ) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let res = RESOLUTION as usize;
        let num_classes = self.classes.len();
        let n = frames.len();

        // Preprocessing WSPÓŁDZIELONY z Burn (`fill_frame`): stretch-resize 560×560,
        // /255, ImageNet normalize. Bufor bez slotów paddingowych — N=liczba klatek.
        // The fill target is a REUSED thread-local scratch: a fresh ~30 MB Vec per
        // batch costs ~1.3 ms of alloc + page-fault churn (measured by
        // examples/detect_post_bench.rs); the scratch faults once and stays hot.
        HOST_INPUT_SCRATCH.with(|cell| {
            let mut data = cell.borrow_mut();
            let need = n * 3 * res * res;
            if data.len() < need {
                data.resize(need, 0.0);
            }
            for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
                fill_frame(&mut data[..need], bi, rgb, w, h)?;
            }

            // Forward + tensor extraction run on the session's dedicated thread (see
            // `SessionPool::run`), which exclusively owns the `ort::Session`. Only the
            // owned dets/labels buffers + derived dims cross back, so no per-thread
            // CUDA resources accumulate on this (arbitrary) caller thread. The input
            // crosses as a raw pointer wrapped into a BORROWED CPU tensor (the same
            // `MemoryInfo::default()` an owned `Value::from_array` would carry), so
            // the scratch is never copied or given away — mirror of the device-path
            // pattern in `forward_device_ptr`.
            let data_ptr = data.as_mut_ptr() as usize;
            let (dets_owned, labels_owned, queries, label_dim) = self.pool.run(move |session| {
                // SAFETY: `data_ptr` covers exactly `n·3·res·res` initialized f32 in
                // this caller thread's scratch. `pool.run` BLOCKS until the forward
                // completes and the scratch is thread-local behind a RefCell borrow
                // held for this whole scope, so nothing can reallocate or reuse it
                // mid-run.
                let value = unsafe {
                    ort::value::TensorRefMut::<f32>::from_raw(
                        ort::memory::MemoryInfo::default(),
                        (data_ptr as *mut ()).cast(),
                        ort::value::Shape::new([n as i64, 3, res as i64, res as i64]),
                    )
                }
                .map_err(|e| anyhow!("rfdetr-ort: TensorRefMut::from_raw: {e}"))?;
                let outputs = session
                    .run(ort::inputs! { INPUT_NAME => value })
                    .map_err(|e| anyhow!("rfdetr-ort: session.run: {e}"))?;

                let (dets_shape, dets_v) = outputs["dets"]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow!("rfdetr-ort: extract dets: {e}"))?;
                let (labels_shape, labels_v) = outputs["labels"]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow!("rfdetr-ort: extract labels: {e}"))?;

                // Walidacja kształtów PRZED slicowaniem — WSPÓLNA z detect_batch_gpu,
                // więc oba wejścia (host tensor tu, device tensor tam) egzekwują ten
                // sam kontrakt grafu.
                let (queries, label_dim) = validate_detr_shapes(
                    dets_shape,
                    labels_shape,
                    dets_v.len(),
                    labels_v.len(),
                    n,
                    num_classes,
                )?;
                Ok((dets_v.to_vec(), labels_v.to_vec(), queries, label_dim))
            })?;

            // Decode WSPÓLNY z detect_batch_gpu — współrzędne detekcji nie mogą się
            // różnić między ścieżką host-tensor a device-tensor.
            Ok(decode_detr_batch(
                &dets_owned,
                &labels_owned,
                n,
                queries,
                label_dim,
                &self.classes,
                threshold,
            ))
        })
    }

    /// GPU-resident detect: mirror of [`RfDetrDetector::detect_batch`] whose
    /// input is a batch of NV12 frames preprocessed ENTIRELY on the GPU. The
    /// fused CUDA kernel (`gpu_preprocess::preprocess_nv12_batch_gpu`) does
    /// YUV→RGB [+ the SAME Q8 bilinear resize to 560 + /255 + ImageNet
    /// normalize] the host path does, leaving the NCHW `[n,3,560,560]` f32 input
    /// in DEVICE memory. That device buffer is handed to ONNX Runtime via
    /// `TensorRefMut::from_raw` (zero host→device copy of the model input), then
    /// the SAME RF-DETR forward + decode as `detect_batch` runs — detections are
    /// bit-parity with the RGB path (the kernel is parity-verified and the
    /// validate/decode are the shared functions).
    ///
    /// `color` is the YUV→RGB matrix/range read from the frame colorimetry
    /// (default BT.709 limited) and applies to the WHOLE batch — callers batch
    /// only frames sharing colorimetry. `mean`/`std`/`s` match `fill_frame`.
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-ort",
        feature = "vision-cuda-preprocess"
    ))]
    pub fn detect_batch_gpu(
        &self,
        frames: &[crate::vision::gpu_preprocess::Nv12Frame<'_>],
        color: crate::vision::gpu_preprocess::ColorCoeffs,
        threshold: Option<f32>,
    ) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let res = RESOLUTION as usize;
        let n = frames.len();

        // Fused GPU preprocess → device buffer [n,3,560,560] f32.
        let batch = crate::vision::gpu_preprocess::preprocess_nv12_batch_gpu(
            frames, res, MEAN, STD, color,
        )?;

        // The device buffer must OUTLIVE the ORT run. It lives in this thread's
        // reusable preprocess scratch (a thread_local in gpu_preprocess); only
        // its raw pointer crosses into the pooled session thread. `pool.run`
        // blocks until the forward completes and this method runs on ONE worker
        // thread, so no other preprocess call can reallocate the scratch mid-run.
        let out = self.forward_device_ptr(batch.device_ptr() as usize, n, res, threshold);
        // Explicit drop marks the end of the device buffer's required lifetime;
        // the backing memory stays in the thread scratch for the next batch.
        drop(batch);
        out
    }

    /// GPU-resident detect from an ALREADY-preprocessed, OWNED device tensor
    /// ([`gpu_preprocess::OwnedDeviceTensor`], `[1,3,560,560]` f32 on device 0) —
    /// the zero-copy (Stage 4) path where the fused NV12→RGB + resize + normalize
    /// ran directly on the NVDEC decode surface (no host download/re-upload) in
    /// the appsink callback. Skips preprocess and runs the SAME ORT forward +
    /// decode as [`detect_batch_gpu`], so detections are bit-identical to the
    /// download path (same kernel output, same shared decode). The tensor is
    /// kept alive by the caller (an `Arc`) for the whole blocking run.
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-ort",
        feature = "vision-cuda-preprocess"
    ))]
    pub fn detect_device_tensor(
        &self,
        tensor: &crate::vision::gpu_preprocess::OwnedDeviceTensor,
        threshold: Option<f32>,
    ) -> Result<Vec<Detection>> {
        let res = RESOLUTION as usize;
        if tensor.s() != res || tensor.n() != 1 {
            return Err(anyhow!(
                "rfdetr-ort gpu: device tensor shape [{},3,{},{}] != [1,3,{res},{res}]",
                tensor.n(),
                tensor.s(),
                tensor.s()
            ));
        }
        Ok(self
            .forward_device_ptr(tensor.device_ptr() as usize, 1, res, threshold)?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    /// Shared ORT device-tensor forward + decode for both the download
    /// ([`detect_batch_gpu`]) and zero-copy ([`detect_device_tensor`]) paths.
    /// `dev_ptr` is a CUDA device-0 buffer of exactly `n·3·res·res` f32 that the
    /// CALLER keeps alive for the whole (synchronous, blocking) run.
    #[cfg(all(feature = "vision-ort", feature = "vision-cuda-preprocess"))]
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn forward_device_ptr(
        &self,
        dev_ptr: usize,
        n: usize,
        res: usize,
        threshold: Option<f32>,
    ) -> Result<Vec<Vec<Detection>>> {
        let num_classes = self.classes.len();
        let (dets_owned, labels_owned, queries, label_dim) = self.pool.run(move |session| {
            use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
            use ort::value::{Shape, TensorRefMut};

            let info = MemoryInfo::new(
                AllocationDevice::CUDA,
                0,
                AllocatorType::Device,
                MemoryType::Default,
            )
            .map_err(|e| anyhow!("rfdetr-ort gpu: MemoryInfo::new: {e}"))?;

            // SAFETY: `dev_ptr` is a CUDA device buffer of exactly n·3·res·res f32,
            // valid on device 0, kept alive by the caller for the whole blocking run.
            let tensor = unsafe {
                TensorRefMut::<f32>::from_raw(
                    info,
                    (dev_ptr as *mut ()).cast(),
                    Shape::new([n as i64, 3, res as i64, res as i64]),
                )
            }
            .map_err(|e| anyhow!("rfdetr-ort gpu: TensorRefMut::from_raw: {e}"))?;

            let outputs = session
                .run(ort::inputs! { INPUT_NAME => tensor })
                .map_err(|e| anyhow!("rfdetr-ort gpu: session.run: {e}"))?;

            let (dets_shape, dets_v) = outputs["dets"]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("rfdetr-ort gpu: extract dets: {e}"))?;
            let (labels_shape, labels_v) = outputs["labels"]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("rfdetr-ort gpu: extract labels: {e}"))?;
            let (queries, label_dim) = validate_detr_shapes(
                dets_shape,
                labels_shape,
                dets_v.len(),
                labels_v.len(),
                n,
                num_classes,
            )?;
            Ok((dets_v.to_vec(), labels_v.to_vec(), queries, label_dim))
        })?;

        Ok(decode_detr_batch(
            &dets_owned,
            &labels_owned,
            n,
            queries,
            label_dim,
            &self.classes,
            threshold,
        ))
    }

    /// Przetwarza N klatek kamer prawdziwym batchowanym forwardem.
    ///
    /// Model jest wkompilowany na sztywno pod `[MODEL_BATCH,3,560,560]`, więc
    /// dzielimy `frames` na chunki po `MODEL_BATCH`. Każdy chunk trafia do JEDNEGO
    /// forwardu na buforze `[MODEL_BATCH,...]`; niepełny ostatni chunk dopełniamy
    /// zerowym paddingiem (sloty `chunk_len..MODEL_BATCH`), którego wyników nie
    /// postprocessujemy ani nie zwracamy. Kolejność wyników = kolejność `frames`,
    /// a długość wektora wynikowego == `frames.len()`.
    #[cfg(not(feature = "vision-ort"))]
    pub fn detect_batch(
        &self,
        frames: &[(&[u8], u32, u32)],
        threshold: Option<f32>,
    ) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let res = RESOLUTION as usize;
        let num_classes = self.classes.len();
        let mut results = Vec::with_capacity(frames.len());

        for chunk in frames.chunks(MODEL_BATCH) {
            let chunk_len = chunk.len();

            // Bufor całego batcha; sloty paddingowe zostają wyzerowane.
            let mut data = vec![0f32; MODEL_BATCH * 3 * res * res];
            for (bi, &(rgb, w, h)) in chunk.iter().enumerate() {
                fill_frame(&mut data, bi, rgb, w, h)?;
            }

            let input = Tensor::<VisionBackend, 4>::from_data(
                TensorData::new(data, [MODEL_BATCH, 3, res, res]),
                &self.device,
            );

            let (o0, o1) = crate::vision::burn_backend::guarded_forward("rfdetr", || {
                self.model.forward(input)
            })?;
            // dets last dim = 4 (cxcywh), labels last dim = num_classes + background.
            let (dets_t, labels_t) = if o0.dims()[2] == 4 {
                (o0, o1)
            } else {
                (o1, o0)
            };
            let dets_dims = dets_t.dims();
            let labels_dims = labels_t.dims();
            let queries = dets_dims[1];
            let label_dim = labels_dims[2];

            // Walidacja kształtów PRZED slicowaniem — bledny kształt grafu (inny
            // batch/queries/last-dim) prowadzilby do panicu z indeksowania buforow.
            if dets_dims[0] != MODEL_BATCH || dets_dims[2] != 4 {
                bail!(
                    "rfdetr: nieoczekiwany kształt wyjścia dets {:?}, oczekiwano [{}, queries, 4]",
                    dets_dims,
                    MODEL_BATCH
                );
            }
            if labels_dims[0] != MODEL_BATCH || labels_dims[1] != queries {
                bail!(
                    "rfdetr: nieoczekiwany kształt wyjścia labels {:?}, oczekiwano [{}, {}, label_dim]",
                    labels_dims,
                    MODEL_BATCH,
                    queries
                );
            }

            if label_dim <= num_classes {
                bail!(
                    "labels dim {} must exceed class count {} (background slot)",
                    label_dim,
                    num_classes
                );
            }

            let dets_v: Vec<f32> = dets_t
                .to_data()
                .to_vec()
                .map_err(|e| anyhow!("dets to_vec: {e:?}"))?;
            let labels_v: Vec<f32> = labels_t
                .to_data()
                .to_vec()
                .map_err(|e| anyhow!("labels to_vec: {e:?}"))?;

            // Po materializacji buforow upewniamy sie, ze dlugosci pokrywaja pelny
            // batch — inaczej wycinki slotow siegnelyby poza bufor (panic).
            if dets_v.len() < MODEL_BATCH * queries * 4 {
                bail!(
                    "rfdetr: bufor dets za krótki: {} < {}",
                    dets_v.len(),
                    MODEL_BATCH * queries * 4
                );
            }
            if labels_v.len() < MODEL_BATCH * queries * label_dim {
                bail!(
                    "rfdetr: bufor labels za krótki: {} < {}",
                    labels_v.len(),
                    MODEL_BATCH * queries * label_dim
                );
            }

            // Wyjścia są ułożone [MODEL_BATCH, queries, ...] w porządku row-major,
            // więc slot `bi` to spójny wycinek. Postprocessujemy tylko realne
            // sloty (0..chunk_len); sloty paddingowe odrzucamy.
            for bi in 0..chunk_len {
                let (dets_slice, labels_slice) =
                    slot_slices(&dets_v, &labels_v, bi, queries, label_dim);
                results.push(crate::vision::rfdetr_post::postprocess_image(
                    dets_slice,
                    labels_slice,
                    queries,
                    label_dim,
                    &self.classes,
                    threshold,
                ));
            }
        }

        Ok(results)
    }
}

/// Validates the RF-DETR head output shapes/lengths against the batch size and
/// class table, returning `(queries, label_dim)`. Shared by the host-tensor
/// (`detect_batch`) and device-tensor (`detect_batch_gpu`) ort paths so the
/// graph contract can never drift between them. `dets_shape`/`labels_shape`
/// deref to `[i64]` (ort `Shape`).
#[cfg(feature = "vision-ort")]
fn validate_detr_shapes(
    dets_shape: &[i64],
    labels_shape: &[i64],
    dets_len: usize,
    labels_len: usize,
    n: usize,
    num_classes: usize,
) -> Result<(usize, usize)> {
    if dets_shape.len() != 3 || labels_shape.len() != 3 {
        bail!(
            "rfdetr-ort: nieoczekiwana liczba wymiarów dets {dets_shape:?} / labels {labels_shape:?}"
        );
    }
    let queries = dets_shape[1] as usize;
    let label_dim = labels_shape[2] as usize;
    if dets_shape[0] as usize != n || dets_shape[2] != 4 {
        bail!(
            "rfdetr-ort: nieoczekiwany kształt dets {dets_shape:?}, oczekiwano [{n}, queries, 4]"
        );
    }
    if labels_shape[0] as usize != n || labels_shape[1] as usize != queries {
        bail!(
            "rfdetr-ort: nieoczekiwany kształt labels {labels_shape:?}, oczekiwano [{n}, {queries}, label_dim]"
        );
    }
    if label_dim <= num_classes {
        bail!("labels dim {label_dim} must exceed class count {num_classes} (background slot)");
    }
    if dets_len < n * queries * 4 {
        bail!(
            "rfdetr-ort: bufor dets za krótki: {dets_len} < {}",
            n * queries * 4
        );
    }
    if labels_len < n * queries * label_dim {
        bail!(
            "rfdetr-ort: bufor labels za krótki: {labels_len} < {}",
            n * queries * label_dim
        );
    }
    Ok((queries, label_dim))
}

/// Decodes the flat `[n, queries, ...]` RF-DETR head buffers into per-image
/// detections via [`slot_slices`] + `rfdetr_post::postprocess_image`. Shared by
/// the host-tensor and device-tensor ort paths — the decode is identical, only
/// the model input differs, so both paths yield bit-identical detections.
/// `pub` so `examples/detect_post_bench.rs` can time the real decode.
#[cfg(feature = "vision-ort")]
pub fn decode_detr_batch(
    dets_owned: &[f32],
    labels_owned: &[f32],
    n: usize,
    queries: usize,
    label_dim: usize,
    classes: &[String],
    threshold: Option<f32>,
) -> Vec<Vec<Detection>> {
    let mut results = Vec::with_capacity(n);
    for bi in 0..n {
        let (dets_slice, labels_slice) =
            slot_slices(dets_owned, labels_owned, bi, queries, label_dim);
        results.push(crate::vision::rfdetr_post::postprocess_image(
            dets_slice,
            labels_slice,
            queries,
            label_dim,
            classes,
            threshold,
        ));
    }
    results
}

/// Zwraca wycinki (dets, labels) slotu `bi` z płaskich buforów batcha ułożonych
/// row-major `[MODEL_BATCH, queries, ...]`. Czysta funkcja (offsety/wycinki)
/// wydzielona z `detect_batch`, by dala sie przetestowac bez modelu/GPU. Wywolujacy
/// gwarantuje, ze bufory pokrywaja pelny batch (walidacja w `detect_batch`).
#[inline]
fn slot_slices<'a>(
    dets_v: &'a [f32],
    labels_v: &'a [f32],
    bi: usize,
    queries: usize,
    label_dim: usize,
) -> (&'a [f32], &'a [f32]) {
    let dets_off = bi * queries * 4;
    let labels_off = bi * queries * label_dim;
    (
        &dets_v[dets_off..dets_off + queries * 4],
        &labels_v[labels_off..labels_off + queries * label_dim],
    )
}

/// Per-channel normalize lookup table: `lut[c][v] = (v/255 - MEAN[c]) / STD[c]`
/// — the exact f32 expression [`fill_frame`] historically computed per pixel,
/// memoized over all 256 byte values, so the hot loop is one table load per
/// element with bit-identical output (3 KiB, stays in L1).
fn normalize_lut() -> &'static [[f32; 256]; 3] {
    static LUT: std::sync::OnceLock<[[f32; 256]; 3]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [[0f32; 256]; 3];
        for c in 0..3 {
            for (v, slot) in lut[c].iter_mut().enumerate() {
                *slot = (v as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
        lut
    })
}

/// Normalizes one already-560×560 RGB24 frame into a `[3, plane]` NCHW slot:
/// single pass over the interleaved pixels, contiguous per-plane writes (the
/// old per-pixel loop wrote all three planes strided, defeating store
/// combining). The zip bounds every access, so the loop body is check-free.
fn normalize_hwc_to_chw(out: &mut [f32], rgb: &[u8], plane: usize) {
    debug_assert_eq!(rgb.len(), plane * 3, "RGB24 length must cover the plane");
    let lut = normalize_lut();
    let (r_plane, rest) = out.split_at_mut(plane);
    let (g_plane, b_plane) = rest.split_at_mut(plane);
    for (((px, r), g), b) in rgb
        .chunks_exact(3)
        .zip(r_plane.iter_mut())
        .zip(g_plane.iter_mut())
        .zip(b_plane.iter_mut())
    {
        *r = lut[0][px[0] as usize];
        *g = lut[1][px[1] as usize];
        *b = lut[2][px[2] as usize];
    }
}

/// Writes one RGB24 frame into batch slot `bi` of a flat NCHW buffer:
/// stretch-resize to 560×560, /255, per-channel ImageNet normalize.
///
/// `pub` so `examples/detect_post_bench.rs` can time the real host-tensor fill
/// against a baseline copy without a model/GPU.
pub fn fill_frame(data: &mut [f32], bi: usize, rgb: &[u8], w: u32, h: u32) -> Result<()> {
    let res = RESOLUTION as usize;
    let plane = res * res;
    let base = bi * 3 * plane;
    let slot = &mut data[base..base + 3 * plane];
    // FAST PATH: the frame is already exactly 560×560 (GPU-scaled by the camera
    // ingest detect branch), so skip `resize_rgb` entirely — normalize + pack
    // straight from the borrowed buffer. This is what makes the pre-scaled
    // detect frame free (removes the ~4 ms full-frame CPU resize). Guarded on
    // the exact input length so a truncated/wrong-stride buffer can never index
    // out of bounds; anything else takes the CPU resize fallback below.
    if w == RESOLUTION && h == RESOLUTION && rgb.len() == plane * 3 {
        normalize_hwc_to_chw(slot, rgb, plane);
        return Ok(());
    }
    let resized = crate::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION)
        .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
    normalize_hwc_to_chw(slot, &resized, plane);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{slot_slices, MODEL_BATCH};

    /// Wypelnia 8 slotow batcha (dets/labels) wartoscia = indeks slotu, przepuszcza
    /// przez `slot_slices` (te sama logike offsetow co `detect_batch`) i weryfikuje,
    /// ze sloty `0..chunk_len` mapuja sie na wlasciwe wycinki, a sloty paddingowe
    /// (chunk_len..MODEL_BATCH, tu wypelnione strazakiem) nie sa dotykane.
    #[test]
    fn slot_slices_mapuja_wlasciwe_sloty() {
        let queries = 3usize;
        let label_dim = 5usize;
        let chunk_len = 3usize; // 3 realne sloty, 5 paddingowych

        let mut dets_v = vec![0f32; MODEL_BATCH * queries * 4];
        let mut labels_v = vec![0f32; MODEL_BATCH * queries * label_dim];

        // Slot `bi`: realne sloty = bi, paddingowe = wartownik 999 (wykrylby bledny
        // wyciek wycinka poza realny slot).
        for bi in 0..MODEL_BATCH {
            let val = if bi < chunk_len { bi as f32 } else { 999.0 };
            let d_off = bi * queries * 4;
            for x in &mut dets_v[d_off..d_off + queries * 4] {
                *x = val;
            }
            let l_off = bi * queries * label_dim;
            for x in &mut labels_v[l_off..l_off + queries * label_dim] {
                *x = val;
            }
        }

        for bi in 0..chunk_len {
            let (d, l) = slot_slices(&dets_v, &labels_v, bi, queries, label_dim);
            assert_eq!(d.len(), queries * 4, "dlugosc wycinka dets slotu {bi}");
            assert_eq!(
                l.len(),
                queries * label_dim,
                "dlugosc wycinka labels slotu {bi}"
            );
            assert!(
                d.iter().all(|&v| v == bi as f32),
                "wycinek dets slotu {bi} zawiera obce wartosci: {d:?}"
            );
            assert!(
                l.iter().all(|&v| v == bi as f32),
                "wycinek labels slotu {bi} zawiera obce wartosci: {l:?}"
            );
        }
    }

    /// Odtwarza kontrakt pętli chunkującej z `detect_batch` BEZ modelu (forward
    /// wymaga wag + GPU): dla `n` klatek liczy chunki po `MODEL_BATCH`, długość
    /// paddingu i łączną liczbę zwracanych wyników. To pilnuje niezmienników:
    /// (a) liczba wyników == n, (b) sloty paddingowe są odrzucane, (c) ostatni
    /// chunk jest dopełniany do `MODEL_BATCH`.
    fn plan(n: usize) -> (usize, usize, usize) {
        // Symulacja `frames.chunks(MODEL_BATCH)`: liczba realnych slotów zebranych
        // przez pętlę `for bi in 0..chunk_len`, liczba chunków oraz padding
        // ostatniego chunku.
        let mut real_slots = 0usize;
        let mut chunks = 0usize;
        let mut last_pad = 0usize;
        let mut left = n;
        while left > 0 {
            let chunk_len = left.min(MODEL_BATCH);
            real_slots += chunk_len; // sloty 0..chunk_len -> postprocess_image
            last_pad = MODEL_BATCH - chunk_len; // sloty chunk_len..MODEL_BATCH -> odrzucone
            chunks += 1;
            left -= chunk_len;
        }
        (real_slots, chunks, last_pad)
    }

    #[test]
    fn three_frames_single_chunk_five_padding() {
        // 3 realne klatki -> 1 chunk (3 realne + 5 padding) -> 3 wyniki.
        let (results, chunks, pad) = plan(3);
        assert_eq!(results, 3);
        assert_eq!(chunks, 1);
        assert_eq!(pad, 5);
    }

    #[test]
    fn ten_frames_two_chunks_six_padding() {
        // 10 klatek -> 2 chunki (8 pełny + 2 realne + 6 padding) -> 10 wyników.
        let (results, chunks, pad) = plan(10);
        assert_eq!(results, 10);
        assert_eq!(chunks, 2);
        assert_eq!(pad, 6);
    }

    #[test]
    fn zero_frames_no_chunks_no_results() {
        // Pusty input -> brak chunków -> pusty wynik.
        let (results, chunks, _pad) = plan(0);
        assert_eq!(results, 0);
        assert_eq!(chunks, 0);
    }

    #[test]
    fn full_batch_single_chunk_no_padding() {
        // Dokładnie MODEL_BATCH klatek -> 1 chunk bez paddingu.
        let (results, chunks, pad) = plan(MODEL_BATCH);
        assert_eq!(results, MODEL_BATCH);
        assert_eq!(chunks, 1);
        assert_eq!(pad, 0);
    }
}
