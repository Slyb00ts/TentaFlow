// =============================================================================
// Plik: services/rag/mod.rs
// Opis: Modul odpowiedzialny za komunikacje z RAG engines przez QUIC + rkyv.
//       Obejmuje klienta QUIC, callback handling i auto-reconnect.
// =============================================================================

pub mod client;

pub use client::{RAGClient, RAGEngineConfigCompat};
