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

pub use tentaflow_sdk_spec::{Field, FieldSpec, FieldValue, Filter, Fusion, SparseVector};

/// One item for a batch upsert: the vector under `ref_id` plus its typed
/// metadata `fields` and optional `sparse` vector.
pub struct UpsertItem<'a> {
    pub ref_id: u64,
    pub vector: &'a [f32],
    pub fields: &'a [Field],
    pub sparse: Option<&'a SparseVector>,
}

/// One result row from a k-NN search. `score` is the raw metric distance
/// returned by the backend (lower = closer for cosine/euclidean; `1 - dot`
/// for dot). `fields` carries the metadata requested via `output_fields`
/// (empty when none requested).
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub ref_id: u64,
    pub score: f32,
    pub fields: Vec<Field>,
}

/// Per-namespace backend. Implementations must be cheap to clone (typically
/// `Arc<Self>` wrapping an internal lock around the native handle). All
/// operations are synchronous because usearch's native methods are not async
/// and run in O(log N) time on a single thread — fast enough that we do not
/// need to ship them off to a blocking pool for F1c scale (<=1M vectors).
pub trait VectorBackend: Send + Sync {
    /// Insert or replace the vector under `ref_id`, with optional typed metadata
    /// `fields` (their names/types must be in the namespace's declared schema)
    /// and an optional `sparse` vector (only valid when the namespace was created
    /// with sparse support, for hybrid search).
    fn upsert(
        &self,
        ref_id: u64,
        vector: &[f32],
        fields: &[Field],
        sparse: Option<&SparseVector>,
    ) -> Result<()>;

    /// Insert/replace many vectors in one go, persisting to disk ONCE at the end.
    /// A flush fsyncs the whole growing index, so a per-element flush makes a
    /// bulk ingest O(n) full-index syncs (a 305-chunk document = 305 flushes of
    /// an index that keeps growing → minutes instead of seconds). Embedded
    /// backends override this to upsert without per-element flush and flush once.
    /// The default loops `upsert` (each flushes) — fine for remote/in-memory
    /// backends where there is no local fsync cost. On error the partial writes
    /// are NOT rolled back here; the caller owns cleanup-on-failure.
    fn upsert_batch(&self, items: &[UpsertItem<'_>]) -> Result<()> {
        for it in items {
            self.upsert(it.ref_id, it.vector, it.fields, it.sparse)?;
        }
        Ok(())
    }

    /// Top-k k-NN search; returns at most `k` hits ordered by ascending distance
    /// (closest first). `filter` restricts results by metadata (the backend
    /// translates the universal AST to its native syntax); `output_fields` lists
    /// metadata field names to return on each hit.
    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
        output_fields: &[String],
    ) -> Result<Vec<SearchHit>>;

    /// Hybrid dense + sparse search fused with `fusion` (RRF or weighted). Only
    /// valid when the namespace was created with sparse support. `filter` and
    /// `output_fields` behave as in `search`.
    fn hybrid_search(
        &self,
        dense: &[f32],
        sparse: &SparseVector,
        k: usize,
        filter: Option<&Filter>,
        output_fields: &[String],
        fusion: Fusion,
    ) -> Result<Vec<SearchHit>>;

    /// Remove the vector under `ref_id`. Returns `true` if the key existed.
    /// Implementations MUST persist to disk before returning Ok so that a
    /// successful return implies durability — callers rely on this to expose
    /// "delete succeeded" upstream without a separate save step.
    fn delete(&self, ref_id: u64) -> Result<bool>;

    /// True when `ref_id` is currently stored. Used by the namespace manager
    /// to decide whether an `upsert` is a replace (no quota delta) or a true
    /// insert (must be counted against the per-addon vector cap).
    fn has_ref(&self, ref_id: u64) -> bool;

    /// Current vector count (authoritative, queried from the native index).
    fn count(&self) -> u64;

    /// Persist the index to disk. The upsert/delete paths call `save()`
    /// internally before returning success, so external callers only need
    /// this for explicit flush points (tests, shutdown hooks).
    fn save(&self) -> Result<()>;

    /// Make the live collection's metadata schema equal `desired` (schema
    /// reconciliation on addon update). `stored` is the schema currently
    /// recorded for the namespace. Implementations apply this however their
    /// engine allows — embedded zvec rebuilds the collection (its online DDL is
    /// numeric-only), external Milvus adds the new columns online (and errors on
    /// a field removal, which Milvus cannot do online). A no-op `Ok` when the
    /// schema already matches.
    fn reconcile_fields(&self, stored: &[FieldSpec], desired: &[FieldSpec]) -> Result<()>;

    /// Geometry of this index — used by the namespace manager to validate
    /// that addon-supplied vectors match the declared dimension.
    fn dim(&self) -> u32;
    fn metric(&self) -> Metric;
}

pub type DynBackend = Arc<dyn VectorBackend>;
