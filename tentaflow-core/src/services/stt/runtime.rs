// =============================================================================
// File: services/stt/runtime.rs — SttRuntime
//
// Single owner STT path (D.3). Handler `/v1/audio/transcriptions`, the flow
// STT adapter and `executor.execute_stt` all delegate through this module.
//
// Every backend here maps to a row in the `services` table: the supervisor
// registers one per local STT service (HTTP wrapper or the in-process
// embedded engine) and unregisters it when the row disappears. No model is
// ever loaded from this module — a missing backend is a typed error telling
// the user to deploy an STT engine.
//
// Dispatch:
//   * `transcribe_for_service(service_id, request)` — backend registered for
//     that service id, or `SttServiceUnavailable`.
//   * `transcribe(request)` — no service selection; picks the embedded
//     backend when it is registered and loaded, otherwise the lowest
//     registered HTTP backend, otherwise `SttServiceUnavailable`.
// =============================================================================

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::api::openai::types::{
    TranscriptionRequest, TranscriptionResponse, TranscriptionSegment,
};
use crate::error::{CoreError, Result};
use crate::services::backend::client::BackendClient;
use crate::stt::{SttManager, TranscribeParams};

use tracing::debug;

/// Backend of a single STT service row. `Embedded` = the in-process engine in
/// the shared `SttManager` singleton (loaded by the embedded deploy of that
/// row); `Http` = python-bundle / docker wrapper reached through
/// `BackendClient` (multipart `/v1/audio/transcriptions`).
pub enum SttBackend {
    Embedded,
    Http(Arc<BackendClient>),
}

/// Single owner STT dispatch with per-service backend registration.
pub struct SttRuntime {
    /// The global `shared_stt_manager()` singleton behind `SttBackend::Embedded`.
    embedded: Arc<RwLock<SttManager>>,
    /// service_id → backend, filled by supervisor reconcile. Ordered so the
    /// model-less `transcribe` path picks a deterministic HTTP backend.
    backends: RwLock<BTreeMap<i64, SttBackend>>,
}

/// Resolved backend for one request; `Embedded` carries no state because the
/// engine is read from the singleton under its own lock.
enum Selected {
    Embedded,
    Http(Arc<BackendClient>),
}

impl SttRuntime {
    pub fn new() -> Self {
        Self::with_manager(crate::stt::shared_stt_manager())
    }

    /// Runtime bound to an explicit manager (tests use a private manager so
    /// they never touch the process-wide singleton).
    pub fn with_manager(embedded: Arc<RwLock<SttManager>>) -> Self {
        Self {
            embedded,
            backends: RwLock::new(BTreeMap::new()),
        }
    }

    /// Registers the backend for `service_id`. Called by supervisor reconcile
    /// after a service row appears; overwrites an existing entry so a redeploy
    /// with a new configuration replaces the old client.
    pub async fn register_backend(&self, service_id: i64, backend: SttBackend) {
        self.backends.write().await.insert(service_id, backend);
    }

    /// Removes the backend for `service_id` (stop / delete).
    pub async fn unregister_backend(&self, service_id: i64) {
        self.backends.write().await.remove(&service_id);
    }

    async fn embedded_loaded(&self) -> bool {
        self.embedded
            .read()
            .await
            .active_engine()
            .map(|e| e.is_loaded())
            .unwrap_or(false)
    }

    fn no_service_error() -> anyhow::Error {
        CoreError::SttServiceUnavailable.into()
    }

    /// Backend for a model-less request: embedded when its row exists and the
    /// model is resident, else the first HTTP service.
    async fn select_default(&self) -> Result<Selected> {
        let backends = self.backends.read().await;
        let has_embedded = backends.values().any(|b| matches!(b, SttBackend::Embedded));
        if has_embedded && self.embedded_loaded().await {
            return Ok(Selected::Embedded);
        }
        backends
            .values()
            .find_map(|b| match b {
                SttBackend::Http(client) => Some(Selected::Http(client.clone())),
                SttBackend::Embedded => None,
            })
            .ok_or_else(Self::no_service_error)
    }

    async fn select_for_service(&self, service_id: i64) -> Result<Selected> {
        let backends = self.backends.read().await;
        match backends.get(&service_id) {
            Some(SttBackend::Http(client)) => Ok(Selected::Http(client.clone())),
            Some(SttBackend::Embedded) => Ok(Selected::Embedded),
            None => Err(Self::no_service_error()),
        }
    }

    /// Transcribes an audio file without service selection. An empty
    /// `request.file` is rejected here as well as in `routing/stt.rs` so the
    /// error surfaces as early as possible.
    pub async fn transcribe(&self, request: TranscriptionRequest) -> Result<TranscriptionResponse> {
        Self::reject_empty(&request)?;
        let selected = self.select_default().await?;
        self.run(selected, request).await
    }

    /// Transcribes through the backend registered for `service_id`.
    pub async fn transcribe_for_service(
        &self,
        service_id: i64,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse> {
        Self::reject_empty(&request)?;
        let selected = self.select_for_service(service_id).await?;
        self.run(selected, request).await
    }

    fn reject_empty(request: &TranscriptionRequest) -> Result<()> {
        if request.file.is_empty() {
            return Err(CoreError::InvalidRequest {
                message: "transcription file is empty (0 bytes)".to_string(),
                details: Some(
                    "Send a non-empty audio file in the multipart `file` field.".to_string(),
                ),
            }
            .into());
        }
        Ok(())
    }

    async fn run(
        &self,
        selected: Selected,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse> {
        match selected {
            Selected::Http(client) => client.audio_transcription(request).await.map_err(|e| {
                CoreError::InternalError {
                    message: format!("audio_transcription HTTP backend: {}", e),
                    source: None,
                }
                .into()
            }),
            Selected::Embedded => self.transcribe_embedded(request).await,
        }
    }

    async fn transcribe_embedded(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse> {
        // `language == None` means auto-detect: the resolver in
        // `api/openai/server.rs` already applied the user preference, so a
        // hardcoded default here would only cause cross-language hallucinations.
        let params = TranscribeParams {
            audio_data: Arc::clone(&request.file),
            language: request.language.clone(),
            translate: false,
            word_timestamps: request
                .timestamp_granularities
                .as_ref()
                .map(|g| g.iter().any(|s| s == "word"))
                .unwrap_or(false),
            temperature: request.temperature,
            no_speech_threshold: request.no_speech_threshold,
            initial_prompt: request.prompt.clone(),
        };

        let result = {
            let mgr = self.embedded.read().await;
            // The row exists but its model is not resident (stopped, or an
            // unpinned service not yet lazy-loaded by residency).
            // Deployed but not resident is a service-availability state, not
            // an internal fault: the caller should see 503 + retry, not 500.
            let engine = mgr
                .active_engine()
                .filter(|e| e.is_loaded())
                .ok_or(CoreError::SttServiceUnavailable)?;
            engine
                .transcribe(params)
                .await
                .map_err(|e| CoreError::InternalError {
                    message: format!("STT engine: {}", e),
                    source: None,
                })?
        };

        debug!(
            "STT runtime: {} segmentow, {:.2}s audio",
            result.segments.len(),
            result.duration_seconds
        );

        // Segment thresholds from the request (no_speech / avg_logprob /
        // compression_ratio) are applied here so `verbose_json` with
        // thresholds behaves the same as the HTTP wrappers.
        let filtered_segments: Vec<_> = result
            .segments
            .iter()
            .filter(|seg| {
                if let Some(thr) = request.no_speech_threshold {
                    if seg.no_speech_prob >= thr {
                        return false;
                    }
                }
                if let Some(thr) = request.avg_logprob_threshold {
                    if seg.avg_logprob < thr {
                        return false;
                    }
                }
                if let Some(thr) = request.compression_ratio_threshold {
                    if seg.compression_ratio > thr {
                        return false;
                    }
                }
                true
            })
            .collect();

        // Only verbose_json carries segments with timestamps.
        let segments = if request.response_format.as_deref() == Some("verbose_json") {
            Some(
                filtered_segments
                    .iter()
                    .map(|seg| TranscriptionSegment {
                        id: seg.id,
                        seek: 0,
                        start: seg.start as f32,
                        end: seg.end as f32,
                        text: seg.text.clone(),
                        tokens: seg.tokens.iter().map(|&t| t as u32).collect(),
                        temperature: 0.0,
                        avg_logprob: seg.avg_logprob,
                        compression_ratio: seg.compression_ratio,
                        no_speech_prob: seg.no_speech_prob,
                        speaker_label: None,
                        speaker_similarity: None,
                        is_known_speaker: None,
                    })
                    .collect(),
            )
        } else {
            None
        };

        // Rebuild the text only when filtering dropped a segment.
        let text = if filtered_segments.len() < result.segments.len() {
            filtered_segments
                .iter()
                .map(|seg| seg.text.as_str())
                .collect::<Vec<_>>()
                .join("")
        } else {
            result.text
        };

        Ok(TranscriptionResponse {
            text,
            task: Some("transcribe".to_string()),
            language: result.language,
            duration: Some(result.duration_seconds as f32),
            segments,
            speakers: None,
        })
    }
}

impl Default for SttRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai::types::SttRequestOptions;
    use crate::stt::{
        SttEngine, SttModelInfo, TranscribeChunk, TranscribeResult, WhisperDeployParams,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// In-memory STT engine: `load_model` flips `loaded`, `transcribe` echoes a
    /// fixed transcript so a test can prove the embedded path was taken.
    struct FakeEngine {
        loaded: AtomicBool,
    }

    #[async_trait::async_trait]
    impl SttEngine for FakeEngine {
        fn backend_name(&self) -> &str {
            "fake"
        }
        fn supported_formats(&self) -> Vec<String> {
            vec!["wav".into()]
        }
        async fn load_model(
            &self,
            model_path: &Path,
            _device: Option<&str>,
            _deploy_params: &WhisperDeployParams,
        ) -> anyhow::Result<SttModelInfo> {
            self.loaded.store(true, Ordering::SeqCst);
            Ok(self.info(model_path))
        }
        async fn unload_model(&self) -> anyhow::Result<()> {
            self.loaded.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn model_info(&self) -> Option<SttModelInfo> {
            self.loaded
                .load(Ordering::SeqCst)
                .then(|| self.info(Path::new("fake.bin")))
        }
        async fn transcribe(&self, _params: TranscribeParams) -> anyhow::Result<TranscribeResult> {
            Ok(TranscribeResult {
                text: "fake transcript".into(),
                language: Some("en".into()),
                duration_seconds: 1.0,
                segments: Vec::new(),
            })
        }
        async fn transcribe_stream(
            &self,
            _params: TranscribeParams,
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<TranscribeChunk>> {
            anyhow::bail!("not used")
        }
    }

    impl FakeEngine {
        fn info(&self, path: &Path) -> SttModelInfo {
            SttModelInfo {
                name: "fake".into(),
                path: path.to_string_lossy().into_owned(),
                size_bytes: 0,
                model_type: "fake".into(),
                backend: "fake".into(),
                loaded: self.loaded.load(Ordering::SeqCst),
                device: "cpu".into(),
            }
        }
    }

    fn private_runtime() -> (SttRuntime, Arc<RwLock<SttManager>>) {
        let mgr = Arc::new(RwLock::new(SttManager::with_engines(vec![Box::new(
            FakeEngine {
                loaded: AtomicBool::new(false),
            },
        )])));
        (SttRuntime::with_manager(mgr.clone()), mgr)
    }

    fn http_client_unreachable() -> Arc<BackendClient> {
        let backend = crate::config::ServiceBackend {
            connection: crate::config::ConnectionType::OpenAIApi {
                url: "http://127.0.0.1:1".into(),
                api_key: Some(String::new()),
                api_key_env: None,
                extra_headers: Vec::new(),
                custom_endpoint: None,
                request_format: None,
                tts_config: None,
            },
            max_concurrent: 1,
            timeout_ms: 1_000,
            weight: 1,
            model_name_override: None,
            health_check_path: None,
        };
        Arc::new(BackendClient::new(backend, None).expect("test backend client"))
    }

    fn empty_request() -> TranscriptionRequest {
        TranscriptionRequest {
            file: std::sync::Arc::from(Vec::<u8>::new().into_boxed_slice()),
            filename: "audio.wav".into(),
            model: String::new(),
            language: None,
            prompt: None,
            response_format: None,
            temperature: None,
            timestamp_granularities: None,
            no_speech_threshold: None,
            avg_logprob_threshold: None,
            compression_ratio_threshold: None,
            options: SttRequestOptions::default(),
        }
    }

    fn audio_request() -> TranscriptionRequest {
        let mut req = empty_request();
        req.file = std::sync::Arc::from(vec![0u8, 1, 2, 3].into_boxed_slice());
        req
    }

    fn downcast(err: anyhow::Error) -> CoreError {
        err.downcast().expect("CoreError downcast")
    }

    /// Empty file → typed `InvalidRequest` before any backend selection.
    #[tokio::test]
    async fn transcribe_rejects_empty_file_before_dispatch() {
        let (runtime, _) = private_runtime();
        let err = runtime
            .transcribe(empty_request())
            .await
            .expect_err("empty file must reject");
        assert!(matches!(downcast(err), CoreError::InvalidRequest { .. }));
    }

    /// No registered service row → `SttServiceUnavailable`, never a model load.
    #[tokio::test]
    async fn transcribe_without_backends_is_service_unavailable() {
        let (runtime, mgr) = private_runtime();
        let err = runtime
            .transcribe(audio_request())
            .await
            .expect_err("no STT service must error");
        assert!(matches!(downcast(err), CoreError::SttServiceUnavailable));
        assert!(
            mgr.read().await.active_engine().is_none(),
            "runtime must not load anything on its own"
        );
    }

    #[tokio::test]
    async fn transcribe_for_unknown_service_is_service_unavailable() {
        let (runtime, _) = private_runtime();
        let err = runtime
            .transcribe_for_service(7, audio_request())
            .await
            .expect_err("unregistered service must error");
        assert!(matches!(downcast(err), CoreError::SttServiceUnavailable));
    }

    /// Embedded row registered but the model is not resident (stopped /
    /// not yet lazy-loaded) → model-less dispatch has no usable backend.
    #[tokio::test]
    async fn embedded_registered_but_unloaded_is_service_unavailable() {
        let (runtime, _) = private_runtime();
        runtime.register_backend(3, SttBackend::Embedded).await;
        let err = runtime
            .transcribe(audio_request())
            .await
            .expect_err("unloaded embedded must not serve");
        assert!(matches!(downcast(err), CoreError::SttServiceUnavailable));
        // The per-service path reports the same availability error: the row is
        // deployed, the model simply is not resident, which is a 503 for the
        // caller and not an internal fault.
        let err = runtime
            .transcribe_for_service(3, audio_request())
            .await
            .expect_err("unloaded embedded must not serve");
        assert!(matches!(downcast(err), CoreError::SttServiceUnavailable));
    }

    /// Embedded row registered and loaded → the singleton engine serves both
    /// the model-less and the per-service path.
    #[tokio::test]
    async fn embedded_registered_and_loaded_uses_singleton() {
        let (runtime, mgr) = private_runtime();
        mgr.write()
            .await
            .load_model(
                Path::new("fake.bin"),
                None,
                Some("fake"),
                WhisperDeployParams::default(),
            )
            .await
            .expect("fake load");
        runtime.register_backend(3, SttBackend::Embedded).await;
        let resp = runtime.transcribe(audio_request()).await.expect("embedded");
        assert_eq!(resp.text, "fake transcript");
        let resp = runtime
            .transcribe_for_service(3, audio_request())
            .await
            .expect("embedded by id");
        assert_eq!(resp.text, "fake transcript");
    }

    /// HTTP row registered → the request goes to the HTTP client (the
    /// unreachable port proves which branch ran), never to the singleton.
    #[tokio::test]
    async fn http_registered_uses_http_client() {
        let (runtime, mgr) = private_runtime();
        runtime
            .register_backend(5, SttBackend::Http(http_client_unreachable()))
            .await;
        for result in [
            runtime.transcribe(audio_request()).await,
            runtime.transcribe_for_service(5, audio_request()).await,
        ] {
            match downcast(result.expect_err("unreachable HTTP backend must error")) {
                CoreError::InternalError { message, .. } => {
                    assert!(message.contains("HTTP backend"), "got: {message}")
                }
                other => panic!("expected InternalError, got {other:?}"),
            }
        }
        assert!(mgr.read().await.active_engine().is_none());
        runtime.unregister_backend(5).await;
        let err = runtime
            .transcribe(audio_request())
            .await
            .expect_err("unregistered → unavailable");
        assert!(matches!(downcast(err), CoreError::SttServiceUnavailable));
    }

    /// R2d (D.3): SttRequestOptions is opt-in (everything false by default).
    #[test]
    fn stt_request_options_default_is_opt_in() {
        let opts = SttRequestOptions::default();
        assert!(!opts.speaker_identification);
        assert!(!opts.diarization);
        assert!(opts.timestamps.is_none());
        assert!(opts.response_format.is_none());
    }

    /// `TranscriptionResponse.speakers` round-trips through JSON.
    #[test]
    fn transcription_response_speakers_field_round_trips() {
        use crate::api::openai::types::SpeakerSegment;
        let resp = TranscriptionResponse {
            text: "ahoj".into(),
            task: Some("transcribe".into()),
            language: Some("pl".into()),
            duration: Some(1.5),
            segments: None,
            speakers: Some(vec![SpeakerSegment {
                start: 0.0,
                end: 1.5,
                text: "ahoj".into(),
                speaker_label: "SPEAKER_00".into(),
                speaker_id: None,
                similarity: None,
            }]),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: TranscriptionResponse = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(parsed.text, "ahoj");
        assert_eq!(parsed.speakers.as_ref().map(|s| s.len()), Some(1));
        assert_eq!(
            parsed.speakers.as_ref().unwrap()[0].speaker_label,
            "SPEAKER_00"
        );
    }
}
