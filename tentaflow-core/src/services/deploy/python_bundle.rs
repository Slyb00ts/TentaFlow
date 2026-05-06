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
    auto_gpu_memory_utilization, build_endpoint_url, build_new_service, category_tag,
    host_os_supported, models_from_manifest, query_cuda0_vram_mib, resolve_display_name,
    smart_health_probe, DeployError, DeployResult, DeployStrategy, LogSink,
    PreparedDeploy, RuntimeHandle, SmartProbeConfig, SmartProbeOutcome,
};
use crate::deploy::process_ctl;
use crate::deploy::python_venv::{self, NativeDeployRequest};
use crate::services::manifest::{NativeRuntime, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

/// Wycina wszystkie wystapienia flagi `--gpu-memory-utilization X` (oraz
/// `--gpu-memory-utilization=X`) ze stringa VLLM_ARGS. Backend dolepia
/// pozniej dokladnie jedno wystapienie, zeby vllm nigdy nie widzial
/// duplikatow.
fn strip_gpu_memory_utilization(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "--gpu-memory-utilization" {
            // Skip flag and its value.
            i += 2;
            continue;
        }
        if tok.starts_with("--gpu-memory-utilization=") {
            i += 1;
            continue;
        }
        out.push(tok.to_string());
        i += 1;
    }
    out.join(" ")
}

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

        // Resolve model PRZED acquire portu — inaczej kazda nieudana proba
        // (wizard bez modelu, API call z pustym configiem) leasuje port ktory
        // dispatcher.rs nie zwalnia w sciezce bledu. Kilka takich w petli
        // wyczerpuje pule 5000-6000.
        let model_repo = super::resolve_model_repo(&self.manifest, &self.user_config);
        if model_repo.is_none() && self.manifest.engine.requires_model.unwrap_or(true) {
            return Err(DeployError::Manifest(format!(
                "engine '{}' requires a model — wizard must send `model_repo` or `model_preset_id`, or the manifest must declare at least one [[model_preset]]",
                self.manifest.engine.id
            )));
        }

        // Typed parametry schema → env + request_time params. apply_parameters_deploy
        // waliduje wartosci wzgledem range/options/kind ZANIM port jest zaalokowany
        // (out-of-range gpu_memory_utilization=2.0 → DeployError przed acquire).
        // DeployTarget::NativePythonBundle wybiera bindings gdzie `when = "native_python_bundle"`.
        let (param_app, request_time) = super::apply_parameters_deploy(
            &self.manifest,
            &self.user_config,
            super::DeployTarget::NativePythonBundle,
        )
        .map_err(|e| DeployError::Manifest(format!("apply parameters: {}", e)))?;

        let port = self
            .ports
            .acquire()
            .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
        let allocated_ports = vec![port];

        // Native deploys ignore any user-provided port — PortAllocator owns
        // the truth. Surface that loudly when the wizard or external caller
        // sent one, so debugging logs don't show "I asked for 8000, got
        // 5001" mystery.
        if let Some(requested) = self.user_config.get("port").and_then(|v| v.as_u64()) {
            if requested as u16 != port {
                if let Some(s) = &self.log_sink {
                    s.info(&format!(
                        "[python-bundle] ignoring user-provided port={} — native deploys always allocate from the pool; using {}",
                        requested, port
                    ));
                }
            }
        }

        let engine_id = self.manifest.engine.id.clone();
        let instance_name = format!("{}-{}", engine_id, port);

        // PORT goes into the engine env so `${PORT}` / `${PORT:-8000}` in
        // bundle.toml `[launch] args` resolves to the allocated port.
        // MODEL z `resolve_model_repo` (model_repo z wizarda lub
        // recommended preset). `param_app.env` z `apply_parameters_deploy`
        // niesie typed schema parametry jako env vars zgodnie z bindingami
        // `when = "native_python_bundle"`.
        let mut env: HashMap<String, String> = param_app.env;
        env.insert("PORT".into(), port.to_string());
        if let Some(model) = model_repo {
            env.insert("MODEL".into(), model);
        }
        // Single source of truth dla --gpu-memory-utilization.
        //   - Manual mode: wizard wysyla user's value (top-level
        //     `gpu_memory_utilization` lub w `vllm_args`). Backend ją honoruje
        //     1:1, BEZ klamrowania. Jezeli user wybral za duzo i vllm padnie
        //     przy starcie — to swiadoma decyzja (slider widzi free VRAM,
        //     wizard ostrzega).
        //   - Auto mode: wizard nie wysyla osobnego pola, vllm_args
        //     pochodzi z `recommended_vllm_args` (backend recommendation).
        //     Tu my dorzucamy auto-clamp na podstawie aktualnego free VRAM,
        //     zeby vllm wstal niezaleznie od stanu hosta.
        // Niezaleznie od trybu, finalnie w VLLM_ARGS jest **dokladnie jedna**
        // flaga --gpu-memory-utilization — wszystkie poprzednie sa wyciete.
        let user_explicit_ratio = env
            .get("GPU_MEMORY_UTILIZATION")
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| {
                self.user_config
                    .get("gpu_memory_utilization")
                    .and_then(|v| v.as_f64())
            });
        let from_vllm_args = env.get("VLLM_ARGS").and_then(|raw| {
            let mut iter = raw.split_whitespace();
            while let Some(tok) = iter.next() {
                if tok == "--gpu-memory-utilization" {
                    return iter.next().and_then(|v| v.parse::<f64>().ok());
                }
                if let Some(rest) = tok.strip_prefix("--gpu-memory-utilization=") {
                    return rest.parse::<f64>().ok();
                }
            }
            None
        });
        // Wybor finalnej wartosci:
        //   1. user explicit (osobne pole) — zawsze wygrywa, manual mode
        //   2. wartosc w vllm_args (gdy wizard manual jeszcze nie laduje
        //      do osobnego pola) — manual mode legacy
        //   3. auto-safe z aktualnego free VRAM — auto mode / no-input
        let final_ratio: Option<f64> = user_explicit_ratio
            .or(from_vllm_args)
            .or_else(auto_gpu_memory_utilization);
        if let Some(ratio) = final_ratio {
            // Wytnij ewentualne stare wystapienia flagi z VLLM_ARGS.
            let cleaned = match env.get("VLLM_ARGS") {
                Some(raw) => strip_gpu_memory_utilization(raw),
                None => String::new(),
            };
            let mut merged_parts: Vec<String> = Vec::new();
            if !cleaned.is_empty() {
                merged_parts.push(cleaned.clone());
            }
            merged_parts.push(format!("--gpu-memory-utilization {:.2}", ratio));

            // CUDA graph capture (default mode w vllm) potrafi zaalokowac
            // 1.5-3 GiB ponad `gpu_memory_utilization` budget przy
            // pierwszym profile run. Gdy user swiadomie ogranicza VRAM
            // (ratio nizsze niz auto-safe), enforce_eager wylacza graph
            // capture i utrzymuje peak alloc w budgecie kosztem ~5-10%
            // throughput. Skip gdy user explicit dal `--enforce-eager`
            // albo `--no-enforce-eager` w vllm_args (nie nadpisujemy).
            let auto_safe = auto_gpu_memory_utilization();
            let user_capped = (user_explicit_ratio.is_some() || from_vllm_args.is_some())
                && match auto_safe {
                    Some(safe) => ratio + 0.001 < safe,
                    None => ratio < 0.85,
                };
            let already_has_eager = cleaned
                .split_whitespace()
                .any(|t| t == "--enforce-eager" || t == "--no-enforce-eager");
            if user_capped && !already_has_eager {
                merged_parts.push("--enforce-eager".to_string());
                if let Some(s) = &self.log_sink {
                    s.info(
                        "[python-bundle] dolepiono --enforce-eager (user_ratio<auto_safe) — wylacza CUDA graph capture, peak alloc w budgecie",
                    );
                }
            }
            let merged = merged_parts.join(" ");
            env.insert("VLLM_ARGS".into(), merged);
            env.insert("GPU_MEMORY_UTILIZATION".into(), format!("{:.2}", ratio));
            if let Some(s) = &self.log_sink {
                let (free_mib, total_mib) = query_cuda0_vram_mib().unwrap_or((0, 0));
                if user_explicit_ratio.is_some() || from_vllm_args.is_some() {
                    let auto_ratio = auto_gpu_memory_utilization();
                    let warn = match auto_ratio {
                        Some(safe) if ratio > safe => format!(
                            " — UWAGA: przekracza bezpieczna wartosc {:.2} (free={}/total={} MiB), vllm moze paść przy starcie",
                            safe, free_mib, total_mib
                        ),
                        _ => String::new(),
                    };
                    s.info(&format!(
                        "[python-bundle] gpu_memory_utilization={:.2} (manual){}",
                        ratio, warn
                    ));
                } else {
                    s.info(&format!(
                        "[python-bundle] gpu_memory_utilization={:.2} (auto z free VRAM, free={}/total={} MiB)",
                        ratio, free_mib, total_mib
                    ));
                }
            }
        }

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
            endpoint_url: Some(build_endpoint_url(
                "127.0.0.1",
                port,
                self.manifest.engine.api,
            )),
            container_id: None,
            instance_dir: Some(venv_dir),
        };
        let models = models_from_manifest(&self.manifest, &self.user_config);
        // Merge typed `request_time_parameters` do config_json — bez tego
        // snapshot_builder.parse_request_time_parameters dostaje pusta mape
        // i BackendClient nigdy nie materializuje overrides do request body.
        let config_json = super::merge_config_json(&self.user_config, &request_time)
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

    #[test]
    fn strip_removes_space_separated_flag_and_value() {
        let raw = "--dtype auto --gpu-memory-utilization 0.6 --max-model-len 8192";
        assert_eq!(
            strip_gpu_memory_utilization(raw),
            "--dtype auto --max-model-len 8192"
        );
    }

    #[test]
    fn strip_removes_equals_form() {
        let raw = "--dtype auto --gpu-memory-utilization=0.6 --max-model-len 8192";
        assert_eq!(
            strip_gpu_memory_utilization(raw),
            "--dtype auto --max-model-len 8192"
        );
    }

    #[test]
    fn strip_removes_multiple_occurrences() {
        let raw = "--gpu-memory-utilization 0.9 --max-model-len 4096 --gpu-memory-utilization 0.6";
        assert_eq!(
            strip_gpu_memory_utilization(raw),
            "--max-model-len 4096"
        );
    }

    #[test]
    fn strip_no_op_when_flag_absent() {
        let raw = "--dtype auto --max-model-len 8192";
        assert_eq!(strip_gpu_memory_utilization(raw), raw);
    }

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
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
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
            parameters: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        };
        let ports = Arc::new(PortAllocator::new((48_000, 48_010), HashSet::new()).unwrap());
        let mut s = PythonBundleDeploy::new(manifest, serde_json::json!({}), ports, None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));
    }
}
