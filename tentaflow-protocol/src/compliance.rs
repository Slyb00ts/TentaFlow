// =============================================================================
// Plik: compliance.rs
// Opis: Binarny protokół CBOR dla administracyjnego odczytu Compliance Core.
// Przykład: MessageBody::ComplianceAdminBody(ComplianceAdminPayload::ListAiEventsRequest(...))
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ComplianceLocalizedText {
    pub locale: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ComplianceDataCategorySummary {
    pub category_id: String,
    pub slug: String,
    pub name_translations: Vec<ComplianceLocalizedText>,
    pub description_translations: Vec<ComplianceLocalizedText>,
    pub personal_data: bool,
    pub sensitive_data: bool,
    pub risk_class: String,
    pub source_scope: String,
    pub addon_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ComplianceRetentionPolicySummary {
    pub retention_policy_id: String,
    pub slug: String,
    pub name_translations: Vec<ComplianceLocalizedText>,
    pub scope_kind: String,
    pub category_id: Option<String>,
    pub retention_days: i64,
    pub minimum_days: i64,
    pub action_after_retention: String,
    pub is_default: bool,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, SerdeSerialize, SerdeDeserialize)]
pub struct ComplianceAiEventListFilter {
    pub status: Option<String>,
    pub user_id: Option<i64>,
    pub addon_id: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ComplianceAiEventSummary {
    pub event_id: String,
    pub user_id: Option<i64>,
    pub node_id: String,
    pub addon_id: Option<String>,
    pub instance_id: Option<String>,
    pub flow_id: Option<i64>,
    pub flow_node_id: Option<String>,
    pub request_id: String,
    pub model_id: String,
    pub backend: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub risk_class: String,
    pub legal_basis_id: Option<String>,
    pub retention_policy_id: String,
    pub prompt_hash: String,
    pub response_hash: String,
    pub audit_log_id: Option<i64>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum ComplianceAdminPayload {
    ListDataCategoriesRequest,
    ListDataCategoriesResponse {
        categories: Vec<ComplianceDataCategorySummary>,
    },
    ListRetentionPoliciesRequest,
    ListRetentionPoliciesResponse {
        policies: Vec<ComplianceRetentionPolicySummary>,
    },
    ListAiEventsRequest(ComplianceAiEventListFilter),
    ListAiEventsResponse {
        events: Vec<ComplianceAiEventSummary>,
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
    fn compliance_payload_round_trip() {
        let payload = ComplianceAdminPayload::ListAiEventsRequest(ComplianceAiEventListFilter {
            status: Some("success".to_string()),
            user_id: Some(7),
            addon_id: Some("contacts".to_string()),
            limit: Some(50),
            offset: Some(10),
        });
        assert_eq!(
            round_trip!(ComplianceAdminPayload, payload.clone()),
            payload
        );
    }

    #[test]
    fn message_body_compliance_round_trip() {
        let body = MessageBody::ComplianceAdminBody(
            ComplianceAdminPayload::ListRetentionPoliciesResponse {
                policies: vec![ComplianceRetentionPolicySummary {
                    retention_policy_id: "ret-core-ai-audit-default".to_string(),
                    slug: "ai_audit_default".to_string(),
                    name_translations: vec![ComplianceLocalizedText {
                        locale: "pl".to_string(),
                        text: "AI audit minimum 6 miesięcy".to_string(),
                    }],
                    scope_kind: "ai_audit".to_string(),
                    category_id: None,
                    retention_days: 183,
                    minimum_days: 183,
                    action_after_retention: "archive".to_string(),
                    is_default: true,
                    is_active: true,
                }],
            },
        );
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }
}
