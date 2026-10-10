//! Handover of everything a person holds (docs/ORG_STRUCTURE_PLAN.md §2.6,
//! docs/PROJECT_STUDIO_WORKFLOW_PLAN.md §4.5, mockup F07).
//!
//! Three reasons share one mechanism. A DEPARTURE is permanent and needs
//! `org.admin`; an ABSENCE is temporary (the work comes back on the return day
//! unless the taker closed or changed it) and is done by the person, their
//! manager on the primary line or an administrator; a PROJECT REMOVAL moves only
//! the items of one project and needs the manager role there.
//!
//! What a person holds is asked of PROVIDERS (`HandoverProvider`), one per kind
//! of thing, each with a proposal for who should take it (`advice`):
//! Project Studio tasks, open test-run items and memberships (`project_work`),
//! and positions and deputies of the structure (`org_items`). Only what is
//! really stored is offered — no category exists without rows behind it.
//!
//! ATOMICITY. The structure lives in one database and Project Studio in one
//! database per project, so a handover cannot be one transaction. What holds:
//! the record of the handover (`record`, with the state of every item) is written
//! first; all org items go in ONE organization transaction together with their
//! record state, and if any of them is refused none is applied and nothing else
//! starts; the Project Studio items then go one by one, each in one conditional
//! statement, and a failure of one is recorded and does not stop the rest. The
//! answer says exactly what became of every item, and `retry` finishes the
//! failed ones from the record. A step repeated after a crash finds its work
//! already moved and reports it done, so the whole thing is safe to repeat.
//!
//! An absence handover is put back by `due::run_due` (a nightly loop of every
//! node, like the profile recompute): on the return day each `done` item whose
//! taker still holds it, unfinished, goes back to the person; the rest is
//! `kept`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

use super::availability;
use super::error::{OrgStructureError as E, Result};
use super::query::Snapshot;
use super::repo::{self, BatchMode, BatchOutcome, Session};
use super::validate::{self, format_date};
use super::WriteCtx;
use crate::db::DbPool;
use crate::project_studio::notifications;
use tentaflow_protocol::project_studio::access::ProjectArea;

mod advice;
mod due;
mod org_items;
mod project_work;
mod record;

pub use due::{run_due, start, DueReport};
pub(crate) use project_work::{remove_project_member, MemberRemoval};

use advice::Advice;
use org_items::{DeputyProvider, PositionProvider};
use project_work::{MembershipProvider, ProjectDirectory, TaskProvider, TestItemProvider};

/// Longest note a handover takes; it is stored, sent to every taker and put on
/// every task as a comment.
pub const MAX_NOTE_CHARS: usize = 4000;

/// Most items one apply takes: every choice is looked up and written to the
/// record, so an unbounded list is a way to pin a worker.
pub const MAX_CHOICES: usize = 2000;

/// Most people `pending_people` looks at: each one costs a read of their items.
const MAX_PENDING_PEOPLE: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Departure,
    Absence,
    ProjectRemoval,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Departure => "departure",
            Self::Absence => "absence",
            Self::ProjectRemoval => "project_removal",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "departure" => Some(Self::Departure),
            "absence" => Some(Self::Absence),
            "project_removal" => Some(Self::ProjectRemoval),
            _ => None,
        }
    }
}

/// Where an item lives; the order of the variants is the order of the groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Task,
    TestItem,
    Membership,
    Position,
    Deputy,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::TestItem => "test_item",
            Self::Membership => "membership",
            Self::Position => "position",
            Self::Deputy => "deputy",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "task" => Some(Self::Task),
            "test_item" => Some(Self::TestItem),
            "membership" => Some(Self::Membership),
            "position" => Some(Self::Position),
            "deputy" => Some(Self::Deputy),
            _ => None,
        }
    }

    const ALL: [Category; 5] = [
        Self::Task,
        Self::TestItem,
        Self::Membership,
        Self::Position,
        Self::Deputy,
    ];

    /// True for what lives in the structure's own database.
    fn is_org(self) -> bool {
        matches!(self, Self::Position | Self::Deputy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A taker is required.
    Transfer,
    /// With a taker the item goes to them; without one it ends.
    TransferOrEnd,
    /// The item ends; nobody takes it.
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub user_id: String,
    pub reason: &'static str,
}

impl Suggestion {
    pub(super) fn new(user_id: &str, reason: &'static str) -> Self {
        Self {
            user_id: user_id.to_string(),
            reason,
        }
    }
}

/// One thing the person holds.
#[derive(Debug, Clone)]
pub struct Held {
    pub key: String,
    pub category: Category,
    pub title: String,
    pub role: String,
    pub state: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub unit_name: Option<String>,
    pub valid_to: Option<String>,
    pub action: Action,
    pub suggestion: Option<Suggestion>,
    /// The only people who may take it; `None` = any active member.
    pub eligible: Option<Vec<String>>,
    pub blocked: Option<&'static str>,
}

/// What the providers read from.
pub(super) struct ListCx<'a> {
    pub conn: &'a Connection,
    pub org_id: &'a str,
    pub user_id: &'a str,
    /// Who asks: an absence lists only what the ACTOR may see in Project Studio.
    pub actor: &'a str,
    pub reason: Reason,
    pub date: NaiveDate,
    /// The one project of a project removal.
    pub project: Option<&'a str>,
    pub advice: &'a Advice,
    pub names: &'a HashMap<String, String>,
    /// Active members of the organization.
    pub members: &'a HashSet<String>,
    /// Names of projects a provider had to leave out of the listing.
    pub skipped: &'a RefCell<Vec<String>>,
}

/// What an item needs to be moved.
#[derive(Debug, Clone)]
pub(super) struct Planned {
    pub key: String,
    pub category: Category,
    pub title: String,
    pub project_id: Option<String>,
    pub taker: Option<String>,
}

/// What an item needs to be put back or finished later.
#[derive(Debug, Clone)]
pub(super) struct Recorded {
    pub key: String,
    pub from_user: String,
    pub taker: Option<String>,
}

pub(super) struct ApplyCx<'a> {
    pub org_id: &'a str,
    pub actor: &'a str,
    pub actor_is_admin: bool,
    pub handover_id: &'a str,
    pub from_user: &'a str,
    pub reason: Reason,
    pub date: NaiveDate,
    pub today: NaiveDate,
    pub note: &'a str,
    pub note_digest: &'a str,
}

/// What became of a step that did not fail.
pub(super) enum Step {
    Done(Json),
    /// Waits for its day (a membership that ends on the departure date).
    Scheduled(Json),
    Skipped(&'static str),
    Refused(&'static str),
}

pub(super) enum Returned {
    Back,
    Kept(&'static str),
}

pub(super) trait HandoverProvider {
    fn category(&self) -> Category;
    /// What `cx.user_id` holds in this store, with a proposed taker for each.
    fn list(&self, cx: &ListCx<'_>) -> Result<Vec<Held>>;
}

/// A store that is not the structure: one conditional step per item.
pub(super) trait WorkProvider: HandoverProvider {
    fn apply(&self, cx: &ApplyCx<'_>, item: &Planned) -> Result<Step>;
    /// Puts a temporarily handed item back, unless the taker closed or changed it.
    fn reverse(&self, cx: &ApplyCx<'_>, item: &Recorded) -> Result<Returned>;
    /// Finishes an item that was `scheduled` for a day that has come.
    fn complete(&self, _cx: &ApplyCx<'_>, _item: &Recorded) -> Result<Step> {
        Ok(Step::Skipped("nothing_scheduled"))
    }
}

/// The structure: applied inside the one organization transaction.
pub(super) trait OrgProvider: HandoverProvider {
    fn apply_in(&self, s: &mut Session<'_>, cx: &ApplyCx<'_>, item: &Planned) -> Result<()>;
}

// =============================================================================
// Requests and answers
// =============================================================================

/// Who asks. `is_admin` is `org.admin` in the organization.
#[derive(Debug, Clone, Copy)]
pub struct Actor<'a> {
    pub user_id: &'a str,
    pub is_admin: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub user_id: &'a str,
    pub reason: Reason,
    pub project_id: Option<&'a str>,
    pub date: Option<NaiveDate>,
    pub return_date: Option<NaiveDate>,
}

#[derive(Debug)]
pub struct Listing {
    pub user_name: String,
    pub reason: Reason,
    pub date: NaiveDate,
    pub return_date: Option<NaiveDate>,
    pub assignment_ended_on: Option<NaiveDate>,
    pub project_name: Option<String>,
    pub groups: Vec<(Category, Vec<Held>)>,
    /// Active members but the person: who may take something.
    pub takers: Vec<(String, String)>,
    /// Projects left out because their task index did not settle.
    pub skipped_projects: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Choice {
    pub key: String,
    pub taker_user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ItemResult {
    pub key: String,
    pub category: Category,
    pub title: String,
    /// `done`, `scheduled`, `failed`, `skipped`, `not_started`, `returned`, `kept`, `pending`.
    pub status: String,
    pub reason: Option<String>,
    pub taker_user_id: Option<String>,
    pub project_name: Option<String>,
}

#[derive(Debug)]
pub struct Applied {
    pub handover_id: Option<String>,
    pub items: Vec<ItemResult>,
    /// Set when nothing was moved because the request itself was refused.
    pub refused: Option<E>,
}

#[derive(Debug)]
pub struct HandoverRecord {
    pub id: String,
    pub user_id: String,
    pub reason: Reason,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub date: NaiveDate,
    pub return_date: Option<NaiveDate>,
    pub note: String,
    pub created_by: String,
    pub created_at_ms: i64,
    pub items: Vec<ItemResult>,
}

#[derive(Debug)]
pub struct Pending {
    pub user_id: String,
    pub count: u32,
    pub ended_on: Option<NaiveDate>,
}

// =============================================================================
// Authorization and gathering
// =============================================================================

fn today_of(conn: &Connection, org_id: &str) -> Result<NaiveDate> {
    validate::today_in_zone(&repo::timezone_of(conn, org_id)?)
}

fn is_member(conn: &Connection, org_id: &str, user_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_memberships WHERE org_id = ?1 AND user_id = ?2)",
        [org_id, user_id],
        |r| r.get(0),
    )?)
}

fn active_members(conn: &Connection, org_id: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT u.id FROM user_accounts u JOIN org_memberships m ON m.user_id = u.id \
         WHERE m.org_id = ?1 AND u.is_active = 1",
    )?;
    let ids = stmt
        .query_map([org_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(ids)
}

/// The person's manager on the primary line today.
fn is_manager_of(
    conn: &Connection,
    org_id: &str,
    manager: &str,
    person: &str,
    today: NaiveDate,
) -> Result<bool> {
    let snap = Snapshot::load(conn, org_id, today)?;
    Ok(snap
        .manager_of(person)
        .is_some_and(|m| m.user_id == manager))
}

/// Departure: an administrator. Project removal: the manager role in that
/// project (an administrator has no special right there: Project Studio lets
/// administrators only inspect and take over orphans, never edit content).
/// Absence: the person, their manager or an administrator.
fn authorize(
    conn: &Connection,
    org_id: &str,
    actor: &Actor<'_>,
    subject: &Target<'_>,
    today: NaiveDate,
) -> Result<()> {
    match subject.reason {
        Reason::Departure => {
            if !actor.is_admin {
                return Err(E::NotPermitted(
                    "only an administrator hands over a departure",
                ));
            }
        }
        Reason::ProjectRemoval => {
            let project = subject.project_id.ok_or(E::EmptyField {
                field: "project_id",
            })?;
            if !ProjectDirectory::available() {
                return Err(E::NotFound {
                    entity: "project",
                    id: project.to_string(),
                });
            }
            let record = crate::project_studio::repository::get_project(org_id, project)?
                .ok_or_else(|| E::NotFound {
                    entity: "project",
                    id: project.to_string(),
                })?;
            let access =
                crate::project_studio::repository::project_access(&record, actor.user_id, false)?;
            if !access.has_access {
                return Err(E::NotFound {
                    entity: "project",
                    id: project.to_string(),
                });
            }
            if !access.can_manage_members {
                return Err(E::NotPermitted(
                    "only a project administrator removes a member and hands over",
                ));
            }
        }
        Reason::Absence => {
            let allowed = actor.user_id == subject.user_id
                || actor.is_admin
                || is_manager_of(conn, org_id, actor.user_id, subject.user_id, today)?;
            if !allowed {
                return Err(E::NotPermitted(
                    "only the person, their manager or an administrator hands over an absence",
                ));
            }
        }
    }
    if !is_member(conn, org_id, subject.user_id)? {
        return Err(E::NotFound {
            entity: "user",
            id: subject.user_id.to_string(),
        });
    }
    Ok(())
}

struct Gathered {
    today: NaiveDate,
    date: NaiveDate,
    return_date: Option<NaiveDate>,
    assignment_ended_on: Option<NaiveDate>,
    held: Vec<Held>,
    names: HashMap<String, String>,
    members: HashSet<String>,
    project_name: Option<String>,
    skipped_projects: Vec<String>,
}

fn gather(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    subject: &Target<'_>,
    today: NaiveDate,
) -> Result<Gathered> {
    let names = availability::display_names(pool, org_id)?;
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let ended = advice::last_end(&conn, org_id, subject.user_id, today)?;
    let date = match subject.reason {
        Reason::Departure => subject.date.or(ended).unwrap_or(today),
        Reason::Absence | Reason::ProjectRemoval => today,
    };
    let reference = advice::reference_day(&conn, org_id, subject.user_id, today)?;
    let advice = Advice::load(&conn, org_id, subject.user_id, reference, date)?;
    let members = active_members(&conn, org_id)?;
    let projects = ProjectDirectory::new(org_id, pool);
    let project = (subject.reason == Reason::ProjectRemoval)
        .then_some(subject.project_id)
        .flatten();
    let skipped = RefCell::new(Vec::new());
    let cx = ListCx {
        conn: &conn,
        org_id,
        user_id: subject.user_id,
        actor,
        reason: subject.reason,
        date,
        project,
        advice: &advice,
        names: &names,
        members: &members,
        skipped: &skipped,
    };
    let providers: [Box<dyn HandoverProvider + '_>; 5] = [
        Box::new(TaskProvider {
            projects: &projects,
        }),
        Box::new(TestItemProvider {
            projects: &projects,
        }),
        Box::new(MembershipProvider {
            projects: &projects,
        }),
        Box::new(PositionProvider),
        Box::new(DeputyProvider),
    ];
    let mut held = Vec::new();
    for provider in &providers {
        let mut items = provider.list(&cx)?;
        debug_assert!(items.iter().all(|i| i.category == provider.category()));
        held.append(&mut items);
    }
    held.sort_by(|a, b| {
        (
            a.category,
            a.project_name.as_deref().unwrap_or(""),
            a.title.to_lowercase(),
            &a.key,
        )
            .cmp(&(
                b.category,
                b.project_name.as_deref().unwrap_or(""),
                b.title.to_lowercase(),
                &b.key,
            ))
    });
    let project_name = match project {
        Some(id) => projects.facts(id)?.map(|f| f.name.clone()),
        None => None,
    };
    Ok(Gathered {
        today,
        date,
        return_date: (subject.reason == Reason::Absence)
            .then_some(subject.return_date)
            .flatten(),
        assignment_ended_on: ended,
        held,
        names,
        members,
        project_name,
        skipped_projects: skipped.into_inner(),
    })
}

/// What the person holds for this reason, grouped, with a proposal for each.
pub fn list(
    pool: &DbPool,
    org_id: &str,
    actor: &Actor<'_>,
    subject: &Target<'_>,
) -> Result<Listing> {
    let today = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        let today = today_of(&conn, org_id)?;
        authorize(&conn, org_id, actor, subject, today)?;
        today
    };
    let gathered = gather(pool, org_id, actor.user_id, subject, today)?;
    let mut groups: Vec<(Category, Vec<Held>)> = Vec::new();
    for category in Category::ALL {
        let items: Vec<Held> = gathered
            .held
            .iter()
            .filter(|h| h.category == category)
            .cloned()
            .collect();
        if !items.is_empty() {
            groups.push((category, items));
        }
    }
    let mut takers: Vec<(String, String)> = gathered
        .members
        .iter()
        .filter(|id| id.as_str() != subject.user_id)
        .map(|id| {
            (
                id.clone(),
                gathered.names.get(id).cloned().unwrap_or_default(),
            )
        })
        .collect();
    takers.sort_by(|a, b| {
        a.1.to_lowercase()
            .cmp(&b.1.to_lowercase())
            .then(a.0.cmp(&b.0))
    });
    Ok(Listing {
        user_name: gathered
            .names
            .get(subject.user_id)
            .cloned()
            .unwrap_or_default(),
        reason: subject.reason,
        date: gathered.date,
        return_date: gathered.return_date,
        assignment_ended_on: gathered.assignment_ended_on,
        project_name: gathered.project_name,
        groups,
        takers,
        skipped_projects: gathered.skipped_projects,
    })
}

// =============================================================================
// Apply
// =============================================================================

fn digest(note: &str) -> String {
    hex::encode(Sha256::digest(note.as_bytes()))[..16].to_string()
}

fn result_of(item: &record::Item, project_names: &HashMap<String, String>) -> ItemResult {
    ItemResult {
        key: item.key.clone(),
        category: item.category,
        title: item.title.clone(),
        status: item.status.clone(),
        reason: item.reason.clone(),
        taker_user_id: item.taker_user_id.clone(),
        project_name: item
            .project_id
            .as_ref()
            .and_then(|p| project_names.get(p).cloned()),
    }
}

fn refusal(key: &str, held: Option<&Held>, reason: &str) -> ItemResult {
    ItemResult {
        key: key.to_string(),
        category: held.map_or(Category::Task, |h| h.category),
        title: held.map(|h| h.title.clone()).unwrap_or_default(),
        status: "failed".into(),
        reason: Some(reason.to_string()),
        taker_user_id: None,
        project_name: held.and_then(|h| h.project_name.clone()),
    }
}

/// Checks one choice against what the person holds and who may take it.
fn plan_of(
    held: &Held,
    choice: &Choice,
    user_id: &str,
    members: &HashSet<String>,
) -> std::result::Result<Planned, &'static str> {
    if let Some(blocked) = held.blocked {
        return Err(blocked);
    }
    let taker = choice.taker_user_id.as_deref().filter(|t| !t.is_empty());
    let taker = match (held.action, taker) {
        (Action::Transfer, None) => return Err("taker_required"),
        (Action::End, _) => None,
        (_, taker) => taker,
    };
    if let Some(taker) = taker {
        let allowed = taker != user_id
            && members.contains(taker)
            && held
                .eligible
                .as_ref()
                .is_none_or(|list| list.iter().any(|id| id == taker));
        if !allowed {
            return Err("taker_not_eligible");
        }
    }
    Ok(Planned {
        key: held.key.clone(),
        category: held.category,
        title: held.title.clone(),
        project_id: held.project_id.clone(),
        taker: taker.map(str::to_string),
    })
}

pub struct ApplyRequest<'a> {
    pub subject: Target<'a>,
    pub note: &'a str,
    pub choices: Vec<Choice>,
}

/// Moves the chosen items and records the handover.
pub fn apply(
    pool: &DbPool,
    org_id: &str,
    actor: &Actor<'_>,
    request: &ApplyRequest<'_>,
) -> Result<Applied> {
    let note = request.note.trim();
    if note.is_empty() {
        return Err(E::EmptyField { field: "note" });
    }
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(E::InvalidValue {
            field: "note",
            reason: format!("longer than {MAX_NOTE_CHARS} characters"),
        });
    }
    if request.choices.len() > MAX_CHOICES {
        return Err(E::InvalidValue {
            field: "items",
            reason: format!("more than {MAX_CHOICES} items in one handover"),
        });
    }
    let subject = &request.subject;
    let today = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        let today = today_of(&conn, org_id)?;
        authorize(&conn, org_id, actor, subject, today)?;
        today
    };
    let return_date = match subject.reason {
        Reason::Absence => {
            let back = subject.return_date.ok_or(E::EmptyField {
                field: "return_date",
            })?;
            if back <= today {
                return Err(E::InvalidValue {
                    field: "return_date",
                    reason: "the return day must be after today".into(),
                });
            }
            Some(back)
        }
        _ => None,
    };
    let gathered = gather(pool, org_id, actor.user_id, subject, today)?;
    let held: HashMap<&str, &Held> = gathered.held.iter().map(|h| (h.key.as_str(), h)).collect();

    let mut seen = HashSet::new();
    let mut planned = Vec::new();
    let mut skipped = Vec::new();
    let mut invalid = Vec::new();
    for choice in &request.choices {
        if !seen.insert(choice.key.as_str()) {
            return Err(E::InvalidValue {
                field: "items",
                reason: format!("item {} appears twice", choice.key),
            });
        }
        match held.get(choice.key.as_str()) {
            // Already moved by somebody else, or by an earlier attempt: nothing to do any more.
            None => skipped.push(ItemResult {
                key: choice.key.clone(),
                category: Category::Task,
                title: String::new(),
                status: "skipped".into(),
                reason: Some("no_longer_held".into()),
                taker_user_id: None,
                project_name: None,
            }),
            Some(h) => match plan_of(h, choice, subject.user_id, &gathered.members) {
                Ok(plan) => planned.push(plan),
                Err(reason) => invalid.push(refusal(&choice.key, Some(h), reason)),
            },
        }
    }
    if !invalid.is_empty() {
        return Ok(Applied {
            handover_id: None,
            items: invalid,
            refused: Some(E::InvalidValue {
                field: "items",
                reason: "some choices cannot be applied".into(),
            }),
        });
    }
    if planned.is_empty() {
        return Ok(Applied {
            handover_id: None,
            items: skipped,
            refused: Some(E::InvalidValue {
                field: "items",
                reason: "nothing to hand over".into(),
            }),
        });
    }

    let header = record::Header {
        id: uuid::Uuid::new_v4().to_string(),
        org_id: org_id.to_string(),
        user_id: subject.user_id.to_string(),
        reason: subject.reason,
        project_id: (subject.reason == Reason::ProjectRemoval)
            .then(|| subject.project_id.map(str::to_string))
            .flatten(),
        effective_on: gathered.date,
        return_on: return_date,
        note: note.to_string(),
        created_by: actor.user_id.to_string(),
        created_at_ms: record::now_ms(),
    };
    let recorded: Vec<record::Item> = planned
        .iter()
        .map(|p| record::Item {
            handover_id: header.id.clone(),
            key: p.key.clone(),
            category: p.category,
            project_id: p.project_id.clone(),
            title: p.title.clone(),
            from_user_id: subject.user_id.to_string(),
            taker_user_id: p.taker.clone(),
            status: "pending".into(),
            reason: None,
            detail: json!({}),
        })
        .collect();
    {
        let mut conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
        record::create(&mut conn, &header, &recorded)?;
    }
    let mut items = execute(pool, &header, &planned, actor, gathered.today, false)?;
    items.extend(skipped);
    audit(
        pool,
        actor.user_id,
        "org.handover.apply",
        &header,
        &items,
        planned.len(),
    );
    notify_takers(&header, &items, &gathered.names);
    Ok(Applied {
        handover_id: Some(header.id),
        items,
        refused: None,
    })
}

/// Which items of a record the asker may read: those of projects they have access to and, for an
/// absence, only of the areas the listing would have shown them (a title is task content).
struct ItemGate<'a> {
    projects: &'a ProjectDirectory,
    actor: &'a str,
    reason: Reason,
    known: HashMap<(String, &'static str), bool>,
}

impl<'a> ItemGate<'a> {
    fn new(projects: &'a ProjectDirectory, actor: &'a str, reason: Reason) -> Self {
        Self {
            projects,
            actor,
            reason,
            known: HashMap::new(),
        }
    }

    fn project_visible(&mut self, project: &str) -> Result<bool> {
        self.area_visible(project, "project", |projects, actor| {
            Ok(projects
                .actor_access(project, actor)?
                .is_some_and(|access| access.has_access))
        })
    }

    fn area_visible(
        &mut self,
        project: &str,
        area: &'static str,
        ask: impl FnOnce(&ProjectDirectory, &str) -> Result<bool>,
    ) -> Result<bool> {
        let key = (project.to_string(), area);
        if let Some(known) = self.known.get(&key) {
            return Ok(*known);
        }
        let allowed = ask(self.projects, self.actor)?;
        self.known.insert(key, allowed);
        Ok(allowed)
    }

    fn item_visible(&mut self, item: &record::Item) -> Result<bool> {
        let Some(project) = item.project_id.as_deref() else {
            return Ok(true);
        };
        if !self.project_visible(project)? {
            return Ok(false);
        }
        if self.reason != Reason::Absence {
            return Ok(true);
        }
        let (label, areas): (&'static str, &[ProjectArea]) = match item.category {
            Category::Task => ("tasks", &[ProjectArea::Tasks, ProjectArea::Board]),
            Category::TestItem => ("tests", &[ProjectArea::Tests]),
            _ => return Ok(true),
        };
        self.area_visible(project, label, |projects, actor| {
            projects.actor_reads(project, actor, areas)
        })
    }
}

/// Tries the failed and not yet started items of a recorded handover again. Only
/// the items the caller may read are tried and answered, like in `records`.
pub fn retry(
    pool: &DbPool,
    org_id: &str,
    actor: &Actor<'_>,
    handover_id: &str,
    keys: &[String],
) -> Result<Applied> {
    let (header, items, today) = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        let today = today_of(&conn, org_id)?;
        let header = record::header(&conn, org_id, handover_id)?;
        let subject = Target {
            user_id: &header.user_id,
            reason: header.reason,
            project_id: header.project_id.as_deref(),
            date: None,
            return_date: header.return_on,
        };
        authorize(&conn, org_id, actor, &subject, today)?;
        (header.clone(), record::items(&conn, handover_id)?, today)
    };
    if header.reason == Reason::Absence && header.return_on.is_some_and(|back| back <= today) {
        return Err(E::InvalidValue {
            field: "handover",
            reason: "the return day has passed; the work is put back by the return job".into(),
        });
    }
    let projects = ProjectDirectory::new(org_id, pool);
    let items = visible_items(&projects, actor, &header, items)?;
    let wanted: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let planned: Vec<Planned> = items
        .iter()
        .filter(|i| matches!(i.status.as_str(), "failed" | "pending"))
        .filter(|i| wanted.is_empty() || wanted.contains(i.key.as_str()))
        .map(|i| Planned {
            key: i.key.clone(),
            category: i.category,
            title: i.title.clone(),
            project_id: i.project_id.clone(),
            taker: i.taker_user_id.clone(),
        })
        .collect();
    if planned.is_empty() {
        return Err(E::InvalidValue {
            field: "keys",
            reason: "no failed item to retry".into(),
        });
    }
    let names = availability::display_names(pool, org_id)?;
    let attempted = execute(pool, &header, &planned, actor, today, true)?;
    audit(
        pool,
        actor.user_id,
        "org.handover.retry",
        &header,
        &attempted,
        planned.len(),
    );
    // Takers hear only of what this attempt moved: the items done earlier were announced then.
    notify_takers(&header, &attempted, &names);
    // What did not need a retry is part of the picture too.
    let retried: HashSet<String> = attempted.iter().map(|r| r.key.clone()).collect();
    let project_names = project_names_of(org_id, &items);
    let mut results = attempted;
    results.extend(
        items
            .iter()
            .filter(|i| !retried.contains(&i.key))
            .map(|i| result_of(i, &project_names)),
    );
    Ok(Applied {
        handover_id: Some(header.id),
        items: results,
        refused: None,
    })
}

/// The items of `header` the asker may read. A departure stays whole for the administrator who
/// made it; every other record shows only the work of projects the asker may see, in the areas
/// the listing it came from would have shown.
fn visible_items(
    projects: &ProjectDirectory,
    actor: &Actor<'_>,
    header: &record::Header,
    items: Vec<record::Item>,
) -> Result<Vec<record::Item>> {
    if header.reason == Reason::Departure && actor.is_admin {
        return Ok(items);
    }
    let mut gate = ItemGate::new(projects, actor.user_id, header.reason);
    let mut kept = Vec::with_capacity(items.len());
    for item in items {
        if gate.item_visible(&item)? {
            kept.push(item);
        }
    }
    Ok(kept)
}

fn project_names_of(org_id: &str, items: &[record::Item]) -> HashMap<String, String> {
    let ids: HashSet<&str> = items
        .iter()
        .filter_map(|i| i.project_id.as_deref())
        .collect();
    ids.into_iter()
        .filter_map(|id| {
            crate::project_studio::repository::get_project(org_id, id)
                .ok()
                .flatten()
                .map(|p| (id.to_string(), p.name))
        })
        .collect()
}

/// Runs the items: the structure's in one transaction first, then the stores
/// of Project Studio one by one. Every item ends with a recorded state.
fn execute(
    pool: &DbPool,
    header: &record::Header,
    items: &[Planned],
    actor: &Actor<'_>,
    today: NaiveDate,
    resumed: bool,
) -> Result<Vec<ItemResult>> {
    let digest = digest(&header.note);
    // A first attempt is dated as asked, so a past day is refused like any backdated change; a retry
    // runs on a later day than the request and must not fail on the day having passed.
    let date = if resumed {
        header.effective_on.max(today)
    } else {
        header.effective_on
    };
    let cx = ApplyCx {
        org_id: &header.org_id,
        actor: actor.user_id,
        actor_is_admin: actor.is_admin,
        handover_id: &header.id,
        from_user: &header.user_id,
        reason: header.reason,
        date,
        today,
        note: &header.note,
        note_digest: &digest,
    };
    let projects = ProjectDirectory::new(&header.org_id, pool);
    let mut done: HashMap<String, ItemResult> = HashMap::new();
    let outcome = |item: &Planned, status: &str, reason: Option<&str>| ItemResult {
        key: item.key.clone(),
        category: item.category,
        title: item.title.clone(),
        status: status.to_string(),
        reason: reason.map(str::to_string),
        taker_user_id: item.taker.clone(),
        project_name: item
            .project_id
            .as_deref()
            .and_then(|id| projects.facts(id).ok().flatten())
            .map(|f| f.name.clone()),
    };

    let org_items: Vec<&Planned> = items.iter().filter(|i| i.category.is_org()).collect();
    let mut org_failed = false;
    if !org_items.is_empty() {
        let failures = run_org(pool, &header.org_id, actor.user_id, &cx, &org_items)?;
        org_failed = !failures.is_empty();
        let failed_keys: HashMap<&str, &str> =
            failures.iter().map(|(k, c)| (k.as_str(), *c)).collect();
        for item in &org_items {
            if let Some(code) = failed_keys.get(item.key.as_str()) {
                if !refused_on_retry(resumed, code) {
                    let conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
                    record::set_status(&conn, &header.id, &item.key, "failed", Some(code), None)?;
                }
                done.insert(item.key.clone(), outcome(item, "failed", Some(code)));
            } else if org_failed {
                done.insert(
                    item.key.clone(),
                    outcome(item, "not_started", Some("org_failed")),
                );
            } else {
                done.insert(item.key.clone(), outcome(item, "done", None));
            }
        }
    }

    let mut work: Vec<&Planned> = items.iter().filter(|i| !i.category.is_org()).collect();
    work.sort_by(|a, b| (a.category, &a.key).cmp(&(b.category, &b.key)));
    for item in work {
        if org_failed {
            // The structure refused something: nothing else starts, so the operator fixes it first.
            done.insert(
                item.key.clone(),
                outcome(item, "not_started", Some("org_failed")),
            );
            continue;
        }
        let Some(provider) = project_work::provider_for(item.category, &projects) else {
            continue;
        };
        let step = match provider.apply(&cx, item) {
            Ok(step) => step,
            Err(e) => {
                tracing::warn!(key = %item.key, "handover item failed: {e}");
                Step::Refused(if matches!(e, E::Db(_)) {
                    "internal"
                } else {
                    e.code()
                })
            }
        };
        let (status, reason, detail) = match step {
            Step::Done(detail) => ("done", None, Some(detail)),
            Step::Scheduled(detail) => ("scheduled", None, Some(detail)),
            Step::Skipped(reason) => ("skipped", Some(reason), None),
            Step::Refused(reason) => ("failed", Some(reason), None),
        };
        if !(status == "failed" && reason.is_some_and(|code| refused_on_retry(resumed, code))) {
            let conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
            record::set_status(
                &conn,
                &header.id,
                &item.key,
                status,
                reason,
                detail.as_ref(),
            )?;
        }
        done.insert(item.key.clone(), outcome(item, status, reason));
    }
    Ok(items.iter().filter_map(|i| done.remove(&i.key)).collect())
}

/// A retry by somebody who may not move the item says so in its answer, but the record keeps
/// why the item failed in the first place: the refusal is about the caller, not about the work.
fn refused_on_retry(resumed: bool, code: &str) -> bool {
    resumed && code == "not_permitted"
}

/// All org items in one organization transaction. Returns the items that were
/// refused, with the rule; when there are any, nothing was applied.
fn run_org(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    cx: &ApplyCx<'_>,
    items: &[&Planned],
) -> Result<Vec<(String, &'static str)>> {
    let ctx = WriteCtx {
        org_id,
        actor_user_id: actor,
        confirm_backdated: false,
    };
    repo::run_batch(
        pool,
        &ctx,
        "handover",
        true,
        BatchMode {
            backdating_checked: false,
            allow_withdraw: false,
        },
        |session| {
            let mut failures: Vec<(String, &'static str)> = Vec::new();
            for item in items {
                let applied = match item.category {
                    Category::Position => PositionProvider.apply_in(session, cx, item),
                    _ => DeputyProvider.apply_in(session, cx, item),
                };
                match applied {
                    Ok(()) => {}
                    Err(E::Db(detail)) => return Err(E::Db(detail)),
                    Err(e) => failures.push((item.key.clone(), e.code())),
                }
            }
            let keep = failures.is_empty();
            if keep {
                for item in items {
                    record::set_status(session.tx, cx.handover_id, &item.key, "done", None, None)?;
                }
                session.record_summary(
                    "org.handover.org_items",
                    format!("org_handover:{}", cx.handover_id),
                    json!({
                        "handover_id": cx.handover_id,
                        "user_id": cx.from_user,
                        "reason": cx.reason.as_str(),
                        "items": items.len(),
                        "from": format_date(cx.date),
                    }),
                );
            }
            Ok(BatchOutcome {
                value: failures,
                keep,
            })
        },
    )
}

/// One entry of the audit chain for the whole handover: who moved how much for
/// which reason, and a digest and length of the note (the note itself is in the
/// record, not in the chain).
fn audit(
    pool: &DbPool,
    actor: &str,
    action: &str,
    header: &record::Header,
    results: &[ItemResult],
    attempted: usize,
) {
    let count = |status: &str| results.iter().filter(|r| r.status == status).count();
    let details = json!({
        "org_id": header.org_id,
        "user_id": header.user_id,
        "reason": header.reason.as_str(),
        "project_id": header.project_id,
        "from": format_date(header.effective_on),
        "return_on": header.return_on.map(format_date),
        "attempted": attempted,
        "done": count("done"),
        "scheduled": count("scheduled"),
        "failed": count("failed"),
        "skipped": count("skipped"),
        "note_chars": header.note.chars().count(),
        "note_sha256": digest(&header.note),
    });
    if let Err(e) = crate::db::repository::log_audit(
        pool,
        Some(actor),
        None,
        action,
        Some(&format!("org_handover:{}", header.id)),
        Some(&details.to_string()),
        None,
        None,
    ) {
        tracing::warn!("handover audit entry failed: {e}");
    }
}

/// One notification per taker: how many items came to them, from whom and the note.
fn notify_takers(header: &record::Header, results: &[ItemResult], names: &HashMap<String, String>) {
    let mut per_taker: HashMap<&str, usize> = HashMap::new();
    for result in results
        .iter()
        .filter(|r| matches!(r.status.as_str(), "done" | "scheduled"))
    {
        if let Some(taker) = result.taker_user_id.as_deref() {
            *per_taker.entry(taker).or_default() += 1;
        }
    }
    let from = names.get(&header.user_id).cloned().unwrap_or_default();
    let note: String = header.note.chars().take(300).collect();
    let return_on = header.return_on.map(format_date);
    for (taker, count) in per_taker {
        // The screen words it in the reader's language from `link_json`; title and body are the
        // fallback of a client that does not know the kind.
        let until = return_on
            .as_deref()
            .map(|d| format!(" (until {d})"))
            .unwrap_or_default();
        notifications::notify(
            &header.org_id,
            taker,
            header.project_id.as_deref().unwrap_or(""),
            "work_handed_over",
            "Work handed over to you",
            &format!("{from}: {count} item(s){until}. {note}"),
            &json!({
                "handover_id": header.id,
                "from_name": from,
                "count": count,
                "return_on": return_on,
                "note": note,
            })
            .to_string(),
        );
    }
}

// =============================================================================
// What is still to do, and what was done
// =============================================================================

/// People whose last assignment ended and who still hold something (an item
/// that failed to move, or a membership waiting for its day, is still held).
/// `org.admin` only (the caller checks).
pub fn pending_people(pool: &DbPool, org_id: &str, admin_id: &str) -> Result<Vec<Pending>> {
    let (today, candidates) = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        let today = today_of(&conn, org_id)?;
        let mut stmt = conn.prepare(
            "SELECT a.user_id, MAX(a.valid_to) FROM org_assignments a \
             JOIN org_memberships m ON m.org_id = a.org_id AND m.user_id = a.user_id \
             WHERE a.org_id = ?1 AND a.user_id IS NOT NULL \
             GROUP BY a.user_id \
             HAVING SUM(a.valid_to IS NULL OR a.valid_to > ?2) = 0 \
             ORDER BY MAX(a.valid_to) DESC LIMIT ?3",
        )?;
        let ended: Vec<(String, Option<String>)> = stmt
            .query_map(
                rusqlite::params![org_id, format_date(today), MAX_PENDING_PEOPLE as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .collect::<rusqlite::Result<_>>()?;
        let mut candidates: Vec<(String, Option<NaiveDate>)> = Vec::new();
        for (user, to) in ended {
            candidates.push((user, to.as_deref().map(validate::parse_date).transpose()?));
        }
        (today, candidates)
    };
    let mut out = Vec::new();
    for (user, ended_on) in candidates {
        let subject = Target {
            user_id: &user,
            reason: Reason::Departure,
            project_id: None,
            date: None,
            return_date: None,
        };
        let count = gather(pool, org_id, admin_id, &subject, today)?.held.len() as u32;
        if count > 0 {
            out.push(Pending {
                user_id: user,
                count,
                ended_on,
            });
        }
    }
    out.sort_by(|a, b| b.ended_on.cmp(&a.ended_on).then(a.user_id.cmp(&b.user_id)));
    Ok(out)
}

/// The handovers made for `user_id`, newest first. Who may see them: the same
/// people who may hand over an absence for the person.
pub fn records(
    pool: &DbPool,
    org_id: &str,
    actor: &Actor<'_>,
    user_id: &str,
) -> Result<Vec<HandoverRecord>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let today = today_of(&conn, org_id)?;
    let subject = Target {
        user_id,
        reason: Reason::Absence,
        project_id: None,
        date: None,
        return_date: None,
    };
    authorize(&conn, org_id, actor, &subject, today)?;
    let projects = ProjectDirectory::new(org_id, pool);
    let mut out = Vec::new();
    for header in record::headers_of(&conn, org_id, user_id)? {
        let all = record::items(&conn, &header.id)?;
        let before = all.len();
        let items = visible_items(&projects, actor, &header, all)?;
        if !(header.reason == Reason::Departure && actor.is_admin) {
            let hidden_project = match header.project_id.as_deref() {
                Some(project) => !ItemGate::new(&projects, actor.user_id, header.reason)
                    .project_visible(project)?,
                None => false,
            };
            if hidden_project || (items.is_empty() && before > 0) {
                continue;
            }
        }
        let project_names = project_names_of(org_id, &items);
        out.push(HandoverRecord {
            project_name: header
                .project_id
                .as_ref()
                .and_then(|p| project_names.get(p).cloned()),
            items: items.iter().map(|i| result_of(i, &project_names)).collect(),
            id: header.id,
            user_id: header.user_id,
            reason: header.reason,
            project_id: header.project_id,
            date: header.effective_on,
            return_date: header.return_on,
            note: header.note,
            created_by: header.created_by,
            created_at_ms: header.created_at_ms,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(action: Action, eligible: Option<&[&str]>, blocked: Option<&'static str>) -> Held {
        Held {
            key: "task:p-1:t-1".into(),
            category: Category::Task,
            title: "#1 Import".into(),
            role: "assignee".into(),
            state: "todo".into(),
            project_id: Some("p-1".into()),
            project_name: Some("NextApp".into()),
            unit_name: None,
            valid_to: None,
            action,
            suggestion: None,
            eligible: eligible.map(|ids| ids.iter().map(|s| s.to_string()).collect()),
            blocked,
        }
    }

    fn choose(taker: Option<&str>) -> Choice {
        Choice {
            key: "task:p-1:t-1".into(),
            taker_user_id: taker.map(str::to_string),
        }
    }

    fn members(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_transfer_needs_a_taker_who_is_a_member_and_among_the_eligible() {
        let item = held(Action::Transfer, Some(&["anna", "marek"]), None);
        let everyone = members(&["anna", "marek", "ewa", "leaver"]);
        assert_eq!(
            plan_of(&item, &choose(None), "leaver", &everyone).unwrap_err(),
            "taker_required"
        );
        assert_eq!(
            plan_of(&item, &choose(Some("")), "leaver", &everyone).unwrap_err(),
            "taker_required"
        );
        assert_eq!(
            plan_of(&item, &choose(Some("ewa")), "leaver", &everyone).unwrap_err(),
            "taker_not_eligible"
        );
        assert_eq!(
            plan_of(&item, &choose(Some("leaver")), "leaver", &everyone).unwrap_err(),
            "taker_not_eligible"
        );
        assert_eq!(
            plan_of(&item, &choose(Some("stranger")), "leaver", &everyone).unwrap_err(),
            "taker_not_eligible"
        );
        let plan = plan_of(&item, &choose(Some("anna")), "leaver", &everyone).unwrap();
        assert_eq!(plan.taker.as_deref(), Some("anna"));
        assert_eq!(
            (plan.key.as_str(), plan.category),
            ("task:p-1:t-1", Category::Task)
        );
    }

    #[test]
    fn an_item_that_only_ends_takes_nobody_whatever_was_sent() {
        let item = held(Action::End, None, None);
        let plan = plan_of(&item, &choose(Some("anna")), "leaver", &members(&["anna"])).unwrap();
        assert_eq!(plan.taker, None);
    }

    #[test]
    fn a_seat_or_a_cover_may_go_to_nobody_but_not_to_somebody_who_cannot_take_it() {
        let item = held(Action::TransferOrEnd, None, None);
        let everyone = members(&["anna", "leaver"]);
        assert_eq!(
            plan_of(&item, &choose(None), "leaver", &everyone)
                .unwrap()
                .taker,
            None
        );
        assert_eq!(
            plan_of(&item, &choose(Some("anna")), "leaver", &everyone)
                .unwrap()
                .taker
                .as_deref(),
            Some("anna")
        );
        assert_eq!(
            plan_of(&item, &choose(Some("ghost")), "leaver", &everyone).unwrap_err(),
            "taker_not_eligible"
        );
    }

    #[test]
    fn a_blocked_item_is_refused_with_the_reason_the_server_gave() {
        let item = held(Action::Transfer, None, Some("project_archived"));
        assert_eq!(
            plan_of(&item, &choose(Some("anna")), "leaver", &members(&["anna"])).unwrap_err(),
            "project_archived"
        );
    }

    #[test]
    fn the_note_digest_is_stable_and_does_not_contain_the_note() {
        let note = "Galaz feature/opc";
        assert_eq!(digest(note), digest(note));
        assert_ne!(digest(note), digest("Galaz feature/opd"));
        assert_eq!(digest(note).len(), 16);
        assert!(!digest(note).contains("Galaz"));
    }

    #[test]
    fn reasons_and_categories_round_trip_through_their_stored_names() {
        for reason in [Reason::Departure, Reason::Absence, Reason::ProjectRemoval] {
            assert_eq!(Reason::parse(reason.as_str()), Some(reason));
        }
        for category in Category::ALL {
            assert_eq!(Category::parse(category.as_str()), Some(category));
        }
        assert_eq!(Reason::parse("holiday"), None);
        // The groups of the screen follow the order of the variants.
        let mut shuffled = vec![
            Category::Deputy,
            Category::Task,
            Category::Position,
            Category::TestItem,
            Category::Membership,
        ];
        shuffled.sort();
        assert_eq!(shuffled, Category::ALL.to_vec());
    }
}
