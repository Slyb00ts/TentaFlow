// =============================================================================
// Plik: meeting/container.rs
// Opis: Niskopoziomowy interfejs do Dockera dla sesji Meeting Bot. Zakłada, że
//       obraz `tentaflow/teams-bot:latest` jest juz zbudowany (Services UI
//       buduje go raz na pierwsze uzycie manifestu agents/teams-bot).
//       Tworzy efemeryczny kontener z nazwa `meeting-bot-<session_id>`, maps
//       wewnetrzne porty 5000/udp, 5900, 6080 na przydzielone porty hosta,
//       przekazuje konfig przez env. Automatyczny cleanup: stop+rm na leave,
//       force-remove stale containers przy starcie.
// =============================================================================

use anyhow::{Context, Result};
use std::collections::HashMap;
use tracing::{info, warn};

use super::port_pool::AllocatedPorts;

pub const IMAGE_TAG: &str = "tentaflow/teams-bot:latest";

/// Parametry startu kontenera Meeting Bot dla pojedynczej sesji.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub session_id: i64,
    pub meeting_url: String,
    pub ports: AllocatedPorts,
    /// Ed25519 secret key bota (hex, 64 znaki). Host używa go żeby obliczyć
    /// EndpointId i połączyć się do bota via iroh.
    pub secret_key_hex: String,
    pub bot_name: String,
    pub stt_alias: Option<String>,
    pub tts_alias: Option<String>,
    pub llm_alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SpawnOutcome {
    pub container_id: String,
    pub container_name: String,
}

/// Nazwa kontenera — deterministyczna po session_id, żeby leave mógł znaleźć
/// kontener nawet jeśli backend został zrestartowany.
pub fn container_name(session_id: i64) -> String {
    format!("meeting-bot-{}", session_id)
}

#[cfg(feature = "docker")]
pub async fn spawn(req: &SpawnRequest) -> Result<SpawnOutcome> {
    use bollard::models::{ContainerCreateBody as Config, HostConfig, PortBinding};
    use bollard::query_parameters::{CreateContainerOptions, StartContainerOptions};
    use bollard::Docker;

    let docker = Docker::connect_with_local_defaults()
        .context("Nie mozna polaczyc z Docker daemon — sprawdz socket i uprawnienia")?;

    // Upewnij sie ze obraz istnieje — jesli nie, zwracamy wyraźny błąd żeby
    // frontend pokazał "addon nie wdrozony". Inaczej bollard sam spróbuje pullować
    // z Docker Hub i wisimy przez minute.
    let image_exists = docker
        .inspect_image(IMAGE_TAG)
        .await
        .is_ok();
    if !image_exists {
        anyhow::bail!(
            "Obraz {} nie istnieje — zbuduj kontener teams-bot z Services (agents/teams-bot)",
            IMAGE_TAG
        );
    }

    let name = container_name(req.session_id);
    // Force-remove ewentualnie istniejacy kontener o tej samej nazwie (stale po crash).
    let _ = docker
        .remove_container(
            &name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    // Port mappings — container ports → host dynamic ports.
    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    port_bindings.insert(
        "5000/udp".into(),
        Some(vec![PortBinding {
            host_ip: Some("0.0.0.0".into()),
            host_port: Some(req.ports.quic.to_string()),
        }]),
    );
    port_bindings.insert(
        "5900/tcp".into(),
        Some(vec![PortBinding {
            host_ip: Some("0.0.0.0".into()),
            host_port: Some(req.ports.vnc.to_string()),
        }]),
    );
    port_bindings.insert(
        "6080/tcp".into(),
        Some(vec![PortBinding {
            host_ip: Some("0.0.0.0".into()),
            host_port: Some(req.ports.novnc.to_string()),
        }]),
    );

    let exposed_ports: Vec<String> = vec!["5000/udp".into(), "5900/tcp".into(), "6080/tcp".into()];

    let env = build_env(req);
    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        // Publish=all=false; używamy eksplicitnych bindings.
        auto_remove: Some(false),
        ..Default::default()
    };

    let config = Config {
        image: Some(IMAGE_TAG.to_string()),
        env: Some(env),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        labels: Some({
            let mut m = HashMap::new();
            m.insert("tentaflow.meeting_session".to_string(), req.session_id.to_string());
            m.insert("tentaflow.kind".to_string(), "meeting-bot".to_string());
            m
        }),
        ..Default::default()
    };

    let create_opts = CreateContainerOptions {
        name: Some(name.clone()),
        platform: String::new(),
    };

    let created = docker
        .create_container(Some(create_opts), config)
        .await
        .with_context(|| format!("create_container {}", name))?;
    docker
        .start_container(&name, None::<StartContainerOptions>)
        .await
        .with_context(|| format!("start_container {}", name))?;

    info!(
        session = %req.session_id,
        container = %name,
        quic = req.ports.quic,
        vnc = req.ports.vnc,
        novnc = req.ports.novnc,
        "Meeting Bot kontener uruchomiony"
    );

    Ok(SpawnOutcome {
        container_id: created.id,
        container_name: name,
    })
}

#[cfg(feature = "docker")]
pub async fn stop(session_id: i64) -> Result<()> {
    use bollard::Docker;

    let docker = Docker::connect_with_local_defaults()?;
    let name = container_name(session_id);
    // Grace stop (10s) — pozwala botowi wyslac leave do Teams.
    if let Err(e) = docker
        .stop_container(
            &name,
            Some(bollard::query_parameters::StopContainerOptions {
                t: Some(10),
                ..Default::default()
            }),
        )
        .await
    {
        warn!(container = %name, "stop_container blad (moze juz nie istnieje): {}", e);
    }
    if let Err(e) = docker
        .remove_container(
            &name,
            Some(bollard::query_parameters::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
    {
        warn!(container = %name, "remove_container blad: {}", e);
    }
    Ok(())
}

/// Cleanup wszystkich kontenerow meeting-bot* ktore zostaly po poprzednim
/// uruchomieniu tentaflow. Uzywane przy starcie procesu.
#[cfg(feature = "docker")]
pub async fn cleanup_stale_containers() -> Result<()> {
    use bollard::query_parameters::ListContainersOptions;
    use bollard::Docker;
    let docker = Docker::connect_with_local_defaults()?;
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("label".to_string(), vec!["tentaflow.kind=meeting-bot".to_string()]);
    let opts = ListContainersOptions {
        all: true,
        filters: Some(filters),
        ..Default::default()
    };
    let containers = docker
        .list_containers(Some(opts))
        .await
        .context("list_containers")?;
    for c in containers {
        if let Some(names) = &c.names {
            if let Some(first) = names.first() {
                let name = first.trim_start_matches('/');
                warn!("cleanup stale meeting-bot container: {}", name);
                let _ = docker
                    .remove_container(
                        name,
                        Some(bollard::query_parameters::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
        }
    }
    Ok(())
}

#[cfg(not(feature = "docker"))]
pub async fn spawn(_req: &SpawnRequest) -> Result<SpawnOutcome> {
    anyhow::bail!("feature `docker` wylaczone — Meeting Bot wymaga dockera")
}

#[cfg(not(feature = "docker"))]
pub async fn stop(_session_id: i64) -> Result<()> {
    Ok(())
}

#[cfg(not(feature = "docker"))]
pub async fn cleanup_stale_containers() -> Result<()> {
    Ok(())
}

fn build_env(req: &SpawnRequest) -> Vec<String> {
    let mut env = vec![
        format!("MEETING_URL={}", req.meeting_url),
        // Wewnątrz kontenera bot nasluchuje na 5000/udp niezależnie od portu
        // hosta — port-binding tylko mapuje zewnątrz.
        "TRANSPORT_PORT=5000".to_string(),
        format!("BOT_SECRET_KEY_HEX={}", req.secret_key_hex),
        format!("BOT_NAME={}", req.bot_name),
        "DISPLAY=:99".to_string(),
        "XDG_RUNTIME_DIR=/tmp/runtime".to_string(),
    ];
    if let Some(v) = &req.stt_alias {
        env.push(format!("STT_MODEL={}", v));
    }
    if let Some(v) = &req.tts_alias {
        env.push(format!("TTS_MODEL={}", v));
    }
    if let Some(v) = &req.llm_alias {
        env.push(format!("LLM_MODEL={}", v));
    }
    env
}
