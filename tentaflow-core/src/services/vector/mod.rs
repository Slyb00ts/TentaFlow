// ============ File: services/vector/mod.rs — embedded vector storage (zvec) ============
//
// Per-addon per-namespace vector indexes backed by the embedded zvec engine
// (one zvec collection per namespace, persisted on disk). Addon-facing API is in
// `addon::host_functions::vector` (vector_*_v1). This module owns the trait
// abstraction, the zvec implementation, the (org, addon, namespace) -> Backend
// cache, and per-addon quotas.

pub mod backend;
pub mod doc_vectors;
pub mod error;
pub mod filter;
#[cfg(feature = "vector-milvus")]
pub mod milvus_backend;
pub mod namespace;
pub mod remote;
pub mod zvec_backend;

pub use backend::{Metric, SearchHit, VectorBackend};
pub use error::{Result as VectorResult, VectorError};
#[cfg(feature = "vector-milvus")]
pub use milvus_backend::MilvusBackend;
pub use namespace::{
    NamespaceManager, ReconcileReport, MAX_NAMESPACES_PER_ADDON, MAX_VECTORS_PER_ADDON,
};
pub use remote::RemoteVectorTransport;
pub use zvec_backend::ZvecBackend;
