// =============================================================================
// File: vision/classifier_stan.rs — placard-state classifier (ort+TRT / Burn)
// =============================================================================
//
// Single-label condition classifier for ADR placards/labels (MobileNetV4).
// Backend inferencji wybierany cfg/feature:
//   * `inference-supertonic` (ONNX Runtime, crate `ort`) → pula sesji ort z
//     łańcuchem EP TensorRT→CUDA→CPU, model `model_stan.onnx`. Pula jest
//     wewnętrznie współbieżna (round-robin `Mutex<Session>`), więc forward NIE
//     idzie już przez jednowątkowy egzekutor Burn/wgpu — cold-path enrichment
//     nie serializuje się na tym jednym wątku ani nie konkuruje z detektorem.
//   * inaczej → wendorowany `burn_stan` (build-time ONNX→Burn codegen), wagi z
//     `model_stan.bpk`; forward MUSI iść przez `burn_backend::run_blocking`
//     (jeden wątek GPU — równoległe forwardy wgpu psują pamięć).
//
// Preprocessing i postprocessing są backend-agnostyczne i IDENTYCZNE dla obu
// ścieżek: RGB crop → SxS bilinear stretch → /255 → per-channel ImageNet
// normalize → NCHW f32 [1,3,S,S]; argmax po logitach (równoważny argmaxowi po
// softmaxie — softmax jest monotoniczny) → dokładnie jedna etykieta stanu.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
#[cfg(not(feature = "inference-supertonic"))]
use burn::tensor::{Tensor, TensorData};
#[cfg(not(feature = "inference-supertonic"))]
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::Deserialize;
use tracing::info;

use crate::paths;
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
#[cfg(not(feature = "inference-supertonic"))]
use crate::vision::burn_stan::Model;

/// Nazwa tensora wejściowego w grafie ONNX (`[batch,3,S,S]`).
#[cfg(feature = "inference-supertonic")]
const INPUT_NAME: &str = "input";

/// Env sterujący rozmiarem puli sesji ort klasyfikatora. Domyślnie 1 = ścieżka
/// bit-identyczna z pojedynczą sesją (jeden forward naraz), a >1 pozwala wielu
/// cropom klasyfikować się równolegle na GPU.
#[cfg(feature = "inference-supertonic")]
const STAN_SESSIONS_ENV: &str = "TENTAFLOW_STAN_SESSIONS";
#[cfg(feature = "inference-supertonic")]
const DEFAULT_STAN_SESSIONS: usize = 1;

/// `stan-classes.json` shape — the deploy-time config next to the model.
/// `progi` jest ignorowane dla modelu softmax (single-label), więc opcjonalne.
#[derive(Debug, Deserialize)]
struct ClassesFile {
    classes: Vec<String>,
    img_size: u32,
    mean: [f32; 3],
    std: [f32; 3],
    #[serde(default)]
    #[allow(dead_code)]
    progi: Option<Vec<f32>>,
    #[allow(dead_code)]
    activation: String,
}

/// Loaded state classifier + config + backend.
pub struct StateClassifier {
    /// Pula sesji ONNX Runtime (TensorRT→CUDA→CPU) — ścieżka ort. Wewnętrznie
    /// współbieżna, więc `classify` bierze `&self` (interior mutability).
    #[cfg(feature = "inference-supertonic")]
    pool: crate::vision::ort_common::SessionPool,
    #[cfg(not(feature = "inference-supertonic"))]
    model: Model<VisionBackend>,
    #[cfg(not(feature = "inference-supertonic"))]
    device: VisionDevice,
    classes: Vec<String>,
    mean: [f32; 3],
    std: [f32; 3],
    img_size: u32,
}

impl StateClassifier {
    /// Builds the classifier from the deploy-time model dir
    /// (`vision_models_dir()/{model_stan.onnx|model_stan.bpk}` + `stan-classes.json`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let classes_path = dir.join("stan-classes.json");

        let classes_bytes = std::fs::read(&classes_path)
            .with_context(|| format!("read {}", classes_path.display()))?;
        let parsed: ClassesFile = serde_json::from_slice(&classes_bytes)
            .with_context(|| format!("parse {}", classes_path.display()))?;
        if parsed.classes.is_empty() {
            bail!("stan-classes.json has no classes");
        }
        if parsed.img_size == 0 {
            bail!("stan-classes.json: img_size must be > 0");
        }

        // Ścieżka ort+TensorRT: pula sesji na modelu dynamic-batch (`model_stan.onnx`),
        // każda z własnym engine-cache; jeden crop = forward batch=1.
        #[cfg(feature = "inference-supertonic")]
        {
            let onnx_path = dir.join("model_stan.onnx");
            if !onnx_path.exists() {
                bail!("state-classifier ONNX missing: {}", onnx_path.display());
            }
            crate::vision::ort_common::ensure_ort_dylib();
            // Klasyfikacja to zawsze pojedynczy crop → pin jednego silnika TRT na
            // batch=1 (min=opt=max), by pierwszy forward nie budował silnika lazy.
            let trt_profile = crate::vision::ort_common::TrtShapeProfile {
                input_name: INPUT_NAME.to_string(),
                min_batch: 1,
                opt_batch: 1,
                max_batch: 1,
                channels: 3,
                height: parsed.img_size,
                width: parsed.img_size,
            };
            let n = crate::vision::ort_common::pool_size_from_env(
                STAN_SESSIONS_ENV,
                DEFAULT_STAN_SESSIONS,
            );
            let pool = crate::vision::ort_common::build_session_pool_from_file(
                &onnx_path,
                &dir.join("trt-cache-stan"),
                Some(&trt_profile),
                n,
            )?;
            info!(
                "[classifier_stan] loaded {} ({} classes, {}px, backend ort TensorRT→CUDA→CPU, pool={} session(s))",
                onnx_path.display(),
                parsed.classes.len(),
                parsed.img_size,
                pool.len()
            );
            Ok(Self {
                pool,
                classes: parsed.classes,
                mean: parsed.mean,
                std: parsed.std,
                img_size: parsed.img_size,
            })
        }

        // Ścieżka Burn: wagi `.bpk` na wybranym backendzie vision-*.
        #[cfg(not(feature = "inference-supertonic"))]
        {
            let weights_path = dir.join("model_stan.bpk");
            if !weights_path.exists() {
                bail!("state-classifier weights missing: {}", weights_path.display());
            }
            let device = burn_backend::device();
            let mut model = Model::<VisionBackend>::new(&device);
            let mut store = BurnpackStore::from_file(&weights_path);
            model
                .load_from(&mut store)
                .map_err(|e| anyhow!("load state weights {}: {e}", weights_path.display()))?;

            info!(
                "[classifier_stan] loaded {} ({} classes, {}px)",
                weights_path.display(),
                parsed.classes.len(),
                parsed.img_size
            );
            Ok(Self {
                model,
                device,
                classes: parsed.classes,
                mean: parsed.mean,
                std: parsed.std,
                img_size: parsed.img_size,
            })
        }
    }

    /// Stretch-resize + /255 + per-channel ImageNet normalize → flat NCHW f32
    /// `[3,S,S]` (batch=1). Backend-agnostyczny preprocessing współdzielony przez
    /// ścieżkę ort i Burn — piksele co do bitu takie same.
    fn preprocess(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<f32>> {
        let s = self.img_size as usize;
        let plane = s * s;
        let resized =
            crate::vision::resize::resize_rgb(crop_rgb, cw, ch, self.img_size, self.img_size)
                .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
        let mut data = vec![0f32; 3 * plane];
        for y in 0..s {
            for x in 0..s {
                let p = (y * s + x) * 3;
                for c in 0..3 {
                    let v = resized[p + c] as f32 / 255.0;
                    data[c * plane + y * s + x] = (v - self.mean[c]) / self.std[c];
                }
            }
        }
        Ok(data)
    }

    /// Argmax po logitach → etykieta stanu w wektorze (kształt `Detection.stan:
    /// Vec<String>`). Waliduje długość logitów == liczba klas przed indeksowaniem.
    fn label_from_logits(&self, logits: &[f32]) -> Result<Vec<String>> {
        let num_classes = self.classes.len();
        if logits.len() != num_classes {
            bail!(
                "state classifier logits len {} != class count {}",
                logits.len(),
                num_classes
            );
        }
        Ok(vec![self.classes[argmax(logits)].clone()])
    }

    /// Runs one crop through the classifier. `crop_rgb` is tightly packed RGB24
    /// of size `cw*ch*3`. Zwraca dokładnie jedną etykietę stanu (klasa o
    /// najwyższym logicie = najwyższym prawdopodobieństwie softmax) w wektorze.
    ///
    /// Ścieżka ort: `&self` (pula sesji jest wewnętrznie współbieżna). Forward
    /// batch=1 na modelu dynamic-batch, wyjście `logits [1, num_classes]`.
    #[cfg(feature = "inference-supertonic")]
    pub fn classify(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<String>> {
        let s = self.img_size as usize;
        let data = self.preprocess(crop_rgb, cw, ch)?;
        let input = ndarray::Array4::from_shape_vec((1, 3, s, s), data)
            .map_err(|e| anyhow!("classifier_stan: build tensor [1,3,{s},{s}]: {e}"))?;
        let value = ort::value::Value::from_array(input)
            .map_err(|e| anyhow!("classifier_stan: Value::from_array: {e}"))?;

        let mut session = self.pool.checkout()?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("classifier_stan: model has no inputs"))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| anyhow!("classifier_stan: model has no outputs"))?;
        let outputs = session
            .run(ort::inputs! { input_name => value })
            .map_err(|e| anyhow!("classifier_stan: session.run: {e}"))?;
        let (shape, logits) = outputs[output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("classifier_stan: extract logits: {e}"))?;
        let num_classes = self.classes.len();
        if shape.len() != 2 || shape[0] != 1 || shape[1] as usize != num_classes {
            bail!(
                "state classifier output shape {shape:?} != [1, {num_classes}]"
            );
        }
        let logits = logits[..num_classes].to_vec();
        drop(outputs);
        drop(session);
        self.label_from_logits(&logits)
    }

    /// Ścieżka Burn: `&self` (generated `Model::forward` bierze `&self`), ale
    /// caller MUSI serializować forwardy jednym wątkiem przez
    /// `burn_backend::run_blocking` — równoległe forwardy wgpu psują pamięć.
    /// Model wkompilowany pod `[1,3,S,S]` — jeden crop = jeden forward batch=1.
    #[cfg(not(feature = "inference-supertonic"))]
    pub fn classify(&self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<String>> {
        let s = self.img_size as usize;
        let data = self.preprocess(crop_rgb, cw, ch)?;
        let input = Tensor::<VisionBackend, 4>::from_data(
            TensorData::new(data, [1, 3, s, s]),
            &self.device,
        );

        let out = crate::vision::burn_backend::guarded_forward("state-classifier", || {
            self.model.forward(input)
        })?;

        // Kształt wyjścia musi być dokładnie [1, num_classes] — inaczej model/eksport
        // nie pasuje do kontraktu. Walidujemy PRZED `to_vec` (jak detektor dims).
        let num_classes = self.classes.len();
        let dims = out.dims();
        if dims != [1, num_classes] {
            bail!(
                "state classifier output dims {:?} != [1, {}]",
                dims,
                num_classes
            );
        }

        let logits: Vec<f32> = out
            .to_data()
            .to_vec()
            .map_err(|e| anyhow!("state logits to_vec: {e:?}"))?;

        self.label_from_logits(&logits)
    }
}

/// Argmax po logitach (single-label softmax — argmax po logitach jest równoważny
/// argmaxowi po softmaxie, bo softmax jest monotoniczny). Pusty wycinek → 0.
#[inline]
fn argmax(logits: &[f32]) -> usize {
    let mut best_idx = 0usize;
    let mut best_logit = f32::NEG_INFINITY;
    for (idx, &l) in logits.iter().enumerate() {
        if l > best_logit {
            best_logit = l;
            best_idx = idx;
        }
    }
    best_idx
}

#[cfg(test)]
mod tests {
    use super::argmax;

    #[test]
    fn argmax_picks_highest_logit() {
        assert_eq!(argmax(&[0.1, 0.9, 0.3, 0.2]), 1);
        assert_eq!(argmax(&[2.0, 1.0, 0.5]), 0);
        assert_eq!(argmax(&[-1.0, -0.5, -2.0]), 1);
        // Pusty wycinek → 0 (nie panikuje).
        assert_eq!(argmax(&[]), 0);
    }
}
