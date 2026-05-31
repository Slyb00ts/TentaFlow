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

// -----------------------------------------------------------------------------
// Input payloads
// -----------------------------------------------------------------------------

/// Input for `vector_upsert_v1`. `vector_b64` is base64 of LE f32 bytes.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct VectorUpsertInput {
    #[n(0)]
    pub namespace: String,
    #[n(1)]
    pub ref_id: u64,
    #[n(2)]
    pub vector_b64: String,
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
        });
    }

    #[test]
    fn roundtrip_search_input_gated_and_open() {
        roundtrip(&VectorSearchInput {
            namespace: "faces".into(),
            query_b64: "AAAAAA==".into(),
            k: 10,
            gate_claim_id: Some("claim_1".into()),
        });
        roundtrip(&VectorSearchInput {
            namespace: "faces".into(),
            query_b64: "AAAAAA==".into(),
            k: 5,
            gate_claim_id: None,
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
                },
                VectorSearchHit {
                    ref_id: 7,
                    score: 0.51,
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
