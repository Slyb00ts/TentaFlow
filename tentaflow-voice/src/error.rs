// =============================================================================
// Plik: error.rs
// Opis: Typy bledow dla tentaflow-voice.
// =============================================================================

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoiceError {
    #[error("Blad I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("Blad parsowania ONNX: {0}")]
    OnnxParse(String),

    #[error("Brak tensora '{0}' w modelu")]
    MissingTensor(String),

    #[error("Niepoprawny shape tensora '{name}': oczekiwano {expected:?}, dostano {actual:?}")]
    ShapeMismatch {
        name: String,
        expected: Vec<usize>,
        actual: Vec<usize>,
    },

    #[error("Niepoprawny rozmiar wejscia: {0}")]
    InvalidInput(String),

    #[error("Nieobslugiwany typ danych: {0}")]
    UnsupportedDtype(String),
}

pub type VoiceResult<T> = Result<T, VoiceError>;

impl From<prost::DecodeError> for VoiceError {
    fn from(e: prost::DecodeError) -> Self {
        VoiceError::OnnxParse(format!("protobuf decode: {}", e))
    }
}
