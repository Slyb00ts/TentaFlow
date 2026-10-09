//! The work a person holds in Project Studio: open tasks, open items of running
//! test runs and project memberships. Every project has its own database, so
//! each item is moved in the store of its project, in one conditional
//! statement (`WHERE assigned_to = <person> AND unfinished`): a repeated call
//! finds nothing left to move, and something somebody closed or gave away in
//! the meantime is never taken back. The record of the handover (`record.rs`)
//! is what ties the steps together.
//!
//! Permissions are the Project Studio rules, applied to the ACTOR: a task goes
//! to an active project writer for its area; project administrators manage
//! memberships, and the owner cannot leave without
//! handing the project to a member. An organization administrator handing over
//! a departing person's work is the one place an administrator writes to
//! projects they are not a member of: the operation only moves that person's own
//! items to members of the same project, and every one is written to that
//! project's activity log (docs: the departure is the administrator's decision).

use std::collections::HashSet;
use std::rc::Rc;

use serde_json::{json, Value as Json};

use super::advice::Candidates;
use super::{
    Action, ApplyCx, Category, HandoverProvider, Held, ListCx, Planned, Reason, Recorded, Returned,
    Step, WorkProvider,
};
use crate::project_studio::{
    activity, db as ps_db, ml_link, notifications, project_db, repository, runs, tasks,
};
use crate::services::org_structure::availability::DeputyScope;
use crate::services::org_structure::error::Result;
use tentaflow_protocol::project_studio::access::{
    ProjectAccessWire, ProjectArea, ProjectPermissionLevel,
};

/// A project as the handover sees it.
pub(super) struct ProjectFacts {
    pub id: String,
    pub name: String,
    pub archived: bool,
    pub owner: String,
    members: Vec<(String, ProjectAccessWire)>,
    active_accounts: HashSet<String>,
}

impl ProjectFacts {
    pub fn access_of(&self, user: &str) -> Option<&ProjectAccessWire> {
        self.members
            .iter()
            .find(|(id, _)| id == user)
            .map(|(_, access)| access)
    }

    fn eligible(
        &self,
        area: ProjectArea,
        minimum: ProjectPermissionLevel,
        person: &str,
    ) -> HashSet<String> {
        self.members
            .iter()
            .filter(|(id, access)| {
                id != person && self.active_accounts.contains(id) && access.allows(area, minimum)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn managers(&self, person: &str) -> Vec<String> {
        self.members
            .iter()
            .filter(|(id, access)| {
                id != person && self.active_accounts.contains(id) && access.can_manage_members
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Projects of the organization, read once per request.
pub(super) struct ProjectDirectory {
    core_db: crate::db::DbPool,
    org_id: String,
}

impl ProjectDirectory {
    pub fn new(org_id: &str, core_db: &crate::db::DbPool) -> Self {
        Self {
            core_db: core_db.clone(),
            org_id: org_id.to_string(),
        }
    }

    /// False on a node where Project Studio is not running: nothing to hand over.
    pub fn available() -> bool {
        ps_db::pool().is_ok()
    }

    pub fn facts(&self, project_id: &str) -> Result<Option<Rc<ProjectFacts>>> {
        let Some(record) = repository::get_project(&self.org_id, project_id)? else {
            return Ok(None);
        };
        let ancestors = repository::project_ancestry(&self.org_id, project_id)?;
        let private_start = ancestors
            .iter()
            .rposition(|node| node.is_private)
            .unwrap_or(0);
        let mut users = HashSet::new();
        for node in &ancestors[private_start..] {
            users.extend(
                repository::list_members(&node.project_id)?
                    .into_iter()
                    .map(|member| member.user_id),
            );
        }
        users.extend(
            repository::effective_principals(
                project_id,
                ProjectArea::Settings,
                ProjectPermissionLevel::None,
            )?
            .into_iter()
            .map(|principal| principal.user_id),
        );
        let mut members = Vec::new();
        let mut active_accounts = HashSet::new();
        for user in users {
            if crate::db::repository::get_user_account_by_id(&self.core_db, &user)?
                .is_some_and(|account| account.is_active)
            {
                active_accounts.insert(user.clone());
            }
            let access = repository::project_access(&record, &user, false)?;
            members.push((user, access));
        }
        members.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(Some(Rc::new(ProjectFacts {
            id: record.project_id,
            name: record.name,
            owner: record.owner_user_id,
            archived: ancestors
                .iter()
                .any(|node| node.status == "archived" || node.lifecycle == "ended"),
            members,
            active_accounts,
        })))
    }

    /// Stored expired origins still identify held work; only active effective
    /// principals may receive it. Every apply reloads facts after the preview.
    pub fn projects_of(&self, user: &str, only: Option<&str>) -> Result<Vec<Rc<ProjectFacts>>> {
        if !Self::available() {
            return Ok(Vec::new());
        }
        let candidates = if let Some(id) = only {
            repository::list_descendants(&self.org_id, id, true)?
        } else {
            repository::list_projects(&self.org_id, true)?
        };
        let mut out = Vec::new();
        for project in candidates {
            if let Some(facts) = self.facts(&project.project_id)? {
                if facts.access_of(user).is_some() {
                    out.push(facts);
                }
            }
        }
        out.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(out)
    }

    /// What Project Studio grants the ACTOR in a project, with no administrator
    /// exception. An absence is handed over by the person's manager, who need
    /// not belong to the person's projects and must not see or move their work.
    pub fn actor_access(&self, project_id: &str, actor: &str) -> Result<Option<ProjectAccessWire>> {
        let Some(record) = repository::get_project(&self.org_id, project_id)? else {
            return Ok(None);
        };
        Ok(Some(repository::project_access(&record, actor, false)?))
    }

    /// True when the actor may read `area` of the project.
    pub fn actor_reads(
        &self,
        project_id: &str,
        actor: &str,
        areas: &[ProjectArea],
    ) -> Result<bool> {
        Ok(self.actor_access(project_id, actor)?.is_some_and(|access| {
            areas
                .iter()
                .any(|area| access.allows(*area, ProjectPermissionLevel::Read))
        }))
    }

    /// True when the actor may reassign `area` work: a writer there or a
    /// project administrator.
    fn actor_reassigns(&self, project_id: &str, actor: &str, area: ProjectArea) -> Result<bool> {
        Ok(self.actor_access(project_id, actor)?.is_some_and(|access| {
            access.has_access
                && (access.can_manage_members || access.allows(area, ProjectPermissionLevel::Write))
        }))
    }

    fn sync_index(&self, project_id: &str, pool: &crate::db::DbPool) -> Result<()> {
        let record = repository::get_project(&self.org_id, project_id)?
            .ok_or_else(|| anyhow::anyhow!("project missing"))?;
        settle(
            || Ok(crate::project_studio::task_index::sync_project(&record, pool, 256)?.lag),
            SYNC_INDEX_MAX_ROUNDS,
            SYNC_INDEX_DEADLINE,
        )
        .map_err(|_| {
            crate::services::org_structure::error::OrgStructureError::Db(format!(
                "task index of project {project_id} did not settle"
            ))
        })
    }
}

/// Rounds of 256 entries one index sync may take, and the time it may spend:
/// a project whose index never settles must not hold a worker forever.
const SYNC_INDEX_MAX_ROUNDS: usize = 64;
const SYNC_INDEX_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Runs `round` until it reports no lag, at most `max_rounds` times and until `deadline`.
fn settle<L: PartialEq + Default>(
    mut round: impl FnMut() -> Result<L>,
    max_rounds: usize,
    deadline: std::time::Duration,
) -> std::result::Result<(), ()> {
    let until = std::time::Instant::now() + deadline;
    for _ in 0..max_rounds {
        if round().map_err(|_| ())? == L::default() {
            return Ok(());
        }
        if std::time::Instant::now() >= until {
            break;
        }
    }
    Err(())
}

/// `task:<project>:<task>` and friends: the project id has no colon.
pub(super) fn parse_key<'a>(key: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = key.strip_prefix(prefix)?.strip_prefix(':')?;
    rest.split_once(':')
}

fn task_key(project: &str, task: &str) -> String {
    format!("task:{project}:{task}")
}

fn test_key(project: &str, item: &str) -> String {
    format!("test:{project}:{item}")
}

fn member_key(project: &str) -> String {
    format!("member:{project}")
}

fn blocked_of(facts: &ProjectFacts) -> Option<&'static str> {
    facts.archived.then_some("project_archived")
}

fn notify_pair(
    cx: &ApplyCx<'_>,
    project_id: &str,
    kind: &str,
    users: [&str; 2],
    title: &str,
    body: &str,
    link: Json,
) {
    for user in users {
        if user != cx.actor {
            notifications::notify(
                cx.org_id,
                user,
                project_id,
                kind,
                title,
                body,
                &link.to_string(),
            );
        }
    }
}

fn spawn_ml_sync(project_id: &str) {
    let project = project_id.to_string();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || ml_link::sync_project_memberships(project));
        }
        Err(_) => ml_link::sync_project_memberships(project),
    }
}

fn open_pool(project_id: &str) -> Result<crate::db::DbPool> {
    Ok(project_db::open(project_id)?)
}

// =============================================================================
// Tasks
// =============================================================================

pub(super) struct TaskProvider<'a> {
    pub projects: &'a ProjectDirectory,
}

impl HandoverProvider for TaskProvider<'_> {
    fn category(&self) -> Category {
        Category::Task
    }

    fn list(&self, cx: &ListCx<'_>) -> Result<Vec<Held>> {
        let mut out = Vec::new();
        for facts in self.projects.projects_of(cx.user_id, cx.project)? {
            if cx.reason == Reason::Absence
                && !self.projects.actor_reads(
                    &facts.id,
                    cx.actor,
                    &[ProjectArea::Tasks, ProjectArea::Board],
                )?
            {
                continue;
            }
            if cx.reason == Reason::ProjectRemoval {
                if let Some(removed_project) = cx.project {
                    let record = repository::get_project(&self.projects.org_id, &facts.id)?
                        .ok_or_else(|| anyhow::anyhow!("project missing"))?;
                    let after = repository::project_access_after_member_removal(
                        &record,
                        cx.user_id,
                        removed_project,
                    )?;
                    if after.allows(ProjectArea::Tasks, ProjectPermissionLevel::Read)
                        || after.allows(ProjectArea::Board, ProjectPermissionLevel::Read)
                    {
                        continue;
                    }
                }
            }
            let pool = open_pool(&facts.id)?;
            self.projects.sync_index(&facts.id, &pool)?;
            let eligible = facts.eligible(
                ProjectArea::Tasks,
                ProjectPermissionLevel::Write,
                cx.user_id,
            );
            let managers = facts.managers(cx.user_id);
            for task in tasks::open_tasks_of(&pool, cx.user_id)? {
                let candidates = Candidates {
                    eligible: Some(&eligible),
                    project_managers: &managers,
                    project_owner: Some(facts.owner.as_str()),
                };
                out.push(Held {
                    key: task_key(&facts.id, &task.task_id),
                    category: Category::Task,
                    title: format!("#{} {}", task.task_no, task.title),
                    role: "assignee".into(),
                    state: task.status,
                    project_id: Some(facts.id.clone()),
                    project_name: Some(facts.name.clone()),
                    unit_name: None,
                    valid_to: None,
                    action: Action::Transfer,
                    suggestion: cx
                        .advice
                        .for_work(&DeputyScope::Project(facts.id.clone()), &candidates),
                    eligible: Some(eligible.iter().cloned().collect()),
                    blocked: blocked_of(&facts),
                });
            }
        }
        Ok(out)
    }
}

/// The taker must be a member of the project (the rule of `task_save`).
fn taker_of<'a>(
    item: &'a Planned,
    facts: &ProjectFacts,
    area: ProjectArea,
    minimum: ProjectPermissionLevel,
) -> std::result::Result<&'a str, &'static str> {
    let taker = item.taker.as_deref().ok_or("taker_required")?;
    match facts.access_of(taker) {
        Some(access) if facts.active_accounts.contains(taker) && access.allows(area, minimum) => {
            Ok(taker)
        }
        _ => Err("taker_not_eligible"),
    }
}

impl WorkProvider for TaskProvider<'_> {
    fn apply(&self, cx: &ApplyCx<'_>, item: &Planned) -> Result<Step> {
        let Some((project_id, task_id)) = parse_key(&item.key, "task") else {
            return Ok(Step::Refused("bad_key"));
        };
        if repository::task_location(task_id)?.is_none_or(|location| {
            location.org_id != cx.org_id || location.project_id != project_id
        }) {
            return Ok(Step::Refused("task_moved"));
        }
        let Some(facts) = self.projects.facts(project_id)? else {
            return Ok(Step::Refused("project_missing"));
        };
        if facts.archived {
            return Ok(Step::Refused("project_archived"));
        }
        if cx.reason == Reason::Absence
            && !self
                .projects
                .actor_reassigns(project_id, cx.actor, ProjectArea::Tasks)?
        {
            return Ok(Step::Refused("not_permitted"));
        }
        let taker = match taker_of(
            item,
            &facts,
            ProjectArea::Tasks,
            ProjectPermissionLevel::Write,
        ) {
            Ok(taker) => taker,
            Err(reason) => return Ok(Step::Refused(reason)),
        };
        let pool = open_pool(project_id)?;
        let before = tasks::get_task(&pool, task_id)?;
        let Some(mutation) = tasks::reassign_open(
            &pool,
            task_id,
            cx.from_user,
            taker,
            &tasks::TaskHandoverInput {
                actor: cx.actor,
                note_md: cx.note,
                mention_user_ids: &[],
                direction: tasks::TaskHandoverDirection::Over,
                handover_id: Some(cx.handover_id),
            },
        )?
        else {
            return Ok(Step::Skipped("no_longer_held"));
        };
        if mutation.changed {
            activity::record(
                &pool,
                cx.actor,
                "user",
                "task.handed_over",
                "task",
                task_id,
                &json!({
                    "from": cx.from_user,
                    "to": taker,
                    "handover_id": cx.handover_id,
                    "reason": cx.reason.as_str(),
                    "note_chars": cx.note.chars().count(),
                    "note_sha256": cx.note_digest,
                })
                .to_string(),
            );
            notifications::notify_task_handover(
                cx.org_id,
                cx.actor,
                project_id,
                &mutation,
                tasks::TaskHandoverDirection::Over,
                &|user| {
                    if !crate::db::repository::get_user_account_by_id(&self.projects.core_db, user)
                        .ok()
                        .flatten()
                        .is_some_and(|account| account.is_active)
                    {
                        return false;
                    }
                    repository::get_project(cx.org_id, project_id)
                        .ok()
                        .flatten()
                        .and_then(|project| repository::project_access(&project, user, false).ok())
                        .is_some_and(|access| {
                            access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Read)
                                || access.allows(ProjectArea::Board, ProjectPermissionLevel::Read)
                        })
                },
            );
        }
        self.projects.sync_index(project_id, &pool)?;
        Ok(Step::Done(json!({
            "status": before.map(|t| t.status).unwrap_or_default(),
        })))
    }

    fn reverse(&self, cx: &ApplyCx<'_>, item: &Recorded) -> Result<Returned> {
        let Some((project_id, task_id)) = parse_key(&item.key, "task") else {
            return Ok(Returned::Kept("bad_key"));
        };
        if repository::task_location(task_id)?.is_none_or(|location| {
            location.org_id != cx.org_id || location.project_id != project_id
        }) {
            return Ok(Returned::Kept("task_moved"));
        }
        let (Some(taker), Some(facts)) = (item.taker.as_deref(), self.projects.facts(project_id)?)
        else {
            return Ok(Returned::Kept("project_missing"));
        };
        if facts.archived {
            return Ok(Returned::Kept("project_archived"));
        }
        let pool = open_pool(project_id)?;
        let Some(task) = tasks::get_task(&pool, task_id)? else {
            return Ok(Returned::Kept("closed"));
        };
        if task.status == "done" {
            return Ok(Returned::Kept("closed"));
        }
        if facts
            .access_of(&item.from_user)
            .is_none_or(|access| !access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Write))
        {
            return Ok(Returned::Kept("not_member"));
        }
        let Some(mutation) = tasks::reassign_open(
            &pool,
            task_id,
            taker,
            &item.from_user,
            &tasks::TaskHandoverInput {
                actor: cx.actor,
                note_md: cx.note,
                mention_user_ids: &[],
                direction: tasks::TaskHandoverDirection::Back,
                handover_id: Some(cx.handover_id),
            },
        )?
        else {
            return Ok(Returned::Kept("changed"));
        };
        if mutation.changed {
            activity::record(
                &pool,
                cx.actor,
                "user",
                "task.handed_back",
                "task",
                task_id,
                &json!({ "from": taker, "to": item.from_user, "handover_id": cx.handover_id })
                    .to_string(),
            );
            notifications::notify_task_handover(
                cx.org_id,
                cx.actor,
                project_id,
                &mutation,
                tasks::TaskHandoverDirection::Back,
                &|user| {
                    if !crate::db::repository::get_user_account_by_id(&self.projects.core_db, user)
                        .ok()
                        .flatten()
                        .is_some_and(|account| account.is_active)
                    {
                        return false;
                    }
                    repository::get_project(cx.org_id, project_id)
                        .ok()
                        .flatten()
                        .and_then(|project| repository::project_access(&project, user, false).ok())
                        .is_some_and(|access| {
                            access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Read)
                                || access.allows(ProjectArea::Board, ProjectPermissionLevel::Read)
                        })
                },
            );
        }
        self.projects.sync_index(project_id, &pool)?;
        Ok(Returned::Back)
    }
}

// =============================================================================
// Test-run items
// =============================================================================

pub(super) struct TestItemProvider<'a> {
    pub projects: &'a ProjectDirectory,
}

impl HandoverProvider for TestItemProvider<'_> {
    fn category(&self) -> Category {
        Category::TestItem
    }

    fn list(&self, cx: &ListCx<'_>) -> Result<Vec<Held>> {
        let mut out = Vec::new();
        for facts in self.projects.projects_of(cx.user_id, cx.project)? {
            if cx.reason == Reason::Absence
                && !self
                    .projects
                    .actor_reads(&facts.id, cx.actor, &[ProjectArea::Tests])?
            {
                continue;
            }
            if cx.reason == Reason::ProjectRemoval {
                if let Some(removed_project) = cx.project {
                    let record = repository::get_project(&self.projects.org_id, &facts.id)?
                        .ok_or_else(|| anyhow::anyhow!("project missing"))?;
                    let after = repository::project_access_after_member_removal(
                        &record,
                        cx.user_id,
                        removed_project,
                    )?;
                    if after.allows(ProjectArea::Tests, ProjectPermissionLevel::Write) {
                        continue;
                    }
                }
            }
            let pool = open_pool(&facts.id)?;
            let eligible = facts.eligible(
                ProjectArea::Tests,
                ProjectPermissionLevel::Write,
                cx.user_id,
            );
            let managers = facts.managers(cx.user_id);
            for item in runs::open_items_of(&pool, cx.user_id)? {
                let candidates = Candidates {
                    eligible: Some(&eligible),
                    project_managers: &managers,
                    project_owner: Some(facts.owner.as_str()),
                };
                out.push(Held {
                    key: test_key(&facts.id, &item.item_id),
                    category: Category::TestItem,
                    title: format!("#{} {} · {}", item.run_no, item.run_name, item.case_title),
                    role: "assignee".into(),
                    state: item.status,
                    project_id: Some(facts.id.clone()),
                    project_name: Some(facts.name.clone()),
                    unit_name: None,
                    valid_to: None,
                    action: Action::Transfer,
                    suggestion: cx
                        .advice
                        .for_work(&DeputyScope::Project(facts.id.clone()), &candidates),
                    eligible: Some(eligible.iter().cloned().collect()),
                    blocked: blocked_of(&facts),
                });
            }
        }
        Ok(out)
    }
}

impl WorkProvider for TestItemProvider<'_> {
    fn apply(&self, cx: &ApplyCx<'_>, item: &Planned) -> Result<Step> {
        let Some((project_id, item_id)) = parse_key(&item.key, "test") else {
            return Ok(Step::Refused("bad_key"));
        };
        let Some(facts) = self.projects.facts(project_id)? else {
            return Ok(Step::Refused("project_missing"));
        };
        if facts.archived {
            return Ok(Step::Refused("project_archived"));
        }
        if cx.reason == Reason::Absence
            && !self
                .projects
                .actor_reassigns(project_id, cx.actor, ProjectArea::Tests)?
        {
            return Ok(Step::Refused("not_permitted"));
        }
        let taker = match taker_of(
            item,
            &facts,
            ProjectArea::Tests,
            ProjectPermissionLevel::Write,
        ) {
            Ok(taker) => taker,
            Err(reason) => return Ok(Step::Refused(reason)),
        };
        let pool = open_pool(project_id)?;
        let before = runs::get_run_item(&pool, item_id)?;
        let moved = runs::reassign_open_item(&pool, item_id, cx.from_user, taker)?;
        let resumed = !moved
            && before.as_ref().is_some_and(|i| {
                i.assigned_to == taker && matches!(i.status.as_str(), "pending" | "in_progress")
            });
        if !moved && !resumed {
            return Ok(Step::Skipped("no_longer_held"));
        }
        activity::record(
            &pool,
            cx.actor,
            "user",
            "run_item.handed_over",
            "run_item",
            item_id,
            &json!({
                "from": cx.from_user,
                "to": taker,
                "handover_id": cx.handover_id,
                "reason": cx.reason.as_str(),
                "note_chars": cx.note.chars().count(),
                "note_sha256": cx.note_digest,
            })
            .to_string(),
        );
        Ok(Step::Done(
            json!({ "status": before.map(|i| i.status).unwrap_or_default() }),
        ))
    }

    fn reverse(&self, cx: &ApplyCx<'_>, item: &Recorded) -> Result<Returned> {
        let Some((project_id, item_id)) = parse_key(&item.key, "test") else {
            return Ok(Returned::Kept("bad_key"));
        };
        let (Some(taker), Some(facts)) = (item.taker.as_deref(), self.projects.facts(project_id)?)
        else {
            return Ok(Returned::Kept("project_missing"));
        };
        if facts.archived {
            return Ok(Returned::Kept("project_archived"));
        }
        let pool = open_pool(project_id)?;
        let Some(current) = runs::get_run_item(&pool, item_id)? else {
            return Ok(Returned::Kept("closed"));
        };
        if !matches!(current.status.as_str(), "pending" | "in_progress") {
            return Ok(Returned::Kept("closed"));
        }
        if current.assigned_to != taker {
            return Ok(Returned::Kept("changed"));
        }
        if facts
            .access_of(&item.from_user)
            .is_none_or(|access| !access.allows(ProjectArea::Tests, ProjectPermissionLevel::Write))
        {
            return Ok(Returned::Kept("not_member"));
        }
        if !runs::reassign_open_item(&pool, item_id, taker, &item.from_user)? {
            return Ok(Returned::Kept("changed"));
        }
        activity::record(
            &pool,
            cx.actor,
            "system",
            "run_item.handed_back",
            "run_item",
            item_id,
            &json!({ "from": taker, "to": item.from_user, "handover_id": cx.handover_id })
                .to_string(),
        );
        notify_pair(
            cx,
            project_id,
            "work_handed_back",
            [item.from_user.as_str(), taker],
            "Przypadek testowy wrócił do właściciela",
            &current.case_title,
            json!({ "project_id": project_id, "item_id": item_id }),
        );
        Ok(Returned::Back)
    }
}

// =============================================================================
// Memberships
// =============================================================================

pub(super) struct MembershipProvider<'a> {
    pub projects: &'a ProjectDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberRemoval {
    Removed,
    Missing,
    HoldsWork,
    Owner,
}

pub(crate) fn remove_project_member(
    org_id: &str,
    project_id: &str,
    user: &str,
    actor: &str,
    details: &str,
) -> Result<MemberRemoval> {
    let Some(project) = repository::get_project(org_id, project_id)? else {
        return Ok(MemberRemoval::Missing);
    };
    if project.owner_user_id == user {
        return Ok(MemberRemoval::Owner);
    }
    for node in repository::list_descendants(org_id, project_id, true)? {
        let after = repository::project_access_after_member_removal(&node, user, project_id)?;
        let task_access = after.allows(ProjectArea::Tasks, ProjectPermissionLevel::Read)
            || after.allows(ProjectArea::Board, ProjectPermissionLevel::Read);
        let test_access = after.allows(ProjectArea::Tests, ProjectPermissionLevel::Write);
        if task_access && test_access {
            continue;
        }
        let content = open_pool(&node.project_id)?;
        if (!task_access && !tasks::open_tasks_of(&content, user)?.is_empty())
            || (!test_access && !runs::open_items_of(&content, user)?.is_empty())
        {
            return Ok(MemberRemoval::HoldsWork);
        }
    }
    let pool = open_pool(project_id)?;
    if !repository::remove_member(project_id, user)? {
        return Ok(MemberRemoval::Missing);
    }
    activity::record(
        &pool,
        actor,
        "user",
        "member.removed",
        "member",
        user,
        details,
    );
    spawn_ml_sync(project_id);
    Ok(MemberRemoval::Removed)
}

impl HandoverProvider for MembershipProvider<'_> {
    fn category(&self) -> Category {
        Category::Membership
    }

    fn list(&self, cx: &ListCx<'_>) -> Result<Vec<Held>> {
        let mut out = Vec::new();
        if cx.reason == Reason::Absence {
            return Ok(out);
        }
        for facts in self.projects.projects_of(cx.user_id, cx.project)? {
            if cx.project.is_some_and(|selected| selected != facts.id) {
                continue;
            }
            if repository::member_access(&facts.id, cx.user_id)?.is_none()
                && facts.owner != cx.user_id
            {
                continue;
            }
            let owner = facts.owner == cx.user_id;
            let eligible = facts
                .members
                .iter()
                .filter(|(id, access)| {
                    id != cx.user_id && facts.active_accounts.contains(id) && access.has_access
                })
                .map(|(id, _)| id.clone())
                .collect::<HashSet<_>>();
            let managers = facts.managers(cx.user_id);
            let candidates = Candidates {
                eligible: Some(&eligible),
                project_managers: &managers,
                project_owner: None,
            };
            out.push(Held {
                key: member_key(&facts.id),
                category: Category::Membership,
                title: facts.name.clone(),
                role: if owner {
                    "owner".into()
                } else {
                    "member".into()
                },
                state: String::new(),
                project_id: Some(facts.id.clone()),
                project_name: Some(facts.name.clone()),
                unit_name: None,
                valid_to: None,
                action: if owner { Action::Transfer } else { Action::End },
                suggestion: owner
                    .then(|| {
                        cx.advice
                            .for_work(&DeputyScope::Project(facts.id.clone()), &candidates)
                    })
                    .flatten(),
                eligible: owner.then(|| eligible.iter().cloned().collect()),
                blocked: blocked_of(&facts),
            });
        }
        Ok(out)
    }
}

impl MembershipProvider<'_> {
    /// Removes the membership if the person no longer holds open work there.
    fn end_now(&self, cx: &ApplyCx<'_>, project_id: &str, user: &str) -> Result<Step> {
        match remove_project_member(
            cx.org_id,
            project_id,
            user,
            cx.actor,
            &json!({ "handover_id": cx.handover_id, "reason": cx.reason.as_str() }).to_string(),
        )? {
            MemberRemoval::Removed => Ok(Step::Done(json!({}))),
            MemberRemoval::Missing => Ok(Step::Skipped("no_longer_held")),
            MemberRemoval::HoldsWork => Ok(Step::Refused("still_holds_work")),
            MemberRemoval::Owner => Ok(Step::Refused("hand_ownership_first")),
        }
    }
}

impl WorkProvider for MembershipProvider<'_> {
    fn apply(&self, cx: &ApplyCx<'_>, item: &Planned) -> Result<Step> {
        let Some(project_id) = item.key.strip_prefix("member:") else {
            return Ok(Step::Refused("bad_key"));
        };
        let Some(facts) = self.projects.facts(project_id)? else {
            return Ok(Step::Refused("project_missing"));
        };
        if facts.archived {
            return Ok(Step::Refused("project_archived"));
        }
        if facts.access_of(cx.from_user).is_none() {
            return Ok(Step::Skipped("no_longer_held"));
        }
        if !cx.actor_is_admin
            && facts
                .access_of(cx.actor)
                .is_none_or(|access| !access.can_manage_members)
        {
            return Ok(Step::Refused("not_permitted"));
        }
        if facts.owner == cx.from_user {
            if !cx.actor_is_admin && cx.actor != facts.owner {
                return Ok(Step::Refused("ownership_required"));
            }
            let taker = item.taker.as_deref().filter(|user| {
                facts
                    .access_of(user)
                    .is_some_and(|access| access.has_access)
            });
            let Some(taker) = taker else {
                return Ok(Step::Refused("taker_not_eligible"));
            };
            repository::transfer_ownership(project_id, cx.from_user, taker)?;
            let pool = open_pool(project_id)?;
            activity::record(
                &pool,
                cx.actor,
                "user",
                "ownership.transferred",
                "project",
                project_id,
                &json!({ "from": cx.from_user, "to": taker, "handover_id": cx.handover_id })
                    .to_string(),
            );
        }
        if cx.date > cx.today {
            return Ok(Step::Scheduled(json!({})));
        }
        self.end_now(cx, project_id, cx.from_user)
    }

    fn reverse(&self, _cx: &ApplyCx<'_>, _item: &Recorded) -> Result<Returned> {
        Ok(Returned::Kept("not_temporary"))
    }

    fn complete(&self, cx: &ApplyCx<'_>, item: &Recorded) -> Result<Step> {
        let Some(project_id) = item.key.strip_prefix("member:") else {
            return Ok(Step::Refused("bad_key"));
        };
        match self.projects.facts(project_id)? {
            None => Ok(Step::Refused("project_missing")),
            Some(facts) if facts.archived => Ok(Step::Refused("project_archived")),
            Some(facts) if facts.access_of(&item.from_user).is_none() => {
                Ok(Step::Skipped("no_longer_held"))
            }
            Some(_) => self.end_now(cx, project_id, &item.from_user),
        }
    }
}

/// The store behind a category, for the return job.
pub(super) fn provider_for<'a>(
    category: Category,
    projects: &'a ProjectDirectory,
) -> Option<Box<dyn WorkProvider + 'a>> {
    match category {
        Category::Task => Some(Box::new(TaskProvider { projects })),
        Category::TestItem => Some(Box::new(TestItemProvider { projects })),
        Category::Membership => Some(Box::new(MembershipProvider { projects })),
        Category::Position | Category::Deputy => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_sync_stops_after_its_round_budget_and_after_its_deadline() {
        let mut rounds = 0;
        let never = settle(
            || {
                rounds += 1;
                Ok(1u64)
            },
            5,
            std::time::Duration::from_secs(60),
        );
        assert!(never.is_err());
        assert_eq!(rounds, 5);

        let mut slow_rounds = 0;
        let slow = settle(
            || {
                slow_rounds += 1;
                std::thread::sleep(std::time::Duration::from_millis(30));
                Ok(1u64)
            },
            1000,
            std::time::Duration::from_millis(50),
        );
        assert!(slow.is_err());
        assert!(slow_rounds < 10, "{slow_rounds}");

        let mut left = 3u64;
        assert!(settle(
            || {
                left -= 1;
                Ok(left)
            },
            10,
            std::time::Duration::from_secs(1)
        )
        .is_ok());
    }

    #[test]
    fn keys_split_into_project_and_item() {
        assert_eq!(parse_key("task:p-1:t-9", "task"), Some(("p-1", "t-9")));
        assert_eq!(parse_key("test:p-1:i-2", "test"), Some(("p-1", "i-2")));
        assert_eq!(parse_key("task:p-1", "task"), None);
        assert_eq!(parse_key("member:p-1", "task"), None);
        // A prefix must be a whole word: "tasks:..." is not a task key.
        assert_eq!(parse_key("tasks:p-1:t-9", "task"), None);
    }

    #[tokio::test]
    async fn an_absence_is_moved_only_by_an_asker_who_may_write_the_work() {
        use crate::project_studio::models::MemberInput;
        let root = tempfile::tempdir().expect("actual project storage");
        let _ = ps_db::init(&root.path().join("projects.db"));
        let state = crate::dispatch::AppState::for_test();
        let org = crate::services::org::DEFAULT_ORG_ID;
        let mut users = Vec::new();
        for name in ["abs-owner", "abs-giver", "abs-taker", "abs-stranger"] {
            let id = crate::db::repository::create_user_account(
                &state.db,
                name,
                "hash",
                name,
                &format!("{name}@example.test"),
            )
            .expect("actual account");
            crate::services::org::add_membership(&state.db, org, &id, "role-org-viewer", "test")
                .expect("actual organization member");
            users.push(id);
        }
        let (owner, giver, taker, stranger) = (&users[0], &users[1], &users[2], &users[3]);
        let grant = |user: &str| MemberInput {
            user_id: user.into(),
            functions: vec!["developer".into(), "tester".into()],
            project_admin: false,
            expires_at: None,
        };
        let id = uuid::Uuid::new_v4().to_string();
        let dir = root.path().join(&id);
        std::fs::create_dir_all(dir.join("files")).expect("actual project directory");
        repository::create_project(
            &id,
            org,
            &format!("Absence {id}"),
            "",
            "custom",
            "[\"tasks\",\"tests\"]",
            owner,
            &dir.to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[grant(giver), grant(taker)],
        )
        .expect("actual project");
        let pool = project_db::open(&id).expect("content");
        let task = tasks::create_task(
            &pool,
            &tasks::TaskInput {
                task_type: "technical",
                title: "Held while away",
                description_md: "",
                severity: "",
                priority: "medium",
                status: "todo",
                assigned_to: giver,
                due_date: "",
                parent_task_id: None,
                links_json: "[]",
                attachments_json: "[]",
            },
            owner,
        )
        .expect("actual held task");
        let item_id = uuid::Uuid::new_v4().to_string();
        {
            let conn = pool.write().expect("content writer");
            conn.execute(
                "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, status, created_by) \
                 VALUES ('abs-run', 1, 'Regression', 'single', 'running', ?1)",
                [owner],
            )
            .expect("actual run");
            conn.execute(
                "INSERT INTO test_run_items (item_id, run_id, case_id, case_title, case_version, \
                    position, assigned_to, status) VALUES (?1, 'abs-run', 'c-1', 'Login', 1, 0, ?2, 'pending')",
                [&item_id, giver],
            )
            .expect("actual run item");
        }
        let directory = ProjectDirectory::new(org, &state.db);
        directory.sync_index(&id, &pool).expect("current location");
        let day = chrono::Utc::now().date_naive();
        let operation = uuid::Uuid::new_v4().to_string();
        let cx = |actor| ApplyCx {
            org_id: org,
            actor,
            actor_is_admin: false,
            handover_id: &operation,
            from_user: giver,
            reason: Reason::Absence,
            date: day,
            today: day,
            note: "Away",
            note_digest: "",
        };
        let task_plan = Planned {
            key: task_key(&id, &task.task_id),
            category: Category::Task,
            title: "Held while away".into(),
            project_id: Some(id.clone()),
            taker: Some(taker.clone()),
        };
        let item_plan = Planned {
            key: test_key(&id, &item_id),
            category: Category::TestItem,
            title: "Login".into(),
            project_id: Some(id.clone()),
            taker: Some(taker.clone()),
        };
        let tasks_of = TaskProvider {
            projects: &directory,
        };
        let items_of = TestItemProvider {
            projects: &directory,
        };

        // Somebody who is not in the project is refused, however the key was learned.
        assert!(matches!(
            tasks_of.apply(&cx(stranger), &task_plan).expect("answered"),
            Step::Refused("not_permitted")
        ));
        assert!(matches!(
            items_of.apply(&cx(stranger), &item_plan).expect("answered"),
            Step::Refused("not_permitted")
        ));
        assert_eq!(
            tasks::get_task(&pool, &task.task_id)
                .unwrap()
                .unwrap()
                .assigned_to,
            *giver
        );
        assert_eq!(
            runs::get_run_item(&pool, &item_id)
                .unwrap()
                .unwrap()
                .assigned_to,
            *giver
        );

        // The project's own owner may.
        assert!(matches!(
            tasks_of.apply(&cx(owner), &task_plan).expect("answered"),
            Step::Done(_)
        ));
        assert!(matches!(
            items_of.apply(&cx(owner), &item_plan).expect("answered"),
            Step::Done(_)
        ));
        assert_eq!(
            tasks::get_task(&pool, &task.task_id)
                .unwrap()
                .unwrap()
                .assigned_to,
            *taker
        );
        std::mem::forget(root);
    }

    #[tokio::test]
    async fn inherited_held_work_respects_private_cuts_surviving_grants_and_moved_uuid() {
        use crate::project_studio::models::MemberInput;
        let root = tempfile::tempdir().expect("actual project storage");
        let _ = ps_db::init(&root.path().join("projects.db"));
        crate::services::ingest_jobs::init(&root.path().join("jobs.db"))
            .expect("actual media queue for transfer");
        let state = crate::dispatch::AppState::for_test();
        let org = crate::services::org::DEFAULT_ORG_ID;
        let mut users = Vec::new();
        for name in ["owner", "giver", "taker"] {
            let id = crate::db::repository::create_user_account(
                &state.db,
                name,
                "hash",
                name,
                &format!("{name}@example.test"),
            )
            .expect("actual account");
            crate::services::org::add_membership(&state.db, org, &id, "role-org-viewer", "test")
                .expect("actual organization member");
            users.push(id);
        }
        let owner = &users[0];
        let giver = &users[1];
        let taker = &users[2];
        let grant = |user: &str| MemberInput {
            user_id: user.into(),
            functions: vec!["developer".into()],
            project_admin: false,
            expires_at: None,
        };
        let project = |name: &str, parent: Option<&str>, private: bool, members: &[MemberInput]| {
            let id = uuid::Uuid::new_v4().to_string();
            let dir = root.path().join(&id);
            std::fs::create_dir_all(dir.join("files")).expect("actual project directory");
            repository::create_project(
                &id,
                org,
                &format!("{name} {id}"),
                "",
                "custom",
                "[\"tasks\",\"tests\"]",
                owner,
                &dir.to_string_lossy(),
                "",
                parent,
                private,
                parent.is_some(),
                parent.is_some(),
                members,
            )
            .expect("actual tree node");
            repository::get_project(org, &id)
                .expect("lookup")
                .expect("project")
        };
        let ancestor = project("Ancestor", None, false, &[grant(giver), grant(taker)]);
        let public = project("Inherited work", Some(&ancestor.project_id), false, &[]);
        let private = project("Private cut", Some(&ancestor.project_id), true, &[]);
        let destination = project(
            "Private destination",
            Some(&ancestor.project_id),
            true,
            &[grant(giver), grant(taker)],
        );
        let pool = project_db::open(&public.project_id).expect("content");
        let task = tasks::create_task(
            &pool,
            &tasks::TaskInput {
                task_type: "technical",
                title: "Inherited assignee work",
                description_md: "",
                severity: "",
                priority: "medium",
                status: "todo",
                assigned_to: giver,
                due_date: "",
                parent_task_id: None,
                links_json: "[]",
                attachments_json: "[]",
            },
            owner,
        )
        .expect("actual held task");
        let directory = ProjectDirectory::new(org, &state.db);
        directory
            .sync_index(&public.project_id, &pool)
            .expect("current UUID location");
        assert!(directory
            .facts(&private.project_id)
            .expect("private facts")
            .expect("private project")
            .access_of(giver)
            .is_none());
        let provider = TaskProvider {
            projects: &directory,
        };
        let day = chrono::Utc::now().date_naive();
        let read_held = |reason, project| {
            let conn = state.db.read().expect("organization reader");
            let advice = super::super::advice::Advice::load(&conn, org, giver, day, day)
                .expect("actual F07 advice");
            let names = std::collections::HashMap::new();
            let members = users.iter().cloned().collect();
            provider
                .list(&ListCx {
                    conn: &conn,
                    org_id: org,
                    user_id: giver,
                    actor: owner,
                    reason,
                    date: day,
                    project,
                    advice: &advice,
                    names: &names,
                    members: &members,
                })
                .expect("real provider inventory")
        };
        let preview = read_held(Reason::ProjectRemoval, Some(ancestor.project_id.as_str()));
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].key, task_key(&public.project_id, &task.task_id));
        assert!(preview[0]
            .eligible
            .as_ref()
            .expect("actual takers")
            .contains(taker));
        assert_eq!(
            remove_project_member(org, &ancestor.project_id, giver, owner, "{}")
                .expect("shared removal guard"),
            MemberRemoval::HoldsWork
        );
        repository::add_members(&public.project_id, &[grant(giver)], owner)
            .expect("surviving local contribution");
        assert!(read_held(Reason::ProjectRemoval, Some(ancestor.project_id.as_str())).is_empty());
        assert_eq!(
            remove_project_member(org, &ancestor.project_id, giver, owner, "{}")
                .expect("remove only ancestor contribution"),
            MemberRemoval::Removed
        );
        assert!(repository::project_access(&public, giver, false)
            .expect("same effective evaluator")
            .allows(ProjectArea::Tasks, ProjectPermissionLevel::Write));
        let expires = chrono::Utc::now() + chrono::Duration::seconds(2);
        let mut expiring = grant(giver);
        expiring.expires_at = Some(expires.to_rfc3339());
        repository::add_members(&ancestor.project_id, &[expiring], owner)
            .expect("actual expiring ancestor contribution");
        repository::remove_member(&public.project_id, giver)
            .expect("ancestor still supplies access");
        while chrono::Utc::now() <= expires {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let expired_preview = read_held(Reason::Departure, None);
        assert_eq!(expired_preview.len(), 1);
        assert_eq!(expired_preview[0].key, preview[0].key);
        assert!(expired_preview[0]
            .eligible
            .as_ref()
            .expect("current eligible takers")
            .contains(taker));
        assert!(
            !repository::project_access(&public, giver, false)
                .expect("expired effective access")
                .has_access
        );
        let planned = Planned {
            key: expired_preview[0].key.clone(),
            category: Category::Task,
            title: expired_preview[0].title.clone(),
            project_id: Some(public.project_id.clone()),
            taker: Some(taker.clone()),
        };
        let destination_pool = project_db::open(&destination.project_id).expect("destination");
        crate::project_studio::task_transfer::transfer_task(
            &state,
            &public,
            &destination,
            &pool,
            &destination_pool,
            &task.task_id,
            owner,
            true,
        )
        .expect("actual UUID transfer with explicit wider-access consent");
        let operation = uuid::Uuid::new_v4().to_string();
        let cx = ApplyCx {
            org_id: org,
            actor: owner,
            actor_is_admin: false,
            handover_id: &operation,
            from_user: giver,
            reason: Reason::Departure,
            date: day,
            today: day,
            note: "Stale handover preview",
            note_digest: "",
        };
        assert!(matches!(
            provider
                .apply(&cx, &planned)
                .expect("fresh current-location check"),
            Step::Refused("task_moved")
        ));
        assert_eq!(
            tasks::get_task(&destination_pool, &task.task_id)
                .expect("current owning project")
                .expect("same task UUID")
                .assigned_to,
            *giver
        );
        assert_eq!(
            repository::task_location(&task.task_id)
                .expect("canonical route")
                .expect("task")
                .project_id,
            destination.project_id
        );
        std::mem::forget(root);
    }
}
