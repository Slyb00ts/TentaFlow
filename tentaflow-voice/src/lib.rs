// =============================================================================
// Plik: lib.rs
// Opis: Pure Rust library do VAD i speaker embeddings — bez ort/onnxruntime.
//       Ladowanie wag z plikow .onnx, forward pass recznie zaimplementowany
//       z optymalizacjami SIMD przez crate `wide`.
// =============================================================================

pub mod error;
pub mod fbank;
pub mod onnx_loader;
pub mod ops;
pub mod silero_vad;
pub mod wespeaker;

mod generated {
    include!("generated/onnx.rs");
}

pub use error::{VoiceError, VoiceResult};
pub use fbank::compute_fbank;
pub use onnx_loader::OnnxWeights;
pub use silero_vad::{SileroVad, SileroVadStreaming};
pub use wespeaker::{cosine_similarity, LayerTimings, WeSpeaker};
