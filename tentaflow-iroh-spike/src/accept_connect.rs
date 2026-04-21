// =============================================================================
// Plik: src/accept_connect.rs
// Opis: Kryterium (a) — `iroh_net::Endpoint` accept/connect.
//       Iroh API (0.28) jest pre-1.0; iroh_net::endpoint::Endpoint to
//       wlasciwa abstrakcja dla QUIC accept/connect (bez relay/discovery).
//
// UWAGA: Pre-1.0 stability concern jest OSOBNYM input do decyzji #22 — jesli
// API zmienia sie co minor, kosztuje to migration overhead vs quinn (1.0+).
// =============================================================================

use anyhow::Result;
#[allow(deprecated)]
use iroh_net::endpoint::Endpoint;

use crate::MESH_ALPN;

/// Smoke test: iroh sie linkuje, mozemy utworzyc NodeId z bajtow.
pub fn iroh_module_loads() -> bool {
    iroh::base::key::NodeId::from_bytes(&[0u8; 32]).is_ok()
}

/// Buduje Endpoint z naszym mesh ALPN, bez discovery service (manual addr).
///
/// UWAGA: iroh-net jest deprecated w 0.28 — przeniesione do iroh::net.
/// Pre-1.0 instability = data point dla decyzji #22.
#[allow(deprecated)]
pub async fn build_endpoint() -> Result<Endpoint> {
    let endpoint = Endpoint::builder()
        .alpns(vec![MESH_ALPN.to_vec()])
        .bind()
        .await?;
    Ok(endpoint)
}

/// Stub: dla compatibility z criterion_a test (nie usuwamy nazwy).
pub async fn build_endpoint_stub() -> Result<()> {
    Ok(())
}
