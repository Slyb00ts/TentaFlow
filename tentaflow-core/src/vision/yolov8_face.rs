// =============================================================================
// Plik: vision/yolov8_face.rs
// Opis: YOLOv8/v11-face detector. Stub — `load()` waliduje ONNX, `detect()`
//       jeszcze nie zaimplementowany (TODO vision-2).
//
//       Pipeline (po implementacji): letterbox 640x640 (gray 114) →
//       (px/255) NCHW → tract forward → output (1, 5+kps, num_anchors)
//       → score threshold + NMS → unletterbox.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::{FaceDetection, FaceDetector};

const YOLO_INPUT_SIZE: u32 = 640;

type RunnableYolo = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct Yolov8FaceEngine {
    #[allow(dead_code)]
    model: Arc<RunnableYolo>,
}

impl FaceDetector for Yolov8FaceEngine {
    fn detect(&self, _image_rgb: &[u8], _width: u32, _height: u32) -> Result<Vec<FaceDetection>> {
        Err(anyhow!(
            "vision yolov8-face: detect() jeszcze nie wpiety — wpisz log z `tract output shape` i dorobimy decoding"
        ))
    }
}

pub fn load(model_path: &Path) -> Result<Yolov8FaceEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "YOLOv8-face ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    let model = tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: nie udalo sie wczytac YOLO ONNX z {}", model_path.display()))?
        .with_input_fact(
            0,
            InferenceFact::dt_shape(
                f32::datum_type(),
                tvec!(1, 3, YOLO_INPUT_SIZE as i32, YOLO_INPUT_SIZE as i32),
            ),
        )?
        .into_optimized()?
        .into_runnable()?;
    Ok(Yolov8FaceEngine {
        model: Arc::new(model),
    })
}
