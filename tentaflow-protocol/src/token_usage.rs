// =============================================================================
// Plik: token_usage.rs
// Opis: Binarny protokół CBOR dla administracji metryk tokenów, limitów (quota)
//       i statusu koordynatora dzierżaw (lease).
// Przykład: MessageBody::TokenUsageBody(TokenUsagePayload::ListQuotasRequest)
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TokenUsageSummaryWire {
    pub key: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub audio_ms: i64,
    pub images: i64,
    pub embedding_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TokenQuotaWire {
    pub id: String,
    pub org_id: String,
    pub scope_type: String,
    pub subject_id: Option<String>,
    pub model_id: Option<String>,
    pub period: String,
    pub max_total_tokens: i64,
    pub is_active: bool,
}

/// Parametry zapisu limitu. `id = None` tworzy nowy wiersz, `Some` aktualizuje.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TokenQuotaUpsertWire {
    pub id: Option<String>,
    pub scope_type: String,
    pub subject_id: Option<String>,
    pub model_id: Option<String>,
    pub period: String,
    pub max_total_tokens: i64,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct TokenLeaseWire {
    pub id: String,
    pub quota_id: String,
    pub node_id: String,
    pub period_key: String,
    pub base_used: i64,
    pub granted_tokens: i64,
    pub coordinator_node_id: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum TokenUsagePayload {
    UsageSummaryRequest {
        period: String,
        period_key: String,
        group_by: String,
    },
    UsageSummaryResponse {
        rows: Vec<TokenUsageSummaryWire>,
    },
    ListQuotasRequest,
    ListQuotasResponse {
        quotas: Vec<TokenQuotaWire>,
    },
    UpsertQuotaRequest {
        quota: TokenQuotaUpsertWire,
    },
    UpsertQuotaResponse {
        id: String,
    },
    DeleteQuotaRequest {
        id: String,
    },
    DeleteQuotaResponse,
    CoordinatorStatusRequest,
    CoordinatorStatusResponse {
        coordinator_node_id: Option<String>,
        leases: Vec<TokenLeaseWire>,
    },
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
    fn token_usage_payload_round_trip() {
        let payload = TokenUsagePayload::UpsertQuotaRequest {
            quota: TokenQuotaUpsertWire {
                id: None,
                scope_type: "user".to_string(),
                subject_id: Some("00000000-0000-0000-0000-000000000007".to_string()),
                model_id: None,
                period: "monthly".to_string(),
                max_total_tokens: 1_000_000,
                is_active: true,
            },
        };
        assert_eq!(round_trip!(TokenUsagePayload, payload.clone()), payload);
    }

    #[test]
    fn message_body_token_usage_round_trip() {
        let body = MessageBody::TokenUsageBody(TokenUsagePayload::CoordinatorStatusResponse {
            coordinator_node_id: Some("node-a".to_string()),
            leases: vec![TokenLeaseWire {
                id: "lease:q1:node-a:2026-06".to_string(),
                quota_id: "q1".to_string(),
                node_id: "node-a".to_string(),
                period_key: "2026-06".to_string(),
                base_used: 1000,
                granted_tokens: 5000,
                coordinator_node_id: "node-a".to_string(),
                expires_at: "2026-06-21T12:00:00Z".to_string(),
            }],
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }
}
