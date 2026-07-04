// =============================================================================
// File: vision/detector_rfdetr.rs — RF-DETR ADR detector (ort+CUDA / Burn)
// =============================================================================
//
// Always-on ADR (dangerous-goods placards / labels) detector for the Acme
// camera-CV PoC. Backend inferencji wybierany cfg/feature:
//   * `inference-supertonic` (ONNX Runtime, crate `ort`) → sesja ort + CUDA EP na
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
#[cfg(not(feature = "inference-supertonic"))]
use burn::tensor::{Tensor, TensorData};
#[cfg(not(feature = "inference-supertonic"))]
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::Deserialize;
use tracing::info;

use crate::paths;
use crate::services::detection_bus::Detection;
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_rfdetr::Model;

/// Square input resolution the exported RF-DETR graph expects.
const RESOLUTION: u32 = 560;

/// Nazwa tensora wejściowego w grafie ONNX (`[batch,3,560,560]`).
#[cfg(feature = "inference-supertonic")]
const INPUT_NAME: &str = "input";

/// Env sterujący rozmiarem puli sesji ort detektora — hot path CV. Domyślnie 1 =
/// ścieżka bit-identyczna z pojedynczą sesją (jeden forward naraz, checkout zawsze
/// bierze slot 0), a >1 pozwala wielu batchowanym forwardom RF-DETR liczyć się
/// równolegle na GPU (każda sesja to własna kopia modelu ≈2.6 GB VRAM).
#[cfg(feature = "inference-supertonic")]
const DETECTOR_SESSIONS_ENV: &str = "TENTAFLOW_VISION_DETECTOR_SESSIONS";
#[cfg(feature = "inference-supertonic")]
const DEFAULT_DETECTOR_SESSIONS: usize = 1;

/// Przybliżony rozmiar rezydentny JEDNEJ sesji RF-DETR na GPU (do komunikatu
/// OOM). N sesji ≈ N×tyle VRAM — patrz fail-loud w [`RfDetrDetector::load`].
#[cfg(feature = "inference-supertonic")]
const DETECTOR_SESSION_VRAM_GB: f32 = 2.6;

/// Rozmiar batcha wkompilowany na stałe w wyeksportowany graf RF-DETR.
/// Model przyjmuje WYŁĄCZNIE wejście `[MODEL_BATCH,3,560,560]` — mniejszy lub
/// większy batch panikuje na stałych kształtach grafu. Klatki chunkujemy po
/// `MODEL_BATCH`, a niepełne chunki dopełniamy zerowym paddingiem.
pub const MODEL_BATCH: usize = 8;

/// Per-channel ImageNet normalization (matches the training transform).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

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
    #[cfg(feature = "inference-supertonic")]
    pool: crate::vision::ort_common::SessionPool,
    #[cfg(not(feature = "inference-supertonic"))]
    model: Model<VisionBackend>,
    #[cfg(not(feature = "inference-supertonic"))]
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
        #[cfg(feature = "inference-supertonic")]
        {
            let onnx_path = dir.join("rfdetr-base.onnx");
            if !onnx_path.exists() {
                bail!("RF-DETR ONNX missing: {}", onnx_path.display());
            }
            crate::vision::ort_common::ensure_ort_dylib();
            // Fixed 560x560 but VARIABLE batch (per-tick camera count): pin one
            // TRT engine over 1..=MODEL_BATCH so the first inference of each new
            // batch size does not trigger a per-shape engine rebuild.
            let trt_profile = crate::vision::ort_common::TrtShapeProfile {
                input_name: INPUT_NAME.to_string(),
                min_batch: 1,
                opt_batch: MODEL_BATCH,
                max_batch: MODEL_BATCH,
                channels: 3,
                height: RESOLUTION,
                width: RESOLUTION,
            };
            let n = crate::vision::ort_common::pool_size_from_env(
                DETECTOR_SESSIONS_ENV,
                DEFAULT_DETECTOR_SESSIONS,
            );
            // Every pooled session is a full model copy on ITS GPU, so a build
            // failure past the first is almost certainly VRAM exhaustion. Fail
            // LOUDLY naming the pool size + per-GPU VRAM math and refuse to fall
            // back to fewer sessions — a silent degrade would mask a misconfigured
            // `TENTAFLOW_VISION_DETECTOR_SESSIONS` / `TENTAFLOW_VISION_GPUS`. The
            // wrapped error names the failing session slot AND its CUDA device id.
            let gpus = crate::vision::ort_common::vision_gpu_set();
            let per_gpu = n.div_ceil(gpus.len());
            let pool = crate::vision::ort_common::build_session_pool_from_file(
                &onnx_path,
                &dir.join("trt-cache"),
                Some(&trt_profile),
                n,
            )
            .map_err(|e| {
                anyhow!(
                    "building RF-DETR ort session pool of {n} session(s) across {n_gpus} GPU(s) \
                     {gpus:?} failed: {e:#}. Each session is a full model copy \
                     (~{DETECTOR_SESSION_VRAM_GB} GB VRAM); sessions spread round-robin, so up to \
                     {per_gpu} land on one GPU needing ~{per_gpu_gb:.1} GB resident PER DEVICE — a \
                     failure past the first session on a device is almost certainly GPU OOM. Lower \
                     {DETECTOR_SESSIONS_ENV} (currently {n}) or add GPUs via TENTAFLOW_VISION_GPUS; \
                     NOT falling back to fewer sessions.",
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
        #[cfg(not(feature = "inference-supertonic"))]
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
    #[cfg(feature = "inference-supertonic")]
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
        let mut data = vec![0f32; n * 3 * res * res];
        for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
            fill_frame(&mut data, bi, rgb, w, h)?;
        }

        let input = ndarray::Array4::from_shape_vec((n, 3, res, res), data)
            .map_err(|e| anyhow!("rfdetr-ort: budowa tensora [{n},3,{res},{res}]: {e}"))?;

        // Forward + tensor extraction run on the session's dedicated thread (see
        // `SessionPool::run`), which exclusively owns the `ort::Session`. Only the
        // owned dets/labels buffers + derived dims cross back, so no per-thread
        // CUDA resources accumulate on this (arbitrary) caller thread.
        let (dets_owned, labels_owned, queries, label_dim) = self.pool.run(move |session| {
            let value = ort::value::Value::from_array(input)
                .map_err(|e| anyhow!("rfdetr-ort: Value::from_array: {e}"))?;
            let outputs = session
                .run(ort::inputs! { INPUT_NAME => value })
                .map_err(|e| anyhow!("rfdetr-ort: session.run: {e}"))?;

            let (dets_shape, dets_v) = outputs["dets"]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("rfdetr-ort: extract dets: {e}"))?;
            let (labels_shape, labels_v) = outputs["labels"]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("rfdetr-ort: extract labels: {e}"))?;

            // Walidacja kształtów PRZED slicowaniem — błędny graf (inny batch/queries/
            // last-dim) prowadziłby do wycinków poza bufor.
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
                bail!(
                    "labels dim {label_dim} must exceed class count {num_classes} (background slot)"
                );
            }
            if dets_v.len() < n * queries * 4 {
                bail!(
                    "rfdetr-ort: bufor dets za krótki: {} < {}",
                    dets_v.len(),
                    n * queries * 4
                );
            }
            if labels_v.len() < n * queries * label_dim {
                bail!(
                    "rfdetr-ort: bufor labels za krótki: {} < {}",
                    labels_v.len(),
                    n * queries * label_dim
                );
            }
            Ok((dets_v.to_vec(), labels_v.to_vec(), queries, label_dim))
        })?;

        // Wyjścia ułożone row-major `[N, queries, ...]` — slot `bi` to spójny
        // wycinek (ta sama funkcja offsetów co ścieżka Burn).
        let mut results = Vec::with_capacity(n);
        for bi in 0..n {
            let (dets_slice, labels_slice) =
                slot_slices(&dets_owned, &labels_owned, bi, queries, label_dim);
            results.push(crate::vision::rfdetr_post::postprocess_image(
                dets_slice,
                labels_slice,
                queries,
                label_dim,
                &self.classes,
                threshold,
            ));
        }
        Ok(results)
    }

    /// Przetwarza N klatek kamer prawdziwym batchowanym forwardem.
    ///
    /// Model jest wkompilowany na sztywno pod `[MODEL_BATCH,3,560,560]`, więc
    /// dzielimy `frames` na chunki po `MODEL_BATCH`. Każdy chunk trafia do JEDNEGO
    /// forwardu na buforze `[MODEL_BATCH,...]`; niepełny ostatni chunk dopełniamy
    /// zerowym paddingiem (sloty `chunk_len..MODEL_BATCH`), którego wyników nie
    /// postprocessujemy ani nie zwracamy. Kolejność wyników = kolejność `frames`,
    /// a długość wektora wynikowego == `frames.len()`.
    #[cfg(not(feature = "inference-supertonic"))]
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
            let (dets_t, labels_t) = if o0.dims()[2] == 4 { (o0, o1) } else { (o1, o0) };
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

/// Writes one RGB24 frame into batch slot `bi` of a flat NCHW buffer:
/// stretch-resize to 560×560, /255, per-channel ImageNet normalize.
fn fill_frame(data: &mut [f32], bi: usize, rgb: &[u8], w: u32, h: u32) -> Result<()> {
    let res = RESOLUTION as usize;
    let resized = crate::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION)
        .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
    let plane = res * res;
    let base = bi * 3 * plane;
    for y in 0..res {
        for x in 0..res {
            let p = (y * res + x) * 3;
            for c in 0..3 {
                let v = resized[p + c] as f32 / 255.0;
                data[base + c * plane + y * res + x] = (v - MEAN[c]) / STD[c];
            }
        }
    }
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
            assert_eq!(l.len(), queries * label_dim, "dlugosc wycinka labels slotu {bi}");
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
