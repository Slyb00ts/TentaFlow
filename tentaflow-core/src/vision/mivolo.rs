// =============================================================================
// Plik: vision/mivolo.rs
// Opis: Wiek + plec. Obecnie placeholder GoogLeNet (Levi/Hassner z onnx-models
//       zoo) — dwa osobne modele:
//         - age_googlenet:    input 224x224 BGR, mean=[104,117,123], output
//                             logits[101] dla wieku (0..100). Estymata = sum(p*i).
//         - gender_googlenet: input 224x224 BGR, output logits[2] (M, F).
//
//       Po dostarczeniu prawdziwego MiVOLO v2 ONNX'a podmieniamy na pojedynczy
//       multi-head model — wtedy `gender_model` zostanie None i pelne wyniki
//       wyciagniemy z pojedynczego forward'a. Dispatch obsluguje obie sciezki.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use image::imageops::FilterType;
use tract_onnx::prelude::*;

use super::preprocessing::rgb_buf_to_image;
use super::{AgeGender, AgeGenderEngine};

const INPUT_SIZE: u32 = 224;
/// Mean BGR (Caffe style) — podany w opisie age_googlenet/gender_googlenet
/// w ONNX Model Zoo. Levi/Hassner trening z BGR ImageNet means.
const BGR_MEAN: [f32; 3] = [104.0, 117.0, 123.0];

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct MivoloEngine {
    age_model: Arc<Runnable>,
    gender_model: Option<Arc<Runnable>>,
}

impl AgeGenderEngine for MivoloEngine {
    fn predict(&self, face_crop_rgb: &[u8], width: u32, height: u32) -> Result<AgeGender> {
        let img = rgb_buf_to_image(face_crop_rgb, width, height)
            .ok_or_else(|| anyhow!("MiVOLO: invalid RGB buffer"))?;
        let resized = image::imageops::resize(&img, INPUT_SIZE, INPUT_SIZE, FilterType::Triangle);
        let nchw = rgb_to_bgr_nchw_caffe(&resized);

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize),
            nchw,
        )
        .context("MiVOLO: nchw shape mismatch")?
        .into();

        // AGE
        let age_outputs = self
            .age_model
            .run(tvec!(input.clone().into()))
            .context("MiVOLO age: tract forward failed")?;
        let age_logits = age_outputs[0]
            .as_slice::<f32>()
            .context("MiVOLO age: output nie jest f32")?;
        let age_years = expected_age_from_logits(age_logits);

        // GENDER (placeholder GoogLeNet — osobny model). Przy prawdziwym MiVOLO
        // multi-head bedzie None i pobierzemy gender z `age_outputs[1]`.
        let gender_male_prob = if let Some(ref gm) = self.gender_model {
            let g_outputs = gm
                .run(tvec!(input.into()))
                .context("MiVOLO gender: tract forward failed")?;
            let g_logits = g_outputs[0]
                .as_slice::<f32>()
                .context("MiVOLO gender: output nie jest f32")?;
            // ONNX zoo gender_googlenet: index 0 = M, index 1 = F.
            softmax2_p0(g_logits)
        } else if age_outputs.len() >= 2 {
            // Wariant multi-head MiVOLO v2 — drugi output to gender (M,F).
            let g = age_outputs[1]
                .as_slice::<f32>()
                .context("MiVOLO age multi-head: output[1] nie jest f32")?;
            softmax2_p0(g)
        } else {
            0.5 // brak danych — neutralne
        };

        Ok(AgeGender {
            age_years,
            gender_male_prob,
        })
    }
}

/// Konwertuje RGB image → BGR + Caffe-style mean subtract → NCHW f32.
fn rgb_to_bgr_nchw_caffe(img: &image::RgbImage) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let plane = (w * h) as usize;
    let mut buf = vec![0f32; plane * 3];
    for (i, p) in img.pixels().enumerate() {
        // BGR ordering w wyjsciowym buforze (kanal 0 = B, 1 = G, 2 = R).
        buf[i] = p[2] as f32 - BGR_MEAN[0]; // B
        buf[plane + i] = p[1] as f32 - BGR_MEAN[1]; // G
        buf[2 * plane + i] = p[0] as f32 - BGR_MEAN[2]; // R
    }
    buf
}

/// Estymata wieku jako wartosc oczekiwana po softmax: sum(p_i * i) dla i=0..100.
/// Wzor uzywany przez Levi/Hassner i wieksze prace age estimation. Daje plynna
/// estymate (np. 27.4) zamiast dyskretnej klasy.
fn expected_age_from_logits(logits: &[f32]) -> f32 {
    if logits.is_empty() {
        return 0.0;
    }
    // Softmax z stable trick (subtract max).
    let max_l = logits.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
    let mut sum_exp = 0.0f32;
    let mut exps = vec![0f32; logits.len()];
    for (i, &l) in logits.iter().enumerate() {
        let e = (l - max_l).exp();
        exps[i] = e;
        sum_exp += e;
    }
    let mut age = 0.0f32;
    for (i, e) in exps.iter().enumerate() {
        age += (i as f32) * (e / sum_exp);
    }
    age
}

/// Softmax 2-klasowy, zwraca prob klasy 0 (M w GoogLeNet zoo).
fn softmax2_p0(logits: &[f32]) -> f32 {
    if logits.len() < 2 {
        return 0.5;
    }
    let m = logits[0].max(logits[1]);
    let e0 = (logits[0] - m).exp();
    let e1 = (logits[1] - m).exp();
    e0 / (e0 + e1)
}

pub fn load(model_path: &Path) -> Result<MivoloEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "MiVOLO age ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    let age = build(model_path)?;
    let gender_path = model_path.with_file_name("mivolo_gender.onnx");
    let gender = if gender_path.exists() {
        Some(build(&gender_path)?)
    } else {
        None
    };
    Ok(MivoloEngine {
        age_model: Arc::new(age),
        gender_model: gender.map(Arc::new),
    })
}

fn build(path: &Path) -> Result<Runnable> {
    let model = tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("tract: MiVOLO/GoogLeNet ONNX z {}", path.display()))?
        .with_input_fact(
            0,
            InferenceFact::dt_shape(
                f32::datum_type(),
                tvec!(1, 3, INPUT_SIZE as i32, INPUT_SIZE as i32),
            ),
        )?
        .into_optimized()?
        .into_runnable()?;
    Ok(model)
}
