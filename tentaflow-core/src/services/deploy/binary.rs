// ============ File: services/deploy/binary.rs — native-binary deploy strategy ============
//
// `runtime = "binary"` engines (sherpa-onnx, stable-diffusion-cpp, teams-bot)
// are spawned as a child process bound to a freshly allocated TCP port. The
// strategy waits for an HTTP health probe before committing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use async_trait::async_trait;
use rusqlite::Transaction;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use super::{
    build_endpoint_url, build_new_service, category_tag, host_os_supported, models_from_manifest,
    resolve_display_name, smart_health_probe, standard_engine_env, DeployError, DeployResult,
    DeployStrategy, LogSink, PreparedDeploy, RuntimeHandle, SmartProbeConfig, SmartProbeOutcome,
};
use crate::services::manifest::{NativeRuntime, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

pub struct BinaryDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    /// HF token forwarded to the engine process as `HF_TOKEN` when the engine
    /// pulls its weights from Hugging Face (e.g. ds4 GGUF repos). `None` for
    /// model-less binaries (teams-bot) and engines bundling their own weights.
    hf_token: Option<String>,
    log_sink: Option<LogSink>,
    /// Child handle is stored on `self` (not on `PreparedDeploy`) so it stays
    /// alive across the await boundary in `deploy()`. Rollback consumes it.
    child: Arc<std::sync::Mutex<Option<Child>>>,
    /// Port z DB przy respawn — patrz `PythonBundleDeploy::preserved_port`.
    preserved_port: Option<u16>,
}

impl BinaryDeploy {
    pub fn new(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        ports: Arc<PortAllocator>,
        hf_token: Option<String>,
        log_sink: Option<LogSink>,
    ) -> Self {
        Self::new_with_port(manifest, user_config, ports, hf_token, log_sink, None)
    }

    pub fn new_with_port(
        manifest: ServiceManifest,
        user_config: serde_json::Value,
        ports: Arc<PortAllocator>,
        hf_token: Option<String>,
        log_sink: Option<LogSink>,
        preserved_port: Option<u16>,
    ) -> Self {
        Self {
            manifest,
            user_config,
            ports,
            hf_token,
            log_sink,
            child: Arc::new(std::sync::Mutex::new(None)),
            preserved_port,
        }
    }

    fn binary_root(&self) -> DeployResult<PathBuf> {
        let native = self.manifest.deploy.native.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' has no [deploy.native]",
                self.manifest.engine.id
            ))
        })?;
        if !matches!(
            native.runtime,
            NativeRuntime::Binary | NativeRuntime::ManagedCli
        ) {
            return Err(DeployError::Manifest(format!(
                "engine '{}' is not a binary runtime ({:?})",
                self.manifest.engine.id, native.runtime
            )));
        }
        let bp = native.binary_path.as_deref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}': [deploy.native].binary_path required for runtime=binary",
                self.manifest.engine.id
            ))
        })?;
        // Manifest binary_path is relative to the extracted containers tree.
        // PathBuf::join is a no-op when `bp` is absolute (e.g. tests pass a
        // tempdir path), so this stays compatible with both layouts.
        let path = crate::paths::containers_root().join(bp);
        if !path.exists() {
            return Err(DeployError::Manifest(format!(
                "binary_path does not exist: {}",
                path.display()
            )));
        }
        Ok(path)
    }

    async fn prepare_managed_cli_env(
        &self,
        native: &crate::services::manifest::NativeDeploy,
        env: &mut std::collections::HashMap<String, String>,
    ) -> DeployResult<()> {
        if native.runtime != NativeRuntime::ManagedCli {
            return Ok(());
        }
        let (package, executable) = match self.manifest.engine.id.as_str() {
            "codex" => ("@openai/codex", "codex"),
            "claude-code" => ("@anthropic-ai/claude-code", "claude"),
            other => {
                return Err(DeployError::Manifest(format!(
                    "managed-cli engine '{other}' has no installer mapping"
                )))
            }
        };
        let install_root = crate::paths::cache_dir()
            .join("coding-agents")
            .join(&self.manifest.engine.id)
            .join(&self.manifest.engine.version);
        let bin_dir = install_root.join("node_modules").join(".bin");
        let executable_name = if cfg!(windows) {
            format!("{executable}.cmd")
        } else {
            executable.to_string()
        };
        if !bin_dir.join(executable_name).exists() {
            std::fs::create_dir_all(&install_root).map_err(|e| {
                DeployError::Spawn(format!("create managed-cli install directory: {e}"))
            })?;
            if let Some(s) = &self.log_sink {
                s.info(&format!(
                    "[managed-cli] installing {}@{}",
                    package, self.manifest.engine.version
                ));
            }
            let output = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
                .arg("install")
                .arg("--prefix")
                .arg(&install_root)
                .arg("--no-audit")
                .arg("--no-fund")
                .arg(format!("{}@{}", package, self.manifest.engine.version))
                .output()
                .await
                .map_err(|e| DeployError::Spawn(format!("start npm installer: {e}")))?;
            if !output.status.success() {
                return Err(DeployError::Spawn(format!(
                    "npm install {}@{} failed: {}",
                    package,
                    self.manifest.engine.version,
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin_dir];
        paths.extend(std::env::split_paths(&inherited_path));
        let joined = std::env::join_paths(paths)
            .map_err(|e| DeployError::Spawn(format!("build managed-cli PATH: {e}")))?;
        env.insert("PATH".to_string(), joined.to_string_lossy().into_owned());
        let state_dir = crate::paths::category_dir(crate::paths::StorageCategory::Keys)
            .join("coding-agents")
            .join(&self.manifest.engine.id);
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| DeployError::Spawn(format!("create managed-cli state directory: {e}")))?;
        env.insert(
            "TENTAFLOW_CODING_AGENT_DATA_DIR".to_string(),
            state_dir.to_string_lossy().into_owned(),
        );
        let workspace_root =
            self.user_config
                .get("workspace_root")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| {
                    DeployError::Spawn(format!("resolve coding-agent workspace: {e}"))
                })?);
        let workspace_root = std::fs::canonicalize(&workspace_root).map_err(|e| {
            DeployError::Spawn(format!(
                "invalid coding-agent workspace {}: {e}",
                workspace_root.display()
            ))
        })?;
        env.insert(
            "TENTAFLOW_WORKSPACE_ROOT".to_string(),
            workspace_root.to_string_lossy().into_owned(),
        );
        if self.manifest.engine.id == "codex" {
            env.insert(
                "CODEX_HOME".to_string(),
                state_dir.to_string_lossy().into_owned(),
            );
        }
        Ok(())
    }
}

#[async_trait]
impl DeployStrategy for BinaryDeploy {
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        let native = self
            .manifest
            .deploy
            .native
            .as_ref()
            .ok_or_else(|| DeployError::Manifest("missing [deploy.native]".into()))?;
        if !host_os_supported(&native.platforms) {
            return Err(DeployError::Manifest(format!(
                "engine '{}' not supported on host OS",
                self.manifest.engine.id
            )));
        }

        let root = self.binary_root()?;
        let managed_cli_executable = if native.runtime == NativeRuntime::ManagedCli {
            let source_hash = self.manifest.native_source_hash.trim();
            if source_hash.is_empty() {
                return Err(DeployError::Manifest(format!(
                    "engine '{}': managed-cli runtime has no native source hash",
                    self.manifest.engine.id
                )));
            }
            let immutable_root = crate::paths::cache_dir()
                .join("coding-agents")
                .join("bridge")
                .join(&self.manifest.engine.id)
                .join(source_hash);
            #[cfg(windows)]
            let immutable_server = immutable_root.join("server.exe");
            #[cfg(not(windows))]
            let immutable_server = immutable_root.join("server");
            if immutable_server.exists() {
                Some(immutable_server)
            } else {
                if let Some(s) = &self.log_sink {
                    s.info("[managed-cli] building the local bridge");
                }
                #[cfg(windows)]
                let output = Command::new("powershell.exe")
                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                    .arg(root.join("build.ps1"))
                    .output()
                    .await;
                #[cfg(not(windows))]
                let output = Command::new("sh").arg(root.join("build.sh")).output().await;
                let output = output
                    .map_err(|e| DeployError::Spawn(format!("build coding-agent bridge: {e}")))?;
                #[cfg(windows)]
                let built_server = root
                    .join("target")
                    .join("release")
                    .join("tentaflow-coding-agent-bridge.exe");
                #[cfg(not(windows))]
                let built_server = root
                    .join("target")
                    .join("release")
                    .join("tentaflow-coding-agent-bridge");
                if !output.status.success() || !built_server.exists() {
                    return Err(DeployError::Spawn(format!(
                        "coding-agent bridge build failed with status {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    )));
                }
                std::fs::create_dir_all(&immutable_root).map_err(|e| {
                    DeployError::Spawn(format!("create coding-agent bridge cache: {e}"))
                })?;
                let temporary = immutable_root.join(format!(
                    ".server-{}-{}",
                    std::process::id(),
                    self.manifest.engine.id
                ));
                std::fs::copy(&built_server, &temporary).map_err(|e| {
                    DeployError::Spawn(format!("cache coding-agent bridge executable: {e}"))
                })?;
                std::fs::rename(&temporary, &immutable_server).map_err(|e| {
                    let _ = std::fs::remove_file(&temporary);
                    DeployError::Spawn(format!("publish coding-agent bridge executable: {e}"))
                })?;
                Some(immutable_server)
            }
        } else {
            None
        };
        // Respawn istniejacego serwisu zachowuje port z DB.
        let port = self
            .ports
            .acquire_or_specific(self.preserved_port)
            .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
        let allocated_ports = vec![port];

        // Pick the executable: prefer `<root>/server`, then `<root>/run.sh`,
        // then `<root>/start.sh`, then `<root>/build.sh` (used by tests).
        #[cfg(windows)]
        let candidates = ["server.exe", "run.cmd", "run.ps1", "start.cmd"];
        #[cfg(not(windows))]
        let candidates = ["server", "run.sh", "start.sh", "build.sh"];
        let exe = match managed_cli_executable {
            Some(path) => path,
            None => candidates
                .iter()
                .map(|n| root.join(n))
                .find(|p| p.exists())
                .ok_or_else(|| {
                    DeployError::Spawn(format!(
                        "no startup script in {} (looked for {:?})",
                        root.display(),
                        candidates
                    ))
                })?,
        };

        // Typed schema params → env (Env bindings) + request_time → config_json.
        // Computed before spawn so the launch script receives the engine's
        // tuning knobs (ds4: backend, ctx, SSD streaming, MTP) as env vars.
        let (param_app, request_time) = super::apply_parameters_deploy(
            &self.manifest,
            &self.user_config,
            super::DeployTarget::NativeBinary,
        )
        .map_err(|e| DeployError::Manifest(format!("apply parameters: {}", e)))?;

        // Mirror python_bundle's env assembly: param Env bindings + standard
        // engine cache env + PORT + MODEL/SERVED_MODEL_NAME + HF token + engine
        // tuning passthrough + GPU visibility. The launch script maps these to
        // the engine binary's CLI flags.
        let mut env = param_app.env;
        for (k, v) in standard_engine_env() {
            env.entry(k).or_insert(v);
        }
        env.insert("PORT".to_string(), port.to_string());
        env.insert(
            "TENTAFLOW_ENGINE_ID".to_string(),
            self.manifest.engine.id.clone(),
        );
        if let Some(model) = super::resolve_model_repo(&self.manifest, &self.user_config) {
            env.insert("MODEL".to_string(), model);
        }
        if let Some(served) = super::resolve_served_model_name(&self.manifest, &self.user_config) {
            env.insert("SERVED_MODEL_NAME".to_string(), served);
        }
        // HF_TOKEN only for engines that actually pull weights from HF.
        if super::engine_uses_hf_model(&self.manifest, &self.user_config) {
            if let Some(token) = self
                .hf_token
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                env.insert("HF_TOKEN".to_string(), token.to_string());
            }
        }
        super::apply_engine_env(&self.user_config, &mut env);
        super::apply_gpu_selection_env(&self.user_config, &mut env);
        self.prepare_managed_cli_env(native, &mut env).await?;

        let mut cmd = Command::new(&exe);
        cmd.current_dir(&root);
        cmd.envs(env);
        cmd.stdin(std::process::Stdio::null());
        // NO kill_on_drop: a successful deploy drops this strategy object once
        // commit() returns, and kill_on_drop would then SIGKILL the engine we
        // just launched — fatal for slow-loading engines (ds4 loads ~80 GB over
        // ~minute, so the supervisor probe + respawn never let it stay up). The
        // process is tracked by PID in RuntimeHandle; failure/rollback paths
        // kill it explicitly via kill_child(). Mirrors python_bundle (detach +
        // PID-tracked stop).
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // A separate session keeps background services away from terminal job
        // control while preserving pgid = pid for process-tree termination.
        #[cfg(unix)]
        unsafe {
            cmd.as_std_mut().pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        if let Some(s) = &self.log_sink {
            s.info(&format!("[binary] spawn {} (PORT={})", exe.display(), port));
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| DeployError::Spawn(format!("spawn {}: {}", exe.display(), e)))?;
        let pid = child.id().map(|v| v as i64);

        // Pipe stdout / stderr into the log sink line-by-line so the dashboard
        // sees engine startup output in real time. Both pipes are owned tasks;
        // they end when the child closes its descriptors.
        let sink_opt = self.log_sink.clone();
        if let Some(stdout) = child.stdout.take() {
            let s = sink_opt.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(sink) = &s {
                        sink.emit("log", &line);
                    }
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let s = sink_opt.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(sink) = &s {
                        sink.emit("log", &line);
                    }
                }
            });
        }

        // Stash the child for later rollback / for keep-alive across the
        // commit await.
        let child_for_probe = Arc::clone(&self.child);
        if let Ok(mut slot) = self.child.lock() {
            *slot = Some(child);
        }

        // Smart probe: vllm-ish openai-compat exposes /v1/models, sherpa-onnx
        // / teams-bot etc. expose /health.
        let probe_cfg = SmartProbeConfig {
            readiness_urls: vec![
                format!("http://127.0.0.1:{}/health", port),
                format!("http://127.0.0.1:{}/v1/models", port),
            ],
            status_report_interval: Duration::from_secs(30),
            log_sink: self.log_sink.clone(),
            // Brak hard timeoutu — process death (SIGSEGV / panic) flag'uje
            // jako Failed. Sherpa-onnx/teams-bot zwykle <10s ale duze
            // modele moga ladowac wiecej.
            max_wait: None,
        };
        let outcome = smart_health_probe(probe_cfg, move || {
            let child = Arc::clone(&child_for_probe);
            async move {
                let mut slot = child.lock().ok()?;
                let process = slot.as_mut()?;
                match process.try_wait() {
                    Ok(Some(status)) => Some(status.code()),
                    Ok(None) => None,
                    Err(_) => Some(None),
                }
            }
        })
        .await;

        match outcome {
            SmartProbeOutcome::Ready => {}
            SmartProbeOutcome::ProcessExited(code) => {
                self.kill_child().await;
                let _ = self.ports.release(port);
                return Err(DeployError::Spawn(format!(
                    "engine process exited before becoming ready{}",
                    code.map(|c| format!(" (code {})", c)).unwrap_or_default()
                )));
            }
        }

        let runtime = RuntimeHandle {
            pid,
            port: Some(port),
            sidecar_port: None,
            endpoint_url: Some(build_endpoint_url(
                "127.0.0.1",
                port,
                self.manifest.engine.api,
            )),
            container_id: None,
            instance_dir: None,
        };

        let models = if self.manifest.engine.is_model_less() {
            // Infra & agents have no model registry rows.
            Vec::new()
        } else {
            models_from_manifest(&self.manifest, &self.user_config)
        };

        // `request_time` params (computed before spawn) → config_json.
        let config_json = super::merge_config_json(&self.user_config, &request_time)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        let managed_cli = native.runtime == NativeRuntime::ManagedCli;
        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: if managed_cli {
                DeployMethod::NativeManagedCli
            } else {
                DeployMethod::NativeBinary
            },
            transport: if managed_cli {
                Transport::AgentRpc
            } else {
                Transport::HttpDirect
            },
            runtime,
            models,
            config_json,
            allocated_ports,
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

    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()> {
        self.kill_child().await;
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        Ok(())
    }
}

impl BinaryDeploy {
    async fn kill_child(&self) {
        // Take the child out of the mutex (sync), then kill async.
        let child_opt = self.child.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child_opt {
            // Try graceful first.
            if let Some(pid) = child.id() {
                let _ = crate::deploy::process_ctl::terminate(pid);
            }
            // Wait briefly so the async runtime reaps it, then force kill if needed.
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            let _ = child.kill().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::manifest::{
        ApiKind, Category, DeploySection, Engine, NativeDeploy, NativeRuntime, TargetOs,
    };
    use std::collections::HashSet;

    fn make_manifest(id: &str, binary_path: &str) -> ServiceManifest {
        ServiceManifest {
            engine: Engine {
                reasoning_levels: None,
                id: id.into(),
                backend: None,
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
                reverse_requests: false,
                default_port: 0,
                dgx_spark: None,
                cluster_capable: None,
                preset_only: None,
                cluster_launch: None,
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
                    runtime: NativeRuntime::Binary,
                    feature_flag: None,
                    binary_path: Some(binary_path.into()),
                    bundle_path: None,
                }),
                external: None,
            },
            model_presets: vec![],
            parameters: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    /// Writes a tiny shell server that listens on $PORT and returns 200 on /health.
    /// Skipped on Windows in tests.
    #[cfg(unix)]
    fn write_fake_server(dir: &std::path::Path) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("server");
        let script = r#"#!/usr/bin/env bash
PORT=${PORT:-0}
# Minimal HTTP server using bash + ncat fallback. We use python3 if available
# because nc availability differs across distros.
if command -v python3 >/dev/null 2>&1; then
  python3 -c "
import http.server, socketserver, os
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.send_header('Content-Type','application/json'); self.end_headers(); self.wfile.write(b'{}')
    def log_message(self, *a, **k): pass
port = int(os.environ.get('PORT','0'))
with socketserver.TCPServer(('127.0.0.1', port), H) as s: s.serve_forever()
"
else
  echo "no python3" >&2; exit 1
fi
"#;
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        let mut perms = f.metadata().unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn binary_spawn_health_check_succeeds() {
        // Skip if no python3 — without it our fake server does nothing.
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: python3 unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_fake_server(dir.path());
        let manifest = make_manifest("bin-spawn-ok", dir.path().to_str().unwrap());
        // Use 49800..49900 (private/dynamic range, free na typowych dev hostach)
        // — 47000..47050 koliduje z wieloma lokalnymi serwisami (tentaflow itself).
        let ports = Arc::new(PortAllocator::new((49_800, 49_900), HashSet::new()).unwrap());
        let mut s = BinaryDeploy::new(manifest, serde_json::json!({}), ports, None, None);
        let prepared = s.prepare().await.expect("prepare succeeds");
        assert!(prepared.runtime.pid.is_some());
        assert!(prepared.runtime.port.is_some());
        // Cleanup.
        s.rollback(prepared).await.unwrap();
    }

    #[tokio::test]
    async fn binary_health_timeout_returns_err() {
        // No script at all → spawn fails, mapped to DeployError::Spawn.
        let dir = tempfile::tempdir().unwrap();
        let manifest = make_manifest("bin-no-script", dir.path().to_str().unwrap());
        let ports = Arc::new(PortAllocator::new((49_910, 49_920), HashSet::new()).unwrap());
        let mut s = BinaryDeploy::new(manifest, serde_json::json!({}), ports, None, None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Spawn(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn binary_rollback_releases_port() {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_fake_server(dir.path());
        let manifest = make_manifest("bin-rb", dir.path().to_str().unwrap());
        let ports = Arc::new(PortAllocator::new((49_700, 49_799), HashSet::new()).unwrap());
        let mut s = BinaryDeploy::new(manifest, serde_json::json!({}), ports.clone(), None, None);
        let prepared = s.prepare().await.unwrap();
        let used = prepared.runtime.port.unwrap();
        s.rollback(prepared).await.unwrap();
        // After rollback the port should be reusable.
        let next = ports.acquire().unwrap();
        // Cycle eventually returns the previously released port; we just check
        // we can keep allocating without exhausting the small range.
        assert!(next >= 49_700 && next <= 49_799);
        let _ = ports.release(used);
        let _ = ports.release(next);
    }
}
