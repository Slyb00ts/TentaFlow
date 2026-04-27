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

use std::path::Path;

use anyhow::Result;

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

/// Otwiera ONNX z embedded resource extraction (`vision_models::*_path()`)
/// i zwraca pre-built tract `TypedRunnableModel`. Wolany przez deploy handler
/// przy rejestracji serwisu vision/* — silnik trzymany w `Arc` w
/// `service_manager::vision_engines` (do zaimplementowania).
pub fn load_engine(kind: VisionEngineKind, model_path: &Path) -> Result<()> {
    // Sygnatura tymczasowa — typ wracajacy bedzie `Box<dyn FaceDetector>` /
    // `Box<dyn AgeGenderEngine>` / `Box<dyn EmotionClassifier>` po
    // implementacji kazdego silnika. Zostawiamy Result<()> jako sentinel
    // ze model sie laduje + tract akceptuje pliki — pelna integracja
    // dispatchu inference w nastepnym kroku.
    match kind {
        VisionEngineKind::Scrfd => scrfd::load(model_path).map(|_| ()),
        VisionEngineKind::Yolov8Face => yolov8_face::load(model_path).map(|_| ()),
        VisionEngineKind::Mivolo => mivolo::load(model_path).map(|_| ()),
        VisionEngineKind::Hsemotion => hsemotion::load(model_path).map(|_| ()),
        VisionEngineKind::Emonet => emonet::load(model_path).map(|_| ()),
    }
}
