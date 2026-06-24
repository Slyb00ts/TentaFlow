// =============================================================================
// File: protocol/ingest.rs — ingest-as-flow host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// `ingest_invoke_v1` host function (RAG Partia 3 prerequisite). The addon hands
// the host a `doc_id_blob` (a reference into the addon's per-instance document
// store) plus its mime, the flow model name and an opaque options JSON; the
// host loads the raw bytes itself, seeds a BINARY flow envelope and dispatches
// the `<model>:ingest` flow. Output carries the reconstructed markdown and the
// number of chunks the ingest flow persisted. Maps use integer keys via
// `#[cbor(map)]` + `#[n(N)]`.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payload
// -----------------------------------------------------------------------------

/// Input for `ingest_invoke_v1`. The raw document bytes do NOT cross the WASM
/// ABI here — `doc_id_blob` references the file already streamed into the
/// addon's per-instance document store (`document_put_v1`), so the host reads
/// the bytes once on its side (zero double-transfer of a multi-MB file).
/// `mime` is the document type (drives image vs generic-file seeding), `model`
/// selects the ingest flow (`<model>:ingest:<modality>`), and `options_json`
/// is an opaque JSON blob (collection_id, graph toggle, chunking params, …)
/// forwarded into the flow meta — the host validates it as JSON only.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct IngestInvokeInput {
    #[n(0)]
    pub doc_id_blob: String,
    #[n(1)]
    pub mime: String,
    #[n(2)]
    pub model: String,
    #[n(3)]
    pub options_json: Option<String>,
}

// -----------------------------------------------------------------------------
// Output payload
// -----------------------------------------------------------------------------

/// Output of `ingest_invoke_v1`. `markdown` is the document reconstruction the
/// ingest flow produced (parse → markdown); `chunks` is the number of chunks
/// the flow's store node persisted to the vector index. `page_count` mirrors
/// the parser's page tally (1 for a single image). The per-chunk text is NOT
/// carried here on purpose: a large document would overflow the 8 MiB ABI cap
/// (PayloadTooLarge), which would mark the ingest failed even though the vectors
/// were already persisted. The addon reads the chunk texts back from the
/// `passages` vector namespace by `doc_id` (the same source of truth it uses for
/// cleanup/delete) to run knowledge-graph extraction.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct IngestInvokeOutput {
    #[n(0)]
    pub markdown: String,
    #[n(1)]
    pub chunks: u32,
    #[n(2)]
    pub page_count: u32,
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
    fn roundtrip_input_with_and_without_options() {
        roundtrip(&IngestInvokeInput {
            doc_id_blob: "doc-abc123".into(),
            mime: "application/pdf".into(),
            model: "rag".into(),
            options_json: Some(r#"{"collection_id":"col-1","graph_enabled":true}"#.into()),
        });
        roundtrip(&IngestInvokeInput {
            doc_id_blob: "doc-xyz".into(),
            mime: "image/png".into(),
            model: "rag".into(),
            options_json: None,
        });
    }

    #[test]
    fn roundtrip_output() {
        roundtrip(&IngestInvokeOutput {
            markdown: "# Faktura\n\nKwota: 100".into(),
            chunks: 7,
            page_count: 3,
        });
    }
}
