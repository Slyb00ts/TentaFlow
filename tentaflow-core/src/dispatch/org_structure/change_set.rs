//! Planned reorganizations over the wire (docs/ORG_STRUCTURE_PLAN.md §2.4).
//!
//! All of it is `org.admin`: a plan names people and moves before anybody is
//! told. The operations are the ordinary write requests of `BatchRequest`,
//! stored as JSON in the change set and run through the batch machinery
//! (batch.rs) — a dry run to validate and preview, and the real run, together
//! with the state change, on approval. Nothing here executes an operation
//! itself, so what a reorganization does is exactly what the same list saved
//! from the edit mode would do.
//!
//! Every refusal is an answer with a typed `OrgOpError` (`self_approval`,
//! `change_set_conflict`, ...) and the change set as it stands, so the screen
//! shows the reason and refreshes its card in one round trip.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tentaflow_protocol::org_history::OrgChangeSet;
use tentaflow_protocol::org_structure as wire;
use tentaflow_protocol::org_structure::OrgStructurePayload as P;
use tentaflow_protocol::{MessageBody, ProtocolError};

use super::batch::{self, AfterKept};
use super::history::view_on;
use super::{db_error, op_error, publish, read_error, require_admin, to_op};
use crate::dispatch::HandlerContext;
use crate::services::org_structure as svc;
use crate::services::org_structure::change_set::{self as cs, ChangeSetError as CE};
use crate::services::org_structure::history::{DiffContext, Privacy};
use crate::services::org_structure::OrgStructureError as E;

pub(super) async fn dispatch(
    ctx: &HandlerContext,
    payload: &P,
) -> Result<MessageBody, ProtocolError> {
    let answer = match payload {
        P::ChangeSetListRequest {} => list(ctx)?,
        P::ChangeSetGetRequest { id } => get(ctx, id)?,
        P::ChangeSetSaveRequest {
            id,
            name,
            effective_date,
            ops,
        } => save(ctx, id.as_deref(), name, effective_date, ops).await?,
        P::ChangeSetSubmitRequest { id } => submit(ctx, id).await?,
        P::ChangeSetApproveRequest { id } => approve(ctx, id).await?,
        P::ChangeSetWithdrawRequest { id } => withdraw(ctx, id)?,
        P::ChangeSetPreviewRequest { id, unit_id } => preview(ctx, id, unit_id).await?,
        _ => return Err(ProtocolError::bad_request("not a change set request")),
    };
    Ok(MessageBody::OrgStructureBody(answer))
}

// =============================================================================
// Wire mapping
// =============================================================================

fn parse_ops(payload: &str) -> Result<Vec<wire::OrgWriteOp>, ProtocolError> {
    serde_json::from_str(&cs::ops_of(payload)).map_err(|e| {
        db_error(format!(
            "stored operations of a change set are unreadable: {e}"
        ))
    })
}

fn to_wire(set: &cs::ChangeSet, with_ops: bool) -> Result<OrgChangeSet, ProtocolError> {
    Ok(OrgChangeSet {
        id: set.id.clone(),
        name: set.name.clone(),
        effective_date: svc::validate::format_date(set.effective_date),
        state: set.state.as_str().to_string(),
        author_user_id: set.author_user_id.clone(),
        author_name: set.author_name.clone(),
        approver_user_id: set.approver_user_id.clone(),
        approver_name: set.approver_name.clone(),
        created_at_ms: set.created_at_ms,
        op_count: set.op_count as u32,
        ops: if with_ops {
            parse_ops(&set.payload)?
        } else {
            Vec::new()
        },
    })
}

fn error_of(e: &CE) -> wire::OrgOpError {
    match e {
        CE::Org(inner) => op_error(inner),
        CE::NotFound(id) | CE::State { id, .. } => wire::OrgOpError {
            code: e.code().to_string(),
            message: e.to_string(),
            id: Some(id.clone()),
            ..Default::default()
        },
        CE::EffectiveDatePassed { date, .. } => wire::OrgOpError {
            code: e.code().to_string(),
            message: e.to_string(),
            date: Some(date.clone()),
            ..Default::default()
        },
        CE::Started { date } => wire::OrgOpError {
            code: e.code().to_string(),
            message: e.to_string(),
            date: Some(date.clone()),
            ..Default::default()
        },
        CE::SelfApproval
        | CE::Invalid { .. }
        | CE::Conflict { .. }
        | CE::Dependents { .. }
        | CE::NoUndo => wire::OrgOpError {
            code: e.code().to_string(),
            message: e.to_string(),
            ..Default::default()
        },
    }
}

/// A storage failure is a protocol error; every other refusal is an answer.
fn refusal(e: &CE) -> Result<(), ProtocolError> {
    match e {
        CE::Org(E::Db(detail)) => Err(db_error(detail.clone())),
        _ => Ok(()),
    }
}

/// The answer to a refused request: the reason, and the change set as it is now
/// (absent when it does not exist).
fn refused(ctx: &HandlerContext, org_id: &str, id: &str, e: &CE) -> Result<P, ProtocolError> {
    refusal(e)?;
    let current = cs::get(&ctx.state.db, org_id, id)
        .ok()
        .map(|set| to_wire(&set, false))
        .transpose()?;
    Ok(P::ChangeSetResponse {
        ok: false,
        error: Some(error_of(e)),
        change_set: current,
        valid: false,
        results: Vec::new(),
        warnings: Vec::new(),
    })
}

// =============================================================================
// Dry run
// =============================================================================

struct DryRun {
    valid: bool,
    results: Vec<wire::OrgBatchOpResult>,
    warnings: Vec<wire::OrgWarning>,
    preview_at: Option<String>,
    preview: Option<wire::OrgStructureView>,
    error: Option<wire::OrgOpError>,
}

fn batch_answer(body: MessageBody) -> Result<P, ProtocolError> {
    match body {
        MessageBody::OrgStructureBody(p @ P::BatchResponse { .. }) => Ok(p),
        _ => Err(db_error(
            "the batch answered with something else".to_string(),
        )),
    }
}

/// The operations of a plan run on a transaction that is dropped: what
/// approving would do to the structure today. An operation dated before the
/// plan's day would take effect early, so it fails the run too.
async fn dry_run(
    ctx: &HandlerContext,
    ops: &[wire::OrgWriteOp],
    effective: chrono::NaiveDate,
) -> Result<DryRun, ProtocolError> {
    let P::BatchResponse {
        ok,
        error,
        mut results,
        warnings,
        preview_at,
        preview,
        ..
    } = batch_answer(batch::dispatch(ctx, ops, true, false).await?)?
    else {
        unreachable!("batch_answer returns a BatchResponse");
    };
    let mut valid = ok;
    for (index, op) in ops.iter().enumerate() {
        let Ok((svc_op, _)) = to_op(&op.request) else {
            continue;
        };
        let Some(day) = svc_op.effective_from().filter(|day| *day < effective) else {
            continue;
        };
        if let Some(result) = results.get_mut(index) {
            if result.ok {
                result.ok = false;
                result.error = Some(wire::OrgOpError {
                    code: "op_before_effective_date".to_string(),
                    message: format!(
                        "the operation takes effect {} but the reorganization {}",
                        svc::validate::format_date(day),
                        svc::validate::format_date(effective)
                    ),
                    date: Some(svc::validate::format_date(day)),
                    ..Default::default()
                });
            }
        }
        valid = false;
    }
    Ok(DryRun {
        valid: valid && error.is_none(),
        results,
        warnings,
        preview_at,
        preview,
        error,
    })
}

// =============================================================================
// Requests
// =============================================================================

fn list(ctx: &HandlerContext) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let items = match cs::list(&ctx.state.db, &org.org_id) {
        Ok(items) => items,
        Err(e) => {
            refusal(&e)?;
            return Err(ProtocolError::bad_request(e.to_string()));
        }
    };
    Ok(P::ChangeSetListResponse {
        items: items
            .iter()
            .map(|set| to_wire(set, false))
            .collect::<Result<_, _>>()?,
        today: svc::validate::format_date(
            svc::org_today(&ctx.state.db, &org.org_id).map_err(read_error)?,
        ),
        sole_admin: cs::is_sole_admin(&ctx.state.db, &org.org_id)
            .map_err(|e| db_error(e.to_string()))?,
    })
}

fn get(ctx: &HandlerContext, id: &str) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    match cs::get(&ctx.state.db, &org.org_id, id) {
        Ok(set) => Ok(P::ChangeSetResponse {
            ok: true,
            error: None,
            change_set: Some(to_wire(&set, true)?),
            valid: false,
            results: Vec::new(),
            warnings: Vec::new(),
        }),
        Err(e) => refused(ctx, &org.org_id, id, &e),
    }
}

async fn save(
    ctx: &HandlerContext,
    id: Option<&str>,
    name: &str,
    effective_date: &str,
    ops: &[wire::OrgWriteOp],
) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let (org_id, actor) = (org.org_id.clone(), org.user_id.clone());
    let plain = |e: E| Ok(refused_without_set(&CE::Org(e)));
    let day = match svc::validate::parse_date(effective_date) {
        Ok(day) => day,
        Err(e) => return plain(e),
    };
    if ops.len() > svc::batch::MAX_OPS {
        return Ok(refused_without_set(&CE::Org(E::InvalidValue {
            field: "ops",
            reason: format!(
                "a reorganization takes at most {} operations",
                svc::batch::MAX_OPS
            ),
        })));
    }
    let payload = serde_json::to_string(ops)
        .map_err(|e| db_error(format!("operations cannot be stored: {e}")))?;
    let saved = match cs::save(&ctx.state.db, &org_id, &actor, id, name, day, &payload) {
        Ok(saved) => saved,
        Err(e) => return refused(ctx, &org_id, id.unwrap_or_default(), &e),
    };
    let run = dry_run(ctx, ops, day).await?;
    Ok(P::ChangeSetResponse {
        ok: true,
        error: run.error,
        change_set: Some(to_wire(&saved, true)?),
        valid: run.valid,
        results: run.results,
        warnings: run.warnings,
    })
}

/// A refusal that has no change set to show (nothing was created).
fn refused_without_set(e: &CE) -> P {
    P::ChangeSetResponse {
        ok: false,
        error: Some(error_of(e)),
        change_set: None,
        valid: false,
        results: Vec::new(),
        warnings: Vec::new(),
    }
}

async fn submit(ctx: &HandlerContext, id: &str) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let (org_id, actor) = (org.org_id.clone(), org.user_id.clone());
    let set = match cs::get(&ctx.state.db, &org_id, id) {
        Ok(set) => set,
        Err(e) => return refused(ctx, &org_id, id, &e),
    };
    let ops = parse_ops(&set.payload)?;
    let run = dry_run(ctx, &ops, set.effective_date).await?;
    if !run.valid {
        let failed = run.results.iter().filter(|r| !r.ok).count();
        return Ok(P::ChangeSetResponse {
            ok: false,
            error: Some(error_of(&CE::Invalid { failed })),
            change_set: Some(to_wire(&set, false)?),
            valid: false,
            results: run.results,
            warnings: run.warnings,
        });
    }
    match cs::submit(&ctx.state.db, &org_id, &actor, id) {
        Ok(submitted) => {
            publish(
                ctx,
                org,
                "org.change_set_submitted",
                json!({ "change_set_id": submitted.id, "effective_date": svc::validate::format_date(submitted.effective_date) }),
            );
            Ok(P::ChangeSetResponse {
                ok: true,
                error: None,
                change_set: Some(to_wire(&submitted, false)?),
                valid: true,
                results: run.results,
                warnings: run.warnings,
            })
        }
        Err(e) => refused(ctx, &org_id, id, &e),
    }
}

/// The approval: refused early with a typed reason; otherwise the operations
/// run as ONE batch and `applied` is recorded on the batch's own transaction.
async fn approve(ctx: &HandlerContext, id: &str) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let (org_id, actor) = (org.org_id.clone(), org.user_id.clone());
    let set = match cs::check_approvable(&ctx.state.db, &org_id, &actor, id) {
        Ok(set) => set,
        Err(e) => return refused(ctx, &org_id, id, &e),
    };
    let ops = parse_ops(&set.payload)?;

    // What the recording step refused, when it did: the batch reports only that
    // it failed, the reason belongs to the reorganization.
    let recorded: Arc<Mutex<Option<CE>>> = Arc::new(Mutex::new(None));
    // The rows as they are before the first operation, to know afterwards exactly what the approval changed.
    let before_rows: Arc<Mutex<Option<cs::Snapshot>>> = Arc::new(Mutex::new(None));
    let before: AfterKept = {
        let before_rows = before_rows.clone();
        Box::new(move |batch| {
            batch.with_transaction(|tx, org_id| {
                let rows = cs::snapshot_in(tx, org_id).map_err(|e| E::Db(e.to_string()))?;
                *before_rows.lock().expect("the slot is never poisoned") = Some(rows);
                Ok(())
            })
        })
    };
    let after: AfterKept = {
        let (recorded, actor, id) = (recorded.clone(), actor.clone(), id.to_string());
        Box::new(move |batch| {
            batch.with_transaction(|tx, org_id| {
                let fail = |e: CE| {
                    let as_org = E::InvalidValue {
                        field: "change_set",
                        reason: e.to_string(),
                    };
                    *recorded.lock().expect("the slot is never poisoned") = Some(e);
                    as_org
                };
                let was = before_rows
                    .lock()
                    .expect("the slot is never poisoned")
                    .take()
                    .unwrap_or_default();
                let now = cs::snapshot_in(tx, org_id).map_err(&fail)?;
                cs::mark_applied_in(tx, org_id, &actor, &id, &cs::undo_between(&was, &now))
                    .map_err(fail)
            })
        })
    };
    // The ids of the rows the plan makes follow from its own id and the operation's index: another
    // administrator approving the same plan on another node at the same moment makes the SAME rows.
    let outcome = batch::dispatch_with(
        ctx,
        &ops,
        false,
        false,
        Some(before),
        Some(after),
        Some(id.to_string()),
    )
    .await;
    if let Some(e) = recorded.lock().expect("the slot is never poisoned").take() {
        return refused(ctx, &org_id, id, &e);
    }
    let P::BatchResponse {
        applied,
        error,
        results,
        warnings,
        ..
    } = batch_answer(outcome?)?
    else {
        unreachable!("batch_answer returns a BatchResponse");
    };
    let current = cs::get(&ctx.state.db, &org_id, id).map_err(|e| db_error(e.to_string()))?;
    if applied {
        publish(
            ctx,
            org,
            "org.change_set_approved",
            json!({ "change_set_id": current.id, "effective_date": svc::validate::format_date(current.effective_date) }),
        );
        return Ok(P::ChangeSetResponse {
            ok: true,
            error: None,
            change_set: Some(to_wire(&current, true)?),
            valid: true,
            results,
            warnings,
        });
    }
    let failed = results.iter().filter(|r| !r.ok).count();
    Ok(P::ChangeSetResponse {
        ok: false,
        error: error.or_else(|| Some(error_of(&CE::Conflict { failed }))),
        change_set: Some(to_wire(&current, true)?),
        valid: false,
        results,
        warnings,
    })
}

fn withdraw(ctx: &HandlerContext, id: &str) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    match cs::withdraw(&ctx.state.db, &org.org_id, &org.user_id, id) {
        Ok(withdrawn) => {
            publish(
                ctx,
                org,
                "org.change_set_withdrawn",
                json!({ "change_set_id": withdrawn.id }),
            );
            Ok(P::ChangeSetResponse {
                ok: true,
                error: None,
                change_set: Some(to_wire(&withdrawn, false)?),
                valid: false,
                results: Vec::new(),
                warnings: Vec::new(),
            })
        }
        Err(e) => refused(ctx, &org.org_id, id, &e),
    }
}

async fn preview(
    ctx: &HandlerContext,
    id: &str,
    unit_id: &Option<String>,
) -> Result<P, ProtocolError> {
    let org = require_admin(ctx)?;
    let org_id = org.org_id.clone();
    let set = match cs::get(&ctx.state.db, &org_id, id) {
        Ok(set) => set,
        Err(e) => {
            refusal(&e)?;
            return Ok(P::ChangeSetPreviewResponse {
                ok: false,
                error: Some(error_of(&e)),
                change_set: None,
                valid: false,
                results: Vec::new(),
                warnings: Vec::new(),
                at: String::new(),
                live: None,
                preview: None,
                items: Vec::new(),
            });
        }
    };
    let ops = parse_ops(&set.payload)?;
    let effective = svc::validate::format_date(set.effective_date);

    // Only a plan that is still a document can be tried; an applied one is
    // already in the structure and a withdrawn one never will be.
    if !matches!(set.state, cs::State::Draft | cs::State::Pending) {
        let live = view_on(ctx, &org_id, Some(set.effective_date))?;
        return Ok(P::ChangeSetPreviewResponse {
            ok: true,
            error: None,
            change_set: Some(to_wire(&set, true)?),
            valid: set.state == cs::State::Applied,
            results: Vec::new(),
            warnings: Vec::new(),
            at: effective,
            live: Some(live.clone()),
            preview: (set.state == cs::State::Applied).then_some(live),
            items: Vec::new(),
        });
    }

    let run = dry_run(ctx, &ops, set.effective_date).await?;
    let at = run.preview_at.clone().unwrap_or(effective);
    let day = svc::validate::parse_date(&at).map_err(read_error)?;
    let live = view_on(ctx, &org_id, Some(day))?;
    let items = match &run.preview {
        Some(after) => {
            let unit = unit_id.clone().filter(|u| !u.trim().is_empty());
            svc::history::diff_views(
                &live,
                after,
                &Privacy {
                    personal_visible: true,
                    viewer_user_id: &org.user_id,
                },
                &DiffContext {
                    type_names: svc::history::unit_type_names(&ctx.state.db, &org_id)
                        .map_err(read_error)?,
                    unit_id: unit.as_deref(),
                },
            )
        }
        None => Vec::new(),
    };
    Ok(P::ChangeSetPreviewResponse {
        ok: true,
        error: run.error,
        change_set: Some(to_wire(&set, true)?),
        valid: run.valid,
        results: run.results,
        warnings: run.warnings,
        at,
        live: Some(live),
        preview: run.preview,
        items,
    })
}
