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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
            .filter(|(id, access)| id != person && access.allows(area, minimum))
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn managers(&self, person: &str) -> Vec<String> {
        self.members
            .iter()
            .filter(|(id, access)| id != person && access.can_manage_members)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Projects of the organization, read once per request.
pub(super) struct ProjectDirectory {
    org_id: String,
    cache: RefCell<HashMap<String, Option<Rc<ProjectFacts>>>>,
}

impl ProjectDirectory {
    pub fn new(org_id: &str) -> Self {
        Self {
            org_id: org_id.to_string(),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// False on a node where Project Studio is not running: nothing to hand over.
    pub fn available() -> bool {
        ps_db::pool().is_ok()
    }

    pub fn facts(&self, project_id: &str) -> Result<Option<Rc<ProjectFacts>>> {
        if let Some(hit) = self.cache.borrow().get(project_id) {
            return Ok(hit.clone());
        }
        let loaded = match repository::get_project(&self.org_id, project_id)? {
            None => None,
            Some(record) => {
                let members = repository::list_members(project_id)?
                    .into_iter()
                    .map(|m| {
                        let access = repository::project_access(&record, &m.user_id, false)?;
                        Ok((m.user_id, access))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Some(Rc::new(ProjectFacts {
                    id: record.project_id,
                    name: record.name,
                    archived: record.status == "archived",
                    owner: record.owner_user_id,
                    members,
                }))
            }
        };
        self.cache
            .borrow_mut()
            .insert(project_id.to_string(), loaded.clone());
        Ok(loaded)
    }

    /// The projects the person belongs to, or just `only`.
    pub fn projects_of(&self, user: &str, only: Option<&str>) -> Result<Vec<Rc<ProjectFacts>>> {
        if !Self::available() {
            return Ok(Vec::new());
        }
        let mut ids: Vec<String> = match only {
            Some(id) => vec![id.to_string()],
            None => repository::list_projects(&self.org_id, true)?
                .into_iter()
                .filter_map(
                    |project| match repository::member_access(&project.project_id, user) {
                        Ok(Some(_)) => Some(Ok(project.project_id)),
                        Ok(None) => None,
                        Err(error) => Some(Err(error)),
                    },
                )
                .collect::<anyhow::Result<Vec<_>>>()?,
        };
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            if let Some(facts) = self.facts(&id)? {
                if facts.access_of(user).is_some() {
                    out.push(facts);
                }
            }
        }
        Ok(out)
    }
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
            let pool = open_pool(&facts.id)?;
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
        Some(access) if access.allows(area, minimum) => Ok(taker),
        _ => Err("taker_not_eligible"),
    }
}

impl WorkProvider for TaskProvider<'_> {
    fn apply(&self, cx: &ApplyCx<'_>, item: &Planned) -> Result<Step> {
        let Some((project_id, task_id)) = parse_key(&item.key, "task") else {
            return Ok(Step::Refused("bad_key"));
        };
        let Some(facts) = self.projects.facts(project_id)? else {
            return Ok(Step::Refused("project_missing"));
        };
        if facts.archived {
            return Ok(Step::Refused("project_archived"));
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
        let moved = tasks::reassign_open(&pool, task_id, cx.from_user, taker)?;
        let resumed = !moved
            && before
                .as_ref()
                .is_some_and(|t| t.assigned_to == taker && t.status != "done");
        if !moved && !resumed {
            return Ok(Step::Skipped("no_longer_held"));
        }
        // The note is what the taker starts from: it goes on the task itself.
        let already = tasks::list_comments(&pool, task_id)?
            .iter()
            .any(|c| c.author_user_id == cx.actor && c.body_md == cx.note);
        if !already {
            tasks::add_comment(&pool, task_id, cx.actor, cx.note)?;
        }
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
        Ok(Step::Done(json!({
            "status": before.map(|t| t.status).unwrap_or_default(),
        })))
    }

    fn reverse(&self, cx: &ApplyCx<'_>, item: &Recorded) -> Result<Returned> {
        let Some((project_id, task_id)) = parse_key(&item.key, "task") else {
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
        let Some(task) = tasks::get_task(&pool, task_id)? else {
            return Ok(Returned::Kept("closed"));
        };
        if task.status == "done" {
            return Ok(Returned::Kept("closed"));
        }
        if task.assigned_to != taker {
            return Ok(Returned::Kept("changed"));
        }
        if facts
            .access_of(&item.from_user)
            .is_none_or(|access| !access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Write))
        {
            return Ok(Returned::Kept("not_member"));
        }
        if !tasks::reassign_open(&pool, task_id, taker, &item.from_user)? {
            return Ok(Returned::Kept("changed"));
        }
        activity::record(
            &pool,
            cx.actor,
            "system",
            "task.handed_back",
            "task",
            task_id,
            &json!({ "from": taker, "to": item.from_user, "handover_id": cx.handover_id })
                .to_string(),
        );
        notify_pair(
            cx,
            project_id,
            "work_handed_back",
            [item.from_user.as_str(), taker],
            "Zadanie wróciło do właściciela",
            &format!("#{} „{}”", task.task_no, task.title),
            json!({ "project_id": project_id, "task_id": task_id }),
        );
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
    let pool = open_pool(project_id)?;
    if !tasks::open_tasks_of(&pool, user)?.is_empty()
        || !runs::open_items_of(&pool, user)?.is_empty()
    {
        return Ok(MemberRemoval::HoldsWork);
    }
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
            let owner = facts.owner == cx.user_id;
            let eligible = facts
                .members
                .iter()
                .filter(|(id, access)| id != cx.user_id && access.has_access)
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
    fn keys_split_into_project_and_item() {
        assert_eq!(parse_key("task:p-1:t-9", "task"), Some(("p-1", "t-9")));
        assert_eq!(parse_key("test:p-1:i-2", "test"), Some(("p-1", "i-2")));
        assert_eq!(parse_key("task:p-1", "task"), None);
        assert_eq!(parse_key("member:p-1", "task"), None);
        // A prefix must be a whole word: "tasks:..." is not a task key.
        assert_eq!(parse_key("tasks:p-1:t-9", "task"), None);
    }
}
