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

/// Fixed container-side QUIC port the sidecar binds (its baked
/// `config.default.toml [transport].port`). The host-allocated sidecar port maps
/// to this inside the container.
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
const SIDECAR_QUIC_CONTAINER_PORT: u16 = 5000;

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

/// Tag obrazu, z którego URUCHOMIONY jest kontener danego serwisu (np.
/// `tentaflow/nemotron-page-elements:v3-30cf3e621865`). ŹRÓDŁO PRAWDY co
/// faktycznie biegnie na węźle — od fixu tagów deploy zaszywa w nim 12-hex
/// `docker_source_hash`, więc reconcile może zsynchronizować
/// `deployed_source_hash` z REALNYM obrazem (stary deploy zapisywał baked hash
/// nie przebudowując flat-tagowanego obrazu → badge update nigdy nie wracał).
///
/// `ContainerSummary.image` niesie napis tagu (a NIE digest jak top-level
/// `Image` w inspect), dlatego listujemy po nazwie zamiast `inspect_container`.
/// Zwraca `None` gdy daemon nieosiągalny lub brak running kontenera o tej
/// nazwie (serwis zatrzymany / single-container nie istnieje). Nazwa kontenera
/// to `tentaflow-<engine_id>-<host_port>` (jak w `run()` i `stop_checked`).
#[cfg(feature = "docker")]
pub(crate) async fn running_container_image_tag(engine_id: &str, host_port: u16) -> Option<String> {
    let docker = backend::connect().await.ok()?;
    let expected = format!("tentaflow-{}-{}", engine_id, host_port);
    let listed = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
        .ok()?;
    listed.into_iter().find_map(|c| {
        // Tylko running — zatrzymany/exited kontener nie reprezentuje tego, co
        // realnie obsługuje ruch; jego stary tag nie powinien sterować badge.
        let running = matches!(
            c.state,
            Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
        );
        if !running {
            return None;
        }
        // Nazwy w bollard mają wiodący `/`; normalizujemy przed porównaniem.
        let matches_name = c
            .names
            .as_ref()
            .map(|ns| ns.iter().any(|n| n.trim_start_matches('/') == expected))
            .unwrap_or(false);
        if matches_name {
            c.image
        } else {
            None
        }
    })
}

/// Wyłuskuje 12-hex `docker_source_hash` z ostatniego segmentu tagu obrazu.
/// Tag może być `<version>`, `<version>-<arch>` (np. `-sm86`),
/// `<version>-<hash>` albo `<version>-<arch>-<hash>`. Hash to ZAWSZE ostatni
/// segment pasujący do `-[0-9a-f]{12}$`; arch-tag (`-sm86`) nie jest 12-hex,
/// więc nie zostanie złapany (flat-bez-hasha → `None` → stary obraz → badge).
#[cfg(feature = "docker")]
pub(crate) fn hash12_from_image_tag(image: &str) -> Option<String> {
    // Tag to część po OSTATNIM `:` (repo może zawierać port rejestru z `:`).
    let tag = image.rsplit(':').next()?;
    let last = tag.rsplit('-').next()?;
    if last.len() == 12
        && last
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Some(last.to_string())
    } else {
        None
    }
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

/// Wybór GPU dla kontenera. `Devices` przekazuje konkretne indeksy kart, dzieki
/// czemu `nvidia-smi -L` w kontenerze widzi tylko je i auto-tensor-parallel w
/// entrypoincie liczy sie poprawnie zamiast brac wszystkie karty hosta.
#[cfg(feature = "docker")]
#[derive(Debug, Clone, PartialEq)]
enum GpuSelection {
    None,
    Count(i64),
    Devices(Vec<String>),
}

/// Wybor GPU pochodzi z kreatora deploy (`gpu_select_mode` + `gpu_ids` w
/// config_json), bo to operator decyduje ktore karty dostaje kontener. Manifest
/// sluzy tylko jako fallback, gdy kreator nie przeslal wyboru — np. serwisy
/// CPU-only (searxng) nie pokazuja kroku GPU i ida sciezka manifestu.
#[cfg(feature = "docker")]
fn resolve_gpu_selection(
    user_config: &serde_json::Value,
    manifest_gpus: Option<&str>,
) -> GpuSelection {
    match user_config.get("gpu_select_mode").and_then(|v| v.as_str()) {
        Some("none") => GpuSelection::None,
        Some("all") => GpuSelection::Count(-1),
        Some("specific") => {
            let ids: Vec<String> = user_config
                .get("gpu_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| {
                            // Kreator moze przeslac indeksy jako liczby lub stringi.
                            if let Some(n) = v.as_u64() {
                                Some(n.to_string())
                            } else {
                                v.as_str()
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_string)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if ids.is_empty() {
                gpu_count_to_selection(resolve_gpu_count(manifest_gpus))
            } else {
                GpuSelection::Devices(ids)
            }
        }
        _ => gpu_count_to_selection(resolve_gpu_count(manifest_gpus)),
    }
}

#[cfg(feature = "docker")]
fn gpu_count_to_selection(count: Option<i64>) -> GpuSelection {
    match count {
        Some(c) => GpuSelection::Count(c),
        None => GpuSelection::None,
    }
}

/// Klucz w `user_config` niosacy konfiguracje distributed-deployu (multi-node TP).
/// Obecnosc bloku przelacza `DockerDeploy::prepare` na sciezke host-networking +
/// RDMA, a komenda `ray start ... && vllm serve ...` i NCCL env przychodza przez
/// istniejace pola `launch_command_override` + `engine_env` (zero nowej sciezki env).
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
pub(super) const DISTRIBUTED_CONFIG_KEY: &str = "_distributed";

/// Etykieta docker grupujaca kontenery jednego distributed-deploymentu — fallback
/// teardownu gdy wiersza serwisu brak.
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
pub(super) const DISTRIBUTED_LABEL: &str = "tentaflow.deployment_cluster_id";

/// Runtime distributed sciagniety z `_distributed` bloku `user_config`.
#[cfg(feature = "docker")]
#[derive(Debug, Clone)]
struct DistributedRuntime {
    /// "head" | "worker".
    role: String,
    /// Port OpenAI head-a (head nasluchuje; worker headless — port tylko do nazwy
    /// kontenera, bez bindu, bo i tak na innym nodzie).
    port: u16,
    /// Port mastera torch.distributed (TCPStore) → `VLLM_PORT`. Przydzielony z tej
    /// samej puli co serve, rozny od `port`, zeby vLLM nie kolidowal z domyslnym 8000.
    dist_port: u16,
    deployment_cluster_id: String,
}

#[cfg(feature = "docker")]
fn parse_distributed(user_config: &serde_json::Value) -> Option<DistributedRuntime> {
    let d = user_config.get(DISTRIBUTED_CONFIG_KEY)?;
    let role = d.get("role").and_then(|v| v.as_str())?.to_string();
    let port = d.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let dist_port = d.get("dist_port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let deployment_cluster_id = d
        .get("deployment_cluster_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(DistributedRuntime {
        role,
        port,
        dist_port,
        deployment_cluster_id,
    })
}

/// Readiness headless workera Ray (`ray start --address ... --block`). Worker nie
/// wystawia HTTP, wiec gotowosc = kontener zyje nieprzerwanie przez `grace`
/// (czas na dolaczenie do GCS Ray). Exit przed `grace` => deploy-fatal.
#[cfg(feature = "docker")]
async fn wait_worker_alive_grace(
    docker: &bollard::Docker,
    name: &str,
    grace: std::time::Duration,
    log: Option<&LogSink>,
) -> SmartProbeOutcome {
    use std::time::Instant;
    let started = Instant::now();
    let probe = std::time::Duration::from_millis(500);
    loop {
        match docker.inspect_container(name, None).await {
            Ok(info) => {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                if !running {
                    let code = info
                        .state
                        .as_ref()
                        .and_then(|s| s.exit_code)
                        .map(|c| c as i32);
                    return SmartProbeOutcome::ProcessExited(code);
                }
            }
            Err(_) => return SmartProbeOutcome::ProcessExited(None),
        }
        if started.elapsed() >= grace {
            if let Some(s) = log {
                s.info("[docker] ray worker dolaczyl do klastra (alive grace ok)");
            }
            return SmartProbeOutcome::Ready;
        }
        tokio::time::sleep(probe).await;
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

    /// Picks the FIRST locally-present base image tag from `candidates`. Used by
    /// the distributed vllm-spark thin-image build: the cienki obraz FROMs the
    /// already-built from-source base. Returns a clear error when NONE exist — we
    /// must never silently trigger a 20-40 min from-source rebuild.
    pub(super) async fn resolve_existing_base_image(
        docker: &Docker,
        candidates: &[String],
    ) -> DeployResult<String> {
        for tag in candidates {
            if image_exists(docker, tag).await? {
                return Ok(tag.clone());
            }
        }
        Err(DeployError::Docker(format!(
            "bazowy obraz vLLM-Spark nie istnieje na tym nodzie (szukano: {}). \
             Zbuduj go najpierw (deploy single-node vllm-spark), zanim uruchomisz deploy rozproszony.",
            candidates.join(", ")
        )))
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
        build_args: Option<std::collections::HashMap<String, String>>,
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
            buildargs: build_args,
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

    /// Opcje docker dla distributed (multi-node tensor-parallel) deployu. Replikuja
    /// flagi z proven recipe: `--network host` (NCCL/Ray gadaja po realnych
    /// interfejsach RDMA, nie po docker-bridge NAT), `--device /dev/infiniband`
    /// (userspace IB verbs), `--cap-add IPC_LOCK` + `--ulimit memlock=-1`
    /// (pinowanie pamieci GPUDirect/RDMA), `--shm-size` (domyslne 64MB -> NCCL
    /// "No space left on device"), neutralny CWD `/root` (editable vllm w /src
    /// cieniuje `import vllm`). Przy host-networking NIE publikujemy portow —
    /// kontener bindu je wprost na hoscie.
    pub(super) struct DistributedDockerOpts {
        pub shm_size_bytes: i64,
        pub working_dir: String,
        /// Pelna komenda powloki (`cd /root && ray start ... && vllm serve ...`).
        /// NADPISUJE entrypoint obrazu (`bash -c <cmd>`) zamiast polegac na tym, ze
        /// bazowy entrypoint.sh uszanuje `ENGINE_LAUNCH_CMD` — baza na nodzie moze
        /// byc STARSZA (bez tej obslugi) i wtedy odpalala domyslny single-node
        /// `vllm serve` (TP=1), gubiac komende ray+TP. Override jest niezalezny od
        /// wersji bazy.
        pub entrypoint_cmd: String,
    }

    /// Creates and starts a container. Returns container id.
    /// `binds` entries: (host_path, container_path, read_only).
    /// `distributed` Some => host-networking + RDMA device + IPC_LOCK + memlock +
    /// shm-size, no port publishing (multi-node TP path).
    pub(super) async fn run(
        docker: &Docker,
        image: &str,
        name: &str,
        ports: &[(u16, u16, &str)], // (host, container, proto: "tcp"|"udp")
        env: &HashMap<String, String>,
        cmd: &[String], // dolaczane do ENTRYPOINT jako "$@" (argv silnika)
        binds: &[(PathBuf, String, bool)],
        labels: &HashMap<String, String>,
        gpu: GpuSelection,
        distributed: Option<DistributedDockerOpts>,
        sandbox: Option<crate::deploy::docker::SandboxLimits>,
    ) -> DeployResult<String> {
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed: Vec<String> = Vec::new();
        // Host-networking distributed deploy publishes nothing — the engine binds
        // directly on the host's network namespace (Ray/NCCL need the real NICs).
        if distributed.is_none() {
            for (host, ctr, proto) in ports {
                let key = format!("{}/{}", ctr, proto);
                port_bindings.insert(
                    key.clone(),
                    Some(vec![PortBinding {
                        // Bind published ports to loopback only: Core reaches services
                        // via 127.0.0.1 and they must not be exposed to the LAN. With
                        // the sidecar gone, this host binding is the containment that
                        // keeps the engine's HTTP port off the network.
                        host_ip: Some("127.0.0.1".into()),
                        host_port: Some(host.to_string()),
                    }]),
                );
                exposed.push(key);
            }
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
        let device_requests = match gpu {
            GpuSelection::None => None,
            GpuSelection::Count(count) => Some(vec![DeviceRequest {
                driver: None,
                count: Some(count),
                device_ids: None,
                capabilities: Some(vec![vec!["gpu".to_string()]]),
                options: None,
            }]),
            GpuSelection::Devices(ids) => Some(vec![DeviceRequest {
                driver: None,
                count: None,
                device_ids: Some(ids),
                capabilities: Some(vec![vec!["gpu".to_string()]]),
                options: None,
            }]),
        };

        let mut host_config = HostConfig {
            port_bindings: Some(port_bindings),
            binds: if binds_vec.is_empty() {
                None
            } else {
                Some(binds_vec)
            },
            device_requests,
            ..Default::default()
        };
        // Distributed (multi-node TP): swap to host-networking + RDMA passthrough.
        if let Some(opts) = &distributed {
            use bollard::models::{DeviceMapping, ResourcesUlimits};
            host_config.network_mode = Some("host".to_string());
            host_config.port_bindings = None;
            host_config.devices = Some(vec![DeviceMapping {
                path_on_host: Some("/dev/infiniband".to_string()),
                path_in_container: Some("/dev/infiniband".to_string()),
                cgroup_permissions: Some("rwm".to_string()),
            }]);
            host_config.cap_add = Some(vec!["IPC_LOCK".to_string()]);
            // memlock=-1 => unlimited locked memory for GPUDirect/RDMA pinning.
            host_config.ulimits = Some(vec![ResourcesUlimits {
                name: Some("memlock".to_string()),
                soft: Some(-1),
                hard: Some(-1),
            }]);
            host_config.shm_size = Some(opts.shm_size_bytes);
        }
        // Untrusted-code engines (the Project Studio test runner) get hard
        // resource + capability limits on top of the normal config.
        if let Some(limits) = &sandbox {
            crate::deploy::docker::apply_sandbox_limits(&mut host_config, limits);
        }
        // Distributed: BYPASS the base entrypoint — run the ray+vllm command
        // directly as `bash -c <cmd>`. `cmd`/`engine_args` are ignored (the full
        // command, incl. TP size + ray, is baked into `entrypoint_cmd`).
        let (entrypoint, final_cmd) = match &distributed {
            Some(o) => (
                Some(vec![
                    "bash".to_string(),
                    "-c".to_string(),
                    o.entrypoint_cmd.clone(),
                ]),
                None,
            ),
            None => (
                None,
                if cmd.is_empty() {
                    None
                } else {
                    Some(cmd.to_vec())
                },
            ),
        };
        let body = ContainerCreateBody {
            image: Some(image.into()),
            entrypoint,
            cmd: final_cmd,
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
            working_dir: distributed.as_ref().map(|o| o.working_dir.clone()),
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

        // Distributed (multi-node TP) — wiedza potrzebna juz przy wyborze obrazu:
        // distributed vllm-spark idzie na CIENKI obraz (ray/rdma na gotowej bazie
        // from-source), zeby nie odpalac 20-40 min rebuildu przy kazdym deployu.
        let distributed = parse_distributed(&self.user_config);

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

        // Hardware-aware build: wykryj arch-tag GPU hosta i wybierz build-args
        // (CUDA base / torch index / wersja pakietu / arch list) z manifestu.
        // default_build_args + arch_variants[arch] (arch wygrywa). Gdy silnik
        // deklaruje `arch_variants`, tag obrazu dostaje sufiks arch, zeby obrazy
        // pod rozne karty (np. sglang Ampere vs Blackwell) nie kolidowaly w
        // cache "build only when missing".
        let gpu = crate::system_check::collect().gpu;
        let arch_tag = gpu.cuda_arch_tag();
        let mut build_args: std::collections::HashMap<String, String> =
            docker_section.default_build_args.clone();
        if let Some(variant) = docker_section.arch_variants.get(&arch_tag) {
            for (k, v) in &variant.build_args {
                build_args.insert(k.clone(), v.clone());
            }
        }
        // Hardware-aware build custom-kerneli CUDA: wstrzykujemy dokladny
        // TORCH_CUDA_ARCH_LIST pod wykryte GPU jako build-arg, zeby host
        // kompilowal kernele (OCR quad_nms, yolox FastCOCOEvalOp) pod swoja
        // realna karte. Manifest WYGRYWA (default_build_args/arch_variants),
        // np. vllm-spark `12.1a` — nie nadpisujemy. Bez GPU (None) zostawiamy
        // Dockerfile'owy default (fat-binary ARG).
        //
        // Gate na realnej deklaracji `ARG TORCH_CUDA_ARCH_LIST` w Dockerfile:
        // tylko nemotron-ocr/yolox kompiluja custom-kernel i deklaruja ten ARG.
        // Wstrzykniecie build-arga niekonsumowanego przez Dockerfile (rerank-vl,
        // parse, embed-vl, comfyui) daje docker warning "build-args were not
        // consumed" ORAZ niepotrzebny arch-aware tag -> zbedny per-arch rebuild
        // serwisu bez custom-kernela. Czytamy DOKLADNIE ten plik, ktory idzie do
        // Bollard build (`bundle_root.join(dockerfile_rel)`); IO error = nie
        // wstrzykujemy (bezpieczny default, brak warningu).
        if !build_args.contains_key("TORCH_CUDA_ARCH_LIST") {
            let dockerfile_declares_arch_arg =
                std::fs::read_to_string(bundle_root.join(&dockerfile_rel))
                    .map(|contents| {
                        contents
                            .lines()
                            .any(|l| l.trim_start().starts_with("ARG TORCH_CUDA_ARCH_LIST"))
                    })
                    .unwrap_or(false);
            if dockerfile_declares_arch_arg {
                if let Some(arch_list) = gpu.torch_cuda_arch_list() {
                    build_args.insert("TORCH_CUDA_ARCH_LIST".to_string(), arch_list);
                }
            }
        }
        // Silnik korzystajacy z build-args (jakikolwiek default/arch) dostaje
        // tag z sufiksem arch, zeby obrazy pod rozne karty nie kolidowaly.
        // Silniki bez build-args (searxng, browser-renderer) zostaja przy plaskim
        // tagu (brak niepotrzebnych przebudow).
        // UWAGA: liczymy PO wstrzyknieciu TORCH_CUDA_ARCH_LIST powyzej. Silniki z
        // custom-kernelem (nemotron-ocr, yolox; deklaruja `ARG TORCH_CUDA_ARCH_LIST`)
        // dostaja wstrzykniety arch-list, wiec ich tag staje sie arch-aware — inaczej
        // obraz zbudowany raz pod jeden arch (np. 8.6 na 3090) zostalby cicho reuzyty
        // na B300 (ten sam plaski tag) i odpalil zly kernel. Serwisy bez tego ARG
        // (rerank-vl, parse, embed-vl, comfyui) nie dostaja wstrzykniecia -> plaski tag.
        let arch_aware = !build_args.is_empty() || !docker_section.arch_variants.is_empty();
        // Source-hash w tagu: zmiana Dockerfile/kontekstu (docker_source_hash —
        // TEN SAM ktory steruje badge'em "Aktualizacja dostepna") daje NOWY tag,
        // wiec `if !image_exists` ponizej zwraca false i obraz SIE PRZEBUDOWUJE.
        // Bez tego plaski tag :v1 nigdy sie nie zmienial -> kazda zmiana Dockerfile
        // byla cicho ignorowana (deploy "0 ms", stary obraz reuzyty), a fix w
        // Dockerfile (np. TORCH_CUDA_ARCH_LIST) nigdy sie nie kompilowal.
        let src = self.manifest.docker_source_hash.as_str();
        let src_suffix = if src.is_empty() {
            String::new()
        } else {
            format!("-{}", &src[..src.len().min(12)])
        };
        let image_tag = if arch_aware {
            // Gruby arch_tag (cuda_arch_tag) NIE rozroznia kart w tej samej rodzinie:
            // B200 (cc 10.0) i B300 (cc 10.3) mapuja sie oba na "cuda-blackwell", choc
            // dostaja rozne TORCH_CUDA_ARCH_LIST ("10.0+PTX" vs "10.3+PTX"). Obraz
            // zbudowany na B300 (SASS 10.3, brak 10.0) reuzyty na B200 by NIE odpalil
            // (PTX forward-compat tylko w gore). Dlatego doklejamy krotki hash
            // zdeterminizowanego odcisku build_args: KAZDA roznica (arch list, torch
            // index, base image, wersja pakietu) -> inny tag -> rebuild; identyczne
            // build-args (dwa B300) -> ten sam tag -> reuse.
            use sha2::{Digest, Sha256};
            let mut keys: Vec<_> = build_args.keys().collect();
            keys.sort();
            let mut hasher = Sha256::new();
            for k in keys {
                hasher.update(k.as_bytes());
                hasher.update(b"=");
                hasher.update(build_args[k].as_bytes());
                hasher.update(b"\n");
            }
            let ba8 = hex::encode(hasher.finalize())[..8].to_string();
            format!(
                "tentaflow/{}:{}-{}-{}{}",
                self.manifest.engine.id, self.manifest.engine.version, arch_tag, ba8, src_suffix
            )
        } else {
            format!(
                "tentaflow/{}:{}{}",
                self.manifest.engine.id, self.manifest.engine.version, src_suffix
            )
        };
        let build_args = if build_args.is_empty() {
            None
        } else {
            Some(build_args)
        };

        // Distributed vllm-spark: zamiast 20-40 min rebuildu vLLM ZE ZRODEL na
        // kazdym deployu, budujemy CIENKA warstwe (ray + rdma-core) na JUZ
        // ZBUDOWANEJ bazie from-source `tentaflow/vllm-spark:<ver>`. Cienki obraz
        // `tentaflow/vllm-spark-ray:<ver>` powstaje w ~1-2 min (potem z cache).
        // Baza MUSI istniec na nodzie — jej brak to czytelny blad (NIE cichy
        // rebuild from-source). Inne silniki (np. `vllm` z PyPI) maja ray we
        // wlasnym, szybkim Dockerfile i ida normalna sciezka.
        let (image_tag, dockerfile_rel, build_args) =
            if distributed.is_some() && self.manifest.engine.id == "vllm-spark" {
                let base = backend::resolve_existing_base_image(
                    &docker,
                    &[
                        // Tag z normalnego deployu bazy (ten sam co `image_tag` tutaj),
                        image_tag.clone(),
                        // Plaski tag (manualnie zbudowana baza / spike).
                        format!("tentaflow/vllm-spark:{}", self.manifest.engine.version),
                    ],
                )
                .await?;
                let thin_tag = format!("tentaflow/vllm-spark-ray:{}", self.manifest.engine.version);
                let mut ba: HashMap<String, String> = HashMap::new();
                ba.insert("BASE_IMAGE".to_string(), base);
                (
                    thin_tag,
                    "tentaflow-containers/llm/docker/vllm-spark-ray/Dockerfile".to_string(),
                    Some(ba),
                )
            } else {
                (image_tag, dockerfile_rel, build_args)
            };

        // Build only when missing — repeated deploys reuse the cached image.
        if !backend::image_exists(&docker, &image_tag).await? {
            if let Some(s) = &self.log_sink {
                s.info(&format!(
                    "[docker] building image {} from {} (dockerfile {}, arch {}, build_args {})",
                    image_tag,
                    bundle_root.display(),
                    dockerfile_rel,
                    arch_tag,
                    build_args.as_ref().map(|m| m.len()).unwrap_or(0)
                ));
            }
            backend::build_image_from_context(
                &docker,
                &bundle_root,
                &dockerfile_rel,
                &image_tag,
                build_args,
                self.log_sink.as_ref(),
            )
            .await?;
        }

        // Distributed (multi-node TP): host-networking, no allocator lease. Head
        // runs the OpenAI endpoint on `spec.port`; worker is headless (Embedded
        // transport, no model rows, no HTTP probe) — `port` is reused only to name
        // the container deterministically (different node, no collision).
        // `distributed` rozpoznane wczesniej (przy wyborze obrazu).
        let is_worker = distributed
            .as_ref()
            .map(|d| d.role == "worker")
            .unwrap_or(false);

        let transport = if matches!(self.manifest.engine.id.as_str(), "codex" | "claude-code") {
            Transport::AgentRpc
        } else {
            match &distributed {
                Some(_) if is_worker => Transport::Embedded,
                Some(_) => Transport::HttpDirect,
                None => self.pick_transport(),
            }
        };
        let internal_port = self.manifest.engine.default_port;
        let mut allocated = Vec::new();

        // Allocate ports. Respawn istniejacego serwisu zachowuje port z DB
        // (preserved_port). Dla SidecarQuic preserved_port to host_http;
        // sidecar_quic_port jest zawsze swiezy bo nie trzymamy go w DB
        // jako stable identifier (rzadko exposed do klientow).
        let (host_http, sidecar_quic) = if let Some(d) = &distributed {
            // Host networking: the engine binds the host port directly, no lease.
            (d.port, None)
        } else if transport == Transport::SidecarQuic {
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
        // DGX Spark: Marlin NVFP4 GEMM (CUTLASS fp4 pada na sm_121). No-op dla
        // nie-fp4, wiec bezwarunkowo dla vllm-spark. Single-node docker.
        for (k, v) in super::spark_engine_env(&self.manifest.engine.id) {
            env.insert(k, v);
        }
        env.insert("PORT".into(), internal_port.to_string());
        env.insert(
            "TENTAFLOW_ENGINE_ID".into(),
            self.manifest.engine.id.clone(),
        );
        // Distributed: VLLM_PORT is the torch.distributed TCPStore master port and
        // MUST differ from the serve API port — the manifest default (8000) is never
        // allocated, so without this every member's vLLM would land on 8000 and
        // collide (EADDRINUSE). Single-node keeps the manifest default. A distributed
        // deploy with dist_port==0 means allocation never reached this node — refuse
        // rather than silently falling back to 8000 (regression of the collision bug).
        let vllm_port = if let Some(d) = &distributed {
            if d.dist_port == 0 {
                return Err(DeployError::PortAlloc(
                    "distributed deploy: dist_port not allocated (0) — refusing to fall back to default 8000".into(),
                ));
            }
            d.dist_port
        } else {
            internal_port
        };
        // Tryb vllm-mp dostaje master TCPStore JAWNIE przez `--master-port
        // <dist_port>` w komendzie serve. Env VLLM_PORT NIE moze wtedy wskazywac
        // tego samego portu: vLLM binduje VLLM_PORT dla wewnetrznego message queue
        // PRZED torch.distributed init i rank0 dostaje EADDRINUSE na wlasnym
        // master porcie. Bez env vLLM sam wybiera wolne porty wewnetrzne.
        let vllm_mp = distributed.is_some()
            && super::distributed::engine_is_vllm_mp(&self.manifest.engine.id);
        if !vllm_mp {
            env.insert("VLLM_PORT".into(), vllm_port.to_string());
        }
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
        // Edytowalna komenda z wizarda (Override): entrypoint wykrywa
        // ENGINE_LAUNCH_CMD i odpala je verbatim przez `sh -c` (placeholdery
        // $MODEL/$PORT rozwija powloka z env powyzej), pomijajac budowane argi.
        let launch_override = self
            .user_config
            .get("launch_command_override")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let Some(cmd) = launch_override {
            env.insert("ENGINE_LAUNCH_CMD".into(), cmd.to_string());
            if let Some(s) = &self.log_sink {
                s.info("[docker] launch_command_override aktywny (ENGINE_LAUNCH_CMD)");
            }
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
        engine_args.extend(crate::deploy::launch_dialect::docker_baseline_args(
            &self.manifest.engine.id,
        ));
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
        // Chat template gemma-4 (tool-calling): sciezka W KONTENERZE — Dockerfile
        // COPY-uje zbundlowany szablon do /app/chat_templates. Recepta vLLM
        // podawala repo-relative `examples/...`, ktorego pip-owy vLLM nie ma.
        if super::resolve_model_repo(&self.manifest, &self.user_config)
            .map(|r| r.to_lowercase().contains("gemma-4"))
            .unwrap_or(false)
        {
            engine_args.push("--chat-template".to_string());
            engine_args.push("/app/chat_templates/tool_chat_template_gemma4.jinja".to_string());
        }
        if self.manifest.engine.is_cuda_vllm() {
            let is_pooling = matches!(
                self.manifest.engine.category,
                crate::services::manifest::Category::Embeddings
                    | crate::services::manifest::Category::Reranker
            );
            let user_explicit_ratio = self
                .user_config
                .get("gpu_memory_utilization")
                .and_then(|v| v.as_f64());
            let from_args = super::parse_gpu_memory_utilization_arg(&engine_args.join(" "));
            let ratio = user_explicit_ratio
                .or(from_args)
                .or_else(|| super::auto_gpu_memory_utilization(is_pooling));
            if let Some(ratio) = ratio {
                engine_args.push("--gpu-memory-utilization".to_string());
                engine_args.push(format!("{:.2}", ratio));
                env.insert("GPU_MEMORY_UTILIZATION".into(), format!("{:.2}", ratio));
                if let Some(s) = &self.log_sink {
                    s.info(&format!("[docker] gpu_memory_utilization={:.2}", ratio));
                }
            }
            // DGX Spark (sm_121a): eager + no-flashinfer-autotune. GB10 ma pamiec
            // ZUNIFIKOWANA — compile/CUDA-graphs alokuja poza budzetem
            // `--gpu-memory-utilization` i puchna do ~100% poola; eager tego unika.
            // Jedno zrodlo prawdy z native/cluster (super::spark_engine_args).
            engine_args.extend(super::spark_engine_args(&self.manifest.engine.id));
        }
        // Dedup last-wins (extra/user args wygrywaja nad bundle/manifest base).
        // entrypoint.sh dorzuca tylko AUTO_PARALLEL gdy brak TP/PP w tych argach.
        let mut engine_args = crate::deploy::python_venv::dedup_cli_args_last_wins(engine_args);
        // Ten sam gate co native: fp8 kv-cache pada na GPU bez fp8e4nv (Ampere).
        // Kontener widzi karty hosta przez nvidia runtime, wiec host `collect()`
        // = arch kontenera.
        crate::deploy::python_venv::gate_fp8_kv_cache(&mut engine_args, None);

        let mut labels = HashMap::new();
        labels.insert(
            "tentaflow.engine_id".to_string(),
            self.manifest.engine.id.clone(),
        );

        // Distributed uses host-networking → publish nothing.
        let mut port_map: Vec<(u16, u16, &str)> = if distributed.is_some() {
            Vec::new()
        } else {
            vec![(host_http, internal_port, "tcp")]
        };
        if let Some(q) = sidecar_quic {
            // The sidecar always listens on the fixed container port 5000 (its
            // baked `config.default.toml [transport].port`; Core does not inject
            // a generated config), so the allocated host port `q` must map to
            // 5000 inside the container — NOT `q → q`, which targeted a container
            // port nothing listens on.
            port_map.push((q, SIDECAR_QUIC_CONTAINER_PORT, "udp"));
        }
        // Group label so a distributed teardown can find every member container
        // even when the service row is gone.
        if let Some(d) = &distributed {
            labels.insert(
                DISTRIBUTED_LABEL.to_string(),
                d.deployment_cluster_id.clone(),
            );
            labels.insert("tentaflow.distributed_role".to_string(), d.role.clone());
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
        let mut binds = vec![
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
        if matches!(self.manifest.engine.id.as_str(), "codex" | "claude-code") {
            let workspace = self
                .user_config
                .get("workspace_root")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir().map_err(|e| {
                    DeployError::Manifest(format!("resolve coding-agent workspace: {e}"))
                })?);
            let workspace = std::fs::canonicalize(&workspace).map_err(|e| {
                DeployError::Manifest(format!(
                    "invalid coding-agent workspace {}: {e}",
                    workspace.display()
                ))
            })?;
            if !workspace.is_dir() {
                return Err(DeployError::Manifest(
                    "coding-agent workspace_root is not a directory".to_string(),
                ));
            }
            let state = crate::paths::keys_dir()
                .join("coding-agents")
                .join(&self.manifest.engine.id);
            std::fs::create_dir_all(&state).map_err(|e| {
                DeployError::Manifest(format!("create coding-agent state directory: {e}"))
            })?;
            binds.push((workspace, "/workspace".to_string(), false));
            binds.push((state, "/data".to_string(), false));
        }

        // ComfyUI nie pobiera wag sam: bez checkpointu w `models/checkpoints`
        // kazda generacja konczy sie `ckpt_name not in []`. Sciagamy plik
        // presetu na host (idempotentnie) PRZED startem kontenera i montujemy
        // katalog do `models/checkpoints`, zeby `/object_info` widzial go od
        // razu po wstaniu kontenera.
        if self.manifest.engine.id == "comfyui" {
            if let Some(preset) = super::resolve_selected_preset(&self.manifest, &self.user_config)
            {
                if let Some(file) = preset
                    .checkpoint_file
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let ckpt_dir = crate::paths::image_gen_checkpoints_dir();
                    std::fs::create_dir_all(&ckpt_dir).map_err(|e| {
                        DeployError::Manifest(format!(
                            "create checkpoints dir {}: {e}",
                            ckpt_dir.display()
                        ))
                    })?;
                    let dest = ckpt_dir.join(file);
                    let url = format!(
                        "https://huggingface.co/{}/resolve/main/{}",
                        preset.repo, file
                    );
                    if let Some(s) = &self.log_sink {
                        s.info(&format!(
                            "[docker] comfyui checkpoint: ensuring {file} (repo={})",
                            preset.repo
                        ));
                    }
                    let sink = self.log_sink.clone();
                    let label = file.to_string();
                    let progress: Option<crate::services::model_download::ProgressFn> =
                        sink.map(|s| {
                            let label = label.clone();
                            Box::new(move |done: u64, total: u64, _l: &str| {
                                let pct = if total > 0 {
                                    (done as f64 / total as f64 * 100.0) as u64
                                } else {
                                    0
                                };
                                s.info(&format!(
                                    "[docker] checkpoint {label}: {} / {} MB ({pct}%)",
                                    done / 1_048_576,
                                    total / 1_048_576
                                ));
                            })
                                as Box<dyn Fn(u64, u64, &str) + Send + Sync>
                        });
                    crate::services::model_download::download_with_progress(
                        &url, &dest, file, progress,
                    )
                    .await
                    .map_err(|e| {
                        DeployError::Manifest(format!("download comfyui checkpoint {file}: {e}"))
                    })?;
                    binds.push((
                        ckpt_dir,
                        crate::paths::COMFYUI_CHECKPOINTS_PATH.to_string(),
                        false,
                    ));
                }
            }
        }

        // Silniki llama-server (gguf_model_mount): entrypoint laduje pojedynczy
        // GGUF z `/data/models/model.gguf` i pada gdy pliku brak. Pobieramy GGUF
        // wybranego presetu na host cache modeli (juz zamontowany pod
        // CONTAINER_MODELS_PATH) i wskazujemy `MODEL_PATH` na jego sciezke w
        // kontenerze — bez osobnego binda. Odpowiednik ComfyUI `checkpoint_file`.
        if docker_section.gguf_model_mount {
            let repo =
                super::resolve_model_repo(&self.manifest, &self.user_config).ok_or_else(|| {
                    DeployError::Manifest(
                        "gguf_model_mount engine has no resolvable model repo".into(),
                    )
                })?;
            let model_file = self
                .user_config
                .get("model_file")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let quantization = self
                .user_config
                .get("quantization")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    super::resolve_selected_preset(&self.manifest, &self.user_config)
                        .and_then(|p| p.quantization.clone())
                });
            let selection = if let Some(file) = model_file.as_deref() {
                if !crate::hub::model_store::valid_hf_relative_path(file) {
                    return Err(DeployError::Manifest(format!(
                        "invalid GGUF filename '{file}'"
                    )));
                }
                crate::hub::model_store::ModelDownloadSelection::ExactFile(file.to_string())
            } else if let Some(q) = quantization.as_deref() {
                crate::hub::model_store::ModelDownloadSelection::GgufQuantization(q.to_string())
            } else {
                return Err(DeployError::Manifest(
                    "gguf_model_mount deploy requires model_file or preset quantization".into(),
                ));
            };

            if let Some(s) = &self.log_sink {
                s.info(&format!("[docker] gguf model: ensuring {repo}"));
            }
            let store = crate::hub::model_store::ModelStore::new(crate::paths::models_root());
            let (progress_tx, mut progress_rx) =
                tokio::sync::mpsc::channel::<crate::hub::model_store::DownloadProgress>(128);
            let progress_sink = self.log_sink.clone();
            let progress_task = tokio::spawn(async move {
                while let Some(p) = progress_rx.recv().await {
                    if let Some(sink) = &progress_sink {
                        sink.info(&format!(
                            "[docker] gguf {} {:.1}% ({}/{} MB)",
                            p.file_name,
                            p.percent,
                            p.bytes_downloaded / 1_048_576,
                            p.bytes_total / 1_048_576
                        ));
                    }
                }
            });
            let host_path = store
                .download_model_selection(&repo, self.hf_token.as_deref(), progress_tx, selection)
                .await
                .map_err(|e| DeployError::Manifest(format!("download gguf {repo}: {e}")))?;
            let _ = progress_task.await;

            // Host cache lezy pod models_root() zamontowanym jako
            // CONTAINER_MODELS_PATH, wiec wystarczy przelozyc sciezke.
            let rel = host_path
                .strip_prefix(crate::paths::models_root())
                .map_err(|_| {
                    DeployError::Manifest(format!(
                        "gguf path {} is not under models_root",
                        host_path.display()
                    ))
                })?;
            let container_path = format!(
                "{}/{}",
                crate::paths::CONTAINER_MODELS_PATH,
                rel.to_string_lossy()
            );
            env.insert("MODEL_PATH".into(), container_path);
        }

        let gpu = resolve_gpu_selection(
            &self.user_config,
            self.manifest
                .deploy
                .docker
                .as_ref()
                .and_then(|d| d.gpus.as_deref()),
        );
        if let Some(s) = &self.log_sink {
            match &gpu {
                GpuSelection::None => s.info("[docker] GPU passthrough: none (CPU)"),
                GpuSelection::Count(c) if *c < 0 => s.info("[docker] GPU passthrough: all"),
                GpuSelection::Count(c) => s.info(&format!("[docker] GPU passthrough: count={}", c)),
                GpuSelection::Devices(ids) => s.info(&format!(
                    "[docker] GPU passthrough: devices=[{}]",
                    ids.join(",")
                )),
            }
        }
        // `--gpus N` hands the container the first N host indices, so the NCCL
        // decision sees the same set the engine will span.
        let nccl_scope = match &gpu {
            GpuSelection::None => None,
            GpuSelection::Count(c) if *c < 0 => Some(super::gpu_topology::GpuScope::All),
            GpuSelection::Count(c) => Some(super::gpu_topology::GpuScope::Indices(
                (0..*c as u32).collect(),
            )),
            GpuSelection::Devices(ids) => Some(super::gpu_topology::GpuScope::Indices(
                super::gpu_topology::parse_gpu_indices(ids.iter().map(String::as_str)),
            )),
        };
        if let Some(scope) = nccl_scope {
            super::gpu_topology::apply_nccl_p2p_level_env(
                &self.manifest.engine.id,
                &self.user_config,
                scope,
                super::gpu_topology::host_topology(),
                &mut env,
                self.log_sink.as_ref(),
            );
        }

        // Recipe shm-size: 16 GiB (default 64 MB → NCCL "No space left on device").
        // The ray+vllm command (set as `launch_command_override` by the distributed
        // config builder) becomes the container ENTRYPOINT — independent of the
        // base image's entrypoint version.
        let distributed_opts = match &distributed {
            Some(_) => {
                let cmd = self
                    .user_config
                    .get("launch_command_override")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        DeployError::Manifest(
                            "distributed deploy bez launch_command_override (komenda ray+vllm)"
                                .to_string(),
                        )
                    })?;
                Some(backend::DistributedDockerOpts {
                    shm_size_bytes: 16 * 1024 * 1024 * 1024,
                    working_dir: "/root".to_string(),
                    entrypoint_cmd: cmd.to_string(),
                })
            }
            None => None,
        };
        // The test runner executes agent-authored scripts: the container is the
        // security boundary, so it runs with a read-only rootfs, dropped
        // capabilities and hard memory/CPU/PID caps.
        let sandbox = (self.manifest.engine.id == crate::project_studio::auto_runs::RUNNER_ENGINE_ID)
            .then(crate::deploy::docker::SandboxLimits::test_runner);
        let id = backend::run(
            &docker,
            &image_tag,
            &container_name,
            &port_map,
            &env,
            &engine_args,
            &binds,
            &labels,
            gpu,
            distributed_opts,
            sandbox,
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

        // Readiness. A Ray worker is headless (no HTTP) — its gotowosc is an
        // alive-grace (container stays up long enough to join the GCS). Head and
        // single-node deploys race the OpenAI/health URLs. The head with
        // `--distributed-executor-backend ray` naturally BLOCKS until the workers
        // join, so no hard timeout (the workers are launched right after by the
        // coordinator).
        let outcome = if is_worker {
            wait_worker_alive_grace(
                &docker,
                &container_name,
                std::time::Duration::from_secs(8),
                self.log_sink.as_ref(),
            )
            .await
        } else {
            let probe_cfg = SmartProbeConfig {
                readiness_urls: vec![
                    format!("http://127.0.0.1:{}/v1/models", host_http),
                    format!("http://127.0.0.1:{}/health", host_http),
                    // SearXNG i inne aplikacje webowe wystawiaja /healthz (konwencja
                    // k8s) zamiast /health — pierwszy 2xx wygrywa, reszta ignorowana.
                    format!("http://127.0.0.1:{}/healthz", host_http),
                    // ComfyUI nie ma /health ani /v1/models — gotowosc po /system_stats.
                    format!("http://127.0.0.1:{}/system_stats", host_http),
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
            smart_health_probe(probe_cfg, move || {
                let d = docker_for_probe.clone();
                let n = name_for_probe.clone();
                async move {
                    match d.inspect_container(&n, None).await {
                        Ok(info) => {
                            let running =
                                info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
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
            .await
        };

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
            // Head + single-node keep the port (routing + container teardown by
            // `tentaflow-<engine>-<port>` name). Worker is headless (Embedded),
            // no endpoint, but it still carries the port so `stop()` can rebuild
            // the deterministic container name on teardown/shutdown.
            port: Some(host_http),
            sidecar_port: sidecar_quic,
            endpoint_url,
            container_id: Some(id),
            instance_dir: None,
        };
        // A Ray worker must NOT advertise the model — it is headless and never
        // serves inference (only the head endpoint is routable). Empty model rows
        // keep it out of the resolver while staying a tracked docker service.
        let models = if is_worker {
            Vec::new()
        } else {
            models_from_manifest(&self.manifest, &self.user_config)
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- GPU selection / passthrough resolution (direct-http migration) ----

    #[cfg(feature = "docker")]
    #[test]
    fn resolve_gpu_count_explicit_values() {
        assert_eq!(resolve_gpu_count(Some("all")), Some(-1));
        assert_eq!(resolve_gpu_count(Some("2")), Some(2));
        assert_eq!(resolve_gpu_count(Some("none")), None);
        assert_eq!(resolve_gpu_count(Some("0")), None);
        assert_eq!(resolve_gpu_count(Some("")), None);
        // Garbage parses to no GPU rather than panicking.
        assert_eq!(resolve_gpu_count(Some("abc")), None);
    }

    #[cfg(feature = "docker")]
    #[test]
    fn resolve_gpu_selection_honors_wizard_mode() {
        // Explicit "none" overrides any host default.
        assert!(matches!(
            resolve_gpu_selection(&serde_json::json!({"gpu_select_mode": "none"}), Some("all")),
            GpuSelection::None
        ));
        // "all" → every GPU.
        assert!(matches!(
            resolve_gpu_selection(&serde_json::json!({"gpu_select_mode": "all"}), None),
            GpuSelection::Count(-1)
        ));
        // "specific" with ids (numbers or strings) → device list.
        match resolve_gpu_selection(
            &serde_json::json!({"gpu_select_mode": "specific", "gpu_ids": [0, "3"]}),
            None,
        ) {
            GpuSelection::Devices(ids) => assert_eq!(ids, vec!["0".to_string(), "3".to_string()]),
            other => panic!("expected Devices, got {:?}", other),
        }
        // "specific" with empty ids → falls back to manifest gpus.
        assert!(matches!(
            resolve_gpu_selection(
                &serde_json::json!({"gpu_select_mode": "specific", "gpu_ids": []}),
                Some("all")
            ),
            GpuSelection::Count(-1)
        ));
        // Manifest gpus="none" with no wizard mode → no GPU.
        assert!(matches!(
            resolve_gpu_selection(&serde_json::json!({}), Some("none")),
            GpuSelection::None
        ));
    }

    fn skeleton_manifest(id: &str) -> ServiceManifest {
        use crate::services::manifest::{
            ApiKind, Category, DeploySection, DockerDeploy as DockerSec, DockerTransport, Engine,
            TargetOs,
        };
        ServiceManifest {
            engine: Engine {
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
                default_port: 8000,
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
                docker: Some(DockerSec {
                    context_path: Some("/nonexistent/ctx".into()),
                    compose_path: None,
                    platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                    download_image: None,
                    download_size_mb: None,
                    transport: Some(DockerTransport::SidecarQuic),
                    gpus: None,
                    ..Default::default()
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
        let base = crate::deploy::launch_dialect::docker_baseline_args("vllm");
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
        let mut base = crate::deploy::launch_dialect::docker_baseline_args("vllm-spark");
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
        use crate::deploy::launch_dialect::docker_baseline_args;
        // sglang ma teraz wlasny baseline (dialekt sglang), nie vLLM-owy.
        let sglang = docker_baseline_args("sglang").join(" ");
        assert!(sglang.contains("--mem-fraction-static"), "got: {sglang}");
        assert!(!sglang.contains("--max-model-len"), "got: {sglang}");
        // llama.cpp / trt-llm — bez Rust-side baseline (entrypoint/runner wlasny).
        assert!(docker_baseline_args("llama-cpp").is_empty());
        assert!(docker_baseline_args("trt-llm").is_empty());
        assert!(!docker_baseline_args("vllm-metal").is_empty());
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
