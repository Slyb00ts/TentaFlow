//! Who to propose as the taker of an item (docs/PROJECT_STUDIO_WORKFLOW_PLAN.md
//! §4.5, docs/ORG_STRUCTURE_PLAN.md §2.6): the person's deputy, then their
//! manager, then the project manager. Only people who can take the item and are
//! present on the day are proposed; when nobody qualifies there is no proposal
//! and the screen asks for a choice.
//!
//! The rules are pure (`Advice` holds what was read); `Advice::load` reads it
//! from the structure on the day.

use std::collections::{HashMap, HashSet};

use chrono::{Days, NaiveDate};
use rusqlite::Connection;

use super::Suggestion;
use crate::services::org_structure::availability::{Availability, DeputyScope};
use crate::services::org_structure::error::Result;
use crate::services::org_structure::escalation;
use crate::services::org_structure::query::Snapshot;
use crate::services::org_structure::Subject;

/// Who may take an item, as far as the item itself says.
pub(super) struct Candidates<'a> {
    /// The people the item's store allows (project members); `None` = any member.
    pub eligible: Option<&'a HashSet<String>>,
    /// The project's managers, then its owner, in the order they are proposed.
    pub project_managers: &'a [String],
    pub project_owner: Option<&'a str>,
}

pub(crate) struct Advice {
    person: String,
    /// The person's deputies in force on the day, oldest appointment first.
    deputies: Vec<(DeputyScope, String)>,
    manager: Option<String>,
    /// Active members with no absence on the day.
    available: HashSet<String>,
    /// Deputy heads of a unit, in their order, as users.
    unit_deputy_heads: HashMap<String, Vec<String>>,
    heads: HashSet<String>,
}

impl Advice {
    /// `reference_day` is a day on which the person still holds their seat (the
    /// structure of the day after they left knows nothing about them);
    /// presence and deputies are read for `date`, the day the work changes hands.
    pub(super) fn load(
        conn: &Connection,
        org_id: &str,
        person: &str,
        reference_day: NaiveDate,
        date: NaiveDate,
    ) -> Result<Self> {
        let snap = Snapshot::load(conn, org_id, reference_day)?;
        let avail = Availability::load(conn, org_id, date)?;
        let manager = escalation::effective_manager(&snap, &avail, person).map(|m| m.user_id);
        let deputies = avail
            .deputies
            .iter()
            .filter(|d| d.user_id == person)
            .map(|d| (d.scope.clone(), d.deputy_user_id.clone()))
            .collect();
        let mut unit_deputy_heads: HashMap<String, Vec<String>> = HashMap::new();
        for head in &snap.deputy_heads {
            let holders = snap
                .holders_of(&head.position_id)
                .filter_map(|a| match &a.subject {
                    Subject::User(id) => Some(id.clone()),
                    Subject::External(_) => None,
                })
                .collect::<Vec<_>>();
            unit_deputy_heads
                .entry(head.unit_id.clone())
                .or_default()
                .extend(holders);
        }
        let heads = snap
            .units
            .iter()
            .filter_map(|u| u.head_position_id.clone())
            .collect();
        let available = avail.available_users().map(str::to_string).collect();
        Ok(Self {
            person: person.to_string(),
            deputies,
            manager,
            available,
            unit_deputy_heads,
            heads,
        })
    }

    fn usable(&self, user: &str, eligible: Option<&HashSet<String>>) -> bool {
        user != self.person
            && self.available.contains(user)
            && eligible.is_none_or(|set| set.contains(user))
    }

    /// The proposal for an item of work in a project: deputy, manager, project
    /// manager, project owner.
    pub(super) fn for_work(
        &self,
        scope: &DeputyScope,
        candidates: &Candidates<'_>,
    ) -> Option<Suggestion> {
        let ok = |user: &str| self.usable(user, candidates.eligible);
        self.deputies
            .iter()
            .filter(|(covers, _)| covers.covers(scope))
            .map(|(_, user)| user)
            .find(|user| ok(user))
            .map(|user| Suggestion::new(user, "deputy"))
            .or_else(|| {
                self.manager
                    .as_deref()
                    .filter(|user| ok(user))
                    .map(|user| Suggestion::new(user, "manager"))
            })
            .or_else(|| {
                candidates
                    .project_managers
                    .iter()
                    .find(|user| ok(user))
                    .map(|user| Suggestion::new(user, "project_manager"))
            })
            .or_else(|| {
                candidates
                    .project_owner
                    .filter(|user| ok(user))
                    .map(|user| Suggestion::new(user, "project_owner"))
            })
    }

    /// The proposal for a seat: when it heads its unit the unit's deputy head,
    /// then the person's deputy, then their manager.
    pub(super) fn for_position(&self, position_id: &str, unit_id: &str) -> Option<Suggestion> {
        let ok = |user: &str| self.usable(user, None);
        let deputy_head = if self.heads.contains(position_id) {
            self.unit_deputy_heads
                .get(unit_id)
                .into_iter()
                .flatten()
                .find(|user| ok(user))
                .map(|user| Suggestion::new(user, "deputy_head"))
        } else {
            None
        };
        deputy_head
            .or_else(|| {
                self.deputies
                    .iter()
                    .find(|(covers, _)| covers.covers(&DeputyScope::All))
                    .map(|(_, user)| user)
                    .filter(|user| ok(user))
                    .map(|user| Suggestion::new(user, "deputy"))
            })
            .or_else(|| {
                self.manager
                    .as_deref()
                    .filter(|user| ok(user))
                    .map(|user| Suggestion::new(user, "manager"))
            })
    }

    /// The proposal for a cover the person gives: somebody else who covers the
    /// same person, then the covered person's own manager is out of reach here,
    /// so it falls back to the person's manager.
    pub(super) fn for_cover(&self, other_deputies: &[String]) -> Option<Suggestion> {
        let ok = |user: &str| self.usable(user, None);
        other_deputies
            .iter()
            .find(|user| ok(user))
            .map(|user| Suggestion::new(user, "next_deputy"))
            .or_else(|| {
                self.manager
                    .as_deref()
                    .filter(|user| ok(user))
                    .map(|user| Suggestion::new(user, "manager"))
            })
    }
}

/// A day the person still held their seat on: today while they hold one, else
/// the day before the last assignment ended.
pub(super) fn reference_day(
    conn: &Connection,
    org_id: &str,
    person: &str,
    today: NaiveDate,
) -> Result<NaiveDate> {
    Ok(match last_end(conn, org_id, person, today)? {
        Some(ended) => ended.checked_sub_days(Days::new(1)).unwrap_or(ended),
        None => today,
    })
}

/// The day the person's last assignment ended, when they hold none on `today`
/// or later and did hold one before.
pub(super) fn last_end(
    conn: &Connection,
    org_id: &str,
    person: &str,
    today: NaiveDate,
) -> Result<Option<NaiveDate>> {
    let (any, open, last): (i64, i64, Option<String>) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(valid_to IS NULL OR valid_to > ?3), 0), MAX(valid_to) \
         FROM org_assignments WHERE org_id = ?1 AND user_id = ?2",
        rusqlite::params![
            org_id,
            person,
            crate::services::org_structure::validate::format_date(today)
        ],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if any == 0 || open > 0 {
        return Ok(None);
    }
    last.as_deref()
        .map(crate::services::org_structure::validate::parse_date)
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advice() -> Advice {
        Advice {
            person: "leaver".into(),
            deputies: vec![
                (DeputyScope::Approvals, "approver".into()),
                (DeputyScope::Project("p-1".into()), "p1-cover".into()),
                (DeputyScope::All, "cover".into()),
            ],
            manager: Some("boss".into()),
            available: [
                "approver", "p1-cover", "cover", "boss", "pm", "owner", "leaver", "dh",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            unit_deputy_heads: HashMap::from([(
                "u-1".to_string(),
                vec!["dh-away".into(), "dh".into()],
            )]),
            heads: HashSet::from(["pos-head".to_string()]),
        }
    }

    fn set(users: &[&str]) -> HashSet<String> {
        users.iter().map(|u| u.to_string()).collect()
    }

    #[test]
    fn work_goes_to_a_deputy_whose_scope_covers_the_project_before_anybody_else() {
        let advice = advice();
        let managers = vec!["pm".to_string()];
        let candidates = Candidates {
            eligible: None,
            project_managers: &managers,
            project_owner: Some("owner"),
        };
        let picked = advice
            .for_work(&DeputyScope::Project("p-1".into()), &candidates)
            .unwrap();
        // "approvals" does not cover a project; the project's own deputy comes first.
        assert_eq!(
            (picked.user_id.as_str(), picked.reason),
            ("p1-cover", "deputy")
        );
        let other = advice
            .for_work(&DeputyScope::Project("p-2".into()), &candidates)
            .unwrap();
        assert_eq!((other.user_id.as_str(), other.reason), ("cover", "deputy"));
    }

    #[test]
    fn without_a_deputy_it_is_the_manager_then_the_project_manager_then_the_owner() {
        let mut advice = advice();
        advice.deputies.clear();
        let managers = vec!["pm".to_string()];
        let candidates = Candidates {
            eligible: None,
            project_managers: &managers,
            project_owner: Some("owner"),
        };
        let scope = DeputyScope::Project("p-1".into());
        assert_eq!(
            advice.for_work(&scope, &candidates).unwrap().reason,
            "manager"
        );
        advice.manager = None;
        let picked = advice.for_work(&scope, &candidates).unwrap();
        assert_eq!(
            (picked.user_id.as_str(), picked.reason),
            ("pm", "project_manager")
        );
        let no_pm = Candidates {
            eligible: None,
            project_managers: &[],
            project_owner: Some("owner"),
        };
        assert_eq!(
            advice.for_work(&scope, &no_pm).unwrap().reason,
            "project_owner"
        );
    }

    #[test]
    fn only_present_people_the_store_allows_are_proposed() {
        let mut advice = advice();
        advice.available.remove("p1-cover");
        advice.available.remove("cover");
        let members = set(&["pm", "owner"]);
        let managers = vec!["pm".to_string()];
        let candidates = Candidates {
            eligible: Some(&members),
            project_managers: &managers,
            project_owner: None,
        };
        // The manager is present but not a member of the project, so the project manager it is.
        let picked = advice
            .for_work(&DeputyScope::Project("p-1".into()), &candidates)
            .unwrap();
        assert_eq!(
            (picked.user_id.as_str(), picked.reason),
            ("pm", "project_manager")
        );
        let strangers = set(&["stranger"]);
        let nobody = Candidates {
            eligible: Some(&strangers),
            project_managers: &managers,
            project_owner: None,
        };
        assert!(advice.for_work(&DeputyScope::All, &nobody).is_none());
    }

    #[test]
    fn the_person_is_never_their_own_taker() {
        let mut advice = advice();
        advice.deputies = vec![(DeputyScope::All, "leaver".into())];
        advice.manager = None;
        let candidates = Candidates {
            eligible: None,
            project_managers: &["leaver".to_string()],
            project_owner: None,
        };
        assert!(advice.for_work(&DeputyScope::All, &candidates).is_none());
    }

    #[test]
    fn a_head_seat_goes_to_the_first_present_deputy_head() {
        let advice = advice();
        let picked = advice.for_position("pos-head", "u-1").unwrap();
        assert_eq!(
            (picked.user_id.as_str(), picked.reason),
            ("dh", "deputy_head")
        );
        // A seat that heads nothing has no deputy head to give it to: the cover, then the manager.
        assert_eq!(
            advice.for_position("pos-other", "u-1").unwrap().reason,
            "deputy"
        );
    }

    #[test]
    fn a_cover_goes_to_another_deputy_of_the_covered_person_then_to_the_manager() {
        let advice = advice();
        let picked = advice.for_cover(&["cover".to_string()]).unwrap();
        assert_eq!(
            (picked.user_id.as_str(), picked.reason),
            ("cover", "next_deputy")
        );
        assert_eq!(advice.for_cover(&[]).unwrap().reason, "manager");
    }
}
