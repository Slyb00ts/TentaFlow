// =============================================================================
// Plik: services/embeddings/mod.rs
// Opis: Modul odpowiedzialny za komunikacje z Embeddings engines przez QUIC + rkyv.
//       Obejmuje klienta QUIC do generowania wektorow z tekstu.
// =============================================================================

// TODO: Przeniesc EmbeddingsClient z Routera gdy plik client.rs zostanie
//       zaimplementowany w zrodle (TentaFlow.Router/src/embeddings/client.rs
//       jest zadeklarowany w mod.rs ale jeszcze nie istnieje).
pub mod client;

pub use client::{EmbeddingsClient, EmbeddingsEngineConfigCompat};
