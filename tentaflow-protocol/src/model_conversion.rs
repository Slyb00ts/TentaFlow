// =============================================================================
// File: model_conversion.rs
// Purpose: Wire types for the TF→ONNX model-conversion wizard step (ROADMAP
//          Z11). Deploying a TensorFlow model (SavedModel/H5) first has to go
//          through the `tf-onnx-converter` python-bundle service; conversion
//          runs the same ASYNC START + POLL contract already used for the
//          PyTorch→ONNX LLM export (`MlStudioFtExportRequest` /
//          `MlStudioFtExportStatusRequest`, `dispatch/ml_studio.rs` +
//          `ml_studio/export_llm.rs`): the start handler kicks off a
//          background task and answers immediately with `status="converting"`;
//          the status handler is a cheap DB read the wizard polls.
//
// State lives in `services.config_json` of the TARGET service row — reused
// per the ZADANIA.md Z11 allocation, no new table. Packed into one
// `ModelConversionPayload` inner enum so the whole surface burns one
// `MessageBody` discriminant slot (same pack pattern as events.rs / storage.rs).
//
// Append-only: new variants go at the END of `ModelConversionPayload` and new
// struct fields carry `#[serde(default)]`, so a peer that predates a field
// still decodes the message instead of failing the frame.
//
// `tolerance` travels WITH the start request rather than living as a server
// default: the caller (wizard) picks the acceptable numeric drift for the
// specific model being converted, and the PASS/FAIL decision against it is
// made by Core (`dispatch/model_conversion.rs::evaluate_tolerance`), not by
// the converter service — the service only reports the measured
// `max_abs_diff` it computed against a REAL test input (a synthetic/random
// comparison would give a false sense of safety, see ZADANIA.md Z11 pitfalls).
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Starts an async TF→ONNX conversion job for an existing `services` row
/// (`service_id`). `source_path` is a SavedModel directory or a `.h5` file on
/// the node's filesystem; `source_format` distinguishes the two so the
/// converter service does not have to sniff it. `tolerance` is the maximum
/// acceptable `max_abs_diff` between the original TF model and the converted
/// ONNX model on the converter's real test input.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelConversionStartRequest {
    pub service_id: i64,
    pub source_path: String,
    /// "tensorflow_savedmodel" | "tensorflow_h5".
    pub source_format: String,
    /// "fp32" | "fp16".
    pub precision: String,
    pub tolerance: f64,
    /// Path to a `.npy` file holding ONE real sample input, used by the
    /// converter to run a numeric-compatibility check between the original TF
    /// model and the converted ONNX model. `None` skips the check entirely —
    /// the conversion may still finish `succeeded`, but
    /// `ModelConversionStatusResponse::validated` is then explicitly `false`,
    /// never a silent pass (ZADANIA.md Z11 pitfall #2: a placeholder/absent
    /// comparison must not read as "close enough"). Added at the end of the
    /// struct, wire-compatible via `#[serde(default)]` — no `SCHEMA_VERSION`
    /// bump needed.
    #[serde(default)]
    pub test_input_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelConversionStartResponse {
    pub service_id: i64,
    /// Always "converting" on a successful start — the wizard polls
    /// `StatusRequest` for the terminal state.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelConversionStatusRequest {
    pub service_id: i64,
}

/// `status`: "none" (never started) | "converting" | "succeeded" | "failed"
/// (conversion itself errored, OR it converted but the measured
/// `max_abs_diff` exceeded the requested `tolerance` — both are a real
/// failure requiring the TF-serving fallback, not a warning; `error` carries
/// the reason either way).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelConversionStatusResponse {
    pub service_id: i64,
    pub status: String,
    #[serde(default)]
    pub onnx_path: Option<String>,
    #[serde(default)]
    pub max_abs_diff: Option<f64>,
    #[serde(default)]
    pub tolerance_passed: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
    /// Whether this conversion was actually numerically compared against the
    /// original TF model on a REAL test input and passed tolerance
    /// (`ModelConversionStartRequest::test_input_path` supplied AND
    /// `tolerance_passed == Some(true)`). A `succeeded` conversion with
    /// `validated=false` means an ONNX file exists but nobody compared it to
    /// the original model — the wizard must never render that the same as a
    /// validated pass.
    #[serde(default)]
    pub validated: bool,
}

/// The whole TF→ONNX conversion surface behind one
/// `MessageBody::ModelConversionBody`.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum ModelConversionPayload {
    StartRequest(ModelConversionStartRequest),
    StartResponse(ModelConversionStartResponse),
    StatusRequest(ModelConversionStatusRequest),
    StatusResponse(ModelConversionStatusResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn start_roundtrip() {
        let req = MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(
            ModelConversionStartRequest {
                service_id: 42,
                source_path: "/data/models/adr-classifier".into(),
                source_format: "tensorflow_savedmodel".into(),
                precision: "fp32".into(),
                tolerance: 0.001,
                test_input_path: None,
            },
        ));
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            crate::cbor::decode::<MessageBody>(&bytes).expect("decode"),
            req
        );

        let resp = MessageBody::ModelConversionBody(ModelConversionPayload::StartResponse(
            ModelConversionStartResponse {
                service_id: 42,
                status: "converting".into(),
            },
        ));
        let bytes = crate::cbor::encode(&resp).expect("encode");
        assert_eq!(
            crate::cbor::decode::<MessageBody>(&bytes).expect("decode"),
            resp
        );
    }

    /// `test_input_path` carries the real sample input path end to end, same
    /// as any other field — pinned separately from `start_roundtrip` (which
    /// covers the `None` / omitted case) so a regression in the `Some(..)`
    /// arm cannot hide behind the other test staying green.
    #[test]
    fn start_roundtrip_with_test_input_path() {
        let req = MessageBody::ModelConversionBody(ModelConversionPayload::StartRequest(
            ModelConversionStartRequest {
                service_id: 42,
                source_path: "/data/models/adr-classifier".into(),
                source_format: "tensorflow_savedmodel".into(),
                precision: "fp32".into(),
                tolerance: 0.001,
                test_input_path: Some("/data/models/adr-classifier/test_input.npy".into()),
            },
        ));
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            crate::cbor::decode::<MessageBody>(&bytes).expect("decode"),
            req
        );
    }

    #[test]
    fn status_roundtrip() {
        let req = MessageBody::ModelConversionBody(ModelConversionPayload::StatusRequest(
            ModelConversionStatusRequest { service_id: 42 },
        ));
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            crate::cbor::decode::<MessageBody>(&bytes).expect("decode"),
            req
        );

        let resp = MessageBody::ModelConversionBody(ModelConversionPayload::StatusResponse(
            ModelConversionStatusResponse {
                service_id: 42,
                status: "succeeded".into(),
                onnx_path: Some("/data/models/adr-classifier/model.onnx".into()),
                max_abs_diff: Some(0.0002),
                tolerance_passed: Some(true),
                error: None,
                validated: true,
            },
        ));
        let bytes = crate::cbor::encode(&resp).expect("encode");
        assert_eq!(
            crate::cbor::decode::<MessageBody>(&bytes).expect("decode"),
            resp
        );
    }

    /// A peer that predates a field must still decode the message, which is
    /// the whole reason the optional fields carry `#[serde(default)]`.
    #[test]
    fn status_response_decodes_without_the_defaulted_fields() {
        let bare = serde_json::json!({"service_id": 7, "status": "none"});
        let bytes = crate::cbor::encode(&bare).expect("encode");
        let decoded: ModelConversionStatusResponse = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded.service_id, 7);
        assert_eq!(decoded.status, "none");
        assert!(decoded.onnx_path.is_none());
        assert!(decoded.max_abs_diff.is_none());
        assert!(decoded.tolerance_passed.is_none());
        assert!(decoded.error.is_none());
        assert!(
            !decoded.validated,
            "a peer that predates `validated` must decode it as false, never a silent pass"
        );
    }

    /// Ciborium tags an externally-tagged enum by variant NAME, not by index —
    /// appending a variant is safe, renaming one is not. Pinned here rather
    /// than argued in a comment (mirrors `events::tests::
    /// message_body_is_tagged_by_variant_name`).
    #[test]
    fn message_body_is_tagged_by_variant_name() {
        let body = MessageBody::ModelConversionBody(ModelConversionPayload::StatusRequest(
            ModelConversionStatusRequest { service_id: 1 },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("ModelConversionBody"),
            "outer variant name must be on the wire"
        );
        assert!(
            text.contains("StatusRequest"),
            "inner variant name must be on the wire"
        );
    }
}
