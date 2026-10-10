//! Read side of the org structure: everything answered "as of" a day.
//!
//! One `Snapshot` (five queries, whatever the size of the organization) holds
//! the structure valid on that day plus the indexes the questions need, so a
//! chain, a subtree or a whole tree costs one load and no per-node query. The
//! projection into `sync_user_org_profiles` reads through the same snapshot,
//! which keeps "who is whose manager" defined in one place.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::Serialize;

use super::error::{OrgStructureError as E, Result};
use super::replication as repl;
use super::repo::{
    fmt, map_assignment, map_deputy, map_line, map_position, map_unit, select_where, timezone_of,
    VALID_AT,
};
use super::types::*;
use super::validate;
use crate::db::DbPool;

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// The structure of one organization on one day, indexed for traversal.
pub struct Snapshot {
    pub at: NaiveDate,
    pub units: Vec<Unit>,
    pub positions: Vec<Position>,
    pub lines: Vec<ReportingLine>,
    pub assignments: Vec<Assignment>,
    pub deputy_heads: Vec<DeputyHead>,
    position_ix: HashMap<String, usize>,
    unit_ix: HashMap<String, usize>,
    /// Primary line: position -> the position it reports to.
    primary_parent: HashMap<String, String>,
    /// Functional lines of a position, in priority order.
    functional_parents: HashMap<String, Vec<String>>,
    /// Primary line, reversed: position -> positions reporting to it.
    children: HashMap<String, Vec<String>>,
    /// Position -> indexes into `assignments`, most relevant holder first.
    holders: HashMap<String, Vec<usize>>,
    /// User -> indexes into `assignments`, primary assignment first.
    by_user: HashMap<String, Vec<usize>>,
}

/// Which of several holders of one position (or several assignments of one
/// person) comes first: the primary one, then the bigger share, then the older
/// one. Deterministic, so two nodes never project different managers.
fn relevance(a: &Assignment, b: &Assignment) -> std::cmp::Ordering {
    b.is_primary
        .cmp(&a.is_primary)
        .then(b.share.total_cmp(&a.share))
        .then(a.valid_from.cmp(&b.valid_from))
        .then(a.id.cmp(&b.id))
}

impl Snapshot {
    pub fn load(conn: &Connection, org_id: &str, at: NaiveDate) -> Result<Self> {
        let params = [org_id, &fmt(at)];
        let units = select_where(
            conn,
            &repl::UNITS,
            &format!("{VALID_AT} ORDER BY name, unit_id"),
            params,
            map_unit,
        )?;
        let positions = select_where(
            conn,
            &repl::POSITIONS,
            &format!("{VALID_AT} ORDER BY name, position_id"),
            params,
            map_position,
        )?;
        let lines = select_where(
            conn,
            &repl::REPORTING_LINES,
            &format!("{VALID_AT} ORDER BY position_id, kind, priority, id"),
            params,
            map_line,
        )?;
        let assignments = select_where(
            conn,
            &repl::ASSIGNMENTS,
            &format!("{VALID_AT} ORDER BY position_id, id"),
            params,
            map_assignment,
        )?;
        let deputy_heads = select_where(
            conn,
            &repl::DEPUTY_HEADS,
            &format!("{VALID_AT} ORDER BY unit_id, ord"),
            params,
            map_deputy,
        )?;
        Ok(Self::index(
            at,
            units,
            positions,
            lines,
            assignments,
            deputy_heads,
        ))
    }

    fn index(
        at: NaiveDate,
        units: Vec<Unit>,
        positions: Vec<Position>,
        lines: Vec<ReportingLine>,
        assignments: Vec<Assignment>,
        deputy_heads: Vec<DeputyHead>,
    ) -> Self {
        let position_ix: HashMap<String, usize> = positions
            .iter()
            .enumerate()
            .map(|(i, p)| (p.position_id.clone(), i))
            .collect();

        let unit_ix: HashMap<String, usize> = units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.unit_id.clone(), i))
            .collect();

        let mut primary_parent = HashMap::new();
        let mut functional_parents: HashMap<String, Vec<String>> = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for line in &lines {
            // A line to a position that is not valid on the day leads nowhere.
            if !position_ix.contains_key(&line.position_id)
                || !position_ix.contains_key(&line.parent_position_id)
            {
                continue;
            }
            match line.kind {
                LineKind::Primary => {
                    primary_parent
                        .insert(line.position_id.clone(), line.parent_position_id.clone());
                    children
                        .entry(line.parent_position_id.clone())
                        .or_default()
                        .push(line.position_id.clone());
                }
                LineKind::Functional => functional_parents
                    .entry(line.position_id.clone())
                    .or_default()
                    .push(line.parent_position_id.clone()),
            }
        }

        let mut holders: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_user: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, a) in assignments.iter().enumerate() {
            if !position_ix.contains_key(&a.position_id) {
                continue;
            }
            holders.entry(a.position_id.clone()).or_default().push(i);
            if let Subject::User(user_id) = &a.subject {
                by_user.entry(user_id.clone()).or_default().push(i);
            }
        }
        for list in holders.values_mut().chain(by_user.values_mut()) {
            list.sort_by(|&x, &y| relevance(&assignments[x], &assignments[y]));
        }

        Self {
            at,
            units,
            positions,
            lines,
            assignments,
            deputy_heads,
            position_ix,
            unit_ix,
            primary_parent,
            functional_parents,
            children,
            holders,
            by_user,
        }
    }

    pub fn position(&self, position_id: &str) -> Option<&Position> {
        self.position_ix
            .get(position_id)
            .map(|&i| &self.positions[i])
    }

    pub fn unit(&self, unit_id: &str) -> Option<&Unit> {
        self.unit_ix.get(unit_id).map(|&i| &self.units[i])
    }

    /// Holders of a position on the day, most relevant first. Empty = vacancy.
    pub fn holders_of(&self, position_id: &str) -> impl Iterator<Item = &Assignment> {
        self.holders
            .get(position_id)
            .into_iter()
            .flatten()
            .map(|&i| &self.assignments[i])
    }

    /// Everything a user holds on the day, primary assignment first.
    pub fn assignments_of(&self, user_id: &str) -> impl Iterator<Item = &Assignment> {
        self.by_user
            .get(user_id)
            .into_iter()
            .flatten()
            .map(|&i| &self.assignments[i])
    }

    /// The assignment that stands for the person in the projection: the one
    /// marked primary, and when none is (the administrator has not chosen yet)
    /// the biggest share, so a person never loses access for lack of a mark.
    pub fn primary_assignment(&self, user_id: &str) -> Option<&Assignment> {
        self.assignments_of(user_id).next()
    }

    pub fn users(&self) -> impl Iterator<Item = &str> {
        self.by_user.keys().map(String::as_str)
    }

    pub fn primary_parent_of(&self, position_id: &str) -> Option<&str> {
        self.primary_parent.get(position_id).map(String::as_str)
    }

    pub fn is_head(&self, position: &Position) -> bool {
        self.unit(&position.unit_id)
            .is_some_and(|u| u.head_position_id.as_deref() == Some(&position.position_id))
    }

    /// Positions above `position_id` on the primary line, nearest first.
    /// Functional lines are not followed. A cycle (validation forbids it, but a
    /// replicated bad row must not hang a reader) ends the walk.
    fn ancestors(&self, position_id: &str) -> Vec<&str> {
        let mut chain = Vec::new();
        let mut seen: HashSet<&str> = HashSet::from([position_id]);
        let mut current = position_id;
        while let Some(parent) = self.primary_parent.get(current) {
            if !seen.insert(parent.as_str()) {
                break;
            }
            chain.push(parent.as_str());
            current = parent;
        }
        chain
    }

    /// Positions below the given ones on the primary line, nearest first, with
    /// their depth (1 = direct report). A staff position reports to its manager
    /// but is never a manager itself, so the walk does not descend from it.
    fn descendants(&self, roots: &[&str], transitive: bool) -> Vec<(&str, u32)> {
        let mut out = Vec::new();
        let mut seen: HashSet<&str> = roots.iter().copied().collect();
        let mut queue: VecDeque<(&str, u32)> = roots.iter().map(|r| (*r, 0)).collect();
        while let Some((current, depth)) = queue.pop_front() {
            if depth > 0 && !transitive {
                continue;
            }
            if self.position(current).is_some_and(|p| p.is_staff) {
                continue;
            }
            for child in self.children.get(current).into_iter().flatten() {
                if seen.insert(child.as_str()) {
                    out.push((child.as_str(), depth + 1));
                    queue.push_back((child.as_str(), depth + 1));
                }
            }
        }
        out
    }

    fn link(&self, position_id: &str, depth: u32) -> Option<Link> {
        let position = self.position(position_id)?;
        Some(Link {
            position_id: position.position_id.clone(),
            unit_id: position.unit_id.clone(),
            name: position.name.clone(),
            depth,
            holders: self
                .holders_of(position_id)
                .map(|a| a.subject.clone())
                .collect(),
        })
    }

    /// The person `user_id` reports to on the day, by the STRUCTURE alone: the
    /// holder of the position above on the primary line. When that position is
    /// vacant and is the head of its unit, the unit's deputy heads stand in, in
    /// their order (docs §1.1: a vacant head is an absent head).
    ///
    /// Absences do not enter here: this is the manager the permission
    /// projection writes, and a week of leave must not move anybody's rights.
    /// `escalation::effective_manager` adds presence and temporary deputies.
    pub fn manager_of(&self, user_id: &str) -> Option<Manager> {
        let own = self.primary_assignment(user_id)?;
        let mut position = own.position_id.as_str();
        let mut seen: HashSet<&str> = HashSet::from([position]);
        loop {
            let parent = self.primary_parent.get(position)?.as_str();
            if !seen.insert(parent) {
                return None;
            }
            let mut any_holder = false;
            let mut self_holds = false;
            for holder in self.holders_of(parent) {
                any_holder = true;
                if let Subject::User(holder_id) = &holder.subject {
                    if holder_id == user_id {
                        self_holds = true;
                    } else {
                        return Some(Manager {
                            user_id: holder_id.clone(),
                            position_id: parent.to_string(),
                            source: ManagerSource::PrimaryHolder,
                        });
                    }
                }
            }
            if !any_holder {
                return self.deputy_head_of_vacancy(parent, user_id);
            }
            // Holders without an account end the search. A person is not their
            // own manager, though: when they hold the parent themselves (two
            // positions in one chain) the next one up is the one to ask.
            if !self_holds {
                return None;
            }
            position = parent;
        }
    }

    /// The first deputy head, in order, of the unit `head_position` heads that
    /// has an account holder other than `requester`. `None` when the position
    /// is not a unit head, or the requester is a deputy head of that unit
    /// (a deputy standing in for the head would become their own peer's
    /// report).
    fn deputy_head_of_vacancy(&self, head_position: &str, requester: &str) -> Option<Manager> {
        let head = self.position(head_position)?;
        if !self.is_head(head) {
            return None;
        }
        let deputies: Vec<&DeputyHead> = self
            .deputy_heads
            .iter()
            .filter(|d| d.unit_id == head.unit_id)
            .collect();
        let holds = |position: &str, user: &str| {
            self.holders_of(position)
                .any(|h| matches!(&h.subject, Subject::User(id) if id == user))
        };
        if deputies.iter().any(|d| holds(&d.position_id, requester)) {
            return None;
        }
        deputies.iter().find_map(|d| {
            self.holders_of(&d.position_id)
                .find_map(|h| match &h.subject {
                    Subject::User(id) if id != requester => Some(Manager {
                        user_id: id.clone(),
                        position_id: d.position_id.clone(),
                        source: ManagerSource::DeputyHead,
                    }),
                    _ => None,
                })
        })
    }

    /// True when `user_id` sits below `manager_id`. With `Primary` this follows
    /// exactly the manager chain the projection writes, so it agrees with the
    /// `manager_subtree` permission; `All` goes through any seat either person
    /// holds (an acting head manages that unit's people).
    ///
    /// WP9's `can_view_person_data` consumes this with `Primary`.
    pub fn is_subordinate_of(&self, user_id: &str, manager_id: &str, scope: SeatScope) -> bool {
        if user_id == manager_id {
            return false;
        }
        match scope {
            SeatScope::Primary => {
                let mut seen: HashSet<String> = HashSet::from([user_id.to_string()]);
                let mut current = user_id.to_string();
                while let Some(manager) = self.manager_of(&current) {
                    if manager.user_id == manager_id {
                        return true;
                    }
                    if !seen.insert(manager.user_id.clone()) {
                        return false;
                    }
                    current = manager.user_id;
                }
                false
            }
            SeatScope::All => {
                let managed_by_manager = |position: &str| {
                    self.position(position).is_some_and(|p| !p.is_staff)
                        && self
                            .holders_of(position)
                            .any(|h| matches!(&h.subject, Subject::User(id) if id == manager_id))
                };
                self.assignments_of(user_id).any(|a| {
                    self.ancestors(&a.position_id)
                        .into_iter()
                        .any(managed_by_manager)
                })
            }
        }
    }

    fn start_positions(&self, target: &Target, scope: SeatScope) -> Result<Vec<&str>> {
        match target {
            Target::Position(id) => self
                .position(id)
                .map(|p| vec![p.position_id.as_str()])
                .ok_or_else(|| E::NotValidAt {
                    entity: "position",
                    id: id.clone(),
                    date: fmt(self.at),
                }),
            Target::User(id) => Ok(self
                .assignments_of(id)
                .take(match scope {
                    SeatScope::Primary => 1,
                    SeatScope::All => usize::MAX,
                })
                .map(|a| a.position_id.as_str())
                .collect()),
        }
    }
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    User(String),
    Position(String),
}

/// Which seats of a person a question looks through. `Primary` is what the
/// permission checks grant (the projection follows the primary assignment
/// only); `All` also counts secondary and acting seats, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatScope {
    Primary,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// One position met on a chain or in a subtree. `holders` is empty for a
/// vacancy: the position is still part of the line, nobody sits on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Link {
    pub position_id: String,
    pub unit_id: String,
    pub name: String,
    /// Steps from the starting position (1 = the next one up or down).
    pub depth: u32,
    pub holders: Vec<Subject>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagerSource {
    /// The person holding the position at the head of the primary line.
    PrimaryHolder,
    /// The position above is a vacant unit head; a deputy head of the unit
    /// stands in.
    DeputyHead,
    /// The holder is away and their deputy (scope `all`) stands in.
    Deputy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manager {
    pub user_id: String,
    /// The position the manager holds in this relation.
    pub position_id: String,
    pub source: ManagerSource,
}

/// A user's assignments on a day, the primary one apart.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserAssignments {
    pub primary: Option<Assignment>,
    pub others: Vec<Assignment>,
}

/// Resolves an optional day to the organization's today.
fn day_or_today(conn: &Connection, org_id: &str, at: Option<NaiveDate>) -> Result<NaiveDate> {
    match at {
        Some(day) => Ok(day),
        None => validate::today_in_zone(&timezone_of(conn, org_id)?),
    }
}

fn snapshot(pool: &DbPool, org_id: &str, at: Option<NaiveDate>) -> Result<Snapshot> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let day = day_or_today(&conn, org_id, at)?;
    Snapshot::load(&conn, org_id, day)
}

pub fn get_assignment(
    pool: &DbPool,
    org_id: &str,
    user_id: &str,
    at: Option<NaiveDate>,
) -> Result<UserAssignments> {
    let snap = snapshot(pool, org_id, at)?;
    let mut all = snap.assignments_of(user_id).cloned();
    let primary = all.next();
    Ok(UserAssignments {
        primary,
        others: all.collect(),
    })
}

/// Up: the positions above the target on the primary line (a user is asked
/// from their primary position). Down: everything below it, on the seats
/// `scope` selects. Functional lines are never part of a chain.
pub fn get_reports_chain(
    pool: &DbPool,
    org_id: &str,
    target: &Target,
    direction: Direction,
    scope: SeatScope,
    at: Option<NaiveDate>,
) -> Result<Vec<Link>> {
    let snap = snapshot(pool, org_id, at)?;
    match direction {
        Direction::Down => links_below(&snap, target, true, scope),
        Direction::Up => {
            let start = match target {
                Target::Position(_) => snap
                    .start_positions(target, SeatScope::Primary)?
                    .into_iter()
                    .next(),
                Target::User(id) => snap.primary_assignment(id).map(|a| a.position_id.as_str()),
            };
            let Some(start) = start else {
                return Ok(Vec::new());
            };
            Ok(snap
                .ancestors(start)
                .into_iter()
                .enumerate()
                .filter_map(|(i, id)| snap.link(id, i as u32 + 1))
                .collect())
        }
    }
}

fn links_below(
    snap: &Snapshot,
    target: &Target,
    transitive: bool,
    scope: SeatScope,
) -> Result<Vec<Link>> {
    let roots = snap.start_positions(target, scope)?;
    Ok(snap
        .descendants(&roots, transitive)
        .into_iter()
        .filter_map(|(id, depth)| snap.link(id, depth))
        .collect())
}

pub fn get_subordinates(
    pool: &DbPool,
    org_id: &str,
    target: &Target,
    transitive: bool,
    scope: SeatScope,
    at: Option<NaiveDate>,
) -> Result<Vec<Link>> {
    let snap = snapshot(pool, org_id, at)?;
    links_below(&snap, target, transitive, scope)
}

pub fn is_subordinate_of(
    pool: &DbPool,
    org_id: &str,
    user_id: &str,
    manager_id: &str,
    scope: SeatScope,
    at: Option<NaiveDate>,
) -> Result<bool> {
    Ok(snapshot(pool, org_id, at)?.is_subordinate_of(user_id, manager_id, scope))
}

// ---------------------------------------------------------------------------
// Whole structure
// ---------------------------------------------------------------------------

/// A person as the structure shows them. Contact data is deliberately absent:
/// the structure is readable by everyone in the organization (docs §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub subject: Subject,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnitView {
    #[serde(flatten)]
    pub unit: Unit,
    /// Deputy heads of the unit, in order.
    pub deputy_head_position_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PositionView {
    #[serde(flatten)]
    pub position: Position,
    pub primary_parent_position_id: Option<String>,
    pub functional_parent_position_ids: Vec<String>,
    pub is_head: bool,
    pub is_vacant: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssignmentView {
    #[serde(flatten)]
    pub assignment: Assignment,
    pub person: Person,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StructureView {
    pub at: NaiveDate,
    pub timezone: String,
    pub units: Vec<UnitView>,
    pub positions: Vec<PositionView>,
    pub assignments: Vec<AssignmentView>,
    /// Positions nobody holds on the day.
    pub vacancies: Vec<String>,
    pub warnings: Vec<Warning>,
}

/// The whole structure on a day: units, positions with their lines, who holds
/// what, the vacancies and what the administrator should look at.
pub fn structure_as_of(
    pool: &DbPool,
    org_id: &str,
    at: Option<NaiveDate>,
) -> Result<StructureView> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    structure_in(&conn, org_id, at)
}

/// `structure_as_of` on a connection the caller already holds — the import
/// previews the structure its still uncommitted transaction would leave.
pub(super) fn structure_in(
    conn: &Connection,
    org_id: &str,
    at: Option<NaiveDate>,
) -> Result<StructureView> {
    let timezone = timezone_of(conn, org_id)?;
    let day = match at {
        Some(day) => day,
        None => validate::today_in_zone(&timezone)?,
    };
    let snap = Snapshot::load(conn, org_id, day)?;
    let names = person_names(conn, org_id)?;

    let mut deputies: HashMap<&str, Vec<&str>> = HashMap::new();
    for d in &snap.deputy_heads {
        deputies
            .entry(d.unit_id.as_str())
            .or_default()
            .push(d.position_id.as_str());
    }
    let units = snap
        .units
        .iter()
        .map(|unit| UnitView {
            unit: unit.clone(),
            deputy_head_position_ids: deputies
                .get(unit.unit_id.as_str())
                .map(|ids| ids.iter().map(|id| id.to_string()).collect())
                .unwrap_or_default(),
        })
        .collect();

    let mut vacancies = Vec::new();
    let positions = snap
        .positions
        .iter()
        .map(|position| {
            let is_vacant = snap.holders_of(&position.position_id).next().is_none();
            if is_vacant {
                vacancies.push(position.position_id.clone());
            }
            PositionView {
                position: position.clone(),
                primary_parent_position_id: snap
                    .primary_parent_of(&position.position_id)
                    .map(str::to_string),
                functional_parent_position_ids: snap
                    .functional_parents
                    .get(&position.position_id)
                    .cloned()
                    .unwrap_or_default(),
                is_head: snap.is_head(position),
                is_vacant,
            }
        })
        .collect();

    let assignments = snap
        .assignments
        .iter()
        .filter(|a| snap.position(&a.position_id).is_some())
        .map(|a| AssignmentView {
            assignment: a.clone(),
            person: Person {
                display_name: names.get(&a.subject).cloned().unwrap_or_default(),
                subject: a.subject.clone(),
            },
        })
        .collect();

    Ok(StructureView {
        at: day,
        timezone,
        units,
        positions,
        assignments,
        vacancies,
        warnings: warnings_of(&snap),
    })
}

/// Display names of the organization's members and of its external persons.
fn person_names(conn: &Connection, org_id: &str) -> Result<HashMap<Subject, String>> {
    let mut names = HashMap::new();
    let mut users = conn.prepare(
        "SELECT u.id, COALESCE(NULLIF(u.display_name, ''), u.username) \
         FROM user_accounts u JOIN org_memberships m ON m.user_id = u.id \
         WHERE m.org_id = ?1",
    )?;
    for row in users.query_map([org_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        names.insert(Subject::User(id), name);
    }
    let mut external =
        conn.prepare("SELECT id, display_name FROM org_external_persons WHERE org_id = ?1")?;
    for row in external.query_map([org_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        names.insert(Subject::External(id), name);
    }
    Ok(names)
}

fn warnings_of(snap: &Snapshot) -> Vec<Warning> {
    let mut warnings = Vec::new();
    for unit in snap.units.iter().filter(|u| u.head_position_id.is_none()) {
        warnings.push(Warning::UnitWithoutHead {
            unit_id: unit.unit_id.clone(),
            from: snap.at,
        });
    }

    let mut users: Vec<&str> = snap.users().collect();
    users.sort_unstable();
    for user_id in users {
        let subject = Subject::User(user_id.to_string());
        let held: Vec<&Assignment> = snap.assignments_of(user_id).collect();
        let total: f64 = held.iter().map(|a| a.share).sum();
        if validate::share_exceeds_full_time(total) {
            warnings.push(Warning::ShareOverbooked {
                subject: subject.clone(),
                from: snap.at,
                total,
            });
        }
        if held.len() > 1 && !held.iter().any(|a| a.is_primary) {
            warnings.push(Warning::PersonWithoutPrimary {
                subject,
                from: snap.at,
            });
        }
    }
    warnings
}
