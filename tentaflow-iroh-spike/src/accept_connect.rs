// =============================================================================
// Plik: src/accept_connect.rs
// Opis: Kryterium (a) — `iroh` linker check + module wiring.
//       Iroh API (0.28) jest pre-1.0 i zmienia sie czesto. Tu trzymamy tylko
//       trivial check ze iroh sie kompiluje i jest w build tree. Faktyczne
//       Endpoint accept/connect testy wymagaja inwentaryzacji aktualnego
//       iroh::net::Endpoint API gdy zaczniemy criterion (a) measurement.
//
// UWAGA: Pre-1.0 stability concern jest OSOBNYM input do decyzji #22 — jesli
// API zmienia sie co minor, kosztuje to migration overhead vs quinn (1.0+).
// =============================================================================

use anyhow::Result;

/// Smoke test: iroh sie linkuje, mozemy utworzyc NodeId z bajtow.
pub fn iroh_module_loads() -> bool {
    iroh::base::key::NodeId::from_bytes(&[0u8; 32]).is_ok()
}

/// Stub: pelne accept/connect implementation czeka na inwentaryzacje
/// iroh::net::Endpoint API w aktualnej wersji.
pub async fn build_endpoint_stub() -> Result<()> {
    Ok(())
}
