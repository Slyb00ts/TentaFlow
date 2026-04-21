// =============================================================================
// Plik: tentaflow-iroh-spike/src/lib.rs
// Opis: Throwaway evaluation crate dla iroh QUIC jako potencjalnej alternatywy
//       dla quinn (Tasks #14-#22). Cel: zmierzyc 7 kryteriow akceptacyjnych
//       (a-g) opisanych w design doc i podjac decyzje (#22) commit OR cut.
//
//       Kazde kryterium ma osobny test integracyjny w tests/criterion_*.rs.
//       Soak/throughput tests sa #[ignore] — wymagaja wall-clock time.
//
//       SCOPE: TYLKO ocena. Implementacja produkcyjna w mesh/quic_mesh.rs
//       zaczyna sie po pozytywnej decyzji #22.
// =============================================================================

pub mod accept_connect;
pub mod epoch_rotation;
pub mod pairing;
pub mod replay;
pub mod trust_state;

/// Nazwa ALPN dla mesh peers — taka sama jak w produkcji `tentaflow-mesh`.
pub const MESH_ALPN: &[u8] = b"tentaflow-mesh";

/// Domyslny rozmiar PIN dla pairing (6 cyfr ASCII).
pub const PIN_LENGTH: usize = 6;
