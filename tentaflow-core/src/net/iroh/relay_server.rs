// =============================================================================
// File: net/iroh/relay_server.rs
// Description: Embedded iroh-relay server for browser clients. It accepts
// WebSocket and WebTransport connections and bridges them into the iroh mesh.
// This lets the daemon act as a self-hosted relay when browsers cannot reach
// the public `use.iroh.network` endpoint.
// =============================================================================

use std::net::SocketAddr;

use anyhow::{Context, Result};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use rcgen::generate_simple_self_signed;

/// Startup configuration for the embedded relay.
pub struct RelayServerConfig {
    /// HTTPS bind address for WebSocket and WebTransport.
    pub bind_addr: SocketAddr,
    /// QUIC bind address. Port 0 means automatic allocation.
    pub quic_bind_addr: Option<SocketAddr>,
    /// Hostname used for the self-signed certificate.
    pub hostname: String,
}

impl Default for RelayServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:4321".parse().unwrap(),
            quic_bind_addr: None,
            hostname: "localhost".to_string(),
        }
    }
}

/// Starts the embedded iroh-relay server and returns its handle. Dropping the
/// handle aborts the background task, so the owner must control shutdown.
///
/// The current implementation only configures the HTTP bind used by
/// WebSocket/WebTransport. A full QUIC bridge still needs an explicit
/// `rustls::ServerConfig` with certificate and key material.
pub async fn spawn_relay_server(config: RelayServerConfig) -> Result<Server> {
    let _cert = generate_simple_self_signed(vec![config.hostname.clone()])
        .context("failed to generate self-signed relay certificate")?;

    // The QUIC bridge is not configured here yet. The HTTP bind is enough for
    // browser traffic until WebTransport/QUIC gets explicit TLS wiring.
    let _ = config.quic_bind_addr;

    let server_config: ServerConfig<(), ()> = ServerConfig {
        relay: Some(RelayConfig {
            http_bind_addr: config.bind_addr,
            tls: None,
            limits: Default::default(),
            key_cache_capacity: Some(1024),
            access: iroh_relay::server::AccessConfig::Everyone,
        }),
        quic: None,
        metrics_addr: None,
    };

    Server::spawn(server_config)
        .await
        .map_err(|e| anyhow::anyhow!("iroh-relay spawn: {e:?}"))
}
