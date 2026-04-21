// =============================================================================
// Plik: tentaflow-transport/src/error.rs
// Opis: `TransportError` — jedyny typ bledu wystawiany przez transport.
//       Wszystkie operacje (endpoint bind, connect, bidi open, frame IO) mapuja
//       sie na ten typ. Konsumenci owijaja go wlasnym typem bledu przez `From`.
// =============================================================================

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport: invalid config: {0}")]
    InvalidConfig(String),

    #[error("transport: endpoint bind failed: {0}")]
    EndpointBind(String),

    #[error("transport: connect failed: {0}")]
    Connect(String),

    #[error("transport: stream io failed: {0}")]
    Io(#[from] io::Error),

    #[error("transport: rkyv serialization failed: {0}")]
    Serialize(String),

    #[error("transport: rkyv deserialization failed: {0}")]
    Deserialize(String),

    #[error("transport: frame exceeds {limit} bytes (got {got})")]
    FrameTooLarge { limit: usize, got: usize },

    #[error("transport: peer closed stream before sending full frame")]
    PeerClosedEarly,

    #[error("transport: operation timed out after {ms} ms")]
    Timeout { ms: u64 },

    #[error("transport: connection closed: {0}")]
    ConnectionClosed(String),
}

impl TransportError {
    pub fn connect<E: std::fmt::Display>(e: E) -> Self {
        TransportError::Connect(e.to_string())
    }

    pub fn bind<E: std::fmt::Display>(e: E) -> Self {
        TransportError::EndpointBind(e.to_string())
    }

    pub fn invalid_config(msg: impl Into<String>) -> Self {
        TransportError::InvalidConfig(msg.into())
    }

    pub fn closed<E: std::fmt::Display>(e: E) -> Self {
        TransportError::ConnectionClosed(e.to_string())
    }
}
