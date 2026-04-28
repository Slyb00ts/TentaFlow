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

/// Konfiguracja deployu jednego kontenera.
#[derive(Debug, Clone)]
pub struct DeployRequest {
    /// Nazwa kontenera w bundle (np. "llm-vllm")
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

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        binds: if binds.is_empty() { None } else { Some(binds) },
        device_requests,
        ..Default::default()
    };

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
