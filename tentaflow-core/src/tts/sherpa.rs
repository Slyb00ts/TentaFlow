// =============================================================================
// Plik: tts/sherpa.rs
// Opis: Adapter sherpa-onnx VITS TTS przez crate sherpa-rs. Wkompilowany w
//       binarke tentaflow przez Cargo feature `inference-sherpa`. Zaczyna
//       od konfiguracji VITS Piper (model + tokens + opcjonalny espeak-ng-
//       data); generate zwraca surowe sample float32 + sample rate.
// =============================================================================

use anyhow::{anyhow, Result};
use sherpa_rs::tts::{CommonTtsConfig, VitsTts, VitsTtsConfig};
use sherpa_rs::OnnxConfig;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{SynthesizeParams, SynthesizeResult, TtsEngine, TtsModelInfo};

/// Embedded TTS engine wokol sherpa-onnx VITS Piper. Loaduje model z
/// katalogu zawierajacego `<model>.onnx` + `tokens.txt` + opcjonalnie
/// `espeak-ng-data/` (wymagane dla wiekszosci VITS Piper voices).
pub struct SherpaTtsEngine {
    inner: Mutex<Option<VitsTts>>,
    model_info: Mutex<Option<TtsModelInfo>>,
}

impl Default for SherpaTtsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SherpaTtsEngine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            model_info: Mutex::new(None),
        }
    }
}

/// Znajduje pierwszy plik o danym suffix w katalogu (przyklad: `.onnx` /
/// `tokens.txt`). Zwraca pelna sciezke albo None.
fn find_file_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.ends_with(ext) {
                    return Some(path);
                }
            }
        }
    }
    None
}

impl TtsEngine for SherpaTtsEngine {
    fn backend_name(&self) -> &str {
        "sherpa-onnx"
    }

    fn load_model(&mut self, model_dir: &Path) -> Result<TtsModelInfo> {
        let model_path = find_file_with_ext(model_dir, ".onnx")
            .ok_or_else(|| anyhow!("brak pliku .onnx w {}", model_dir.display()))?;
        let tokens_path = model_dir.join("tokens.txt");
        if !tokens_path.exists() {
            anyhow::bail!("brak tokens.txt w {}", model_dir.display());
        }
        let espeak_dir = model_dir.join("espeak-ng-data");
        let data_dir_str = if espeak_dir.exists() {
            espeak_dir.to_string_lossy().into_owned()
        } else {
            String::new()
        };

        let config = VitsTtsConfig {
            model: model_path.to_string_lossy().into_owned(),
            tokens: tokens_path.to_string_lossy().into_owned(),
            data_dir: data_dir_str,
            length_scale: 1.0,
            noise_scale: 0.667,
            noise_scale_w: 0.8,
            silence_scale: 0.0,
            onnx_config: OnnxConfig {
                provider: "cpu".to_string(),
                num_threads: 2,
                debug: false,
                ..Default::default()
            },
            tts_config: CommonTtsConfig {
                max_num_sentences: 1,
                silence_scale: 0.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let tts = VitsTts::new(config);
        // Sample rate poznajemy po pierwszej syntezie — ustawiamy domyslny
        // VITS 22050 Hz; faktyczna wartosc dopowiada SynthesizeResult.
        let info = TtsModelInfo {
            name: model_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("vits")
                .to_string(),
            backend: "sherpa-onnx".to_string(),
            sample_rate: 22050,
            speakers: 1,
        };

        *self.inner.lock().unwrap() = Some(tts);
        *self.model_info.lock().unwrap() = Some(info.clone());
        Ok(info)
    }

    fn synthesize(&self, params: SynthesizeParams) -> Result<SynthesizeResult> {
        let mut guard = self.inner.lock().unwrap();
        let tts = guard.as_mut().ok_or_else(|| anyhow!("model not loaded"))?;
        let audio = tts
            .create(&params.text, params.speaker_id, params.speed)
            .map_err(|e| anyhow!("sherpa create: {e:?}"))?;
        Ok(SynthesizeResult {
            samples: audio.samples,
            sample_rate: audio.sample_rate,
        })
    }

    fn model_info(&self) -> Option<&TtsModelInfo> {
        // Mutex nie pozwala na safe & — caller dostaje clone przez load_model.
        // Zwracamy None zeby nie naruszac borrow rules; w praktyce caller
        // trzyma zwrocony info z load_model.
        None
    }
}
