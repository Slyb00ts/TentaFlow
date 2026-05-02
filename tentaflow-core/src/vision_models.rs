// =============================================================================
// Plik: vision_models.rs
// Opis: Vision ONNX models (YOLOv8-face, SCRFD, HSEmotion, YOLOv8n-pose,
//       MoveNet-lightning). Po Etapie 12d-1 download odbywa się deploy-time
//       przez `ensure_for_kind`, z progress callback do `LogSink`. Funkcje
//       `*_path()` to zero-IO stat-checki — zwracają Some(path) gdy plik
//       istnieje, None gdy brak. Caller (deploy strategy) wywołuje
//       `ensure_for_kind` przed `vision::register_engine`.
// =============================================================================

use std::path::PathBuf;

use crate::paths::vision_models_dir;
use crate::services::deploy::LogSink;
use crate::services::model_download::{download_with_progress, ProgressFn};
use crate::vision::VisionEngineKind;

/// Plik istnieje i ma sensowny rozmiar (≥ 100 KB — odsiewa zaślepki HTTP-error).
fn file_ok(path: &PathBuf) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() >= 100 * 1024)
        .unwrap_or(false)
}

pub fn yolov8_face_path() -> Option<PathBuf> {
    let p = vision_models_dir().join("yolov8-face.onnx");
    file_ok(&p).then_some(p)
}

pub fn scrfd_path() -> Option<PathBuf> {
    let p = vision_models_dir().join("scrfd.onnx");
    file_ok(&p).then_some(p)
}

pub fn hsemotion_path() -> Option<PathBuf> {
    let p = vision_models_dir().join("hsemotion.onnx");
    file_ok(&p).then_some(p)
}

pub fn yolov8n_pose_path() -> Option<PathBuf> {
    let p = vision_models_dir().join("yolov8n-pose.onnx");
    file_ok(&p).then_some(p)
}

pub fn movenet_lightning_path() -> Option<PathBuf> {
    let p = vision_models_dir().join("movenet-lightning.onnx");
    file_ok(&p).then_some(p)
}

/// Mapuje kind do (filename, url) używanego do prostego pojedynczego
/// downloadu. Returns None dla SCRFD — zip extract idzie osobną ścieżką
/// (`ensure_scrfd_async`).
fn url_for_kind(kind: VisionEngineKind) -> Option<(&'static str, &'static str)> {
    match kind {
        VisionEngineKind::Yolov8Face => Some((
            "yolov8-face.onnx",
            "https://huggingface.co/AdamCodd/YOLOv11n-face-detection/resolve/main/model.onnx",
        )),
        VisionEngineKind::Scrfd => None,
        VisionEngineKind::Hsemotion => Some((
            "hsemotion.onnx",
            "https://github.com/HSE-asavchenko/face-emotion-recognition/raw/main/models/affectnet_emotions/onnx/enet_b0_8_best_afew.onnx",
        )),
        VisionEngineKind::Yolov8nPose => Some((
            "yolov8n-pose.onnx",
            "https://huggingface.co/Xenova/yolov8n-pose/resolve/main/onnx/model.onnx",
        )),
        VisionEngineKind::MovenetLightning => Some((
            "movenet-lightning.onnx",
            "https://huggingface.co/Xenova/movenet-singlepose-lightning/resolve/main/onnx/model.onnx",
        )),
    }
}

/// Buduje progress callback emitujący do `LogSink::progress` w fazie
/// `downloading-vision`. `LogSink: Clone` — closure capture przez move.
fn progress_for_sink(sink: LogSink, label: String) -> ProgressFn {
    Box::new(move |downloaded: u64, total: u64, _label: &str| {
        let pct: u8 = if total > 0 {
            (((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)) as u8
        } else {
            0
        };
        let line = if total > 0 {
            format!(
                "{}: {}/{} KB ({}%)",
                label,
                downloaded / 1024,
                total / 1024,
                pct
            )
        } else {
            format!("{}: {} KB", label, downloaded / 1024)
        };
        sink.progress("downloading-vision", pct, &line);
    })
}

/// Asynchronicznie pobiera ONNX dla danego silnika do `vision_models_dir()`.
/// Idempotentne (skip gdy plik już istnieje). Progress emitowany do
/// `log_sink` jako `kind="phase"` (start) oraz `kind="progress"` (chunki).
/// Zwraca Some(path) gdy plik gotowy, None gdy brak URL'a / download fail.
pub async fn ensure_for_kind(
    kind: VisionEngineKind,
    log_sink: Option<&LogSink>,
) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(vision_models_dir()) {
        if let Some(s) = log_sink {
            s.info(&format!(
                "vision: nie mogę utworzyć {}: {}",
                vision_models_dir().display(),
                e
            ));
        }
        return None;
    }

    // SCRFD ma osobny path — pobranie zip + ekstrakcja.
    if matches!(kind, VisionEngineKind::Scrfd) {
        return ensure_scrfd_async(log_sink).await;
    }

    // SCRFD jest obsłużony wyżej; wszystkie pozostałe silniki mają URL.
    let (filename, url) = url_for_kind(kind)
        .expect("url_for_kind should return Some for non-SCRFD vision engines");

    let dest = vision_models_dir().join(filename);
    if file_ok(&dest) {
        return Some(dest);
    }

    if let Some(s) = log_sink {
        s.phase("downloading-vision", &format!("Pobieram {}", filename));
    }

    let progress: Option<ProgressFn> = log_sink
        .cloned()
        .map(|sink| progress_for_sink(sink, filename.to_string()));

    match download_with_progress(url, &dest, filename, progress).await {
        Ok(_) => {
            if let Some(s) = log_sink {
                s.info(&format!("vision: {} pobrany", filename));
            }
            Some(dest)
        }
        Err(e) => {
            if let Some(s) = log_sink {
                s.info(&format!("vision: {} download failed: {}", filename, e));
            }
            None
        }
    }
}

/// SCRFD siedzi w `buffalo_s.zip` z InsightFace. Pobieramy zip z progress,
/// wyciągamy `det_*.onnx` jako `scrfd.onnx`, kasujemy zip.
async fn ensure_scrfd_async(log_sink: Option<&LogSink>) -> Option<PathBuf> {
    let dir = vision_models_dir();
    let target = dir.join("scrfd.onnx");
    if file_ok(&target) {
        return Some(target);
    }

    let zip_path = dir.join("buffalo_s.zip");
    let needs_download = !zip_path.exists();
    if needs_download {
        let zip_url =
            "https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_s.zip";
        if let Some(s) = log_sink {
            s.phase("downloading-vision", "Pobieram buffalo_s.zip (SCRFD)");
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, "buffalo_s.zip".to_string()));

        if let Err(e) = download_with_progress(zip_url, &zip_path, "buffalo_s.zip", progress).await
        {
            if let Some(s) = log_sink {
                s.info(&format!("scrfd: download buffalo_s.zip failed: {}", e));
            }
            return None;
        }
    }

    // Ekstrakcja synchroniczna — zip jest mały (≈30 MB) i szybko się rozpakuje.
    let zip_bytes = std::fs::read(&zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().to_string();
        if name.ends_with(".onnx") && name.contains("det_") {
            let mut out = std::fs::File::create(&target).ok()?;
            if std::io::copy(&mut entry, &mut out).is_err() {
                let _ = std::fs::remove_file(&target);
                return None;
            }
            let _ = std::fs::remove_file(&zip_path);
            if let Some(s) = log_sink {
                s.info(&format!("scrfd: wyekstrahowany do {}", target.display()));
            }
            return Some(target);
        }
    }
    if let Some(s) = log_sink {
        s.info("scrfd: brak det_*.onnx w buffalo_s.zip");
    }
    None
}
