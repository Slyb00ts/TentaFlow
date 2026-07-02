// =============================================================================
// File: vision/classifier_stan.rs — placard-state classifier (Burn)
// =============================================================================
//
// Single-label condition classifier for ADR placards/labels (MobileNetV4).
// Architecture vendored as `burn_stan` (build-time ONNX→Burn codegen); weights
// load at runtime from `model_stan.bpk`. Turns a detector crop into ONE state
// tag (np. "uszkodzona").
//
// Preprocessing mirrors the training transform: RGB crop → SxS bilinear stretch
// → /255 → per-channel ImageNet normalize → NCHW f32 [1,3,S,S]. Model jest
// softmaxowym klasyfikatorem single-label (4 klasy) — postprocessing bierze
// argmax po logitach (równoważny argmaxowi po softmaxie), więc wynik to zawsze
// dokładnie jedna etykieta o najwyższym prawdopodobieństwie.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
use burn::tensor::{Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::Deserialize;
use tracing::info;

use crate::paths;
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
use crate::vision::burn_stan::Model;

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

/// Loaded state classifier + config + backend device.
pub struct StateClassifier {
    model: Model<VisionBackend>,
    device: VisionDevice,
    classes: Vec<String>,
    mean: [f32; 3],
    std: [f32; 3],
    img_size: u32,
}

impl StateClassifier {
    /// Builds the classifier from the deploy-time model dir
    /// (`vision_models_dir()/model_stan.bpk` + `stan-classes.json`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let weights_path = dir.join("model_stan.bpk");
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

    /// Runs one crop through the classifier. `crop_rgb` is tightly packed RGB24
    /// of size `cw*ch*3`. Zwraca dokładnie jedną etykietę stanu (klasa o
    /// najwyższym logicie = najwyższym prawdopodobieństwie softmax) w wektorze,
    /// żeby zachować kształt `Detection.stan: Vec<String>`.
    ///
    /// Model jest wkompilowany na sztywno pod `[1,3,S,S]` — jeden crop = jeden
    /// forward batch=1. Preprocessing: stretch-resize do S×S, /255, per-channel
    /// ImageNet normalize → NCHW f32 `[1,3,S,S]`; postprocessing to argmax po
    /// logitach (single-label softmax).
    pub fn classify(&mut self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<String>> {
        let s = self.img_size as usize;
        let plane = s * s;
        let num_classes = self.classes.len();

        let resized = crate::vision::resize::resize_rgb(crop_rgb, cw, ch, self.img_size, self.img_size)
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

        let input = Tensor::<VisionBackend, 4>::from_data(
            TensorData::new(data, [1, 3, s, s]),
            &self.device,
        );

        let out = crate::vision::burn_backend::guarded_forward("state-classifier", || {
            self.model.forward(input)
        })?;

        // Kształt wyjścia musi być dokładnie [1, num_classes] — inaczej model/eksport
        // nie pasuje do kontraktu. Walidujemy PRZED `to_vec` (jak detektor dims).
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

        Ok(vec![self.classes[argmax(&logits)].clone()])
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
