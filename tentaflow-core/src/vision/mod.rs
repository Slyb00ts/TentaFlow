// =============================================================================
// Plik: vision/mod.rs
// Opis: Silniki vision (detekcja twarzy, wiek/plec, emocje) wkompilowane
//       w binarke. Backend: tract-onnx (pure Rust ONNX runtime), bez ABI hell
//       i bez Cargo features — kompiluje sie na linux/macos/windows/ios/android.
//
//       Kazdy silnik implementuje wlasciwy trait (FaceDetector / AgeGender /
//       EmotionClassifier). Dispatcher (`load`) bierze engine_id i sciezke
//       do ONNX (z `crate::vision_models::*_path()`) i zwraca Box<dyn>.
//
//       Inference: bezstanowe; sesja tract'a (TypedRunnableModel) trzymana
//       w `Arc` pod `OnceLock`-iem zeby lazy init i zero kosztu re-loadu
//       miedzy requestami.
// =============================================================================

pub mod nms;
pub mod preprocessing;

pub mod scrfd;
pub mod yolov8_face;
pub mod mivolo;
pub mod hsemotion;
pub mod emonet;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};
use parking_lot::RwLock;

/// Bbox + 5 keypoints + score. Wspolny typ dla wszystkich face detectorow.
#[derive(Debug, Clone)]
pub struct FaceDetection {
    /// (x1, y1, x2, y2) w pikselach orginalnego obrazka.
    pub bbox: (f32, f32, f32, f32),
    pub score: f32,
    /// 5 keypoints: lewe oko, prawe oko, nos, lewy kacik ust, prawy kacik ust.
    /// Format: [(x, y); 5] w pikselach orginalnego obrazka.
    /// `None` gdy detector nie ma keypoint head'a (np. golе YOLO bez face-kps).
    pub keypoints: Option<[(f32, f32); 5]>,
}

/// Wynik MiVOLO / GoogLeNet age+gender. `age` w latach (regresja),
/// `gender` jako prob mezczyzny [0..1] (1=mezczyzna, 0=kobieta).
#[derive(Debug, Clone)]
pub struct AgeGender {
    pub age_years: f32,
    pub gender_male_prob: f32,
}

/// Wynik HSEmotion / EmoNet. 8 emocji + valence/arousal (continuous).
#[derive(Debug, Clone)]
pub struct EmotionResult {
    pub label: String,
    pub probabilities: Vec<(String, f32)>,
    /// Valence/arousal — zwracane przez EmoNet (continuous affect space).
    /// HSEmotion nie ma tego wyjscia → None.
    pub valence: Option<f32>,
    pub arousal: Option<f32>,
}

/// Detector twarzy — wspolny trait dla SCRFD, YOLOv8-face.
pub trait FaceDetector: Send + Sync {
    /// `image_rgb` — pixele RGB w kolejnosci row-major (height * width * 3).
    /// `width`/`height` — wymiary `image_rgb`.
    fn detect(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<FaceDetection>>;
}

/// Klasyfikator wieku i plci — MiVOLO (placeholder GoogLeNet).
pub trait AgeGenderEngine: Send + Sync {
    /// `face_crop_rgb` — wycieta i wyrownana twarz (zalecany 224x224 RGB).
    fn predict(&self, face_crop_rgb: &[u8], width: u32, height: u32) -> Result<AgeGender>;
}

/// Klasyfikator emocji — HSEmotion / EmoNet.
pub trait EmotionClassifier: Send + Sync {
    fn classify(&self, face_crop_rgb: &[u8], width: u32, height: u32) -> Result<EmotionResult>;
}

/// Identyfikator silnika vision — odpowiada `engine.id` z manifestu TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionEngineKind {
    Yolov8Face,
    Scrfd,
    Mivolo,
    Hsemotion,
    Emonet,
}

impl VisionEngineKind {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "yolov8-face" => Some(Self::Yolov8Face),
            "scrfd" => Some(Self::Scrfd),
            "mivolo" => Some(Self::Mivolo),
            "hsemotion" => Some(Self::Hsemotion),
            "emonet" => Some(Self::Emonet),
            _ => None,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::Yolov8Face => "yolov8-face",
            Self::Scrfd => "scrfd",
            Self::Mivolo => "mivolo",
            Self::Hsemotion => "hsemotion",
            Self::Emonet => "emonet",
        }
    }
}

/// Tagged Arc — registry trzyma jednorodny typ a inference dispatcher
/// matchuje per-kind przy zapytaniu.
pub enum LoadedEngine {
    FaceDetector(Arc<dyn FaceDetector>),
    AgeGender(Arc<dyn AgeGenderEngine>),
    Emotion(Arc<dyn EmotionClassifier>),
    /// Stub — load() przeszedl walidacje ONNX'a ale silnik nie ma jeszcze
    /// pelnej implementacji `detect/predict/classify`. Zwracany dla
    /// EmoNet (brak ONNX) jako placeholder.
    Stub,
}

/// Globalny registry zaladowanych silnikow — `OnceLock` lazy init, `RwLock`
/// dla per-deploy upsert. Klucz: `service_name` (typowo `tentaflow-<engine>-<rand>`),
/// zeby kilka instancji tego samego enginu na jednym hoscie nie sie nadpisywalo.
static REGISTRY: OnceLock<RwLock<HashMap<String, LoadedEngine>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, LoadedEngine>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Wkłada zaladowany silnik do registry pod kluczem `service_name`. Wolany
/// przez deploy handler. Stary wpis (gdy redeploy) jest zastapiony.
pub fn register_engine(service_name: String, engine: LoadedEngine) {
    registry().write().insert(service_name, engine);
}

/// Usuwa silnik z registry (np. przy stop service / delete service).
pub fn unregister_engine(service_name: &str) -> bool {
    registry().write().remove(service_name).is_some()
}

/// Zwraca clone Arc-a do FaceDetectora pod `service_name`. Inne traity
/// (`AgeGender`, `Emotion`) maja wlasne gettery — Rust nie pozwala na
/// `dyn Trait` downcast, wiec rozdzielamy. None gdy klucz nie istnieje
/// albo jego LoadedEngine jest innego typu.
pub fn get_face_detector(service_name: &str) -> Option<Arc<dyn FaceDetector>> {
    let guard = registry().read();
    match guard.get(service_name)? {
        LoadedEngine::FaceDetector(d) => Some(Arc::clone(d)),
        _ => None,
    }
}

pub fn get_age_gender(service_name: &str) -> Option<Arc<dyn AgeGenderEngine>> {
    let guard = registry().read();
    match guard.get(service_name)? {
        LoadedEngine::AgeGender(e) => Some(Arc::clone(e)),
        _ => None,
    }
}

pub fn get_emotion(service_name: &str) -> Option<Arc<dyn EmotionClassifier>> {
    let guard = registry().read();
    match guard.get(service_name)? {
        LoadedEngine::Emotion(e) => Some(Arc::clone(e)),
        _ => None,
    }
}

/// Otwiera ONNX z `vision_models::*_path()` i zwraca `LoadedEngine` zdatny
/// do wpisania do registry. Wolany przez deploy handler runtime=embedded.
pub fn load_engine(kind: VisionEngineKind, model_path: &Path) -> Result<LoadedEngine> {
    match kind {
        VisionEngineKind::Scrfd => {
            let e = scrfd::load(model_path)?;
            Ok(LoadedEngine::FaceDetector(Arc::new(e)))
        }
        VisionEngineKind::Yolov8Face => {
            let e = yolov8_face::load(model_path)?;
            Ok(LoadedEngine::FaceDetector(Arc::new(e)))
        }
        VisionEngineKind::Mivolo => {
            let e = mivolo::load(model_path)?;
            Ok(LoadedEngine::AgeGender(Arc::new(e)))
        }
        VisionEngineKind::Hsemotion => {
            let e = hsemotion::load(model_path)?;
            Ok(LoadedEngine::Emotion(Arc::new(e)))
        }
        VisionEngineKind::Emonet => {
            // emonet::load() zwraca Err gdy ONNX brak — propagujemy to
            // bo bez modelu deploy nie powinien tworzyc serwisu.
            let _ = emonet::load(model_path)?;
            Ok(LoadedEngine::Stub)
        }
    }
}

/// Mapuje engine_id do sciezki ONNX'a wyekstrahowanego z embedded blob'a.
/// Zwraca None gdy build.rs nie embedowal pliku (pusty placeholder).
pub fn model_path_for(kind: VisionEngineKind) -> Option<std::path::PathBuf> {
    match kind {
        VisionEngineKind::Yolov8Face => crate::vision_models::yolov8_face_path(),
        VisionEngineKind::Scrfd => crate::vision_models::scrfd_path(),
        VisionEngineKind::Mivolo => crate::vision_models::mivolo_age_path(),
        VisionEngineKind::Hsemotion => crate::vision_models::hsemotion_path(),
        VisionEngineKind::Emonet => crate::vision_models::emonet_path(),
    }
}

/// Wynik inference w jednolitym formacie — uzywany przez dispatch handler,
/// niezalezne od ktorego silnika (FaceDetector / AgeGender / Emotion).
#[derive(Debug, Clone)]
pub enum InferOutput {
    Faces(Vec<FaceDetection>),
    AgeGender(AgeGender),
    Emotion(EmotionResult),
}

/// Inference dispatch — bierze service_name z registry i wola odpowiedni
/// trait na obrazku RGB. Bezstanowe; wszystkie silniki sa Send+Sync wiec
/// caller moze to wywolywac z dowolnego watka tokio bez extra synchronizacji.
pub fn infer(service_name: &str, image_rgb: &[u8], width: u32, height: u32) -> Result<InferOutput> {
    let guard = registry().read();
    let engine = guard
        .get(service_name)
        .ok_or_else(|| anyhow!("vision: brak zaladowanego silnika '{}'", service_name))?;
    match engine {
        LoadedEngine::FaceDetector(d) => Ok(InferOutput::Faces(d.detect(image_rgb, width, height)?)),
        LoadedEngine::AgeGender(e) => Ok(InferOutput::AgeGender(e.predict(image_rgb, width, height)?)),
        LoadedEngine::Emotion(e) => Ok(InferOutput::Emotion(e.classify(image_rgb, width, height)?)),
        LoadedEngine::Stub => Err(anyhow!(
            "vision: silnik '{}' jest stubem (brak modelu lub niewpiety inference)",
            service_name
        )),
    }
}
