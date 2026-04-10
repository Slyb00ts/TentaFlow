// =============================================================================
// Plik: diarization/embedding.rs
// Opis: Wrapper na tentaflow_voice::WeSpeaker — pure Rust speaker embedding
//       extraction. Zastepuje wczesniejsza implementacje przez ort.
// =============================================================================

use crate::diarization::error::DiarizationError;
use std::path::Path;
use tentaflow_voice::WeSpeaker;

/// Re-export cosine similarity z tentaflow-voice
pub use tentaflow_voice::cosine_similarity;

/// Speaker embedding extractor — pure Rust WeSpeaker ECAPA-TDNN forward pass.
pub struct EmbeddingExtractor {
    model: WeSpeaker,
}

impl EmbeddingExtractor {
    /// Laduje model z pliku .onnx (tylko wagi, forward pass jest pure Rust)
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, DiarizationError> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let model = WeSpeaker::from_file(&path_str)
            .map_err(|e| DiarizationError::ModelLoad(format!("{}", e)))?;
        Ok(Self { model })
    }

    /// Ekstrahuje 192-dim embedding z audio 16kHz mono f32
    pub fn extract(&self, samples: &[f32]) -> Result<Vec<f32>, DiarizationError> {
        self.model
            .extract(samples)
            .map_err(|e| DiarizationError::Extract(format!("{}", e)))
    }
}
