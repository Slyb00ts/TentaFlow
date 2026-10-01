//! Runs the planned operations through the repository functions a manual edit
//! uses, inside the batch transaction. An operation the repository refuses is
//! rolled back on its own and reported on its row; operations that depend on
//! something that was not made are skipped without a report of their own.

use std::collections::HashMap;

use super::plan::{Op, OpKind, PositionRef, UnitRef};
use super::report::{Issue, IssueKind};
use crate::services::org_structure::error::{OrgStructureError as E, Result};
use crate::services::org_structure::repo::{self, Session};
use crate::services::org_structure::types::*;

pub struct Executed {
    /// Ids of the units and positions the run made, by the code key of the file.
    pub unit_ids: HashMap<String, String>,
    pub position_ids: HashMap<String, String>,
    pub failures: Vec<Issue>,
    /// Per operation of the plan: it ran and stayed.
    pub done: Vec<bool>,
}

struct Ids {
    units: HashMap<String, String>,
    positions: HashMap<String, String>,
}

impl Ids {
    fn unit(&self, reference: &UnitRef) -> Option<String> {
        match reference {
            UnitRef::Existing(id) => Some(id.clone()),
            UnitRef::New(key) => self.units.get(key).cloned(),
        }
    }

    fn position(&self, reference: &PositionRef) -> Option<String> {
        match reference {
            PositionRef::Existing(id) => Some(id.clone()),
            PositionRef::New(key) => self.positions.get(key).cloned(),
        }
    }

    /// `Some(None)` is "no parent", `None` is "the parent was not made".
    fn optional_unit(&self, reference: &Option<UnitRef>) -> Option<Option<String>> {
        match reference {
            None => Some(None),
            Some(reference) => self.unit(reference).map(Some),
        }
    }

    fn optional_position(&self, reference: &Option<PositionRef>) -> Option<Option<String>> {
        match reference {
            None => Some(None),
            Some(reference) => self.position(reference).map(Some),
        }
    }
}

pub fn execute(s: &mut Session<'_>, ops: &[Op]) -> Result<Executed> {
    let mut ids = Ids {
        units: HashMap::new(),
        positions: HashMap::new(),
    };
    let mut failures = Vec::new();
    let mut done = Vec::with_capacity(ops.len());
    for op in ops {
        let outcome = run_op(s, &mut ids, &op.kind);
        match outcome {
            Ok(ran) => done.push(ran),
            Err(E::Db(detail)) => return Err(E::Db(detail)),
            Err(refused) => {
                done.push(false);
                failures.push(
                    Issue::new(op.row, IssueKind::Rejected, refused.to_string())
                        .code(refused.code()),
                );
            }
        }
    }
    Ok(Executed {
        unit_ids: ids.units,
        position_ids: ids.positions,
        failures,
        done,
    })
}

/// `Ok(false)`: skipped because something it needs was not made.
fn run_op(s: &mut Session<'_>, ids: &mut Ids, kind: &OpKind) -> Result<bool> {
    match kind {
        OpKind::CreateUnit {
            key,
            name,
            code,
            type_id,
            parent,
            from,
        } => {
            let Some(parent_unit_id) = ids.optional_unit(parent) else {
                return Ok(false);
            };
            let new = NewUnit {
                name: name.clone(),
                code: Some(code.clone()),
                type_id: type_id.clone(),
                parent_unit_id,
                color: None,
                valid_from: *from,
                valid_to: None,
            };
            let unit = s.attempt("org.unit.create", |s| repo::create_unit_in(s, &new))?;
            ids.units.insert(key.clone(), unit.unit_id);
        }
        OpKind::UpdateUnit {
            unit_id,
            name,
            type_id,
            from,
        } => {
            let patch = UnitPatch {
                name: name.clone(),
                code: None,
                type_id: type_id.clone(),
                color: None,
            };
            s.attempt("org.unit.update", |s| {
                repo::update_unit_in(s, unit_id, &patch, *from)
            })?;
        }
        OpKind::MoveUnit {
            unit_id,
            parent,
            from,
        } => {
            let Some(parent) = ids.optional_unit(parent) else {
                return Ok(false);
            };
            s.attempt("org.unit.move", |s| {
                repo::move_unit_in(s, unit_id, parent.as_deref(), *from)
            })?;
        }
        OpKind::CreatePosition {
            key,
            unit,
            name,
            code,
            staff,
            manager,
            from,
        } => {
            let (Some(unit_id), Some(parent_position_id)) =
                (ids.unit(unit), ids.optional_position(manager))
            else {
                return Ok(false);
            };
            let new = NewPosition {
                unit_id,
                name: name.clone(),
                code: Some(code.clone()),
                role_id: None,
                is_manager: None,
                is_staff: *staff,
                parent_position_id,
                valid_from: *from,
                valid_to: None,
            };
            let position =
                s.attempt("org.position.create", |s| repo::create_position_in(s, &new))?;
            ids.positions.insert(key.clone(), position.position_id);
        }
        OpKind::UpdatePosition {
            position_id,
            name,
            staff,
            from,
        } => {
            let patch = PositionPatch {
                name: name.clone(),
                is_staff: *staff,
                ..PositionPatch::default()
            };
            s.attempt("org.position.update", |s| {
                repo::update_position_in(s, position_id, &patch, *from)
            })?;
        }
        OpKind::MovePosition {
            position_id,
            manager,
            from,
        } => {
            let Some(manager) = ids.optional_position(manager) else {
                return Ok(false);
            };
            s.attempt("org.position.move", |s| {
                repo::move_position_in(s, position_id, manager.as_deref(), *from)
            })?;
        }
        OpKind::SetHead {
            unit,
            position,
            from,
        } => {
            let (Some(unit_id), Some(position_id)) =
                (ids.unit(unit), ids.optional_position(position))
            else {
                return Ok(false);
            };
            s.attempt("org.unit.head_set", |s| {
                repo::set_head_in(s, &unit_id, position_id.as_deref(), *from)
            })?;
        }
        OpKind::SetDeputies {
            unit,
            positions,
            from,
        } => {
            let Some(unit_id) = ids.unit(unit) else {
                return Ok(false);
            };
            let position_ids: Option<Vec<String>> =
                positions.iter().map(|p| ids.position(p)).collect();
            let Some(position_ids) = position_ids else {
                return Ok(false);
            };
            s.attempt("org.unit.deputies_set", |s| {
                repo::set_deputy_heads_in(s, &unit_id, &position_ids, *from)
            })?;
        }
        OpKind::DemotePrimary {
            assignment_id,
            from,
        } => {
            let patch = AssignmentPatch {
                is_primary: Some(false),
                ..AssignmentPatch::default()
            };
            s.attempt("org.assignment.update", |s| {
                repo::update_assignment_in(s, assignment_id, &patch, *from)
            })?;
        }
        OpKind::Assign {
            position,
            subject,
            share,
            primary,
            from,
        } => {
            let Some(position_id) = ids.position(position) else {
                return Ok(false);
            };
            let new = NewAssignment {
                position_id,
                subject: subject.clone(),
                kind: AssignmentType::Permanent,
                share: *share,
                is_primary: *primary,
                valid_from: *from,
                valid_to: None,
            };
            s.attempt("org.assignment.create", |s| repo::assign_in(s, &new))?;
        }
        OpKind::UpdateAssignment {
            assignment_id,
            share,
            primary,
            from,
        } => {
            let patch = AssignmentPatch {
                kind: None,
                share: *share,
                is_primary: *primary,
            };
            s.attempt("org.assignment.update", |s| {
                repo::update_assignment_in(s, assignment_id, &patch, *from)
            })?;
        }
        OpKind::EndAssignment {
            assignment_id,
            from,
        } => {
            s.attempt("org.assignment.end", |s| {
                repo::end_assignment_in(s, assignment_id, *from)
            })?;
        }
        OpKind::EndPosition { position_id, from } => {
            s.attempt("org.position.end", |s| {
                repo::end_position_in(s, position_id, *from)
            })?;
        }
        OpKind::EndUnit { unit_id, from } => {
            s.attempt("org.unit.end", |s| repo::end_unit_in(s, unit_id, *from))?;
        }
    }
    Ok(true)
}
