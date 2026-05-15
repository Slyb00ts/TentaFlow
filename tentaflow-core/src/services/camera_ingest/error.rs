// =============================================================================
// File: services/camera_ingest/error.rs — error type for camera ingest layer
// =============================================================================

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CameraIngestError {
    #[error("unsupported vendor: {0}")]
    UnsupportedVendor(String),
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("symlink not allowed: {0}")]
    SymlinkNotAllowed(String),
    #[error("camera already exists: {0}")]
    AlreadyExists(String),
    #[error("camera not found: {0}")]
    NotFound(String),
    #[error("gstreamer init failed: {0}")]
    GstInit(String),
    #[error("pipeline build failed: {0}")]
    PipelineBuild(String),
    #[error("pipeline state error: {0}")]
    PipelineState(String),
    #[error("session crashed: {0}")]
    SessionCrashed(String),
    #[error("snapshot timeout")]
    SnapshotTimeout,
    #[error("snapshot failed: {0}")]
    SnapshotFailed(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),
}

pub type Result<T> = std::result::Result<T, CameraIngestError>;
