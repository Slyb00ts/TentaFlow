// =============================================================================
// Plik: vision_models.rs
// Opis: Modele vision (YOLOv8-face, SCRFD, MiVOLO age+gender, HSEmotion, EmoNet)
//       sa embedded w binarce przez `tentaflow-core/build.rs::embed_vision_models`
//       (`include_bytes!` na plikach z `tentaflow-core/models/vision/` pobranych
//       wczesniej przez `scripts/setup.sh::download_vision_models`).
//
//       Runtime ekstraktor wypakowuje je przy pierwszym uruchomieniu do
//       `dirs::data_local_dir()/tentaflow/models/vision/`. Sciezki sa
//       cache'owane w globalnych `OnceLock` zeby kolejne wywolania byly O(1).
//
//       Idempotentne: jezeli plik istnieje + ma sensowny rozmiar (>= 100 KB)
//       i pasuje do wbudowanego blob.len(), skipujemy ekstrakcje.
//
//       Funkcje zwracaja `Option<PathBuf>` — None oznacza ze setup.sh nie
//       pobral modelu (build.rs wlozyl pusty placeholder). W runtime caller
//       loguje warn i zaznacza silnik jako wylaczony.
// =============================================================================

use std::path::PathBuf;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/vision_models_embed.rs"));

static YOLOV8_FACE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SCRFD_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static MIVOLO_AGE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static MIVOLO_GENDER_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static HSEMOTION_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static EMONET_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static YOLOV8N_POSE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static MOVENET_LIGHTNING_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn models_root() -> Option<PathBuf> {
    let base = crate::paths::models_root().join("vision");
    std::fs::create_dir_all(&base).ok();
    Some(base)
}

fn extract_blob(name: &str, blob: &[u8]) -> Option<PathBuf> {
    if blob.is_empty() {
        tracing::warn!(
            "vision_models: embedded {} jest pusty (setup.sh nie pobral) — silnik wylaczony",
            name
        );
        return None;
    }
    let dir = models_root()?;
    let path = dir.join(name);
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() as usize >= 100 * 1024 && meta.len() as usize == blob.len() {
            return Some(path);
        }
    }
    if let Err(e) = std::fs::write(&path, blob) {
        tracing::warn!("vision_models: zapis {} -> {:?}", name, e);
        return None;
    }
    tracing::info!(
        "vision_models: wyekstrahowano {} ({} KB) -> {}",
        name,
        blob.len() / 1024,
        path.display()
    );
    Some(path)
}

fn download_model_file(name: &str, url: &str) -> Option<PathBuf> {
    let dir = models_root()?;
    let path = dir.join(name);
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() >= 100 * 1024 {
            return Some(path);
        }
    }

    let tmp = path.with_extension("onnx.download");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .ok()?;
    let mut response = match client.get(url).send() {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!("vision_models: download {} failed: {}", name, e);
            return None;
        }
    };
    if !response.status().is_success() {
        tracing::warn!(
            "vision_models: download {} returned HTTP {}",
            name,
            response.status()
        );
        return None;
    }

    let mut file = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("vision_models: create {} failed: {}", tmp.display(), e);
            return None;
        }
    };
    if let Err(e) = std::io::copy(&mut response, &mut file) {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("vision_models: write {} failed: {}", tmp.display(), e);
        return None;
    }
    if let Ok(meta) = std::fs::metadata(&tmp) {
        if meta.len() < 100 * 1024 {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!("vision_models: downloaded {} is too small", name);
            return None;
        }
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("vision_models: move {} failed: {}", path.display(), e);
        return None;
    }
    tracing::info!("vision_models: downloaded {} -> {}", name, path.display());
    Some(path)
}

fn ensure_blob_or_download(name: &str, blob: &[u8], url: &str) -> Option<PathBuf> {
    match extract_blob(name, blob) {
        Some(path) => Some(path),
        None => download_model_file(name, url),
    }
}

/// Sciezka do yolov8-face.onnx (faktycznie YOLOv11n-face — patrz setup.sh).
pub fn yolov8_face_path() -> Option<PathBuf> {
    YOLOV8_FACE_PATH
        .get_or_init(|| extract_blob("yolov8-face.onnx", YOLOV8_FACE_ONNX))
        .clone()
}

/// Sciezka do scrfd.onnx (InsightFace SCRFD detector wyciagniety z buffalo_s.zip).
pub fn scrfd_path() -> Option<PathBuf> {
    SCRFD_PATH
        .get_or_init(|| extract_blob("scrfd.onnx", SCRFD_ONNX))
        .clone()
}

/// Sciezka do mivolo_age.onnx (obecnie GoogLeNet placeholder, MAE ~6).
pub fn mivolo_age_path() -> Option<PathBuf> {
    MIVOLO_AGE_PATH
        .get_or_init(|| extract_blob("mivolo_age.onnx", MIVOLO_AGE_ONNX))
        .clone()
}

/// Sciezka do mivolo_gender.onnx (obecnie GoogLeNet placeholder).
pub fn mivolo_gender_path() -> Option<PathBuf> {
    MIVOLO_GENDER_PATH
        .get_or_init(|| extract_blob("mivolo_gender.onnx", MIVOLO_GENDER_ONNX))
        .clone()
}

/// Sciezka do hsemotion.onnx (EfficientNet-B0 + AffectNet 8 emocji).
pub fn hsemotion_path() -> Option<PathBuf> {
    HSEMOTION_PATH
        .get_or_init(|| extract_blob("hsemotion.onnx", HSEMOTION_ONNX))
        .clone()
}

/// Sciezka do emonet.onnx (obecnie brak — wymaga eksportu z PyTorch).
pub fn emonet_path() -> Option<PathBuf> {
    EMONET_PATH
        .get_or_init(|| extract_blob("emonet.onnx", EMONET_ONNX))
        .clone()
}

pub fn yolov8n_pose_path() -> Option<PathBuf> {
    YOLOV8N_POSE_PATH
        .get_or_init(|| {
            ensure_blob_or_download(
                "yolov8n-pose.onnx",
                YOLOV8N_POSE_ONNX,
                "https://huggingface.co/Xenova/yolov8n-pose/resolve/main/onnx/model.onnx",
            )
        })
        .clone()
}

pub fn movenet_lightning_path() -> Option<PathBuf> {
    MOVENET_LIGHTNING_PATH
        .get_or_init(|| {
            ensure_blob_or_download(
                "movenet-lightning.onnx",
                MOVENET_LIGHTNING_ONNX,
                "https://huggingface.co/Xenova/movenet-singlepose-lightning/resolve/main/onnx/model.onnx",
            )
        })
        .clone()
}
