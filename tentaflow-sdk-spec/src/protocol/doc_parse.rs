// =============================================================================
// File: protocol/doc_parse.rs — document-parse host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// `doc_parse_v1` host function (RAG E1.2). Shared verbatim by the core host
// (decode input / encode output) and the addon SDK (encode input / decode
// output) so the wire format cannot drift. The page image crosses the wire as
// a base64-encoded byte string inside `image_b64`. Maps use integer keys via
// `#[cbor(map)]` + `#[n(N)]`.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// Input payload
// -----------------------------------------------------------------------------

/// Input for `doc_parse_v1`. `image_b64` is base64 of the raw page image bytes
/// (PNG/JPEG); `mime` describes that encoding so the backend can decode it.
/// `model_alias` selects the vision parse service surface — defaults to
/// `"rag-parse"` (alias-aware failover, like `rag-reranker`) so addons need not
/// pin a concrete model. A missing map key decodes to `None`, so older addons
/// that omit the alias fall back to the default.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocParseInput {
    #[n(0)]
    pub image_b64: String,
    #[n(1)]
    pub mime: String,
    #[n(2)]
    pub model_alias: Option<String>,
}

// -----------------------------------------------------------------------------
// Output payloads
// -----------------------------------------------------------------------------

/// One layout block extracted from a parsed page. `class` is the detector's
/// layout label (`Text`, `Table`, `Picture`, …); `bbox` is `[x1, y1, x2, y2]`
/// in original-image pixel coordinates; `text` is the block's markdown/HTML
/// content; `confidence` is the detector score when the backend reports one
/// (vision parse services that do not score a block leave it absent → `None`).
/// `page` is the 0-based page index (always 0 for the single-image slice; the
/// PDF→image slice will populate it).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocBlock {
    #[n(0)]
    pub page: u32,
    #[n(1)]
    pub class: String,
    #[n(2)]
    pub bbox: [f32; 4],
    #[n(3)]
    pub text: String,
    #[n(4)]
    pub confidence: Option<f32>,
}

/// Output of `doc_parse_v1`. `markdown` is the whole-page reconstruction;
/// `blocks` is the per-region layout breakdown; `page_count` is the number of
/// pages parsed (always 1 for the single-image slice).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocParseOutput {
    #[n(0)]
    pub markdown: String,
    #[n(1)]
    pub blocks: Vec<DocBlock>,
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
    fn roundtrip_input_with_and_without_alias() {
        roundtrip(&DocParseInput {
            image_b64: "AAAAAA==".into(),
            mime: "image/png".into(),
            model_alias: Some("rag-parse".into()),
        });
        roundtrip(&DocParseInput {
            image_b64: "AAAAAA==".into(),
            mime: "image/jpeg".into(),
            model_alias: None,
        });
    }

    #[test]
    fn roundtrip_output_with_blocks() {
        roundtrip(&DocParseOutput {
            markdown: "# Faktura\n\nKwota: 100".into(),
            blocks: vec![
                DocBlock {
                    page: 0,
                    class: "Title".into(),
                    bbox: [10.0, 12.5, 200.0, 40.0],
                    text: "# Faktura".into(),
                    confidence: Some(0.98),
                },
                DocBlock {
                    page: 0,
                    class: "Text".into(),
                    bbox: [10.0, 50.0, 200.0, 80.0],
                    text: "Kwota: 100".into(),
                    confidence: None,
                },
            ],
            page_count: 1,
        });
    }

    #[test]
    fn roundtrip_empty_output() {
        roundtrip(&DocParseOutput {
            markdown: String::new(),
            blocks: Vec::new(),
            page_count: 1,
        });
    }
}
