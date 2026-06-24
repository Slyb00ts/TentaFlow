// =============================================================================
// File: vision/camera_cv_models.rs — camera-CV model bundle download (Acme PoC)
// =============================================================================
//
// The always-on camera analysis pipeline (RF-DETR detector + state classifier +
// plate OCR) is exposed as three installable `vision` services so the models are
// installable from the catalog and visible to other mesh nodes — like any other
// engine. The runners in `vision/{detector_rfdetr,classifier_stan,ocr_plate}.rs`
// load lazily from `vision_models_dir()`, so the only deploy-time work is making
// sure the model files land there.
//
// Each bundle mixes two kinds of files:
//   * binary weights (`*.onnx`, ONNX external `*.onnx.data`) — large, downloaded
//     from the release URL declared in the manifest's `[[model_preset]] repo`,
//   * sidecar configs (`*-classes.json`, `*-config.json`) — tiny and stable, so
//     they are embedded in the binary and written out verbatim. This keeps the
//     pipeline working even when the release server only hosts the weights.
//
// The download base comes from the manifest (mesh-propagated), so pointing the
// install at a different release server is a manifest edit, not a code change.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

use crate::paths::vision_models_dir;
use crate::services::deploy::LogSink;
use crate::services::model_download::{download_with_progress, ProgressFn};

/// One file in a camera-CV bundle. `remote` set → fetched from `<base>/<name>`;
/// `embedded` set → written from the binary-embedded bytes (config sidecars).
struct CvFile {
    name: &'static str,
    remote: bool,
    embedded: Option<&'static str>,
}

struct CvBundle {
    engine_id: &'static str,
    files: &'static [CvFile],
}

const RFDETR_CLASSES: &str = include_str!("cv_assets/rfdetr-classes.json");
const STAN_CLASSES: &str = include_str!("cv_assets/stan-classes.json");
const PLATE_CONFIG: &str = include_str!("cv_assets/plate-ocr-config.json");

const BUNDLES: &[CvBundle] = &[
    CvBundle {
        engine_id: "rfdetr-adr",
        files: &[
            CvFile {
                // Burn weights artifact (architecture is compiled in; only weights
                // are distributed). Host the .bpk at the release URL.
                name: "rfdetr-base.bpk",
                remote: true,
                embedded: None,
            },
            CvFile {
                name: "rfdetr-classes.json",
                remote: false,
                embedded: Some(RFDETR_CLASSES),
            },
        ],
    },
    CvBundle {
        engine_id: "nalepka-stan",
        files: &[
            CvFile {
                name: "model_stan.bpk",
                remote: true,
                embedded: None,
            },
            CvFile {
                name: "stan-classes.json",
                remote: false,
                embedded: Some(STAN_CLASSES),
            },
        ],
    },
    CvBundle {
        engine_id: "plate-ocr",
        files: &[
            CvFile {
                name: "plate_ocr.bpk",
                remote: true,
                embedded: None,
            },
            CvFile {
                name: "plate-ocr-config.json",
                remote: false,
                embedded: Some(PLATE_CONFIG),
            },
        ],
    },
    CvBundle {
        // Depth Anything V2 Metric (Burn). Architecture is compiled in; only the
        // `.bpk` weights are distributed. Deploying `depth-native` (embedded) provisions
        // this file into `vision_models_dir()` and registers the service for mesh
        // discovery. Host the .bpk at the release URL.
        engine_id: "depth-native",
        files: &[CvFile {
            name: "depth-anything-v2-metric.bpk",
            remote: true,
            embedded: None,
        }],
    },
];

/// True when `engine_id` is one of the camera-CV pipeline services. Lets the
/// vision deploy path route these away from the tract `LoadedEngine` registry.
pub fn is_camera_cv_engine(engine_id: &str) -> bool {
    BUNDLES.iter().any(|b| b.engine_id == engine_id)
}

/// PP-OCRv5 (onnx-ocr) bundle: `(nazwa, wymagany, opcjonalny_absolutny_url)`.
/// Gdy `abs_url` = `Some`, plik leci z tego URL-a zamiast `<base_url>/<name>` —
/// dict PP-OCRv5 trzymamy z kanonicznego repo PaddleOCR (modele det/rec hostuja
/// repo ONNX bez slownika). `cls` opcjonalny (404 nie przerywa deployu — silnik
/// pomija korekte kata), `det`/`rec`/`dict` wymagane.
const ONNX_OCR_FILES: &[(&str, bool, Option<&str>)] = &[
    ("ppocrv5_det.onnx", true, None),
    ("ppocrv5_rec.onnx", true, None),
    ("ppocrv5_cls.onnx", false, None),
    (
        "ppocrv5_dict.txt",
        true,
        Some("https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/main/ppocr/utils/dict/ppocrv5_dict.txt"),
    ),
];

/// Materializuje bundle PP-OCRv5 (onnx-ocr) do `vision_models_dir()`: pobiera
/// det/rec/cls/dict z `<base_url>/<file>`. `base_url` to manifestowy
/// `[[model_preset]] repo` (URL katalogu z plikami). Idempotentne — istniejace
/// pliki sa pomijane. Opcjonalny `cls` ktorego brak na serwerze (404) jest
/// tolerowany; brak wymaganego pliku to blad.
pub async fn ensure_onnx_ocr_bundle(base_url: &str, log_sink: Option<&LogSink>) -> Result<()> {
    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    let base = base_url.trim_end_matches('/');
    if base.is_empty() || !base.starts_with("http") {
        return Err(anyhow!(
            "onnx-ocr: brak URL bundla (manifest model_preset.repo musi byc URL-em http(s) do katalogu z plikami PP-OCRv5)"
        ));
    }

    for (name, required, abs_url) in ONNX_OCR_FILES {
        let dest = dir.join(name);
        if file_ok(&dest) {
            continue;
        }
        let url = match abs_url {
            Some(u) => (*u).to_string(),
            None => format!("{}/{}", base, name),
        };
        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, name.to_string()));

        match download_with_progress(&url, &dest, name, progress).await {
            Ok(_) => {
                if let Some(s) = log_sink {
                    s.info(&format!("onnx-ocr: {} pobrany", name));
                }
            }
            Err(e) => {
                if *required {
                    return Err(anyhow!("download {} from {}: {}", name, url, e));
                }
                if let Some(s) = log_sink {
                    s.info(&format!(
                        "onnx-ocr: opcjonalny {} niedostepny ({}) — pomijam korekte kata",
                        name, e
                    ));
                }
            }
        }
    }

    Ok(())
}

fn bundle(engine_id: &str) -> Option<&'static CvBundle> {
    BUNDLES.iter().find(|b| b.engine_id == engine_id)
}

fn file_ok(path: &PathBuf) -> bool {
    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}

fn progress_for_sink(sink: LogSink, label: String) -> ProgressFn {
    Box::new(move |downloaded: u64, total: u64, _label: &str| {
        let pct: u8 = if total > 0 {
            (((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)) as u8
        } else {
            0
        };
        let line = if total > 0 {
            format!("{}: {}/{} KB ({}%)", label, downloaded / 1024, total / 1024, pct)
        } else {
            format!("{}: {} KB", label, downloaded / 1024)
        };
        sink.progress("downloading-vision", pct, &line);
    })
}

/// Materializes the camera-CV bundle for `engine_id` into `vision_models_dir()`:
/// downloads the weights from `<base_url>/<file>` and writes the embedded config
/// sidecars. `base_url` is the manifest `[[model_preset]] repo` (a release-dir
/// URL). Idempotent — files already present on disk are left untouched.
pub async fn ensure_bundle(
    engine_id: &str,
    base_url: &str,
    log_sink: Option<&LogSink>,
) -> Result<()> {
    let bundle = bundle(engine_id)
        .ok_or_else(|| anyhow!("'{}' is not a camera-CV engine", engine_id))?;

    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    for f in bundle.files {
        let dest = dir.join(f.name);

        if let Some(contents) = f.embedded {
            // Configs are authoritative in-binary; rewrite so a stale on-disk
            // copy never shadows a runner-format change shipped with the build.
            std::fs::write(&dest, contents)
                .map_err(|e| anyhow!("write {}: {}", dest.display(), e))?;
            continue;
        }

        if !f.remote {
            continue;
        }

        if file_ok(&dest) {
            continue;
        }

        let base = base_url.trim_end_matches('/');
        if base.is_empty() {
            return Err(anyhow!(
                "camera-CV '{}': no release URL configured (manifest model_preset.repo is empty)",
                engine_id
            ));
        }
        let url = format!("{}/{}", base, f.name);

        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", f.name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, f.name.to_string()));

        download_with_progress(&url, &dest, f.name, progress)
            .await
            .map_err(|e| anyhow!("download {} from {}: {}", f.name, url, e))?;

        if let Some(s) = log_sink {
            s.info(&format!("vision: {} pobrany", f.name));
        }
    }

    Ok(())
}
