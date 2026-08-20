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
    /// Subject name (user/group per `scope_type`); `None` for org/model.
    #[serde(default)]
    pub subject_display_name: Option<String>,
    /// user → email|username; others → `None`.
    #[serde(default)]
    pub subject_subtitle: Option<String>,
    /// Group member count (`scope_type=group`).
    #[serde(default)]
    pub subject_member_count: Option<i64>,
    #[serde(default)]
    pub model_display_name: Option<String>,
    /// Current period key of the quota (UTC): `YYYY-MM-DD` / `YYYY-MM`.
    #[serde(default)]
    pub period_key: String,
    /// Tokens consumed in the current period within the quota scope, computed
    /// from `model_metrics_rollup` (the dashboard's source of truth).
    #[serde(default)]
    pub used_tokens: i64,
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
    #[serde(default)]
    pub node_display_name: Option<String>,
    #[serde(default)]
    pub node_last_seen_at: Option<String>,
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
        #[serde(default)]
        coordinator_display_name: Option<String>,
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
                node_display_name: Some("hazai".to_string()),
                node_last_seen_at: Some("2026-06-21T11:59:00Z".to_string()),
            }],
            coordinator_display_name: Some("hazai".to_string()),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    #[test]
    fn quota_wire_round_trip_with_usage() {
        let quota = TokenQuotaWire {
            id: "q1".to_string(),
            org_id: "org-default".to_string(),
            scope_type: "group".to_string(),
            subject_id: Some("g-marketing".to_string()),
            model_id: Some("qwen".to_string()),
            period: "monthly".to_string(),
            max_total_tokens: 50_000_000,
            is_active: true,
            subject_display_name: Some("Marketing".to_string()),
            subject_subtitle: None,
            subject_member_count: Some(6),
            model_display_name: Some("Qwen 3.8 27B AWQ".to_string()),
            period_key: "2026-08".to_string(),
            used_tokens: 12_345,
        };
        assert_eq!(round_trip!(TokenQuotaWire, quota.clone()), quota);
    }

    /// A payload from an old peer (without the new fields) decodes to the
    /// defaults instead of failing.
    #[test]
    fn legacy_payloads_decode_with_defaults() {
        #[derive(serde::Serialize)]
        struct LegacyQuota {
            id: String,
            org_id: String,
            scope_type: String,
            subject_id: Option<String>,
            model_id: Option<String>,
            period: String,
            max_total_tokens: i64,
            is_active: bool,
        }
        let bytes = crate::cbor::encode(&LegacyQuota {
            id: "q1".to_string(),
            org_id: "org-default".to_string(),
            scope_type: "org".to_string(),
            subject_id: None,
            model_id: None,
            period: "daily".to_string(),
            max_total_tokens: 10,
            is_active: false,
        })
        .expect("encode");
        let quota = crate::cbor::decode::<TokenQuotaWire>(&bytes).expect("decode");
        assert_eq!(quota.id, "q1");
        assert_eq!(quota.period_key, "");
        assert_eq!(quota.used_tokens, 0);
        assert_eq!(quota.subject_display_name, None);
        assert_eq!(quota.model_display_name, None);

        #[derive(serde::Serialize)]
        struct LegacyLease {
            id: String,
            quota_id: String,
            node_id: String,
            period_key: String,
            base_used: i64,
            granted_tokens: i64,
            coordinator_node_id: String,
            expires_at: String,
        }
        #[derive(serde::Serialize)]
        enum LegacyPayload {
            CoordinatorStatusResponse {
                coordinator_node_id: Option<String>,
                leases: Vec<LegacyLease>,
            },
        }
        let bytes = crate::cbor::encode(&LegacyPayload::CoordinatorStatusResponse {
            coordinator_node_id: Some("node-a".to_string()),
            leases: vec![LegacyLease {
                id: "l1".to_string(),
                quota_id: "q1".to_string(),
                node_id: "node-a".to_string(),
                period_key: "2026-06".to_string(),
                base_used: 1,
                granted_tokens: 2,
                coordinator_node_id: "node-a".to_string(),
                expires_at: "2026-06-21T12:00:00Z".to_string(),
            }],
        })
        .expect("encode");
        match crate::cbor::decode::<TokenUsagePayload>(&bytes).expect("decode") {
            TokenUsagePayload::CoordinatorStatusResponse {
                coordinator_node_id,
                leases,
                coordinator_display_name,
            } => {
                assert_eq!(coordinator_node_id.as_deref(), Some("node-a"));
                assert_eq!(coordinator_display_name, None);
                assert_eq!(leases.len(), 1);
                assert_eq!(leases[0].node_display_name, None);
                assert_eq!(leases[0].node_last_seen_at, None);
            }
            other => panic!("unexpected payload {other:?}"),
        }
    }
}
