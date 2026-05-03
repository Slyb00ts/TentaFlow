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

struct EmbeddedLlmSelection {
    model_name: String,
    repo: String,
    quantization: Option<String>,
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
        let model_path =
            crate::vision_models::ensure_for_kind(kind, self.log_sink.as_ref())
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

        let model_path = if let Some(path) = self
            .user_config
            .get("model_path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            PathBuf::from(path)
        } else {
            if selection.repo.starts_with("http://") || selection.repo.starts_with("https://") {
                return Err(DeployError::Manifest(format!(
                    "embedded LLM repo '{}' must be a HuggingFace repo id or local model_path",
                    selection.repo
                )));
            }

            if let Some(s) = &self.log_sink {
                s.phase("download-model", &format!("[model] downloading {}", selection.repo));
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
            let path = store
                .download_model(&selection.repo, None, progress_tx)
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

        let shared = crate::inference::shared_inference_manager();
        let mut manager = shared.write().await;
        let info = manager
            .load_model(&load_path, None, Some(preferred_backend))
            .await
            .map_err(|e| {
                DeployError::Other(format!(
                    "load embedded model '{}' with backend '{}': {}",
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
        let config_json = serde_json::to_string(&persisted_config)
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

    fn commit(&self, tx: &Transaction<'_>, prepared: &PreparedDeploy) -> DeployResult<i64> {
        let new = build_new_service(prepared, ServiceStatus::Running);
        let id = services_repo::insert_in_tx(tx, &new)?;
        Ok(id)
    }

    async fn rollback(&self, _prepared: PreparedDeploy) -> DeployResult<()> {
        for key in &self.registered_vision_keys {
            crate::vision::unregister_engine(key);
        }
        Ok(())
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
                resource_kind: None,
                requires_model: None,
                gpu_supported: None,
                default_port: 0,
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
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            }],
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
        let m = manifest(
            "emb-ok",
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
