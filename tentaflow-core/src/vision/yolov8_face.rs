// =============================================================================
// Plik: vision/yolov8_face.rs
// Opis: YOLOv8/v11-face detector. Pure Rust ONNX inference przez tract-onnx.
//
//       Output YOLOv8/v11-face: jeden tensor (1, attrs, 8400) dla 640x640:
//         - 4    = bbox (cx, cy, w, h) w pikselach input image
//         - 1    = cls score (0..1) dla jedynej klasy "face"
//         - 5*3  = 5 keypointow * (x, y, visibility) — TYLKO w eksportach -pose
//       Czysty detektor 1-klasowy daje attrs=5 (bez keypointow), wariant
//       z keypointami attrs=20. Niektore exporty transponuja na (1, 8400, attrs)
//       — layout wykrywamy heurystyka po osi rownej attrs.
//
//       Decode:
//         1. Filter score >= 0.5
//         2. cxcywh -> xyxy
//         3. Unletterbox (rescale do oryginalnego obrazka)
//         4. NMS IoU 0.45
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::nms::nms;
use super::preprocessing::{letterbox, rgb_buf_to_image, rgb_to_nchw_normalized, unletterbox_xy};
use super::{FaceDetection, FaceDetector};

const YOLO_INPUT_SIZE: u32 = 640;
const SCORE_THRESHOLD: f32 = 0.5;
const NMS_IOU_THRESHOLD: f32 = 0.45;

/// Liczba atrybutow per anchor dla 1-klasowego eksportu YOLOv8/v11-face:
/// 5 = 4 bbox + 1 score (bez keypointow), 20 = 4 + 1 + 5*3 (z keypointami).
#[inline]
fn is_attrs(n: usize) -> bool {
    n == 5 || n == 20
}

type RunnableYolo = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct Yolov8FaceEngine {
    model: Arc<RunnableYolo>,
}

impl FaceDetector for Yolov8FaceEngine {
    fn detect(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<FaceDetection>> {
        let img = rgb_buf_to_image(image_rgb, width, height).ok_or_else(|| {
            anyhow!(
                "YOLOv8-face: invalid RGB buffer ({} bytes for {}x{})",
                image_rgb.len(),
                width,
                height
            )
        })?;

        // YOLOv8 standard preprocessing: letterbox z gray (114), normalize /255.
        let (canvas, meta) = letterbox(&img, YOLO_INPUT_SIZE, [114, 114, 114]);
        let nchw = rgb_to_nchw_normalized(&canvas, [0.0, 0.0, 0.0], [255.0, 255.0, 255.0]);

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, YOLO_INPUT_SIZE as usize, YOLO_INPUT_SIZE as usize),
            nchw,
        )
        .context("YOLO: nchw shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("YOLO: tract forward failed")?;

        let out = outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("YOLO: brak output tensorow"))?;

        let shape = out.shape().to_vec();
        let data = out.as_slice::<f32>().context("YOLO: output nie jest f32")?;

        // YOLOv8/v11 zwraca (1, attrs, anchors) lub transponowane (1, anchors, attrs).
        // attrs zalezy od eksportu:
        //   - czysty detektor 1-klasowy ("face") -> 5  = 4 bbox + 1 score
        //   - wariant z keypointami (-pose, 5 punktow) -> 20 = 4 bbox + 1 score + 5*3
        // 8400 anchorow dla wejscia 640. Os z attrs to ta, ktora == 5 lub 20;
        // druga (wieksza) to liczba anchorow.
        let (attrs, anchors, transposed) = match shape.as_slice() {
            [1, a, b] if is_attrs(*a) => (*a, *b, false),
            [1, a, b] if is_attrs(*b) => (*b, *a, true),
            _ => return Err(anyhow!("YOLO: nieoczekiwany output shape {:?}", shape)),
        };
        let has_kps = attrs >= 20;

        let mut detections: Vec<FaceDetection> = Vec::with_capacity(64);

        for i in 0..anchors {
            // Indeksowanie zalezne od layoutu.
            let get = |attr: usize| -> f32 {
                if transposed {
                    data[i * attrs + attr]
                } else {
                    data[attr * anchors + i]
                }
            };

            let score = get(4);
            if score < SCORE_THRESHOLD {
                continue;
            }

            let cx = get(0);
            let cy = get(1);
            let w = get(2);
            let h = get(3);

            let (x1, y1) = unletterbox_xy(cx - w * 0.5, cy - h * 0.5, &meta);
            let (x2, y2) = unletterbox_xy(cx + w * 0.5, cy + h * 0.5, &meta);

            // Keypointy tylko gdy eksport je niesie (5 punktow * (x, y, visibility);
            // visibility ignorujemy). Inaczej zwracamy sam bbox.
            let keypoints = has_kps.then(|| {
                let mut kps = [(0f32, 0f32); 5];
                for (k, kp) in kps.iter_mut().enumerate() {
                    let base = 5 + k * 3;
                    *kp = unletterbox_xy(get(base), get(base + 1), &meta);
                }
                kps
            });

            detections.push(FaceDetection {
                bbox: (x1, y1, x2, y2),
                score,
                keypoints,
            });
        }

        Ok(nms(detections, NMS_IOU_THRESHOLD))
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
        .with_context(|| format!("tract: YOLOv8-face ONNX z {}", model_path.display()))?
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
