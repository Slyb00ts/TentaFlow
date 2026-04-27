// =============================================================================
// Plik: vision/hsemotion.rs
// Opis: HSEmotion (EfficientNet-B0 + AffectNet 8 emocji).
//       Model `enet_b0_8_best_afew.onnx` — input 224x224 RGB, ImageNet
//       preprocessing (mean=[0.485, 0.456, 0.406], std=[0.229, 0.224, 0.225],
//       wszystko skalowane * 255 bo robimy z f32 [0..255]).
//       Output: logits[8] dla emocji (anger/contempt/disgust/fear/happy/
//       neutral/sad/surprise — kolejnosc AffectNet8 standardowa).
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use image::imageops::FilterType;
use tract_onnx::prelude::*;

use super::preprocessing::{rgb_buf_to_image, rgb_to_nchw_imagenet};
use super::{EmotionClassifier, EmotionResult};

const INPUT_SIZE: u32 = 224;

/// Kolejnosc klas — alfabetyczna AffectNet8 (zgodna z `enet_b0_8_best_afew`).
/// Patrz: HSE-asavchenko/face-emotion-recognition/src/affectnet_emotions.py.
pub const EMOTION_LABELS: [&str; 8] = [
    "Anger", "Contempt", "Disgust", "Fear", "Happiness", "Neutral", "Sadness", "Surprise",
];

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct HsemotionEngine {
    model: Arc<Runnable>,
}

impl EmotionClassifier for HsemotionEngine {
    fn classify(
        &self,
        face_crop_rgb: &[u8],
        width: u32,
        height: u32,
    ) -> Result<EmotionResult> {
        let img = rgb_buf_to_image(face_crop_rgb, width, height)
            .ok_or_else(|| anyhow!("HSEmotion: invalid RGB buffer"))?;
        let resized = image::imageops::resize(&img, INPUT_SIZE, INPUT_SIZE, FilterType::Triangle);
        let nchw = rgb_to_nchw_imagenet(&resized);

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize),
            nchw,
        )
        .context("HSEmotion: nchw shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("HSEmotion: tract forward failed")?;

        let logits = outputs[0]
            .as_slice::<f32>()
            .context("HSEmotion: output nie jest f32")?;
        if logits.len() < EMOTION_LABELS.len() {
            return Err(anyhow!(
                "HSEmotion: output ma {} elementow, oczekiwano >= {}",
                logits.len(),
                EMOTION_LABELS.len()
            ));
        }

        // Stable softmax na pierwszych 8 klasach.
        let used = &logits[..EMOTION_LABELS.len()];
        let max_l = used.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let mut sum_e = 0.0f32;
        let mut exps = [0f32; 8];
        for (i, &l) in used.iter().enumerate() {
            let e = (l - max_l).exp();
            exps[i] = e;
            sum_e += e;
        }
        let probs: Vec<(String, f32)> = exps
            .iter()
            .enumerate()
            .map(|(i, &e)| (EMOTION_LABELS[i].to_string(), e / sum_e))
            .collect();

        let (best_idx, _best_prob) = probs
            .iter()
            .enumerate()
            .max_by(|a, b| a.1 .1.partial_cmp(&b.1 .1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, p)| (i, p.1))
            .unwrap_or((0, 0.0));

        Ok(EmotionResult {
            label: EMOTION_LABELS[best_idx].to_string(),
            probabilities: probs,
            valence: None,
            arousal: None,
        })
    }
}

pub fn load(model_path: &Path) -> Result<HsemotionEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "HSEmotion ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    let model = tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: HSEmotion ONNX z {}", model_path.display()))?
        .with_input_fact(
            0,
            InferenceFact::dt_shape(
                f32::datum_type(),
                tvec!(1, 3, INPUT_SIZE as i32, INPUT_SIZE as i32),
            ),
        )?
        .into_optimized()?
        .into_runnable()?;
    Ok(HsemotionEngine {
        model: Arc::new(model),
    })
}
