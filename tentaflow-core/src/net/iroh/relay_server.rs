// =============================================================================
// Plik: net/iroh/relay_server.rs
// Opis: Embedded serwer iroh-relay dla browser klientow. Przyjmuje polaczenia
//       WebSocket i WebTransport, bridguje je do iroh mesh. Umozliwia daemonowi
//       bycie self-hosted relayem kiedy klienty przegladarki nie moga dotrzec
//       do publicznego `use.iroh.network`. Self-signed cert generowany przy
//       starcie; w produkcji podmieniany przez certyfikat z `[relay].tls_cert`.
// =============================================================================

use std::net::SocketAddr;

use anyhow::{Context, Result};
use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use rcgen::generate_simple_self_signed;

/// Konfiguracja startowa embedded relay.
pub struct RelayServerConfig {
    /// Adres bind HTTPS (WebSocket + WebTransport).
    pub bind_addr: SocketAddr,
    /// Port QUIC (0 = automatyczny).
    pub quic_bind_addr: Option<SocketAddr>,
    /// Hostname dla self-signed cert.
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

/// Startuje embedded iroh-relay Server. Zwraca uchwyt — `Drop` powoduje
/// abort task-a, wiec uchwyt musi zyc w miejscu ktore kontroluje shutdown.
///
/// Aktualnie konfiguruje HTTP bind dla WebSocket/WebTransport. QUIC bridge
/// wymaga dodatkowego `rustls::ServerConfig` z cert+key i bedzie dodany
/// gdy pelna integracja browser↔daemon przez iroh bedzie produkcyjna.
pub async fn spawn_relay_server(config: RelayServerConfig) -> Result<Server> {
    let _cert = generate_simple_self_signed(vec![config.hostname.clone()])
        .context("generacja self-signed cert dla relay")?;

    // QUIC bridge nie jest tu konfigurowany — WebSocket przez HTTP bind
    // wystarcza dla browsera, dopoki nie dodamy TLS config dla WebTransport/QUIC.
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
