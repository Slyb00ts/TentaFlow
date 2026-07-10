// =============================================================================
// File: protocol/vector.rs — vector storage host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// three `vector_*_v1` host functions. Shared verbatim by the core host (decode
// input / encode output) and the addon SDK (encode input / decode output) so
// the wire format cannot drift. Vectors themselves cross the wire as
// base64-encoded little-endian f32 bytes inside the string fields. Maps use
// integer keys via `#[cbor(map)]` + `#[n(N)]`.
// =============================================================================

use minicbor::{Decode, Encode};

use super::vector_query::{Field, Filter, Fusion, SparseVector};

// -----------------------------------------------------------------------------
// Input payloads
// -----------------------------------------------------------------------------

/// Input for `vector_upsert_v1`. `vector_b64` is base64 of LE f32 bytes.
/// `fields` carries the typed metadata values for this vector; they must match
/// the namespace's declared `[[vector_namespace]].fields` schema. `Option` on
/// the wire (a missing map key decodes to `None`) so older addons that send no
/// metadata stay compatible.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorUpsertInput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub ref_id: u64,
    #[n(2)]
    pub vector_b64: String,
    #[n(3)]
    pub fields: Option<Vec<Field>>,
    /// Optional sparse vector stored alongside the dense one (for hybrid search).
    /// Only valid when the namespace declares `sparse = true` in its manifest.
    #[n(4)]
    pub sparse: Option<SparseVector>,
}

/// Input for `vector_hybrid_search_v1` — combined dense + sparse retrieval fused
/// into one ranking. The namespace must declare `sparse = true`. `dense_b64` is
/// base64 of LE f32 bytes (same as `vector_search_v1`); `sparse` is the sparse
/// query vector. `fusion` selects how the two result lists are merged.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorHybridSearchInput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub dense_b64: String,
    #[n(2)]
    pub sparse: SparseVector,
    #[n(3)]
    pub k: u32,
    #[n(4)]
    pub gate_claim_id: Option<String>,
    #[n(5)]
    pub filter: Option<Filter>,
    #[n(6)]
    pub output_fields: Option<Vec<String>>,
    /// Fusion strategy; absent = RRF with the conventional rank constant 60.
    #[n(7)]
    pub fusion: Option<Fusion>,
}

/// Input for `vector_search_v1`. `gate_claim_id` is required only when the
/// namespace declares a `gate` in the manifest; it is `Option` on the wire so a
/// non-gated query can omit it.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorSearchInput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub query_b64: String,
    #[n(2)]
    pub k: u32,
    #[n(3)]
    pub gate_claim_id: Option<String>,
    /// Optional metadata pre-filter (the universal AST). The core translates it
    /// to the selected backend's native expression.
    #[n(4)]
    pub filter: Option<Filter>,
    /// Names of declared metadata fields to return alongside each hit. Empty /
    /// absent = return only `ref_id` + `score`.
    #[n(5)]
    pub output_fields: Option<Vec<String>>,
}

/// Input for `vector_delete_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorDeleteInput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub ref_id: u64,
}

// -----------------------------------------------------------------------------
// Output payloads
// -----------------------------------------------------------------------------

/// Output of `vector_upsert_v1`. `count` is the post-upsert vector count.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorUpsertOutput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub ref_id: u64,
    #[n(2)]
    pub count: u64,
}

/// One k-NN hit in `vector_search_v1`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorSearchHit {
    #[n(0)]
    pub ref_id: u64,
    #[n(1)]
    pub score: f32,
    /// Returned metadata fields (those requested via `output_fields`). Absent
    /// when no output fields were requested.
    #[n(2)]
    pub fields: Option<Vec<Field>>,
}

/// Output of `vector_search_v1` — top-k, closest first.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorSearchOutput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub hits: Vec<VectorSearchHit>,
}

/// Output of `vector_delete_v1`. `removed` is true when the key existed.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorDeleteOutput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub ref_id: u64,
    #[n(2)]
    pub removed: bool,
    #[n(3)]
    pub count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_upsert_input() {
        roundtrip(&VectorUpsertInput {
            namespace: "faces".into(),
            ref_id: 42,
            vector_b64: "AAAAAA==".into(),
            fields: Some(vec![Field {
                name: "source".into(),
                value: crate::FieldValue::Str("inbox".into()),
            }]),
            sparse: Some(crate::SparseVector {
                indices: vec![3, 17, 902],
                values: vec![0.5, 1.2, 0.8],
            }),
        });
        roundtrip(&VectorUpsertInput {
            namespace: "faces".into(),
            ref_id: 7,
            vector_b64: "AAAAAA==".into(),
            fields: None,
            sparse: None,
        });
    }

    #[test]
    fn roundtrip_search_input_gated_and_open() {
        roundtrip(&VectorSearchInput {
            namespace: "faces".into(),
            query_b64: "AAAAAA==".into(),
            k: 10,
            gate_claim_id: Some("claim_1".into()),
            filter: Some(Filter::Eq(
                "source".into(),
                crate::FieldValue::Str("inbox".into()),
            )),
            output_fields: Some(vec!["source".into()]),
        });
        roundtrip(&VectorSearchInput {
            namespace: "faces".into(),
            query_b64: "AAAAAA==".into(),
            k: 5,
            gate_claim_id: None,
            filter: None,
            output_fields: None,
        });
    }

    #[test]
    fn roundtrip_hybrid_search_input() {
        roundtrip(&VectorHybridSearchInput {
            namespace: "documents".into(),
            dense_b64: "AAAAAA==".into(),
            sparse: crate::SparseVector {
                indices: vec![1, 88, 30012],
                values: vec![0.9, 2.1, 3.4],
            },
            k: 10,
            gate_claim_id: None,
            filter: Some(Filter::Eq(
                "source".into(),
                crate::FieldValue::Str("inbox".into()),
            )),
            output_fields: Some(vec!["source".into()]),
            fusion: Some(crate::Fusion::Rrf(60)),
        });
        roundtrip(&VectorHybridSearchInput {
            namespace: "documents".into(),
            dense_b64: "AAAAAA==".into(),
            sparse: crate::SparseVector {
                indices: vec![1],
                values: vec![1.0],
            },
            k: 5,
            gate_claim_id: None,
            filter: None,
            output_fields: None,
            fusion: Some(crate::Fusion::Weighted(0.7, 0.3)),
        });
    }

    #[test]
    fn roundtrip_search_output() {
        roundtrip(&VectorSearchOutput {
            namespace: "faces".into(),
            hits: vec![
                VectorSearchHit {
                    ref_id: 1,
                    score: 0.987,
                    fields: Some(vec![Field {
                        name: "source".into(),
                        value: crate::FieldValue::Str("inbox".into()),
                    }]),
                },
                VectorSearchHit {
                    ref_id: 7,
                    score: 0.51,
                    fields: None,
                },
            ],
        });
    }

    #[test]
    fn roundtrip_delete_output() {
        roundtrip(&VectorDeleteOutput {
            namespace: "faces".into(),
            ref_id: 42,
            removed: true,
            count: 3,
        });
    }
}
