// ============ File: services/vector/mod.rs — embedded HNSW vector storage (F1c P3) ============
//
// Per-addon per-namespace vector indexes backed by usearch (HNSW + mmap on
// disk). Addon-facing API is in `addon::host_functions::vector` (vector_*_v1).
// This module owns the trait abstraction, the usearch implementation, the
// (addon_id, namespace) -> Backend cache, and per-addon quotas.

pub mod backend;
pub mod error;
pub mod namespace;
pub mod usearch_backend;

pub use backend::{Metric, SearchHit, VectorBackend};
pub use error::{Result as VectorResult, VectorError};
pub use namespace::{NamespaceManager, MAX_NAMESPACES_PER_ADDON, MAX_VECTORS_PER_ADDON};
pub use usearch_backend::UsearchBackend;
