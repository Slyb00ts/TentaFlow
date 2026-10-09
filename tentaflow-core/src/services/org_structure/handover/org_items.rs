//! What a person holds in the structure itself: the positions they occupy and
//! the deputies they are part of. Both are changed through the same write
//! session as every other edit of the structure (`repo::run_batch`), all of a
//! handover's org items in ONE transaction: either the structure takes all of
//! them or none, and the record of the handover is updated in that same
//! transaction.
//!
//! A position handed to somebody is ended for the person on the day and given
//! to the taker from that day as an ACTING assignment ("p.o."): the taker
//! stands in until the administrator makes it permanent or fills the seat. A
//! position handed to nobody becomes a vacancy, which the tree shows.

use std::collections::HashSet;

use crate::services::org_structure::availability::{deputies_where, Deputy};
use crate::services::org_structure::error::{OrgStructureError as E, Result};
use crate::services::org_structure::query::Snapshot;
use crate::services::org_structure::replication as repl;
use crate::services::org_structure::repo::{self, cover, NewDeputy, Session};
use crate::services::org_structure::validate::format_date;
use crate::services::org_structure::{AssignmentType, NewAssignment, Subject};

use super::{
    Action, ApplyCx, Category, HandoverProvider, Held, ListCx, OrgProvider, Planned, Reason,
    Suggestion,
};

// =============================================================================
// Positions
// =============================================================================

pub(super) struct PositionProvider;

impl HandoverProvider for PositionProvider {
    fn category(&self) -> Category {
        Category::Position
    }

    fn list(&self, cx: &ListCx<'_>) -> Result<Vec<Held>> {
        if cx.reason != Reason::Departure {
            return Ok(Vec::new());
        }
        let snap = Snapshot::load(cx.conn, cx.org_id, cx.date)?;
        let deputy_head_positions: HashSet<&str> = snap
            .deputy_heads
            .iter()
            .map(|d| d.position_id.as_str())
            .collect();
        let mut out = Vec::new();
        for assignment in snap.assignments_of(cx.user_id) {
            let Some(position) = snap.position(&assignment.position_id) else {
                continue;
            };
            let role = if snap.is_head(position) {
                "head"
            } else if deputy_head_positions.contains(position.position_id.as_str()) {
                "deputy_head"
            } else {
                "member"
            };
            let unit_name = snap.unit(&position.unit_id).map(|u| u.name.clone());
            out.push(Held {
                key: format!("position:{}", assignment.id),
                category: Category::Position,
                title: position.name.clone(),
                role: role.into(),
                state: String::new(),
                project_id: None,
                project_name: None,
                unit_name,
                valid_to: assignment.valid_to.map(format_date),
                action: Action::TransferOrEnd,
                suggestion: cx
                    .advice
                    .for_position(&position.position_id, &position.unit_id),
                eligible: Some(
                    cx.members
                        .iter()
                        .filter(|id| id.as_str() != cx.user_id)
                        .cloned()
                        .collect(),
                ),
                blocked: None,
            });
        }
        Ok(out)
    }
}

impl OrgProvider for PositionProvider {
    fn apply_in(&self, s: &mut Session<'_>, cx: &ApplyCx<'_>, item: &Planned) -> Result<()> {
        let id = item
            .key
            .strip_prefix("position:")
            .ok_or_else(|| E::InvalidValue {
                field: "key",
                reason: "not a position key".into(),
            })?
            .to_string();
        let held = repo::select_where(
            s.tx,
            &repl::ASSIGNMENTS,
            "org_id = ?1 AND id = ?2",
            [s.org_id, id.as_str()],
            repo::map_assignment,
        )?;
        let assignment = held.into_iter().next().ok_or_else(|| E::NotFound {
            entity: "assignment",
            id: id.clone(),
        })?;
        if assignment.subject != Subject::User(cx.from_user.to_string()) {
            return Err(E::NotFound {
                entity: "assignment",
                id,
            });
        }
        s.attempt("org.assignment.end", |s| {
            repo::end_assignment_in(s, &assignment.id, cx.date)
        })?;
        if let Some(taker) = &item.taker {
            let new = NewAssignment {
                position_id: assignment.position_id.clone(),
                subject: Subject::User(taker.clone()),
                kind: AssignmentType::Acting,
                share: assignment.share,
                is_primary: None,
                valid_from: cx.date,
                valid_to: assignment.valid_to,
            };
            s.attempt("org.assignment.create", |s| repo::assign_in(s, &new))?;
        }
        Ok(())
    }
}

// =============================================================================
// Deputies
// =============================================================================

pub(super) struct DeputyProvider;

fn deputies_of(
    conn: &rusqlite::Connection,
    org_id: &str,
    user: &str,
    date: chrono::NaiveDate,
) -> Result<Vec<Deputy>> {
    deputies_where(
        conn,
        "org_id = ?1 AND (user_id = ?2 OR deputy_user_id = ?2) \
         AND (valid_to IS NULL OR valid_to > ?3) ORDER BY valid_from, id",
        [org_id, user, format_date(date).as_str()],
    )
}

impl HandoverProvider for DeputyProvider {
    fn category(&self) -> Category {
        Category::Deputy
    }

    fn list(&self, cx: &ListCx<'_>) -> Result<Vec<Held>> {
        if cx.reason != Reason::Departure {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for row in deputies_of(cx.conn, cx.org_id, cx.user_id, cx.date)? {
            let covers_others = row.deputy_user_id == cx.user_id;
            let other = if covers_others {
                &row.user_id
            } else {
                &row.deputy_user_id
            };
            let title = cx.names.get(other).cloned().unwrap_or_default();
            let (action, suggestion, eligible) = if covers_others {
                // Somebody else who covers the same person, else the person's manager.
                let others: Vec<String> = deputies_where(
                    cx.conn,
                    "org_id = ?1 AND user_id = ?2 AND deputy_user_id <> ?3 \
                     AND (valid_to IS NULL OR valid_to > ?4) ORDER BY valid_from, id",
                    [
                        cx.org_id,
                        row.user_id.as_str(),
                        cx.user_id,
                        format_date(cx.date).as_str(),
                    ],
                )?
                .into_iter()
                .map(|d| d.deputy_user_id)
                .collect();
                let eligible = cx
                    .members
                    .iter()
                    .filter(|id| **id != cx.user_id && **id != row.user_id)
                    .cloned()
                    .collect();
                (
                    Action::TransferOrEnd,
                    cx.advice
                        .for_cover(&others)
                        .filter(|s: &Suggestion| s.user_id != row.user_id),
                    Some(eligible),
                )
            } else {
                (Action::End, None, None)
            };
            out.push(Held {
                key: format!("deputy:{}", row.id),
                category: Category::Deputy,
                title,
                role: if covers_others { "deputy" } else { "covered" }.into(),
                state: row.scope.as_wire(),
                project_id: None,
                project_name: None,
                unit_name: None,
                valid_to: row.valid_to.map(format_date),
                action,
                suggestion,
                eligible,
                blocked: None,
            });
        }
        Ok(out)
    }
}

impl OrgProvider for DeputyProvider {
    fn apply_in(&self, s: &mut Session<'_>, cx: &ApplyCx<'_>, item: &Planned) -> Result<()> {
        let id = item
            .key
            .strip_prefix("deputy:")
            .ok_or_else(|| E::InvalidValue {
                field: "key",
                reason: "not a deputy key".into(),
            })?
            .to_string();
        let row = deputies_where(s.tx, "org_id = ?1 AND id = ?2", [s.org_id, id.as_str()])?
            .into_iter()
            .next()
            .filter(|d| d.user_id == cx.from_user || d.deputy_user_id == cx.from_user)
            .ok_or_else(|| E::NotFound {
                entity: "deputy",
                id: id.clone(),
            })?;
        s.attempt("org.deputy.end", |s| {
            cover::end_deputy_in(s, cx.actor_is_admin, &row.id, cx.date)
        })?;
        // Only the person who covers can hand the cover on; a covered person's deputies just stop.
        if let (Some(taker), true) = (&item.taker, row.deputy_user_id == cx.from_user) {
            let new = NewDeputy {
                user_id: row.user_id.clone(),
                deputy_user_id: taker.clone(),
                scope: row.scope.clone(),
                valid_from: cx.date.max(row.valid_from),
                valid_to: row.valid_to,
            };
            s.attempt("org.deputy.set", |s| {
                cover::set_deputy_in(s, cx.actor, cx.actor_is_admin, &new)
            })?;
        }
        Ok(())
    }
}
