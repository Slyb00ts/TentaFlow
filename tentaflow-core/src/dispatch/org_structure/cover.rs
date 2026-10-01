//! Deputies, absences, the escalation chain and visibility over the
//! organizational-structure protocol (docs/ORG_STRUCTURE_PLAN.md §1, §2.1a,
//! §6.3).
//!
//! Reads are open to every member and FILTERED here by privacy: an absence is
//! listed only to those who may see its dates and carries its reason only for
//! the person, the manager on the primary line and administrators. Writes:
//! deputies need `org.admin`; an absence is written by its person or by an
//! administrator (the service decides per row and answers `not_permitted`,
//! which leaves this layer as `PolicyDenied`).

use std::collections::HashMap;

use serde_json::json;
use tentaflow_protocol::org_structure::{OrgStructurePayload as P, OrgWriteResult};
use tentaflow_protocol::org_structure_cover as cover;
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::{
    clear_set, db_error, done, fmt, op_error, opt_day, opt_day_write, publish, read_error,
    require_admin, require_member, tri, Applied, PERM_ADMIN,
};
use crate::dispatch::HandlerContext;
use crate::services::org_structure as svc;
use crate::services::org_structure::availability::{
    self, Absence, AbsenceKind, Availability, Deputy, DeputyScope,
};
use crate::services::org_structure::escalation::{self, Problem, SkipReason, Via};
use crate::services::org_structure::privacy::{self, PersonDataKind, ViewRule};
use crate::services::org_structure::query::Snapshot;
use crate::services::org_structure::{OrgStructureError as E, WriteCtx};
use crate::services::rbac::{OrgContext, PermissionMatrix};

pub(super) fn dispatch(ctx: &HandlerContext, payload: &P) -> Result<MessageBody, ProtocolError> {
    let body = match payload {
        P::CoverRequest {
            user_id,
            at,
            include_past,
        } => person_cover(ctx, user_id.as_deref(), at.as_deref(), *include_past)?,
        P::MemberListRequest {} => member_list(ctx)?,
        P::AvailabilityRequest { at } => availability_of(ctx, at.as_deref())?,
        P::EscalationChainRequest { user_id, scope, at } => {
            escalation_chain(ctx, user_id, scope.as_deref(), at.as_deref())?
        }
        P::IsAvailableRequest { user_id, at } => is_available(ctx, user_id, at.as_deref())?,
        P::CanViewPersonDataRequest {
            viewer_user_id,
            subject_user_id,
            kind,
            at,
        } => can_view(
            ctx,
            viewer_user_id.as_deref(),
            subject_user_id,
            kind,
            at.as_deref(),
        )?,
        P::VisibilityRequest { user_id, at } => visibility(ctx, user_id.as_deref(), at.as_deref())?,
        P::WhoSeesRequest {
            subject_user_id,
            at,
        } => who_sees(ctx, subject_user_id.as_deref(), at.as_deref())?,
        write => return apply_write(ctx, write),
    };
    Ok(MessageBody::OrgStructureBody(body))
}

// =============================================================================
// Reads
// =============================================================================

fn names(ctx: &HandlerContext, org: &OrgContext) -> Result<HashMap<String, String>, ProtocolError> {
    availability::display_names(&ctx.state.db, &org.org_id).map_err(read_error)
}

fn name_of(names: &HashMap<String, String>, user_id: &str) -> String {
    names.get(user_id).cloned().unwrap_or_default()
}

fn person_ref(names: &HashMap<String, String>, user_id: &str) -> cover::OrgPersonRef {
    cover::OrgPersonRef {
        user_id: user_id.to_string(),
        display_name: name_of(names, user_id),
    }
}

fn deputy_to_wire(d: Deputy, names: &HashMap<String, String>) -> cover::OrgDeputy {
    cover::OrgDeputy {
        user_name: name_of(names, &d.user_id),
        deputy_name: name_of(names, &d.deputy_user_id),
        id: d.id,
        user_id: d.user_id,
        deputy_user_id: d.deputy_user_id,
        scope: d.scope.as_wire(),
        valid_from: fmt(d.valid_from),
        valid_to: d.valid_to.map(fmt),
    }
}

fn kind_to_wire(kind: AbsenceKind) -> cover::OrgAbsenceKind {
    match kind {
        AbsenceKind::Leave => cover::OrgAbsenceKind::Leave,
        AbsenceKind::Training => cover::OrgAbsenceKind::Training,
        AbsenceKind::Other => cover::OrgAbsenceKind::Other,
    }
}

fn kind_from_wire(kind: cover::OrgAbsenceKind) -> AbsenceKind {
    match kind {
        cover::OrgAbsenceKind::Leave => AbsenceKind::Leave,
        cover::OrgAbsenceKind::Training => AbsenceKind::Training,
        cover::OrgAbsenceKind::Other => AbsenceKind::Other,
    }
}

fn absence_to_wire(a: Absence) -> cover::OrgAbsence {
    cover::OrgAbsence {
        id: a.id,
        user_id: a.user_id,
        valid_from: fmt(a.valid_from),
        valid_to: a.valid_to.map(fmt),
        kind: kind_to_wire(a.kind),
        reason: a.reason,
        source: a.source,
    }
}

fn person_cover(
    ctx: &HandlerContext,
    user_id: Option<&str>,
    at: Option<&str>,
    include_past: bool,
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let subject = user_id.filter(|u| !u.is_empty()).unwrap_or(&org.user_id);
    let pc = privacy::person_cover(
        &ctx.state.db,
        &org.org_id,
        &org.user_id,
        org.has(PERM_ADMIN),
        subject,
        opt_day(at)?,
        include_past,
    )
    .map_err(read_error)?;
    let names = names(ctx, org)?;
    Ok(P::CoverResponse {
        user_id: pc.user_id.clone(),
        display_name: name_of(&names, &pc.user_id),
        available: pc.available,
        today: fmt(pc.today),
        absences: pc.absences.into_iter().map(absence_to_wire).collect(),
        covered_by: pc
            .covered_by
            .into_iter()
            .map(|d| deputy_to_wire(d, &names))
            .collect(),
        covering: pc
            .covering
            .into_iter()
            .map(|d| deputy_to_wire(d, &names))
            .collect(),
        can_see_absences: pc.can_see_absences,
        can_see_reason: pc.can_see_reason,
        can_edit_absences: pc.can_edit_absences,
        can_edit_deputies: pc.can_edit_absences,
        is_admin: org.has(PERM_ADMIN),
    })
}

fn member_list(ctx: &HandlerContext) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let members = availability::active_members(&ctx.state.db, &org.org_id).map_err(read_error)?;
    Ok(P::MemberListResponse {
        members: members
            .into_iter()
            .map(|(user_id, display_name)| cover::OrgPersonRef {
                user_id,
                display_name,
            })
            .collect(),
    })
}

fn availability_of(ctx: &HandlerContext, at: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let (day, absent, deputies) =
        availability::availability_on(&ctx.state.db, &org.org_id, opt_day(at)?)
            .map_err(read_error)?;
    let names = names(ctx, org)?;
    Ok(P::AvailabilityResponse {
        at: fmt(day),
        absent_user_ids: absent,
        deputies: deputies
            .into_iter()
            .map(|d| deputy_to_wire(d, &names))
            .collect(),
    })
}

fn is_available(ctx: &HandlerContext, user_id: &str, at: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let available = availability::is_available(&ctx.state.db, &org.org_id, user_id, opt_day(at)?)
        .map_err(read_error)?;
    Ok(P::IsAvailableResponse { available })
}

fn load(
    ctx: &HandlerContext,
    org: &OrgContext,
    at: Option<&str>,
) -> Result<(Snapshot, Availability), ProtocolError> {
    let conn = ctx.state.db.read().map_err(|e| db_error(e.to_string()))?;
    let day = match opt_day(at)? {
        Some(day) => day,
        None => svc::org_today(&ctx.state.db, &org.org_id).map_err(read_error)?,
    };
    let snap = Snapshot::load(&conn, &org.org_id, day).map_err(read_error)?;
    let avail = Availability::load(&conn, &org.org_id, day).map_err(read_error)?;
    Ok((snap, avail))
}

fn escalation_chain(
    ctx: &HandlerContext,
    user_id: &str,
    scope: Option<&str>,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let scope = match scope.filter(|s| !s.is_empty()) {
        Some(raw) => DeputyScope::parse(raw).map_err(read_error)?,
        None => DeputyScope::Escalations,
    };
    let (snap, avail) = load(ctx, org, at)?;
    let chain = escalation::escalation_chain(&snap, &avail, user_id, &scope);
    let names = names(ctx, org)?;
    let position_name = |id: &str| {
        snap.position(id)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    };
    Ok(P::EscalationChainResponse {
        steps: chain
            .steps
            .into_iter()
            .map(|s| cover::OrgEscalationStep {
                level: s.level,
                display_name: name_of(&names, &s.user_id),
                position_name: position_name(&s.position_id),
                via: match s.via {
                    Via::Holder => "holder",
                    Via::DeputyHead => "deputy_head",
                    Via::Deputy => "deputy",
                }
                .to_string(),
                covering_name: s.covering_user_id.as_deref().map(|u| name_of(&names, u)),
                user_id: s.user_id,
                position_id: s.position_id,
                covering_user_id: s.covering_user_id,
            })
            .collect(),
        skipped: chain
            .skipped
            .into_iter()
            .map(|s| cover::OrgEscalationSkip {
                level: s.level,
                position_name: position_name(&s.position_id),
                position_id: s.position_id,
                reason: match s.reason {
                    SkipReason::Vacant => "vacant",
                    SkipReason::Unavailable => "unavailable",
                }
                .to_string(),
            })
            .collect(),
        problem: chain.problem.map(|p| match p {
            Problem::Cycle { position_id } => cover::OrgEscalationProblem {
                kind: "cycle".to_string(),
                position_id: Some(position_id),
            },
            Problem::TooDeep => cover::OrgEscalationProblem {
                kind: "too_deep".to_string(),
                position_id: None,
            },
        }),
    })
}

/// The snake_case name serde gives an enum value (`Area`, `Verdict`).
fn snake<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn rule_name(rule: ViewRule) -> &'static str {
    match rule {
        ViewRule::Owner => "owner",
        ViewRule::PrimaryManager => "primary_manager",
        ViewRule::Supervisor => "supervisor",
        ViewRule::Administrator => "administrator",
        ViewRule::EveryMember => "every_member",
        ViewRule::None => "none",
    }
}

fn is_admin_of(
    ctx: &HandlerContext,
    org: &OrgContext,
    user_id: &str,
) -> Result<bool, ProtocolError> {
    if user_id == org.user_id {
        return Ok(org.has(PERM_ADMIN));
    }
    PermissionMatrix::global()
        .has_permission(&ctx.state.db, user_id, &org.org_id, PERM_ADMIN)
        .map_err(|e| db_error(e.to_string()))
}

/// Somebody other than the caller is being asked about: that is the
/// administrator's inspector, not something a member may probe.
fn require_self_or_admin<'a>(
    ctx: &'a HandlerContext,
    other: Option<&str>,
) -> Result<(&'a OrgContext, String), ProtocolError> {
    let org = require_member(ctx)?;
    match other.filter(|u| !u.is_empty() && *u != org.user_id) {
        None => Ok((org, org.user_id.clone())),
        Some(user) => {
            let org = require_admin(ctx)?;
            if !availability::is_member(&ctx.state.db, &org.org_id, user).map_err(read_error)? {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::NotFound,
                    "user not found",
                ));
            }
            Ok((org, user.to_string()))
        }
    }
}

fn can_view(
    ctx: &HandlerContext,
    viewer: Option<&str>,
    subject: &str,
    kind: &str,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let (org, viewer) = require_self_or_admin(ctx, viewer)?;
    let kind = PersonDataKind::parse(kind)
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown person data kind '{kind}'")))?;
    let admin = is_admin_of(ctx, org, &viewer)?;
    let (snap, _) = load(ctx, org, at)?;
    let decision = privacy::can_view_person_data(&snap, &viewer, subject, kind, admin);
    Ok(P::CanViewPersonDataResponse {
        allowed: decision.allowed,
        rule: rule_name(decision.rule).to_string(),
    })
}

fn visibility(
    ctx: &HandlerContext,
    user_id: Option<&str>,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let (org, user) = require_self_or_admin(ctx, user_id)?;
    let admin = is_admin_of(ctx, org, &user)?;
    let (snap, _) = load(ctx, org, at)?;
    let view = privacy::visibility_of(&snap, &user, admin);
    let names = names(ctx, org)?;
    let refs = |ids: &[String]| {
        ids.iter()
            .map(|id| person_ref(&names, id))
            .collect::<Vec<_>>()
    };
    Ok(P::VisibilityResponse {
        manager: snap
            .manager_of(&user)
            .map(|m| person_ref(&names, &m.user_id)),
        subtree: refs(&view.subtree),
        direct: refs(&view.direct),
        rows: view
            .rows
            .iter()
            .map(|r| cover::OrgVisibilityRow {
                area: snake(r.area),
                verdict: snake(r.verdict),
                rule: rule_name(r.rule).to_string(),
            })
            .collect(),
        user: person_ref(&names, &user),
    })
}

fn who_sees(
    ctx: &HandlerContext,
    subject: Option<&str>,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let (org, subject) = require_self_or_admin(ctx, subject)?;
    let (snap, _) = load(ctx, org, at)?;
    // The administrators are people with the permission, whoever the structure says they are.
    let members: Vec<String> = {
        let conn = ctx.state.db.read().map_err(|e| db_error(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT user_id FROM org_memberships WHERE org_id = ?1 ORDER BY user_id")
            .map_err(|e| db_error(e.to_string()))?;
        let rows = stmt
            .query_map([&org.org_id], |r| r.get::<_, String>(0))
            .map_err(|e| db_error(e.to_string()))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error(e.to_string()))?;
        rows
    };
    let mut admins = Vec::new();
    for member in members {
        if is_admin_of(ctx, org, &member)? {
            admins.push(member);
        }
    }
    let names = names(ctx, org)?;
    Ok(P::WhoSeesResponse {
        viewers: privacy::who_sees(&snap, &subject, &admins)
            .into_iter()
            .map(|w| cover::OrgViewer {
                display_name: name_of(&names, &w.user_id),
                user_id: w.user_id,
                rule: rule_name(w.rule).to_string(),
                kinds: w.kinds.iter().map(|k| k.as_str().to_string()).collect(),
            })
            .collect(),
        subject: person_ref(&names, &subject),
    })
}

// =============================================================================
// Writes
// =============================================================================

fn apply_write(ctx: &HandlerContext, payload: &P) -> Result<MessageBody, ProtocolError> {
    // The service decides per row: the covered person's own deputies and absences, or any for an
    // administrator. A manager has no right to either.
    let org = require_member(ctx)?;
    let response = match write(ctx, org, payload) {
        Ok(applied) => {
            for (name, event) in applied.events {
                publish(ctx, org, name, event);
            }
            P::WriteResponse {
                ok: true,
                error: None,
                warnings: applied.warnings.into_iter().map(Into::into).collect(),
                result: Some(applied.result),
            }
        }
        Err(E::Db(detail)) => return Err(db_error(detail)),
        Err(E::NotPermitted(what)) => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                format!("not permitted: {what}"),
            ))
        }
        Err(e) => P::WriteResponse {
            ok: false,
            error: Some(op_error(&e)),
            warnings: Vec::new(),
            result: None,
        },
    };
    Ok(MessageBody::OrgStructureBody(response))
}

/// Tells the deputy their cover began or ended, through the platform's notification store (the same
/// one the work handover uses). Best-effort: a failed notification never fails the write it announces.
fn notify_deputy(
    org: &OrgContext,
    deputy: &Deputy,
    kind: &str,
    title: &str,
    names: &HashMap<String, String>,
) {
    if deputy.deputy_user_id == org.user_id {
        return;
    }
    let who = name_of(names, &deputy.user_id);
    let until = deputy
        .valid_to
        .map(|d| format!(" do {}", fmt(d)))
        .unwrap_or_default();
    crate::project_studio::notifications::notify(
        &org.org_id,
        &deputy.deputy_user_id,
        "",
        kind,
        title,
        &format!(
            "{who}: od {}{until}, zakres: {}.",
            fmt(deputy.valid_from),
            deputy.scope.as_wire()
        ),
        &json!({ "deputy_id": deputy.id, "user_id": deputy.user_id }).to_string(),
    );
}

fn scope_of(raw: &str) -> Result<DeputyScope, E> {
    DeputyScope::parse(raw)
}

fn write(ctx: &HandlerContext, org: &OrgContext, payload: &P) -> Result<Applied, E> {
    let pool = &ctx.state.db;
    let wc = |confirm_backdated: bool| WriteCtx {
        org_id: &org.org_id,
        actor_user_id: &org.user_id,
        confirm_backdated,
    };
    let who = svc::Actor {
        is_admin: org.has(PERM_ADMIN),
    };
    let names = availability::display_names(pool, &org.org_id)?;
    let deputy_result = |d: Deputy| OrgWriteResult::Deputy(deputy_to_wire(d, &names));
    match payload {
        P::DeputySetRequest {
            user_id,
            deputy_user_id,
            scope,
            valid_from,
            valid_to,
            confirm_backdated,
        } => {
            svc::ensure_may_write_deputy(&org.user_id, who.is_admin, user_id)?;
            let w = svc::set_deputy(
                pool,
                &wc(*confirm_backdated),
                &svc::NewDeputy {
                    user_id: user_id.clone(),
                    deputy_user_id: deputy_user_id.clone(),
                    scope: scope_of(scope)?,
                    valid_from: super::day(valid_from)?,
                    valid_to: opt_day_write(valid_to)?,
                },
            )?;
            let event = json!({
                "id": w.value.id, "user_id": w.value.user_id,
                "deputy_user_id": w.value.deputy_user_id, "scope": w.value.scope.as_wire(),
            });
            notify_deputy(
                org,
                &w.value,
                "deputy_appointed",
                "Zostałeś zastępcą",
                &names,
            );
            Ok(done(w, deputy_result).event("org.deputy_set", event))
        }
        P::DeputyUpdateRequest {
            id,
            scope,
            valid_from,
            valid_to,
            clear,
            confirm_backdated,
        } => {
            let clear = clear_set(clear, &["valid_to"])?;
            let owner = availability::get_deputy(pool, &org.org_id, id)?.user_id;
            svc::ensure_may_write_deputy(&org.user_id, who.is_admin, &owner)?;
            let w = svc::update_deputy(
                pool,
                &wc(*confirm_backdated),
                id,
                &svc::DeputyPatch {
                    scope: scope.as_deref().map(scope_of).transpose()?,
                    valid_from: opt_day_write(valid_from)?,
                    valid_to: tri("valid_to", &opt_day_write(valid_to)?, &clear)?,
                },
            )?;
            let event = json!({
                "id": w.value.id, "user_id": w.value.user_id,
                "deputy_user_id": w.value.deputy_user_id,
            });
            Ok(done(w, deputy_result).event("org.deputy_updated", event))
        }
        P::DeputyEndRequest {
            id,
            from,
            confirm_backdated,
        } => {
            let before = availability::get_deputy(pool, &org.org_id, id)?;
            svc::ensure_may_write_deputy(&org.user_id, who.is_admin, &before.user_id)?;
            let w = svc::end_deputy(pool, &wc(*confirm_backdated), id, super::day(from)?)?;
            let event = json!({
                "id": id, "user_id": before.user_id, "deputy_user_id": before.deputy_user_id,
            });
            notify_deputy(
                org,
                &before,
                "deputy_ended",
                "Zastępstwo zakończone",
                &names,
            );
            Ok(done(w, |()| OrgWriteResult::Done).event("org.deputy_ended", event))
        }
        P::AbsenceAddRequest {
            user_id,
            valid_from,
            valid_to,
            kind,
            reason,
            confirm_backdated,
        } => {
            let user = user_id
                .clone()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| org.user_id.clone());
            let w = svc::add_absence(
                pool,
                &wc(*confirm_backdated),
                who,
                &svc::NewAbsence {
                    user_id: user,
                    valid_from: super::day(valid_from)?,
                    valid_to: opt_day_write(valid_to)?,
                    kind: kind_from_wire(*kind),
                    reason: reason.clone(),
                },
            )?;
            // Ids and dates only: neither the kind nor the reason goes on the bus.
            let event = json!({
                "id": w.value.id, "user_id": w.value.user_id,
                "valid_from": fmt(w.value.valid_from), "valid_to": w.value.valid_to.map(fmt),
            });
            Ok(done(w, |a| OrgWriteResult::Absence(absence_to_wire(a)))
                .event("org.absence_added", event))
        }
        P::AbsenceUpdateRequest {
            id,
            valid_from,
            valid_to,
            kind,
            reason,
            clear,
            confirm_backdated,
        } => {
            let clear = clear_set(clear, &["valid_to", "reason"])?;
            let w = svc::update_absence(
                pool,
                &wc(*confirm_backdated),
                who,
                id,
                &svc::AbsencePatch {
                    valid_from: opt_day_write(valid_from)?,
                    valid_to: tri("valid_to", &opt_day_write(valid_to)?, &clear)?,
                    kind: kind.map(kind_from_wire),
                    reason: tri("reason", reason, &clear)?,
                },
            )?;
            let event = json!({
                "id": w.value.id, "user_id": w.value.user_id,
                "valid_from": fmt(w.value.valid_from), "valid_to": w.value.valid_to.map(fmt),
            });
            Ok(done(w, |a| OrgWriteResult::Absence(absence_to_wire(a)))
                .event("org.absence_updated", event))
        }
        P::AbsenceDeleteRequest {
            id,
            confirm_backdated,
        } => {
            let before = availability::get_absence(pool, &org.org_id, id)?;
            let w = svc::delete_absence(pool, &wc(*confirm_backdated), who, id)?;
            let event = json!({ "id": id, "user_id": before.user_id });
            Ok(done(w, |()| OrgWriteResult::Done).event("org.absence_deleted", event))
        }
        _ => Err(E::InvalidValue {
            field: "request",
            reason: "not a deputy or absence request".to_string(),
        }),
    }
}
