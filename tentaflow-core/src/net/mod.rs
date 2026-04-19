// =============================================================================
// Plik: net/mod.rs
// Opis: Warstwa sieciowa. Jedyny transport to `iroh` (re-eksport z crate
//       `tentaflow-transport`). Submodul `iroh_client` to cienki wrapper
//       dostarczajacy stare nazwy (`QuicClient`, `QuicConfig`) dla kodu w
//       `routing/*` i `services/*`, zeby migracja nie wymuszala masowych zmian.
// =============================================================================

pub mod iroh;
pub mod iroh_client;

/// Alias zachowany dla istniejacych callerow — odwoluje sie do tych samych typow
/// co `iroh_client`.
pub use iroh_client as quic;
