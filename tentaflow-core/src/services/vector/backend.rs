// ============ File: services/vector/backend.rs — VectorBackend trait + Metric enum ============
//
// Trait abstraction so that future fallbacks (`hnsw_rs` for mobile when
// cross-compiling usearch's C++ core proves too painful, or `QdrantBackend`
// when the embedded path runs out of headroom in F2+) can drop in without
// touching the host functions. F1c ships exactly one implementation:
// `UsearchBackend`.

use std::sync::Arc;

use super::error::Result;

/// Distance metric understood by the backend. Wire string form matches the
/// manifest enum used in `[[vector_namespace]].distance` plus the
/// `addon_vector_namespaces.metric` CHECK constraint (cosine | euclidean | dot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cosine,
    Euclidean,
    Dot,
}

impl Metric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::Euclidean => "euclidean",
            Self::Dot => "dot",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cosine" => Some(Self::Cosine),
            "euclidean" => Some(Self::Euclidean),
            "dot" => Some(Self::Dot),
            _ => None,
        }
    }
}

/// One result row from a k-NN search. `score` is the raw metric distance
/// returned by the backend (lower = closer for cosine/euclidean; `1 - dot`
/// for dot). Callers that want a 0..1 similarity must convert per metric.
#[derive(Debug, Clone, Copy)]
pub struct SearchHit {
    pub ref_id: u64,
    pub score: f32,
}

/// Per-namespace backend. Implementations must be cheap to clone (typically
/// `Arc<Self>` wrapping an internal lock around the native handle). All
/// operations are synchronous because usearch's native methods are not async
/// and run in O(log N) time on a single thread — fast enough that we do not
/// need to ship them off to a blocking pool for F1c scale (<=1M vectors).
pub trait VectorBackend: Send + Sync {
    /// Insert or replace the vector under `ref_id`. usearch enforces unique
    /// keys when the index is created with `multi=false` (our default).
    fn upsert(&self, ref_id: u64, vector: &[f32]) -> Result<()>;

    /// Top-k k-NN search; returns at most `k` hits ordered by ascending
    /// distance (closest first).
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<SearchHit>>;

    /// Remove the vector under `ref_id`. Returns `true` if the key existed.
    fn delete(&self, ref_id: u64) -> Result<bool>;

    /// Current vector count (authoritative, queried from the native index).
    fn count(&self) -> u64;

    /// Persist the index to disk. F1c calls this after every successful
    /// upsert/delete so a crash does not lose committed writes; F2 may
    /// switch to a batched policy.
    fn save(&self) -> Result<()>;

    /// Geometry of this index — used by the namespace manager to validate
    /// that addon-supplied vectors match the declared dimension.
    fn dim(&self) -> u32;
    fn metric(&self) -> Metric;
}

pub type DynBackend = Arc<dyn VectorBackend>;
