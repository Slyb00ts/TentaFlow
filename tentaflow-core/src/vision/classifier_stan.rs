// =============================================================================
// File: vision/classifier_stan.rs — placard-state classifier (Burn)
// =============================================================================
//
// Multi-label condition classifier for ADR placards/labels (MobileNetV4).
// Architecture vendored as `burn_stan` (build-time ONNX→Burn codegen); weights
// load at runtime from `model_stan.bpk`. Turns a detector crop into state tags
// (e.g. ["uszkodzona", "wyblakla"]).
//
// Preprocessing mirrors the training transform: RGB crop → SxS bilinear stretch
// → /255 → per-channel ImageNet normalize → NCHW f32 [1,3,S,S]. Postprocessing
// is per-class sigmoid with a per-class threshold (`progi`): a class is emitted
// when `prob[i] > progi[i]`, so the result is multi-label and may be empty.

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
#[derive(Debug, Deserialize)]
struct ClassesFile {
    classes: Vec<String>,
    img_size: u32,
    mean: [f32; 3],
    std: [f32; 3],
    progi: Vec<f32>,
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
    progi: Vec<f32>,
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
        if parsed.progi.len() != parsed.classes.len() {
            bail!(
                "stan-classes.json: progi len {} != classes len {}",
                parsed.progi.len(),
                parsed.classes.len()
            );
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
            progi: parsed.progi,
        })
    }

    /// Runs one crop through the classifier. `crop_rgb` is tightly packed RGB24
    /// of size `cw*ch*3`. Returns the matched state tags (multi-label).
    pub fn classify(&mut self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<String>> {
        let s = self.img_size as usize;
        let resized = crate::vision::resize::resize_rgb(crop_rgb, cw, ch, self.img_size, self.img_size)
            .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
        let plane = s * s;
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
        let input =
            Tensor::<VisionBackend, 4>::from_data(TensorData::new(data, [1, 3, s, s]), &self.device);

        let out =
            crate::vision::burn_backend::guarded_forward("state-classifier", || self.model.forward(input))?;
        let logits: Vec<f32> = out
            .to_data()
            .to_vec()
            .map_err(|e| anyhow!("state logits to_vec: {e:?}"))?;

        let num_classes = self.classes.len();
        if logits.len() < num_classes {
            bail!("logits len {} < class count {}", logits.len(), num_classes);
        }

        let mut stany = Vec::new();
        for (i, name) in self.classes.iter().enumerate() {
            if sigmoid(logits[i]) > self.progi[i] {
                stany.push(name.clone());
            }
        }
        Ok(stany)
    }
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
