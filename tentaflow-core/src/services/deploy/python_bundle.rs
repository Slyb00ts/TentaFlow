// ============ File: services/deploy/python_bundle.rs — python-bundle deploy strategy ============
//
// `runtime = "python-bundle"` engines (vllm, xtts, parakeet, voxcpm, qwen-asr,
// comfyui) use a venv built from `<bundle_path>/requirements.lock`. The
// strategy hardlinks the venv from a cached template into a per-deployment
// instance dir, then spawns `python <entrypoint>` listening on $PORT.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Transaction;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use super::{
    build_new_service, host_os_supported, http_health_wait, models_from_manifest,
    standard_engine_env, DeployError, DeployResult, DeployStrategy, LogSink, PreparedDeploy,
    RuntimeHandle,
};
use crate::services::manifest::{NativeRuntime, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
use crate::services_repo::services::{self as services_repo, DeployMethod, ServiceStatus};

pub struct PythonBundleDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    log_sink: Option<LogSink>,
    child: std::sync::Mutex<Option<Child>>,
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
            child: std::sync::Mutex::new(None),
        }
    }
}

/// Cache root for venv templates / instances. Honors `TENTAFLOW_CACHE_DIR`.
fn cache_root() -> PathBuf {
    if let Ok(v) = std::env::var("TENTAFLOW_CACHE_DIR") {
        let p = PathBuf::from(v);
        if !p.exists() {
            let _ = std::fs::create_dir_all(&p);
        }
        return p;
    }
    crate::paths::tentaflow_home().join("cache")
}

fn template_dir(engine: &str, hash: &str) -> PathBuf {
    cache_root()
        .join("bundle-templates")
        .join(engine)
        .join(hash)
}

fn instance_dir(engine: &str, name: &str) -> PathBuf {
    cache_root()
        .join("bundle-instances")
        .join(engine)
        .join(name)
}

fn template_hash(bundle_path: &Path) -> DeployResult<String> {
    let mut hasher = Sha256::new();
    for fname in &["requirements.lock", "bundle.toml"] {
        let p = bundle_path.join(fname);
        if p.exists() {
            let data = std::fs::read(&p)?;
            hasher.update(fname.as_bytes());
            hasher.update(&data);
        }
    }
    Ok(hex::encode(hasher.finalize())[..16].to_string())
}

/// Locates `python3` on the host. Returns Err if absent so tests skip gracefully.
fn locate_python() -> DeployResult<PathBuf> {
    let candidates = ["python3", "python"];
    for c in candidates {
        if let Ok(out) = std::process::Command::new(c).arg("--version").output() {
            if out.status.success() {
                return Ok(PathBuf::from(c));
            }
        }
    }
    Err(DeployError::Spawn("python3 not in PATH".into()))
}

/// Builds the template venv if missing. Idempotent.
fn ensure_template(bundle_path: &Path, template: &Path) -> DeployResult<()> {
    if template.join("venv").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(template)?;
    let venv = template.join("venv");
    let python = locate_python()?;
    let status = std::process::Command::new(&python)
        .arg("-m")
        .arg("venv")
        .arg(&venv)
        .status()
        .map_err(|e| DeployError::Spawn(format!("python -m venv: {}", e)))?;
    if !status.success() {
        return Err(DeployError::Spawn(format!(
            "python -m venv exited with {:?}",
            status.code()
        )));
    }
    // Install requirements.lock if present.
    let req = bundle_path.join("requirements.lock");
    if req.exists() {
        let pip = if cfg!(windows) {
            venv.join("Scripts").join("pip.exe")
        } else {
            venv.join("bin").join("pip")
        };
        let status = std::process::Command::new(&pip)
            .arg("install")
            .arg("-r")
            .arg(&req)
            .status()
            .map_err(|e| DeployError::Spawn(format!("pip install: {}", e)))?;
        if !status.success() {
            return Err(DeployError::Spawn(format!(
                "pip install -r requirements.lock failed: code={:?}",
                status.code()
            )));
        }
    }
    Ok(())
}

/// Materializes an instance from template by trying hardlinks first, falling
/// back to a recursive copy when crossing filesystems.
fn materialize_instance(template: &Path, instance: &Path) -> DeployResult<()> {
    if instance.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(instance)?;
    copy_or_hardlink_dir(&template.join("venv"), &instance.join("venv"))?;
    Ok(())
}

fn copy_or_hardlink_dir(src: &Path, dst: &Path) -> DeployResult<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.file_name().ok_or_else(|| {
            DeployError::Other(format!("invalid file name in {}", path.display()))
        })?;
        let target = dst.join(rel);
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_or_hardlink_dir(&path, &target)?;
        } else if ty.is_symlink() {
            // Skip symlinks; they tend to point inside the template.
            let real = std::fs::read_link(&path)?;
            let _ = std::os::unix::fs::symlink(&real, &target).or_else(|_| {
                #[cfg(windows)]
                {
                    std::os::windows::fs::symlink_file(&real, &target)
                }
                #[cfg(not(windows))]
                {
                    Ok::<(), std::io::Error>(())
                }
            });
        } else {
            // Try hardlink; fall back to copy.
            if std::fs::hard_link(&path, &target).is_err() {
                std::fs::copy(&path, &target)?;
            }
        }
    }
    Ok(())
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
        let bundle = PathBuf::from(bundle_path);
        if !bundle.exists() {
            return Err(DeployError::Manifest(format!(
                "bundle_path does not exist: {}",
                bundle.display()
            )));
        }

        // Template + instance.
        let hash = template_hash(&bundle)?;
        let template = template_dir(&self.manifest.engine.id, &hash);
        ensure_template(&bundle, &template)?;

        let port = self
            .ports
            .acquire()
            .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
        let allocated_ports = vec![port];
        let instance_name = format!("{}-{}", self.manifest.engine.id, port);
        let instance = instance_dir(&self.manifest.engine.id, &instance_name);
        materialize_instance(&template, &instance)?;

        // Resolve entrypoint: prefer bundle/server.py, then bundle/main.py.
        let entry = ["server.py", "main.py", "app.py"]
            .iter()
            .map(|n| bundle.join(n))
            .find(|p| p.exists())
            .ok_or_else(|| {
                DeployError::Manifest(format!(
                    "bundle '{}' has no server.py / main.py / app.py",
                    bundle.display()
                ))
            })?;

        let venv_python = if cfg!(windows) {
            instance.join("venv").join("Scripts").join("python.exe")
        } else {
            instance.join("venv").join("bin").join("python")
        };
        if !venv_python.exists() {
            let _ = self.ports.release(port);
            return Err(DeployError::Spawn(format!(
                "venv python not found at {}",
                venv_python.display()
            )));
        }

        let mut env = standard_engine_env();
        env.insert("PORT".into(), port.to_string());

        let mut cmd = Command::new(&venv_python);
        cmd.arg(&entry);
        cmd.current_dir(&bundle);
        cmd.envs(env);
        cmd.kill_on_drop(true);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[python-bundle] spawn {} {} (PORT={})",
                venv_python.display(),
                entry.display(),
                port
            ));
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| DeployError::Spawn(format!("spawn python: {}", e)))?;
        let pid = child.id().map(|v| v as i64);

        // Pipe stdout / stderr line-by-line to the log sink while the child runs.
        if let Some(sink) = self.log_sink.clone() {
            if let Some(stdout) = child.stdout.take() {
                let s = sink.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        s.emit("log", &line);
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        sink.emit("log", &line);
                    }
                });
            }
        }

        if let Ok(mut slot) = self.child.lock() {
            *slot = Some(child);
        }

        // Health: vllm and friends expose /v1/models; xtts variants expose /health.
        let url_a = format!("http://127.0.0.1:{}/v1/models", port);
        let url_b = format!("http://127.0.0.1:{}/health", port);
        let res = wait_either(&url_a, &url_b, 60).await;
        if let Err(e) = res {
            self.kill_child().await;
            let _ = self.ports.release(port);
            return Err(e);
        }

        let runtime = RuntimeHandle {
            pid,
            port: Some(port),
            sidecar_port: None,
            endpoint_url: Some(format!("http://127.0.0.1:{}", port)),
            container_id: None,
            instance_dir: Some(instance),
        };
        let models = models_from_manifest(&self.manifest);
        let config_json = serde_json::to_string(&self.user_config)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
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
        self.kill_child().await;
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        // Intentionally keep instance_dir for debug; cleanup happens on
        // explicit DELETE service in a later phase.
        Ok(())
    }
}

impl PythonBundleDeploy {
    async fn kill_child(&self) {
        let child_opt = self.child.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut child) = child_opt {
            if let Some(pid) = child.id() {
                let _ = crate::deploy::process_ctl::terminate(pid);
            }
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            let _ = child.kill().await;
        }
    }
}

async fn wait_either(a: &str, b: &str, timeout_secs: u64) -> DeployResult<()> {
    use tokio::select;
    let fa = http_health_wait(a, timeout_secs);
    let fb = http_health_wait(b, timeout_secs);
    tokio::pin!(fa);
    tokio::pin!(fb);
    select! {
        r = &mut fa => r,
        r = &mut fb => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn python_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn template_hash_changes_with_requirements() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path();
        std::fs::write(bundle.join("bundle.toml"), b"engine='x'").unwrap();
        std::fs::write(bundle.join("requirements.lock"), b"a==1").unwrap();
        let h1 = template_hash(bundle).unwrap();
        std::fs::write(bundle.join("requirements.lock"), b"a==2").unwrap();
        let h2 = template_hash(bundle).unwrap();
        assert_ne!(h1, h2, "hash must change with requirements");
    }

    #[tokio::test]
    async fn python_bundle_template_is_cached_between_prepares() {
        if !python_available() {
            eprintln!("skipping: python3 unavailable");
            return;
        }
        // Use TENTAFLOW_CACHE_DIR to isolate this test.
        let cache = tempfile::tempdir().unwrap();
        std::env::set_var("TENTAFLOW_CACHE_DIR", cache.path());

        let bundle = tempfile::tempdir().unwrap();
        // No requirements.lock → ensure_template is fast (just creates empty venv).
        std::fs::write(bundle.path().join("server.py"), "raise SystemExit(0)").unwrap();
        let hash = template_hash(bundle.path()).unwrap();
        let tdir = template_dir("test-eng", &hash);
        ensure_template(bundle.path(), &tdir).unwrap();
        assert!(tdir.join("venv").exists(), "venv was created");

        // Second call should be a no-op (template already exists).
        let mtime_before = std::fs::metadata(tdir.join("venv"))
            .unwrap()
            .modified()
            .unwrap();
        ensure_template(bundle.path(), &tdir).unwrap();
        let mtime_after = std::fs::metadata(tdir.join("venv"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(mtime_before, mtime_after, "template was reused");

        // Cleanup env var to not leak into other tests.
        std::env::remove_var("TENTAFLOW_CACHE_DIR");
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
