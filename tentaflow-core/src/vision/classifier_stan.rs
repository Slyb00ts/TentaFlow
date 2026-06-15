// =============================================================================
// File: vision/classifier_stan.rs — placard-state classifier (ONNX via `ort`)
// =============================================================================
//
// Multi-label condition classifier for ADR placards/labels (MobileNetV4).
// Loads `model_stan.onnx` (+ external weights `model_stan.onnx.data` next to it,
// which `ort` picks up automatically) through ONNX Runtime and turns a detector
// crop into a set of state tags (e.g. ["uszkodzona", "wyblakla"]).
//
// Preprocessing mirrors the training transform: RGB crop → 160×160 bilinear
// stretch → /255 → per-channel ImageNet normalize → NCHW f32 [1,3,160,160].
// Postprocessing is per-class sigmoid with a per-class threshold (`progi`): a
// class is emitted when `prob[i] > progi[i]`, so the result is multi-label and
// may be empty or contain several tags.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Value;
use serde::Deserialize;
use tracing::info;

use crate::paths;

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

/// Loaded state-classifier session plus its config. `Session::run` needs
/// `&mut`, so `classify` takes `&mut self`; a single shared instance is driven
/// from one analysis task (or behind a mutex when shared across cameras).
pub struct StateClassifier {
    session: Session,
    classes: Vec<String>,
    mean: [f32; 3],
    std: [f32; 3],
    img_size: u32,
    progi: Vec<f32>,
    /// Graph input tensor name, read from the model so we do not hard-code it.
    input_name: String,
}

impl StateClassifier {
    /// Builds the classifier from the deploy-time model dir
    /// (`vision_models_dir()/model_stan.onnx` + `stan-classes.json`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let model_path = dir.join("model_stan.onnx");
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

        let session = super::ort_session::build_session(&model_path)?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("state classifier model has no inputs"))?;

        info!(
            "[classifier_stan] loaded {} ({} classes, {}px, input '{}')",
            model_path.display(),
            parsed.classes.len(),
            parsed.img_size,
            input_name
        );
        Ok(Self {
            session,
            classes: parsed.classes,
            mean: parsed.mean,
            std: parsed.std,
            img_size: parsed.img_size,
            progi: parsed.progi,
            input_name,
        })
    }

    /// Runs one crop through the classifier. `crop_rgb` is tightly packed RGB24
    /// of size `cw*ch*3`. Returns the matched state tags (multi-label; may be
    /// empty or hold several entries).
    pub fn classify(&mut self, crop_rgb: &[u8], cw: u32, ch: u32) -> Result<Vec<String>> {
        let input = self.preprocess(crop_rgb, cw, ch)?;
        let input_value = Value::from_array(input)?;

        let outputs = self
            .session
            .run(ort::inputs! { self.input_name.as_str() => &input_value })?;

        let output = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow!("state classifier produced no outputs"))?
            .1;
        let (logits_shape, logits) = output.try_extract_tensor::<f32>()?;

        let num_classes = self.classes.len();
        let total: usize = logits.len();
        if total < num_classes {
            bail!(
                "logits len {} < class count {} (shape {:?})",
                total,
                num_classes,
                &*logits_shape
            );
        }

        let mut stany = Vec::new();
        for (i, name) in self.classes.iter().enumerate() {
            let prob = sigmoid(logits[i]);
            if prob > self.progi[i] {
                stany.push(name.clone());
            }
        }
        Ok(stany)
    }

    /// RGB24 crop → NCHW f32 [1,3,S,S]: stretch-resize, /255, ImageNet normalize.
    fn preprocess(&self, rgb: &[u8], w: u32, h: u32) -> Result<Array4<f32>> {
        let s = self.img_size;
        let resized = crate::vision::resize::resize_rgb(rgb, w, h, s, s)
            .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;

        let su = s as usize;
        let mut tensor = Array4::<f32>::zeros((1, 3, su, su));
        for y in 0..su {
            for x in 0..su {
                let p = (y * su + x) * 3;
                for c in 0..3 {
                    let v = resized[p + c] as f32 / 255.0;
                    tensor[[0, c, y, x]] = (v - self.mean[c]) / self.std[c];
                }
            }
        }
        Ok(tensor)
    }
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
