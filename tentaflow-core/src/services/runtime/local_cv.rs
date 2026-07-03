// =============================================================================
// Plik: services/runtime/local_cv.rs
// Opis: Typy żądań i lokalny handler typed surface `CameraCv` — dispatch po
//       `engine_id` embedded backendu do procesowych singletonów CV (RF-DETR,
//       klasyfikator stanu, OCR tablic / in-process OCR runner).
// =============================================================================

use std::sync::Arc;

use tentaflow_protocol::{CameraCvResult, CvOcrMode};

/// Żądanie operacji CV dla `ModelRuntimeExecutor::execute_camera_cv`. `model`
/// to alias/nazwa serwisu z katalogu (resolve przez `ServiceSurface::CameraCv`);
/// klatki w `op` są współdzielone przez `Arc` — lokalny dispatch jest zero-copy,
/// kopiowanie do payloadu drutu następuje dopiero w gałęzi MeshForward.
#[derive(Debug, Clone)]
pub struct CameraCvRequest {
    pub model: String,
    pub op: CameraCvOpLocal,
}

/// Operacja CV w wariancie lokalnym — lustro `tentaflow_protocol::CameraCvOp`,
/// tylko z klatkami zero-copy zamiast `Vec<u8>`.
#[derive(Debug, Clone)]
pub enum CameraCvOpLocal {
    /// Detekcja obiektów na batchu klatek. `threshold` nadpisuje próg score
    /// detektora (`None` = domyślny próg modelu).
    Detect {
        frames: Vec<CvFrameLocal>,
        threshold: Option<f32>,
    },

    /// Klasyfikacja stanu na wyciętym fragmencie klatki (crop).
    ClassifyState { crop: CvFrameLocal },

    /// OCR na wyciętym fragmencie klatki (crop) w zadanym trybie.
    Ocr { crop: CvFrameLocal, mode: CvOcrMode },
}

/// Klatka RGB24 (row-major, 3 bajty/piksel, stride = `width * 3`) współdzielona
/// przez `Arc` — lokalny dispatch nie kopiuje pikseli.
#[derive(Debug, Clone)]
pub struct CvFrameLocal {
    pub data: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
}

/// Lokalny handler surface `CameraCv`: mapuje `engine_id` embedded backendu na
/// procesowy singleton modelu i wykonuje operację. Błędy jako `String` —
/// executor mapuje je na `ExecutorError::Internal`, żeby pętla failover mogła
/// próbować kolejnych kandydatów.
pub struct LocalCameraCvHandler;

impl LocalCameraCvHandler {
    pub async fn execute(engine_id: &str, op: CameraCvOpLocal) -> Result<CameraCvResult, String> {
        match engine_id {
            "rfdetr-adr" => match op {
                CameraCvOpLocal::Detect { frames, threshold } => {
                    detect_local(frames, threshold).await
                }
                _ => Err("silnik 'rfdetr-adr' obsługuje wyłącznie operację Detect".into()),
            },
            "nalepka-stan" => match op {
                CameraCvOpLocal::ClassifyState { crop } => classify_local(crop).await,
                _ => Err("silnik 'nalepka-stan' obsługuje wyłącznie operację ClassifyState".into()),
            },
            "plate-ocr" => match op {
                CameraCvOpLocal::Ocr { crop, mode } => ocr_local(crop, mode).await,
                _ => Err("silnik 'plate-ocr' obsługuje wyłącznie operację Ocr".into()),
            },
            // In-process OCR runnery (PP-OCRv5 / Apple Vision) rejestrowane przez
            // deploy — działają niezależnie od feature `inference-vision-gpu`.
            // Tryb OCR jest ignorowany: runner czyta tekst ogólnie.
            "onnx-ocr" | "apple-ocr" => match op {
                CameraCvOpLocal::Ocr { crop, .. } => ocr_runner_local(crop).await,
                _ => Err(format!(
                    "silnik '{}' obsługuje wyłącznie operację Ocr",
                    engine_id
                )),
            },
            other => Err(format!("nieznany silnik camera-cv: '{}'", other)),
        }
    }
}

/// Detekcja RF-DETR na batchu klatek. Używa TEGO SAMEGO procesowego singletonu
/// co zawsze-włączony silnik kamer (`vision_analysis::get_detector`), a forward
/// idzie przez `burn_backend::run_blocking` — pojedynczy wątek inferencji
/// gwarantuje jeden forward GPU naraz (równoległe forwardy = korupcja stanu).
#[cfg(feature = "inference-vision-gpu")]
async fn detect_local(
    frames: Vec<CvFrameLocal>,
    threshold: Option<f32>,
) -> Result<CameraCvResult, String> {
    use tentaflow_protocol::CvDetection;

    let detector = crate::services::camera_ingest::vision_analysis::get_detector()
        .await
        .ok_or_else(|| "detektor RF-DETR niedostępny (load nie powiódł się)".to_string())?;
    let batch = crate::vision::burn_backend::run_blocking(move || {
        let refs: Vec<(&[u8], u32, u32)> = frames
            .iter()
            .map(|f| (&f.data[..], f.width, f.height))
            .collect();
        let mut guard = detector.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .detect_batch(&refs, threshold)
            .map_err(|e| format!("detect_batch: {e:#}"))
    })
    .await
    .map_err(|e| format!("camera-cv detect executor: {e}"))??;
    let per_frame = batch
        .into_iter()
        .map(|dets| {
            dets.into_iter()
                .map(|d| CvDetection {
                    klasa: d.klasa,
                    bbox: d.bbox,
                    score: d.score,
                })
                .collect()
        })
        .collect();
    Ok(CameraCvResult::Detections { per_frame })
}

/// Klasyfikacja stanu na cropie — singleton `vision_analysis::get_classifier`,
/// forward przez `burn_backend::run_blocking` (jeden forward GPU naraz).
#[cfg(feature = "inference-vision-gpu")]
async fn classify_local(crop: CvFrameLocal) -> Result<CameraCvResult, String> {
    let classifier = crate::services::camera_ingest::vision_analysis::get_classifier()
        .await
        .ok_or_else(|| "klasyfikator stanu niedostępny (load nie powiódł się)".to_string())?;
    let stan = crate::vision::burn_backend::run_blocking(move || {
        let mut guard = classifier.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .classify(&crop.data, crop.width, crop.height)
            .map_err(|e| format!("classify: {e:#}"))
    })
    .await
    .map_err(|e| format!("camera-cv classify executor: {e}"))??;
    Ok(CameraCvResult::Labels { stan })
}

/// OCR tablic na cropie — singleton `vision_analysis::get_ocr`, forward przez
/// `burn_backend::run_blocking` (jeden forward GPU naraz). Tryb `Adr` używa
/// dedykowanej ścieżki `read_adr`; `Plate` i `Generic` idą przez `read`.
#[cfg(feature = "inference-vision-gpu")]
async fn ocr_local(crop: CvFrameLocal, mode: CvOcrMode) -> Result<CameraCvResult, String> {
    let ocr = crate::services::camera_ingest::vision_analysis::get_ocr()
        .await
        .ok_or_else(|| "OCR tablic niedostępny (load nie powiódł się)".to_string())?;
    let tekst = crate::vision::burn_backend::run_blocking(move || {
        let mut guard = ocr.lock().unwrap_or_else(|e| e.into_inner());
        match mode {
            CvOcrMode::Adr => guard.read_adr(&crop.data, crop.width, crop.height),
            CvOcrMode::Plate | CvOcrMode::Generic => {
                guard.read(&crop.data, crop.width, crop.height)
            }
        }
        .map_err(|e| format!("ocr: {e:#}"))
    })
    .await
    .map_err(|e| format!("camera-cv ocr executor: {e}"))??;
    Ok(CameraCvResult::Text { tekst })
}

#[cfg(not(feature = "inference-vision-gpu"))]
async fn detect_local(
    _frames: Vec<CvFrameLocal>,
    _threshold: Option<f32>,
) -> Result<CameraCvResult, String> {
    Err("camera-cv: detekcja wymaga feature 'inference-vision-gpu'".into())
}

#[cfg(not(feature = "inference-vision-gpu"))]
async fn classify_local(_crop: CvFrameLocal) -> Result<CameraCvResult, String> {
    Err("camera-cv: klasyfikacja stanu wymaga feature 'inference-vision-gpu'".into())
}

#[cfg(not(feature = "inference-vision-gpu"))]
async fn ocr_local(_crop: CvFrameLocal, _mode: CvOcrMode) -> Result<CameraCvResult, String> {
    Err("camera-cv: OCR tablic wymaga feature 'inference-vision-gpu'".into())
}

/// OCR przez nadpisany in-process runner (`vision::get_ocr_runner`, np.
/// PP-OCRv5 / Apple Vision). Runner nie jest silnikiem Burn/GPU — wystarczy
/// `spawn_blocking`, dokładnie jak w `vision_impl::try_override_ocr`.
async fn ocr_runner_local(crop: CvFrameLocal) -> Result<CameraCvResult, String> {
    let runner = crate::vision::get_ocr_runner()
        .ok_or_else(|| "in-process OCR runner nie jest zarejestrowany".to_string())?;
    let tekst =
        tokio::task::spawn_blocking(move || runner.read(&crop.data, crop.width, crop.height))
            .await
            .map_err(|e| format!("camera-cv ocr task: {e}"))?
            .map_err(|e| format!("ocr: {e:#}"))?;
    Ok(CameraCvResult::Text { tekst })
}
