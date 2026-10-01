// =============================================================================
// File: dispatch/org_structure.rs — organizational structure: units, positions,
//       reporting lines and assignments over the binary protocol
// =============================================================================
//
// `MessageBody::OrgStructureBody` is the control plane of the structure the
// permission checks and the projects read. The wire tier is `UserSession`; the
// real gates are inside:
//   * reads — every MEMBER of the organization in the session context (docs
//     §6.1: the structure is visible to everyone with access to the program);
//   * writes — `org.admin` only, otherwise `PolicyDenied`.
// A caller who is not a member of the organization gets `NotFound` for reads
// and writes alike, so an error code cannot be used to probe another tenant.
// A validation failure is a normal answer (`WriteResponse { ok: false, .. }`)
// the screen turns into a message; only authorization and storage failures are
// protocol errors.
//
// Every successful write publishes an `org.*` event carrying ids only. The
// audit entry is written by the repository in the same transaction as the
// change, so this layer does not duplicate it.

use std::collections::HashSet;

use chrono::NaiveDate;
use serde_json::{json, Value as Json};
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::org_structure as wire;
use tentaflow_protocol::org_structure::{OrgStructurePayload as P, OrgWriteResult};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

mod batch;
mod change_set;
mod cover;
mod handover;
mod history;

use super::HandlerContext;
use crate::services::org_structure as svc;
use crate::services::org_structure::batch::{Op, OpValue};
use crate::services::org_structure::query::{Direction, Target};
use crate::services::org_structure::{OrgStructureError as E, Subject, WriteCtx};
use crate::services::rbac::OrgContext;

const PERM_ADMIN: &str = "org.admin";

/// The caller must belong to the organization of the session.
fn require_member(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let not_found = || ProtocolError::new(ProtocolErrorCode::NotFound, "organization not found");
    let org = ctx.org_context.as_ref().ok_or_else(not_found)?;
    let conn = ctx.state.db.read().map_err(|e| db_error(e.to_string()))?;
    let member: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM org_memberships WHERE org_id = ?1 AND user_id = ?2)",
            [&org.org_id, &org.user_id],
            |r| r.get(0),
        )
        .map_err(|e| db_error(e.to_string()))?;
    if member {
        Ok(org)
    } else {
        Err(not_found())
    }
}

fn require_admin(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_member(ctx)?;
    if !org.has(PERM_ADMIN) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{PERM_ADMIN} permission required"),
        ));
    }
    Ok(org)
}

fn db_error(detail: String) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::Internal,
        format!("org_structure: {detail}"),
    )
}

/// A failure of a READ: there is no response body to carry it.
fn read_error(e: E) -> ProtocolError {
    match e {
        // A position that is not in the snapshot of this organization on the
        // day is, for the caller, one that does not exist — whether it never
        // did or belongs to another tenant.
        E::NotFound { entity, id }
        | E::CrossOrgReference { entity, id }
        | E::NotValidAt { entity, id, .. } => ProtocolError::new(
            ProtocolErrorCode::NotFound,
            format!("{entity} not found: {id}"),
        ),
        E::Db(detail) => db_error(detail),
        other => ProtocolError::bad_request(other.to_string()),
    }
}

#[handler(variant = "OrgStructureBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn org_structure_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::OrgStructureBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected OrgStructureBody")),
    };
    let body = match payload {
        P::StructureRequest { at } => structure(ctx, at.as_deref())?,
        P::ReportsChainRequest {
            target,
            direction,
            seat_scope,
            at,
        } => reports_chain(ctx, target, *direction, *seat_scope, at.as_deref())?,
        P::SubordinatesRequest {
            target,
            transitive,
            seat_scope,
            at,
        } => subordinates(ctx, target, *transitive, *seat_scope, at.as_deref())?,
        P::ManagerRequest { user_id, at } => manager(ctx, user_id, at.as_deref())?,
        P::AssignmentRequest { user_id, at } => assignment(ctx, user_id, at.as_deref())?,
        P::IntegrityReportRequest { at } => integrity_report(ctx, at.as_deref())?,
        P::RecomputeRequest {} => recompute(ctx)?,
        P::ExportRequest { format, at } => export(ctx, *format, at.as_deref())?,
        P::ImportDryRunRequest { .. }
        | P::ImportApplyRequest { .. }
        | P::ExportErrorsRequest { .. } => return import_dispatch(ctx, payload).await,
        P::BatchRequest {
            ops,
            dry_run,
            confirm_backdated,
        } => return batch::dispatch(ctx, ops, *dry_run, *confirm_backdated).await,
        P::HistoryListRequest {
            from,
            to,
            unit_id,
            offset,
            limit,
        } => return history::list(ctx, from, to, unit_id, *offset, *limit).await,
        P::HistoryDiffRequest { from, to, unit_id } => {
            return history::diff(ctx, from, to, unit_id)
        }
        P::ChangeSetListRequest {}
        | P::ChangeSetGetRequest { .. }
        | P::ChangeSetSaveRequest { .. }
        | P::ChangeSetSubmitRequest { .. }
        | P::ChangeSetApproveRequest { .. }
        | P::ChangeSetWithdrawRequest { .. }
        | P::ChangeSetPreviewRequest { .. } => return change_set::dispatch(ctx, payload).await,
        P::CoverRequest { .. }
        | P::MemberListRequest { .. }
        | P::AvailabilityRequest { .. }
        | P::EscalationChainRequest { .. }
        | P::IsAvailableRequest { .. }
        | P::CanViewPersonDataRequest { .. }
        | P::VisibilityRequest { .. }
        | P::WhoSeesRequest { .. }
        | P::DeputySetRequest { .. }
        | P::DeputyUpdateRequest { .. }
        | P::DeputyEndRequest { .. }
        | P::AbsenceAddRequest { .. }
        | P::AbsenceUpdateRequest { .. }
        | P::AbsenceDeleteRequest { .. } => return cover::dispatch(ctx, payload),
        P::HandoverListRequest { .. }
        | P::HandoverApplyRequest { .. }
        | P::HandoverRetryRequest { .. }
        | P::HandoverPendingRequest { .. }
        | P::HandoverRecordsRequest { .. } => return handover::dispatch(ctx, payload),
        P::CoverResponse { .. }
        | P::MemberListResponse { .. }
        | P::AvailabilityResponse { .. }
        | P::EscalationChainResponse { .. }
        | P::IsAvailableResponse { .. }
        | P::CanViewPersonDataResponse { .. }
        | P::VisibilityResponse { .. }
        | P::WhoSeesResponse { .. }
        | P::StructureResponse { .. }
        | P::ReportsChainResponse { .. }
        | P::SubordinatesResponse { .. }
        | P::ManagerResponse { .. }
        | P::AssignmentResponse { .. }
        | P::IntegrityReportResponse { .. }
        | P::RecomputeResponse { .. }
        | P::WriteResponse { .. }
        | P::ImportReportResponse { .. }
        | P::ExportResponse { .. }
        | P::BatchResponse { .. }
        | P::HistoryListResponse { .. }
        | P::HistoryDiffResponse { .. }
        | P::ChangeSetListResponse { .. }
        | P::ChangeSetResponse { .. }
        | P::ChangeSetPreviewResponse { .. }
        | P::HandoverListResponse { .. }
        | P::HandoverApplyResponse { .. }
        | P::HandoverPendingResponse { .. }
        | P::HandoverRecordsResponse { .. } => {
            return Err(ProtocolError::bad_request(
                "that variant is a reply, not a request",
            ))
        }
        write => return apply_write(ctx, write),
    };
    Ok(MessageBody::OrgStructureBody(body))
}

// =============================================================================
// Reads
// =============================================================================

fn opt_day(at: Option<&str>) -> Result<Option<NaiveDate>, ProtocolError> {
    at.filter(|raw| !raw.is_empty())
        .map(svc::validate::parse_date)
        .transpose()
        .map_err(read_error)
}

fn structure(ctx: &HandlerContext, at: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let at = opt_day(at)?;
    let view = svc::query::structure_as_of(&ctx.state.db, &org.org_id, at).map_err(read_error)?;
    let unit_types = svc::list_unit_types(&ctx.state.db, &org.org_id).map_err(read_error)?;
    let mut view: wire::OrgStructureView = view.into();
    // Who held a position on a past day is the person's history (docs §6.3).
    svc::history::pseudonymize_past_holders(
        &mut view,
        svc::org_today(&ctx.state.db, &org.org_id).map_err(read_error)?,
        &svc::history::Privacy {
            personal_visible: org.has(PERM_ADMIN),
            viewer_user_id: &org.user_id,
        },
    );
    Ok(P::StructureResponse {
        view,
        unit_types: unit_types.into_iter().map(Into::into).collect(),
        my_permissions: [PERM_ADMIN]
            .into_iter()
            .filter(|p| org.has(p))
            .map(str::to_string)
            .collect(),
        import_max_file_bytes: file::MAX_FILE_BYTES as u32,
        import_max_rows: file::MAX_ROWS as u32,
        batch_max_ops: svc::batch::MAX_OPS as u32,
    })
}

fn target_of(target: &wire::OrgTarget) -> Target {
    match target {
        wire::OrgTarget::User(id) => Target::User(id.clone()),
        wire::OrgTarget::Position(id) => Target::Position(id.clone()),
    }
}

fn seat_scope_of(scope: wire::OrgSeatScope) -> svc::query::SeatScope {
    match scope {
        wire::OrgSeatScope::Primary => svc::query::SeatScope::Primary,
        wire::OrgSeatScope::All => svc::query::SeatScope::All,
    }
}

fn reports_chain(
    ctx: &HandlerContext,
    target: &wire::OrgTarget,
    direction: wire::OrgDirection,
    seat_scope: wire::OrgSeatScope,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let direction = match direction {
        wire::OrgDirection::Up => Direction::Up,
        wire::OrgDirection::Down => Direction::Down,
    };
    let links = svc::query::get_reports_chain(
        &ctx.state.db,
        &org.org_id,
        &target_of(target),
        direction,
        seat_scope_of(seat_scope),
        opt_day(at)?,
    )
    .map_err(read_error)?;
    Ok(P::ReportsChainResponse {
        links: links.into_iter().map(Into::into).collect(),
    })
}

fn subordinates(
    ctx: &HandlerContext,
    target: &wire::OrgTarget,
    transitive: bool,
    seat_scope: wire::OrgSeatScope,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let links = svc::query::get_subordinates(
        &ctx.state.db,
        &org.org_id,
        &target_of(target),
        transitive,
        seat_scope_of(seat_scope),
        opt_day(at)?,
    )
    .map_err(read_error)?;
    Ok(P::SubordinatesResponse {
        links: links.into_iter().map(Into::into).collect(),
    })
}

fn manager(ctx: &HandlerContext, user_id: &str, at: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let manager = svc::query::get_manager(&ctx.state.db, &org.org_id, user_id, opt_day(at)?)
        .map_err(read_error)?;
    Ok(P::ManagerResponse {
        manager: manager.map(|m| wire::OrgManager {
            user_id: m.user_id,
            position_id: m.position_id,
            source: match m.source {
                svc::query::ManagerSource::PrimaryHolder => "primary_holder",
                svc::query::ManagerSource::DeputyHead => "deputy_head",
                svc::query::ManagerSource::Deputy => "deputy",
            }
            .to_string(),
        }),
    })
}

fn assignment(ctx: &HandlerContext, user_id: &str, at: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let held = svc::query::get_assignment(&ctx.state.db, &org.org_id, user_id, opt_day(at)?)
        .map_err(read_error)?;
    Ok(P::AssignmentResponse {
        primary: held.primary.map(Into::into),
        others: held.others.into_iter().map(Into::into).collect(),
    })
}

fn integrity_report(ctx: &HandlerContext, at: Option<&str>) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    let violations =
        svc::integrity_report(&ctx.state.db, &org.org_id, opt_day(at)?).map_err(read_error)?;
    Ok(P::IntegrityReportResponse {
        violations: violations.into_iter().map(Into::into).collect(),
    })
}

/// "Przelicz": not a structure change, so no event — but it rewrites the
/// permission projection of the whole organization, hence admin only.
fn recompute(ctx: &HandlerContext) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let report = svc::projection::recompute_all(&ctx.state.db, &org.org_id).map_err(read_error)?;
    Ok(P::RecomputeResponse {
        written: report.written as u32,
        removed: report.removed as u32,
        unchanged: report.unchanged as u32,
        cycles_broken: report.cycles_broken,
    })
}

// =============================================================================
// File import and export
// =============================================================================

use svc::import as file;

fn format_of(format: wire::OrgFileFormat) -> file::FileFormat {
    match format {
        wire::OrgFileFormat::Csv => file::FileFormat::Csv,
        wire::OrgFileFormat::Xlsx => file::FileFormat::Xlsx,
    }
}

fn export(
    ctx: &HandlerContext,
    format: wire::OrgFileFormat,
    at: Option<&str>,
) -> Result<P, ProtocolError> {
    let org = require_member(ctx)?;
    // A file of a past day lists who held each position then: the person's
    // history (docs §6.3), an administrator's.
    if !org.has(PERM_ADMIN) {
        let today = svc::org_today(&ctx.state.db, &org.org_id).map_err(read_error)?;
        if opt_day(at)?.is_some_and(|day| day < today) {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                format!("{PERM_ADMIN} permission required to export a past day"),
            ));
        }
    }
    // Every member sees the structure, so every member may export it; the
    // logins and e-mail addresses of other people are an administrator's.
    let preferred = crate::db::repository::get_user_preferred_language(&ctx.state.db, &org.user_id)
        .map_err(|e| db_error(e.to_string()))?;
    let exported = file::export::export_structure(
        &ctx.state.db,
        &org.org_id,
        format_of(format),
        opt_day(at)?,
        org.has(PERM_ADMIN),
        file::columns::HeaderLanguage::of_preference(preferred.as_deref()),
    )
    .map_err(read_error)?;
    Ok(exported_file(exported))
}

fn exported_file(file: file::export::ExportFile) -> P {
    P::ExportResponse {
        file_name: file.file_name,
        mime: file.mime.to_string(),
        bytes: file.bytes,
    }
}

/// A dry run, an apply and the error report are three views of one run of the
/// file; the work is disk and CPU for seconds on a big file, so it runs off
/// the async runtime.
async fn import_dispatch(ctx: &HandlerContext, payload: &P) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let (
        commit,
        errors_only,
        format,
        bytes,
        mode,
        as_of,
        confirm_backdated,
        confirm_ended,
        resolutions,
    ) = match payload {
        P::ImportDryRunRequest {
            format,
            bytes,
            mode,
            as_of,
            confirm_backdated,
            confirm_ended,
            resolutions,
        } => (
            false,
            false,
            format,
            bytes,
            mode,
            as_of,
            confirm_backdated,
            confirm_ended,
            resolutions,
        ),
        P::ImportApplyRequest {
            format,
            bytes,
            mode,
            as_of,
            confirm_backdated,
            confirm_ended,
            resolutions,
        } => (
            true,
            false,
            format,
            bytes,
            mode,
            as_of,
            confirm_backdated,
            confirm_ended,
            resolutions,
        ),
        P::ExportErrorsRequest {
            format,
            bytes,
            mode,
            as_of,
            confirm_backdated,
            confirm_ended,
            resolutions,
        } => (
            false,
            true,
            format,
            bytes,
            mode,
            as_of,
            confirm_backdated,
            confirm_ended,
            resolutions,
        ),
        _ => return Err(ProtocolError::bad_request("not an import request")),
    };
    if resolutions.len() > file::MAX_RESOLUTIONS {
        return Err(ProtocolError::bad_request(format!(
            "more than {} decisions in one request",
            file::MAX_RESOLUTIONS
        )));
    }
    let as_of = opt_day(as_of.as_deref())?;
    let format = format_of(*format);
    let mode = match mode {
        wire::OrgImportMode::Upsert => file::Mode::Upsert,
        wire::OrgImportMode::Replace => file::Mode::Replace,
    };
    let resolutions: Vec<file::Resolution> = resolutions
        .iter()
        .map(|r| file::Resolution {
            row: r.row,
            login: r.login.clone(),
            action: match r.action {
                wire::OrgImportAction::UseSuggestedLogin => {
                    file::ResolutionAction::UseSuggestedLogin
                }
                wire::OrgImportAction::LeaveVacant => file::ResolutionAction::LeaveVacant,
                wire::OrgImportAction::SkipRow => file::ResolutionAction::SkipRow,
            },
        })
        .collect();

    let pool = ctx.state.db.clone();
    let (org_id, user_id) = (org.org_id.clone(), org.user_id.clone());
    let (bytes, confirm_backdated, confirm_ended) =
        (bytes.clone(), *confirm_backdated, *confirm_ended);
    let report = tokio::task::spawn_blocking(move || {
        let write_ctx = WriteCtx {
            org_id: &org_id,
            actor_user_id: &user_id,
            confirm_backdated,
        };
        file::run(
            &pool,
            &write_ctx,
            &file::ImportRequest {
                format,
                bytes: &bytes,
                mode,
                as_of,
                resolutions: &resolutions,
                confirm_ended,
            },
            commit,
        )
    })
    .await
    .map_err(|e| db_error(format!("import task: {e}")))?
    .map_err(read_error)?;

    if errors_only {
        if let Some(error) = &report.file_error {
            return Err(ProtocolError::bad_request(error.to_string()));
        }
        let exported =
            file::export::export_errors(&report.errors, report.as_of).map_err(read_error)?;
        return Ok(MessageBody::OrgStructureBody(exported_file(exported)));
    }
    if report.applied {
        // One event for the whole file: a listener that needs the details
        // reads the structure, a per-row event would be thousands.
        publish(
            ctx,
            org,
            "org.structure_imported",
            json!({
                "mode": match report.mode { file::Mode::Upsert => "upsert", file::Mode::Replace => "replace" },
                "as_of": fmt(report.as_of),
                "counts": file::counts_json(&report.counts),
            }),
        );
    }
    Ok(MessageBody::OrgStructureBody(P::ImportReportResponse {
        report: report_to_wire(report),
    }))
}

fn issue_to_wire(issue: file::report::Issue) -> wire::OrgImportIssue {
    wire::OrgImportIssue {
        row: issue.row,
        rows: issue.rows,
        column: issue.column.map(|c| c.key().to_string()),
        kind: issue.kind.code().to_string(),
        value: issue.value,
        message: issue.message,
        suggestion: issue.suggestion,
        suggestion_label: issue.suggestion_label,
        code: issue.code.map(str::to_string),
    }
}

fn report_to_wire(report: file::report::Report) -> wire::OrgImportReport {
    use file::report::RowStatus;
    let counts = report.counts;
    wire::OrgImportReport {
        mode: match report.mode {
            file::Mode::Upsert => wire::OrgImportMode::Upsert,
            file::Mode::Replace => wire::OrgImportMode::Replace,
        },
        as_of: fmt(report.as_of),
        preview_at: fmt(report.preview_at),
        applied: report.applied,
        file_error: report.file_error.map(|error| wire::OrgOpError {
            code: error.code().to_string(),
            message: error.to_string(),
            field: error.column().map(|c| c.key().to_string()),
            ..Default::default()
        }),
        sheet: report.sheet,
        ended: report
            .ended
            .into_iter()
            .map(|e| wire::OrgImportEnded {
                kind: e.kind.to_string(),
                code: e.code,
                name: e.name,
                holders: e.holders,
            })
            .collect(),
        max_file_bytes: file::MAX_FILE_BYTES as u32,
        max_rows: file::MAX_ROWS as u32,
        counts: wire::OrgImportCounts {
            rows: counts.rows,
            added: counts.added,
            changed: counts.changed,
            unchanged: counts.unchanged,
            errors: counts.errors,
            issues: counts.issues,
            units_added: counts.units_added,
            units_changed: counts.units_changed,
            units_ended: counts.units_ended,
            positions_added: counts.positions_added,
            positions_changed: counts.positions_changed,
            positions_ended: counts.positions_ended,
            assignments_added: counts.assignments_added,
            assignments_changed: counts.assignments_changed,
            assignments_ended: counts.assignments_ended,
        },
        rows: report
            .rows
            .into_iter()
            .map(|row| wire::OrgImportRow {
                row: row.row,
                status: match row.status {
                    RowStatus::Added => wire::OrgImportRowStatus::Added,
                    RowStatus::Changed => wire::OrgImportRowStatus::Changed,
                    RowStatus::Unchanged => wire::OrgImportRowStatus::Unchanged,
                    RowStatus::Error => wire::OrgImportRowStatus::Error,
                },
                effect: match row.effect {
                    RowStatus::Added => wire::OrgImportRowStatus::Added,
                    RowStatus::Changed => wire::OrgImportRowStatus::Changed,
                    _ => wire::OrgImportRowStatus::Unchanged,
                },
                cells: row
                    .cells
                    .into_iter()
                    .map(|(header, value)| wire::OrgImportCell { header, value })
                    .collect(),
                unit_code: row.unit_code,
                position_code: row.position_code,
                person: row.person,
                changes: row
                    .changes
                    .into_iter()
                    .map(|c| wire::OrgImportFieldChange {
                        entity: c.entity.to_string(),
                        field: c.field.to_string(),
                        before: c.before,
                        after: c.after,
                    })
                    .collect(),
                unit_id: row.unit_id,
                position_id: row.position_id,
            })
            .collect(),
        errors: report.errors.into_iter().map(issue_to_wire).collect(),
        warnings: report.warnings.into_iter().map(issue_to_wire).collect(),
        preview: report.preview.map(Into::into),
        preview_partial: report.preview_partial,
    }
}

// =============================================================================
// Writes
// =============================================================================

/// What one write produced, before it is put on the wire and the bus.
struct Applied {
    result: OrgWriteResult,
    warnings: Vec<svc::Warning>,
    events: Vec<(&'static str, Json)>,
}

impl Applied {
    fn event(mut self, name: &'static str, payload: Json) -> Self {
        self.events.push((name, payload));
        self
    }
}

fn done<T>(written: svc::Written<T>, to_result: impl FnOnce(T) -> OrgWriteResult) -> Applied {
    Applied {
        warnings: written.warnings,
        result: to_result(written.value),
        events: Vec::new(),
    }
}

fn apply_write(ctx: &HandlerContext, payload: &P) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
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
        Err(e) => P::WriteResponse {
            ok: false,
            error: Some(op_error(&e)),
            warnings: Vec::new(),
            result: None,
        },
    };
    Ok(MessageBody::OrgStructureBody(response))
}

/// A field is set by a value and cleared by name; both at once is a
/// contradiction the caller has to resolve, not something to pick a winner in.
fn tri<T: Clone>(
    field: &'static str,
    value: &Option<T>,
    clear: &HashSet<&str>,
) -> Result<Option<Option<T>>, E> {
    match (value, clear.contains(field)) {
        (Some(_), true) => Err(E::InvalidValue {
            field,
            reason: "both set and listed in clear".to_string(),
        }),
        (_, true) => Ok(Some(None)),
        (Some(v), false) => Ok(Some(Some(v.clone()))),
        (None, false) => Ok(None),
    }
}

fn clear_set<'a>(clear: &'a [String], allowed: &[&'static str]) -> Result<HashSet<&'a str>, E> {
    for name in clear {
        if !allowed.contains(&name.as_str()) {
            return Err(E::InvalidValue {
                field: "clear",
                reason: format!("'{name}' cannot be cleared"),
            });
        }
    }
    Ok(clear.iter().map(String::as_str).collect())
}

fn day(raw: &str) -> Result<NaiveDate, E> {
    svc::validate::parse_date(raw)
}

fn opt_day_write(raw: &Option<String>) -> Result<Option<NaiveDate>, E> {
    raw.as_deref()
        .filter(|r| !r.is_empty())
        .map(svc::validate::parse_date)
        .transpose()
}

fn fmt(date: NaiveDate) -> String {
    svc::validate::format_date(date)
}

/// Positions below `position_id` (itself included) on `from`: what a change of
/// its reporting line touches. A failure here must not fail a write that has
/// already committed, so it is logged and the list is left short.
fn affected_subtree(
    ctx: &HandlerContext,
    org_id: &str,
    position_id: &str,
    from: NaiveDate,
) -> Vec<String> {
    let below = svc::query::get_subordinates(
        &ctx.state.db,
        org_id,
        &Target::Position(position_id.to_string()),
        true,
        svc::query::SeatScope::Primary,
        Some(from),
    );
    let mut ids = vec![position_id.to_string()];
    match below {
        Ok(links) => ids.extend(links.into_iter().map(|l| l.position_id)),
        Err(e) => tracing::warn!("org_structure: subtree of {position_id} not resolved: {e}"),
    }
    ids
}

use svc::validate::{MAX_CODE_CHARS, MAX_NAME_CHARS};
const MAX_NOTE_CHARS: usize = 1000;
const MAX_DEPUTY_HEADS: usize = 20;

fn check_len(field: &'static str, value: Option<&str>, max: usize) -> Result<(), E> {
    match value {
        Some(v) if v.chars().count() > max => Err(E::InvalidValue {
            field,
            reason: format!("longer than {max} characters"),
        }),
        _ => Ok(()),
    }
}

/// Sizes are bounded here, at the wire, because the rows are replicated to
/// every node and a name of megabytes would travel with them.
fn check_sizes(payload: &P) -> Result<(), E> {
    match payload {
        P::UnitTypeCreateRequest { name, .. } => check_len("name", Some(name), MAX_NAME_CHARS),
        P::UnitTypeUpdateRequest { name, .. } => check_len("name", name.as_deref(), MAX_NAME_CHARS),
        P::UnitCreateRequest { name, code, .. } => {
            check_len("name", Some(name), MAX_NAME_CHARS)?;
            check_len("code", code.as_deref(), MAX_CODE_CHARS)
        }
        P::UnitUpdateRequest { name, code, .. } => {
            check_len("name", name.as_deref(), MAX_NAME_CHARS)?;
            check_len("code", code.as_deref(), MAX_CODE_CHARS)
        }
        P::PositionCreateRequest { name, code, .. } => {
            check_len("name", Some(name), MAX_NAME_CHARS)?;
            check_len("code", code.as_deref(), MAX_CODE_CHARS)
        }
        P::PositionUpdateRequest { name, code, .. } => {
            check_len("name", name.as_deref(), MAX_NAME_CHARS)?;
            check_len("code", code.as_deref(), MAX_CODE_CHARS)
        }
        P::ExternalPersonCreateRequest {
            display_name,
            email,
            note,
        } => {
            check_len("display_name", Some(display_name), MAX_NAME_CHARS)?;
            check_len("email", email.as_deref(), MAX_NAME_CHARS)?;
            check_len("note", note.as_deref(), MAX_NOTE_CHARS)
        }
        P::DeputyHeadsSetRequest { position_ids, .. } if position_ids.len() > MAX_DEPUTY_HEADS => {
            Err(E::InvalidValue {
                field: "position_ids",
                reason: format!("more than {MAX_DEPUTY_HEADS} deputy heads"),
            })
        }
        _ => Ok(()),
    }
}

/// A write request as the operation the repository runs, and whether the
/// request confirmed rewriting history. The one place the wire's shapes (a
/// patch is set by value and cleared by name, days are text) become the
/// repository's, for a single write and for every operation of a batch.
fn to_op(payload: &P) -> Result<(Op, bool), E> {
    check_sizes(payload)?;
    Ok(match payload {
        P::UnitTypeCreateRequest { name, color, icon } => (
            Op::UnitTypeCreate {
                name: name.clone(),
                color: color.clone(),
                icon: icon.clone(),
            },
            false,
        ),
        P::UnitTypeUpdateRequest {
            id,
            name,
            color,
            icon,
            clear,
        } => {
            let clear = clear_set(clear, &["color", "icon"])?;
            let patch = svc::UnitTypePatch {
                name: name.clone(),
                color: tri("color", color, &clear)?,
                icon: tri("icon", icon, &clear)?,
            };
            (
                Op::UnitTypeUpdate {
                    id: id.clone(),
                    patch,
                },
                false,
            )
        }
        P::UnitTypeDeleteRequest { id } => (Op::UnitTypeDelete { id: id.clone() }, false),
        P::UnitCreateRequest {
            name,
            code,
            type_id,
            parent_unit_id,
            color,
            valid_from,
            valid_to,
            confirm_backdated,
        } => (
            Op::UnitCreate(svc::NewUnit {
                name: name.clone(),
                code: code.clone(),
                type_id: type_id.clone(),
                parent_unit_id: parent_unit_id.clone(),
                color: color.clone(),
                valid_from: day(valid_from)?,
                valid_to: opt_day_write(valid_to)?,
            }),
            *confirm_backdated,
        ),
        P::UnitUpdateRequest {
            unit_id,
            name,
            code,
            type_id,
            color,
            clear,
            from,
            confirm_backdated,
        } => {
            let clear = clear_set(clear, &["code", "type_id", "color"])?;
            let patch = svc::UnitPatch {
                name: name.clone(),
                code: tri("code", code, &clear)?,
                type_id: tri("type_id", type_id, &clear)?,
                color: tri("color", color, &clear)?,
            };
            (
                Op::UnitUpdate {
                    unit_id: unit_id.clone(),
                    patch,
                    from: day(from)?,
                },
                *confirm_backdated,
            )
        }
        P::UnitMoveRequest {
            unit_id,
            new_parent_unit_id,
            from,
            confirm_backdated,
        } => (
            Op::UnitMove {
                unit_id: unit_id.clone(),
                new_parent_unit_id: new_parent_unit_id.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::UnitEndRequest {
            unit_id,
            from,
            confirm_backdated,
        } => (
            Op::UnitEnd {
                unit_id: unit_id.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::HeadSetRequest {
            unit_id,
            head_position_id,
            from,
            confirm_backdated,
        } => (
            Op::HeadSet {
                unit_id: unit_id.clone(),
                head_position_id: head_position_id.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::DeputyHeadsSetRequest {
            unit_id,
            position_ids,
            from,
            confirm_backdated,
        } => (
            Op::DeputyHeadsSet {
                unit_id: unit_id.clone(),
                position_ids: position_ids.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::PositionCreateRequest {
            unit_id,
            name,
            code,
            role_id,
            is_manager,
            is_staff,
            parent_position_id,
            valid_from,
            valid_to,
            confirm_backdated,
        } => (
            Op::PositionCreate(svc::NewPosition {
                unit_id: unit_id.clone(),
                name: name.clone(),
                code: code.clone(),
                role_id: role_id.clone(),
                is_manager: *is_manager,
                is_staff: *is_staff,
                parent_position_id: parent_position_id.clone(),
                valid_from: day(valid_from)?,
                valid_to: opt_day_write(valid_to)?,
            }),
            *confirm_backdated,
        ),
        P::PositionUpdateRequest {
            position_id,
            name,
            code,
            role_id,
            is_manager,
            is_staff,
            clear,
            from,
            confirm_backdated,
        } => {
            let clear = clear_set(clear, &["code", "role_id", "is_manager"])?;
            let patch = svc::PositionPatch {
                name: name.clone(),
                code: tri("code", code, &clear)?,
                role_id: tri("role_id", role_id, &clear)?,
                is_manager: tri("is_manager", is_manager, &clear)?,
                is_staff: *is_staff,
            };
            (
                Op::PositionUpdate {
                    position_id: position_id.clone(),
                    patch,
                    from: day(from)?,
                },
                *confirm_backdated,
            )
        }
        P::PositionMoveRequest {
            position_id,
            new_parent_position_id,
            from,
            confirm_backdated,
        } => (
            Op::PositionMove {
                position_id: position_id.clone(),
                new_parent_position_id: new_parent_position_id.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::PositionEndRequest {
            position_id,
            from,
            confirm_backdated,
        } => (
            Op::PositionEnd {
                position_id: position_id.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::ReportingLineSetRequest {
            position_id,
            parent_position_id,
            kind,
            priority,
            valid_from,
            valid_to,
            confirm_backdated,
        } => (
            Op::ReportingLineSet(svc::NewLine {
                position_id: position_id.clone(),
                parent_position_id: parent_position_id.clone(),
                kind: (*kind).into(),
                priority: *priority,
                valid_from: day(valid_from)?,
                valid_to: opt_day_write(valid_to)?,
            }),
            *confirm_backdated,
        ),
        P::ExternalPersonCreateRequest {
            display_name,
            email,
            note,
        } => (
            Op::ExternalPersonCreate(svc::NewExternalPerson {
                display_name: display_name.clone(),
                email: email.clone(),
                note: note.clone(),
            }),
            false,
        ),
        P::AssignRequest {
            position_id,
            subject,
            assignment_type,
            share,
            is_primary,
            valid_from,
            valid_to,
            confirm_backdated,
        } => (
            Op::Assign(svc::NewAssignment {
                position_id: position_id.clone(),
                subject: subject.clone().into(),
                kind: (*assignment_type).into(),
                share: *share,
                is_primary: *is_primary,
                valid_from: day(valid_from)?,
                valid_to: opt_day_write(valid_to)?,
            }),
            *confirm_backdated,
        ),
        P::AssignmentUpdateRequest {
            assignment_id,
            assignment_type,
            share,
            is_primary,
            from,
            confirm_backdated,
        } => (
            Op::AssignmentUpdate {
                assignment_id: assignment_id.clone(),
                patch: svc::AssignmentPatch {
                    kind: assignment_type.map(Into::into),
                    share: *share,
                    is_primary: *is_primary,
                },
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        P::AssignmentEndRequest {
            assignment_id,
            from,
            confirm_backdated,
        } => (
            Op::AssignmentEnd {
                assignment_id: assignment_id.clone(),
                from: day(from)?,
            },
            *confirm_backdated,
        ),
        // Reads and replies never reach here: `org_structure_dispatch` routes them.
        _ => {
            return Err(E::InvalidValue {
                field: "request",
                reason: "not a write".to_string(),
            })
        }
    })
}

fn to_result(value: OpValue) -> OrgWriteResult {
    match value {
        OpValue::UnitType(t) => OrgWriteResult::UnitType(t.into()),
        OpValue::Unit(u) => OrgWriteResult::Unit(u.into()),
        OpValue::DeputyHeads(rows) => {
            OrgWriteResult::DeputyHeads(rows.into_iter().map(Into::into).collect())
        }
        OpValue::Position(p) => OrgWriteResult::Position(p.into()),
        OpValue::ReportingLine(line) => OrgWriteResult::ReportingLine(line.map(Into::into)),
        OpValue::Ended(e) => OrgWriteResult::Ended(e.into()),
        OpValue::ExternalPerson(p) => OrgWriteResult::ExternalPerson(p.into()),
        OpValue::Assignment(a) => OrgWriteResult::Assignment(a.into()),
        OpValue::Done => OrgWriteResult::Done,
    }
}

/// The events of one committed write: ids only, so a listener that needs the
/// details reads the structure.
fn events_of(
    ctx: &HandlerContext,
    org: &OrgContext,
    payload: &P,
    value: &OpValue,
) -> Vec<(&'static str, Json)> {
    let created = value.entity_id().map(str::to_string);
    match payload {
        P::UnitTypeCreateRequest { .. } => {
            vec![("org.unit_type_created", json!({ "unit_type_id": created }))]
        }
        P::UnitTypeUpdateRequest { id, .. } => {
            vec![("org.unit_type_updated", json!({ "unit_type_id": id }))]
        }
        P::UnitTypeDeleteRequest { id } => {
            vec![("org.unit_type_deleted", json!({ "unit_type_id": id }))]
        }
        P::UnitCreateRequest {
            parent_unit_id,
            valid_from,
            ..
        } => vec![(
            "org.unit_created",
            json!({ "unit_id": created, "parent_unit_id": parent_unit_id, "from": valid_from }),
        )],
        P::UnitUpdateRequest { unit_id, from, .. } => vec![(
            "org.unit_updated",
            json!({ "unit_id": unit_id, "from": from }),
        )],
        P::UnitMoveRequest {
            unit_id,
            new_parent_unit_id,
            from,
            ..
        } => vec![(
            "org.unit_moved",
            json!({ "unit_id": unit_id, "new_parent_unit_id": new_parent_unit_id, "from": from }),
        )],
        P::UnitEndRequest { unit_id, from, .. } => {
            vec![(
                "org.unit_ended",
                json!({ "unit_id": unit_id, "from": from }),
            )]
        }
        P::HeadSetRequest {
            unit_id,
            head_position_id,
            from,
            ..
        } => vec![(
            "org.unit_updated",
            json!({ "unit_id": unit_id, "head_position_id": head_position_id, "from": from }),
        )],
        P::DeputyHeadsSetRequest {
            unit_id,
            position_ids,
            from,
            ..
        } => vec![(
            "org.deputy_heads_changed",
            json!({ "unit_id": unit_id, "position_ids": position_ids, "from": from }),
        )],
        P::PositionCreateRequest {
            unit_id,
            parent_position_id,
            valid_from,
            ..
        } => {
            let mut events = vec![(
                "org.position_created",
                json!({ "position_id": created, "unit_id": unit_id, "from": valid_from }),
            )];
            if let Some(parent) = parent_position_id {
                events.push((
                    "org.reporting_line_changed",
                    json!({
                        "position_id": created,
                        "parent_position_id": parent,
                        "from": valid_from,
                        "affected_position_ids": [created],
                    }),
                ));
            }
            events
        }
        P::PositionUpdateRequest {
            position_id, from, ..
        } => vec![(
            "org.position_updated",
            json!({ "position_id": position_id, "from": from }),
        )],
        P::PositionMoveRequest {
            position_id,
            new_parent_position_id,
            from,
            ..
        } => {
            let affected = day(from)
                .map(|d| affected_subtree(ctx, &org.org_id, position_id, d))
                .unwrap_or_default();
            vec![(
                "org.reporting_line_changed",
                json!({
                    "position_id": position_id,
                    "parent_position_id": new_parent_position_id,
                    "from": from,
                    "affected_position_ids": affected,
                }),
            )]
        }
        P::PositionEndRequest {
            position_id, from, ..
        } => {
            let ended = match value {
                OpValue::Ended(e) => wire::OrgEnded::from(e.clone()),
                _ => wire::OrgEnded::default(),
            };
            vec![(
                "org.position_ended",
                json!({
                    "position_id": position_id,
                    "from": from,
                    "ended_reporting_line_ids": ended.reporting_lines,
                    "ended_deputy_head_ids": ended.deputy_heads,
                    "ended_assignment_ids": ended.assignments,
                }),
            )]
        }
        P::ReportingLineSetRequest {
            position_id,
            parent_position_id,
            valid_from,
            ..
        } => {
            let affected = day(valid_from)
                .map(|d| affected_subtree(ctx, &org.org_id, position_id, d))
                .unwrap_or_default();
            vec![(
                "org.reporting_line_changed",
                json!({
                    "position_id": position_id,
                    "parent_position_id": parent_position_id,
                    "from": valid_from,
                    "affected_position_ids": affected,
                }),
            )]
        }
        P::ExternalPersonCreateRequest { .. } => vec![(
            "org.external_person_created",
            json!({ "external_person_id": created }),
        )],
        P::AssignRequest {
            position_id,
            subject,
            valid_from,
            ..
        } => vec![(
            "org.person_assigned",
            json!({
                "assignment_id": created,
                "position_id": position_id,
                "subject": subject,
                "from": valid_from,
            }),
        )],
        P::AssignmentUpdateRequest {
            assignment_id,
            from,
            ..
        } => vec![(
            "org.assignment_updated",
            json!({ "assignment_id": assignment_id, "from": from }),
        )],
        P::AssignmentEndRequest {
            assignment_id,
            from,
            ..
        } => vec![(
            "org.person_unassigned",
            json!({ "assignment_id": assignment_id, "from": from }),
        )],
        _ => Vec::new(),
    }
}

fn write(ctx: &HandlerContext, org: &OrgContext, payload: &P) -> Result<Applied, E> {
    let pool = &ctx.state.db;
    let wc = |confirm_backdated: bool| WriteCtx {
        org_id: &org.org_id,
        actor_user_id: &org.user_id,
        confirm_backdated,
    };
    if let P::TimezoneSetRequest { timezone } = payload {
        let w = svc::set_timezone(pool, &wc(false), timezone)?;
        let timezone = w.value.timezone.clone();
        return Ok(done(w, |s| OrgWriteResult::Settings(s.into()))
            .event("org.settings_changed", json!({ "timezone": timezone })));
    }
    let (op, confirm_backdated) = to_op(payload)?;
    let written = svc::batch::run_single(pool, &wc(confirm_backdated), &op)?;
    Ok(Applied {
        events: events_of(ctx, org, payload, &written.value),
        result: to_result(written.value),
        warnings: written.warnings,
    })
}

/// Publishes on the process-wide bus (the addon manager's when the node runs
/// one). No bus means nobody subscribed yet; the audit entry is the record.
fn publish(ctx: &HandlerContext, org: &OrgContext, event_type: &str, mut payload: Json) {
    let bus = ctx
        .state
        .addon_manager
        .as_ref()
        .map(|m| m.event_bus().clone())
        .or_else(crate::addon::event_publish::global);
    let Some(bus) = bus else {
        tracing::debug!("org_structure: no event bus, {event_type} not published");
        return;
    };
    if let Json::Object(map) = &mut payload {
        map.insert("org_id".to_string(), json!(org.org_id));
    }
    bus.publish(crate::addon::event_bus::Event {
        event_type: event_type.to_string(),
        source_addon: None,
        source_user: Some(org.user_id.clone()),
        payload,
        timestamp: chrono::Utc::now(),
    });
}

/// The screen keys its message on `code`. A reference to another organization
/// is reported exactly like a missing one, so the answer cannot confirm that
/// an id exists elsewhere.
fn op_error(e: &E) -> wire::OrgOpError {
    let mut out = wire::OrgOpError {
        code: e.code().to_string(),
        message: e.to_string(),
        ..Default::default()
    };
    match e {
        E::NotFound { entity, id } | E::CrossOrgReference { entity, id } => {
            out.message = format!("{entity} not found: {id}");
            out.id = Some(id.clone());
        }
        E::InvalidInterval { from, .. } => out.date = Some(from.clone()),
        E::EmptyField { field } | E::InvalidValue { field, .. } => {
            out.field = Some((*field).to_string());
        }
        E::BackdatedConfirmationRequired { date, .. }
        | E::ReportingCycle { date }
        | E::UnitCycle { date } => {
            out.date = Some(date.clone());
        }
        E::NotValidAt { id, date, .. } => {
            out.id = Some(id.clone());
            out.date = Some(date.clone());
        }
        E::OutsideValidity { id, from, .. } => {
            out.id = Some(id.clone());
            out.date = Some(from.clone());
        }
        E::PrimaryLineOverlap { position_id, from } => {
            out.id = Some(position_id.clone());
            out.date = Some(from.clone());
        }
        E::DuplicateFunctionalLine { position_id, .. }
        | E::AssignmentOverlap { position_id }
        | E::PositionHeadsAnotherUnit { position_id }
        | E::PositionNotInUnit { position_id, .. } => out.id = Some(position_id.clone()),
        E::StaffPositionCannotManage(id)
        | E::PositionIsHead(id)
        | E::HeadIsDeputy(id)
        | E::UnitTypeInUse(id) => out.id = Some(id.clone()),
        E::PrimaryAssignmentOverlap { from } => out.date = Some(from.clone()),
        E::UnitNotEmpty { unit_id, date, .. } => {
            out.id = Some(unit_id.clone());
            out.date = Some(date.clone());
        }
        E::PositionHasSubordinates {
            position_id, date, ..
        } => {
            out.id = Some(position_id.clone());
            out.date = Some(date.clone());
        }
        E::InvalidDate(_)
        | E::InvalidTimezone(_)
        | E::Duplicate { .. }
        | E::NotPermitted(_)
        | E::Db(_) => {}
    }
    out
}

// =============================================================================
// Storage types → wire types
// =============================================================================

impl From<svc::UnitType> for wire::OrgUnitType {
    fn from(t: svc::UnitType) -> Self {
        Self {
            id: t.id,
            name: t.name,
            color: t.color,
            icon: t.icon,
        }
    }
}

fn unit_to_wire(unit: svc::Unit, deputy_head_position_ids: Vec<String>) -> wire::OrgUnit {
    wire::OrgUnit {
        id: unit.id,
        unit_id: unit.unit_id,
        name: unit.name,
        code: unit.code,
        type_id: unit.type_id,
        parent_unit_id: unit.parent_unit_id,
        color: unit.color,
        head_position_id: unit.head_position_id,
        deputy_head_position_ids,
        valid_from: fmt(unit.valid_from),
        valid_to: unit.valid_to.map(fmt),
    }
}

impl From<svc::Unit> for wire::OrgUnit {
    fn from(unit: svc::Unit) -> Self {
        unit_to_wire(unit, Vec::new())
    }
}

impl From<svc::Position> for wire::OrgPosition {
    fn from(p: svc::Position) -> Self {
        Self {
            id: p.id,
            position_id: p.position_id,
            unit_id: p.unit_id,
            name: p.name,
            code: p.code,
            role_id: p.role_id,
            is_manager: p.is_manager,
            is_staff: p.is_staff,
            valid_from: fmt(p.valid_from),
            valid_to: p.valid_to.map(fmt),
            primary_parent_position_id: None,
            functional_parent_position_ids: Vec::new(),
            is_head: false,
            is_vacant: false,
        }
    }
}

impl From<svc::LineKind> for wire::OrgLineKind {
    fn from(kind: svc::LineKind) -> Self {
        match kind {
            svc::LineKind::Primary => Self::Primary,
            svc::LineKind::Functional => Self::Functional,
        }
    }
}

impl From<wire::OrgLineKind> for svc::LineKind {
    fn from(kind: wire::OrgLineKind) -> Self {
        match kind {
            wire::OrgLineKind::Primary => Self::Primary,
            wire::OrgLineKind::Functional => Self::Functional,
        }
    }
}

impl From<svc::ReportingLine> for wire::OrgReportingLine {
    fn from(l: svc::ReportingLine) -> Self {
        Self {
            id: l.id,
            position_id: l.position_id,
            parent_position_id: l.parent_position_id,
            kind: l.kind.into(),
            priority: l.priority,
            valid_from: fmt(l.valid_from),
            valid_to: l.valid_to.map(fmt),
        }
    }
}

impl From<svc::DeputyHead> for wire::OrgDeputyHead {
    fn from(d: svc::DeputyHead) -> Self {
        Self {
            id: d.id,
            unit_id: d.unit_id,
            position_id: d.position_id,
            ord: d.ord,
            valid_from: fmt(d.valid_from),
            valid_to: d.valid_to.map(fmt),
        }
    }
}

impl From<svc::AssignmentType> for wire::OrgAssignmentType {
    fn from(t: svc::AssignmentType) -> Self {
        match t {
            svc::AssignmentType::Permanent => Self::Permanent,
            svc::AssignmentType::Acting => Self::Acting,
            svc::AssignmentType::Contractor => Self::Contractor,
        }
    }
}

impl From<wire::OrgAssignmentType> for svc::AssignmentType {
    fn from(t: wire::OrgAssignmentType) -> Self {
        match t {
            wire::OrgAssignmentType::Permanent => Self::Permanent,
            wire::OrgAssignmentType::Acting => Self::Acting,
            wire::OrgAssignmentType::Contractor => Self::Contractor,
        }
    }
}

impl From<Subject> for wire::OrgSubject {
    fn from(s: Subject) -> Self {
        match s {
            Subject::User(id) => Self::User(id),
            Subject::External(id) => Self::External(id),
        }
    }
}

impl From<wire::OrgSubject> for Subject {
    fn from(s: wire::OrgSubject) -> Self {
        match s {
            wire::OrgSubject::User(id) => Self::User(id),
            wire::OrgSubject::External(id) => Self::External(id),
        }
    }
}

fn assignment_to_wire(a: svc::Assignment, display_name: String) -> wire::OrgAssignment {
    wire::OrgAssignment {
        id: a.id,
        position_id: a.position_id,
        subject: a.subject.into(),
        assignment_type: a.kind.into(),
        share: a.share,
        is_primary: a.is_primary,
        valid_from: fmt(a.valid_from),
        valid_to: a.valid_to.map(fmt),
        display_name,
    }
}

impl From<svc::Assignment> for wire::OrgAssignment {
    fn from(a: svc::Assignment) -> Self {
        assignment_to_wire(a, String::new())
    }
}

impl From<svc::ExternalPerson> for wire::OrgExternalPerson {
    fn from(p: svc::ExternalPerson) -> Self {
        Self {
            id: p.id,
            display_name: p.display_name,
            email: p.email,
            note: p.note,
        }
    }
}

impl From<svc::Settings> for wire::OrgSettings {
    fn from(s: svc::Settings) -> Self {
        Self {
            org_id: s.org_id,
            timezone: s.timezone,
        }
    }
}

impl From<svc::Ended> for wire::OrgEnded {
    fn from(e: svc::Ended) -> Self {
        Self {
            reporting_lines: e.reporting_lines,
            deputy_heads: e.deputy_heads,
            assignments: e.assignments,
        }
    }
}

impl From<svc::Warning> for wire::OrgWarning {
    fn from(w: svc::Warning) -> Self {
        match w {
            svc::Warning::ShareOverbooked {
                subject,
                from,
                total,
            } => Self::ShareOverbooked {
                subject: subject.into(),
                from: fmt(from),
                total,
            },
            svc::Warning::UnitWithoutHead { unit_id, from } => Self::UnitWithoutHead {
                unit_id,
                from: fmt(from),
            },
            svc::Warning::PersonWithoutPrimary { subject, from } => Self::PersonWithoutPrimary {
                subject: subject.into(),
                from: fmt(from),
            },
        }
    }
}

impl From<svc::Violation> for wire::OrgViolation {
    fn from(v: svc::Violation) -> Self {
        match v {
            svc::Violation::VersionsOverlap { entity, id, from } => Self::VersionsOverlap {
                entity,
                id,
                from: fmt(from),
            },
            svc::Violation::PrimaryLinesOverlap { position_id, from } => {
                Self::PrimaryLinesOverlap {
                    position_id,
                    from: fmt(from),
                }
            }
            svc::Violation::PrimaryAssignmentsOverlap { subject, from } => {
                Self::PrimaryAssignmentsOverlap {
                    subject: subject.into(),
                    from: fmt(from),
                }
            }
            svc::Violation::PositionHeadsMultipleUnits { position_id, from } => {
                Self::PositionHeadsMultipleUnits {
                    position_id,
                    from: fmt(from),
                }
            }
            svc::Violation::DeputyIsHead {
                unit_id,
                position_id,
                from,
            } => Self::DeputyIsHead {
                unit_id,
                position_id,
                from: fmt(from),
            },
            svc::Violation::PositionCycle { position_id, from } => Self::PositionCycle {
                position_id,
                from: fmt(from),
            },
            svc::Violation::UnitCycle { unit_id, from } => Self::UnitCycle {
                unit_id,
                from: fmt(from),
            },
            svc::Violation::DanglingReference {
                entity,
                id,
                field,
                missing_id,
            } => Self::DanglingReference {
                entity,
                id,
                field,
                missing_id,
            },
        }
    }
}

impl From<svc::query::Link> for wire::OrgLink {
    fn from(l: svc::query::Link) -> Self {
        Self {
            position_id: l.position_id,
            unit_id: l.unit_id,
            name: l.name,
            depth: l.depth,
            holders: l.holders.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<svc::query::StructureView> for wire::OrgStructureView {
    fn from(v: svc::query::StructureView) -> Self {
        Self {
            at: fmt(v.at),
            timezone: v.timezone,
            units: v
                .units
                .into_iter()
                .map(|u| unit_to_wire(u.unit, u.deputy_head_position_ids))
                .collect(),
            positions: v
                .positions
                .into_iter()
                .map(|p| wire::OrgPosition {
                    primary_parent_position_id: p.primary_parent_position_id,
                    functional_parent_position_ids: p.functional_parent_position_ids,
                    is_head: p.is_head,
                    is_vacant: p.is_vacant,
                    ..p.position.into()
                })
                .collect(),
            assignments: v
                .assignments
                .into_iter()
                .map(|a| assignment_to_wire(a.assignment, a.person.display_name))
                .collect(),
            vacancies: v.vacancies,
            warnings: v.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

// `#[handler]` registers the family name, which no frame carries —
// `variant_name_of` reports the concrete request variant, so each one needs its
// own registry entry pointing at the same dispatch wrapper. The tier is
// `UserSession` for all of them; the membership and `org.admin` checks inside
// the handler are the real gate.
macro_rules! org_structure_handler_entry {
    ($($variant:literal => $metric:literal),+ $(,)?) => {
        $(
            ::inventory::submit! {
                crate::dispatch::HandlerMeta {
                    variant_name: $variant,
                    since_major: 1,
                    since_minor: 0,
                    required_auth: crate::dispatch::SessionAuthKind::UserSession,
                    metric_name: $metric,
                    dispatch_fn: __tentaflow_dispatch_org_structure_dispatch,
                }
            }
        )+
    };
}

org_structure_handler_entry! {
    "OrgStructureStructureRequest" => "tentaflow_ws_handler_org_structure_structure",
    "OrgStructureReportsChainRequest" => "tentaflow_ws_handler_org_structure_reports_chain",
    "OrgStructureSubordinatesRequest" => "tentaflow_ws_handler_org_structure_subordinates",
    "OrgStructureManagerRequest" => "tentaflow_ws_handler_org_structure_manager",
    "OrgStructureAssignmentRequest" => "tentaflow_ws_handler_org_structure_assignment",
    "OrgStructureIntegrityReportRequest" => "tentaflow_ws_handler_org_structure_integrity_report",
    "OrgStructureUnitTypeCreateRequest" => "tentaflow_ws_handler_org_structure_unit_type_create",
    "OrgStructureUnitTypeUpdateRequest" => "tentaflow_ws_handler_org_structure_unit_type_update",
    "OrgStructureUnitTypeDeleteRequest" => "tentaflow_ws_handler_org_structure_unit_type_delete",
    "OrgStructureUnitCreateRequest" => "tentaflow_ws_handler_org_structure_unit_create",
    "OrgStructureUnitUpdateRequest" => "tentaflow_ws_handler_org_structure_unit_update",
    "OrgStructureUnitMoveRequest" => "tentaflow_ws_handler_org_structure_unit_move",
    "OrgStructureUnitEndRequest" => "tentaflow_ws_handler_org_structure_unit_end",
    "OrgStructureHeadSetRequest" => "tentaflow_ws_handler_org_structure_head_set",
    "OrgStructureDeputyHeadsSetRequest" => "tentaflow_ws_handler_org_structure_deputy_heads_set",
    "OrgStructurePositionCreateRequest" => "tentaflow_ws_handler_org_structure_position_create",
    "OrgStructurePositionUpdateRequest" => "tentaflow_ws_handler_org_structure_position_update",
    "OrgStructurePositionMoveRequest" => "tentaflow_ws_handler_org_structure_position_move",
    "OrgStructurePositionEndRequest" => "tentaflow_ws_handler_org_structure_position_end",
    "OrgStructureReportingLineSetRequest" => "tentaflow_ws_handler_org_structure_reporting_line_set",
    "OrgStructureExternalPersonCreateRequest" => "tentaflow_ws_handler_org_structure_external_person_create",
    "OrgStructureAssignRequest" => "tentaflow_ws_handler_org_structure_assign",
    "OrgStructureAssignmentUpdateRequest" => "tentaflow_ws_handler_org_structure_assignment_update",
    "OrgStructureAssignmentEndRequest" => "tentaflow_ws_handler_org_structure_assignment_end",
    "OrgStructureTimezoneSetRequest" => "tentaflow_ws_handler_org_structure_timezone_set",
    "OrgStructureRecomputeRequest" => "tentaflow_ws_handler_org_structure_recompute",
    "OrgStructureImportDryRunRequest" => "tentaflow_ws_handler_org_structure_import_dry_run",
    "OrgStructureImportApplyRequest" => "tentaflow_ws_handler_org_structure_import_apply",
    "OrgStructureExportRequest" => "tentaflow_ws_handler_org_structure_export",
    "OrgStructureExportErrorsRequest" => "tentaflow_ws_handler_org_structure_export_errors",
    "OrgStructureBatchRequest" => "tentaflow_ws_handler_org_structure_batch",
    "OrgStructureHistoryListRequest" => "tentaflow_ws_handler_org_structure_history_list",
    "OrgStructureHistoryDiffRequest" => "tentaflow_ws_handler_org_structure_history_diff",
    "OrgStructureChangeSetListRequest" => "tentaflow_ws_handler_org_structure_change_set_list",
    "OrgStructureChangeSetGetRequest" => "tentaflow_ws_handler_org_structure_change_set_get",
    "OrgStructureChangeSetSaveRequest" => "tentaflow_ws_handler_org_structure_change_set_save",
    "OrgStructureChangeSetSubmitRequest" => "tentaflow_ws_handler_org_structure_change_set_submit",
    "OrgStructureChangeSetApproveRequest" => "tentaflow_ws_handler_org_structure_change_set_approve",
    "OrgStructureChangeSetWithdrawRequest" => "tentaflow_ws_handler_org_structure_change_set_withdraw",
    "OrgStructureChangeSetPreviewRequest" => "tentaflow_ws_handler_org_structure_change_set_preview",
    "OrgStructureCoverRequest" => "tentaflow_ws_handler_org_structure_cover",
    "OrgStructureMemberListRequest" => "tentaflow_ws_handler_org_structure_member_list",
    "OrgStructureAvailabilityRequest" => "tentaflow_ws_handler_org_structure_availability",
    "OrgStructureEscalationChainRequest" => "tentaflow_ws_handler_org_structure_escalation_chain",
    "OrgStructureIsAvailableRequest" => "tentaflow_ws_handler_org_structure_is_available",
    "OrgStructureCanViewPersonDataRequest" => "tentaflow_ws_handler_org_structure_can_view_person_data",
    "OrgStructureVisibilityRequest" => "tentaflow_ws_handler_org_structure_visibility",
    "OrgStructureWhoSeesRequest" => "tentaflow_ws_handler_org_structure_who_sees",
    "OrgStructureDeputySetRequest" => "tentaflow_ws_handler_org_structure_deputy_set",
    "OrgStructureDeputyUpdateRequest" => "tentaflow_ws_handler_org_structure_deputy_update",
    "OrgStructureDeputyEndRequest" => "tentaflow_ws_handler_org_structure_deputy_end",
    "OrgStructureAbsenceAddRequest" => "tentaflow_ws_handler_org_structure_absence_add",
    "OrgStructureAbsenceUpdateRequest" => "tentaflow_ws_handler_org_structure_absence_update",
    "OrgStructureAbsenceDeleteRequest" => "tentaflow_ws_handler_org_structure_absence_delete",
    "OrgStructureHandoverListRequest" => "tentaflow_ws_handler_org_structure_handover_list",
    "OrgStructureHandoverApplyRequest" => "tentaflow_ws_handler_org_structure_handover_apply",
    "OrgStructureHandoverRetryRequest" => "tentaflow_ws_handler_org_structure_handover_retry",
    "OrgStructureHandoverPendingRequest" => "tentaflow_ws_handler_org_structure_handover_pending",
    "OrgStructureHandoverRecordsRequest" => "tentaflow_ws_handler_org_structure_handover_records",
}
