//! One edit of the structure as data (`Op`), and a run of many of them as ONE
//! transaction (`run`): the editor collects a draft and saves it in one call.
//!
//! A single write and a batch execute an `Op` the same way (`Op::run_in`), so a
//! rule of the repository cannot hold for one and not for the other. Inside a
//! batch every operation has its own savepoint: one the repository refuses is
//! reported and undone on its own, the next one still runs, and the batch is
//! kept only when the caller says so (no operation failed, not a dry run).
//! The finishing work — sync captures, ONE projection recompute, ONE audit entry
//! that lists the operations — runs for a dry run too, inside the transaction
//! that is then dropped, so a dry run fails where the save would.

use chrono::NaiveDate;
use serde_json::Value as Json;

use super::error::Result;
use super::query::{self, StructureView};
use super::repo::{self, Audited, BatchMode, BatchOutcome, Session};
use super::types::*;
use crate::db::DbPool;

/// The most operations one batch takes: a draft this large is a re-organization
/// that belongs in an import file, and the request is one frame of the socket.
pub const MAX_OPS: usize = 500;

/// One edit. The dates and ids are already parsed and resolved.
#[derive(Debug, Clone)]
pub enum Op {
    UnitTypeCreate {
        name: String,
        color: Option<String>,
        icon: Option<String>,
    },
    UnitTypeUpdate {
        id: String,
        patch: UnitTypePatch,
    },
    UnitTypeDelete {
        id: String,
    },
    UnitCreate(NewUnit),
    UnitUpdate {
        unit_id: String,
        patch: UnitPatch,
        from: NaiveDate,
    },
    UnitMove {
        unit_id: String,
        new_parent_unit_id: Option<String>,
        from: NaiveDate,
    },
    UnitEnd {
        unit_id: String,
        from: NaiveDate,
    },
    HeadSet {
        unit_id: String,
        head_position_id: Option<String>,
        from: NaiveDate,
    },
    DeputyHeadsSet {
        unit_id: String,
        position_ids: Vec<String>,
        from: NaiveDate,
    },
    PositionCreate(NewPosition),
    PositionUpdate {
        position_id: String,
        patch: PositionPatch,
        from: NaiveDate,
    },
    PositionMove {
        position_id: String,
        new_parent_position_id: Option<String>,
        from: NaiveDate,
    },
    PositionEnd {
        position_id: String,
        from: NaiveDate,
    },
    ReportingLineSet(NewLine),
    ExternalPersonCreate(NewExternalPerson),
    Assign(NewAssignment),
    AssignmentUpdate {
        assignment_id: String,
        patch: AssignmentPatch,
        from: NaiveDate,
    },
    AssignmentEnd {
        assignment_id: String,
        from: NaiveDate,
    },
}

/// What an operation produced.
#[derive(Debug, Clone)]
pub enum OpValue {
    UnitType(UnitType),
    Unit(Unit),
    DeputyHeads(Vec<DeputyHead>),
    Position(Position),
    ReportingLine(Option<ReportingLine>),
    Ended(Ended),
    ExternalPerson(ExternalPerson),
    Assignment(Assignment),
    Done,
}

impl OpValue {
    /// The id of the thing the value is about; what a batch's later operations
    /// name a thing an operation made by. `None` for operations that return
    /// no entity.
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::UnitType(t) => Some(&t.id),
            Self::Unit(u) => Some(&u.unit_id),
            Self::Position(p) => Some(&p.position_id),
            Self::ExternalPerson(p) => Some(&p.id),
            Self::Assignment(a) => Some(&a.id),
            _ => None,
        }
    }
}

fn wrap<T>(audited: Audited<T>, to_value: impl FnOnce(T) -> OpValue) -> Audited<OpValue> {
    Audited {
        value: to_value(audited.value),
        target: audited.target,
        summary: audited.summary,
    }
}

impl Op {
    /// The audit action of the operation.
    pub fn action(&self) -> &'static str {
        match self {
            Self::UnitTypeCreate { .. } => "org.unit_type.create",
            Self::UnitTypeUpdate { .. } => "org.unit_type.update",
            Self::UnitTypeDelete { .. } => "org.unit_type.delete",
            Self::UnitCreate(_) => "org.unit.create",
            Self::UnitUpdate { .. } => "org.unit.update",
            Self::UnitMove { .. } => "org.unit.move",
            Self::UnitEnd { .. } => "org.unit.end",
            Self::HeadSet { .. } => "org.unit.head_set",
            Self::DeputyHeadsSet { .. } => "org.unit.deputies_set",
            Self::PositionCreate(_) => "org.position.create",
            Self::PositionUpdate { .. } => "org.position.update",
            Self::PositionMove { .. } => "org.position.move",
            Self::PositionEnd { .. } => "org.position.end",
            Self::ReportingLineSet(_) => "org.reporting_line.set",
            Self::ExternalPersonCreate(_) => "org.external_person.create",
            Self::Assign(_) => "org.assignment.create",
            Self::AssignmentUpdate { .. } => "org.assignment.update",
            Self::AssignmentEnd { .. } => "org.assignment.end",
        }
    }

    /// The day the edit takes effect; unit types and external persons have none.
    pub fn effective_from(&self) -> Option<NaiveDate> {
        match self {
            Self::UnitTypeCreate { .. }
            | Self::UnitTypeUpdate { .. }
            | Self::UnitTypeDelete { .. }
            | Self::ExternalPersonCreate(_) => None,
            Self::UnitCreate(n) => Some(n.valid_from),
            Self::PositionCreate(n) => Some(n.valid_from),
            Self::ReportingLineSet(n) => Some(n.valid_from),
            Self::Assign(n) => Some(n.valid_from),
            Self::UnitUpdate { from, .. }
            | Self::UnitMove { from, .. }
            | Self::UnitEnd { from, .. }
            | Self::HeadSet { from, .. }
            | Self::DeputyHeadsSet { from, .. }
            | Self::PositionUpdate { from, .. }
            | Self::PositionMove { from, .. }
            | Self::PositionEnd { from, .. }
            | Self::AssignmentUpdate { from, .. }
            | Self::AssignmentEnd { from, .. } => Some(*from),
        }
    }

    pub(super) fn run_in(&self, s: &mut Session<'_>) -> Result<Audited<OpValue>> {
        Ok(match self {
            Self::UnitTypeCreate { name, color, icon } => wrap(
                repo::create_unit_type_in(s, name, color.as_deref(), icon.as_deref())?,
                OpValue::UnitType,
            ),
            Self::UnitTypeUpdate { id, patch } => {
                wrap(repo::update_unit_type_in(s, id, patch)?, OpValue::UnitType)
            }
            Self::UnitTypeDelete { id } => {
                wrap(repo::delete_unit_type_in(s, id)?, |()| OpValue::Done)
            }
            Self::UnitCreate(new) => wrap(repo::create_unit_in(s, new)?, OpValue::Unit),
            Self::UnitUpdate {
                unit_id,
                patch,
                from,
            } => wrap(
                repo::update_unit_in(s, unit_id, patch, *from)?,
                OpValue::Unit,
            ),
            Self::UnitMove {
                unit_id,
                new_parent_unit_id,
                from,
            } => wrap(
                repo::move_unit_in(s, unit_id, new_parent_unit_id.as_deref(), *from)?,
                OpValue::Unit,
            ),
            Self::UnitEnd { unit_id, from } => {
                wrap(repo::end_unit_in(s, unit_id, *from)?, |()| OpValue::Done)
            }
            Self::HeadSet {
                unit_id,
                head_position_id,
                from,
            } => wrap(
                repo::set_head_in(s, unit_id, head_position_id.as_deref(), *from)?,
                OpValue::Unit,
            ),
            Self::DeputyHeadsSet {
                unit_id,
                position_ids,
                from,
            } => wrap(
                repo::set_deputy_heads_in(s, unit_id, position_ids, *from)?,
                OpValue::DeputyHeads,
            ),
            Self::PositionCreate(new) => wrap(repo::create_position_in(s, new)?, OpValue::Position),
            Self::PositionUpdate {
                position_id,
                patch,
                from,
            } => wrap(
                repo::update_position_in(s, position_id, patch, *from)?,
                OpValue::Position,
            ),
            Self::PositionMove {
                position_id,
                new_parent_position_id,
                from,
            } => wrap(
                repo::move_position_in(s, position_id, new_parent_position_id.as_deref(), *from)?,
                OpValue::ReportingLine,
            ),
            Self::PositionEnd { position_id, from } => wrap(
                repo::end_position_in(s, position_id, *from)?,
                OpValue::Ended,
            ),
            Self::ReportingLineSet(new) => wrap(repo::set_reporting_line_in(s, new)?, |l| {
                OpValue::ReportingLine(Some(l))
            }),
            Self::ExternalPersonCreate(new) => wrap(
                repo::create_external_person_in(s, new)?,
                OpValue::ExternalPerson,
            ),
            Self::Assign(new) => wrap(repo::assign_in(s, new)?, OpValue::Assignment),
            Self::AssignmentUpdate {
                assignment_id,
                patch,
                from,
            } => wrap(
                repo::update_assignment_in(s, assignment_id, patch, *from)?,
                OpValue::Assignment,
            ),
            Self::AssignmentEnd {
                assignment_id,
                from,
            } => wrap(repo::end_assignment_in(s, assignment_id, *from)?, |()| {
                OpValue::Done
            }),
        })
    }
}

/// One edit as its own transaction: what a single write request does.
pub fn run_single(pool: &DbPool, ctx: &WriteCtx<'_>, op: &Op) -> Result<Written<OpValue>> {
    repo::run(pool, ctx, op.action(), |s| op.run_in(s))
}

/// The session of a batch, as the caller sees it.
pub struct Batch<'s, 'a> {
    session: &'s mut Session<'a>,
}

impl Batch<'_, '_> {
    /// Runs `op` on its own savepoint. A refusal by the repository is `Err`
    /// (the operation left no trace, the batch goes on); only a storage failure
    /// should abort the batch, and the caller sees it as `OrgStructureError::Db`.
    pub fn apply(&mut self, op: &Op, confirm_backdated: bool) -> Result<OpValue> {
        self.session.set_confirm_backdated(confirm_backdated);
        self.session.attempt(op.action(), |s| op.run_in(s))
    }

    /// Runs `f` on the batch's own transaction, with the organization id. For a
    /// write that has to commit or roll back WITH the operations: approving a
    /// planned reorganization records its state this way (change_set.rs), so a
    /// batch that fails leaves the reorganization pending.
    pub fn with_transaction<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Transaction<'_>, &str) -> Result<T>,
    ) -> Result<T> {
        f(self.session.tx, self.session.org_id)
    }

    /// The structure the batch so far leaves, on `at`.
    pub fn preview(&self, at: Option<NaiveDate>) -> Result<StructureView> {
        query::structure_in(self.session.tx, self.session.org_id, at)
    }

    /// The warnings the operations raised while they ran. A later operation
    /// can resolve one (a head set after the unit was made); `preview` shows
    /// which still hold.
    pub fn warnings(&self) -> Vec<Warning> {
        self.session.warnings().to_vec()
    }
}

/// What the body of a batch decides.
pub struct Decision<T> {
    pub value: T,
    /// Keep the changes (when the run is not a dry run).
    pub keep: bool,
    /// The batch's own entry in the audit trail; the operations are added to it.
    pub summary: Json,
}

/// Runs `body` in ONE transaction, then the finishing work, and commits when
/// `commit` and the body asks for it. The caller has not confirmed anything by
/// default: each operation is refused when it rewrites history unless the
/// `confirm_backdated` given to `apply` says otherwise.
pub fn run<T>(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    commit: bool,
    body: impl FnOnce(&mut Batch<'_, '_>) -> Result<Decision<T>>,
) -> Result<T> {
    repo::run_batch(
        pool,
        ctx,
        "batch",
        commit,
        BatchMode {
            backdating_checked: false,
            allow_withdraw: false,
        },
        |session| {
            let decision = body(&mut Batch {
                session: &mut *session,
            })?;
            session.record_summary(
                "org.structure.batch",
                format!("org:{}", ctx.org_id),
                decision.summary,
            );
            Ok(BatchOutcome {
                value: decision.value,
                keep: decision.keep,
            })
        },
    )
}
