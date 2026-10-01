//! `BatchRequest`: the edit mode's draft saved (or tried) in ONE call.
//!
//! Every operation is an ordinary write request, converted by the same
//! `to_op` a single write uses and run on its own savepoint by the batch
//! session of the service layer. This file adds what only a draft needs:
//! temporary ids that let an operation refer to something an earlier one made,
//! the per-operation report, and ONE event for the whole batch.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use serde_json::json;
use tentaflow_protocol::org_structure as wire;
use tentaflow_protocol::org_structure::OrgStructurePayload as P;
use tentaflow_protocol::{MessageBody, ProtocolError};

use super::{db_error, fmt, op_error, publish, require_admin, to_op, to_result};
use crate::dispatch::HandlerContext;
use crate::services::org_structure as svc;
use crate::services::org_structure::batch::{self as run, OpValue};
use crate::services::org_structure::{OrgStructureError as E, Warning, WriteCtx};

/// What a temporary id may stand for; an id is only accepted where the same
/// kind of thing is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum IdKind {
    UnitType,
    Unit,
    Position,
    Assignment,
    ExternalPerson,
}

/// What the operation makes, when it makes something a later one can name.
fn creates(request: &P) -> Option<IdKind> {
    match request {
        P::UnitTypeCreateRequest { .. } => Some(IdKind::UnitType),
        P::UnitCreateRequest { .. } => Some(IdKind::Unit),
        P::PositionCreateRequest { .. } => Some(IdKind::Position),
        P::AssignRequest { .. } => Some(IdKind::Assignment),
        P::ExternalPersonCreateRequest { .. } => Some(IdKind::ExternalPerson),
        _ => None,
    }
}

/// Only the writes the editor produces; a read, a reply, the timezone (which
/// changes what "today" is for the rest of the batch) and a nested batch are not.
fn is_batch_op(request: &P) -> bool {
    matches!(
        request,
        P::UnitTypeCreateRequest { .. }
            | P::UnitTypeUpdateRequest { .. }
            | P::UnitTypeDeleteRequest { .. }
            | P::UnitCreateRequest { .. }
            | P::UnitUpdateRequest { .. }
            | P::UnitMoveRequest { .. }
            | P::UnitEndRequest { .. }
            | P::HeadSetRequest { .. }
            | P::DeputyHeadsSetRequest { .. }
            | P::PositionCreateRequest { .. }
            | P::PositionUpdateRequest { .. }
            | P::PositionMoveRequest { .. }
            | P::PositionEndRequest { .. }
            | P::ReportingLineSetRequest { .. }
            | P::ExternalPersonCreateRequest { .. }
            | P::AssignRequest { .. }
            | P::AssignmentUpdateRequest { .. }
            | P::AssignmentEndRequest { .. }
    )
}

/// Every field of the request that holds the id of a unit type, unit,
/// position, assignment or external person — the fields a temporary id can
/// stand in. Role ids are not here: no operation of a batch makes a role.
fn id_slots(request: &mut P) -> Vec<(IdKind, &mut String)> {
    use IdKind::*;
    fn opt(kind: IdKind, field: &mut Option<String>) -> Vec<(IdKind, &mut String)> {
        field.iter_mut().map(|id| (kind, id)).collect()
    }
    match request {
        P::UnitTypeUpdateRequest { id, .. } | P::UnitTypeDeleteRequest { id } => {
            vec![(UnitType, id)]
        }
        P::UnitCreateRequest {
            type_id,
            parent_unit_id,
            ..
        } => {
            let mut slots = opt(UnitType, type_id);
            slots.extend(opt(Unit, parent_unit_id));
            slots
        }
        P::UnitUpdateRequest {
            unit_id, type_id, ..
        } => {
            let mut slots = vec![(Unit, unit_id)];
            slots.extend(opt(UnitType, type_id));
            slots
        }
        P::UnitMoveRequest {
            unit_id,
            new_parent_unit_id,
            ..
        } => {
            let mut slots = vec![(Unit, unit_id)];
            slots.extend(opt(Unit, new_parent_unit_id));
            slots
        }
        P::UnitEndRequest { unit_id, .. } => vec![(Unit, unit_id)],
        P::HeadSetRequest {
            unit_id,
            head_position_id,
            ..
        } => {
            let mut slots = vec![(Unit, unit_id)];
            slots.extend(opt(Position, head_position_id));
            slots
        }
        P::DeputyHeadsSetRequest {
            unit_id,
            position_ids,
            ..
        } => {
            let mut slots = vec![(Unit, unit_id)];
            slots.extend(position_ids.iter_mut().map(|id| (Position, id)));
            slots
        }
        P::PositionCreateRequest {
            unit_id,
            parent_position_id,
            ..
        } => {
            let mut slots = vec![(Unit, unit_id)];
            slots.extend(opt(Position, parent_position_id));
            slots
        }
        P::PositionUpdateRequest { position_id, .. }
        | P::PositionEndRequest { position_id, .. } => {
            vec![(Position, position_id)]
        }
        P::PositionMoveRequest {
            position_id,
            new_parent_position_id,
            ..
        } => {
            let mut slots = vec![(Position, position_id)];
            slots.extend(opt(Position, new_parent_position_id));
            slots
        }
        P::ReportingLineSetRequest {
            position_id,
            parent_position_id,
            ..
        } => vec![(Position, position_id), (Position, parent_position_id)],
        P::AssignRequest {
            position_id,
            subject,
            ..
        } => {
            let mut slots = vec![(Position, position_id)];
            if let wire::OrgSubject::External(id) = subject {
                slots.push((ExternalPerson, id));
            }
            slots
        }
        P::AssignmentUpdateRequest { assignment_id, .. }
        | P::AssignmentEndRequest { assignment_id, .. } => vec![(Assignment, assignment_id)],
        _ => Vec::new(),
    }
}

const TEMP_PREFIX: &str = "tmp:";
const MAX_TEMP_ID_CHARS: usize = 64;

fn refusal(code: &str, message: String, id: Option<String>) -> wire::OrgOpError {
    wire::OrgOpError {
        code: code.to_string(),
        message,
        id,
        ..Default::default()
    }
}

/// The temporary ids of a batch. `declared` includes the ones whose operation
/// failed: naming one of those is a different mistake from a typo.
#[derive(Default)]
struct TempIds {
    made: HashMap<String, (IdKind, String)>,
    declared: BTreeMap<String, Option<IdKind>>,
}

impl TempIds {
    fn declare(&mut self, request: &P, temp_id: &str) -> Result<(), wire::OrgOpError> {
        if !temp_id.starts_with(TEMP_PREFIX)
            || temp_id.len() == TEMP_PREFIX.len()
            || temp_id.chars().count() > MAX_TEMP_ID_CHARS
        {
            return Err(refusal(
                "invalid_temp_id",
                format!("a temporary id is '{TEMP_PREFIX}' and a name of up to {MAX_TEMP_ID_CHARS} characters"),
                Some(temp_id.to_string()),
            ));
        }
        let Some(kind) = creates(request) else {
            return Err(refusal(
                "temp_id_not_allowed",
                "only an operation that makes a unit type, unit, position, assignment or external person takes a temporary id".to_string(),
                Some(temp_id.to_string()),
            ));
        };
        if self.declared.contains_key(temp_id) {
            return Err(refusal(
                "duplicate_temp_id",
                format!("{temp_id} is already the id of an earlier operation"),
                Some(temp_id.to_string()),
            ));
        }
        self.declared.insert(temp_id.to_string(), Some(kind));
        Ok(())
    }

    /// Puts the real id of everything the request names by a temporary one.
    fn resolve(&self, request: &mut P) -> Result<(), wire::OrgOpError> {
        for (expected, id) in id_slots(request) {
            if !id.starts_with(TEMP_PREFIX) {
                continue;
            }
            match self.made.get(id.as_str()) {
                Some((kind, real)) if *kind == expected => *id = real.clone(),
                Some(_) => {
                    return Err(refusal(
                        "temp_id_wrong_kind",
                        format!("{id} stands for another kind of thing than the field takes"),
                        Some(id.clone()),
                    ))
                }
                None if self.declared.contains_key(id.as_str()) => {
                    return Err(refusal(
                        "temp_id_not_made",
                        format!("{id} belongs to an operation that failed"),
                        Some(id.clone()),
                    ))
                }
                None => {
                    return Err(refusal(
                        "unknown_temp_id",
                        format!("{id} is not the id of an earlier operation"),
                        Some(id.clone()),
                    ))
                }
            }
        }
        Ok(())
    }

    fn made(&mut self, temp_id: &str, kind: IdKind, real: &str) {
        self.made
            .insert(temp_id.to_string(), (kind, real.to_string()));
    }
}

/// Everything a batch touched, by kind, for the one event.
#[derive(Default)]
struct Affected(BTreeMap<IdKind, BTreeSet<String>>);

impl Affected {
    fn add(&mut self, kind: IdKind, id: &str) {
        self.0.entry(kind).or_default().insert(id.to_string());
    }

    fn json(&self) -> serde_json::Value {
        let of = |kind: IdKind| -> Vec<&String> {
            self.0
                .get(&kind)
                .map(|s| s.iter().collect())
                .unwrap_or_default()
        };
        json!({
            "unit_type_ids": of(IdKind::UnitType),
            "unit_ids": of(IdKind::Unit),
            "position_ids": of(IdKind::Position),
            "assignment_ids": of(IdKind::Assignment),
            "external_person_ids": of(IdKind::ExternalPerson),
        })
    }
}

/// How one operation ended, before it is put on the wire.
enum Outcome {
    Kept(OpValue),
    Refused(wire::OrgOpError),
}

/// What the body of the transaction hands back.
struct Ran {
    results: Vec<wire::OrgBatchOpResult>,
    warnings: Vec<wire::OrgWarning>,
    preview_at: NaiveDate,
    preview: Option<wire::OrgStructureView>,
    counts: BTreeMap<&'static str, u32>,
    affected: Affected,
}

fn same_warning(a: &Warning, b: &Warning) -> bool {
    match (a, b) {
        (
            Warning::UnitWithoutHead { unit_id: a, .. },
            Warning::UnitWithoutHead { unit_id: b, .. },
        ) => a == b,
        (
            Warning::ShareOverbooked { subject: a, .. },
            Warning::ShareOverbooked { subject: b, .. },
        )
        | (
            Warning::PersonWithoutPrimary { subject: a, .. },
            Warning::PersonWithoutPrimary { subject: b, .. },
        ) => a == b,
        _ => false,
    }
}

/// The warnings a later operation did not resolve, once each.
fn warnings_that_hold(raised: Vec<Warning>, view: &svc::query::StructureView) -> Vec<Warning> {
    let mut kept: Vec<Warning> = Vec::new();
    for warning in raised {
        let holds = view.warnings.iter().any(|w| same_warning(&warning, w));
        if holds && !kept.iter().any(|k| same_warning(k, &warning)) {
            kept.push(warning);
        }
    }
    kept
}

/// Runs on the batch's transaction: before the first operation (`before_ops`) or once every operation
/// succeeded, and only then (`after_kept`);
/// what it writes commits or rolls back with the operations. A planned
/// reorganization records its approval this way (change_set.rs).
pub(super) type AfterKept = Box<dyn FnOnce(&mut run::Batch<'_, '_>) -> Result<(), E> + Send>;

pub(super) async fn dispatch(
    ctx: &HandlerContext,
    ops: &[wire::OrgWriteOp],
    dry_run: bool,
    confirm_backdated: bool,
) -> Result<MessageBody, ProtocolError> {
    dispatch_with(ctx, ops, dry_run, confirm_backdated, None, None).await
}

pub(super) async fn dispatch_with(
    ctx: &HandlerContext,
    ops: &[wire::OrgWriteOp],
    dry_run: bool,
    confirm_backdated: bool,
    before_ops: Option<AfterKept>,
    after_kept: Option<AfterKept>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let respond = |response: P| Ok(MessageBody::OrgStructureBody(response));
    if ops.len() > run::MAX_OPS {
        return respond(P::BatchResponse {
            ok: false,
            applied: false,
            error: Some(refusal(
                "too_many_ops",
                format!("a batch takes at most {} operations", run::MAX_OPS),
                None,
            )),
            results: Vec::new(),
            warnings: Vec::new(),
            preview_at: None,
            preview: None,
            max_ops: run::MAX_OPS as u32,
        });
    }

    let pool = ctx.state.db.clone();
    let (org_id, user_id) = (org.org_id.clone(), org.user_id.clone());
    let ops = ops.to_vec();
    let today = svc::org_today(&pool, &org_id).map_err(super::read_error)?;
    let ran = tokio::task::spawn_blocking(move || {
        let write_ctx = WriteCtx {
            org_id: &org_id,
            actor_user_id: &user_id,
            confirm_backdated: false,
        };
        run::run(&pool, &write_ctx, !dry_run, |batch| {
            if let Some(before) = before_ops {
                before(batch)?;
            }
            let mut temp = TempIds::default();
            let mut results = Vec::with_capacity(ops.len());
            let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
            let mut affected = Affected::default();
            let mut preview_at: Option<NaiveDate> = None;
            for (index, op) in ops.iter().enumerate() {
                let mut request = op.request.clone();
                let outcome = (|| {
                    if !is_batch_op(&request) {
                        return Ok(Outcome::Refused(refusal(
                            "not_a_batch_op",
                            "a batch takes writes of units, positions, lines and assignments"
                                .to_string(),
                            None,
                        )));
                    }
                    if let Some(temp_id) = &op.temp_id {
                        if let Err(refused) = temp.declare(&request, temp_id) {
                            return Ok(Outcome::Refused(refused));
                        }
                    }
                    if let Err(refused) = temp.resolve(&mut request) {
                        return Ok(Outcome::Refused(refused));
                    }
                    let (svc_op, op_confirm) = match to_op(&request) {
                        Ok(pair) => pair,
                        Err(E::Db(detail)) => return Err(E::Db(detail)),
                        Err(refused) => return Ok(Outcome::Refused(op_error(&refused))),
                    };
                    match batch.apply(&svc_op, op_confirm || confirm_backdated) {
                        Ok(value) => {
                            if let Some(day) = svc_op.effective_from() {
                                preview_at = Some(preview_at.map_or(day, |d| d.max(day)));
                            }
                            *counts.entry(svc_op.action()).or_default() += 1;
                            Ok(Outcome::Kept(value))
                        }
                        Err(E::Db(detail)) => Err(E::Db(detail)),
                        Err(refused) => Ok(Outcome::Refused(op_error(&refused))),
                    }
                })()?;
                results.push(match outcome {
                    Outcome::Kept(value) => {
                        let created_id = creates(&request)
                            .and_then(|_| value.entity_id())
                            .map(str::to_string);
                        if let (Some(temp_id), Some(real), Some(kind)) =
                            (&op.temp_id, &created_id, creates(&request))
                        {
                            temp.made(temp_id, kind, real);
                        }
                        for (kind, id) in id_slots(&mut request) {
                            affected.add(kind, id);
                        }
                        if let (Some(real), Some(kind)) = (&created_id, creates(&request)) {
                            affected.add(kind, real);
                        }
                        wire::OrgBatchOpResult {
                            index: index as u32,
                            ok: true,
                            error: None,
                            result: Some(to_result(value)),
                            temp_id: op.temp_id.clone(),
                            created_id,
                        }
                    }
                    Outcome::Refused(error) => wire::OrgBatchOpResult {
                        index: index as u32,
                        ok: false,
                        error: Some(error),
                        result: None,
                        temp_id: op.temp_id.clone(),
                        created_id: None,
                    },
                });
            }
            let failed = results.iter().filter(|r| !r.ok).count();
            let from_change_set = after_kept.is_some();
            if failed == 0 {
                if let Some(after) = after_kept {
                    after(batch)?;
                }
            }
            let preview_at = preview_at.unwrap_or(today);
            let view = batch.preview(Some(preview_at))?;
            let warnings = warnings_that_hold(batch.warnings(), &view)
                .into_iter()
                .map(Into::into)
                .collect();
            let summary = json!({
                "ops": ops.len(),
                "failed": failed,
                "counts": counts,
                "preview_at": fmt(preview_at),
                "change_set": from_change_set,
            });
            Ok(run::Decision {
                value: Ran {
                    results,
                    warnings,
                    preview_at,
                    preview: dry_run.then(|| view.into()),
                    counts,
                    affected,
                },
                keep: failed == 0,
                summary,
            })
        })
    })
    .await
    .map_err(|e| db_error(format!("batch task: {e}")))?
    .map_err(|e| match e {
        E::Db(detail) => db_error(detail),
        other => ProtocolError::bad_request(other.to_string()),
    })?;

    let kept = ran.results.iter().all(|r| r.ok);
    let applied = kept && !dry_run;
    if applied {
        publish(
            ctx,
            org,
            "org.structure_batch_applied",
            json!({
                "ops": ran.results.len(),
                "counts": ran.counts,
                "preview_at": fmt(ran.preview_at),
                "affected": ran.affected.json(),
            }),
        );
    }
    respond(P::BatchResponse {
        ok: kept,
        applied,
        error: None,
        results: ran.results,
        warnings: ran.warnings,
        preview_at: Some(fmt(ran.preview_at)),
        preview: ran.preview,
        max_ops: run::MAX_OPS as u32,
    })
}
