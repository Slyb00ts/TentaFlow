// =============================================================================
// File: protocol/vector_query.rs — universal vector metadata + filter model
// Purpose: backend-agnostic types that an addon uses to attach typed metadata to
// vectors and to filter k-NN results. The core host translates these into the
// native form of the selected backend (zvec / Milvus); the addon never writes
// backend-specific filter syntax. CBOR (minicbor) so the SAME shapes are shared
// by the Rust host, the Rust addon SDK, and the generated Python / C# SDKs.
// =============================================================================

use minicbor::{Decode, Encode};

/// Declared type of a metadata field on a vector namespace. Both zvec and Milvus
/// require a typed column at collection-creation time; this is the universal type
/// the core maps onto each backend's column type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum FieldType {
    #[n(0)]
    Str,
    #[n(1)]
    Int,
    #[n(2)]
    Float,
    #[n(3)]
    Bool,
}

/// A typed metadata value attached to a vector (or used inside a filter).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub enum FieldValue {
    #[n(0)]
    Str(#[n(0)] String),
    #[n(1)]
    Int(#[n(0)] i64),
    #[n(2)]
    Float(#[n(0)] f64),
    #[n(3)]
    Bool(#[n(0)] bool),
}

/// One named metadata field on a vector document.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Field {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub value: FieldValue,
}

/// Declaration of one metadata field in a namespace schema (name + type +
/// whether it is indexed for filtering).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FieldSpec {
    #[n(0)]
    pub name: String,
    #[n(1)]
    pub field_type: FieldType,
    /// Build a scalar/inverted index so this field can be used in filters.
    #[n(2)]
    pub indexed: bool,
}

/// A sparse vector (e.g. BM25 / SPLADE term weights) as parallel index/value
/// arrays: `indices[i]` is the term/dimension id, `values[i]` its weight. The
/// addon produces this exactly as it produces the dense embedding; the core is
/// tokenizer-agnostic. `indices` and `values` MUST have equal length. Used for
/// hybrid (dense + sparse) search in RAG-style retrieval.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct SparseVector {
    #[n(0)]
    pub indices: Vec<u32>,
    #[n(1)]
    pub values: Vec<f32>,
}

/// How a hybrid search fuses the dense and sparse result lists into one ranking.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub enum Fusion {
    /// Reciprocal Rank Fusion — rank-only, no tuning. `k` is the rank constant
    /// (60 is the conventional default). Robust default for RAG.
    #[n(0)]
    Rrf(#[n(0)] u32),
    /// Weighted sum of normalized scores: `dense*dense_weight + sparse*sparse_weight`.
    #[n(1)]
    Weighted(#[n(0)] f32, #[n(1)] f32),
}

/// Backend-agnostic filter AST over metadata fields. The core translates it to
/// each backend's native expression; addons (Rust/Python/C#) only ever build
/// this tree. Comparison variants are `(field_name, value)`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub enum Filter {
    #[n(0)]
    Eq(#[n(0)] String, #[n(1)] FieldValue),
    #[n(1)]
    Ne(#[n(0)] String, #[n(1)] FieldValue),
    #[n(2)]
    Gt(#[n(0)] String, #[n(1)] FieldValue),
    #[n(3)]
    Gte(#[n(0)] String, #[n(1)] FieldValue),
    #[n(4)]
    Lt(#[n(0)] String, #[n(1)] FieldValue),
    #[n(5)]
    Lte(#[n(0)] String, #[n(1)] FieldValue),
    /// `field IN [values]`.
    #[n(6)]
    In(#[n(0)] String, #[n(1)] Vec<FieldValue>),
    #[n(7)]
    And(#[n(0)] Vec<Filter>),
    #[n(8)]
    Or(#[n(0)] Vec<Filter>),
    #[n(9)]
    Not(#[n(0)] Box<Filter>),
}
