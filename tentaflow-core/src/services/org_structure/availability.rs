//! Who is present on a day, and who covers whom (docs/ORG_STRUCTURE_PLAN.md
//! §1 `org_deputies` / `org_absences`, §2.1 `is_available`).
//!
//! An `Availability` is loaded next to a `Snapshot` for the same day. Kept
//! apart from it on purpose: the projection into `sync_user_org_profiles`
//! (permissions) reads only the structure, so an absence never moves a
//! permission; the escalation chain and the effective manager read both.
//!
//! A person is AVAILABLE on a day when their account is an active member of
//! the organization and no absence covers the day. An absence has no reason
//! at all: everything here answers "unavailable", nothing says why (docs §6.3).

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::error::{OrgStructureError as E, Result};
use super::repo::fmt;
use super::validate;
use crate::db::DbPool;

/// What a deputy covers. `Project` carries the project id verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeputyScope {
    All,
    Approvals,
    Escalations,
    Project(String),
}

impl DeputyScope {
    pub const PROJECT_PREFIX: &'static str = "project:";
    /// Longest project id a scope may carry; rows replicate to every node.
    pub const MAX_PROJECT_ID_CHARS: usize = 100;

    pub fn parse(raw: &str) -> Result<Self> {
        let invalid = |reason: &str| E::InvalidValue {
            field: "scope",
            reason: reason.to_string(),
        };
        match raw {
            "all" => Ok(Self::All),
            "approvals" => Ok(Self::Approvals),
            "escalations" => Ok(Self::Escalations),
            other => match other.strip_prefix(Self::PROJECT_PREFIX) {
                Some(id) if !id.trim().is_empty() => {
                    if id.chars().count() > Self::MAX_PROJECT_ID_CHARS {
                        return Err(invalid("project id is too long"));
                    }
                    if id.chars().any(char::is_control) {
                        return Err(invalid("project id has control characters"));
                    }
                    Ok(Self::Project(id.to_string()))
                }
                _ => Err(invalid(
                    "expected all, approvals, escalations or project:<id>",
                )),
            },
        }
    }

    pub fn as_wire(&self) -> String {
        match self {
            Self::All => "all".into(),
            Self::Approvals => "approvals".into(),
            Self::Escalations => "escalations".into(),
            Self::Project(id) => format!("{}{id}", Self::PROJECT_PREFIX),
        }
    }

    /// True when a deputy appointed for `self` may act for a question asked in
    /// `requested`: an `all` deputy covers every question, any other scope only
    /// its own.
    pub fn covers(&self, requested: &DeputyScope) -> bool {
        matches!(self, Self::All) || self == requested
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deputy {
    pub id: String,
    /// The person who is covered.
    pub user_id: String,
    pub deputy_user_id: String,
    pub scope: DeputyScope,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbsenceKind {
    Leave,
    Training,
    Other,
}

impl AbsenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leave => "leave",
            Self::Training => "training",
            Self::Other => "other",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "leave" => Ok(Self::Leave),
            "training" => Ok(Self::Training),
            "other" => Ok(Self::Other),
            _ => Err(E::InvalidValue {
                field: "kind",
                reason: "expected leave, training or other".into(),
            }),
        }
    }
}

/// Where an absence came from. Only a manual entry may be changed by the person.
pub const SOURCE_MANUAL: &str = "manual";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Absence {
    pub id: String,
    pub user_id: String,
    pub valid_from: NaiveDate,
    /// Exclusive, like every end date of the structure; `None` = until ended.
    pub valid_to: Option<NaiveDate>,
    pub kind: AbsenceKind,
    pub source: String,
    pub created_by: Option<String>,
}

impl Absence {
    pub fn covers(&self, day: NaiveDate) -> bool {
        self.valid_from <= day && self.valid_to.is_none_or(|end| day < end)
    }
}

/// Presence and cover on one day.
pub struct Availability {
    pub at: NaiveDate,
    /// Deputies whose interval contains `at`.
    pub deputies: Vec<Deputy>,
    absent: HashSet<String>,
    /// Active accounts that are members of the organization.
    usable: HashSet<String>,
}

impl Availability {
    pub fn load(conn: &Connection, org_id: &str, at: NaiveDate) -> Result<Self> {
        let day = fmt(at);
        let mut stmt = conn.prepare(
            "SELECT user_id FROM org_absences \
             WHERE org_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2)",
        )?;
        let absent = stmt
            .query_map([org_id, &day], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;

        let mut stmt = conn.prepare(
            "SELECT u.id FROM user_accounts u JOIN org_memberships m ON m.user_id = u.id \
             WHERE m.org_id = ?1 AND u.is_active = 1",
        )?;
        let usable = stmt
            .query_map([org_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;

        let deputies = deputies_where(
            conn,
            "org_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2) \
             ORDER BY valid_from, id",
            [org_id, day.as_str()],
        )?;
        Ok(Self {
            at,
            deputies,
            absent,
            usable,
        })
    }

    /// The picture for a day, where everybody `may_see` keeps the real state
    /// of the day and everybody else shows only what is true TODAY: an
    /// absence or a cover of a person whose dates are private must not be
    /// readable by asking about another day. `today` is the same organization
    /// loaded for the real today.
    pub fn limited_to_today(
        mut self,
        today: &Availability,
        may_see: impl Fn(&str) -> bool,
    ) -> Self {
        let people: Vec<String> = self
            .absent
            .union(&today.absent)
            .filter(|user| !may_see(user))
            .cloned()
            .collect();
        for user in people {
            if today.absent.contains(&user) {
                self.absent.insert(user);
            } else {
                self.absent.remove(&user);
            }
        }
        self.deputies.retain(|d| may_see(&d.user_id));
        self.deputies.extend(
            today
                .deputies
                .iter()
                .filter(|d| !may_see(&d.user_id))
                .cloned(),
        );
        self.deputies
            .sort_by(|a, b| (a.valid_from, &a.id).cmp(&(b.valid_from, &b.id)));
        self
    }

    /// An active member with no absence on the day.
    pub fn is_available(&self, user_id: &str) -> bool {
        self.is_active(user_id) && !self.absent.contains(user_id)
    }

    /// An active member of the organization; an absence does not matter here.
    pub fn is_active(&self, user_id: &str) -> bool {
        self.usable.contains(user_id)
    }

    pub fn is_absent(&self, user_id: &str) -> bool {
        self.absent.contains(user_id)
    }

    /// Every active member with no absence on the day.
    pub fn available_users(&self) -> impl Iterator<Item = &str> {
        self.usable
            .iter()
            .filter(|user| !self.absent.contains(*user))
            .map(String::as_str)
    }

    /// Everybody with an absence on the day, sorted (a stable answer).
    pub fn absent_users(&self) -> Vec<&str> {
        let mut users: Vec<&str> = self.absent.iter().map(String::as_str).collect();
        users.sort_unstable();
        users
    }

    /// The deputies of `user_id` on the day whose scope covers `requested`,
    /// oldest appointment first (deterministic: two nodes pick the same one).
    pub fn deputies_of<'a>(
        &'a self,
        user_id: &'a str,
        requested: &'a DeputyScope,
    ) -> impl Iterator<Item = &'a Deputy> {
        self.deputies
            .iter()
            .filter(move |d| d.user_id == user_id && d.scope.covers(requested))
    }
}

pub(super) fn deputies_where<P: rusqlite::Params>(
    conn: &Connection,
    predicate: &str,
    params: P,
) -> Result<Vec<Deputy>> {
    let sql = format!(
        "SELECT id, user_id, deputy_user_id, scope, valid_from, valid_to, created_by \
         FROM org_deputies WHERE {predicate}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params, |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(id, user_id, deputy_user_id, scope, from, to, created_by)| {
                Ok(Deputy {
                    id,
                    user_id,
                    deputy_user_id,
                    scope: DeputyScope::parse(&scope)?,
                    valid_from: validate::parse_date(&from)?,
                    valid_to: to.as_deref().map(validate::parse_date).transpose()?,
                    created_by,
                })
            },
        )
        .collect()
}

pub(super) fn absences_where<P: rusqlite::Params>(
    conn: &Connection,
    predicate: &str,
    params: P,
) -> Result<Vec<Absence>> {
    let sql = format!(
        "SELECT id, user_id, valid_from, valid_to, kind, source, created_by \
         FROM org_absences WHERE {predicate}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params, |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(id, user_id, from, to, kind, source, created_by)| {
            Ok(Absence {
                id,
                user_id,
                valid_from: validate::parse_date(&from)?,
                valid_to: to.as_deref().map(validate::parse_date).transpose()?,
                kind: AbsenceKind::parse(&kind)?,
                source,
                created_by,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reads over a pool
// ---------------------------------------------------------------------------

/// One person's absences, newest first. `include_past = false` leaves out the
/// ones that ended before `day`.
pub fn absences_of(
    pool: &DbPool,
    org_id: &str,
    user_id: &str,
    day: NaiveDate,
    include_past: bool,
) -> Result<Vec<Absence>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let mut list = absences_where(
        &conn,
        "org_id = ?1 AND user_id = ?2 ORDER BY valid_from DESC, id",
        [org_id, user_id],
    )?;
    if !include_past {
        list.retain(|a| a.valid_to.is_none_or(|end| end > day));
    }
    Ok(list)
}

pub fn get_absence(pool: &DbPool, org_id: &str, id: &str) -> Result<Absence> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    absences_where(&conn, "org_id = ?1 AND id = ?2", [org_id, id])?
        .into_iter()
        .next()
        .ok_or_else(|| E::NotFound {
            entity: "absence",
            id: id.to_string(),
        })
}

/// Deputies covering `user_id` (`covered_by`) and deputies `user_id` covers
/// (`covering`), those in force on `day` or later.
pub fn deputies_around(
    pool: &DbPool,
    org_id: &str,
    user_id: &str,
    day: NaiveDate,
) -> Result<(Vec<Deputy>, Vec<Deputy>)> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let fmt_day = fmt(day);
    let covered_by = deputies_where(
        &conn,
        "org_id = ?1 AND user_id = ?2 AND (valid_to IS NULL OR valid_to > ?3) \
         ORDER BY valid_from, id",
        [org_id, user_id, fmt_day.as_str()],
    )?;
    let covering = deputies_where(
        &conn,
        "org_id = ?1 AND deputy_user_id = ?2 AND (valid_to IS NULL OR valid_to > ?3) \
         ORDER BY valid_from, id",
        [org_id, user_id, fmt_day.as_str()],
    )?;
    Ok((covered_by, covering))
}

pub fn get_deputy(pool: &DbPool, org_id: &str, id: &str) -> Result<Deputy> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    deputies_where(&conn, "org_id = ?1 AND id = ?2", [org_id, id])?
        .into_iter()
        .next()
        .ok_or_else(|| E::NotFound {
            entity: "deputy",
            id: id.to_string(),
        })
}

/// Display names of the organization's members, by user id.
pub fn display_names(pool: &DbPool, org_id: &str) -> Result<HashMap<String, String>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let mut stmt = conn.prepare(
        "SELECT u.id, COALESCE(NULLIF(u.display_name, ''), u.username) \
         FROM user_accounts u JOIN org_memberships m ON m.user_id = u.id \
         WHERE m.org_id = ?1",
    )?;
    let names = stmt
        .query_map([org_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(names)
}

/// Active accounts that are members of the organization, by name.
pub fn active_members(pool: &DbPool, org_id: &str) -> Result<Vec<(String, String)>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let mut stmt = conn.prepare(
        "SELECT u.id, COALESCE(NULLIF(u.display_name, ''), u.username) \
         FROM user_accounts u JOIN org_memberships m ON m.user_id = u.id \
         WHERE m.org_id = ?1 AND u.is_active = 1 ORDER BY 2, 1",
    )?;
    let rows = stmt
        .query_map([org_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// True when `user_id` is a member of the organization.
pub fn is_member(pool: &DbPool, org_id: &str, user_id: &str) -> Result<bool> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_memberships WHERE org_id = ?1 AND user_id = ?2)",
        [org_id, user_id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn scope_round_trips_and_rejects_what_the_table_would_refuse() {
        for raw in ["all", "approvals", "escalations", "project:p-1"] {
            assert_eq!(DeputyScope::parse(raw).unwrap().as_wire(), raw);
        }
        for raw in ["", "everything", "project:", "project:   ", "ALL"] {
            assert!(DeputyScope::parse(raw).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn an_all_deputy_covers_every_question_and_any_other_only_its_own() {
        let project = DeputyScope::Project("p-1".into());
        assert!(DeputyScope::All.covers(&DeputyScope::Approvals));
        assert!(DeputyScope::All.covers(&project));
        assert!(DeputyScope::Approvals.covers(&DeputyScope::Approvals));
        assert!(!DeputyScope::Approvals.covers(&DeputyScope::Escalations));
        assert!(project.covers(&DeputyScope::Project("p-1".into())));
        assert!(!project.covers(&DeputyScope::Project("p-2".into())));
        assert!(!DeputyScope::Escalations.covers(&DeputyScope::All));
    }

    #[test]
    fn an_absence_is_half_open() {
        let d = |n: u32| NaiveDate::from_ymd_opt(2026, 10, n).unwrap();
        let a = Absence {
            id: "a".into(),
            user_id: "u".into(),
            valid_from: d(5),
            valid_to: Some(d(8)),
            kind: AbsenceKind::Leave,
            source: SOURCE_MANUAL.into(),
            created_by: None,
        };
        assert!(!a.covers(d(4)) && a.covers(d(5)) && a.covers(d(7)) && !a.covers(d(8)));
    }
}
