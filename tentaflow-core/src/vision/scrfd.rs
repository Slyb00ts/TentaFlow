// =============================================================================
// Plik: vision/scrfd.rs
// Opis: SCRFD (InsightFace) face detector. Pure Rust ONNX inference przez
//       tract-onnx. Model `det_500m.onnx` z buffalo_s: 3 strides (8/16/32),
//       2 anchory per pozycja, FPN heads zwracajace score/bbox/kps tensorom.
//
//       Pipeline detect():
//         1. RGB → letterbox 640x640 (pad gray 0,0,0)
//         2. Normalize (px-127.5)/128.0 → NCHW f32 (1,3,640,640)
//         3. tract forward — 9 output tensorow per stride
//         4. Decode anchorow: anchor_center = (gx*stride + stride/2, ...)
//            bbox_xy = anchor_center +/- distance * stride
//         5. Score threshold 0.5, NMS IoU 0.4
//         6. Unletterbox: rescale do oryginalnego rozmiaru obrazka
//
//       UWAGA: full decode jest do dorobienia w nastepnej iteracji — wymaga
//       wgladu w faktyczny shape output tensorow konkretnego ONNX (det_500m
//       vs det_2.5g vs det_10g maja minimalnie rozne layouty). load()
//       jest gotowe i potwierdza ze model laduje sie czysto przez tract.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::preprocessing::{letterbox, rgb_buf_to_image, rgb_to_nchw_scrfd};
use super::{FaceDetection, FaceDetector};

/// Input size SCRFD det_500m / det_2.5g / det_10g — wszystkie zoptymalizowane
/// pod 640x640, wieksze input daje minimalna poprawe recall ale duzy koszt CPU.
const SCRFD_INPUT_SIZE: u32 = 640;

/// Score threshold po sigmoid'zie (faktyczny output ONNX'a). 0.5 to dobry
/// kompromis precision/recall dla face detection na typowym video calls.
const SCORE_THRESHOLD: f32 = 0.5;

/// IoU threshold dla NMS. SCRFD authors sugeruja 0.4 — agresywniej tnie
/// duplikaty niz typowe 0.5, dziala lepiej dla zbliżonych twarzy.
#[allow(dead_code)]
const NMS_IOU_THRESHOLD: f32 = 0.4;

type RunnableScrfd = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct ScrfdEngine {
    /// Pre-built tract plan z fixed input shape 640x640. Inference wola
    /// `model.run(...)` ktore jest thread-safe przez Arc.
    model: Arc<RunnableScrfd>,
}

impl ScrfdEngine {
    pub fn new(model_path: &Path) -> Result<Self> {
        let model = build_runnable(model_path)?;
        Ok(Self {
            model: Arc::new(model),
        })
    }
}

impl FaceDetector for ScrfdEngine {
    fn detect(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<FaceDetection>> {
        let img = rgb_buf_to_image(image_rgb, width, height)
            .ok_or_else(|| anyhow!("invalid RGB buffer ({} bytes for {}x{})", image_rgb.len(), width, height))?;

        let (canvas, meta) = letterbox(&img, SCRFD_INPUT_SIZE, [0, 0, 0]);
        let nchw = rgb_to_nchw_scrfd(&canvas);

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, SCRFD_INPUT_SIZE as usize, SCRFD_INPUT_SIZE as usize),
            nchw,
        )
        .context("nchw shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("tract SCRFD forward failed")?;

        // FULL DECODE — anchor decoding per stride, score threshold, NMS,
        // unletterbox — bedzie dorobiony w kolejnym kroku po pierwszym
        // logu shape'ow z realnego ONNX'a (rozne warianty SCRFD maja
        // troche rozne layouty: kanal-pierwszy vs anchor-pierwszy).
        // Tymczasowo: zwracamy pusta liste + log diagnostyczny ze shape'ami.
        for (i, t) in outputs.iter().enumerate() {
            tracing::debug!(
                "SCRFD output[{}] shape={:?} dt={:?}",
                i,
                t.shape(),
                t.datum_type()
            );
        }
        let _ = meta;

        // TODO(vision-2): anchor decode + NMS — wymaga shape inspection
        // pod konkretny det_500m.onnx z buffalo_s. Tymczasowo zwracamy [].
        Ok(Vec::new())
    }
}

/// Buduje tract `SimplePlan` z fixed shape 640x640. Pre-built plan jest
/// reusable miedzy wieloma `model.run()` — alokowany raz, zero kosztu
/// kompilacji per request.
fn build_runnable(model_path: &Path) -> Result<RunnableScrfd> {
    let model = tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: nie udalo sie wczytac SCRFD ONNX z {}", model_path.display()))?
        .with_input_fact(
            0,
            InferenceFact::dt_shape(
                f32::datum_type(),
                tvec!(1, 3, SCRFD_INPUT_SIZE as i32, SCRFD_INPUT_SIZE as i32),
            ),
        )?
        .into_optimized()?
        .into_runnable()?;
    Ok(model)
}

/// Wolany przez `vision::load_engine(VisionEngineKind::Scrfd, path)`. Zwraca
/// silnik gotowy do `detect()`. Tract walidacja struktury ONNX'a przy
/// `into_optimized()` — bledny model failuje tutaj, nie przy pierwszym
/// inference.
pub fn load(model_path: &Path) -> Result<ScrfdEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "SCRFD ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    let _ = SCORE_THRESHOLD;
    ScrfdEngine::new(model_path)
}
