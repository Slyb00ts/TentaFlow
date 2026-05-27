// =============================================================================
// File: services/stream_hub/error.rs — StreamHubError variants
// =============================================================================

/// Errors produced by the stream hub. Reason strings are static so callers can
/// match on the variant without parsing the message.
#[derive(Debug, thiserror::Error)]
pub enum StreamHubError {
    #[error("stream source not registered: {0}")]
    NotRegistered(String),

    #[error("source factory failed: {0}")]
    FactoryFailed(String),

    #[error("source already active")]
    AlreadyActive,

    #[error("backpressure: subscriber lagged")]
    SubscriberLagged,
}
