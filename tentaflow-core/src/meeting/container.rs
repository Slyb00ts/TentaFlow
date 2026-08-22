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

#[cfg(feature = "docker")]
use anyhow::Context;
use anyhow::Result;
#[cfg(feature = "docker")]
use std::collections::HashMap;
#[cfg(feature = "docker")]
use tracing::{info, warn};

use super::port_pool::AllocatedPorts;

/// Tag obrazu bota — DOKLADNIE ten, ktory buduje deploy silnika `teams-bot`
/// (`services::deploy::docker`). Tag zawiera wersje z manifestu i hash kontekstu
/// budowania, wiec nie mozna go zapisac na sztywno: zmiana Dockerfile'a daje nowy
/// tag i staly `:latest` wskazywalby na obraz, ktorego deploy nigdy nie zbudowal.
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
fn image_tag() -> Result<String> {
    let manifest = bot_manifest()?;
    let docker = manifest.deploy.docker.as_ref().ok_or_else(|| {
        anyhow::anyhow!("manifest 'teams-bot' nie ma sekcji [deploy.docker]")
    })?;
    // Silnik z build-argsami dostaje tag per architektura GPU; odtworzenie go tutaj
    // wymagaloby powtorzenia calego wyboru build-argsow z deployu. Bot ich nie ma —
    // gdyby doszly, lepiej zatrzymac sie z czytelnym bledem niz szukac zlego tagu.
    if !docker.default_build_args.is_empty() || !docker.arch_variants.is_empty() {
        anyhow::bail!(
            "manifest 'teams-bot' deklaruje docker build-args — tag obrazu jest wtedy \
             zalezny od architektury GPU i musi byc pobrany z deployu, nie odtworzony"
        );
    }
    Ok(crate::services::deploy::docker::plain_image_tag(manifest))
}

/// Parametry startu kontenera Meeting Bot dla pojedynczej sesji.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub session_id: i64,
    pub meeting_url: String,
    /// Klucz sesji = meeting_sessions.meeting_key. Przekazywany botowi jako env
    /// MEETING_ID — każdy transkrypt router zapisze pod tym samym session_id.
    pub meeting_key: String,
    pub ports: AllocatedPorts,
    /// Ed25519 secret key bota (hex, 64 znaki). Host używa go żeby obliczyć
    /// EndpointId i połączyć się do bota via iroh.
    pub secret_key_hex: String,
    pub bot_name: String,
    /// The only router-side alias the bot resolves itself (periodic summary);
    /// STT/LLM/TTS/flow are resolved by Core per turn from the session row.
    pub summarization_alias: String,
    /// Czy bot ma odpowiadać w meetingu (LLM → TTS).
    pub respond_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SpawnOutcome {
    pub container_id: String,
    pub container_name: String,
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
    let image_tag = image_tag()?;
    let image_exists = docker.inspect_image(&image_tag).await.is_ok();
    if !image_exists {
        anyhow::bail!(
            "Obraz {} nie istnieje — zbuduj kontener teams-bot z Services (agents/teams-bot)",
            image_tag
        );
    }

    let name = super::container_name(req.session_id);
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
    // Model Silero VAD nie jest w obrazie — deploy pobral go raz do
    // `<models>/engine-assets/teams-bot/`, kontener widzi go read-only pod
    // `mount_path` z manifestu.
    let binds: Vec<String> = asset_binds()?
        .into_iter()
        .map(|(host, target, _)| format!("{}:{}:ro", host.display(), target))
        .collect();
    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        binds: if binds.is_empty() { None } else { Some(binds) },
        // Publish=all=false; używamy eksplicitnych bindings.
        auto_remove: Some(false),
        ..Default::default()
    };

    let config = Config {
        image: Some(image_tag.clone()),
        env: Some(env),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        labels: Some({
            let mut m = HashMap::new();
            m.insert(
                "tentaflow.meeting_session".to_string(),
                req.session_id.to_string(),
            );
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
    let name = super::container_name(session_id);
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
    filters.insert(
        "label".to_string(),
        vec!["tentaflow.kind=meeting-bot".to_string()],
    );
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

/// Manifest of the bot engine — source of truth for its required assets.
pub(super) fn bot_manifest() -> Result<&'static crate::services::manifest::ServiceManifest> {
    crate::services::manifest::registry()
        .by_id("teams-bot")
        .ok_or_else(|| anyhow::anyhow!("manifest 'teams-bot' nie istnieje w rejestrze silnikow"))
}

/// Read-only bind mounts for the bot's required assets. Fails loudly when a
/// file is missing — a bot started without the VAD model would silently fall
/// back to RMS detection.
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
fn asset_binds() -> Result<Vec<(std::path::PathBuf, String, bool)>> {
    let manifest = bot_manifest()?;
    crate::services::deploy::required_assets::container_binds(manifest)
        .map_err(|e| anyhow::anyhow!("Meeting Bot: {}", e))
}

#[cfg_attr(not(feature = "docker"), allow(dead_code))]
pub(super) fn build_env(req: &SpawnRequest) -> Vec<String> {
    let mut env = vec![
        format!("MEETING_URL={}", req.meeting_url),
        // Klucz sesji — bot kopiuje do każdego transkrypt eventu, router zapisuje
        // pod tym kluczem do meeting_sessions (get_or_create znajdzie naszą sesję).
        format!("MEETING_ID={}", req.meeting_key),
        // Wewnątrz kontenera bot nasluchuje na 5000/udp niezależnie od portu
        // hosta — port-binding tylko mapuje zewnątrz.
        "TRANSPORT_PORT=5000".to_string(),
        format!("BOT_SECRET_KEY_HEX={}", req.secret_key_hex),
        format!("BOT_NAME={}", req.bot_name),
        "DISPLAY=:99".to_string(),
        "XDG_RUNTIME_DIR=/tmp/runtime".to_string(),
        format!("SUMMARIZATION_ALIAS={}", req.summarization_alias),
        format!(
            "RESPOND_ENABLED={}",
            if req.respond_enabled { "true" } else { "false" }
        ),
    ];
    // Sciezki modeli w kontenerze (VAD_MODEL_PATH) — z manifestu, nie zgadywane.
    if let Ok(manifest) = bot_manifest() {
        for (key, value) in crate::services::deploy::required_assets::asset_env(manifest, true) {
            env.push(format!("{}={}", key, value));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::port_pool::AllocatedPorts;

    fn sample(sum: &str) -> SpawnRequest {
        SpawnRequest {
            session_id: 42,
            meeting_url: "https://teams.example/meet".to_string(),
            meeting_key: "mtg-xyz".to_string(),
            ports: AllocatedPorts {
                quic: 40001,
                vnc: 40002,
                novnc: 40003,
            },
            secret_key_hex: "deadbeef".to_string(),
            bot_name: "TF Bot".to_string(),
            summarization_alias: sum.to_string(),
            respond_enabled: false,
        }
    }

    /// Obraz startowany per spotkanie musi byc TYM, ktory buduje deploy silnika:
    /// wersja z manifestu + hash kontekstu budowania, nigdy staly `:latest`.
    #[test]
    fn image_tag_matches_engine_deploy_tag() {
        let manifest = bot_manifest().expect("manifest teams-bot jest wbudowany");
        let tag = image_tag().expect("tag obrazu");
        assert_eq!(
            tag,
            crate::services::deploy::docker::plain_image_tag(manifest)
        );
        assert!(
            tag.starts_with(&format!(
                "tentaflow/teams-bot:{}",
                manifest.engine.version
            )),
            "nieoczekiwany tag: {tag}"
        );
        assert!(!tag.ends_with(":latest"), "tag nie moze byc ruchomy: {tag}");
    }

    #[test]
    fn build_env_emits_keys_expected_by_bot() {
        let req = sample("teams-summarization");
        let env = build_env(&req);
        assert!(env.contains(&"SUMMARIZATION_ALIAS=teams-summarization".to_string()));
        assert!(env.contains(&"MEETING_URL=https://teams.example/meet".to_string()));
        assert!(env.contains(&"MEETING_ID=mtg-xyz".to_string()));
    }

    // STT/LLM/TTS/flow are resolved by Core per turn — the bot must not get
    // them, or a stale env would silently win over the session row.
    #[test]
    fn build_env_carries_no_pipeline_aliases() {
        let req = sample("my-sum");
        let env = build_env(&req);
        assert!(env.contains(&"SUMMARIZATION_ALIAS=my-sum".to_string()));
        for key in ["STT_ALIAS=", "TTS_ALIAS=", "LLM_ALIAS=", "FLOW_ALIAS="] {
            assert!(!env.iter().any(|e| e.starts_with(key)), "{key} leaked");
        }
    }
}
