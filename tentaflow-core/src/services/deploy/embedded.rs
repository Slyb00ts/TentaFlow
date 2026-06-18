// ============ File: services/deploy/embedded.rs — embedded (in-process) deploy strategy ============
//
// `runtime = "embedded"` engines run inside the tentaflow binary (llama.cpp,
// MLX, whisper, sherpa-onnx, vision/*). There is no external process and no
// network endpoint; commit only writes the DB rows so that the rest of the
// system knows the engine exists.

use async_trait::async_trait;
use rusqlite::Transaction;
use std::path::{Path, PathBuf};

use super::{
    build_new_service, category_tag, host_os_supported, models_from_manifest, resolve_display_name,
    DeployError, DeployResult, DeployStrategy, LogSink, PreparedDeploy, RuntimeHandle,
};
use crate::services::manifest::{Category, ModelPreset, NativeRuntime, ServiceManifest};
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

/// Serializuje CIEZKIE ladowanie modeli embedded (LLM/STT/TTS) do pamieci.
/// Boot reloaduje kilka silnikow [detached] RoWNOLEGLE — bez tej bramki ich
/// peaki pamieci podczas ladowania (MLX alokuje bufory grafu, ~2x wag) sumuja
/// sie i przekraczaja limit aplikacji iOS -> OOM/jetsam kill, mimo ze
/// steady-state wszystkich modeli by sie zmiescil. Bramka (1 permit) wpuszcza
/// jeden load na raz: peak = jeden ladowany model + reszta w steady-state.
/// Pobranie modelu (siec/dysk) jest POZA bramka — serializujemy tylko load.
static EMBEDDED_LOAD_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

struct EmbeddedLlmSelection {
    model_name: String,
    repo: String,
    quantization: Option<String>,
    model_file: Option<String>,
}

pub struct EmbeddedDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    log_sink: Option<LogSink>,
    registered_vision_keys: Vec<String>,
}

impl EmbeddedDeploy {
    pub fn new(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        log_sink: Option<LogSink>,
    ) -> Self {
        Self {
            manifest,
            user_config,
            log_sink,
            registered_vision_keys: Vec::new(),
        }
    }

    async fn prepare_embedded_vision(&mut self) -> DeployResult<()> {
        if self.manifest.engine.category != Category::Vision {
            return Ok(());
        }

        let engine_id = self.manifest.engine.id.clone();

        // Camera-CV pipeline models (RF-DETR detector / state classifier / plate
        // OCR) are ort-based singletons loaded lazily by the always-on analysis
        // engine, not tract `LoadedEngine`s. Deploy only fetches the bundle into
        // `vision_models_dir()`; the runner loads itself on first camera tick.
        if crate::vision::camera_cv_models::is_camera_cv_engine(&engine_id) {
            let base_url = self
                .manifest
                .model_presets
                .iter()
                .map(|p| p.repo.clone())
                .find(|r| r.starts_with("http"))
                .unwrap_or_default();
            crate::vision::camera_cv_models::ensure_bundle(
                &engine_id,
                &base_url,
                self.log_sink.as_ref(),
            )
            .await
            .map_err(|e| DeployError::Other(format!("camera-CV bundle '{}': {:#}", engine_id, e)))?;
            if let Some(s) = &self.log_sink {
                s.info(&format!("[vision] camera-CV bundle ready for {}", engine_id));
            }
            return Ok(());
        }

        let kind = crate::vision::VisionEngineKind::from_id(&engine_id).ok_or_else(|| {
            DeployError::Manifest(format!(
                "vision engine '{}' is not registered in runtime",
                engine_id
            ))
        })?;
        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[vision] preparing embedded model for {}",
                engine_id
            ));
        }

        // Pre-download ONNX (async, z progress do GUI) zanim załadujemy go z dysku.
        // `vision_models::*_path()` po Etapie 12d-1 jest pure stat-checkiem, więc
        // bez tego wywołania `model_path_for` zwróciłoby None.
        let model_path = crate::vision_models::ensure_for_kind(kind, self.log_sink.as_ref())
            .await
            .ok_or_else(|| {
                DeployError::Other(format!(
                    "vision model '{}' is not available (download failed or no URL)",
                    engine_id
                ))
            })?;

        let model_path_for_load = model_path.clone();
        let engine = tokio::task::spawn_blocking(move || {
            crate::vision::load_engine(kind, &model_path_for_load)
                .map_err(|e| DeployError::Other(format!("load vision model: {:#}", e)))
        })
        .await
        .map_err(|e| DeployError::Other(format!("vision prepare task: {}", e)))??;

        let mut keys = vec![self.manifest.engine.id.clone(), kind.id().to_string()];
        keys.extend(self.manifest.model_presets.iter().map(|p| p.id.clone()));
        keys.sort();
        keys.dedup();
        for key in &keys {
            crate::vision::register_engine(key.clone(), engine.clone());
        }
        self.registered_vision_keys = keys;
        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[vision] model loaded from {}",
                model_path.display()
            ));
        }
        Ok(())
    }

    async fn prepare_embedded_stt(&self) -> DeployResult<()> {
        if self.manifest.engine.category != Category::Stt {
            return Ok(());
        }

        // Embedded STT: whisper.cpp (engine.id = "whisper", plik ggml) lub
        // mlx-whisper (engine.id = "mlx-whisper", katalog MLX safetensors).
        // Model trafia do `shared_stt_manager()`, tego samego singletonu z
        // ktorego czyta `SttRuntime::transcribe`. Bez tego kroku usluga jest
        // oznaczona `running`, ale `active_engine()` zostaje None i kazda
        // transkrypcja konczy sie "no STT engine loaded".
        let engine_id = self.manifest.engine.id.clone();
        let model_repo = self.selected_model_repo();
        if let Some(s) = &self.log_sink {
            s.phase(
                "load-model",
                &format!("[stt] loading embedded {engine_id} ({model_repo})"),
            );
        }
        // Serializuj load wzgledem innych embedded (peak pamieci) — patrz
        // EMBEDDED_LOAD_GATE. Permit trzymany do konca funkcji (przez load).
        let _load_gate = EMBEDDED_LOAD_GATE
            .acquire()
            .await
            .expect("EMBEDDED_LOAD_GATE never closed");
        // whisper.cpp jest single-device: bierzemy pierwsza wybrana karte z
        // kreatora. Embedded nie reaguje na CUDA_VISIBLE_DEVICES, wiec indeks
        // karty trafia do silnika przez WhisperDeployParams -> WhisperLoadConfig.
        let mut deploy_params = crate::stt::WhisperDeployParams::default();
        let gpu_mode = self
            .user_config
            .get("gpu_select_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        if gpu_mode == "specific" {
            if let Some(&first) = parse_gpu_ids(&self.user_config).first() {
                deploy_params.gpu_device = first as i32;
            }
        }
        let shared = crate::stt::shared_stt_manager();
        let info = {
            let mut mgr = shared.write().await;
            mgr.ensure_and_load(
                Some(&engine_id),
                Some(&model_repo),
                None,
                self.log_sink.as_ref(),
                deploy_params,
            )
            .await
        }
        .map_err(|e| DeployError::Other(format!("load embedded STT '{engine_id}': {e}")))?;
        if let Some(s) = &self.log_sink {
            s.info(&format!("[stt] whisper model loaded from {}", info.path));
        }
        Ok(())
    }

    async fn prepare_embedded_tts(&self) -> DeployResult<()> {
        if self.manifest.engine.category != Category::Tts {
            return Ok(());
        }

        // Embedded TTS (sherpa-onnx VITS / kokoro / apple-tts). Silnik trafia
        // do `shared_tts_manager()` pod kluczem `engine.id` — tym samym, ktory
        // `execute_tts` przekazuje do `synthesize`. Bez tego kroku usluga jest
        // `running`, ale `synthesize` zwraca "TTS engine '...' nie
        // zarejestrowany".
        let engine_id = self.manifest.engine.id.clone();
        let model_repo = self.selected_model_repo();
        // Preset id (np. `vits-piper-pl_PL-jarvis_wg_glos-medium`) jako voice
        // hint — wielogłosowe repo musi zaladowac wlasciwy voice.
        let voice_hint = self
            .user_config
            .get("model_preset_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(s) = &self.log_sink {
            s.phase(
                "load-model",
                &format!("[tts] loading embedded {engine_id} ({model_repo})"),
            );
        }
        // Serializuj load wzgledem innych embedded (peak pamieci) — patrz
        // EMBEDDED_LOAD_GATE.
        let _load_gate = EMBEDDED_LOAD_GATE
            .acquire()
            .await
            .expect("EMBEDDED_LOAD_GATE never closed");
        crate::tts::ensure_embedded_engine_loaded(&engine_id, &model_repo, voice_hint)
            .await
            // {e:#} — pelny lancuch przyczyn anyhow (np. "dlopen ... nieudane:
            // Library not loaded @rpath/MLX.framework"), nie tylko zewnetrzny
            // context. Bez tego deploy-log pokazywal generyczne "load kokoro".
            .map_err(|e| DeployError::Other(format!("load embedded TTS '{engine_id}': {e:#}")))?;
        if let Some(s) = &self.log_sink {
            s.info(&format!("[tts] {engine_id} engine registered"));
        }
        Ok(())
    }

    /// HF repo (lub voice id dla apple-tts) dla embedded STT/TTS: jawny
    /// `model_repo` z configu ma priorytet, inaczej preset po
    /// `model_preset_id`, potem rekomendowany / pierwszy z manifestu.
    fn selected_model_repo(&self) -> String {
        if let Some(repo) = self
            .user_config
            .get("model_repo")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return repo.to_string();
        }
        self.user_config
            .get("model_preset_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|id| self.manifest.model_presets.iter().find(|p| p.id == id))
            .or_else(|| self.manifest.model_presets.iter().find(|p| p.recommended))
            .or_else(|| self.manifest.model_presets.first())
            .map(|p| p.repo.clone())
            .unwrap_or_default()
    }

    fn selected_llm_model(&self) -> Option<EmbeddedLlmSelection> {
        if self.manifest.engine.category != Category::Llm {
            return None;
        }

        if let Some(repo) = self
            .user_config
            .get("model_repo")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(EmbeddedLlmSelection {
                model_name: repo.to_string(),
                repo: repo.to_string(),
                quantization: None,
                model_file: self
                    .user_config
                    .get("model_file")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            });
        }

        let preset = self
            .user_config
            .get("model_preset_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|id| self.manifest.model_presets.iter().find(|p| p.id == id))
            .or_else(|| self.manifest.model_presets.iter().find(|p| p.recommended))
            .or_else(|| self.manifest.model_presets.first());

        preset.map(|p: &ModelPreset| EmbeddedLlmSelection {
            model_name: p.id.clone(),
            repo: p.repo.clone(),
            quantization: p.quantization.clone(),
            model_file: None,
        })
    }

    async fn prepare_embedded_llm(&self) -> DeployResult<Option<PathBuf>> {
        let Some(selection) = self.selected_llm_model() else {
            return Ok(None);
        };

        let preferred_backend = match self.manifest.engine.id.as_str() {
            "mlx" => "mlx",
            "llama-cpp" => "llamacpp",
            other => {
                return Err(DeployError::Manifest(format!(
                    "embedded LLM engine '{}' has no local inference backend mapping",
                    other
                )))
            }
        };

        // Test mode: cfg(test) returns a stub PathBuf without touching HF
        // download or the real backend load. Plumbing tests (DB writes,
        // transport selection, log sink) do not need a loaded model —
        // load_model requires a real .gguf/.safetensors file which unit
        // tests do not provide.
        #[cfg(test)]
        {
            let _ = (selection, preferred_backend);
            return Ok(Some(std::path::PathBuf::from(
                "/tmp/tentaflow-test-model.gguf",
            )));
        }
        #[cfg(not(test))]
        {
            // Persisted `model_path` to absolutna sciezka zapisana po pierwszym
            // deployu. Na iOS katalog Data aplikacji ma UUID rotowany przy
            // reinstalacji — stara absolutna sciezka wskazuje wtedy na nieistniejacy
            // kontener (objaw: "Sciezka nie istnieje" + load kod -1 przy boot-reload).
            // Gdy zapisana sciezka nie istnieje, ignorujemy ja i re-resolvujemy z
            // repo: ModelStore liczy katalog pod BIEZACYM kontenerem, a download jest
            // idempotentny (pomija jesli model juz pobrany). Tak samo zachowuje sie
            // embedded STT/TTS, ktore przezywaja rotacje kontenera.
            let persisted = self
                .user_config
                .get("model_path")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.exists());
            // Bypass downloadu dla LOKALNEGO GGUF (cykl FT: trenuj→eksportuj→
            // DEPLOY). Model FT po eksporcie nie żyje w repo HF — jego `.gguf`
            // leży na dysku pod absolutną ścieżką (`gguf_path` z eksportu, np.
            // /mnt/.../exports/<id>/model-q8_0.gguf). `model_file` niesie tu tę
            // ścieżkę WPROST, więc gdy jest absolutna pomijamy ModelStore i
            // ładujemy plik bez sieci. Relatywna ścieżka = nazwa pliku w repo HF
            // (istniejąca ścieżka download poniżej).
            let local_gguf = selection
                .model_file
                .as_deref()
                .map(Path::new)
                .filter(|p| p.is_absolute());
            if let Some(local) = local_gguf {
                if !local.exists() {
                    return Err(DeployError::Other(format!(
                        "lokalny model nie istnieje: {}",
                        local.display()
                    )));
                }
                // Backend mlx (Apple): `model_file` to KATALOG modelu HF safetensors
                // (eksport MLX). Backend llamacpp: pojedynczy plik `.gguf`.
                if preferred_backend == "mlx" {
                    let is_safetensors_dir = local.is_dir()
                        && (local.join("config.json").exists()
                            || std::fs::read_dir(local)
                                .map(|rd| {
                                    rd.filter_map(Result::ok).any(|e| {
                                        e.path()
                                            .extension()
                                            .and_then(|x| x.to_str())
                                            .map(|x| x.eq_ignore_ascii_case("safetensors"))
                                            .unwrap_or(false)
                                    })
                                })
                                .unwrap_or(false));
                    if !is_safetensors_dir {
                        return Err(DeployError::Other(format!(
                            "lokalna ścieżka modelu MLX nie jest katalogiem safetensors: {}",
                            local.display()
                        )));
                    }
                    if let Some(s) = &self.log_sink {
                        s.info(&format!("[model] using local MLX safetensors: {}", local.display()));
                    }
                } else {
                    if local
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| !e.eq_ignore_ascii_case("gguf"))
                        .unwrap_or(true)
                    {
                        return Err(DeployError::Other(format!(
                            "lokalna ścieżka modelu nie jest plikiem .gguf: {}",
                            local.display()
                        )));
                    }
                    if let Some(s) = &self.log_sink {
                        s.info(&format!("[model] using local GGUF: {}", local.display()));
                    }
                }
            }
            let model_path = if let Some(local) = local_gguf {
                local.to_path_buf()
            } else if let Some(path) = persisted {
                path
            } else {
                if selection.repo.starts_with("http://") || selection.repo.starts_with("https://") {
                    return Err(DeployError::Manifest(format!(
                        "embedded LLM repo '{}' must be a HuggingFace repo id or local model_path",
                        selection.repo
                    )));
                }

                if let Some(s) = &self.log_sink {
                    s.phase(
                        "download-model",
                        &format!("[model] downloading {}", selection.repo),
                    );
                }
                let store = crate::hub::model_store::ModelStore::default_for_platform();
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::channel::<crate::hub::model_store::DownloadProgress>(128);
                let progress_sink = self.log_sink.clone();
                let progress_task = tokio::spawn(async move {
                    while let Some(p) = progress_rx.recv().await {
                        if let Some(sink) = &progress_sink {
                            sink.progress(
                                "download-model",
                                p.percent.round().clamp(0.0, 100.0) as u8,
                                &format!(
                                    "[model] {} {:.1}% ({}/{})",
                                    p.file_name, p.percent, p.bytes_downloaded, p.bytes_total
                                ),
                            );
                        }
                    }
                });
                let download_selection = match preferred_backend {
                    "llamacpp" => {
                        if let Some(file) = selection.model_file.as_deref() {
                            if !crate::hub::model_store::valid_hf_relative_path(file) {
                                return Err(DeployError::Other(format!(
                                    "invalid GGUF filename '{}'",
                                    file
                                )));
                            }
                            crate::hub::model_store::ModelDownloadSelection::ExactFile(
                                file.to_string(),
                            )
                        } else if let Some(quantization) = selection.quantization.as_deref() {
                            crate::hub::model_store::ModelDownloadSelection::GgufQuantization(
                                quantization.to_string(),
                            )
                        } else {
                            return Err(DeployError::Other(
                                "llama.cpp GGUF deploy requires model_file or preset quantization"
                                    .to_string(),
                            ));
                        }
                    }
                    _ => crate::hub::model_store::ModelDownloadSelection::All,
                };
                let path = store
                    .download_model_selection(
                        &selection.repo,
                        None,
                        progress_tx,
                        download_selection,
                    )
                    .await
                    .map_err(|e| {
                        DeployError::Other(format!("download model {}: {}", selection.repo, e))
                    })?;
                let _ = progress_task.await;
                path
            };

            let load_path = match preferred_backend {
                "llamacpp" if model_path.is_dir() => {
                    find_gguf(&model_path, selection.quantization.as_deref()).ok_or_else(|| {
                        DeployError::Other(format!(
                            "no GGUF file found in downloaded model directory {}",
                            model_path.display()
                        ))
                    })?
                }
                _ => model_path.clone(),
            };

            if let Some(s) = &self.log_sink {
                s.phase(
                    "load-model",
                    &format!(
                        "[model] loading {} from {}",
                        selection.model_name,
                        load_path.display()
                    ),
                );
            }

            // Typed deploy params z manifest schema. apply_parameters_deploy
            // produkuje `app.llamacpp` / `app.mlx` mapy (load-time tunables
            // jak ctx_size/n_gpu_layers/threads/batch_size dla llama-cpp;
            // request-time defaults dla mlx). DeployTarget::NativeEmbedded
            // dla wszystkich embedded LLM/STT.
            let (mut app, _req_time) = super::apply_parameters_deploy(
                &self.manifest,
                &self.user_config,
                super::DeployTarget::NativeEmbedded,
            )
            .map_err(|e| DeployError::Manifest(format!("apply parameters: {}", e)))?;
            // _req_time intentionally dropped here — `prepare()` re-runs
            // apply_parameters_deploy gdy serializuje config_json zeby
            // request_time_parameters byly persystowane (snapshot_builder
            // potem czyta z config_json).

            // Wybór kart z kreatora (`gpu_select_mode`/`gpu_ids`) tlumaczymy na
            // `tensor_split`/`main_gpu`/`n_gpu_layers` w `app.llamacpp`. Embedded
            // llama.cpp dziala w jednym procesie core (CUDA init raz), wiec
            // CUDA_VISIBLE_DEVICES ustawiane po starcie nie dziala — zawezenie do
            // wybranych kart idzie tylko tymi parametrami ladowania do FFI.
            apply_gpu_selection_llamacpp(&self.user_config, &mut app.llamacpp);

            let deploy_params = crate::inference::DeployParamsSnapshot {
                llamacpp: app.llamacpp.clone(),
                mlx: app.mlx.clone(),
            };

            // Serializuj load wzgledem innych embedded (peak pamieci) — patrz
            // EMBEDDED_LOAD_GATE. Pobranie modelu juz za nami (poza bramka).
            let _load_gate = EMBEDDED_LOAD_GATE
                .acquire()
                .await
                .expect("EMBEDDED_LOAD_GATE never closed");
            let shared = crate::inference::shared_inference_manager();
            let mut manager = shared.write().await;
            let info = manager
                .load_model(&load_path, deploy_params, Some(preferred_backend))
                .await
                .map_err(|e| {
                    DeployError::Other(format!(
                        "load embedded model '{}' with backend '{}': {:#}",
                        load_path.display(),
                        preferred_backend,
                        e
                    ))
                })?;

            if let Some(s) = &self.log_sink {
                s.info(&format!(
                    "[model] loaded {} via {}",
                    info.name, preferred_backend
                ));
            }

            Ok(Some(load_path))
        }
    }
}

fn find_gguf(dir: &Path, quantization: Option<&str>) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    let needle = quantization.map(|q| q.to_ascii_lowercase());
    let mut first = None;
    while let Some(path) = stack.pop() {
        let entries = std::fs::read_dir(path).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
            {
                if first.is_none() {
                    first = Some(path.clone());
                }
                if let Some(ref needle) = needle {
                    let file_name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if file_name.contains(needle) {
                        return Some(path);
                    }
                }
            }
        }
    }
    first
}

#[async_trait]
impl DeployStrategy for EmbeddedDeploy {
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        let native = self.manifest.deploy.native.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' has no [deploy.native] section",
                self.manifest.engine.id
            ))
        })?;

        if native.runtime != NativeRuntime::Embedded {
            return Err(DeployError::Manifest(format!(
                "engine '{}' is not embedded (runtime={:?})",
                self.manifest.engine.id, native.runtime
            )));
        }

        if !host_os_supported(&native.platforms) {
            return Err(DeployError::Manifest(format!(
                "engine '{}' is not supported on the host OS",
                self.manifest.engine.id
            )));
        }

        // If the manifest declares a Cargo feature_flag for this embedded engine,
        // it MUST have been compiled in. We can't introspect cfg() from outside
        // its module, but we can fall back to a name-based registry of features
        // known to be optional. The conservative behaviour is: trust the build —
        // if the manifest is in the registry, the engine is available. Anything
        // gated by `target_os` already passed `host_os_supported`.
        // Future work (Phase 5+): plumb a feature-availability map from build.rs.

        self.prepare_embedded_vision().await?;
        self.prepare_embedded_stt().await?;
        self.prepare_embedded_tts().await?;
        let loaded_model_path = self.prepare_embedded_llm().await?;

        let runtime = RuntimeHandle::default();
        let models = models_from_manifest(&self.manifest, &self.user_config);
        let mut persisted_config = self.user_config.clone();
        if let (Some(path), Some(obj)) = (loaded_model_path, persisted_config.as_object_mut()) {
            obj.insert(
                "model_path".to_string(),
                serde_json::Value::String(path.to_string_lossy().to_string()),
            );
        }
        // Re-aplikuj typed parametry zeby request_time trafilo do config_json.
        // Dla embedded vision (Category::Vision) `parameters` jest puste i
        // RequestTimeParameters::default() — no-op. Embedded LLM/STT z
        // parametrami dorzucamy faktyczne wartosci.
        let request_time = super::apply_parameters_deploy(
            &self.manifest,
            &self.user_config,
            super::DeployTarget::NativeEmbedded,
        )
        .map(|(_, req)| req)
        .unwrap_or_default();
        let config_json = super::merge_config_json(&persisted_config, &request_time)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::NativeEmbedded,
            transport: Transport::Embedded,
            runtime,
            models,
            config_json,
            allocated_ports: Vec::new(),
        })
    }

    fn commit(
        &self,
        tx: &Transaction<'_>,
        service_id: i64,
        prepared: &PreparedDeploy,
    ) -> DeployResult<()> {
        let new = build_new_service(prepared, ServiceStatus::Running);
        Ok(services_repo::finish_deploy_in_tx(
            tx,
            service_id,
            &new,
            ServiceStatus::Running,
        )?)
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> DeployResult<()> {
        for key in &self.registered_vision_keys {
            crate::vision::unregister_engine(key);
        }
        Ok(())
    }
}

/// Odczytuje wybrane indeksy kart z `gpu_ids` (akceptuje liczby i stringi).
fn parse_gpu_ids(user_config: &serde_json::Value) -> Vec<usize> {
    user_config
        .get("gpu_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    v.as_u64()
                        .map(|n| n as usize)
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<usize>().ok()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Tlumaczy wybor kart z kreatora (`gpu_select_mode`/`gpu_ids`) na parametry
/// ladowania embedded llama.cpp. `none` => `n_gpu_layers=0` (tylko CPU);
/// `specific` z niepusta lista => `tensor_split` z waga 1.0 na wybranych kartach
/// (0.0 wyklucza pozostale) + `main_gpu` = pierwsza wybrana karta. `all` oraz
/// pusta lista nie zmieniaja niczego (domyslny rozklad na wszystkie karty).
fn apply_gpu_selection_llamacpp(
    user_config: &serde_json::Value,
    llamacpp: &mut std::collections::HashMap<String, serde_json::Value>,
) {
    let mode = user_config
        .get("gpu_select_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    match mode {
        "none" => {
            llamacpp.insert("n_gpu_layers".to_string(), serde_json::json!(0));
        }
        "specific" => {
            let ids = parse_gpu_ids(user_config);
            if let Some(&max_index) = ids.iter().max() {
                let mut split = vec![0.0_f32; max_index + 1];
                for &idx in &ids {
                    split[idx] = 1.0;
                }
                llamacpp.insert("tensor_split".to_string(), serde_json::json!(split));
                llamacpp.insert("main_gpu".to_string(), serde_json::json!(ids[0]));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::manifest::{
        ApiKind, Category, DeploySection, Engine, ModelPreset, NativeDeploy, TargetOs,
    };

    fn manifest(id: &str, runtime: NativeRuntime, platforms: Vec<TargetOs>) -> ServiceManifest {
        ServiceManifest {
            engine: Engine {
                id: id.into(),
                category: Category::Llm,
                name: id.into(),
                description_pl: "".into(),
                description_en: "".into(),
                homepage: "".into(),
                license: "".into(),
                icon: None,
                provider: None,
                resource_kind: None,
                requires_model: None,
                gpu_supported: None,
                default_port: 0,
                dgx_spark: None,
                api: ApiKind::OpenaiCompatible,
                version: "0".into(),
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            },
            deploy: DeploySection {
                docker: None,
                native: Some(NativeDeploy {
                    platforms,
                    runtime,
                    feature_flag: None,
                    binary_path: None,
                    bundle_path: None,
                }),
                external: None,
            },
            model_presets: vec![ModelPreset {
                id: "p1".into(),
                display_name: "Preset 1".into(),
                repo: "x".into(),
                quantization: None,
                recommended: true,
                featured: false,
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
                speculator_repo: None,
                speculator_method: None,
                speculator_num_tokens: None,
                vllm: None,
            }],
            parameters: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    #[tokio::test]
    async fn prepare_rejects_non_embedded_runtime() {
        let m = manifest(
            "binary-engine",
            NativeRuntime::Binary,
            vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
        );
        let mut s = EmbeddedDeploy::new(m, serde_json::json!({}), None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));
    }

    #[tokio::test]
    async fn prepare_rejects_unsupported_host_os() {
        // Build a platforms list that excludes the host OS.
        let host_excl = if cfg!(target_os = "linux") {
            vec![TargetOs::Macos, TargetOs::Windows]
        } else if cfg!(target_os = "macos") {
            vec![TargetOs::Linux, TargetOs::Windows]
        } else {
            vec![TargetOs::Linux, TargetOs::Macos]
        };
        let m = manifest("emb-foreign", NativeRuntime::Embedded, host_excl);
        let mut s = EmbeddedDeploy::new(m, serde_json::json!({}), None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));
    }

    #[tokio::test]
    async fn prepare_emits_models_for_embedded() {
        // engine.id must be known to prepare_embedded_llm — "llama-cpp" or "mlx".
        // Other ids return DeployError::Manifest("no local inference backend mapping").
        let m = manifest(
            "llama-cpp",
            NativeRuntime::Embedded,
            vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
        );
        let mut s = EmbeddedDeploy::new(m, serde_json::json!({}), None);
        let prepared = s.prepare().await.unwrap();
        assert_eq!(prepared.transport, Transport::Embedded);
        assert_eq!(prepared.models.len(), 1);
        assert!(prepared.models[0].is_default);
    }
}
