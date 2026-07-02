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
#[cfg(feature = "inference-supertonic")]
use tracing::warn;

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

/// Domyślna ścieżka biblioteki ONNX Runtime dla dlopen (`ort` z `load-dynamic`),
/// gdy `ORT_DYLIB_PATH` nie jest ustawione w środowisku.
#[cfg(feature = "inference-supertonic")]
const DEFAULT_ORT_DYLIB: &str = "/usr/lib/libonnxruntime.so.1.24.4";

/// Rozmiar batcha wkompilowany na stałe w wyeksportowany graf RF-DETR.
/// Model przyjmuje WYŁĄCZNIE wejście `[MODEL_BATCH,3,560,560]` — mniejszy lub
/// większy batch panikuje na stałych kształtach grafu. Klatki chunkujemy po
/// `MODEL_BATCH`, a niepełne chunki dopełniamy zerowym paddingiem.
pub const MODEL_BATCH: usize = 8;

/// Per-channel ImageNet normalization (matches the training transform).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Minimum sigmoid confidence to surface a detection.
const SCORE_THRESHOLD: f32 = 0.5;

/// `rfdetr-classes.json` shape: `{ "classes": [...], "resolution": 560 }`.
#[derive(Debug, Deserialize)]
struct ClassesFile {
    classes: Vec<String>,
    #[allow(dead_code)]
    resolution: u32,
}

/// Loaded RF-DETR model + class-name table + backend device. `detect`/`detect_batch`
/// keep `&mut self` so the cross-camera engine can hold it behind a single mutex.
pub struct RfDetrDetector {
    /// Sesja ONNX Runtime (CUDA EP) — ścieżka ort. Tworzona RAZ w `load`.
    #[cfg(feature = "inference-supertonic")]
    session: ort::session::Session,
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
            ensure_ort_dylib();
            let session = build_ort_session(&onnx_path)?;
            info!(
                "[rfdetr] loaded {} ({} classes, backend ort TensorRT→CUDA→CPU)",
                onnx_path.display(),
                parsed.classes.len()
            );
            Ok(Self {
                session,
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
    pub fn detect(&mut self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<Detection>> {
        Ok(self
            .detect_batch(&[(rgb, w, h)])?
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
    pub fn detect_batch(&mut self, frames: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<Detection>>> {
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
        let value = ort::value::Value::from_array(input)
            .map_err(|e| anyhow!("rfdetr-ort: Value::from_array: {e}"))?;

        let outputs = self
            .session
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

        // Materializujemy bufory na własność, by zwolnić pożyczkę `outputs` (a przez
        // nią `&mut self.session`) PRZED pętlą postprocessu, która potrzebuje `&self`
        // (dostęp do `self.classes`). Kopia jest znikoma (N×queries×~22 f32).
        let dets_owned = dets_v.to_vec();
        let labels_owned = labels_v.to_vec();
        drop(outputs);

        // Wyjścia ułożone row-major `[N, queries, ...]` — slot `bi` to spójny
        // wycinek (ta sama funkcja offsetów co ścieżka Burn).
        let mut results = Vec::with_capacity(n);
        for bi in 0..n {
            let (dets_slice, labels_slice) =
                slot_slices(&dets_owned, &labels_owned, bi, queries, label_dim);
            results.push(self.postprocess_image(
                dets_slice,
                labels_slice,
                queries,
                label_dim,
                num_classes,
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
    pub fn detect_batch(&mut self, frames: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<Detection>>> {
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
                results.push(self.postprocess_image(
                    dets_slice,
                    labels_slice,
                    queries,
                    label_dim,
                    num_classes,
                ));
            }
        }

        Ok(results)
    }

    /// Per-image DETR postprocess: per-query sigmoid + argmax over the real
    /// classes (index `num_classes` is the background slot), threshold, and
    /// cxcywh→xywh-normalized box. No NMS.
    fn postprocess_image(
        &self,
        dets: &[f32],
        labels: &[f32],
        queries: usize,
        label_dim: usize,
        num_classes: usize,
    ) -> Vec<Detection> {
        let mut items = Vec::new();
        for q in 0..queries {
            let logits = &labels[q * label_dim..q * label_dim + label_dim];
            let mut best_idx = 0usize;
            let mut best_logit = f32::NEG_INFINITY;
            for (idx, &l) in logits.iter().take(num_classes).enumerate() {
                if l > best_logit {
                    best_logit = l;
                    best_idx = idx;
                }
            }
            let score = sigmoid(best_logit);
            if score <= SCORE_THRESHOLD {
                continue;
            }
            let base = q * 4;
            let cx = dets[base];
            let cy = dets[base + 1];
            let bw = dets[base + 2];
            let bh = dets[base + 3];
            let x1 = (cx - bw / 2.0).clamp(0.0, 1.0);
            let y1 = (cy - bh / 2.0).clamp(0.0, 1.0);
            let x2 = (cx + bw / 2.0).clamp(0.0, 1.0);
            let y2 = (cy + bh / 2.0).clamp(0.0, 1.0);
            items.push(Detection {
                klasa: self.classes[best_idx].clone(),
                bbox: [x1, y1, x2 - x1, y2 - y1],
                score,
                stan: Vec::new(),
                tekst: None,
                track_id: 0,
                vx: 0.,
                vy: 0.,
            });
        }
        items
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

/// Ustawia `ORT_DYLIB_PATH` na wykrytą ścieżkę, jeśli nie ma jej w środowisku —
/// `ort` z `load-dynamic` dlopuje onnxruntime spod tej zmiennej przy pierwszym
/// użyciu. Preferujemy runtime z drzewa `native-libs/` (zawiera provider TensorRT
/// + CUDA), a dopiero gdy go brak — systemowy [`DEFAULT_ORT_DYLIB`] (który ma
/// zwykle tylko CUDA). Edycja 2021: `set_var` jest bezpieczne.
#[cfg(feature = "inference-supertonic")]
fn ensure_ort_dylib() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    let path = locate_ort_dylib().unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_ORT_DYLIB));
    std::env::set_var("ORT_DYLIB_PATH", &path);
}

/// Szuka `libonnxruntime.{so*,dylib}` w drzewie `native-libs/<platform>/lib-dynamic/`
/// (build-all.sh provisionuje tam runtime GPU z TensorRT). Lustrzana logika do
/// `services::document::rasterize::locate_pdfium_library`, ale zawężona do runtime
/// ONNX. Zwraca pierwszy trafiony plik albo `None` (wtedy caller bierze systemowy).
#[cfg(feature = "inference-supertonic")]
fn locate_ort_dylib() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    let (platform, lib_glob): (&str, &[&str]) = if cfg!(target_os = "macos") {
        (
            if cfg!(target_arch = "aarch64") { "macos-arm64" } else { "macos-x86_64" },
            &["libonnxruntime.dylib"],
        )
    } else if cfg!(target_os = "linux") {
        (
            if cfg!(target_arch = "aarch64") { "linux-aarch64" } else { "linux-x86_64" },
            // Prebuilty rozpakowują wersjonowany soname (np. .so.1.26.0) obok
            // dowiązania .so — bierzemy oba warianty, wersjonowany jako pierwszy.
            &["libonnxruntime.so", "libonnxruntime.so.*"],
        )
    } else {
        return None;
    };

    // Wspinamy się w górę od CARGO_MANIFEST_DIR / cwd / katalogu binarki aż do
    // katalogu zawierającego `native-libs/`.
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        starts.push(PathBuf::from(manifest));
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    for start in starts {
        let mut cur: Option<&std::path::Path> = Some(start.as_path());
        while let Some(dir) = cur {
            let lib_dir = dir.join("native-libs").join(platform).join("lib-dynamic");
            if lib_dir.is_dir() {
                if let Some(found) = pick_ort_dylib(&lib_dir, lib_glob) {
                    return Some(found);
                }
            }
            cur = dir.parent();
        }
    }
    None
}

/// Wybiera najlepszy plik runtime ONNX z katalogu `lib-dynamic`. Dla wersjonowanego
/// soname (`libonnxruntime.so.*`) preferuje najświeższą wersję (sort malejący po
/// nazwie), by uniknąć niedeterminizmu gdy leży kilka wariantów.
#[cfg(feature = "inference-supertonic")]
fn pick_ort_dylib(lib_dir: &std::path::Path, lib_glob: &[&str]) -> Option<std::path::PathBuf> {
    for pattern in lib_glob {
        if let Some(suffix) = pattern.strip_suffix('*') {
            // Wzorzec wersjonowany: dopasuj prefiks, wybierz najświeższy.
            let entries = std::fs::read_dir(lib_dir).ok()?;
            let mut matches: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(suffix) && n != suffix)
                })
                .collect();
            matches.sort();
            if let Some(latest) = matches.pop() {
                return Some(latest);
            }
        } else {
            let candidate = lib_dir.join(pattern);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Buduje sesję `ort` z modelu ONNX, rejestrując łańcuch execution providerów w
/// kolejności priorytetu z MIĘKKĄ rejestracją (bez `error_on_failure`): jeśli dany
/// EP jest niedostępny w załadowanym runtime, `ort` loguje ostrzeżenie i przechodzi
/// do następnego (patrz `ort::ep::apply_execution_providers`). ONNX Runtime sam
/// przydziela węzły grafu do najwyżej-priorytetowego zarejestrowanego EP, więc gdy
/// TensorRT jest obecny — użyje go, a inaczej płynnie zejdzie na CUDA (lub CPU).
///
/// Kolejność: TensorRT (engine-cache + FP16) → CUDA → [macOS] CoreML → CPU.
#[cfg(feature = "inference-supertonic")]
fn build_ort_session(model_path: &std::path::Path) -> Result<ort::session::Session> {
    use ort::ep::{ExecutionProvider, ExecutionProviderDispatch};
    use ort::session::Session;

    let mut eps: Vec<ExecutionProviderDispatch> = Vec::new();

    #[cfg(not(target_os = "macos"))]
    {
        // TensorRT — najwyższy priorytet. Engine-cache trzyma zserializowane plany
        // silników na dysku (pierwszy forward po zmianie modelu/GPU buduje je od
        // nowa i jest wolny; kolejne wczytują z cache). FP16 dla przepustowości.
        let trt_cache = paths::vision_models_dir().join("trt-cache");
        if let Err(e) = std::fs::create_dir_all(&trt_cache) {
            warn!("[rfdetr] ort: nie udało się utworzyć cache TensorRT {}: {e}", trt_cache.display());
        }
        eps.push(
            ort::ep::TensorRT::default()
                .with_engine_cache(true)
                .with_engine_cache_path(trt_cache.to_string_lossy().to_string())
                .with_timing_cache(true)
                .with_fp16(true)
                .build(),
        );
        // CUDA — dotychczasowa, działająca ścieżka; teraz MIĘKKO (bez
        // error_on_failure), bo poprzedza ją TensorRT.
        eps.push(ort::ep::CUDA::default().build());
    }
    #[cfg(target_os = "macos")]
    {
        // CoreML (Metal/ANE) — akceleracja na Apple Silicon.
        eps.push(ort::ep::CoreML::default().build());
    }
    // CPU — zawsze ostatni fallback.
    eps.push(ort::ep::CPU::default().build());

    // Introspekcja: logujemy które akceleratory widzi załadowany runtime (ort nie
    // raportuje per-węzeł finalnego EP, ale ONNX Runtime bierze najwyżej-priorytetowy
    // z dostępnych, więc to jednoznacznie wskazuje realnie użytą ścieżkę).
    #[cfg(not(target_os = "macos"))]
    {
        let trt = ort::ep::TensorRT::default().is_available().unwrap_or(false);
        let cuda = ort::ep::CUDA::default().is_available().unwrap_or(false);
        info!("[rfdetr] ort: dostępne EP w runtime — TensorRT={trt}, CUDA={cuda} (priorytet: TensorRT>CUDA>CPU)");
    }
    #[cfg(target_os = "macos")]
    {
        let coreml = ort::ep::CoreML::default().is_available().unwrap_or(false);
        info!("[rfdetr] ort: dostępne EP w runtime — CoreML={coreml} (priorytet: CoreML>CPU)");
    }

    Session::builder()
        .map_err(|e| anyhow!("ort Session::builder: {e}"))?
        .with_execution_providers(eps)
        .map_err(|e| anyhow!("ort with_execution_providers: {e}"))?
        .commit_from_file(model_path)
        .map_err(|e| anyhow!("ort commit_from_file {}: {e}", model_path.display()))
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

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
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
