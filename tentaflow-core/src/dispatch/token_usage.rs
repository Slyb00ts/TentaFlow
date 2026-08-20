// =============================================================================
// Plik: dispatch/token_usage.rs
// Opis: Handlery binarnego API metryk tokenów — zużycie, limity (quota) oraz
//       status koordynatora dzierżaw. Wszystko po CBOR, nigdy REST.
// Przykład: TokenUsagePayload::ListQuotasRequest zwraca limity organizacji.
// =============================================================================

use std::collections::{HashMap, HashSet};

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, ProtocolErrorCode, TokenLeaseWire, TokenQuotaUpsertWire,
    TokenQuotaWire, TokenUsagePayload, TokenUsageSummaryWire,
};

use super::model_metrics::{period_window, resolve_nodes, user_presentation, NodePresentation};
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
        audio_ms: row.audio_ms,
        images: row.images,
        embedding_tokens: row.embedding_tokens,
    }
}

/// Current-period window of a quota: (`period_key`, inclusive `hour_bucket`
/// bounds). Anything other than `monthly` is daily, like the lease coordinator.
fn quota_period_window(period: &str) -> Result<(String, String, String), ProtocolError> {
    let period_key = crate::mesh::pipeline::token_period_key(period);
    let canonical = if period == "monthly" {
        "monthly"
    } else {
        "daily"
    };
    let (hour_from, hour_to) = period_window(canonical, &period_key)?;
    Ok((period_key, hour_from, hour_to))
}

/// Names resolved once per request for every quota subject/model.
struct QuotaNames {
    users: HashMap<String, repository::UserNameRow>,
    groups: HashMap<String, (String, i64)>,
    models: HashMap<String, String>,
}

fn resolve_quota_names(ctx: &HandlerContext, quotas: &[TokenQuota]) -> anyhow::Result<QuotaNames> {
    let mut user_ids = HashSet::new();
    let mut group_ids = HashSet::new();
    let mut model_ids = HashSet::new();
    for q in quotas {
        match (q.scope_type.as_str(), q.subject_id.as_deref()) {
            ("user", Some(id)) => {
                user_ids.insert(id.to_string());
            }
            ("group", Some(id)) => {
                group_ids.insert(id.to_string());
            }
            ("model", Some(id)) => {
                model_ids.insert(id.to_string());
            }
            _ => {}
        }
        if let Some(model) = &q.model_id {
            model_ids.insert(model.clone());
        }
    }
    let user_ids: Vec<String> = user_ids.into_iter().collect();
    let group_ids: Vec<String> = group_ids.into_iter().collect();
    let model_ids: Vec<String> = model_ids.into_iter().collect();
    Ok(QuotaNames {
        users: repository::lookup_user_names(&ctx.state.db, &user_ids)?,
        groups: repository::lookup_group_info(&ctx.state.db, &group_ids)?,
        models: repository::lookup_model_display_names(&ctx.state.db, &model_ids)?,
    })
}

fn quota_to_wire(
    ctx: &HandlerContext,
    names: &QuotaNames,
    quota: TokenQuota,
) -> Result<TokenQuotaWire, ProtocolError> {
    let (period_key, hour_from, hour_to) = quota_period_window(&quota.period)?;
    let used_tokens =
        repository::rollup_usage_for_quota(&ctx.state.db, &quota, &hour_from, &hour_to)
            .map_err(|e| db_error("quota_usage", e))?;
    let mut subject_display_name = None;
    let mut subject_subtitle = None;
    let mut subject_member_count = None;
    // A model-scoped quota names its model in `subject_id`; `model_id` is an
    // extra restriction for the other scopes.
    let mut model_display_name = quota
        .model_id
        .as_ref()
        .and_then(|m| names.models.get(m).cloned());
    match (quota.scope_type.as_str(), quota.subject_id.as_deref()) {
        ("user", Some(id)) => {
            if let Some(u) = names.users.get(id) {
                let (name, subtitle) = user_presentation(u);
                subject_display_name = Some(name);
                subject_subtitle = Some(subtitle);
            }
        }
        ("group", Some(id)) => {
            if let Some((name, members)) = names.groups.get(id) {
                subject_display_name = Some(name.clone());
                subject_member_count = Some(*members);
            }
        }
        ("model", Some(id)) => {
            if model_display_name.is_none() {
                model_display_name = names.models.get(id).cloned();
            }
        }
        _ => {}
    }
    Ok(TokenQuotaWire {
        id: quota.id,
        org_id: quota.org_id,
        scope_type: quota.scope_type,
        subject_id: quota.subject_id,
        model_id: quota.model_id,
        period: quota.period,
        max_total_tokens: quota.max_total_tokens,
        is_active: quota.is_active,
        subject_display_name,
        subject_subtitle,
        subject_member_count,
        model_display_name,
        period_key,
        used_tokens,
    })
}

fn lease_to_wire(lease: TokenLease, node: Option<&NodePresentation>) -> TokenLeaseWire {
    TokenLeaseWire {
        id: lease.id,
        quota_id: lease.quota_id,
        node_id: lease.node_id,
        period_key: lease.period_key,
        base_used: lease.base_used,
        granted_tokens: lease.granted_tokens,
        coordinator_node_id: lease.coordinator_node_id,
        expires_at: lease.expires_at,
        node_display_name: node.and_then(|n| n.display_name.clone()),
        node_last_seen_at: node.and_then(|n| n.last_seen_at.clone()),
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
        .map_err(|e| db_error("list_quotas", e))?;
    let names = resolve_quota_names(ctx, &quotas).map_err(|e| db_error("quota_names", e))?;
    let quotas = quotas
        .into_iter()
        .map(|q| quota_to_wire(ctx, &names, q))
        .collect::<Result<Vec<_>, _>>()?;
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
    let leases = repository::list_token_leases(&ctx.state.db, &org.org_id)
        .map_err(|e| db_error("leases", e))?;
    // Koordynator wynika z wierszy lease — bez ponownego liczenia HRW tutaj.
    let coordinator_node_id = leases
        .iter()
        .max_by(|a, b| a.period_key.cmp(&b.period_key))
        .map(|lease| lease.coordinator_node_id.clone());
    let node_ids: Vec<String> = leases
        .iter()
        .map(|l| l.node_id.clone())
        .chain(coordinator_node_id.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let nodes = resolve_nodes(ctx, &node_ids).map_err(|e| db_error("lease_nodes", e))?;
    let coordinator_display_name = coordinator_node_id
        .as_ref()
        .and_then(|id| nodes.get(id))
        .and_then(|n| n.display_name.clone());
    let leases = leases
        .into_iter()
        .map(|l| {
            let node = nodes.get(&l.node_id);
            lease_to_wire(l, node)
        })
        .collect();
    Ok(MessageBody::TokenUsageBody(
        TokenUsagePayload::CoordinatorStatusResponse {
            coordinator_node_id,
            leases,
            coordinator_display_name,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{NewTokenQuota, TokenLeaseUpsert};
    use crate::dispatch::model_metrics::tests::{bump, reader_ctx, seed_directory};
    use crate::services::org::DEFAULT_ORG_ID;

    const REMOTE_NODE: &str = "7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d";

    fn quota(
        ctx: &HandlerContext,
        scope: &str,
        subject: Option<&str>,
        model: Option<&str>,
        period: &str,
    ) -> String {
        repository::create_token_quota(
            &ctx.state.db,
            &NewTokenQuota {
                org_id: DEFAULT_ORG_ID,
                scope_type: scope,
                subject_id: subject,
                model_id: model,
                period,
                max_total_tokens: 1_000_000,
                is_active: true,
            },
        )
        .unwrap()
    }

    fn list(ctx: &HandlerContext) -> Vec<TokenQuotaWire> {
        match list_quotas_v1(ctx).unwrap() {
            MessageBody::TokenUsageBody(TokenUsagePayload::ListQuotasResponse { quotas }) => quotas,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn quotas_report_rollup_usage_for_current_period_and_names() {
        let ctx = reader_ctx();
        seed_directory(&ctx);
        let local = ctx.state.local_node_id.to_string();
        let now = chrono::Utc::now();
        let this_hour = now.format("%Y-%m-%dT%H:00:00Z").to_string();
        // Previous month: must not count toward the current period.
        let old_hour = "2020-01-15T10:00:00Z";
        bump(&ctx, &local, "u1", "qwen", &this_hour, 1000);
        bump(&ctx, REMOTE_NODE, "u2", "qwen", &this_hour, 400);
        bump(&ctx, REMOTE_NODE, "u2", "other", &this_hour, 70);
        bump(&ctx, &local, "u3", "qwen", &this_hour, 50);
        bump(&ctx, &local, "u1", "qwen", old_hour, 9999);

        let q_user = quota(&ctx, "user", Some("u1"), None, "daily");
        let q_group = quota(&ctx, "group", Some("g1"), Some("qwen"), "monthly");
        let q_org = quota(&ctx, "org", None, None, "monthly");
        let q_model = quota(&ctx, "model", Some("qwen"), None, "daily");

        let quotas = list(&ctx);
        let find = |id: &str| quotas.iter().find(|q| q.id == id).unwrap().clone();

        let user = find(&q_user);
        assert_eq!(user.used_tokens, 1000);
        assert_eq!(user.period_key, now.format("%Y-%m-%d").to_string());
        assert_eq!(
            user.subject_display_name.as_deref(),
            Some("Marta Kowalczyk")
        );
        assert_eq!(user.subject_subtitle.as_deref(), Some("marta.k@firma.pl"));

        let group = find(&q_group);
        assert_eq!(group.used_tokens, 1400, "members u1+u2 on qwen only");
        assert_eq!(group.period_key, now.format("%Y-%m").to_string());
        assert_eq!(group.subject_display_name.as_deref(), Some("Marketing"));
        assert_eq!(group.subject_member_count, Some(2));
        assert_eq!(
            group.model_display_name.as_deref(),
            Some("Qwen 3.8 27B AWQ")
        );

        assert_eq!(find(&q_org).used_tokens, 1520);
        let model = find(&q_model);
        assert_eq!(model.used_tokens, 1450);
        assert_eq!(
            model.model_display_name.as_deref(),
            Some("Qwen 3.8 27B AWQ")
        );
        assert_eq!(model.subject_display_name, None);
    }

    #[test]
    fn coordinator_status_resolves_node_names() {
        let ctx = reader_ctx();
        seed_directory(&ctx);
        let local = ctx.state.local_node_id.to_string();
        let q = quota(&ctx, "org", None, None, "monthly");
        for (node, granted) in [(local.as_str(), 500), (REMOTE_NODE, 200)] {
            repository::upsert_token_lease(
                &ctx.state.db,
                &TokenLeaseUpsert {
                    org_id: DEFAULT_ORG_ID,
                    quota_id: &q,
                    node_id: node,
                    period_key: "2026-08",
                    base_used: 0,
                    granted_tokens: granted,
                    coordinator_node_id: &local,
                    expires_at: "2026-08-19T13:00:00Z",
                },
            )
            .unwrap();
        }
        let (coordinator, name, leases) = match coordinator_status_v1(&ctx).unwrap() {
            MessageBody::TokenUsageBody(TokenUsagePayload::CoordinatorStatusResponse {
                coordinator_node_id,
                leases,
                coordinator_display_name,
            }) => (coordinator_node_id, coordinator_display_name, leases),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(coordinator.as_deref(), Some(local.as_str()));
        assert_eq!(
            name.as_deref(),
            Some(crate::mesh::node_info_collector::local_hostname().as_str())
        );
        let remote = leases.iter().find(|l| l.node_id == REMOTE_NODE).unwrap();
        assert_eq!(remote.node_display_name.as_deref(), Some("biuro-mini"));
        assert_eq!(
            remote.node_last_seen_at.as_deref(),
            Some("2026-08-19T10:00:00Z")
        );
        let local_lease = leases.iter().find(|l| l.node_id == local).unwrap();
        assert!(local_lease.node_last_seen_at.is_some());
    }
}
