// =============================================================================
// Plik: web_research/error.rs
// Opis: Error type used by web research providers, HTTP reading and extraction.
// =============================================================================

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WebResearchError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("network policy denied: {0}")]
    PolicyDenied(String),

    #[error("http error: {0}")]
    Http(String),

    #[error("search provider error: {0}")]
    SearchProvider(String),

    #[error("extraction failed: {0}")]
    Extraction(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, WebResearchError>;
