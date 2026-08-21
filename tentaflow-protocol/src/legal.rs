// =============================================================================
// File: legal.rs
// Purpose: Admin-side binary protocol for RODO/GDPR legal document management
//          (F2 P8.c). All RPCs are packed into a single `LegalAdminPayload`
//          inner enum so the whole legal surface occupies one `MessageBody`
//          variant — same pattern as `CameraAdminPayload` and
//          `ProfilingPayload`. (There is no 256-variant cap: tags are NAMES.)
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// One row in `legal_documents` projected onto the wire. Mirrors the columns
/// the dashboard list view needs without exposing the on-disk PDF path —
/// downloads always go through the signed-URL endpoint, never the listing.
#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentSummary {
    pub doc_id: String,
    pub org_id: String,
    /// Canonical lowercase variant string: `short` | `standard` | `full`.
    pub variant: String,
    /// Unix-ms timestamp the row was inserted.
    pub generated_at: i64,
    /// Membership id of the user that generated the PDF (TEXT, not legacy i64).
    pub generated_by_user_id: String,
    /// Blake3 hex of the on-disk PDF — 64 lowercase hex chars.
    pub content_hash: String,
    /// `revoked_at` unix-ms if the row has been soft-deleted, otherwise 0.
    /// `0` is the sentinel for "active" so the wire shape stays fixed.
    pub revoked_at_ms: i64,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentsListRequest {
    /// When `true`, soft-deleted rows are included. Default `false` matches
    /// the default dashboard view.
    pub include_revoked: bool,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentsListResponse {
    pub documents: Vec<LegalDocumentSummary>,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentGenerateRequest {
    /// Canonical lowercase variant string: `short` | `standard` | `full`.
    /// Validated against `services::legal::RodoVariant::from_str` server-side.
    pub variant: String,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentGenerateResponse {
    pub doc_id: String,
    /// 64 lowercase hex chars (blake3 hex of the on-disk PDF).
    pub content_hash: String,
    /// Fully-formed signed download URL — relative to the server root,
    /// `/legal/<doc_id>?token=...&exp=...&org=...&nonce=...`. Multi-use within
    /// the 1 h TTL. Clients render this as a download link.
    pub signed_url: String,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentRevokeRequest {
    pub doc_id: String,
}

#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub struct LegalDocumentRevokeResponse {
    pub doc_id: String,
    pub revoked_at_ms: i64,
}

/// Inner-enum pack — keeps every admin legal RPC in a single
/// `MessageBody::LegalAdminBody` slot. Same shape as `CameraAdminPayload`.
#[derive(
    SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq,
)]
pub enum LegalAdminPayload {
    ListRequest(LegalDocumentsListRequest),
    ListResponse(LegalDocumentsListResponse),
    GenerateRequest(LegalDocumentGenerateRequest),
    GenerateResponse(LegalDocumentGenerateResponse),
    RevokeRequest(LegalDocumentRevokeRequest),
    RevokeResponse(LegalDocumentRevokeResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    macro_rules! round_trip {
        ($ty:ty, $value:expr) => {{
            let bytes = crate::cbor::encode(&$value).expect("encode");
            crate::cbor::decode::<$ty>(&bytes).expect("decode")
        }};
    }

    #[test]
    fn list_request_round_trip() {
        let v = LegalAdminPayload::ListRequest(LegalDocumentsListRequest {
            include_revoked: true,
        });
        assert_eq!(round_trip!(LegalAdminPayload, v.clone()), v);
    }

    #[test]
    fn list_response_round_trip() {
        let v = LegalAdminPayload::ListResponse(LegalDocumentsListResponse {
            documents: vec![LegalDocumentSummary {
                doc_id: "11111111-1111-4111-8111-111111111111".into(),
                org_id: "22222222-2222-4222-8222-222222222222".into(),
                variant: "standard".into(),
                generated_at: 1_700_000_000_000,
                generated_by_user_id: "u-1".into(),
                content_hash: "a".repeat(64),
                revoked_at_ms: 0,
            }],
        });
        assert_eq!(round_trip!(LegalAdminPayload, v.clone()), v);
    }

    #[test]
    fn generate_request_round_trip() {
        let v = LegalAdminPayload::GenerateRequest(LegalDocumentGenerateRequest {
            variant: "full".into(),
        });
        assert_eq!(round_trip!(LegalAdminPayload, v.clone()), v);
    }

    #[test]
    fn generate_response_round_trip() {
        let v = LegalAdminPayload::GenerateResponse(LegalDocumentGenerateResponse {
            doc_id: "33333333-3333-4333-8333-333333333333".into(),
            content_hash: "b".repeat(64),
            signed_url:
                "/legal/33333333-3333-4333-8333-333333333333?token=XYZ&exp=999&org=O&nonce=N".into(),
        });
        assert_eq!(round_trip!(LegalAdminPayload, v.clone()), v);
    }

    #[test]
    fn revoke_request_round_trip() {
        let v = LegalAdminPayload::RevokeRequest(LegalDocumentRevokeRequest {
            doc_id: "44444444-4444-4444-8444-444444444444".into(),
        });
        assert_eq!(round_trip!(LegalAdminPayload, v.clone()), v);
    }

    #[test]
    fn revoke_response_round_trip() {
        let v = LegalAdminPayload::RevokeResponse(LegalDocumentRevokeResponse {
            doc_id: "44444444-4444-4444-8444-444444444444".into(),
            revoked_at_ms: 1_700_000_000_000,
        });
        assert_eq!(round_trip!(LegalAdminPayload, v.clone()), v);
    }

    #[test]
    fn message_body_legal_admin_round_trip() {
        let body = MessageBody::LegalAdminBody(LegalAdminPayload::GenerateRequest(
            LegalDocumentGenerateRequest {
                variant: "short".into(),
            },
        ));
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }
}
