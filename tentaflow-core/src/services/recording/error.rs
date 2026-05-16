// =============================================================================
// File: services/recording/error.rs — RecordingError enum
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("png encode failed: {0}")]
    PngEncode(String),
    #[error("gstreamer pipeline failed: {0}")]
    GstPipeline(String),
    #[error("invalid camera_id format")]
    InvalidCameraId,
    #[error("invalid retention_class: {0}")]
    InvalidRetentionClass(String),
    #[error("base directory unavailable: {0}")]
    BaseDirUnavailable(String),
    #[error("invalid dimensions: buffer {0} bytes != {1}x{2}x3")]
    InvalidDimensions(usize, u32, u32),
}

pub type Result<T> = std::result::Result<T, RecordingError>;
