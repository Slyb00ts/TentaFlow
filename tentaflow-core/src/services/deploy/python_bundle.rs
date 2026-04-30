// ============ File: services/deploy/python_bundle.rs — python-bundle deploy strategy ============
//
// Thin adapter over `crate::deploy::python_venv`: that module owns the full
// "bundle.toml → venv → wheels → spawn" flow (pypi / git / vllm-metal /
// install_variants per backend, ${PORT} substitution, FlashInfer/CUDA env
// wiring, engine.log redirection). This strategy only handles the service-
// layer concerns: manifest+platform checks, port allocation, log forwarding,
// HTTP health probe, and the commit/rollback contract.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Transaction;

use super::{
    build_new_service, category_tag, host_os_supported, models_from_manifest, resolve_display_name,
    smart_health_probe, DeployError, DeployResult, DeployStrategy, LogSink, PreparedDeploy,
    RuntimeHandle, SmartProbeConfig, SmartProbeOutcome,
};
use crate::deploy::process_ctl;
use crate::deploy::python_venv::{self, NativeDeployRequest};
use crate::services::manifest::{NativeRuntime, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

/// Tracked state for rollback: the spawned engine's PID. The venv dir is
/// captured by the prepare flow and reused via `RuntimeHandle.instance_dir`
/// for the dashboard / debug log path.
struct RunningState {
    pid: u32,
}

pub struct PythonBundleDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    log_sink: Option<LogSink>,
    running: Mutex<Option<RunningState>>,
}

impl PythonBundleDeploy {
    pub fn new(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        ports: Arc<PortAllocator>,
        log_sink: Option<LogSink>,
    ) -> Self {
        Self {
            manifest,
            user_config,
            ports,
            log_sink,
            running: Mutex::new(None),
        }
    }

    /// Builds a `python_venv::LogSink` callback that forwards every line
    /// into the service-layer `LogSink` (broadcaster + DB log_tail).
    fn build_venv_log(&self) -> python_venv::LogSink {
        let outer = self.log_sink.clone();
        Arc::new(move |line: &str| {
            if let Some(sink) = &outer {
                sink.emit("log", line);
            }
        })
    }
}

#[async_trait]
impl DeployStrategy for PythonBundleDeploy {
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        let native = self.manifest.deploy.native.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' missing [deploy.native]",
                self.manifest.engine.id
            ))
        })?;
        if native.runtime != NativeRuntime::PythonBundle {
            return Err(DeployError::Manifest(format!(
                "engine '{}' is not a python-bundle runtime ({:?})",
                self.manifest.engine.id, native.runtime
            )));
        }
        if !host_os_supported(&native.platforms) {
            return Err(DeployError::Manifest(format!(
                "engine '{}' not supported on host OS",
                self.manifest.engine.id
            )));
        }
        let bundle_path = native
            .bundle_path
            .as_deref()
            .ok_or_else(|| DeployError::Manifest("python-bundle requires bundle_path".into()))?;
        // Manifest paths are relative to the extracted `tentaflow-containers/`
        // tree, which `paths::ensure_app_dirs` guarantees lives under
        // `<tentaflow_home>/containers/`.
        let bundle = crate::paths::containers_root().join(bundle_path);
        if !bundle.exists() {
            return Err(DeployError::Manifest(format!(
                "bundle_path does not exist: {}",
                bundle.display()
            )));
        }

        let port = self
            .ports
            .acquire()
            .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
        let allocated_ports = vec![port];

        let engine_id = self.manifest.engine.id.clone();
        let instance_name = format!("{}-{}", engine_id, port);

        // PORT goes into the engine env so `${PORT}` / `${PORT:-8000}` in
        // bundle.toml `[launch] args` resolves to the allocated port.
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("PORT".into(), port.to_string());

        let req = NativeDeployRequest {
            engine: engine_id.clone(),
            instance_name: Some(instance_name.clone()),
            env,
        };
        let log = self.build_venv_log();

        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[python-bundle] bootstrap+spawn engine={} port={}",
                engine_id, port
            ));
        }

        // bootstrap (Python + uv + venv + wheels + git/pypi/vllm-metal install)
        // and engine spawn are blocking; offload onto the blocking pool so we
        // don't stall the runtime while pip/uv compile flashinfer for minutes.
        let running =
            tokio::task::spawn_blocking(move || python_venv::deploy_with_logs(&req, &log))
                .await
                .map_err(|e| DeployError::Spawn(format!("join python_venv: {}", e)))?
                .map_err(|e| DeployError::Spawn(format!("python_venv deploy: {:#}", e)))?;

        let pid = running.child.id();
        let venv_dir = running.venv_dir.clone();

        // `std::process::Child` does NOT kill the process on drop (unlike
        // `tokio::process::Child` with kill_on_drop). Letting it drop here
        // detaches the engine; we track it by PID for rollback / stop.
        drop(running);

        if let Ok(mut slot) = self.running.lock() {
            *slot = Some(RunningState { pid });
        }

        // Smart probe: race readiness URLs forever, bail only on process
        // exit. vllm, sglang, qwen-asr, parakeet expose /v1/models;
        // xtts/voxcpm wrappers expose /health.
        let probe_cfg = SmartProbeConfig {
            readiness_urls: vec![
                format!("http://127.0.0.1:{}/v1/models", port),
                format!("http://127.0.0.1:{}/health", port),
            ],
            status_report_interval: Duration::from_secs(30),
            log_sink: self.log_sink.clone(),
        };
        let outcome = smart_health_probe(probe_cfg, move || async move {
            // None = still alive; Some(_) = exited (we cannot recover the
            // exit code via kill(pid, 0), only liveness — that's fine).
            if process_ctl::is_alive(pid) {
                None
            } else {
                Some(None)
            }
        })
        .await;

        match outcome {
            SmartProbeOutcome::Ready => {}
            SmartProbeOutcome::ProcessExited(code) => {
                if let Some(s) = &self.log_sink {
                    s.info(&format!(
                        "[python-bundle] engine process exited{} — see {}/engine.log",
                        code.map(|c| format!(" with code {}", c))
                            .unwrap_or_default(),
                        venv_dir.display()
                    ));
                }
                self.kill_running().await;
                let _ = self.ports.release(port);
                return Err(DeployError::Spawn(format!(
                    "engine process exited before becoming ready (see {}/engine.log)",
                    venv_dir.display()
                )));
            }
        }

        let runtime = RuntimeHandle {
            pid: Some(pid as i64),
            port: Some(port),
            sidecar_port: None,
            endpoint_url: Some(format!("http://127.0.0.1:{}", port)),
            container_id: None,
            instance_dir: Some(venv_dir),
        };
        let models = models_from_manifest(&self.manifest, &self.user_config);
        let config_json = serde_json::to_string(&self.user_config)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id,
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::NativePythonBundle,
            transport: Transport::HttpDirect,
            runtime,
            models,
            config_json,
            allocated_ports,
        })
    }

    fn commit(&self, tx: &Transaction<'_>, prepared: &PreparedDeploy) -> DeployResult<i64> {
        let new = build_new_service(prepared, ServiceStatus::Running);
        Ok(services_repo::insert_in_tx(tx, &new)?)
    }

    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()> {
        self.kill_running().await;
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        // Intentionally keep instance_dir (engine.log + venv) for debug;
        // cleanup happens on explicit DELETE service in a later phase.
        Ok(())
    }
}

impl PythonBundleDeploy {
    async fn kill_running(&self) {
        let state = self.running.lock().ok().and_then(|mut slot| slot.take());
        let Some(state) = state else { return };
        let _ = crate::deploy::process_ctl::terminate(state.pid);
        // Give the engine a moment to flush before any caller wipes the
        // venv. We don't wait on the Child handle (we forgot it earlier);
        // a fixed grace is enough for clean shutdown logs.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Cover the missing-bundle-path failure path; doesn't need python.
    #[tokio::test]
    async fn prepare_fails_when_bundle_path_missing() {
        use crate::services::manifest::{
            ApiKind, Category, DeploySection, Engine, NativeDeploy, NativeRuntime, TargetOs,
        };
        let manifest = ServiceManifest {
            engine: Engine {
                id: "no-bundle".into(),
                category: Category::Llm,
                name: "no-bundle".into(),
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
            },
            deploy: DeploySection {
                docker: None,
                native: Some(NativeDeploy {
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    runtime: NativeRuntime::PythonBundle,
                    feature_flag: None,
                    binary_path: None,
                    bundle_path: Some("/nonexistent/bundle/path".into()),
                }),
                external: None,
            },
            model_presets: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        };
        let ports = Arc::new(PortAllocator::new((48_000, 48_010), HashSet::new()).unwrap());
        let mut s = PythonBundleDeploy::new(manifest, serde_json::json!({}), ports, None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));
    }
}
