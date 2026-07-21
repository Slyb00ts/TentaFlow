// ===== File: error.rs — unified error type for the FORGE stack =====

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ForgeError>;

#[derive(Debug, Error)]
pub enum ForgeError {
    #[error("device error: {0}")]
    Device(String),
    #[error("out of device memory: requested {requested} bytes, available {available}")]
    OutOfMemory { requested: usize, available: usize },
    #[error("kernel error: {0}")]
    Kernel(String),
    #[error("model format error: {0}")]
    Format(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    #[error("grammar error: {0}")]
    Grammar(String),
    #[error("scheduler error: {0}")]
    Scheduler(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}
