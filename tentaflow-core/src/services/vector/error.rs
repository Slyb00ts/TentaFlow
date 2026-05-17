// ============ File: services/vector/error.rs — vector backend errors (F1c P3) ============
//
// Single error enum shared by the trait + the usearch implementation + the
// namespace manager. Mapped to `addon::errors::AbiError` by the host function
// dispatcher so the i32 codes returned to addons stay aligned with the rest
// of the ABI.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VectorError {
    #[error("namespace not found: addon_id={addon_id} namespace={namespace}")]
    NamespaceNotFound { addon_id: String, namespace: String },

    #[error("namespace already exists: addon_id={addon_id} namespace={namespace}")]
    NamespaceExists { addon_id: String, namespace: String },

    #[error("dimension mismatch: namespace expects {expected}, got {actual}")]
    DimMismatch { expected: u32, actual: u32 },

    #[error("invalid dimension {0} (allowed range 1..=4096)")]
    InvalidDim(u32),

    #[error("metric mismatch: namespace uses {expected}, request asked for {actual}")]
    MetricMismatch { expected: &'static str, actual: String },

    #[error("invalid namespace name '{0}' (must match ^[a-z0-9_-]{{1,64}}$)")]
    InvalidNamespaceName(String),

    #[error("invalid ref_id 0 (vector keys must be >= 1)")]
    InvalidRefId,

    #[error("empty vector payload")]
    EmptyVector,

    #[error("quota exceeded: addon {addon_id} already has {current} namespaces (max {max})")]
    NamespaceQuotaExceeded {
        addon_id: String,
        current: u32,
        max: u32,
    },

    #[error("quota exceeded: addon {addon_id} reached {current} vectors total (max {max})")]
    VectorQuotaExceeded {
        addon_id: String,
        current: u64,
        max: u64,
    },

    #[error("io error at {path:?}: {source}")]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    #[error("usearch error: {0}")]
    Backend(String),

    #[error("database error: {0}")]
    Db(String),
}

pub type Result<T> = std::result::Result<T, VectorError>;
