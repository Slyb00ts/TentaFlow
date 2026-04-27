// =============================================================================
// Plik: vision/mivolo.rs
// Opis: Wiek+plec. Obecnie placeholder GoogLeNet (z onnx-models zoo) — dwa
//       osobne modele: `mivolo_age.onnx` (regresja wiekiem) i `mivolo_gender.onnx`
//       (klasyfikacja 2-klasowa). Przy dostarczeniu prawdziwego MiVOLO v2
//       podmieniamy na pojedynczy plik `mivolo.onnx` (multi-head).
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::{AgeGender, AgeGenderEngine};

/// GoogLeNet age+gender oczekuje 224x224 RGB (ImageNet preprocessing).
const INPUT_SIZE: u32 = 224;

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct MivoloEngine {
    #[allow(dead_code)]
    age_model: Arc<Runnable>,
    /// `gender_model` jest opcjonalny — gdy dostarczony zostanie pojedynczy
    /// plik MiVOLO v2 (multi-head), `age_model` zwraca obie wartosci, a
    /// `gender_model` bedzie `None`.
    #[allow(dead_code)]
    gender_model: Option<Arc<Runnable>>,
}

impl AgeGenderEngine for MivoloEngine {
    fn predict(
        &self,
        _face_crop_rgb: &[u8],
        _width: u32,
        _height: u32,
    ) -> Result<AgeGender> {
        Err(anyhow!(
            "vision mivolo: predict() jeszcze nie wpiety — patrz scrfd.rs jako referencja"
        ))
    }
}

/// Wolany dla `mivolo_age.onnx`. Drugi plik (`mivolo_gender.onnx`) ladujemy
/// dodatkowo, gdy istnieje obok — typowo deploy handler woła `load` raz dla
/// `mivolo` engine_id i sam ogarnia obie sciezki.
pub fn load(model_path: &Path) -> Result<MivoloEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "MiVOLO age ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    let age = build(model_path)?;

    // Gender ONNX siedzi obok jako `mivolo_gender.onnx`. Przy MiVOLO v2
    // multi-head bedzie tylko jeden plik — wtedy gender_model = None.
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
        .with_context(|| format!("tract: nie udalo sie wczytac MiVOLO ONNX z {}", path.display()))?
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
