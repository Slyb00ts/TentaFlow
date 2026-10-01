//! The handover screen "Do przekazania" over the organizational-structure
//! protocol (docs/ORG_STRUCTURE_PLAN.md §2.6, docs/PROJECT_STUDIO_WORKFLOW_PLAN.md
//! §4.5). Who may hand over what is decided by the service (departure: `org.admin`;
//! project removal: the project's manager; absence: the person, their manager or
//! `org.admin`); this layer needs only an organization member, refuses with
//! `PolicyDenied` what the service refuses as not permitted, and turns rule
//! failures of an apply into a normal answer the screen can show.

use serde_json::json;
use tentaflow_protocol::org_structure::OrgStructurePayload as P;
use tentaflow_protocol::org_structure_cover::OrgPersonRef;
use tentaflow_protocol::org_structure_handover as h;
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::{
    db_error, fmt, op_error, opt_day_write, publish, read_error, require_admin, require_member,
    PERM_ADMIN,
};
use crate::dispatch::HandlerContext;
use crate::services::org_structure::availability;
use crate::services::org_structure::handover as service;
use crate::services::org_structure::OrgStructureError as E;
use crate::services::rbac::OrgContext;

pub(super) fn dispatch(ctx: &HandlerContext, payload: &P) -> Result<MessageBody, ProtocolError> {
    let body = match payload {
        P::HandoverListRequest {
            user_id,
            reason,
            project_id,
            date,
            return_date,
        } => list(
            ctx,
            user_id,
            *reason,
            project_id.as_deref(),
            date,
            return_date,
        )?,
        P::HandoverApplyRequest {
            user_id,
            reason,
            project_id,
            date,
            return_date,
            note,
            items,
        } => apply(
            ctx,
            user_id,
            *reason,
            project_id.as_deref(),
            date,
            return_date,
            note,
            items,
        )?,
        P::HandoverRetryRequest { handover_id, keys } => retry(ctx, handover_id, keys)?,
        P::HandoverPendingRequest {} => pending(ctx)?,
        P::HandoverRecordsRequest { user_id } => records(ctx, user_id.as_deref())?,
        _ => return Err(ProtocolError::bad_request("not a handover request")),
    };
    Ok(MessageBody::OrgStructureBody(body))
}

fn reason_of(reason: h::OrgHandoverReason) -> service::Reason {
    match reason {
        h::OrgHandoverReason::Departure => service::Reason::Departure,
        h::OrgHandoverReason::Absence => service::Reason::Absence,
        h::OrgHandoverReason::ProjectRemoval => service::Reason::ProjectRemoval,
    }
}

fn reason_to_wire(reason: service::Reason) -> h::OrgHandoverReason {
    match reason {
        service::Reason::Departure => h::OrgHandoverReason::Departure,
        service::Reason::Absence => h::OrgHandoverReason::Absence,
        service::Reason::ProjectRemoval => h::OrgHandoverReason::ProjectRemoval,
    }
}

fn category_to_wire(category: service::Category) -> h::OrgHandoverCategory {
    match category {
        service::Category::Task => h::OrgHandoverCategory::Task,
        service::Category::TestItem => h::OrgHandoverCategory::TestItem,
        service::Category::Membership => h::OrgHandoverCategory::Membership,
        service::Category::Position => h::OrgHandoverCategory::Position,
        service::Category::Deputy => h::OrgHandoverCategory::Deputy,
    }
}

fn action_to_wire(action: service::Action) -> h::OrgHandoverAction {
    match action {
        service::Action::Transfer => h::OrgHandoverAction::Transfer,
        service::Action::TransferOrEnd => h::OrgHandoverAction::TransferOrEnd,
        service::Action::End => h::OrgHandoverAction::End,
    }
}

fn item_to_wire(item: service::Held) -> h::OrgHandoverItem {
    h::OrgHandoverItem {
        key: item.key,
        category: category_to_wire(item.category),
        title: item.title,
        role: item.role,
        state: item.state,
        project_id: item.project_id,
        project_name: item.project_name,
        unit_name: item.unit_name,
        valid_to: item.valid_to,
        action: action_to_wire(item.action),
        suggestion: item.suggestion.map(|s| h::OrgHandoverSuggestion {
            user_id: s.user_id,
            reason: s.reason.to_string(),
        }),
        eligible_user_ids: item.eligible,
        blocked: item.blocked.map(str::to_string),
    }
}

fn result_to_wire(result: service::ItemResult) -> h::OrgHandoverItemResult {
    h::OrgHandoverItemResult {
        key: result.key,
        category: category_to_wire(result.category),
        title: result.title,
        status: result.status,
        reason: result.reason,
        taker_user_id: result.taker_user_id,
        project_name: result.project_name,
    }
}

fn actor(org: &OrgContext) -> service::Actor<'_> {
    service::Actor {
        user_id: &org.user_id,
        is_admin: org.has(PERM_ADMIN),
    }
}

/// A refusal of the service as a protocol error: what a person may not do is
/// `PolicyDenied`, a thing that does not exist for them is `NotFound`.
fn refuse(e: E) -> ProtocolError {
    match e {
        E::NotPermitted(what) => ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("not permitted: {what}"),
        ),
        other => read_error(other),
    }
}

fn list(
    ctx: &HandlerContext,
    user_id: &str,
    reason: h::OrgHandoverReason,
    project_id: Option<&str>,
    date: &Option<String>,
    return_date: &Option<String>,
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let target = service::Target {
        user_id,
        reason: reason_of(reason),
        project_id: project_id.filter(|p| !p.is_empty()),
        date: opt_day_write(date).map_err(read_error)?,
        return_date: opt_day_write(return_date).map_err(read_error)?,
    };
    let listing =
        service::list(&ctx.state.db, &org.org_id, &actor(org), &target).map_err(refuse)?;
    Ok(P::HandoverListResponse {
        user: OrgPersonRef {
            user_id: user_id.to_string(),
            display_name: listing.user_name,
        },
        reason: reason_to_wire(listing.reason),
        date: fmt(listing.date),
        return_date: listing.return_date.map(fmt),
        assignment_ended_on: listing.assignment_ended_on.map(fmt),
        project_name: listing.project_name,
        groups: listing
            .groups
            .into_iter()
            .map(|(category, items)| h::OrgHandoverGroup {
                category: category_to_wire(category),
                items: items.into_iter().map(item_to_wire).collect(),
            })
            .collect(),
        takers: listing
            .takers
            .into_iter()
            .map(|(user_id, display_name)| OrgPersonRef {
                user_id,
                display_name,
            })
            .collect(),
    })
}

fn applied_to_wire(applied: service::Applied) -> P {
    let items: Vec<h::OrgHandoverItemResult> =
        applied.items.into_iter().map(result_to_wire).collect();
    let count = |statuses: &[&str]| {
        items
            .iter()
            .filter(|i| statuses.contains(&i.status.as_str()))
            .count() as u32
    };
    let failed = count(&["failed", "not_started"]);
    P::HandoverApplyResponse {
        ok: applied.refused.is_none() && failed == 0,
        handover_id: applied.handover_id,
        error: applied.refused.as_ref().map(op_error),
        applied: count(&["done"]),
        scheduled: count(&["scheduled"]),
        failed,
        items,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply(
    ctx: &HandlerContext,
    user_id: &str,
    reason: h::OrgHandoverReason,
    project_id: Option<&str>,
    date: &Option<String>,
    return_date: &Option<String>,
    note: &str,
    items: &[h::OrgHandoverChoice],
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let parsed = (|| -> Result<_, E> {
        Ok(service::Target {
            user_id,
            reason: reason_of(reason),
            project_id: project_id.filter(|p| !p.is_empty()),
            date: opt_day_write(date)?,
            return_date: opt_day_write(return_date)?,
        })
    })();
    let request = match parsed {
        Ok(subject) => service::ApplyRequest {
            subject,
            note,
            choices: items
                .iter()
                .map(|c| service::Choice {
                    key: c.key.clone(),
                    taker_user_id: c.taker_user_id.clone(),
                })
                .collect(),
        },
        Err(e) => return Ok(rejected(&e)),
    };
    match service::apply(&ctx.state.db, &org.org_id, &actor(org), &request) {
        Ok(applied) => {
            publish_applied(ctx, org, user_id, reason, &applied);
            Ok(applied_to_wire(applied))
        }
        Err(E::Db(detail)) => Err(db_error(detail)),
        Err(e @ (E::NotPermitted(_) | E::NotFound { .. } | E::CrossOrgReference { .. })) => {
            Err(refuse(e))
        }
        Err(e) => Ok(rejected(&e)),
    }
}

/// A request the rules refuse before anything moved.
fn rejected(e: &E) -> P {
    P::HandoverApplyResponse {
        ok: false,
        handover_id: None,
        error: Some(op_error(e)),
        items: Vec::new(),
        applied: 0,
        scheduled: 0,
        failed: 0,
    }
}

fn publish_applied(
    ctx: &HandlerContext,
    org: &OrgContext,
    user_id: &str,
    reason: h::OrgHandoverReason,
    applied: &service::Applied,
) {
    let Some(handover_id) = &applied.handover_id else {
        return;
    };
    let moved = applied
        .items
        .iter()
        .filter(|i| matches!(i.status.as_str(), "done" | "scheduled"))
        .count();
    // Ids and counts only: the structure changed when positions moved, and the tree reads it again.
    publish(
        ctx,
        org,
        "org.handover_applied",
        json!({
            "handover_id": handover_id,
            "user_id": user_id,
            "reason": reason_of(reason).as_str(),
            "moved": moved,
            "positions_touched": applied.items.iter().any(|i| {
                i.status == "done" && matches!(i.category, service::Category::Position | service::Category::Deputy)
            }),
        }),
    );
}

fn retry(ctx: &HandlerContext, handover_id: &str, keys: &[String]) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    match service::retry(&ctx.state.db, &org.org_id, &actor(org), handover_id, keys) {
        Ok(applied) => {
            if let Some(id) = &applied.handover_id {
                publish(
                    ctx,
                    org,
                    "org.handover_retried",
                    json!({ "handover_id": id }),
                );
            }
            Ok(applied_to_wire(applied))
        }
        Err(E::Db(detail)) => Err(db_error(detail)),
        Err(e @ (E::NotPermitted(_) | E::NotFound { .. } | E::CrossOrgReference { .. })) => {
            Err(refuse(e))
        }
        Err(e) => Ok(rejected(&e)),
    }
}

fn pending(ctx: &HandlerContext) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let people = service::pending_people(&ctx.state.db, &org.org_id).map_err(refuse)?;
    let names = availability::display_names(&ctx.state.db, &org.org_id).map_err(read_error)?;
    Ok(P::HandoverPendingResponse {
        people: people
            .into_iter()
            .map(|p| h::OrgHandoverPending {
                display_name: names.get(&p.user_id).cloned().unwrap_or_default(),
                user_id: p.user_id,
                count: p.count,
                ended_on: p.ended_on.map(fmt),
            })
            .collect(),
    })
}

fn records(ctx: &HandlerContext, user_id: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let subject = user_id.filter(|u| !u.is_empty()).unwrap_or(&org.user_id);
    let records =
        service::records(&ctx.state.db, &org.org_id, &actor(org), subject).map_err(refuse)?;
    let names = availability::display_names(&ctx.state.db, &org.org_id).map_err(read_error)?;
    Ok(P::HandoverRecordsResponse {
        records: records
            .into_iter()
            .map(|r| h::OrgHandoverRecord {
                created_by_name: names.get(&r.created_by).cloned().unwrap_or_default(),
                id: r.id,
                user_id: r.user_id,
                reason: reason_to_wire(r.reason),
                project_id: r.project_id,
                project_name: r.project_name,
                date: fmt(r.date),
                return_date: r.return_date.map(fmt),
                note: r.note,
                created_at_ms: r.created_at_ms,
                items: r.items.into_iter().map(result_to_wire).collect(),
            })
            .collect(),
    })
}
