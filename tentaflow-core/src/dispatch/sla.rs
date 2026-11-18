// =============================================================================
// File: dispatch/sla.rs — service SLA policies over the binary protocol (TB-03)
// =============================================================================
//
// `MessageBody::SlaBody` carries the whole family: policy CRUD, assignment of a
// policy to a flow, the live status of governed flows and the breach history.
// The wire tier is `UserSession`; the real gates are inside:
//   * reads (list, get, status, breaches) — any session with a resolved
//     organization. The flow list itself is open to every session and no
//     per-flow read permission exists, so "may view flows" is exactly this;
//   * writes (upsert, delete) — `sla.manage` (migration 213, held by
//     `org_admin`), otherwise `PolicyDenied`;
//   * assign / unassign — `sla.manage` AND the right to edit flows, which is
//     what `flow_update` asks for: a `PowerUser` session (role `power_user` or
//     `admin`). The first alone would let an organization administrator
//     re-govern a flow they may not edit.
//
// Isolation: every read is pinned to `ctx.org_context.org_id`. The platform
// super-admin (account role `admin`, the same test as the Events browser,
// TB-02 decision 1) may name another organization in `org_id` of a read;
// nobody else can, whatever the request carries. Writes always act on the
// caller's own organization. A policy or flow of another organization answers
// `NotFound`, as one that does not exist does. Platform flows (`is_system`)
// cannot be governed (`system_flow`).
//
// A refusal the administrator can act on (`invalid_value` with the field,
// `name_taken`, `policy_in_use`, `system_flow`) is a normal response carrying a
// coded `SlaErrorWire`, which the screen translates; authorization, missing
// objects and storage failures are protocol errors.
//
// Audit: `sla.policy.create` / `update` / `delete` and `sla.policy.assign` /
// `unassign`, each stamped with the organization and carrying the state before
// and after as JSON in `details`.

use std::collections::HashMap;

use serde_json::{json, Value};
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::sla::{
    SlaAssignmentWire, SlaBreachWire, SlaBrief, SlaErrorWire, SlaEvaluationWire, SlaFlowStatusWire,
    SlaPayload, SlaPolicySpecWire, SlaPolicyWire, SlaWindowWire,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::{HandlerContext, SessionAuthKind};
use crate::services::rbac::OrgContext;
use crate::sla::breach::{self, Breach, BreachQuery};
use crate::sla::evaluator::WindowViews;
use crate::sla::status::{self, ServiceStatus};
use crate::sla::{
    self, repository, BusOrdering, SlaAssignment, SlaError, SlaPolicy, SlaPolicySpec, SlaRegistry,
    SLA_TARGET_FLOW,
};

const PERM_MANAGE: &str = "sla.manage";
const DEFAULT_PAGE: u32 = 50;

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_manage(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_MANAGE) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{PERM_MANAGE} permission required"),
        ));
    }
    Ok(org)
}

/// Assigning governs a flow, so it needs the right to edit it on top of
/// `sla.manage`: the tier `flow_update` is registered with.
fn require_flow_edit(ctx: &HandlerContext) -> Result<(), ProtocolError> {
    if !SessionAuthKind::PowerUser.session_satisfies(&ctx.session) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "the right to edit flows is required to assign a policy",
        ));
    }
    Ok(())
}

/// The organization a read is answered for: the caller's own, unless the caller
/// is the platform super-admin and names another one. The super-admin who names
/// one needs no organization of their own, as in the Events browser.
fn read_org(ctx: &HandlerContext, requested: Option<&str>) -> Result<String, ProtocolError> {
    match requested {
        Some(org_id) if SessionAuthKind::Admin.session_satisfies(&ctx.session) => {
            Ok(org_id.to_string())
        }
        _ => require_org(ctx).map(|own| own.org_id.clone()),
    }
}

fn not_found(what: &str) -> ProtocolError {
    ProtocolError::not_found(format!("{what} not found"))
}

/// Splits a repository failure into what the response carries and what is a
/// protocol error.
fn refusal(error: SlaError) -> Result<SlaErrorWire, ProtocolError> {
    let code = error.code().to_string();
    match error {
        SlaError::NotFound { entity, .. } => Err(not_found(entity)),
        SlaError::Storage(detail) => {
            tracing::warn!(%detail, "SLA storage error");
            Err(ProtocolError::internal("sla: storage error"))
        }
        SlaError::Invalid { field, .. } => Ok(SlaErrorWire {
            code,
            field: Some(field.to_string()),
            assignments: None,
            detail: error.to_string(),
        }),
        SlaError::PolicyInUse { assignments, .. } => Ok(SlaErrorWire {
            code,
            field: None,
            assignments: Some(assignments),
            detail: error.to_string(),
        }),
        SlaError::NameTaken(_) => Ok(SlaErrorWire {
            code,
            field: Some("name".to_string()),
            assignments: None,
            detail: error.to_string(),
        }),
        SlaError::GovernedElsewhere(_) | SlaError::SystemFlow(_) => Ok(SlaErrorWire {
            code,
            field: Some("flow_id".to_string()),
            assignments: None,
            detail: error.to_string(),
        }),
    }
}

fn read_failure(error: SlaError) -> ProtocolError {
    match refusal(error) {
        Err(protocol) => protocol,
        Ok(coded) => ProtocolError::bad_request(coded.detail),
    }
}

fn spec_from_wire(wire: &SlaPolicySpecWire) -> Result<SlaPolicySpec, SlaError> {
    let ordering = BusOrdering::parse(&wire.ordering).ok_or_else(|| SlaError::Invalid {
        field: "ordering",
        reason: format!("must be 'partition' or 'none', got '{}'", wire.ordering),
    })?;
    Ok(SlaPolicySpec {
        name: wire.name.clone(),
        description: wire.description.clone(),
        max_concurrency: wire.max_concurrency,
        max_queue: wire.max_queue,
        queue_timeout_ms: wire.queue_timeout_ms,
        call_limit: wire.call_limit,
        call_window_secs: wire.call_window_secs,
        timeout_ms: wire.timeout_ms,
        latency_p95_ms: wire.latency_p95_ms,
        error_rate_pct: wire.error_rate_pct,
        warn_at_pct: wire.warn_at_pct,
        ordering,
    })
}

fn spec_to_wire(spec: &SlaPolicySpec) -> SlaPolicySpecWire {
    SlaPolicySpecWire {
        name: spec.name.clone(),
        description: spec.description.clone(),
        max_concurrency: spec.max_concurrency,
        max_queue: spec.max_queue,
        queue_timeout_ms: spec.queue_timeout_ms,
        call_limit: spec.call_limit,
        call_window_secs: spec.call_window_secs,
        timeout_ms: spec.timeout_ms,
        latency_p95_ms: spec.latency_p95_ms,
        error_rate_pct: spec.error_rate_pct,
        warn_at_pct: spec.warn_at_pct,
        ordering: spec.ordering.as_str().to_string(),
    }
}

fn policy_to_wire(policy: &SlaPolicy, assigned_flows: u32) -> SlaPolicyWire {
    SlaPolicyWire {
        id: policy.id.clone(),
        org_id: policy.org_id.clone(),
        spec: spec_to_wire(&policy.spec),
        created_at_ms: policy.created_at_ms,
        updated_at_ms: policy.updated_at_ms,
        updated_by: policy.updated_by.clone(),
        assigned_flows,
    }
}

fn assignment_to_wire(assignment: &SlaAssignment) -> SlaAssignmentWire {
    SlaAssignmentWire {
        flow_id: assignment.target_id.clone(),
        policy_id: assignment.policy_id.clone(),
        assigned_at_ms: assignment.assigned_at_ms,
        assigned_by: assignment.assigned_by.clone(),
    }
}

fn breach_to_wire(breach: &Breach) -> SlaBreachWire {
    use crate::sla::breach::BreachUnit;
    SlaBreachWire {
        id: breach.id.clone(),
        flow_id: breach.target_id.clone(),
        policy_id: breach.policy_id.clone(),
        kind: breach.kind.as_str().to_string(),
        unit: match breach.kind.unit() {
            BreachUnit::Milliseconds => "ms",
            BreachUnit::Percent => "percent",
            BreachUnit::Calls => "calls",
        }
        .to_string(),
        node_id: breach.node_id.clone(),
        threshold: breach.threshold,
        peak_value: breach.peak_value,
        raised_at_ms: breach.raised_at_ms,
        resolved_at_ms: breach.resolved_at_ms,
        call_limit: breach.call_limit,
        peak_calls: breach.peak_calls,
    }
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn status_to_wire(status: &ServiceStatus) -> SlaFlowStatusWire {
    SlaFlowStatusWire {
        flow_id: status.target_id.clone(),
        policy_id: status.policy_id.clone(),
        state: status.health().as_str().to_string(),
        in_flight: saturating_u32(status.in_flight),
        waiting: saturating_u32(status.waiting),
        max_concurrency: status.max_concurrency,
        window: status.call_window.map(|w| SlaWindowWire {
            used: w.used,
            limit: w.limit,
            window_secs: w.window_secs,
            resets_at_ms: w.resets_at_ms,
        }),
        evaluation: status.window.map(|w| SlaEvaluationWire {
            at_ms: w.at_ms,
            span_ms: w.span_ms,
            finished: w.finished,
            errors: w.errors,
            timeouts: w.timeouts,
            rejected_calls: w.rejected_calls,
            rejected_concurrency: w.rejected_concurrency,
            p95_ms: w.p95_ms,
            error_rate_pct: w.error_rate_pct,
        }),
        rejected_calls_total: status.rejected_calls_total,
        rejected_concurrency_total: status.rejected_concurrency_total,
        rejected_calls_today: status.rejected_calls_today,
        rejected_concurrency_today: status.rejected_concurrency_today,
        open_breaches: status.open_breaches.iter().map(breach_to_wire).collect(),
    }
}

fn brief_of(status: &ServiceStatus, governed_elsewhere: bool) -> SlaBrief {
    SlaBrief {
        state: status.health().as_str().to_string(),
        in_flight: saturating_u32(status.in_flight),
        max_concurrency: status.max_concurrency,
        window_used: status.call_window.map(|w| w.used),
        window_limit: status.call_window.map(|w| w.limit),
        governed_elsewhere,
    }
}

/// What the flow list shows of one flow's SLA.
pub(crate) struct FlowSla {
    /// The caller's own policy; `None` when another organization governs the
    /// flow, whose policy is never named.
    pub policy_id: Option<String>,
    pub brief: SlaBrief,
}

/// The SLA part of every flow row, by flow id: the caller's own assignments and,
/// for flows the caller does not govern, the state of whatever another
/// organization's policy does to them (`governed_elsewhere`): flows are
/// node-global, so its limit is what the caller's own callers meet. Constant
/// cost whatever the number of flows (two assignment reads, two breach reads and
/// the registry's generation). A failure leaves the list without SLA data
/// instead of failing it.
pub(crate) fn flow_sla_overview(
    ctx: &HandlerContext,
    registry: &SlaRegistry,
) -> HashMap<String, FlowSla> {
    let Some(org) = ctx.org_context.as_ref() else {
        return HashMap::new();
    };
    let views = WindowViews::global();
    let db = &ctx.state.db;
    let day_start_ms = status::local_day_start_ms();
    let read = || -> Result<_, SlaError> {
        Ok((
            status::list_service_statuses(db, registry, &views, &org.org_id, day_start_ms)?,
            status::list_foreign_service_statuses(db, registry, &views, &org.org_id, day_start_ms)?,
        ))
    };
    let (own, foreign) = match read() {
        Ok(read) => read,
        Err(error) => {
            tracing::warn!(%error, "SLA overview for the flow list unavailable");
            return HashMap::new();
        }
    };
    let mut overview: HashMap<String, FlowSla> = foreign
        .iter()
        .map(|s| {
            (
                s.target_id.clone(),
                FlowSla {
                    policy_id: None,
                    brief: brief_of(s, true),
                },
            )
        })
        .collect();
    for s in &own {
        overview.insert(
            s.target_id.clone(),
            FlowSla {
                policy_id: Some(s.policy_id.clone()),
                brief: brief_of(s, false),
            },
        );
    }
    overview
}

#[handler(variant = "SlaBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn sla_dispatch(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    dispatch_with(req, ctx, sla::global())
}

/// The handler over an explicit registry: the node's own in production, a
/// private one in tests.
fn dispatch_with(
    req: &MessageBody,
    ctx: &HandlerContext,
    registry: &SlaRegistry,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::SlaBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected SlaBody")),
    };
    let reply = match payload {
        SlaPayload::PolicyListRequest { org_id } => policy_list(ctx, org_id.as_deref())?,
        SlaPayload::PolicyGetRequest { policy_id, org_id } => {
            policy_get(ctx, policy_id, org_id.as_deref())?
        }
        SlaPayload::PolicyUpsertRequest { policy_id, spec } => {
            policy_upsert(ctx, policy_id.as_deref(), spec)?
        }
        SlaPayload::PolicyDeleteRequest { policy_id } => policy_delete(ctx, policy_id)?,
        SlaPayload::AssignRequest { flow_id, policy_id } => assign(ctx, flow_id, policy_id)?,
        SlaPayload::UnassignRequest { flow_id } => unassign(ctx, flow_id)?,
        SlaPayload::StatusRequest { flow_id, org_id } => {
            flow_status(ctx, registry, flow_id.as_deref(), org_id.as_deref())?
        }
        SlaPayload::BreachListRequest {
            flow_id,
            open_only,
            offset,
            limit,
            org_id,
        } => breach_list(
            ctx,
            flow_id.as_deref(),
            *open_only,
            *offset,
            *limit,
            org_id.as_deref(),
        )?,
        SlaPayload::PolicyListResponse { .. }
        | SlaPayload::PolicyGetResponse { .. }
        | SlaPayload::PolicyUpsertResponse { .. }
        | SlaPayload::PolicyDeleteResponse { .. }
        | SlaPayload::AssignResponse { .. }
        | SlaPayload::UnassignResponse { .. }
        | SlaPayload::StatusResponse { .. }
        | SlaPayload::BreachListResponse { .. } => {
            return Err(ProtocolError::bad_request(
                "that variant is a reply, not a request",
            ))
        }
    };
    Ok(MessageBody::SlaBody(reply))
}

fn policy_list(ctx: &HandlerContext, requested: Option<&str>) -> Result<SlaPayload, ProtocolError> {
    let org = read_org(ctx, requested)?;
    let policies = repository::list_policies(&ctx.state.db, &org).map_err(read_failure)?;
    let mut governed: HashMap<String, u32> = HashMap::new();
    for assignment in repository::list_assignments(&ctx.state.db, &org).map_err(read_failure)? {
        *governed.entry(assignment.policy_id).or_default() += 1;
    }
    let policies = policies
        .iter()
        .map(|p| policy_to_wire(p, governed.get(&p.id).copied().unwrap_or(0)))
        .collect();
    // Managing is a right in the caller's own organization, not in one a
    // super-admin only reads.
    let can_manage = ctx
        .org_context
        .as_ref()
        .is_some_and(|own| own.has(PERM_MANAGE) && own.org_id == org);
    Ok(SlaPayload::PolicyListResponse {
        policies,
        can_manage,
    })
}

fn policy_get(
    ctx: &HandlerContext,
    policy_id: &str,
    requested: Option<&str>,
) -> Result<SlaPayload, ProtocolError> {
    let org = read_org(ctx, requested)?;
    let policy = repository::get_policy(&ctx.state.db, &org, policy_id)
        .map_err(read_failure)?
        .ok_or_else(|| not_found("policy"))?;
    let flow_ids: Vec<String> = repository::list_assignments(&ctx.state.db, &org)
        .map_err(read_failure)?
        .into_iter()
        .filter(|a| a.policy_id == policy.id && a.target_kind == SLA_TARGET_FLOW)
        .map(|a| a.target_id)
        .collect();
    let governed = u32::try_from(flow_ids.len()).unwrap_or(u32::MAX);
    Ok(SlaPayload::PolicyGetResponse {
        policy: policy_to_wire(&policy, governed),
        flow_ids,
    })
}

fn policy_state(policy: &SlaPolicy) -> Value {
    let mut state = serde_json::to_value(spec_to_wire(&policy.spec)).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut state {
        map.insert("id".into(), json!(policy.id));
    }
    state
}

fn audit(
    ctx: &HandlerContext,
    org: &OrgContext,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    before: Value,
    after: Value,
) {
    let details = json!({ "org_id": org.org_id, "before": before, "after": after }).to_string();
    if let Err(error) = crate::db::repository::log_audit_full(
        &ctx.state.db,
        Some(&org.user_id),
        None,
        action,
        Some(resource_type),
        Some(resource_id),
        Some(&details),
        "info",
        "B",
        Some("success"),
        Some(&org.org_id),
        None,
        Some(ctx.state.local_node_id.as_ref()),
    ) {
        tracing::warn!(%error, action, "SLA audit row not written");
    }
}

fn policy_upsert(
    ctx: &HandlerContext,
    policy_id: Option<&str>,
    wire: &SlaPolicySpecWire,
) -> Result<SlaPayload, ProtocolError> {
    let org = require_manage(ctx)?;
    let refused = |error| -> Result<SlaPayload, ProtocolError> {
        Ok(SlaPayload::PolicyUpsertResponse {
            policy: None,
            created: false,
            error: Some(refusal(error)?),
        })
    };
    let spec = match spec_from_wire(wire) {
        Ok(spec) => spec,
        Err(error) => return refused(error),
    };
    let (before, result) = match policy_id {
        Some(id) => {
            let Some(before) =
                repository::get_policy(&ctx.state.db, &org.org_id, id).map_err(read_failure)?
            else {
                return Err(not_found("policy"));
            };
            let result =
                repository::update_policy(&ctx.state.db, &org.org_id, &org.user_id, id, &spec);
            (Some(before), result)
        }
        None => (
            None,
            repository::create_policy(&ctx.state.db, &org.org_id, &org.user_id, &spec),
        ),
    };
    let stored = match result {
        Ok(stored) => stored,
        Err(error) => return refused(error),
    };
    audit(
        ctx,
        org,
        if before.is_some() {
            "sla.policy.update"
        } else {
            "sla.policy.create"
        },
        "sla_policy",
        &stored.id,
        before.as_ref().map_or(Value::Null, policy_state),
        policy_state(&stored),
    );
    Ok(SlaPayload::PolicyUpsertResponse {
        policy: Some(policy_to_wire(&stored, 0)),
        created: before.is_none(),
        error: None,
    })
}

fn policy_delete(ctx: &HandlerContext, policy_id: &str) -> Result<SlaPayload, ProtocolError> {
    let org = require_manage(ctx)?;
    let Some(before) =
        repository::get_policy(&ctx.state.db, &org.org_id, policy_id).map_err(read_failure)?
    else {
        return Err(not_found("policy"));
    };
    if let Err(error) =
        repository::delete_policy(&ctx.state.db, &org.org_id, &org.user_id, policy_id)
    {
        return Ok(SlaPayload::PolicyDeleteResponse {
            error: Some(refusal(error)?),
        });
    }
    audit(
        ctx,
        org,
        "sla.policy.delete",
        "sla_policy",
        policy_id,
        policy_state(&before),
        Value::Null,
    );
    Ok(SlaPayload::PolicyDeleteResponse { error: None })
}

fn assignment_state(assignment: Option<&SlaAssignment>) -> Value {
    assignment.map_or(Value::Null, |a| json!({ "policy_id": a.policy_id }))
}

fn assign(
    ctx: &HandlerContext,
    flow_id: &str,
    policy_id: &str,
) -> Result<SlaPayload, ProtocolError> {
    let org = require_manage(ctx)?;
    require_flow_edit(ctx)?;
    let assigned = match repository::assign(
        &ctx.state.db,
        &org.org_id,
        &org.user_id,
        SLA_TARGET_FLOW,
        flow_id,
        policy_id,
    ) {
        Ok(assigned) => assigned,
        Err(error) => {
            return Ok(SlaPayload::AssignResponse {
                assignment: None,
                error: Some(refusal(error)?),
            })
        }
    };
    audit(
        ctx,
        org,
        "sla.policy.assign",
        SLA_TARGET_FLOW,
        flow_id,
        assignment_state(assigned.previous.as_ref()),
        assignment_state(Some(&assigned.current)),
    );
    Ok(SlaPayload::AssignResponse {
        assignment: Some(assignment_to_wire(&assigned.current)),
        error: None,
    })
}

fn unassign(ctx: &HandlerContext, flow_id: &str) -> Result<SlaPayload, ProtocolError> {
    let org = require_manage(ctx)?;
    require_flow_edit(ctx)?;
    let previous = match repository::unassign(
        &ctx.state.db,
        &org.org_id,
        &org.user_id,
        SLA_TARGET_FLOW,
        flow_id,
    ) {
        Ok(previous) => previous,
        Err(error) => {
            return Ok(SlaPayload::UnassignResponse {
                removed: false,
                error: Some(refusal(error)?),
            })
        }
    };
    if previous.is_some() {
        audit(
            ctx,
            org,
            "sla.policy.unassign",
            SLA_TARGET_FLOW,
            flow_id,
            assignment_state(previous.as_ref()),
            Value::Null,
        );
    }
    Ok(SlaPayload::UnassignResponse {
        removed: previous.is_some(),
        error: None,
    })
}

fn flow_status(
    ctx: &HandlerContext,
    registry: &SlaRegistry,
    flow_id: Option<&str>,
    requested: Option<&str>,
) -> Result<SlaPayload, ProtocolError> {
    let org = read_org(ctx, requested)?;
    let views = WindowViews::global();
    let day_start_ms = status::local_day_start_ms();
    let statuses = match flow_id {
        Some(flow_id) => {
            vec![status::service_status(
                &ctx.state.db,
                registry,
                &views,
                &org,
                flow_id,
                day_start_ms,
            )
            .map_err(read_failure)?]
        }
        None => status::list_service_statuses(&ctx.state.db, registry, &views, &org, day_start_ms)
            .map_err(read_failure)?,
    };
    Ok(SlaPayload::StatusResponse {
        statuses: statuses.iter().map(status_to_wire).collect(),
    })
}

fn breach_list(
    ctx: &HandlerContext,
    flow_id: Option<&str>,
    open_only: bool,
    offset: u32,
    limit: u32,
    requested: Option<&str>,
) -> Result<SlaPayload, ProtocolError> {
    let org = read_org(ctx, requested)?;
    let page = breach::list_breaches(
        &ctx.state.db,
        &org,
        flow_id,
        BreachQuery {
            open_only,
            offset,
            limit: if limit == 0 { DEFAULT_PAGE } else { limit },
        },
    )
    .map_err(read_failure)?;
    Ok(SlaPayload::BreachListResponse {
        breaches: page.items.iter().map(breach_to_wire).collect(),
        total: page.total,
    })
}

// `#[handler]` registers the family name, which no frame carries:
// `variant_name_of` reports the concrete request variant, so each one needs its
// own registry entry pointing at the same dispatch wrapper. The tier is
// `UserSession` for all of them; the checks inside are the real gate.
macro_rules! sla_handler_entry {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_sla_dispatch,
            }
        }
    };
}

sla_handler_entry!(
    "SlaPolicyListRequest",
    "tentaflow_ws_handler_sla_policy_list"
);
sla_handler_entry!("SlaPolicyGetRequest", "tentaflow_ws_handler_sla_policy_get");
sla_handler_entry!(
    "SlaPolicyUpsertRequest",
    "tentaflow_ws_handler_sla_policy_upsert"
);
sla_handler_entry!(
    "SlaPolicyDeleteRequest",
    "tentaflow_ws_handler_sla_policy_delete"
);
sla_handler_entry!("SlaAssignRequest", "tentaflow_ws_handler_sla_assign");
sla_handler_entry!("SlaUnassignRequest", "tentaflow_ws_handler_sla_unassign");
sla_handler_entry!("SlaStatusRequest", "tentaflow_ws_handler_sla_status");
sla_handler_entry!(
    "SlaBreachListRequest",
    "tentaflow_ws_handler_sla_breach_list"
);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use tentaflow_protocol::SessionAuth;

    use super::*;
    use crate::dispatch::state::AppState;
    use crate::dispatch::RequestOrigin;
    use crate::sla::breach::{BreachKind, Candidate};

    const ORG_A: &str = "org-a";
    const ORG_B: &str = "org-b";
    const MANAGE: &[&str] = &["sla.manage"];

    thread_local! {
        /// Every test runs on its own thread, so each gets a registry of its
        /// own instead of the node-global one other tests reload from their
        /// databases.
        static REGISTRY: SlaRegistry = SlaRegistry::default();
    }

    struct Fixture {
        state: Arc<AppState>,
        flow: String,
        system_flow: String,
    }

    fn fixture() -> Fixture {
        let state = AppState::for_test();
        let flow = format!("flow-{}", uuid::Uuid::new_v4());
        let system_flow = format!("flow-sys-{}", uuid::Uuid::new_v4());
        {
            let conn = state.db.write().expect("test db writer");
            for org in [ORG_A, ORG_B] {
                conn.execute(
                    "INSERT OR IGNORE INTO organizations (org_id, name, slug, created_at) \
                     VALUES (?1, ?1, ?1, datetime('now'))",
                    [org],
                )
                .unwrap();
            }
            for user in ["u-a", "u-b"] {
                conn.execute(
                    "INSERT OR IGNORE INTO user_accounts (id, username, password_hash) \
                     VALUES (?1, ?1, 'x')",
                    [user],
                )
                .unwrap();
            }
            for id in [&flow, &system_flow] {
                conn.execute(
                    "INSERT INTO flows (id, name, flow_json) VALUES (?1, ?1, '{}')",
                    [id],
                )
                .unwrap();
            }
            conn.execute(
                "UPDATE flows SET is_system = 1 WHERE id = ?1",
                [&system_flow],
            )
            .unwrap();
        }
        Fixture {
            state,
            flow,
            system_flow,
        }
    }

    fn ctx(f: &Fixture, org: &str, user: &str, role: &str, perms: &[&str]) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: Some(role.to_string()),
            },
            correlation_id: 1,
            connection_id: 1,
            resume_secret: None,
            state: f.state.clone(),
            origin: RequestOrigin::Local,
            org_context: Some(OrgContext {
                user_id: user.to_string(),
                org_id: org.to_string(),
                role_id: "r-1".to_string(),
                permissions: perms.iter().map(|p| p.to_string()).collect::<HashSet<_>>(),
            }),
        }
    }

    /// Organization administrator with the right to edit flows.
    fn admin_a(f: &Fixture) -> HandlerContext {
        ctx(f, ORG_A, "u-a", "power_user", MANAGE)
    }

    fn admin_b(f: &Fixture) -> HandlerContext {
        ctx(f, ORG_B, "u-b", "power_user", MANAGE)
    }

    fn call(ctx: &HandlerContext, payload: SlaPayload) -> Result<SlaPayload, ProtocolError> {
        match REGISTRY
            .with(|registry| dispatch_with(&MessageBody::SlaBody(payload), ctx, registry))?
        {
            MessageBody::SlaBody(reply) => Ok(reply),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    fn spec(name: &str) -> SlaPolicySpecWire {
        SlaPolicySpecWire {
            name: name.to_string(),
            max_concurrency: Some(2),
            call_limit: Some(60),
            call_window_secs: Some(60),
            ordering: "partition".to_string(),
            ..SlaPolicySpecWire::default()
        }
    }

    fn create(ctx: &HandlerContext, name: &str) -> SlaPolicyWire {
        match call(
            ctx,
            SlaPayload::PolicyUpsertRequest {
                policy_id: None,
                spec: spec(name),
            },
        )
        .expect("upsert")
        {
            SlaPayload::PolicyUpsertResponse {
                policy: Some(policy),
                created: true,
                error: None,
            } => policy,
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    fn assign_req(
        ctx: &HandlerContext,
        flow: &str,
        policy: &str,
    ) -> Result<SlaPayload, ProtocolError> {
        call(
            ctx,
            SlaPayload::AssignRequest {
                flow_id: flow.to_string(),
                policy_id: policy.to_string(),
            },
        )
    }

    fn denied(result: Result<SlaPayload, ProtocolError>) {
        assert_eq!(
            result.expect_err("must be refused").code,
            ProtocolErrorCode::PolicyDenied
        );
    }

    fn not_found_code(result: Result<SlaPayload, ProtocolError>) {
        assert_eq!(
            result.expect_err("must be not_found").code,
            ProtocolErrorCode::NotFound
        );
    }

    fn coded(reply: SlaPayload) -> SlaErrorWire {
        match reply {
            SlaPayload::PolicyUpsertResponse {
                policy: None,
                created: false,
                error: Some(error),
            }
            | SlaPayload::PolicyDeleteResponse { error: Some(error) }
            | SlaPayload::AssignResponse {
                assignment: None,
                error: Some(error),
            } => error,
            other => panic!("expected a coded refusal, got {other:?}"),
        }
    }

    fn raise_breach(f: &Fixture, org: &str, flow: &str, policy: &str, kind: BreachKind) {
        crate::sla::breach::raise_or_refresh(
            &f.state.db,
            &Candidate {
                org_id: org.to_string(),
                target_id: flow.to_string(),
                policy_id: policy.to_string(),
                kind,
                threshold: 100.0,
                value: 150.0,
                calls: None,
            },
            "node-1",
            1_000,
        )
        .expect("raise");
    }

    fn audit_rows(f: &Fixture) -> Vec<(String, Option<String>, Option<String>, Value)> {
        let conn = f.state.db.read().expect("reader");
        let mut stmt = conn
            .prepare(
                "SELECT action, org_id, user_id, details FROM audit_log \
                 WHERE action LIKE 'sla.policy.%' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| {
            let details: String = r.get(3)?;
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                serde_json::from_str(&details).expect("details are JSON"),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    #[test]
    fn writes_without_sla_manage_are_refused_and_change_nothing() {
        let f = fixture();
        let owner = admin_a(&f);
        let policy = create(&owner, "Gold");
        let bare = ctx(&f, ORG_A, "u-a", "admin", &["org.read"]);

        denied(call(
            &bare,
            SlaPayload::PolicyUpsertRequest {
                policy_id: None,
                spec: spec("Silver"),
            },
        ));
        denied(call(
            &bare,
            SlaPayload::PolicyUpsertRequest {
                policy_id: Some(policy.id.clone()),
                spec: spec("Renamed"),
            },
        ));
        denied(call(
            &bare,
            SlaPayload::PolicyDeleteRequest {
                policy_id: policy.id.clone(),
            },
        ));
        denied(assign_req(&bare, &f.flow, &policy.id));
        denied(call(
            &bare,
            SlaPayload::UnassignRequest {
                flow_id: f.flow.clone(),
            },
        ));

        let stored = repository::list_policies(&f.state.db, ORG_A).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].spec.name, "Gold");
        assert!(repository::list_assignments(&f.state.db, ORG_A)
            .unwrap()
            .is_empty());
        assert_eq!(audit_rows(&f).len(), 1, "only the creation was audited");
    }

    #[test]
    fn assigning_needs_the_right_to_edit_flows_on_top_of_sla_manage() {
        let f = fixture();
        let policy = create(&admin_a(&f), "Gold");
        let viewer = ctx(&f, ORG_A, "u-a", "user", MANAGE);

        denied(assign_req(&viewer, &f.flow, &policy.id));
        denied(call(
            &viewer,
            SlaPayload::UnassignRequest {
                flow_id: f.flow.clone(),
            },
        ));
        assert!(repository::list_assignments(&f.state.db, ORG_A)
            .unwrap()
            .is_empty());

        // The policy itself is the manager's to edit without touching a flow.
        assert!(matches!(
            call(
                &viewer,
                SlaPayload::PolicyUpsertRequest {
                    policy_id: Some(policy.id.clone()),
                    spec: spec("Gold"),
                },
            ),
            Ok(SlaPayload::PolicyUpsertResponse { error: None, .. })
        ));

        let editor = admin_a(&f);
        assert!(matches!(
            assign_req(&editor, &f.flow, &policy.id),
            Ok(SlaPayload::AssignResponse {
                error: None,
                assignment: Some(_)
            })
        ));
    }

    #[test]
    fn another_organizations_policies_flows_and_state_read_as_missing() {
        let f = fixture();
        let owner = admin_a(&f);
        let policy = create(&owner, "Gold");
        assign_req(&owner, &f.flow, &policy.id).unwrap();
        raise_breach(&f, ORG_A, &f.flow, &policy.id, BreachKind::Latency);

        let stranger = admin_b(&f);
        // Reads.
        let SlaPayload::PolicyListResponse { policies, .. } =
            call(&stranger, SlaPayload::PolicyListRequest { org_id: None }).unwrap()
        else {
            panic!("list");
        };
        assert!(policies.is_empty());
        not_found_code(call(
            &stranger,
            SlaPayload::PolicyGetRequest {
                policy_id: policy.id.clone(),
                org_id: None,
            },
        ));
        not_found_code(call(
            &stranger,
            SlaPayload::StatusRequest {
                flow_id: Some(f.flow.clone()),
                org_id: None,
            },
        ));
        let SlaPayload::StatusResponse { statuses } = call(
            &stranger,
            SlaPayload::StatusRequest {
                flow_id: None,
                org_id: None,
            },
        )
        .unwrap() else {
            panic!("status");
        };
        assert!(statuses.is_empty());
        for flow_id in [None, Some(f.flow.clone())] {
            let SlaPayload::BreachListResponse { breaches, total } = call(
                &stranger,
                SlaPayload::BreachListRequest {
                    flow_id,
                    open_only: false,
                    offset: 0,
                    limit: 0,
                    org_id: None,
                },
            )
            .unwrap() else {
                panic!("breaches");
            };
            assert!(breaches.is_empty() && total == 0);
        }
        // Naming the other organization changes nothing for an ordinary caller.
        let SlaPayload::PolicyListResponse { policies, .. } = call(
            &stranger,
            SlaPayload::PolicyListRequest {
                org_id: Some(ORG_A.to_string()),
            },
        )
        .unwrap() else {
            panic!("list");
        };
        assert!(policies.is_empty());
        not_found_code(call(
            &stranger,
            SlaPayload::PolicyGetRequest {
                policy_id: policy.id.clone(),
                org_id: Some(ORG_A.to_string()),
            },
        ));

        // Writes.
        not_found_code(call(
            &stranger,
            SlaPayload::PolicyUpsertRequest {
                policy_id: Some(policy.id.clone()),
                spec: spec("Hijacked"),
            },
        ));
        not_found_code(call(
            &stranger,
            SlaPayload::PolicyDeleteRequest {
                policy_id: policy.id.clone(),
            },
        ));
        not_found_code(assign_req(&stranger, &f.flow, &policy.id));
        // The flow is node-global, so the stranger may name it, but the other
        // organization's assignment is not theirs to replace or lift.
        let own = create(&stranger, "Own");
        assert_eq!(
            coded(assign_req(&stranger, &f.flow, &own.id).unwrap()).code,
            "governed_elsewhere"
        );
        let SlaPayload::UnassignResponse { removed, error } = call(
            &stranger,
            SlaPayload::UnassignRequest {
                flow_id: f.flow.clone(),
            },
        )
        .unwrap() else {
            panic!("unassign");
        };
        assert!(!removed);
        assert_eq!(error.expect("coded").code, "governed_elsewhere");

        let kept = repository::get_assignment(&f.state.db, ORG_A, SLA_TARGET_FLOW, &f.flow)
            .unwrap()
            .expect("the owner's assignment is untouched");
        assert_eq!(kept.policy_id, policy.id);
        assert_eq!(
            repository::get_policy(&f.state.db, ORG_A, &policy.id)
                .unwrap()
                .unwrap()
                .spec
                .name,
            "Gold"
        );
    }

    #[test]
    fn the_platform_super_admin_reads_across_organizations_but_still_writes_in_its_own() {
        let f = fixture();
        let owner = admin_a(&f);
        let policy = create(&owner, "Gold");
        assign_req(&owner, &f.flow, &policy.id).unwrap();
        raise_breach(&f, ORG_A, &f.flow, &policy.id, BreachKind::Errors);

        let root = ctx(&f, ORG_B, "u-b", "admin", MANAGE);
        let named = Some(ORG_A.to_string());

        let SlaPayload::PolicyListResponse {
            policies,
            can_manage,
        } = call(
            &root,
            SlaPayload::PolicyListRequest {
                org_id: named.clone(),
            },
        )
        .unwrap()
        else {
            panic!("list");
        };
        assert!(
            !can_manage,
            "sla.manage in org-b is no right to manage org-a"
        );
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].assigned_flows, 1);
        let SlaPayload::PolicyGetResponse { flow_ids, .. } = call(
            &root,
            SlaPayload::PolicyGetRequest {
                policy_id: policy.id.clone(),
                org_id: named.clone(),
            },
        )
        .unwrap() else {
            panic!("get");
        };
        assert_eq!(flow_ids, vec![f.flow.clone()]);
        let SlaPayload::StatusResponse { statuses } = call(
            &root,
            SlaPayload::StatusRequest {
                flow_id: Some(f.flow.clone()),
                org_id: named.clone(),
            },
        )
        .unwrap() else {
            panic!("status");
        };
        assert_eq!(statuses[0].state, "breach");
        assert_eq!(statuses[0].open_breaches.len(), 1);
        let SlaPayload::BreachListResponse { total, .. } = call(
            &root,
            SlaPayload::BreachListRequest {
                flow_id: None,
                open_only: true,
                offset: 0,
                limit: 0,
                org_id: named,
            },
        )
        .unwrap() else {
            panic!("breaches");
        };
        assert_eq!(total, 1);

        // Unnamed, the super-admin sees its own organization, which has nothing.
        let SlaPayload::PolicyListResponse {
            policies,
            can_manage,
        } = call(&root, SlaPayload::PolicyListRequest { org_id: None }).unwrap()
        else {
            panic!("list");
        };
        assert!(policies.is_empty());
        assert!(can_manage, "in its own organization the right holds");
        // A write does not follow the read pin: it lands in the caller's own org.
        let own = create(&root, "Root policy");
        assert_eq!(own.org_id, ORG_B);
        not_found_code(call(
            &root,
            SlaPayload::PolicyDeleteRequest {
                policy_id: policy.id,
            },
        ));
    }

    /// A super-admin without a membership reads across organizations by naming
    /// one, as in the Events browser; unnamed it has no organization to answer
    /// for, and it cannot write at all.
    #[test]
    fn a_super_admin_without_an_organization_reads_a_named_one_and_writes_nothing() {
        let f = fixture();
        let owner = admin_a(&f);
        let policy = create(&owner, "Gold");
        let mut root = ctx(&f, ORG_B, "u-b", "admin", MANAGE);
        root.org_context = None;

        let SlaPayload::PolicyListResponse {
            policies,
            can_manage,
        } = call(
            &root,
            SlaPayload::PolicyListRequest {
                org_id: Some(ORG_A.to_string()),
            },
        )
        .unwrap()
        else {
            panic!("list");
        };
        assert_eq!(policies.len(), 1);
        assert!(!can_manage);
        assert!(matches!(
            call(
                &root,
                SlaPayload::PolicyGetRequest {
                    policy_id: policy.id.clone(),
                    org_id: Some(ORG_A.to_string()),
                },
            ),
            Ok(SlaPayload::PolicyGetResponse { .. })
        ));
        assert_eq!(
            call(&root, SlaPayload::PolicyListRequest { org_id: None })
                .expect_err("no organization to answer for")
                .code,
            ProtocolErrorCode::AuthRequired
        );
        assert_eq!(
            call(
                &root,
                SlaPayload::PolicyUpsertRequest {
                    policy_id: None,
                    spec: spec("Nope"),
                },
            )
            .expect_err("writes need an own organization")
            .code,
            ProtocolErrorCode::AuthRequired
        );
        // An ordinary caller without an organization gets nothing by naming one.
        let mut plain = ctx(&f, ORG_B, "u-b", "user", MANAGE);
        plain.org_context = None;
        assert_eq!(
            call(
                &plain,
                SlaPayload::PolicyListRequest {
                    org_id: Some(ORG_A.to_string()),
                },
            )
            .expect_err("not a super-admin")
            .code,
            ProtocolErrorCode::AuthRequired
        );
    }

    #[test]
    fn every_write_leaves_an_org_stamped_audit_row_with_before_and_after() {
        let f = fixture();
        let ctx = admin_a(&f);
        let created = create(&ctx, "Gold");
        let mut changed = spec("Gold");
        changed.max_concurrency = Some(8);
        call(
            &ctx,
            SlaPayload::PolicyUpsertRequest {
                policy_id: Some(created.id.clone()),
                spec: changed,
            },
        )
        .unwrap();
        let silver = create(&ctx, "Silver");
        assign_req(&ctx, &f.flow, &created.id).unwrap();
        assign_req(&ctx, &f.flow, &silver.id).unwrap();
        call(
            &ctx,
            SlaPayload::UnassignRequest {
                flow_id: f.flow.clone(),
            },
        )
        .unwrap();
        // A second unassign removes nothing and is not audited.
        call(
            &ctx,
            SlaPayload::UnassignRequest {
                flow_id: f.flow.clone(),
            },
        )
        .unwrap();
        call(
            &ctx,
            SlaPayload::PolicyDeleteRequest {
                policy_id: created.id.clone(),
            },
        )
        .unwrap();

        let rows = audit_rows(&f);
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            actions,
            [
                "sla.policy.create",
                "sla.policy.update",
                "sla.policy.create",
                "sla.policy.assign",
                "sla.policy.assign",
                "sla.policy.unassign",
                "sla.policy.delete",
            ]
        );
        for (_, org, user, details) in &rows {
            assert_eq!(org.as_deref(), Some(ORG_A));
            assert_eq!(user.as_deref(), Some("u-a"));
            assert_eq!(details["org_id"], ORG_A);
        }
        assert_eq!(rows[0].3["before"], Value::Null);
        assert_eq!(rows[0].3["after"]["max_concurrency"], 2);
        assert_eq!(rows[1].3["before"]["max_concurrency"], 2);
        assert_eq!(rows[1].3["after"]["max_concurrency"], 8);
        assert_eq!(rows[1].3["after"]["id"], created.id);
        assert_eq!(rows[3].3["before"], Value::Null);
        assert_eq!(rows[3].3["after"]["policy_id"], created.id);
        assert_eq!(rows[4].3["before"]["policy_id"], created.id);
        assert_eq!(rows[4].3["after"]["policy_id"], silver.id);
        assert_eq!(rows[5].3["before"]["policy_id"], silver.id);
        assert_eq!(rows[5].3["after"], Value::Null);
        assert_eq!(rows[6].3["before"]["name"], "Gold");
        assert_eq!(rows[6].3["after"], Value::Null);
    }

    #[test]
    fn validation_failures_are_coded_and_write_nothing() {
        let f = fixture();
        let ctx = admin_a(&f);
        let upsert = |spec: SlaPolicySpecWire| {
            call(
                &ctx,
                SlaPayload::PolicyUpsertRequest {
                    policy_id: None,
                    spec,
                },
            )
            .unwrap()
        };

        let mut bad = spec("Bad");
        bad.max_concurrency = Some(0);
        let error = coded(upsert(bad));
        assert_eq!(
            (error.code.as_str(), error.field.as_deref()),
            ("invalid_value", Some("max_concurrency"))
        );

        let mut half = spec("Half");
        half.call_window_secs = None;
        let error = coded(upsert(half));
        assert_eq!(
            (error.code.as_str(), error.field.as_deref()),
            ("invalid_value", Some("call_window_secs"))
        );

        let mut order = spec("Order");
        order.ordering = "random".to_string();
        let error = coded(upsert(order));
        assert_eq!(
            (error.code.as_str(), error.field.as_deref()),
            ("invalid_value", Some("ordering"))
        );

        let error = coded(upsert(spec("   ")));
        assert_eq!(error.field.as_deref(), Some("name"));
        assert!(repository::list_policies(&f.state.db, ORG_A)
            .unwrap()
            .is_empty());
        assert!(
            audit_rows(&f).is_empty(),
            "a refused write is not audited as done"
        );

        let policy = create(&ctx, "Gold");
        let error = coded(upsert(spec("Gold")));
        assert_eq!(
            (error.code.as_str(), error.field.as_deref()),
            ("name_taken", Some("name"))
        );

        assign_req(&ctx, &f.flow, &policy.id).unwrap();
        let error = coded(
            call(
                &ctx,
                SlaPayload::PolicyDeleteRequest {
                    policy_id: policy.id.clone(),
                },
            )
            .unwrap(),
        );
        assert_eq!(
            (error.code.as_str(), error.assignments),
            ("policy_in_use", Some(1))
        );
        assert_eq!(
            audit_rows(&f).len(),
            2,
            "create and assign, not the refused delete"
        );

        let error = coded(assign_req(&ctx, &f.system_flow, &policy.id).unwrap());
        assert_eq!(error.code, "system_flow");
        not_found_code(assign_req(&ctx, "no-such-flow", &policy.id));
    }

    #[test]
    fn the_flow_list_shows_the_callers_policy_and_only_the_state_of_another_organizations() {
        let f = fixture();
        let owner = admin_a(&f);
        let policy = create(&owner, "Gold");
        assign_req(&owner, &f.flow, &policy.id).unwrap();
        let stranger = admin_b(&f);

        let overview =
            |ctx: &HandlerContext| REGISTRY.with(|registry| flow_sla_overview(ctx, registry));

        let mine = &overview(&owner)[&f.flow];
        assert_eq!(mine.policy_id.as_deref(), Some(policy.id.as_str()));
        assert!(!mine.brief.governed_elsewhere);
        assert_eq!(mine.brief.state, "ok");
        assert_eq!(
            (
                mine.brief.max_concurrency,
                mine.brief.window_used,
                mine.brief.window_limit,
                mine.brief.in_flight
            ),
            (Some(2), Some(0), Some(60), 0),
            "the policy's limits, read off the live gate"
        );
        assert!(!overview(&owner).contains_key(&f.system_flow));

        raise_breach(&f, ORG_A, &f.flow, &policy.id, BreachKind::CallsWarn);
        assert_eq!(overview(&owner)[&f.flow].brief.state, "warn");
        raise_breach(&f, ORG_A, &f.flow, &policy.id, BreachKind::Timeouts);
        assert_eq!(overview(&owner)[&f.flow].brief.state, "breach");

        // The other organization sees what its callers meet, never the policy.
        let theirs = &overview(&stranger)[&f.flow];
        assert_eq!(theirs.policy_id, None);
        assert!(theirs.brief.governed_elsewhere);
        assert_eq!(theirs.brief.state, "breach");
        assert_eq!(
            (
                theirs.brief.max_concurrency,
                theirs.brief.window_used,
                theirs.brief.window_limit
            ),
            (Some(2), Some(0), Some(60))
        );

        // The handler wires it into the rows (it reads the node-global
        // registry, so only what the database decides is asserted).
        let listed = |ctx: &HandlerContext| match crate::dispatch::handlers::flow_list(
            &MessageBody::FlowListRequest,
            ctx,
        )
        .expect("flow list")
        {
            MessageBody::FlowListResponse { flows } => flows,
            other => panic!("unexpected reply: {other:?}"),
        };
        let row = |flows: &[tentaflow_protocol::FlowSummary]| {
            flows
                .iter()
                .find(|s| s.id == f.flow)
                .cloned()
                .expect("listed")
        };
        let mine = row(&listed(&owner));
        assert_eq!(mine.sla_policy_id.as_deref(), Some(policy.id.as_str()));
        assert!(!mine.sla_brief.expect("brief").governed_elsewhere);
        let theirs = row(&listed(&stranger));
        assert_eq!(theirs.sla_policy_id, None);
        let brief = theirs.sla_brief.expect("brief");
        assert!(brief.governed_elsewhere);
        assert_eq!(brief.state, "breach");
    }

    #[test]
    fn a_reply_variant_is_not_accepted_as_a_request() {
        let f = fixture();
        let error = call(
            &admin_a(&f),
            SlaPayload::UnassignResponse {
                removed: true,
                error: None,
            },
        )
        .expect_err("replies are refused");
        assert_eq!(error.code, ProtocolErrorCode::BadRequest);
    }

    #[test]
    fn policy_list_reports_how_many_flows_each_policy_governs_and_whether_the_caller_manages() {
        let f = fixture();
        let owner = admin_a(&f);
        let gold = create(&owner, "Gold");
        create(&owner, "Idle");
        assign_req(&owner, &f.flow, &gold.id).unwrap();

        let reader = ctx(&f, ORG_A, "u-a", "user", &[]);
        let SlaPayload::PolicyListResponse {
            policies,
            can_manage,
        } = call(&reader, SlaPayload::PolicyListRequest { org_id: None }).unwrap()
        else {
            panic!("list");
        };
        assert!(!can_manage);
        let counts: Vec<(&str, u32)> = policies
            .iter()
            .map(|p| (p.spec.name.as_str(), p.assigned_flows))
            .collect();
        assert_eq!(counts, [("Gold", 1), ("Idle", 0)]);
    }
}
