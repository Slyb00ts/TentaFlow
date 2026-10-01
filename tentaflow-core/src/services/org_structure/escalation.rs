//! `org.escalation_chain` and the manager with deputies (docs §2.1a, §1.1).
//!
//! Computed at the moment of the question from one `Snapshot` and one
//! `Availability` of the same day, never remembered: a change of structure in
//! the middle of an SLA run takes effect at the next step.
//!
//! One level of the chain is one position on the primary line above the
//! person. The level answers with ONE person, in this order (the first that is
//! available wins):
//!   1. a holder of the position;
//!   2. when the position heads a unit: the unit's deputy heads, in order —
//!      only now, because a deputy head acts only under the head's absence;
//!   3. a temporary deputy (`org_deputies`) of an absent holder whose scope
//!      covers the question.
//!
//! A level nobody answers is skipped and the walk continues one position up.
//! Functional lines are never followed. The walk stops at the top, on a
//! repeated position (a cycle in the data: reported, not looped on) and after
//! `MAX_LEVELS` levels.
//!
//! Not modeled: several equal heads of one unit ("współprowadzący"). A unit
//! has one `head_position_id`; the plan leaves the setting to the unit, and
//! the unit has none yet.

use std::collections::HashSet;

use serde::Serialize;

use super::availability::{Availability, DeputyScope};
use super::query::{Manager, ManagerSource, Snapshot};
use super::types::Subject;

/// The most levels one chain walks — a guard against bad data.
pub const MAX_LEVELS: u32 = 20;

/// Why the person at a level is the one asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// A holder of the position.
    Holder,
    /// A deputy head of the unit, because the head is away or the seat vacant.
    DeputyHead,
    /// A temporary deputy of an away holder.
    Deputy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    /// 1 = the position directly above the person; skipped levels count.
    pub level: u32,
    pub user_id: String,
    pub position_id: String,
    pub via: Via,
    /// Whose place the person takes; `None` for the holder itself.
    pub covering_user_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Nobody holds the position (or only the asking person does).
    Vacant,
    /// Holders exist but none is available and nobody covers them.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub level: u32,
    pub position_id: String,
    pub reason: SkipReason,
}

/// A defect of the structure the walk met. It is data for the administrator;
/// the chain up to it is still returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Problem {
    /// The line leads back to a position already visited.
    Cycle { position_id: String },
    /// More than `MAX_LEVELS` levels.
    TooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Chain {
    pub steps: Vec<Step>,
    pub skipped: Vec<Skipped>,
    pub problem: Option<Problem>,
}

enum Level {
    Found {
        user_id: String,
        position_id: String,
        via: Via,
        covering: Option<String>,
    },
    Skipped(SkipReason),
}

fn user_holders<'a>(snap: &'a Snapshot, position_id: &str, requester: &str) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    snap.holders_of(position_id)
        .filter_map(|a| match &a.subject {
            Subject::User(id) if id != requester => Some(id.as_str()),
            _ => None,
        })
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Who answers for `position_id` on the day, or why nobody does. `taken` are
/// people the chain already returned; they are not asked twice.
fn resolve_level(
    snap: &Snapshot,
    avail: &Availability,
    requester: &str,
    position_id: &str,
    scope: &DeputyScope,
    taken: &HashSet<String>,
) -> Level {
    let free = |user: &str| user != requester && !taken.contains(user) && avail.is_available(user);
    let holders = user_holders(snap, position_id, requester);

    if let Some(holder) = holders.iter().find(|h| free(h)) {
        return Level::Found {
            user_id: holder.to_string(),
            position_id: position_id.to_string(),
            via: Via::Holder,
            covering: None,
        };
    }

    // The person the substitutes stand in for: the first holder, or nobody.
    let covered = holders.first().map(|h| h.to_string());

    if let Some(position) = snap.position(position_id).filter(|p| snap.is_head(p)) {
        for deputy_head in snap
            .deputy_heads
            .iter()
            .filter(|d| d.unit_id == position.unit_id)
        {
            for user in user_holders(snap, &deputy_head.position_id, requester) {
                if free(user) {
                    return Level::Found {
                        user_id: user.to_string(),
                        position_id: deputy_head.position_id.clone(),
                        via: Via::DeputyHead,
                        covering: covered,
                    };
                }
            }
        }
    }

    // A temporary deputy stands in for a person who is there but away; a
    // closed account has nobody to cover.
    for holder in holders.iter().filter(|h| avail.is_active(h)) {
        for deputy in avail.deputies_of(holder, scope) {
            if free(&deputy.deputy_user_id) {
                return Level::Found {
                    user_id: deputy.deputy_user_id.clone(),
                    position_id: position_id.to_string(),
                    via: Via::Deputy,
                    covering: Some((*holder).to_string()),
                };
            }
        }
    }

    Level::Skipped(if holders.is_empty() {
        SkipReason::Vacant
    } else {
        SkipReason::Unavailable
    })
}

/// `org.escalation_chain(user, scope)`: the people to ask one after another,
/// from the manager up, on the day of `snap`.
pub fn escalation_chain(
    snap: &Snapshot,
    avail: &Availability,
    user_id: &str,
    scope: &DeputyScope,
) -> Chain {
    let mut chain = Chain::default();
    let Some(own) = snap.primary_assignment(user_id) else {
        return chain;
    };
    let mut position = own.position_id.as_str();
    let mut visited: HashSet<&str> = HashSet::from([position]);
    let mut taken: HashSet<String> = HashSet::new();

    for level in 1..=MAX_LEVELS + 1 {
        let Some(parent) = snap.primary_parent_of(position) else {
            return chain;
        };
        if level > MAX_LEVELS {
            chain.problem = Some(Problem::TooDeep);
            return chain;
        }
        if !visited.insert(parent) {
            chain.problem = Some(Problem::Cycle {
                position_id: parent.to_string(),
            });
            return chain;
        }
        match resolve_level(snap, avail, user_id, parent, scope, &taken) {
            Level::Found {
                user_id: found,
                position_id,
                via,
                covering,
            } => {
                taken.insert(found.clone());
                chain.steps.push(Step {
                    level,
                    user_id: found,
                    position_id,
                    via,
                    covering_user_id: covering,
                });
            }
            Level::Skipped(reason) => chain.skipped.push(Skipped {
                level,
                position_id: parent.to_string(),
                reason,
            }),
        }
        position = parent;
    }
    chain
}

/// `org.get_manager` with deputies. The structural manager (`Snapshot::manager_of`)
/// when they are available; otherwise the deputy head or temporary deputy
/// (scope `all`) who stands in; and when nobody does, the structural manager
/// all the same — a manager on leave is still the manager.
pub fn effective_manager(snap: &Snapshot, avail: &Availability, user_id: &str) -> Option<Manager> {
    let structural = snap.manager_of(user_id)?;
    if avail.is_available(&structural.user_id) {
        return Some(structural);
    }
    let taken = HashSet::new();
    let substitute = match resolve_level(
        snap,
        avail,
        user_id,
        &structural.position_id,
        &DeputyScope::All,
        &taken,
    ) {
        Level::Found {
            user_id,
            position_id,
            via,
            ..
        } => Some(Manager {
            user_id,
            position_id,
            source: match via {
                Via::Holder => ManagerSource::PrimaryHolder,
                Via::DeputyHead => ManagerSource::DeputyHead,
                Via::Deputy => ManagerSource::Deputy,
            },
        }),
        Level::Skipped(_) => None,
    };
    Some(substitute.unwrap_or(structural))
}
