// =============================================================================
// Plik: vision/hsemotion.rs
// Opis: HSEmotion (EfficientNet-B0 + AffectNet 8 emocji). Stub — load() jest,
//       classify() do dorobienia w nastepnej iteracji.
//
//       Kolejnosc klas (AffectNet 8): neutral, happy, sad, surprise, fear,
//       disgust, anger, contempt.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::{EmotionClassifier, EmotionResult};

const INPUT_SIZE: u32 = 260;

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

#[allow(dead_code)]
pub const EMOTION_LABELS: &[&str] = &[
    "neutral", "happy", "sad", "surprise", "fear", "disgust", "anger", "contempt",
];

pub struct HsemotionEngine {
    #[allow(dead_code)]
    model: Arc<Runnable>,
}

impl EmotionClassifier for HsemotionEngine {
    fn classify(
        &self,
        _face_crop_rgb: &[u8],
        _width: u32,
        _height: u32,
    ) -> Result<EmotionResult> {
        Err(anyhow!(
            "vision hsemotion: classify() jeszcze nie wpiety"
        ))
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
