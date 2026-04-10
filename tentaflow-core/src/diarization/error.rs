// =============================================================================
// Plik: diarization/error.rs
// Opis: Typy bledow dla diarization module.
// =============================================================================

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiarizationError {
    #[error("Blad ladowania modelu: {0}")]
    ModelLoad(String),

    #[error("Blad ekstrakcji embeddingu: {0}")]
    Extract(String),

    #[error("Niepoprawny rozmiar wejscia: {0}")]
    InvalidInput(String),
}
