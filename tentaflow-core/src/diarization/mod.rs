// =============================================================================
// Plik: diarization/mod.rs
// Opis: Speaker diarization — ekstrakcja embeddingow glosowych (WeSpeaker
//       ECAPA-TDNN, ONNX) + incremental clustering. Dostepne pod feature
//       `inference-diarization` (wymaga ort + mel_spec). Port ze Solutio.AI.STT.
// =============================================================================

pub mod embedding;
pub mod error;
pub mod service;
pub mod tracker;

pub use embedding::{cosine_similarity, EmbeddingExtractor};
pub use error::DiarizationError;
pub use service::{identify_speaker, reset_tracker};
pub use tracker::SpeakerTracker;
