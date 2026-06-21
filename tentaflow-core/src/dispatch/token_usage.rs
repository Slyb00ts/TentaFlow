// =============================================================================
// Plik: dispatch/token_usage.rs
// Opis: Handlery binarnego API metryk tokenów — zużycie, limity (quota) oraz
//       status koordynatora dzierżaw. Wszystko po CBOR, nigdy REST.
// Przykład: TokenUsagePayload::ListQuotasRequest zwraca limity organizacji.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, ProtocolErrorCode, TokenLeaseWire, TokenQuotaUpsertWire,
    TokenQuotaWire, TokenUsagePayload, TokenUsageSummaryWire,
};

use super::HandlerContext;
use crate::db::models::{NewTokenQuota, TokenLease, TokenQuota, UpdateTokenQuota, UsageSummaryRow};
use crate::db::repository;
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "tokens.read";
const PERM_WRITE: &str = "tokens.write";

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "tokens.read permission required",
        ));
    }
    Ok(org)
}

fn require_write(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_WRITE) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "tokens.write permission required",
        ));
    }
    Ok(org)
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "token usage admin database error");
    ProtocolError::internal("token usage database error")
}

fn summary_to_wire(row: UsageSummaryRow) -> TokenUsageSummaryWire {
    TokenUsageSummaryWire {
        key: row.key,
        prompt_tokens: row.prompt_tokens,
        completion_tokens: row.completion_tokens,
        total_tokens: row.total_tokens,
        request_count: row.request_count,
    }
}

fn quota_to_wire(quota: TokenQuota) -> TokenQuotaWire {
    TokenQuotaWire {
        id: quota.id,
        org_id: quota.org_id,
        scope_type: quota.scope_type,
        subject_id: quota.subject_id,
        model_id: quota.model_id,
        period: quota.period,
        max_total_tokens: quota.max_total_tokens,
        is_active: quota.is_active,
    }
}

fn lease_to_wire(lease: TokenLease) -> TokenLeaseWire {
    TokenLeaseWire {
        id: lease.id,
        quota_id: lease.quota_id,
        node_id: lease.node_id,
        period_key: lease.period_key,
        base_used: lease.base_used,
        granted_tokens: lease.granted_tokens,
        coordinator_node_id: lease.coordinator_node_id,
        expires_at: lease.expires_at,
    }
}

#[handler(variant = "TokenUsageBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn token_usage_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::TokenUsageBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected TokenUsageBody")),
    };

    match payload {
        TokenUsagePayload::UsageSummaryRequest {
            period,
            period_key,
            group_by,
        } => usage_summary_v1(ctx, period, period_key, group_by),
        TokenUsagePayload::ListQuotasRequest => list_quotas_v1(ctx),
        TokenUsagePayload::UpsertQuotaRequest { quota } => upsert_quota_v1(ctx, quota.clone()),
        TokenUsagePayload::DeleteQuotaRequest { id } => delete_quota_v1(ctx, id),
        TokenUsagePayload::CoordinatorStatusRequest => coordinator_status_v1(ctx),
        TokenUsagePayload::UsageSummaryResponse { .. }
        | TokenUsagePayload::ListQuotasResponse { .. }
        | TokenUsagePayload::UpsertQuotaResponse { .. }
        | TokenUsagePayload::DeleteQuotaResponse
        | TokenUsagePayload::CoordinatorStatusResponse { .. } => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_token_usage_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_token_usage_dispatch,
            }
        }
    };
}

register_token_usage_variant!(
    "TokenUsageSummaryRequest",
    "tentaflow_ws_handler_token_usage_summary"
);
register_token_usage_variant!(
    "TokenListQuotasRequest",
    "tentaflow_ws_handler_token_quotas_list"
);
register_token_usage_variant!(
    "TokenUpsertQuotaRequest",
    "tentaflow_ws_handler_token_quota_upsert"
);
register_token_usage_variant!(
    "TokenDeleteQuotaRequest",
    "tentaflow_ws_handler_token_quota_delete"
);
register_token_usage_variant!(
    "TokenCoordinatorStatusRequest",
    "tentaflow_ws_handler_token_coordinator_status"
);

fn usage_summary_v1(
    ctx: &HandlerContext,
    period: &str,
    period_key: &str,
    group_by: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let rows = repository::usage_summary(&ctx.state.db, &org.org_id, period, period_key, group_by)
        .map_err(|e| db_error("usage_summary", e))?
        .into_iter()
        .map(summary_to_wire)
        .collect();
    Ok(MessageBody::TokenUsageBody(
        TokenUsagePayload::UsageSummaryResponse { rows },
    ))
}

fn list_quotas_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let quotas = repository::list_token_quotas(&ctx.state.db, &org.org_id)
        .map_err(|e| db_error("list_quotas", e))?
        .into_iter()
        .map(quota_to_wire)
        .collect();
    Ok(MessageBody::TokenUsageBody(
        TokenUsagePayload::ListQuotasResponse { quotas },
    ))
}

fn upsert_quota_v1(
    ctx: &HandlerContext,
    quota: TokenQuotaUpsertWire,
) -> Result<MessageBody, ProtocolError> {
    let org = require_write(ctx)?;
    let id = match quota.id.as_deref() {
        Some(id) => {
            repository::update_token_quota(
                &ctx.state.db,
                &UpdateTokenQuota {
                    id,
                    org_id: &org.org_id,
                    scope_type: &quota.scope_type,
                    subject_id: quota.subject_id.as_deref(),
                    model_id: quota.model_id.as_deref(),
                    period: &quota.period,
                    max_total_tokens: quota.max_total_tokens,
                    is_active: quota.is_active,
                },
            )
            .map_err(|e| db_error("update_quota", e))?;
            id.to_string()
        }
        None => repository::create_token_quota(
            &ctx.state.db,
            &NewTokenQuota {
                org_id: &org.org_id,
                scope_type: &quota.scope_type,
                subject_id: quota.subject_id.as_deref(),
                model_id: quota.model_id.as_deref(),
                period: &quota.period,
                max_total_tokens: quota.max_total_tokens,
                is_active: quota.is_active,
            },
        )
        .map_err(|e| db_error("create_quota", e))?,
    };
    Ok(MessageBody::TokenUsageBody(
        TokenUsagePayload::UpsertQuotaResponse { id },
    ))
}

fn delete_quota_v1(ctx: &HandlerContext, id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_write(ctx)?;
    repository::delete_token_quota(&ctx.state.db, &org.org_id, id)
        .map_err(|e| db_error("delete_quota", e))?;
    Ok(MessageBody::TokenUsageBody(
        TokenUsagePayload::DeleteQuotaResponse,
    ))
}

fn coordinator_status_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let leases =
        repository::list_token_leases(&ctx.state.db, &org.org_id).map_err(|e| db_error("leases", e))?;
    // Koordynator wynika z wierszy lease — bez ponownego liczenia HRW tutaj.
    let coordinator_node_id = leases
        .iter()
        .max_by(|a, b| a.period_key.cmp(&b.period_key))
        .map(|lease| lease.coordinator_node_id.clone());
    let leases = leases.into_iter().map(lease_to_wire).collect();
    Ok(MessageBody::TokenUsageBody(
        TokenUsagePayload::CoordinatorStatusResponse {
            coordinator_node_id,
            leases,
        },
    ))
}
