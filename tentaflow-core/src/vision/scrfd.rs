// =============================================================================
// Plik: vision/scrfd.rs
// Opis: SCRFD (InsightFace) face detector. Pure Rust ONNX inference przez
//       tract-onnx. Model `det_500m.onnx` z buffalo_s.
//
//       Architektura SCRFD: 3 FPN strides (8 / 16 / 32), 2 anchory per pozycja,
//       3 heady (score / bbox / kps) — razem 9 output tensorow (eksport buffalo_s
//       daje je jako 2D `(anchors, C)` bez wymiaru batcha).
//       Dla input 640x640 liczba anchorow per stride:
//         stride 8  → 80*80*2 = 12800
//         stride 16 → 40*40*2 = 3200
//         stride 32 → 20*20*2 = 800
//       Razem 16800 candidate boxes per forward.
//
//       Decode (referencja: insightface/python/scrfd.py):
//         anchor_centers[i] = (sx*stride, sy*stride)        // top-left grid pos
//         bbox = distance2bbox(anchor_centers, bbox_preds * stride)
//             x1 = ac.x - dl, y1 = ac.y - dt
//             x2 = ac.x + dr, y2 = ac.y + db
//         kp[k] = ac + kp_pred[k] * stride  // 5 punktow (oczy, nos, kaciki ust)
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use tract_onnx::prelude::TValue;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::nms::nms;
use super::preprocessing::{letterbox, rgb_buf_to_image, rgb_to_nchw_scrfd, unletterbox_xy};
use super::{FaceDetection, FaceDetector};

const SCRFD_INPUT_SIZE: u32 = 640;
const SCORE_THRESHOLD: f32 = 0.5;
const NMS_IOU_THRESHOLD: f32 = 0.4;
const STRIDES: [u32; 3] = [8, 16, 32];
const NUM_ANCHORS: usize = 2;

type RunnableScrfd = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct ScrfdEngine {
    model: Arc<RunnableScrfd>,
}

impl ScrfdEngine {
    pub fn new(model_path: &Path) -> Result<Self> {
        let proto = patch_resize_to_static_scales(model_path)?;
        let model = tract_onnx::onnx()
            .model_for_proto_model(&proto)
            .with_context(|| format!("tract: SCRFD ONNX z {}", model_path.display()))?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec!(1, 3, SCRFD_INPUT_SIZE as i32, SCRFD_INPUT_SIZE as i32),
                ),
            )?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self {
            model: Arc::new(model),
        })
    }
}

/// Przepisuje wezly `Resize` modelu SCRFD na STALY upsampling 2x (scales
/// `[1,1,2,2]`), eliminujac dynamiczny rozmiar wyjscia.
///
/// `det_500m.onnx` (buffalo_s) ma wejscie `[1,3,?,?]`, a FPN-owe `Resize`
/// wyznaczaja rozmiar wyjscia z `sizes = Concat(Slice(Shape(wyzsza_mapa)), ...)`
/// (puste `scales`). tract poprawnie zwija ten podgraf na stala TYLKO w buildzie
/// release; w buildzie debug Resize ewaluuje dynamicznie i daje zly rozmiar
/// (`Resize_108: expected 1,16,40,40, got 1,16,20,20`), bo nie aplikuje skali 2x.
/// Sciezka FPN SCRFD zawsze podwaja rozdzielczosc (stride 32->16->8 przy stalym
/// wejsciu 640), wiec podmieniamy `sizes` na puste i ustawiamy `scales=[1,1,2,2]`
/// — wynik jest identyczny, a graf jest deterministyczny niezaleznie od profilu.
fn patch_resize_to_static_scales(model_path: &Path) -> Result<tract_onnx::pb::ModelProto> {
    use tract_onnx::pb::{tensor_proto::DataType, TensorProto};

    let mut proto = tract_onnx::onnx()
        .proto_model_for_path(model_path)
        .with_context(|| format!("tract: odczyt proto SCRFD z {}", model_path.display()))?;

    let graph = proto
        .graph
        .as_mut()
        .ok_or_else(|| anyhow!("SCRFD: model bez grafu"))?;

    const SCALES_NAME: &str = "tf_scrfd_resize_scales_2x";
    graph.initializer.push(TensorProto {
        dims: vec![4],
        data_type: DataType::Float as i32,
        float_data: vec![1.0, 1.0, 2.0, 2.0],
        name: SCALES_NAME.to_string(),
        ..Default::default()
    });

    for node in graph.node.iter_mut() {
        if node.op_type != "Resize" {
            continue;
        }
        // Resize ma wejscia [X, roi, scales, sizes]. Wymuszamy 4 sloty,
        // ustawiamy `scales` na nasz staly tensor i czyscimy `sizes`.
        while node.input.len() < 4 {
            node.input.push(String::new());
        }
        node.input[2] = SCALES_NAME.to_string();
        node.input[3] = String::new();
    }

    Ok(proto)
}

impl FaceDetector for ScrfdEngine {
    fn detect(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<FaceDetection>> {
        let img = rgb_buf_to_image(image_rgb, width, height).ok_or_else(|| {
            anyhow!(
                "SCRFD: invalid RGB buffer ({} bytes for {}x{})",
                image_rgb.len(),
                width,
                height
            )
        })?;

        let (canvas, meta) = letterbox(&img, SCRFD_INPUT_SIZE, [0, 0, 0]);
        let nchw = rgb_to_nchw_scrfd(&canvas);

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, SCRFD_INPUT_SIZE as usize, SCRFD_INPUT_SIZE as usize),
            nchw,
        )
        .context("SCRFD: nchw shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("SCRFD: tract forward failed")?;

        // Sortujemy 9 wyjsc po heurystyce: ostatnia oska to glowa (1=score, 4=bbox,
        // 10=kps), przedostatnia to liczba anchorow (12800/3200/800 dla 640x640).
        // buffalo_s `det_500m.onnx` eksportuje wyjscia jako 2D (N, C) — bez wymiaru
        // batcha — wiec obslugujemy zarowno (N, C) jak i (1, N, C).
        let mut buckets: Vec<TensorBucket> = Vec::with_capacity(9);
        for t in outputs.iter() {
            let shape = t.shape();
            let (n, c) = match shape.len() {
                2 => (shape[0], shape[1]),
                3 => (shape[1], shape[2]),
                _ => continue,
            };
            let head = match c {
                1 => Head::Score,
                4 => Head::Bbox,
                10 => Head::Kps,
                _ => continue,
            };
            buckets.push(TensorBucket {
                head,
                anchors: n,
                tensor: t.clone(),
            });
        }

        // Per stride zbieramy trzy gleby (score, bbox, kps) — match po anchors count.
        let mut detections: Vec<FaceDetection> = Vec::new();
        for &stride in &STRIDES {
            let feat = (SCRFD_INPUT_SIZE / stride) as usize;
            let expected = feat * feat * NUM_ANCHORS;
            let score = match find_bucket(&buckets, Head::Score, expected) {
                Some(b) => b,
                None => {
                    tracing::warn!(
                        "SCRFD: brak score head dla stride {} (expected {} anchors)",
                        stride,
                        expected
                    );
                    continue;
                }
            };
            let bbox = match find_bucket(&buckets, Head::Bbox, expected) {
                Some(b) => b,
                None => continue,
            };
            let kps = find_bucket(&buckets, Head::Kps, expected);

            let scores = score
                .tensor
                .as_slice::<f32>()
                .context("SCRFD: score tensor nie jest f32")?;
            let bboxes = bbox
                .tensor
                .as_slice::<f32>()
                .context("SCRFD: bbox tensor nie jest f32")?;
            let kps_slice = match kps {
                Some(k) => k.tensor.as_slice::<f32>().ok(),
                None => None,
            };

            decode_stride(
                stride,
                feat,
                scores,
                bboxes,
                kps_slice,
                &meta,
                &mut detections,
            );
        }

        let kept = nms(detections, NMS_IOU_THRESHOLD);
        Ok(kept)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Head {
    Score,
    Bbox,
    Kps,
}

struct TensorBucket {
    head: Head,
    anchors: usize,
    tensor: TValue,
}

fn find_bucket(buckets: &[TensorBucket], head: Head, anchors: usize) -> Option<&TensorBucket> {
    buckets
        .iter()
        .find(|b| b.head == head && b.anchors == anchors)
}

/// Decode jednego stride'a. `scores` (anchors,1), `bboxes` (anchors,4),
/// `kps` (anchors,10) opcjonalne. Anchor center dla pozycji (sy, sx, a):
/// `(sx * stride, sy * stride)` — top-left grid (NIE pixel center) zgodnie
/// z `insightface/python/scrfd.py::generate_anchors_centers`.
fn decode_stride(
    stride: u32,
    feat: usize,
    scores: &[f32],
    bboxes: &[f32],
    kps: Option<&[f32]>,
    meta: &super::preprocessing::LetterboxMeta,
    out: &mut Vec<FaceDetection>,
) {
    let stride_f = stride as f32;
    let mut idx = 0usize;
    for sy in 0..feat {
        for sx in 0..feat {
            for _a in 0..NUM_ANCHORS {
                let s = scores[idx];
                if s >= SCORE_THRESHOLD {
                    let cx = sx as f32 * stride_f;
                    let cy = sy as f32 * stride_f;

                    let bb = idx * 4;
                    let dl = bboxes[bb] * stride_f;
                    let dt = bboxes[bb + 1] * stride_f;
                    let dr = bboxes[bb + 2] * stride_f;
                    let db = bboxes[bb + 3] * stride_f;

                    // Bbox w pikselach letterbox'a (640x640). Unletterbox
                    // odwzorowuje do oryginalnego obrazka.
                    let (x1, y1) = unletterbox_xy(cx - dl, cy - dt, meta);
                    let (x2, y2) = unletterbox_xy(cx + dr, cy + db, meta);

                    let keypoints = kps.map(|k| {
                        let kb = idx * 10;
                        let mut pts = [(0f32, 0f32); 5];
                        for j in 0..5 {
                            let kx = cx + k[kb + j * 2] * stride_f;
                            let ky = cy + k[kb + j * 2 + 1] * stride_f;
                            pts[j] = unletterbox_xy(kx, ky, meta);
                        }
                        pts
                    });

                    out.push(FaceDetection {
                        bbox: (x1, y1, x2, y2),
                        score: s,
                        keypoints,
                    });
                }
                idx += 1;
            }
        }
    }
}

pub fn load(model_path: &Path) -> Result<ScrfdEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "SCRFD ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    ScrfdEngine::new(model_path)
}
