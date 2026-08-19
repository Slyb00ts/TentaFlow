// =============================================================================
// Plik: deploy/docker.rs
// Opis: Operacje Docker przez bollard — build obrazu z embedowanego kontekstu,
//       run kontenera, status, stop. Aktywne tylko z feature `docker`.
// =============================================================================

use anyhow::{Context, Result};
use bollard::Docker;
use std::collections::HashMap;
use std::path::Path;

use super::bundle;

/// Hard limits of a container executing UNTRUSTED code (the Project Studio
/// test runner). Bollard's default `HostConfig` constrains nothing — a
/// container running agent-authored tests would get the whole host RAM, full
/// capabilities and a writable rootfs.
///
/// ARCHITECTURE NOTE: the target design (F3 variant A) has Core create an
/// EPHEMERAL container per run and also enforce `network_mode = "none"` for
/// unit tests. This version applies the limits to the long-lived test-runner
/// service, because per-run container orchestration needs its own lifecycle
/// (build/mount/cleanup) outside the scope of this change. Network allowlist
/// enforcement already runs on the Python side (`executor/sandbox_net.py` plus
/// Playwright routing), so the network boundary does not disappear — only the
/// place where it is enforced.
#[derive(Debug, Clone)]
pub struct SandboxLimits {
    /// `"none"` = no network, `"bridge"` = network with the runner-side allowlist.
    pub network_mode: String,
    pub memory_bytes: i64,
    /// CPU limit in nano-cores (1 core = 1_000_000_000).
    pub nano_cpus: i64,
    pub pids_limit: i64,
    /// Read-only rootfs plus a `noexec` tmpfs on `/tmp`.
    pub readonly_rootfs: bool,
    pub tmpfs_bytes: u64,
    /// Writable tmpfs for the service work directory, or `None` when the image
    /// only needs `/tmp`. A read-only rootfs makes a container whose process
    /// creates its work tree (the test runner rmtree+mkdir's
    /// `TEST_RUNNER_WORK_DIR` on startup) fail before it can serve `/health`.
    pub work_tmpfs: Option<(String, u64)>,
}

impl SandboxLimits {
    /// Test-runner profile: 4 GiB RAM, 2 cores, 512 processes, read-only
    /// rootfs, no extra capabilities and writes confined to two tmpfs mounts —
    /// `/tmp` (1 GiB, noexec) and the run work tree `/var/lib/tentaflow`
    /// (2 GiB), which holds the generated scripts, pytest reports, Playwright
    /// traces and screenshots of every concurrent run.
    ///
    /// The memory cap covers BOTH tmpfs mounts: tmpfs pages are charged to the
    /// container's memory cgroup, so a 2 GiB cap next to 3 GiB of tmpfs would
    /// OOM-kill the runner as soon as a run produced large artifacts.
    pub fn test_runner() -> Self {
        Self {
            network_mode: "bridge".to_string(),
            memory_bytes: 4 * 1024 * 1024 * 1024,
            nano_cpus: 2_000_000_000,
            pids_limit: 512,
            readonly_rootfs: true,
            tmpfs_bytes: 1024 * 1024 * 1024,
            work_tmpfs: Some(("/var/lib/tentaflow".to_string(), 2 * 1024 * 1024 * 1024)),
        }
    }

    /// Code Studio session profile (§7.2): 8 GiB RAM, 4 cores, 1024 processes,
    /// a read-only rootfs and a 2 GiB `/tmp` — a build toolchain needs far more
    /// room than a test run, and everything it writes goes either into the
    /// worktree mount or into that tmpfs.
    ///
    /// `network_mode` is the profile's network axis and has exactly two honest
    /// values: `"none"` for `network_access = none` (no route at all, so the
    /// only way out is the git shim socket) and the name of the workspace's
    /// INTERNAL egress network for `network_access = gateway`. The default
    /// bridge is never one of them — it would mean unfiltered internet under a
    /// profile that claims to be filtered.
    ///
    /// Two of the requirements of §7.2 are not `HostConfig` fields and are
    /// therefore enforced where the container is created (`code_studio::
    /// sandbox`): the non-root user the process runs as, and the absence of any
    /// docker-socket bind — a mounted socket is a root shell on the host, which
    /// would make every limit here decoration.
    pub fn code_session(network_mode: impl Into<String>) -> Self {
        Self {
            network_mode: network_mode.into(),
            memory_bytes: 8 * 1024 * 1024 * 1024,
            nano_cpus: 4_000_000_000,
            pids_limit: 1024,
            readonly_rootfs: true,
            tmpfs_bytes: 2 * 1024 * 1024 * 1024,
            work_tmpfs: None,
        }
    }
}

/// Konfiguracja deployu jednego kontenera.
#[derive(Debug, Clone)]
pub struct DeployRequest {
    /// Sciezka kontekstu wzgledem `tentaflow-containers/` w bundle, np.
    /// "llm/docker/vllm" — `build_image` dokleja prefix i ladowa Dockerfile
    /// z `tentaflow-containers/<container>/Dockerfile`. Historycznie pole
    /// nazywalo sie "container" gdy struktura byla plaska (`llm-vllm/`),
    /// po reorganizacji do category-based layoutu jest to context_path.
    pub container: String,
    /// Tag obrazu, domyslnie "tentaflow/<container>:latest"
    pub image_tag: Option<String>,
    /// Nazwa kontenera Docker (--name)
    pub instance_name: Option<String>,
    /// Mapowanie portow host:container (np. [("5010","5000/udp")])
    pub ports: Vec<(String, String)>,
    /// Volume mounts: (host_path, container_path)
    pub volumes: Vec<(String, String)>,
    /// Zmienne srodowiskowe
    pub env: HashMap<String, String>,
    /// Czy uzyc GPU (--gpus all)
    pub gpu: bool,
    /// Twarde limity dla kontenerow wykonujacych niezaufany kod. `None` =
    /// zaufany silnik (LLM, TTS...) bez dodatkowych ograniczen.
    pub sandbox: Option<SandboxLimits>,
}

/// Buduje obraz Docker z embedowanego kontekstu i uruchamia kontener.
/// Gdy obraz `image_tag` juz istnieje lokalnie, build jest pomijany — chroni
/// to przed podwojnym buildem, gdy caller (np. `deploy::runner`) zbudowal go
/// wczesniej przez CLI z BuildKitem (bollard nie wspiera `--mount=type=cache`
/// z Dockerfile'i a takie mounty dziala tylko z BuildKit'em).
pub async fn deploy(req: &DeployRequest) -> Result<String> {
    let docker = Docker::connect_with_local_defaults().context(
        "nie mozna polaczyc z Docker daemon (sprawdz czy dziala i uzytkownik ma uprawnienia)",
    )?;

    let image_tag = req
        .image_tag
        .clone()
        .unwrap_or_else(|| format!("tentaflow/{}:latest", req.container));

    if !image_exists(&docker, &image_tag).await? {
        let workdir = tempfile::tempdir().context("tworzenie tmpdir dla kontekstu")?;
        bundle::extract_to(workdir.path()).context("rozpakowanie embedowanego bundle")?;
        build_image(&docker, workdir.path(), &req.container, &image_tag).await?;
    }

    run_container(&docker, req, &image_tag).await
}

/// `docker inspect <tag>` przez bollard — true gdy obraz istnieje lokalnie.
async fn image_exists(docker: &Docker, tag: &str) -> Result<bool> {
    match docker.inspect_image(tag).await {
        Ok(_) => Ok(true),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(false),
        Err(e) => Err(anyhow::anyhow!("inspect_image({}): {}", tag, e)),
    }
}

async fn build_image(docker: &Docker, context: &Path, container: &str, tag: &str) -> Result<()> {
    use bollard::query_parameters::BuildImageOptions;
    use futures::StreamExt;

    let dockerfile = format!("tentaflow-containers/{}/Dockerfile", container);
    let opts = BuildImageOptions {
        dockerfile,
        t: Some(tag.to_string()),
        rm: true,
        ..Default::default()
    };

    // Spakuj kontekst do tar (in-memory) bo bollard tego oczekuje
    let mut tar_builder = tar::Builder::new(Vec::new());
    tar_builder
        .append_dir_all(".", context)
        .context("pakowanie kontekstu do tar dla bollard")?;
    let tar_bytes = tar_builder.into_inner()?;

    use bollard::body_full;
    use hyper::body::Bytes;
    let body = body_full(Bytes::from(tar_bytes));
    let mut stream = docker.build_image(opts, None, Some(body));
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                if let Some(stream) = info.stream {
                    tracing::info!(target: "docker_build", "{}", stream.trim_end());
                }
                if let Some(err_detail) = info.error_detail {
                    anyhow::bail!(
                        "docker build error: {}",
                        err_detail.message.unwrap_or_default()
                    );
                }
            }
            Err(e) => return Err(anyhow::anyhow!("bollard build: {}", e)),
        }
    }
    tracing::info!(image = %tag, "Obraz zbudowany");
    Ok(())
}

/// Applies `SandboxLimits` to a `HostConfig`. Additive: fields the sandbox does
/// not touch (ports, binds, GPU) stay as they are.
#[cfg(feature = "docker")]
pub fn apply_sandbox_limits(
    host_config: &mut bollard::models::HostConfig,
    limits: &SandboxLimits,
) {
    host_config.network_mode = Some(limits.network_mode.clone());
    host_config.memory = Some(limits.memory_bytes);
    host_config.nano_cpus = Some(limits.nano_cpus);
    host_config.pids_limit = Some(limits.pids_limit);
    // Drop ALL capabilities: a test process needs none of them.
    host_config.cap_drop = Some(vec!["ALL".to_string()]);
    // Blocks escalation through setuid binaries inside the image.
    host_config.security_opt = Some(vec!["no-new-privileges".to_string()]);
    host_config.readonly_rootfs = Some(limits.readonly_rootfs);
    if limits.readonly_rootfs {
        // A read-only rootfs needs a writable /tmp — `noexec` prevents running
        // a downloaded binary from there, `nosuid` blocks setuid.
        let mut tmpfs = HashMap::new();
        tmpfs.insert(
            "/tmp".to_string(),
            format!("rw,noexec,nosuid,nodev,size={}", limits.tmpfs_bytes),
        );
        if let Some((path, size)) = &limits.work_tmpfs {
            // The work tree is created by the container's own (non-root) user,
            // so the mount needs world-writable permissions; `mode=1777` also
            // keeps one run from deleting another user's files (sticky bit).
            // Deliberately WITHOUT `noexec`: build/test toolchains materialise
            // executables (venv shims, node_modules/.bin) inside the run
            // directory. Containment stays with cap_drop + no-new-privileges +
            // the read-only rootfs.
            tmpfs.insert(
                path.clone(),
                format!("rw,nosuid,nodev,mode=1777,size={size}"),
            );
        }
        host_config.tmpfs = Some(tmpfs);
    }
}

async fn run_container(docker: &Docker, req: &DeployRequest, image: &str) -> Result<String> {
    use bollard::models::{ContainerCreateBody as Config, DeviceRequest, HostConfig, PortBinding};
    use bollard::query_parameters::{CreateContainerOptions, StartContainerOptions};

    let name = req
        .instance_name
        .clone()
        .unwrap_or_else(|| format!("tentaflow-{}", req.container));

    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    let mut exposed: HashMap<String, HashMap<(), ()>> = HashMap::new();
    for (host, ctr) in &req.ports {
        port_bindings.insert(
            ctr.clone(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".into()),
                host_port: Some(host.clone()),
            }]),
        );
        exposed.insert(ctr.clone(), HashMap::new());
    }

    let binds: Vec<String> = req
        .volumes
        .iter()
        .map(|(h, c)| format!("{}:{}", h, c))
        .collect();

    let env: Vec<String> = req
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();

    let device_requests = if req.gpu {
        Some(vec![DeviceRequest {
            driver: Some("".into()),
            count: Some(-1),
            capabilities: Some(vec![vec!["gpu".into()]]),
            ..Default::default()
        }])
    } else {
        None
    };

    let mut host_config = HostConfig {
        port_bindings: Some(port_bindings),
        binds: if binds.is_empty() { None } else { Some(binds) },
        device_requests,
        ..Default::default()
    };
    if let Some(limits) = &req.sandbox {
        apply_sandbox_limits(&mut host_config, limits);
    }

    let exposed_ports_vec: Vec<String> = exposed.into_keys().collect();
    let config = Config {
        image: Some(image.to_string()),
        env: if env.is_empty() { None } else { Some(env) },
        exposed_ports: if exposed_ports_vec.is_empty() {
            None
        } else {
            Some(exposed_ports_vec)
        },
        host_config: Some(host_config),
        ..Default::default()
    };

    let create_opts = CreateContainerOptions {
        name: Some(name.clone()),
        platform: String::new(),
    };

    // Usun stary kontener o tej samej nazwie (jesli istnieje)
    let _ = docker
        .remove_container(
            &name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    docker
        .create_container(Some(create_opts), config)
        .await
        .with_context(|| format!("create_container {}", name))?;
    docker
        .start_container(&name, None::<StartContainerOptions>)
        .await
        .with_context(|| format!("start_container {}", name))?;

    tracing::info!(container = %name, image = %image, "Kontener uruchomiony");
    Ok(name)
}

/// Zatrzymuje i usuwa kontener.
pub async fn stop(name: &str) -> Result<()> {
    let docker = Docker::connect_with_local_defaults()?;
    docker
        .stop_container(name, None)
        .await
        .with_context(|| format!("stop {}", name))?;
    docker
        .remove_container(
            name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
        .with_context(|| format!("remove {}", name))?;
    Ok(())
}
