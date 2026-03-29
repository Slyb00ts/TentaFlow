// =============================================================================
// Plik: services/tts/mod.rs
// Opis: Modul Text-to-Speech — integracja syntezy mowy z pipeline routingu.
//       Konwertuje oczyszczony tekst na audio chunks streamowane do klienta.
// =============================================================================

pub mod client;
pub mod processor;

pub use client::{TTSClient, TTSConfigCompat};
pub use processor::{SynthesizeCallback, TTSBufferingProcessor};
