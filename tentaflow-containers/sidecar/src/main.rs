// =============================================================================
// Plik: main.rs
// Opis: Punkt wejscia generycznego sidecara TentaFlow. Laduje konfiguracje,
//       uruchamia QUIC server i dispatchuje requesty do handlera wybranego
//       przez `role` w config.toml.
// =============================================================================

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use tentaflow_sidecar::config::{Role, SidecarConfig};
use tentaflow_sidecar::roles;

#[derive(Parser, Debug)]
#[command(name = "tentaflow-sidecar")]
#[command(about = "Generyczny sidecar QUIC dla kontenerow TentaFlow")]
struct Args {
    /// Sciezka do pliku konfiguracji. Jesli nie istnieje, probuje /data/config.toml
    /// a potem /app/config.default.toml.
    #[arg(short, long, default_value = "/data/config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Blad instalacji CryptoProvider");

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let config_path = resolve_config_path(&args.config);
    tracing::info!(path = %config_path.display(), "Laduje konfiguracje sidecara");

    let config = SidecarConfig::load(&config_path)?;
    tracing::info!(service = %config.service_name, port = config.quic.port, "Sidecar start");

    match &config.role {
        Role::ReverseProxy { upstream_url, api, .. } => {
            tracing::info!(upstream = %upstream_url, api = ?api, "Rola: ReverseProxy");
            roles::reverse_proxy::run(config).await?;
        }
        Role::OnnxInProcess { model_path, task } => {
            tracing::info!(model = %model_path, task = ?task, "Rola: OnnxInProcess (TODO impl)");
            anyhow::bail!("Rola OnnxInProcess nie jest jeszcze zaimplementowana");
        }
        Role::TeamsBot => {
            tracing::info!("Rola: TeamsBot (TODO — migracja z crate tentaflow-teams-bot)");
            anyhow::bail!("Rola TeamsBot wymaga migracji istniejacego kodu z tentaflow-teams-bot");
        }
    }

    Ok(())
}

/// Rozwiazuje faktyczna sciezke config — preferuje argument CLI, potem
/// /data/config.toml (volume mount), potem /app/config.default.toml (wbudowane).
fn resolve_config_path(requested: &PathBuf) -> PathBuf {
    if requested.exists() {
        return requested.clone();
    }
    for fallback in ["/data/config.toml", "/app/config.default.toml"] {
        let p = PathBuf::from(fallback);
        if p.exists() {
            tracing::warn!(
                "Config {} nie istnieje, uzywam fallback: {}",
                requested.display(),
                p.display()
            );
            return p;
        }
    }
    requested.clone()
}
