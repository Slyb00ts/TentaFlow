// ============ File: services/deploy/docker.rs — docker-container deploy strategy ============
//
// Default transport is `sidecar_quic`: a Rust QUIC sidecar speaks to the
// container's native HTTP API on a host-mapped port. A `transport_explicit:
// "direct_http"` hint in `user_config` skips the sidecar and exposes the
// container's HTTP port directly (Phase 6 preview for engines like Ollama).
//
// This strategy compiles only with the `docker` feature. Without it the
// `DockerDeploy::new` factory returns a stub that always errors at prepare.

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Transaction;

#[cfg(feature = "docker")]
use std::path::PathBuf;

#[cfg(feature = "docker")]
use super::{
    build_endpoint_url, category_tag, models_from_manifest, resolve_display_name,
    smart_health_probe, RuntimeHandle, SmartProbeConfig, SmartProbeOutcome,
};
use super::{
    build_new_service, transport_hint, DeployError, DeployResult, DeployStrategy, LogSink,
    PreparedDeploy,
};
use crate::services::manifest::{DockerTransport, ServiceManifest};
use crate::services::ports::PortAllocator;
use crate::services::transport::Transport;
#[cfg(feature = "docker")]
use crate::services_repo::services::DeployMethod;
use crate::services_repo::services::{self as services_repo, ServiceStatus};

pub struct DockerDeploy {
    manifest: ServiceManifest,
    user_config: serde_json::Value,
    ports: Arc<PortAllocator>,
    /// Token HF rozwiazany per-node w `deploy()` z secure setting. Idzie tylko do
    /// ENV kontenera (`HF_TOKEN`), NIGDY do `user_config`/config_json. `None` =
    /// brak tokenu (publiczne repo).
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    hf_token: Option<String>,
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    log_sink: Option<LogSink>,
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    container_id: std::sync::Mutex<Option<String>>,
    /// Port z DB przy respawn — patrz `PythonBundleDeploy::preserved_port`.
    preserved_port: Option<u16>,
}

impl DockerDeploy {
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
            container_id: std::sync::Mutex::new(None),
            preserved_port,
        }
    }

    /// Resolves the runtime transport for this docker deploy.
    ///
    /// Source of truth (Phase 6): the manifest's `[deploy.docker].transport`
    /// field — `sidecar-quic` or `direct-http`. The legacy `transport_explicit`
    /// hint in `user_config` is honoured as an override only when set, which
    /// keeps existing wizard requests working until the GUI stops sending it.
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    fn pick_transport(&self) -> Transport {
        if let Some(hint) = transport_hint(&self.user_config) {
            return match hint.as_str() {
                "direct_http" | "direct-http" => Transport::HttpDirect,
                _ => Transport::SidecarQuic,
            };
        }
        match self
            .manifest
            .deploy
            .docker
            .as_ref()
            .and_then(|d| d.transport)
        {
            Some(DockerTransport::DirectHttp) => Transport::HttpDirect,
            Some(DockerTransport::SidecarQuic) | None => Transport::SidecarQuic,
        }
    }

    /// Host port explicitly requested by the admin in the deploy form
    /// (`config_json.port`). Used as the preferred port for a fresh compose
    /// deploy — honored when free, otherwise the allocator falls back to the
    /// next free one (a busy suggested port never fails the deploy).
    #[cfg_attr(not(feature = "docker"), allow(dead_code))]
    fn requested_port(&self) -> Option<u16> {
        self.user_config
            .get("port")
            .and_then(|v| v.as_u64())
            .filter(|p| (1..=u64::from(u16::MAX)).contains(p))
            .map(|p| p as u16)
    }

    /// Launch a `compose_path` stack (multi-container infra like Milvus /
    /// iroh-relay) via `docker compose up -d --wait`. Each deploy runs as its own
    /// compose PROJECT named `tentaflow-<engine>-<port>`, so N independent
    /// instances coexist on one host — every container/volume/network is
    /// project-prefixed by compose (no sharing). We allocate a free host port for
    /// the engine's API and inject it via `MILVUS_GRPC_PORT` (the stack maps it to
    /// the container's gRPC port). `--wait` blocks until the stack's own
    /// healthchecks pass, so a successful return means it is up.
    #[cfg(feature = "docker")]
    async fn prepare_compose(&mut self, compose_path: &str) -> DeployResult<PreparedDeploy> {
        let compose_file = crate::paths::containers_root().join(compose_path);
        if !compose_file.exists() {
            return Err(DeployError::Manifest(format!(
                "docker compose_path does not exist: {}",
                compose_file.display()
            )));
        }

        // Ports already published by ANY docker container — including ones
        // created outside TentaFlow. `docker ps` reflects the daemon's own port
        // reservations, so this catches conflicts the kernel bind-probe misses
        // when dockerd runs with userland-proxy disabled (iptables DNAT leaves
        // no host listener for our `TcpListener::bind` to trip on).
        let docker_busy = docker_published_host_ports().await;

        // Pick a host port that is free per the allocator (own ledger + kernel
        // probe) AND not published by docker; bring the stack up; retry on a
        // port-allocation conflict (race with a concurrent deploy/container).
        // Ports we skip stay leased during the loop so each turn gets a fresh
        // one; the unused ones are released once we succeed or give up.
        let mut held: Vec<u16> = Vec::new();
        let mut chosen: Option<(u16, String)> = None;
        let mut last_err = String::new();
        // First attempt honors the port the admin chose in the form (or the
        // service's preserved port on respawn); later attempts take the next
        // free one if that was already taken.
        let first_choice = self.preserved_port.or_else(|| self.requested_port());
        for attempt in 0..30usize {
            let preferred = if attempt == 0 { first_choice } else { None };
            let port = match self.ports.acquire_or_specific(preferred) {
                Ok(p) => p,
                Err(e) => {
                    last_err = e.to_string();
                    break;
                }
            };
            // Proactively skip ports docker already owns (unless it is this
            // service's own preserved port, which it legitimately still holds).
            if docker_busy.contains(&port) && Some(port) != self.preserved_port {
                held.push(port);
                continue;
            }
            let project = compose_project_name(&self.manifest.engine.id, port);
            if let Some(s) = &self.log_sink {
                s.info(&format!(
                    "[compose] starting stack {} (project {}, host port {})",
                    compose_file.display(),
                    project,
                    port
                ));
            }
            let output = tokio::process::Command::new("docker")
                .arg("compose")
                .arg("-f")
                .arg(&compose_file)
                .arg("-p")
                .arg(&project)
                .arg("up")
                .arg("-d")
                .arg("--wait")
                .env("MILVUS_GRPC_PORT", port.to_string())
                .output()
                .await
                .map_err(|e| {
                    let _ = self.ports.release(port);
                    for h in &held {
                        let _ = self.ports.release(*h);
                    }
                    DeployError::Spawn(format!(
                        "docker compose up: {e} (is the `docker compose` CLI plugin installed?)"
                    ))
                })?;
            if output.status.success() {
                chosen = Some((port, project));
                break;
            }
            // Always tear down the partial project before retrying / failing.
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let _ = tokio::process::Command::new("docker")
                .args(["compose", "-p", &project, "down"])
                .output()
                .await;
            if is_port_conflict(&stderr) {
                // Hold this port leased so the next acquire skips it; retry.
                held.push(port);
                last_err = stderr.trim().to_string();
                continue;
            }
            // Non-port failure (image pull, bad config, …) — do not retry.
            let _ = self.ports.release(port);
            for h in &held {
                let _ = self.ports.release(*h);
            }
            return Err(DeployError::Spawn(format!(
                "docker compose up failed for '{}': {}",
                self.manifest.engine.id,
                stderr.trim()
            )));
        }

        let (port, project) = match chosen {
            Some(v) => v,
            None => {
                for h in &held {
                    let _ = self.ports.release(*h);
                }
                return Err(DeployError::PortAlloc(format!(
                    "no free host port for '{}' (every candidate was taken by docker or the OS; last: {})",
                    self.manifest.engine.id, last_err
                )));
            }
        };
        // Release the ports we skipped/failed; keep only the chosen one leased.
        for h in &held {
            if *h != port {
                let _ = self.ports.release(*h);
            }
        }

        let transport = self.pick_transport();
        let endpoint_url = Some(build_endpoint_url(
            "127.0.0.1",
            port,
            self.manifest.engine.api,
        ));

        let runtime = RuntimeHandle {
            pid: None,
            port: Some(port),
            sidecar_port: None,
            endpoint_url,
            // Sentinel so `stop()` tears the whole stack down with
            // `docker compose -p <project> down` instead of removing one container.
            container_id: Some(format!("compose:{project}")),
            instance_dir: None,
        };
        let models = models_from_manifest(&self.manifest, &self.user_config);
        // Sekret nigdy do config_json (services.config_json przez commit).
        let config_json = serde_json::to_string(&super::strip_hf_token(&self.user_config))
            .map_err(|e| DeployError::Other(format!("serialize config: {e}")))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::Docker,
            transport,
            runtime,
            models,
            config_json,
            allocated_ports: vec![port],
        })
    }
}

/// Deterministic docker-compose project name for one deployed stack instance.
/// Includes the host port so multiple instances of the same engine get distinct
/// projects, and `stop()` can reconstruct it from `engine_id` + `runtime_port`
/// to run `docker compose -p <name> down`.
#[cfg(feature = "docker")]
pub(super) fn compose_project_name(engine_id: &str, port: u16) -> String {
    let safe: String = engine_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("tentaflow-{safe}-{port}")
}

/// Host ports currently published by any running docker container (incl. ones
/// not managed by TentaFlow). Parsed from `docker ps --format '{{.Ports}}'`,
/// whose entries look like `0.0.0.0:5001->19530/tcp, [::]:5001->19530/tcp`.
/// Reflects the daemon's port reservations regardless of userland-proxy mode.
#[cfg(feature = "docker")]
async fn docker_published_host_ports() -> std::collections::HashSet<u16> {
    let mut out = std::collections::HashSet::new();
    let Ok(o) = tokio::process::Command::new("docker")
        .args(["ps", "--format", "{{.Ports}}"])
        .output()
        .await
    else {
        return out;
    };
    let text = String::from_utf8_lossy(&o.stdout);
    for line in text.lines() {
        for seg in line.split(',') {
            // Only mappings with an explicit host binding ("host:PORT->container").
            if let Some(arrow) = seg.find("->") {
                let left = &seg[..arrow];
                if let Some(colon) = left.rfind(':') {
                    if let Ok(p) = left[colon + 1..].trim().parse::<u16>() {
                        out.insert(p);
                    }
                }
            }
        }
    }
    out
}

/// True when a `docker compose up` failure is a host-port collision (so the
/// caller should retry on a different port rather than abort the deploy).
#[cfg(feature = "docker")]
fn is_port_conflict(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("port is already allocated")
        || s.contains("address already in use")
        || s.contains("bind for") // "Bind for 0.0.0.0:5001 failed: port is already allocated"
        || s.contains("already in use by container")
}

/// True when the host exposes at least one NVIDIA GPU (probed once via
/// `nvidia-smi -L`). Cached so repeated deploys don't re-spawn the probe.
#[cfg(feature = "docker")]
fn host_has_nvidia_gpu() -> bool {
    use std::sync::OnceLock;
    static HAS_GPU: OnceLock<bool> = OnceLock::new();
    *HAS_GPU.get_or_init(|| {
        std::process::Command::new("nvidia-smi")
            .arg("-L")
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
    })
}

/// Resolves how many GPUs to attach to a container (Docker `--gpus`), from the
/// manifest's `[deploy.docker].gpus` field. `Some(-1)` = all, `Some(n)` = n,
/// `None` = no GPU. When the field is absent we default to all GPUs iff the host
/// has an NVIDIA GPU, so AI engines get the device without per-image flags while
/// CPU-only hosts (and CPU services like searxng) simply run without it.
#[cfg(feature = "docker")]
fn resolve_gpu_count(manifest_gpus: Option<&str>) -> Option<i64> {
    match manifest_gpus.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if v == "all" => Some(-1),
        Some(v) if v.is_empty() || v == "none" || v == "0" || v == "false" => None,
        Some(v) => match v.parse::<i64>() {
            Ok(n) if n > 0 => Some(n),
            _ => None,
        },
        None => {
            if host_has_nvidia_gpu() {
                Some(-1)
            } else {
                None
            }
        }
    }
}

#[cfg(feature = "docker")]
mod backend {
    use super::*;
    use bollard::models::{ContainerCreateBody, DeviceRequest, HostConfig, PortBinding};
    use bollard::query_parameters::{
        BuildImageOptions, CreateContainerOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use bollard::Docker;
    use std::collections::HashMap;
    use std::path::Path;

    pub(super) async fn connect() -> DeployResult<Docker> {
        Docker::connect_with_local_defaults()
            .map_err(|e| DeployError::Docker(format!("connect: {}", e)))
    }

    pub(super) async fn ping(docker: &Docker) -> DeployResult<()> {
        docker
            .ping()
            .await
            .map(|_| ())
            .map_err(|e| DeployError::Docker(format!("ping: {}", e)))
    }

    /// Returns true when a tagged image is already present locally.
    pub(super) async fn image_exists(docker: &Docker, tag: &str) -> DeployResult<bool> {
        match docker.inspect_image(tag).await {
            Ok(_) => Ok(true),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(false),
            Err(e) => Err(DeployError::Docker(format!(
                "inspect_image({}): {}",
                tag, e
            ))),
        }
    }

    /// Recursively appends a context directory to the build tar, forcing mode
    /// 0755 on `*.sh` files. The classic builder (bollard uses `/build`, no
    /// BuildKit) does not support `COPY --chmod` and takes file modes from the
    /// tar headers — but the exec bit is unreliable on disk: the containers
    /// bundle ships scripts as 0644 and Windows has no exec bit at all.
    /// Zwraca true dla wpisow ktore nie powinny trafic do tar kontekstu:
    /// ciezkie katalogi cache (`target`, `node_modules`, `.git`, `.build*`),
    /// nigdy nie kopiowane przez Dockerfile'e.
    fn should_skip_context_entry(name: &std::ffi::OsStr) -> bool {
        let Some(name) = name.to_str() else {
            return false;
        };
        matches!(name, "target" | "node_modules" | ".git") || name.starts_with(".build")
    }

    fn append_context_dir(
        builder: &mut tar::Builder<Vec<u8>>,
        dir: &Path,
        prefix: &Path,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            // Pakujemy korzen bundla jako kontekst, wiec pomijamy ciezkie
            // katalogi ktorych zaden Dockerfile nie kopiuje (cache buildow),
            // zeby nie wysylac ich do daemona. Wykluczenia dotycza nazw, wiec
            // nie ruszaja tentaflow-protocol/transport/voice, vendor, sidecar.
            if should_skip_context_entry(&name) {
                continue;
            }
            let rel = prefix.join(&name);
            if path.is_dir() {
                builder.append_dir(&rel, &path)?;
                append_context_dir(builder, &path, &rel)?;
            } else {
                let mut file = std::fs::File::open(&path)?;
                if rel.extension().is_some_and(|e| e == "sh") {
                    let mut header = tar::Header::new_gnu();
                    header.set_metadata(&file.metadata()?);
                    header.set_mode(0o755);
                    builder.append_data(&mut header, &rel, &mut file)?;
                } else {
                    builder.append_file(&rel, &mut file)?;
                }
            }
        }
        Ok(())
    }

    /// Builds an image from `context` jako KORZENIA bundla (tar root). Dockerfile'e
    /// kopiuja sciezki wzgledem korzenia bundla (`tentaflow-protocol`, `vendor`,
    /// `tentaflow-containers/...`), wiec kontekstem jest korzen bundla, a `dockerfile_rel`
    /// wskazuje plik Dockerfile pod podscieszka (`tentaflow-containers/<cat>/docker/<eng>/Dockerfile`).
    /// Streams build log lines into `log` (when present).
    pub(super) async fn build_image_from_context(
        docker: &Docker,
        context: &Path,
        dockerfile_rel: &str,
        tag: &str,
        log: Option<&LogSink>,
    ) -> DeployResult<()> {
        use futures::StreamExt;

        if !context.is_dir() {
            return Err(DeployError::Manifest(format!(
                "docker context not a directory: {}",
                context.display()
            )));
        }

        // Pack the bundle root as the tar root so Dockerfile COPY paths resolve
        // against it. Bollard streams this body as the build context.
        let mut tar_builder = tar::Builder::new(Vec::new());
        append_context_dir(&mut tar_builder, context, Path::new(""))
            .map_err(|e| DeployError::Docker(format!("tar context: {}", e)))?;
        let tar_bytes = tar_builder
            .into_inner()
            .map_err(|e| DeployError::Docker(format!("tar finalize: {}", e)))?;

        let opts = BuildImageOptions {
            dockerfile: dockerfile_rel.to_string(),
            t: Some(tag.to_string()),
            rm: true,
            ..Default::default()
        };

        use bollard::body_full;
        use hyper::body::Bytes;
        let body = body_full(Bytes::from(tar_bytes));
        let mut stream = docker.build_image(opts, None, Some(body));

        let emit = |msg: &str| {
            if let Some(s) = log {
                s.info(msg);
            } else {
                tracing::info!(target: "docker_build", "{}", msg);
            }
        };

        // Heartbeat: the classic builder relays a long RUN step's stdout, but
        // tools that draw progress with `\r` (pip, cmake/ninja, nvcc, git clone)
        // emit no newline for minutes, so the stream goes silent mid-step. Park
        // on `stream.next()` AND a timer so we can surface a liveness line during
        // those silent stretches — otherwise CUDA kernel compiles look frozen.
        let start = std::time::Instant::now();
        let mut last_output = std::time::Instant::now();
        let mut current_step = String::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
        ticker.tick().await; // first tick fires immediately — drop it

        loop {
            tokio::select! {
                item = stream.next() => {
                    let Some(item) = item else { break };
                    match item {
                        Ok(info) => {
                            let mut emitted = false;
                            if let Some(line) = info.stream {
                                let trimmed = line.trim_end();
                                if !trimmed.is_empty() {
                                    if trimmed.starts_with("Step ") {
                                        current_step = trimmed.to_string();
                                    }
                                    emit(&format!("[docker build] {}", trimmed));
                                    emitted = true;
                                }
                            }
                            // Pull/extract phases arrive as status+progress, not
                            // stream — forward them so downloads aren't invisible.
                            if !emitted {
                                if let Some(status) = info.status {
                                    let status = status.trim();
                                    if !status.is_empty() {
                                        let detail = info.progress_detail.and_then(|p| {
                                            match (p.current, p.total) {
                                                (Some(c), Some(t)) if t > 0 => {
                                                    Some(format!(" {}/{}", c, t))
                                                }
                                                _ => None,
                                            }
                                        });
                                        match detail {
                                            Some(d) => emit(&format!("[docker build] {}{}", status, d)),
                                            None => emit(&format!("[docker build] {}", status)),
                                        }
                                        emitted = true;
                                    }
                                }
                            }
                            if emitted {
                                last_output = std::time::Instant::now();
                            }
                            if let Some(err_detail) = info.error_detail {
                                return Err(DeployError::Docker(format!(
                                    "build error: {}",
                                    err_detail.message.unwrap_or_default()
                                )));
                            }
                        }
                        Err(e) => return Err(DeployError::Docker(format!("build stream: {}", e))),
                    }
                }
                _ = ticker.tick() => {
                    if last_output.elapsed().as_secs() >= 15 {
                        let step = if current_step.is_empty() {
                            "build w toku".to_string()
                        } else {
                            current_step.clone()
                        };
                        emit(&format!(
                            "[docker build] … {} — pracuje ({}s ciszy, {}s łącznie)",
                            step,
                            last_output.elapsed().as_secs(),
                            start.elapsed().as_secs()
                        ));
                        last_output = std::time::Instant::now();
                    }
                }
            }
        }
        Ok(())
    }

    /// Creates and starts a container. Returns container id.
    /// `binds` entries: (host_path, container_path, read_only).
    pub(super) async fn run(
        docker: &Docker,
        image: &str,
        name: &str,
        ports: &[(u16, u16, &str)], // (host, container, proto: "tcp"|"udp")
        env: &HashMap<String, String>,
        cmd: &[String], // dolaczane do ENTRYPOINT jako "$@" (argv silnika)
        binds: &[(PathBuf, String, bool)],
        labels: &HashMap<String, String>,
        gpu_count: Option<i64>, // Some(-1)=all GPUs, Some(n)=n GPUs, None=no GPU
    ) -> DeployResult<String> {
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed: Vec<String> = Vec::new();
        for (host, ctr, proto) in ports {
            let key = format!("{}/{}", ctr, proto);
            port_bindings.insert(
                key.clone(),
                Some(vec![PortBinding {
                    host_ip: Some("0.0.0.0".into()),
                    host_port: Some(host.to_string()),
                }]),
            );
            exposed.push(key);
        }

        let env_vec: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        let binds_vec: Vec<String> = binds
            .iter()
            .map(|(h, c, ro)| {
                if *ro {
                    format!("{}:{}:ro", h.display(), c)
                } else {
                    format!("{}:{}", h.display(), c)
                }
            })
            .collect();

        // GPU passthrough — equivalent of `docker run --gpus`. Empty driver +
        // `["gpu"]` capability is what the CLI sends for `--gpus`, letting Docker
        // pick the NVIDIA device-request driver (named `nvidia` runtime is not
        // registered on this host, but the device-request path works).
        let device_requests = gpu_count.map(|count| {
            vec![DeviceRequest {
                driver: None,
                count: Some(count),
                device_ids: None,
                capabilities: Some(vec![vec!["gpu".to_string()]]),
                options: None,
            }]
        });

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            binds: if binds_vec.is_empty() {
                None
            } else {
                Some(binds_vec)
            },
            device_requests,
            ..Default::default()
        };
        let body = ContainerCreateBody {
            image: Some(image.into()),
            cmd: if cmd.is_empty() {
                None
            } else {
                Some(cmd.to_vec())
            },
            env: if env_vec.is_empty() {
                None
            } else {
                Some(env_vec)
            },
            exposed_ports: if exposed.is_empty() {
                None
            } else {
                Some(exposed)
            },
            labels: if labels.is_empty() {
                None
            } else {
                Some(labels.clone())
            },
            host_config: Some(host_config),
            ..Default::default()
        };
        let opts = CreateContainerOptions {
            name: Some(name.into()),
            platform: String::new(),
        };
        // Best-effort cleanup of an old container with the same name.
        let _ = docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let created = docker
            .create_container(Some(opts), body)
            .await
            .map_err(|e| DeployError::Docker(format!("create: {}", e)))?;
        docker
            .start_container(name, None::<StartContainerOptions>)
            .await
            .map_err(|e| DeployError::Docker(format!("start: {}", e)))?;
        Ok(created.id)
    }

    pub(super) async fn stop_and_remove(docker: &Docker, id: &str) -> DeployResult<()> {
        let _ = docker.stop_container(id, None).await;
        docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| DeployError::Docker(format!("remove: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl DeployStrategy for DockerDeploy {
    #[cfg(feature = "docker")]
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        use std::collections::HashMap;
        let docker_section = self.manifest.deploy.docker.as_ref().ok_or_else(|| {
            DeployError::Manifest(format!(
                "engine '{}' has no [deploy.docker]",
                self.manifest.engine.id
            ))
        })?;
        // Multi-container engines (infra stacks like Milvus / iroh-relay) declare
        // `compose_path` instead of `context_path`: launch the whole stack via
        // `docker compose up` rather than building a single image.
        let context_path = match docker_section.context_path.clone() {
            Some(p) => p,
            None => {
                let compose_path = docker_section.compose_path.clone().ok_or_else(|| {
                    DeployError::Manifest("docker deploy needs context_path or compose_path".into())
                })?;
                return self.prepare_compose(&compose_path).await;
            }
        };
        let context_path = context_path.as_str();

        let docker = backend::connect().await?;
        backend::ping(&docker).await?;

        // Walidacja: podkatalog silnika musi istniec (czytelny blad gdy context_path zly).
        let context_dir = crate::paths::containers_root().join(context_path);
        if !context_dir.exists() {
            return Err(DeployError::Manifest(format!(
                "docker context_path does not exist: {}",
                context_dir.display()
            )));
        }
        // Kontekstem budowania jest KORZEN bundla (rodzic `tentaflow-containers/`),
        // bo Dockerfile'e kopiuja sciezki wzgledem korzenia bundla. Dockerfile
        // lezy pod podscieszka silnika.
        let bundle_root = crate::paths::containers_root()
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| {
                DeployError::Manifest(
                    "cannot resolve bundle root (containers_root has no parent)".into(),
                )
            })?;
        let dockerfile_rel = format!("tentaflow-containers/{}/Dockerfile", context_path);
        let image_tag = format!(
            "tentaflow/{}:{}",
            self.manifest.engine.id, self.manifest.engine.version
        );

        // Build only when missing — repeated deploys reuse the cached image.
        if !backend::image_exists(&docker, &image_tag).await? {
            if let Some(s) = &self.log_sink {
                s.info(&format!(
                    "[docker] building image {} from {} (dockerfile {})",
                    image_tag,
                    bundle_root.display(),
                    dockerfile_rel
                ));
            }
            backend::build_image_from_context(
                &docker,
                &bundle_root,
                &dockerfile_rel,
                &image_tag,
                self.log_sink.as_ref(),
            )
            .await?;
        }

        let transport = self.pick_transport();
        let internal_port = self.manifest.engine.default_port;
        let mut allocated = Vec::new();

        // Allocate ports. Respawn istniejacego serwisu zachowuje port z DB
        // (preserved_port). Dla SidecarQuic preserved_port to host_http;
        // sidecar_quic_port jest zawsze swiezy bo nie trzymamy go w DB
        // jako stable identifier (rzadko exposed do klientow).
        let (host_http, sidecar_quic) = if transport == Transport::SidecarQuic {
            let http = self
                .ports
                .acquire_or_specific(self.preserved_port)
                .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
            let quic = self
                .ports
                .acquire()
                .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
            allocated.push(http);
            allocated.push(quic);
            (http, Some(quic))
        } else {
            let p = self
                .ports
                .acquire_or_specific(self.preserved_port)
                .map_err(|e| DeployError::PortAlloc(e.to_string()))?;
            allocated.push(p);
            (p, None)
        };

        let (param_app, request_time) = super::apply_parameters_deploy(
            &self.manifest,
            &self.user_config,
            super::DeployTarget::Docker,
        )
        .map_err(|e| DeployError::Manifest(format!("apply parameters: {}", e)))?;

        // Build env / labels.
        let mut env = super::standard_engine_env();
        for (k, v) in param_app.env {
            env.insert(k, v);
        }
        env.insert("PORT".into(), internal_port.to_string());
        env.insert("VLLM_PORT".into(), internal_port.to_string());
        if let Some(model) = super::resolve_model_repo(&self.manifest, &self.user_config) {
            env.insert("MODEL".into(), model);
        }
        // vLLM featured presets (Bielik NVFP4 + draft, Qwen MTP): NVFP4
        // self-quant + speculative-config env the bare MODEL repo can't carry.
        // HF_TOKEN wstrzykujemy TYLKO gdy deploy realnie rozwiazuje model HF —
        // silniki infra (searxng, browser-renderer) nie maja modelu i nie moga
        // widziec sekretu w env. Speculative/quantize env leci niezaleznie.
        let hf_token_for_env = if super::engine_uses_hf_model(&self.manifest, &self.user_config) {
            self.hf_token.as_deref()
        } else {
            None
        };
        for (k, v) in super::vllm_deploy_env(&self.manifest, &self.user_config, hf_token_for_env) {
            env.insert(k, v);
        }
        // Recipe / user engine env passthrough (e.g. VLLM_USE_FLASHINFER_MOE_FP4).
        super::apply_engine_env(&self.user_config, &mut env);
        // See python_bundle.rs: the served name must equal the advertised slug
        // (`models_from_manifest` model_name) or dispatch 404s when preset id
        // differs from the repo. entrypoint.sh reads `$SERVED_MODEL_NAME`.
        if let Some(served) = super::resolve_served_model_name(&self.manifest, &self.user_config) {
            env.insert("SERVED_MODEL_NAME".into(), served);
        }
        // Argumenty CLI silnika budowane jako strukturalny Vec<String> i
        // przekazywane do kontenera jako bollard `Cmd` (array) → entrypoint
        // odbiera je jako `"$@"`. Nie ma round-tripu przez stringowy VLLM_ARGS
        // env + xargs, wiec kompaktowy JSON `--speculative-config {...}` plynie
        // jako pojedynczy nietkniety element. Identyczna sciezka jak native.
        let mut engine_args: Vec<String> = Vec::new();
        // Native bierze ten baseline z bundle.toml [launch] args; docker nie ma
        // bundle.toml, wiec musimy zasiac te same defaulty Rust-side jako POCZATEK
        // argv. Bez tego kontener startuje vLLM z pelnokontekstowymi defaultami
        // (brak --max-model-len / --max-num-batched-tokens) → OOM. Baseline idzie
        // PRZED user/spec/gpu, wiec dedup_cli_args_last_wins pozwala je nadpisac.
        engine_args.extend(vllm_docker_baseline_args(&self.manifest.engine.id));
        // User-typed `vllm_args` (wizard Advanced) — user sam cytuje, wiec
        // shlex split jest poprawny dla niego (np. JSON w single-quotes).
        if let Some(raw_args) = self
            .user_config
            .get("vllm_args")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            match shlex::split(raw_args) {
                Some(parts) => engine_args.extend(parts),
                None => engine_args.extend(raw_args.split_whitespace().map(String::from)),
            }
        }
        // Speculative-config: flaga + JSON jako dwa osobne elementy argv.
        if let Some(spec_args) =
            super::vllm_native_speculative_arg(&self.manifest, &self.user_config)
        {
            engine_args.extend(spec_args);
        }
        if super::is_cuda_vllm_engine(&self.manifest.engine.id) {
            let user_explicit_ratio = self
                .user_config
                .get("gpu_memory_utilization")
                .and_then(|v| v.as_f64());
            let from_args = super::parse_gpu_memory_utilization_arg(&engine_args.join(" "));
            let ratio = user_explicit_ratio
                .or(from_args)
                .or_else(super::auto_gpu_memory_utilization);
            if let Some(ratio) = ratio {
                engine_args.push("--gpu-memory-utilization".to_string());
                engine_args.push(format!("{:.2}", ratio));
                env.insert("GPU_MEMORY_UTILIZATION".into(), format!("{:.2}", ratio));
                if let Some(s) = &self.log_sink {
                    s.info(&format!("[docker] gpu_memory_utilization={:.2}", ratio));
                }
            }
            if self.manifest.engine.id == "vllm-spark" {
                // Spark wymaga wylaczenia flashinfer autotune; dedup last-wins
                // skasuje ewentualny `--enable-...` z user args.
                engine_args.push("--no-enable-flashinfer-autotune".to_string());
            }
        }
        // Dedup last-wins (extra/user args wygrywaja nad bundle/manifest base).
        // entrypoint.sh dorzuca tylko AUTO_PARALLEL gdy brak TP/PP w tych argach.
        let engine_args = crate::deploy::python_venv::dedup_cli_args_last_wins(engine_args);

        let mut labels = HashMap::new();
        labels.insert(
            "tentaflow.engine_id".to_string(),
            self.manifest.engine.id.clone(),
        );

        let mut port_map = vec![(host_http, internal_port, "tcp")];
        if let Some(q) = sidecar_quic {
            port_map.push((q, q, "udp"));
        }

        let container_name = format!("tentaflow-{}-{}", self.manifest.engine.id, host_http);
        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[docker] starting container '{}' image={} host_port={}",
                container_name, image_tag, host_http
            ));
        }

        // Mount the shared host models cache so HF / Torch downloads from a
        // Docker engine end up in the same place as native deploys. Read-write
        // because the container is the one populating the cache.
        let models_host = crate::paths::models_root();
        let _ = std::fs::create_dir_all(&models_host);
        // vLLM cache (Triton kernels, torch.compile, FlashInfer JIT). Kept on
        // the host so a docker rebuild / container restart doesn't trigger a
        // 1-2 min recompile. Mounted at `CONTAINER_VLLM_CACHE_PATH`, paired
        // with the `VLLM_CACHE_ROOT` env from `standard_engine_env`.
        let vllm_cache_host = crate::paths::vllm_cache_dir();
        let _ = std::fs::create_dir_all(&vllm_cache_host);
        let binds = vec![
            (
                models_host,
                crate::paths::CONTAINER_MODELS_PATH.to_string(),
                false,
            ),
            (
                vllm_cache_host,
                crate::paths::CONTAINER_VLLM_CACHE_PATH.to_string(),
                false,
            ),
        ];

        let gpu_count = resolve_gpu_count(
            self.manifest
                .deploy
                .docker
                .as_ref()
                .and_then(|d| d.gpus.as_deref()),
        );
        if let Some(s) = &self.log_sink {
            match gpu_count {
                Some(c) => s.info(&format!(
                    "[docker] GPU passthrough: {}",
                    if c < 0 {
                        "all".to_string()
                    } else {
                        c.to_string()
                    }
                )),
                None => s.info("[docker] GPU passthrough: none (CPU)"),
            }
        }

        let id = backend::run(
            &docker,
            &image_tag,
            &container_name,
            &port_map,
            &env,
            &engine_args,
            &binds,
            &labels,
            gpu_count,
        )
        .await?;

        // Save id for rollback.
        if let Ok(mut slot) = self.container_id.lock() {
            *slot = Some(id.clone());
        }
        if let Some(s) = &self.log_sink {
            s.info(&format!(
                "[docker] container '{}' started (id={})",
                container_name,
                &id[..id.len().min(12)]
            ));
        }

        // Stream container logs into the dashboard sink. Background task
        // ends when the container stops or the daemon closes the stream.
        {
            let docker_for_logs = docker.clone();
            let name_for_logs = container_name.clone();
            let sink = self.log_sink.clone();
            tokio::spawn(async move {
                use futures::StreamExt;
                let opts = bollard::query_parameters::LogsOptionsBuilder::default()
                    .follow(true)
                    .stdout(true)
                    .stderr(true)
                    .tail("0")
                    .build();
                let mut stream = docker_for_logs.logs(&name_for_logs, Some(opts));
                while let Some(item) = stream.next().await {
                    if let Ok(out) = item {
                        let line = out.to_string();
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(s) = &sink {
                            s.emit("log", trimmed);
                        }
                    }
                }
            });
        }

        // Smart probe: race readiness URLs forever, abort only on
        // container exit.
        let probe_cfg = SmartProbeConfig {
            readiness_urls: vec![
                format!("http://127.0.0.1:{}/v1/models", host_http),
                format!("http://127.0.0.1:{}/health", host_http),
            ],
            status_report_interval: std::time::Duration::from_secs(30),
            log_sink: self.log_sink.clone(),
            // Brak hard timeoutu — docker container exit (CUDA OOM,
            // OOMKilled przez kernel cgroups itp.) flag'uje jako
            // Failed natychmiast. Bez timeoutu zeby duze modele
            // (70B+, multi-GB HF download) mogly sie ladowac dluzej.
            max_wait: None,
        };
        let docker_for_probe = docker.clone();
        let name_for_probe = container_name.clone();
        let outcome = smart_health_probe(probe_cfg, move || {
            let d = docker_for_probe.clone();
            let n = name_for_probe.clone();
            async move {
                match d.inspect_container(&n, None).await {
                    Ok(info) => {
                        let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                        if running {
                            None
                        } else {
                            // Exited — surface the exit code if Docker
                            // reported one.
                            let code = info
                                .state
                                .as_ref()
                                .and_then(|s| s.exit_code)
                                .map(|c| c as i32);
                            Some(code)
                        }
                    }
                    // Inspect failed — likely the container vanished.
                    Err(_) => Some(None),
                }
            }
        })
        .await;

        match outcome {
            SmartProbeOutcome::Ready => {}
            SmartProbeOutcome::ProcessExited(code) => {
                if let Some(s) = &self.log_sink {
                    s.info(&format!(
                        "[docker] container '{}' exited{} before becoming ready",
                        container_name,
                        code.map(|c| format!(" (code {})", c)).unwrap_or_default()
                    ));
                }
                let _ = backend::stop_and_remove(&docker, &id).await;
                for p in &allocated {
                    let _ = self.ports.release(*p);
                }
                return Err(DeployError::Spawn(format!(
                    "container '{}' exited before readiness",
                    container_name
                )));
            }
        }

        let endpoint_url = match transport {
            Transport::SidecarQuic => Some(format!("quic://127.0.0.1:{}", sidecar_quic.unwrap())),
            Transport::HttpDirect => Some(build_endpoint_url(
                "127.0.0.1",
                host_http,
                self.manifest.engine.api,
            )),
            _ => None,
        };

        let runtime = RuntimeHandle {
            pid: None,
            port: Some(host_http),
            sidecar_port: sidecar_quic,
            endpoint_url,
            container_id: Some(id),
            instance_dir: None,
        };
        let models = models_from_manifest(&self.manifest, &self.user_config);
        // Typed schema params + request_time → config_json. Docker silniki
        // konsumuja env binding (vllm/sglang/tensorrt-llm — env do
        // entrypoint.sh) plus opcjonalnie request-time (gdy api jest
        // OpenAI-compat, materializuje sie przez BackendClient).
        let config_json = super::merge_config_json(&self.user_config, &request_time)
            .map_err(|e| DeployError::Other(format!("serialize config: {}", e)))?;

        Ok(PreparedDeploy {
            engine_id: self.manifest.engine.id.clone(),
            category: category_tag(&self.manifest).to_string(),
            display_name: resolve_display_name(&self.manifest),
            deploy_method: DeployMethod::Docker,
            transport,
            runtime,
            models,
            config_json,
            allocated_ports: allocated,
        })
    }

    #[cfg(not(feature = "docker"))]
    async fn prepare(&mut self) -> DeployResult<PreparedDeploy> {
        Err(DeployError::Docker(
            "tentaflow-core compiled without `docker` feature".into(),
        ))
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

    #[cfg(feature = "docker")]
    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()> {
        let id = self
            .container_id
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(id) = id {
            if let Ok(docker) = backend::connect().await {
                let _ = backend::stop_and_remove(&docker, &id).await;
            }
        }
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        Ok(())
    }

    #[cfg(not(feature = "docker"))]
    async fn rollback(&self, prepared: PreparedDeploy) -> DeployResult<()> {
        for p in &prepared.allocated_ports {
            let _ = self.ports.release(*p);
        }
        Ok(())
    }
}

/// Baseline argv dla silnikow vLLM przy deployu docker. Native bierze te same
/// flagi z bundle.toml `[launch] args`; docker bundle.toml nie ma, wiec
/// odtwarzamy je tu jako single source of truth Rust-side. Dotyczy WYLACZNIE
/// rodziny vLLM (vllm / vllm-spark / vllm-metal) — sglang / llama.cpp / trt
/// maja inny zestaw flag i nie dostaja tego baseline. `--enable-flashinfer-autotune`
/// jest w baseline; dla vllm-spark `prepare` dorzuca pozniej
/// `--no-enable-flashinfer-autotune`, ktore wygrywa przez dedup last-wins.
#[cfg(feature = "docker")]
fn vllm_docker_baseline_args(engine_id: &str) -> Vec<String> {
    if !matches!(engine_id, "vllm" | "vllm-spark" | "vllm-metal") {
        return Vec::new();
    }
    [
        "--dtype",
        "auto",
        "--max-model-len",
        "8192",
        "--max-num-batched-tokens",
        "8192",
        "--enable-prefix-caching",
        "--enable-chunked-prefill",
        "--enable-flashinfer-autotune",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn skeleton_manifest(id: &str) -> ServiceManifest {
        use crate::services::manifest::{
            ApiKind, Category, DeploySection, DockerDeploy as DockerSec, DockerTransport, Engine,
            TargetOs,
        };
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
                default_port: 8000,
                dgx_spark: None,
                api: ApiKind::OpenaiCompatible,
                version: "0".into(),
                service_surfaces: None,
                input_modalities: None,
                output_modalities: None,
            },
            deploy: DeploySection {
                docker: Some(DockerSec {
                    context_path: Some("/nonexistent/ctx".into()),
                    compose_path: None,
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    download_image: None,
                    download_size_mb: None,
                    transport: Some(DockerTransport::SidecarQuic),
                    gpus: None,
                }),
                native: None,
                external: None,
            },
            model_presets: vec![],
            parameters: vec![],
            docker_source_hash: String::new(),
            native_source_hash: String::new(),
        }
    }

    /// Without the `docker` feature compiled in, prepare must return an error.
    #[cfg(not(feature = "docker"))]
    #[tokio::test]
    async fn prepare_errors_without_docker_feature() {
        let m = skeleton_manifest("no-docker");
        let ports = Arc::new(PortAllocator::new((48_500, 48_510), HashSet::new()).unwrap());
        let mut s = DockerDeploy::new(m, serde_json::json!({}), ports, None, None);
        let err = s.prepare().await.unwrap_err();
        assert!(matches!(err, DeployError::Docker(_)));
    }

    #[test]
    fn pick_transport_default_is_sidecar_quic() {
        let m = skeleton_manifest("def");
        let ports = Arc::new(PortAllocator::new((48_600, 48_610), HashSet::new()).unwrap());
        let s = DockerDeploy::new(m, serde_json::json!({}), ports, None, None);
        assert_eq!(s.pick_transport(), Transport::SidecarQuic);
    }

    #[test]
    fn pick_transport_honors_direct_http_hint() {
        let m = skeleton_manifest("hint");
        let ports = Arc::new(PortAllocator::new((48_700, 48_710), HashSet::new()).unwrap());
        let s = DockerDeploy::new(
            m,
            serde_json::json!({"transport_explicit": "direct_http"}),
            ports,
            None,
            None,
        );
        assert_eq!(s.pick_transport(), Transport::HttpDirect);
    }

    /// Bez user-args silnik vllm musi dostac komplet baseline flag (te same co
    /// native z bundle.toml) zasiane Rust-side, inaczej kontener leci na
    /// pelnokontekstowych defaultach vLLM → OOM.
    #[cfg(feature = "docker")]
    #[test]
    fn docker_baseline_seeded_for_vllm() {
        let base = vllm_docker_baseline_args("vllm");
        let args = crate::deploy::python_venv::dedup_cli_args_last_wins(base);
        let joined = args.join(" ");
        assert!(joined.contains("--dtype auto"), "got: {joined}");
        assert!(joined.contains("--max-model-len 8192"), "got: {joined}");
        assert!(
            joined.contains("--max-num-batched-tokens 8192"),
            "got: {joined}"
        );
        assert!(args.iter().any(|a| a == "--enable-prefix-caching"));
        assert!(args.iter().any(|a| a == "--enable-chunked-prefill"));
        assert!(args.iter().any(|a| a == "--enable-flashinfer-autotune"));
    }

    /// vllm-spark: baseline ma `--enable-flashinfer-autotune`, ale prepare
    /// dorzuca `--no-enable-flashinfer-autotune`; dedup last-wins musi zostawic
    /// WYLACZONY autotune (spark wymaga off) i zero duplikatow tej flagi.
    #[cfg(feature = "docker")]
    #[test]
    fn docker_spark_disables_flashinfer_autotune_after_dedup() {
        let mut base = vllm_docker_baseline_args("vllm-spark");
        // Symuluj normalizacje z prepare (engine_args.push po user/spec/gpu).
        base.push("--no-enable-flashinfer-autotune".to_string());
        let args = crate::deploy::python_venv::dedup_cli_args_last_wins(base);
        assert!(
            args.iter().any(|a| a == "--no-enable-flashinfer-autotune"),
            "autotune musi byc wylaczony: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--enable-flashinfer-autotune"),
            "duplikat enable nie moze zostac: {args:?}"
        );
    }

    /// Silniki spoza rodziny vLLM nie dostaja vllm baseline.
    #[cfg(feature = "docker")]
    #[test]
    fn docker_baseline_empty_for_non_vllm() {
        assert!(vllm_docker_baseline_args("sglang").is_empty());
        assert!(vllm_docker_baseline_args("llama-cpp").is_empty());
        assert!(vllm_docker_baseline_args("trt-llm").is_empty());
        assert!(!vllm_docker_baseline_args("vllm-metal").is_empty());
    }

    /// Live docker test — gated on a running daemon. Skipped silently when
    /// docker isn't reachable (CI without privileges, sandboxed builds).
    #[cfg(feature = "docker")]
    #[tokio::test]
    #[ignore]
    async fn docker_daemon_reachable_for_live_tests() {
        let docker = match super::backend::connect().await {
            Ok(d) => d,
            Err(_) => return,
        };
        let _ = super::backend::ping(&docker).await;
    }
}
