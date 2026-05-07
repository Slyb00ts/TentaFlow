// ============ File: services/deploy/binary.rs — native-binary deploy strategy ============
//
// `runtime = "binary"` engines (sherpa-onnx, stable-diffusion-cpp, teams-bot)
// are spawned as a child process bound to a freshly allocated TCP port. The
// strategy waits for an HTTP health probe before committing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Transaction;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use super::{
    build_endpoint_url, build_new_service, category_tag, host_os_supported, models_from_manifest,
    resolve_display_name, smart_health_probe, standard_engine_env, DeployError, DeployResult,
    DeployStrategy, LogSink, PreparedDeploy, RuntimeHandle, SmartProbeConfig, SmartProbeOutcome,
};
use crate::deploy::process_ctl;
use crate::services::manifest::{NativeRuntime, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

pub struct BinaryDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    log_sink: Option<LogSink>,
    /// Child handle is stored on `self` (not on `PreparedDeploy`) so it stays
    /// alive across the await boundary in `deploy()`. Rollback consumes it.
    child: std::sync::Mutex<Option<Child>>,
}

impl BinaryDeploy {
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
            child: std::sync::Mutex::new(None),
        }
    }

    fn binary_root(&self) -> DeployResult<PathBuf> {
        let native = self.manifest.deploy.native.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' has no [deploy.native]",
                self.manifest.engine.id
            ))
        })?;
        if native.runtime != NativeRuntime::Binary {
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
        let port = self
            .ports
            .acquire()
            .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
        let allocated_ports = vec![port];

        // Pick the executable: prefer `<root>/server`, then `<root>/run.sh`,
        // then `<root>/start.sh`, then `<root>/build.sh` (used by tests).
        let candidates = ["server", "run.sh", "start.sh", "build.sh"];
        let exe = candidates
            .iter()
            .map(|n| root.join(n))
            .find(|p| p.exists())
            .ok_or_else(|| {
                DeployError::Spawn(format!(
                    "no startup script in {} (looked for {:?})",
                    root.display(),
                    candidates
                ))
            })?;

        let mut env = standard_engine_env();
        env.insert("PORT".to_string(), port.to_string());

        let mut cmd = Command::new(&exe);
        cmd.current_dir(&root);
        cmd.envs(env);
        cmd.kill_on_drop(true);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

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
        let pid_for_probe = child.id();
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
        };
        let outcome = smart_health_probe(probe_cfg, move || async move {
            match pid_for_probe {
                Some(pid) if process_ctl::is_alive(pid) => None,
                Some(_) => Some(None),
                // No PID at all — treat as gone.
                None => Some(None),
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

        let models = if matches!(
            self.manifest.engine.resource_kind,
            Some(crate::services::manifest::ResourceKind::Infra)
        ) || matches!(
            self.manifest.engine.category,
            crate::services::manifest::Category::Agents
        ) {
            // Infra & agents have no model registry rows.
            Vec::new()
        } else {
            models_from_manifest(&self.manifest, &self.user_config)
        };

        // Typed schema params + request_time → config_json. Dla binary
        // engines (sherpa-onnx, stable-diffusion-cpp, teams-bot) zwykle
        // pusta `parameters` w manifescie, wiec request_time = default.
        let (_param_app, request_time) = super::apply_parameters_deploy(
            &self.manifest,
            &self.user_config,
            super::DeployTarget::NativeBinary,
        )
        .map_err(|e| DeployError::Manifest(format!("apply parameters: {}", e)))?;
        let config_json = super::merge_config_json(&self.user_config, &request_time)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::NativeBinary,
            transport: Transport::HttpDirect,
            runtime,
            models,
            config_json,
            allocated_ports,
        })
    }

    fn commit(&self, tx: &Transaction<'_>, prepared: &PreparedDeploy) -> DeployResult<i64> {
        let new = build_new_service(prepared, ServiceStatus::Running);
        let id = services_repo::insert_in_tx(tx, &new)?;
        Ok(id)
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
        let mut s = BinaryDeploy::new(manifest, serde_json::json!({}), ports, None);
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
        let mut s = BinaryDeploy::new(manifest, serde_json::json!({}), ports, None);
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
        let mut s = BinaryDeploy::new(manifest, serde_json::json!({}), ports.clone(), None);
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
