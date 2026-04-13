// =============================================================================
// Plik: roles/reverse_proxy.rs
// Opis: Rola ReverseProxy — sidecar nasluchuje QUIC od routera, forwarduje
//       requesty do lokalnego HTTP API (vLLM, llama.cpp-server, sherpa).
//       Obsluguje tlumaczenie QUIC/rkyv ↔ HTTP/JSON zaleznie od typu API.
//       NA RAZIE skielet — QUIC server + handler-stubs, pelna implementacja
//       bedzie dopisana wraz z dedykowanymi translatorami per-API.
// =============================================================================

use anyhow::Result;

use crate::config::SidecarConfig;

pub async fn run(config: SidecarConfig) -> Result<()> {
    tracing::info!(
        service = %config.service_name,
        aliases = ?config.model_aliases,
        "ReverseProxy: start (skielet, pelna impl w nastepnej iteracji)"
    );

    // TODO: uruchomic QUIC server (szkielet z teams-bot/quic_server.rs, zgeneralizowany)
    // TODO: handler requestow -> tlumaczenie na lokalne HTTP zaleznie od config.role.api
    // TODO: streamowanie odpowiedzi SSE -> rkyv chunks
    // TODO: ServiceAnnounce po starcie

    // Na razie blokuj w petli keep-alive zeby kontener nie padl
    let shutdown = tokio::signal::ctrl_c();
    shutdown.await?;
    tracing::info!("Shutdown sygnal odebrany");
    Ok(())
}
