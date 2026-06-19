// =============================================================================
// Plik: dispatch/compliance_admin.rs
// Opis: Handlery binarnego API Compliance Core dla ROPA, retencji i AI audit.
// Przykład: ComplianceAdminPayload::ListAiEventsRequest zwraca skróty eventów bez treści promptów.
// =============================================================================

use std::collections::BTreeMap;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    ComplianceAdminPayload, ComplianceAiEventListFilter, ComplianceAiEventSummary,
    ComplianceDataCategorySummary, ComplianceLocalizedText, ComplianceRetentionPolicySummary,
    MessageBody, ProtocolError, ProtocolErrorCode,
};

use super::HandlerContext;
use crate::compliance::models::{
    AiEventListFilter, AiEventStatus, ComplianceAiEvent, ComplianceDataCategory,
    ComplianceRetentionPolicy,
};
use crate::compliance::repository::{
    list_ai_events, list_data_categories, list_retention_policies,
};
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "compliance.read";

fn require_org<'a>(ctx: &'a HandlerContext) -> Result<&'a OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "compliance.read permission required",
        ));
    }
    Ok(org)
}

fn translations_from_json(value: &str) -> Result<Vec<ComplianceLocalizedText>, ProtocolError> {
    let map: BTreeMap<String, String> = serde_json::from_str(value)
        .map_err(|_| ProtocolError::internal("invalid compliance translations"))?;
    Ok(map
        .into_iter()
        .map(|(locale, text)| ComplianceLocalizedText { locale, text })
        .collect())
}

fn category_to_summary(
    category: ComplianceDataCategory,
) -> Result<ComplianceDataCategorySummary, ProtocolError> {
    Ok(ComplianceDataCategorySummary {
        category_id: category.category_id,
        slug: category.slug,
        name_translations: translations_from_json(&category.name_translations)?,
        description_translations: translations_from_json(&category.description_translations)?,
        personal_data: category.personal_data,
        sensitive_data: category.sensitive_data,
        risk_class: category.risk_class.as_str().to_string(),
        source_scope: category.source_scope,
        addon_id: category.addon_id,
    })
}

fn policy_to_summary(
    policy: ComplianceRetentionPolicy,
) -> Result<ComplianceRetentionPolicySummary, ProtocolError> {
    Ok(ComplianceRetentionPolicySummary {
        retention_policy_id: policy.retention_policy_id,
        slug: policy.slug,
        name_translations: translations_from_json(&policy.name_translations)?,
        scope_kind: policy.scope_kind.as_str().to_string(),
        category_id: policy.category_id,
        retention_days: policy.retention_days,
        minimum_days: policy.minimum_days,
        action_after_retention: policy.action_after_retention,
        is_default: policy.is_default,
        is_active: policy.is_active,
    })
}

fn ai_event_to_summary(event: ComplianceAiEvent) -> ComplianceAiEventSummary {
    ComplianceAiEventSummary {
        event_id: event.event_id,
        user_id: event.user_id,
        node_id: event.node_id,
        addon_id: event.addon_id,
        instance_id: event.instance_id,
        flow_id: event.flow_id,
        flow_node_id: event.flow_node_id,
        request_id: event.request_id,
        model_id: event.model_id,
        backend: event.backend,
        started_at: event.started_at,
        finished_at: event.finished_at,
        status: event.status.as_str().to_string(),
        risk_class: event.risk_class.as_str().to_string(),
        legal_basis_id: event.legal_basis_id,
        retention_policy_id: event.retention_policy_id,
        prompt_hash: event.prompt_hash,
        response_hash: event.response_hash,
        audit_log_id: event.audit_log_id,
        error_message: event.error_message,
    }
}

fn filter_from_protocol(
    filter: ComplianceAiEventListFilter,
) -> Result<AiEventListFilter, ProtocolError> {
    let status = match filter.status.as_deref() {
        Some(value) => Some(AiEventStatus::from_str(value).ok_or_else(|| {
            ProtocolError::bad_request("status must be running | success | failed | cancelled")
        })?),
        None => None,
    };
    Ok(AiEventListFilter {
        status,
        user_id: filter.user_id,
        addon_id: filter.addon_id,
        limit: filter.limit.unwrap_or(100),
        offset: filter.offset.unwrap_or(0),
    })
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "compliance admin database error");
    ProtocolError::internal("compliance database error")
}

#[handler(variant = "ComplianceAdminBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn compliance_admin_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::ComplianceAdminBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected ComplianceAdminBody")),
    };

    match payload {
        ComplianceAdminPayload::ListDataCategoriesRequest => list_data_categories_v1(ctx),
        ComplianceAdminPayload::ListRetentionPoliciesRequest => list_retention_policies_v1(ctx),
        ComplianceAdminPayload::ListAiEventsRequest(filter) => {
            list_ai_events_v1(ctx, filter.clone())
        }
        ComplianceAdminPayload::ListDataCategoriesResponse { .. }
        | ComplianceAdminPayload::ListRetentionPoliciesResponse { .. }
        | ComplianceAdminPayload::ListAiEventsResponse { .. } => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_compliance_admin_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_compliance_admin_dispatch,
            }
        }
    };
}

register_compliance_admin_variant!(
    "ComplianceDataCategoriesListRequest",
    "tentaflow_ws_handler_compliance_categories_list"
);
register_compliance_admin_variant!(
    "ComplianceRetentionPoliciesListRequest",
    "tentaflow_ws_handler_compliance_retention_list"
);
register_compliance_admin_variant!(
    "ComplianceAiEventsListRequest",
    "tentaflow_ws_handler_compliance_ai_events_list"
);

fn list_data_categories_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let conn = ctx
        .state
        .db
        .read()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    let categories = list_data_categories(&conn, &org.org_id)
        .map_err(|e| db_error("categories", e))?
        .into_iter()
        .map(category_to_summary)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MessageBody::ComplianceAdminBody(
        ComplianceAdminPayload::ListDataCategoriesResponse { categories },
    ))
}

fn list_retention_policies_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let conn = ctx
        .state
        .db
        .read()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    let policies = list_retention_policies(&conn, &org.org_id)
        .map_err(|e| db_error("retention", e))?
        .into_iter()
        .map(policy_to_summary)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MessageBody::ComplianceAdminBody(
        ComplianceAdminPayload::ListRetentionPoliciesResponse { policies },
    ))
}

fn list_ai_events_v1(
    ctx: &HandlerContext,
    filter: ComplianceAiEventListFilter,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let repo_filter = filter_from_protocol(filter)?;
    let conn = ctx
        .state
        .db
        .read()
        .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
    let events = list_ai_events(&conn, &org.org_id, &repo_filter)
        .map_err(|e| db_error("ai_events", e))?
        .into_iter()
        .map(ai_event_to_summary)
        .collect();
    Ok(MessageBody::ComplianceAdminBody(
        ComplianceAdminPayload::ListAiEventsResponse { events },
    ))
}
