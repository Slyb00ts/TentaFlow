// =============================================================================
// Plik: stt/whisper.rs
// Opis: Adapter whisper-rs (whisper.cpp) dla transkrypcji mowy.
//       Implementuje trait SttEngine z wykorzystaniem crate whisper-rs.
// =============================================================================

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{
    SttEngine, SttModelInfo, TranscribeChunk, TranscribeParams, TranscribeResult, TranscribeSegment,
};

/// Zaladowany model Whisper ze wszystkimi zasobami
struct LoadedWhisperModel {
    ctx: WhisperContext,
    info: SttModelInfo,
}

// WhisperContext z whisper-rs operuje na wskaznikach C — oznaczamy recznie
unsafe impl Send for LoadedWhisperModel {}
unsafe impl Sync for LoadedWhisperModel {}

/// Adapter whisper.cpp — lokalna transkrypcja mowy.
/// Trzyma snapshot deploy_params z `load_model` zeby `transcribe()` moglo
/// uzyc ich jako baseline gdy `TranscribeParams` per-call nie ma wartosci
/// (np. brak `language` w request → fallback do `default_language` z
/// deploy params).
pub struct WhisperEngine {
    state: Arc<Mutex<Option<LoadedWhisperModel>>>,
    deploy_params: Arc<Mutex<super::WhisperDeployParams>>,
}

impl WhisperEngine {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            deploy_params: Arc::new(Mutex::new(super::WhisperDeployParams::default())),
        }
    }

    /// Liczba watkow do przetwarzania. Honoruje `deploy_params.n_threads`
    /// gdy jest, inaczej spada do `available_parallelism`.
    fn num_threads_with_default(default: Option<i32>) -> i32 {
        if let Some(n) = default {
            return n;
        }
        std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4)
    }

    /// Wykrywa typ modelu na podstawie rozmiaru pliku
    fn detect_model_type(size_bytes: u64) -> &'static str {
        match size_bytes {
            0..=100_000_000 => "tiny",
            100_000_001..=300_000_000 => "base",
            300_000_001..=800_000_000 => "small",
            800_000_001..=2_000_000_000 => "medium",
            2_000_000_001..=2_500_000_000 => "large-v3-turbo",
            _ => "large-v3",
        }
    }

    /// Buduje FullParams z TranscribeParams + deploy_params jako fallback
    /// dla niewypelnionych pol w request (language/translate/beam_size).
    /// `n_threads` ZAWSZE z deploy_params (jezeli jest), inaczej CPU detect.
    /// Beam search: gdy `default_beam_size > 1` w deploy_params, uzywamy
    /// `BeamSearch` zamiast `Greedy { best_of: 1 }`.
    fn build_full_params<'a>(
        params: &'a TranscribeParams,
        deploy: &'a super::WhisperDeployParams,
    ) -> FullParams<'a, 'a> {
        let beam = deploy.default_beam_size.unwrap_or(1);
        let strategy = if beam > 1 {
            SamplingStrategy::BeamSearch {
                beam_size: beam,
                patience: 1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut fp = FullParams::new(strategy);

        // Jezyk: per-request override > deploy default > None (auto-detect).
        if let Some(ref lang) = params.language {
            fp.set_language(Some(lang));
        } else if let Some(ref lang) = deploy.default_language {
            fp.set_language(Some(lang));
        } else {
            fp.set_language(None);
        }

        // Translate: per-request override > deploy default > false.
        let translate = if params.translate {
            true
        } else {
            deploy.default_translate.unwrap_or(false)
        };
        fp.set_translate(translate);

        fp.set_print_special(false);
        fp.set_print_progress(false);
        fp.set_print_realtime(false);
        fp.set_print_timestamps(false);
        fp.set_token_timestamps(params.word_timestamps);

        if let Some(ref prompt) = params.initial_prompt {
            fp.set_initial_prompt(prompt);
        }

        fp.set_temperature(params.temperature.unwrap_or(0.0));
        fp.set_no_speech_thold(params.no_speech_threshold.unwrap_or(0.6));
        fp.set_n_threads(Self::num_threads_with_default(deploy.n_threads));

        fp
    }

    /// Wykonuje transkrypcje synchronicznie (wywoływane w spawn_blocking)
    fn transcribe_sync(
        loaded: &LoadedWhisperModel,
        pcm: &[f32],
        params: &TranscribeParams,
        deploy: &super::WhisperDeployParams,
    ) -> Result<TranscribeResult> {
        let start = Instant::now();

        let mut state = loaded
            .ctx
            .create_state()
            .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc stanu Whisper: {}", e))?;

        let full_params = Self::build_full_params(params, deploy);

        state
            .full(full_params, pcm)
            .map_err(|e| anyhow::anyhow!("Blad transkrypcji Whisper: {}", e))?;

        let n_segments = state.full_n_segments();

        let mut segments = Vec::with_capacity(n_segments as usize);
        let mut full_text = String::new();

        for i in 0..n_segments {
            let seg = match state.get_segment(i) {
                Some(s) => s,
                None => continue,
            };

            let text = seg
                .to_str_lossy()
                .map_err(|e| anyhow::anyhow!("Blad odczytu tekstu segmentu {}: {}", i, e))?;

            // Jednostki whisper: centisekundy (10ms per tick)
            let start_sec = seg.start_timestamp() as f64 * 0.01;
            let end_sec = seg.end_timestamp() as f64 * 0.01;
            let no_speech_prob = seg.no_speech_probability();

            let trimmed = text.trim();
            if !full_text.is_empty() && !trimmed.is_empty() {
                full_text.push(' ');
            }
            full_text.push_str(trimmed);

            segments.push(TranscribeSegment {
                id: i as u32,
                start: start_sec,
                end: end_sec,
                text: trimmed.to_string(),
                no_speech_prob,
                avg_logprob: 0.0,
                compression_ratio: 0.0,
                tokens: Vec::new(),
            });
        }

        // Czas trwania audio na podstawie liczby probek (16kHz)
        let duration_seconds = pcm.len() as f64 / 16000.0;

        let elapsed = start.elapsed();
        debug!(
            "Transkrypcja zakonczona: {} segmentow, {:.2}s audio, {:.2}s przetwarzania",
            segments.len(),
            duration_seconds,
            elapsed.as_secs_f64(),
        );

        // Wykryty jezyk
        let language = params
            .language
            .clone()
            .unwrap_or_else(|| "auto".to_string());

        Ok(TranscribeResult {
            text: full_text,
            language,
            duration_seconds,
            segments,
        })
    }
}

#[async_trait]
impl SttEngine for WhisperEngine {
    fn backend_name(&self) -> &str {
        "whisper"
    }

    fn supported_formats(&self) -> Vec<String> {
        vec![
            "wav".to_string(),
            "mp3".to_string(),
            "ogg".to_string(),
            "flac".to_string(),
            "m4a".to_string(),
            "webm".to_string(),
        ]
    }

    async fn load_model(
        &self,
        model_path: &Path,
        device: Option<&str>,
        deploy_params: &super::WhisperDeployParams,
    ) -> Result<SttModelInfo> {
        let path = model_path.to_path_buf();
        let device_str = device.unwrap_or("cpu").to_string();
        // Zachowaj deploy_params zeby `transcribe()` moglo ich uzyc jako
        // baseline gdy `TranscribeParams` per-call nie ma wartosci
        // (default_language/default_translate/default_beam_size/n_threads).
        *self.deploy_params.lock().await = deploy_params.clone();

        info!(
            "Ladowanie modelu Whisper: {} (device={})",
            path.display(),
            device_str,
        );

        let loaded = tokio::task::spawn_blocking(move || {
            let mut ctx_params = WhisperContextParameters::default();

            // GPU: zawsze probujemy wlaczyc, niezaleznie od ustawienia aliasu.
            // whisper.cpp ignoruje use_gpu jesli build nie ma GPU backendu
            // (CPU fallback). Eksplicytne `device="cpu"` w aliasie wymusza CPU.
            if !device_str.eq_ignore_ascii_case("cpu") {
                ctx_params.use_gpu(true);
            }

            let ctx =
                WhisperContext::new_with_params(path.to_str().unwrap_or_default(), ctx_params)
                    .map_err(|e| {
                        anyhow::anyhow!("Nie udalo sie zaladowac modelu Whisper: {}", e)
                    })?;

            // Odczytaj rozmiar pliku
            let metadata = std::fs::metadata(&path)
                .context("Nie udalo sie odczytac metadanych pliku modelu")?;
            let size_bytes = metadata.len();

            let model_type = Self::detect_model_type(size_bytes);

            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            info!(
                "Model Whisper zaladowany: {} (typ={}, rozmiar={}MB)",
                name,
                model_type,
                size_bytes / (1024 * 1024),
            );

            let info = SttModelInfo {
                name,
                path: path.to_string_lossy().to_string(),
                size_bytes,
                model_type: model_type.to_string(),
                backend: "whisper".to_string(),
                loaded: true,
                device: device_str,
            };

            Ok::<LoadedWhisperModel, anyhow::Error>(LoadedWhisperModel { ctx, info })
        })
        .await
        .context("Blad w spawn_blocking podczas ladowania modelu Whisper")?
        .context("Nie udalo sie zaladowac modelu Whisper")?;

        let info = loaded.info.clone();
        *self.state.lock().await = Some(loaded);

        info!("Model Whisper gotowy: {}", info.name);
        Ok(info)
    }

    async fn unload_model(&self) -> Result<()> {
        let mut guard = self.state.lock().await;
        if guard.is_some() {
            let name = guard
                .as_ref()
                .map(|m| m.info.name.clone())
                .unwrap_or_default();
            *guard = None;
            info!("Model Whisper '{}' wyladowany z pamieci", name);
        } else {
            warn!("Proba wyladowania modelu Whisper gdy zaden nie jest zaladowany");
        }
        Ok(())
    }

    fn model_info(&self) -> Option<SttModelInfo> {
        self.state
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|m| m.info.clone()))
    }

    async fn transcribe(&self, params: TranscribeParams) -> Result<TranscribeResult> {
        {
            let guard = self.state.lock().await;
            if guard.is_none() {
                anyhow::bail!("Model Whisper nie jest zaladowany — wywolaj load_model() najpierw");
            }
        }

        // Dekodowanie audio do PCM f32 16kHz mono
        let pcm = super::audio::decode_to_pcm_f32(&params.audio_data)
            .context("Blad dekodowania audio do PCM")?;

        debug!(
            "Audio zdekodowane: {} probek ({:.2}s)",
            pcm.len(),
            pcm.len() as f64 / 16000.0
        );

        let state = self.state.clone();
        let deploy_params = self.deploy_params.lock().await.clone();

        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let guard = rt.block_on(state.lock());
            let loaded = guard
                .as_ref()
                .context("Model Whisper zostal wyladowany w trakcie transkrypcji")?;

            Self::transcribe_sync(loaded, &pcm, &params, &deploy_params)
        })
        .await
        .context("Blad w spawn_blocking podczas transkrypcji")?
    }

    async fn transcribe_stream(
        &self,
        params: TranscribeParams,
    ) -> Result<mpsc::Receiver<TranscribeChunk>> {
        {
            let guard = self.state.lock().await;
            if guard.is_none() {
                anyhow::bail!("Model Whisper nie jest zaladowany — wywolaj load_model() najpierw");
            }
        }

        let pcm = super::audio::decode_to_pcm_f32(&params.audio_data)
            .context("Blad dekodowania audio do PCM")?;

        let (tx, rx) = mpsc::channel::<TranscribeChunk>(64);
        let state = self.state.clone();
        let deploy_params = self.deploy_params.lock().await.clone();

        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            let guard = rt.block_on(state.lock());
            let loaded = match guard.as_ref() {
                Some(m) => m,
                None => {
                    warn!("Model Whisper wyladowany przed rozpoczeciem streamingu");
                    return;
                }
            };

            // Transkrypcja calosciowa, nastepnie wysylka segmentow jako chunki
            match Self::transcribe_sync(loaded, &pcm, &params, &deploy_params) {
                Ok(result) => {
                    for segment in &result.segments {
                        let chunk = TranscribeChunk {
                            text: segment.text.clone(),
                            is_final: false,
                            segment: Some(segment.clone()),
                        };
                        if tx.blocking_send(chunk).is_err() {
                            return;
                        }
                    }
                    // Koncowy chunk
                    let _ = tx.blocking_send(TranscribeChunk {
                        text: String::new(),
                        is_final: true,
                        segment: None,
                    });
                }
                Err(e) => {
                    warn!("Blad podczas transkrypcji w trybie stream: {}", e);
                }
            }
        });

        Ok(rx)
    }
}
