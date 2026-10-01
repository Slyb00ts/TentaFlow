//! Who may see which personal data of a person (docs §6.3), and the two views
//! the Widoczność tab draws from the same rules: what one person sees, and who
//! sees one person's data.
//!
//! The rules read the PRIMARY reporting line only (`Snapshot::manager_of` and
//! `is_subordinate_of(.., SeatScope::Primary)`): a functional line — a matrix
//! manager — never grants a view. The check is the single source; the two
//! views are built by asking it, and a test holds them equal to it.
//!
//! Not modeled yet: a project manager (only within the project) and the board
//! (docs §6.3, time and utilization). The Projects module has no reader for
//! them here and no organization-wide permission names the board.

use std::collections::HashSet;

use chrono::NaiveDate;
use serde::Serialize;

use super::availability::{self, Absence, Availability, Deputy};
use super::error::{OrgStructureError as E, Result};
use super::query::{SeatScope, Snapshot};
use super::repo::timezone_of;
use super::validate;
use crate::db::DbPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonDataKind {
    /// Why the person is away.
    AbsenceReason,
    /// When the person is away — the dates, not the reason.
    AbsenceDates,
    /// Time tracking and utilization.
    TimeUtilization,
    /// The positions the person held over time.
    PositionHistory,
}

impl PersonDataKind {
    pub const ALL: [Self; 4] = [
        Self::AbsenceReason,
        Self::AbsenceDates,
        Self::TimeUtilization,
        Self::PositionHistory,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AbsenceReason => "absence_reason",
            Self::AbsenceDates => "absence_dates",
            Self::TimeUtilization => "time_utilization",
            Self::PositionHistory => "position_history",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }
}

/// The rule that grants the view, strongest first. A viewer who qualifies
/// twice is reported under the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewRule {
    /// The data is the viewer's own.
    Owner,
    /// The manager on the primary line, one step above.
    PrimaryManager,
    /// Somebody higher on the primary line.
    Supervisor,
    Administrator,
    /// Anybody with an account in the organization (the structure itself).
    EveryMember,
    /// Nothing grants it.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub allowed: bool,
    pub rule: ViewRule,
}

impl Decision {
    fn by(rule: ViewRule) -> Self {
        Self {
            allowed: rule != ViewRule::None,
            rule,
        }
    }
}

fn is_direct_manager(snap: &Snapshot, viewer: &str, subject: &str) -> bool {
    snap.manager_of(subject)
        .is_some_and(|m| m.user_id == viewer)
}

/// `org.can_view_person_data(viewer, subject, kind)`. `viewer_is_admin` is
/// `org.admin` in the organization, decided by the caller.
pub fn can_view_person_data(
    snap: &Snapshot,
    viewer: &str,
    subject: &str,
    kind: PersonDataKind,
    viewer_is_admin: bool,
) -> Decision {
    if viewer == subject {
        return Decision::by(ViewRule::Owner);
    }
    let admin = || viewer_is_admin.then_some(ViewRule::Administrator);
    let direct = || is_direct_manager(snap, viewer, subject).then_some(ViewRule::PrimaryManager);
    let above = || {
        snap.is_subordinate_of(subject, viewer, SeatScope::Primary)
            .then_some(ViewRule::Supervisor)
    };
    let rule = match kind {
        PersonDataKind::AbsenceReason => direct().or_else(admin),
        PersonDataKind::AbsenceDates => direct().or_else(above).or_else(admin),
        PersonDataKind::TimeUtilization => direct().or_else(above),
        PersonDataKind::PositionHistory => admin(),
    };
    Decision::by(rule.unwrap_or(ViewRule::None))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Viewer {
    pub user_id: String,
    pub rule: ViewRule,
}

/// Everybody who may see `kind` of `subject`: the person, the managers above
/// them on the primary line as the kind allows, and `admins`.
pub fn who_can_view(
    snap: &Snapshot,
    subject: &str,
    kind: PersonDataKind,
    admins: &[String],
) -> Vec<Viewer> {
    let mut out: Vec<Viewer> = vec![Viewer {
        user_id: subject.to_string(),
        rule: ViewRule::Owner,
    }];
    let mut seen: HashSet<String> = HashSet::from([subject.to_string()]);
    let mut push = |user: &str, rule: ViewRule, out: &mut Vec<Viewer>| {
        if seen.insert(user.to_string()) {
            out.push(Viewer {
                user_id: user.to_string(),
                rule,
            });
        }
    };

    let managers = matches!(
        kind,
        PersonDataKind::AbsenceReason
            | PersonDataKind::AbsenceDates
            | PersonDataKind::TimeUtilization
    );
    if managers {
        let mut current = subject.to_string();
        let mut walked: HashSet<String> = HashSet::from([current.clone()]);
        let mut first = true;
        while let Some(manager) = snap.manager_of(&current) {
            let rule = if first {
                ViewRule::PrimaryManager
            } else {
                ViewRule::Supervisor
            };
            // The reason is for the direct manager only; the dates and the
            // utilization for everybody above.
            if first || kind != PersonDataKind::AbsenceReason {
                push(&manager.user_id, rule, &mut out);
            }
            first = false;
            if !walked.insert(manager.user_id.clone()) {
                break;
            }
            current = manager.user_id;
        }
    }
    let admins_see = matches!(
        kind,
        PersonDataKind::AbsenceReason
            | PersonDataKind::AbsenceDates
            | PersonDataKind::PositionHistory
    );
    if admins_see {
        for admin in admins {
            push(admin, ViewRule::Administrator, &mut out);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Structure,
    Utilization,
    AbsenceDates,
    AbsenceReasons,
    PositionHistory,
    EveryoneElse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// All the data of everybody in the organization.
    All,
    /// The people below the viewer on the primary line.
    Subtree,
    /// Only the people one step below.
    Direct,
    /// Only the viewer's own.
    Own,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AreaRow {
    pub area: Area,
    pub verdict: Verdict,
    pub rule: ViewRule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Visibility {
    /// Everybody below the viewer on the primary line, sorted by id.
    pub subtree: Vec<String>,
    /// The ones one step below.
    pub direct: Vec<String>,
    pub rows: Vec<AreaRow>,
}

/// What `viewer` sees, area by area, with the rule behind each answer.
pub fn visibility_of(snap: &Snapshot, viewer: &str, viewer_is_admin: bool) -> Visibility {
    let mut users: Vec<&str> = snap.users().collect();
    users.sort_unstable();
    let subtree: Vec<String> = users
        .iter()
        .filter(|u| snap.is_subordinate_of(u, viewer, SeatScope::Primary))
        .map(|u| u.to_string())
        .collect();
    let direct: Vec<String> = subtree
        .iter()
        .filter(|u| is_direct_manager(snap, viewer, u))
        .cloned()
        .collect();

    let down = |present: bool, rule: ViewRule| {
        if present {
            (Verdict::Subtree, rule)
        } else {
            (Verdict::Own, ViewRule::Owner)
        }
    };
    let (utilization, utilization_rule) = down(!subtree.is_empty(), ViewRule::PrimaryManager);
    let (dates, dates_rule) = if viewer_is_admin {
        (Verdict::All, ViewRule::Administrator)
    } else {
        down(!subtree.is_empty(), ViewRule::PrimaryManager)
    };
    let (reasons, reasons_rule) = if viewer_is_admin {
        (Verdict::All, ViewRule::Administrator)
    } else if !direct.is_empty() {
        (Verdict::Direct, ViewRule::PrimaryManager)
    } else {
        (Verdict::Own, ViewRule::Owner)
    };
    let (history, history_rule) = if viewer_is_admin {
        (Verdict::All, ViewRule::Administrator)
    } else {
        (Verdict::Own, ViewRule::Owner)
    };

    Visibility {
        rows: vec![
            AreaRow {
                area: Area::Structure,
                verdict: Verdict::All,
                rule: ViewRule::EveryMember,
            },
            AreaRow {
                area: Area::Utilization,
                verdict: utilization,
                rule: utilization_rule,
            },
            AreaRow {
                area: Area::AbsenceDates,
                verdict: dates,
                rule: dates_rule,
            },
            AreaRow {
                area: Area::AbsenceReasons,
                verdict: reasons,
                rule: reasons_rule,
            },
            AreaRow {
                area: Area::PositionHistory,
                verdict: history,
                rule: history_rule,
            },
            AreaRow {
                area: Area::EveryoneElse,
                verdict: Verdict::None,
                rule: ViewRule::None,
            },
        ],
        subtree,
        direct,
    }
}

// ---------------------------------------------------------------------------
// The answers the screens read, composed from the rules above
// ---------------------------------------------------------------------------

fn rank(rule: ViewRule) -> u8 {
    match rule {
        ViewRule::Owner => 0,
        ViewRule::PrimaryManager => 1,
        ViewRule::Supervisor => 2,
        ViewRule::Administrator => 3,
        ViewRule::EveryMember => 4,
        ViewRule::None => 5,
    }
}

/// One person's presence, absences and deputies as `viewer` may see them.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonCover {
    pub user_id: String,
    pub today: NaiveDate,
    pub available: bool,
    /// Empty unless the viewer may see the dates; the reason is cleared unless
    /// the viewer may see that too.
    pub absences: Vec<Absence>,
    pub covered_by: Vec<Deputy>,
    pub covering: Vec<Deputy>,
    pub can_see_absences: bool,
    pub can_see_reason: bool,
    pub can_edit_absences: bool,
}

fn day_or_today(pool: &DbPool, org_id: &str, at: Option<NaiveDate>) -> Result<NaiveDate> {
    match at {
        Some(day) => Ok(day),
        None => {
            let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
            validate::today_in_zone(&timezone_of(&conn, org_id)?)
        }
    }
}

pub fn person_cover(
    pool: &DbPool,
    org_id: &str,
    viewer: &str,
    viewer_is_admin: bool,
    subject: &str,
    at: Option<NaiveDate>,
    include_past: bool,
) -> Result<PersonCover> {
    if !availability::is_member(pool, org_id, subject)? {
        return Err(E::NotFound {
            entity: "user",
            id: subject.to_string(),
        });
    }
    let today = day_or_today(pool, org_id, at)?;
    let (snap, avail) = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        (
            Snapshot::load(&conn, org_id, today)?,
            Availability::load(&conn, org_id, today)?,
        )
    };
    let decide = |kind| can_view_person_data(&snap, viewer, subject, kind, viewer_is_admin).allowed;
    let can_see_absences = decide(PersonDataKind::AbsenceDates);
    let can_see_reason = decide(PersonDataKind::AbsenceReason);
    let mut absences = if can_see_absences {
        availability::absences_of(pool, org_id, subject, today, include_past)?
    } else {
        Vec::new()
    };
    if !can_see_reason {
        for absence in &mut absences {
            absence.reason = None;
        }
    }
    let (covered_by, covering) = availability::deputies_around(pool, org_id, subject, today)?;
    Ok(PersonCover {
        user_id: subject.to_string(),
        today,
        available: avail.is_available(subject),
        absences,
        covered_by,
        covering,
        can_see_absences,
        can_see_reason,
        can_edit_absences: viewer == subject || viewer_is_admin,
    })
}

/// A person who may see something of the subject: the strongest rule and every
/// kind they may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoSees {
    pub user_id: String,
    pub rule: ViewRule,
    pub kinds: Vec<PersonDataKind>,
}

/// Everybody who may see any personal data of `subject`, strongest rule first.
pub fn who_sees(snap: &Snapshot, subject: &str, admins: &[String]) -> Vec<WhoSees> {
    let mut merged: Vec<WhoSees> = Vec::new();
    for kind in PersonDataKind::ALL {
        for viewer in who_can_view(snap, subject, kind, admins) {
            match merged.iter_mut().find(|w| w.user_id == viewer.user_id) {
                Some(entry) => {
                    entry.kinds.push(kind);
                    if rank(viewer.rule) < rank(entry.rule) {
                        entry.rule = viewer.rule;
                    }
                }
                None => merged.push(WhoSees {
                    user_id: viewer.user_id,
                    rule: viewer.rule,
                    kinds: vec![kind],
                }),
            }
        }
    }
    merged.sort_by(|a, b| {
        rank(a.rule)
            .cmp(&rank(b.rule))
            .then(b.kinds.len().cmp(&a.kinds.len()))
            .then(a.user_id.cmp(&b.user_id))
    });
    merged
}
