// ============ File: project_studio/task_transfer.rs — durable task movement and relation recovery ============

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tentaflow_protocol::project_studio::access::{ProjectArea, ProjectPermissionLevel};

use super::models::{
    ProjectRecord, TaskCommentRecord, TaskEventAlias, TaskEventRecord, TaskIndexEvent,
    TaskKeyAlias, TaskLinkAlias, TaskLocation, TaskRecord, TaskRelationRoute, TaskTransferJournal,
};
use crate::db::DbPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub task_ids: Vec<String>,
    pub old_keys: Vec<String>,
    pub destination_type_valid: bool,
    pub widens_access: bool,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferOutcome {
    pub operation_id: String,
    pub task_id: String,
    pub destination_project_id: String,
    pub new_key: String,
    pub moved_task_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransferCopy {
    locations: Vec<TaskLocation>,
    event_aliases: Vec<TaskEventAlias>,
    link_aliases: Vec<TaskLinkAlias>,
    relation_routes: Vec<TaskRelationRoute>,
}

fn require_active_actor(core_db: &DbPool, actor: &str) -> Result<()> {
    let active = crate::db::repository::get_user_account_by_id(core_db, actor)?
        .is_some_and(|account| account.is_active);
    if !active {
        bail!("task transfer actor is inactive");
    }
    Ok(())
}

pub fn preview_transfer(
    source: &ProjectRecord,
    destination: &ProjectRecord,
    source_pool: &DbPool,
    destination_pool: &DbPool,
    task_id: &str,
) -> Result<TransferPlan> {
    if source.project_id == destination.project_id || source.org_id != destination.org_id {
        bail!("task transfer requires different projects in one organization");
    }
    let mut blockers = Vec::new();
    for project in [source, destination] {
        let (modules, _) = super::repository::effective_project_settings(&project.project_id)?;
        if project.status != "active"
            || project.lifecycle != "active"
            || !modules.iter().any(|module| module == "tasks")
            || super::repository::project_write_admission(&project.project_id).is_err()
        {
            if !blockers.contains(&"project_read_only".to_string()) {
                blockers.push("project_read_only".into());
            }
        }
    }
    let tasks = {
        let conn = source_pool
            .read()
            .map_err(|error| anyhow!("task transfer source read: {error}"))?;
        closure_tasks_on(&conn, task_id)?
    };
    if tasks.is_empty() {
        bail!("task transfer source task not found");
    }
    let ids: HashSet<&str> = tasks.iter().map(|row| row.0.as_str()).collect();
    if tasks.iter().any(|row| {
        row.3
            .as_ref()
            .is_some_and(|parent| !ids.contains(parent.as_str()))
    }) {
        blockers.push("hierarchy_requires_group".into());
    }
    let (_, type_source) = super::repository::effective_project_settings(&destination.project_id)?;
    let type_pool = if type_source == destination.project_id {
        destination_pool.clone()
    } else {
        super::project_db::open(&type_source)?
    };
    let type_conn = type_pool
        .read()
        .map_err(|error| anyhow!("task type source read: {error}"))?;
    let mut destination_type_valid = true;
    for (_, _, task_type, _) in &tasks {
        let active: Option<bool> = type_conn
            .query_row(
                "SELECT active FROM task_types WHERE type_id=?1",
                [task_type],
                |row| row.get(0),
            )
            .optional()?;
        if active != Some(true) {
            destination_type_valid = false;
        }
    }
    drop(type_conn);
    if !destination_type_valid {
        blockers.push("destination_type_unavailable".into());
    }
    let pending = super::repository::pending_task_transfers()?;
    if pending
        .iter()
        .any(|operation| tasks.iter().any(|row| operation.task_ids.contains(&row.0)))
    {
        blockers.push("task_transfer_in_progress".into());
    }
    let destination_ids = readable_principals(&destination.project_id)?;
    let source_ids = readable_principals(&source.project_id)?;
    for (task_id, _, _, _) in &tasks {
        let task = super::tasks::get_task(source_pool, task_id)?
            .ok_or_else(|| anyhow!("task disappeared during transfer preview"))?;
        if task.archived_at.is_some() && !blockers.contains(&"task_archived".to_string()) {
            blockers.push("task_archived".into());
        }
        if !task.assigned_to.is_empty() && !destination_ids.contains(&task.assigned_to) {
            blockers.push("assignee_loses_access".into());
            break;
        }
    }
    let mut checked = HashSet::new();
    for (task_id, _, _, _) in &tasks {
        for attachment in super::tasks::task_attachment_inventory(source_pool, task_id)? {
            if checked.insert(attachment.sha256.clone()) {
                if !super::media::is_sha256(&attachment.sha256) {
                    if !blockers.contains(&"attachment_unavailable".to_string()) {
                        blockers.push("attachment_unavailable".into());
                    }
                    continue;
                }
                let path = Path::new(&source.dir_path)
                    .join("files")
                    .join(&attachment.sha256);
                let available = super::media::open_regular(&path).and_then(|file| {
                    if file.metadata()?.len() == attachment.size_bytes {
                        Ok(())
                    } else {
                        bail!("attachment size differs from reference")
                    }
                });
                if available.is_err() && !blockers.contains(&"attachment_unavailable".to_string()) {
                    blockers.push("attachment_unavailable".into());
                }
            }
        }
    }
    let widens_access = !destination_ids.is_subset(&source_ids);
    Ok(TransferPlan {
        task_ids: tasks.iter().map(|row| row.0.clone()).collect(),
        old_keys: tasks.iter().map(|row| row.1.clone()).collect(),
        destination_type_valid,
        widens_access,
        blocking_reasons: blockers,
    })
}

fn closure_tasks_on(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> Result<Vec<(String, String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE closure(task_id) AS (SELECT task_id FROM tasks WHERE task_id=?1 \
         UNION SELECT child.task_id FROM tasks child JOIN closure parent \
         ON child.parent_task_id=parent.task_id) \
         SELECT t.task_id,t.task_key,t.task_type,t.parent_task_id \
         FROM closure c JOIN tasks t ON t.task_id=c.task_id ORDER BY t.task_no",
    )?;
    let rows = stmt.query_map([task_id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    let tasks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tasks)
}

fn readable_principals(project_id: &str) -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    for area in [ProjectArea::Tasks, ProjectArea::Board] {
        for principal in
            super::repository::effective_principals(project_id, area, ProjectPermissionLevel::Read)?
        {
            ids.insert(principal.user_id);
        }
    }
    Ok(ids)
}

#[derive(Debug)]
struct StoredLink {
    route: TaskRelationRoute,
    lag_days: i32,
    created_by: String,
    created_at: String,
}

struct SourceCopy {
    tasks: Vec<TaskRecord>,
    comments: Vec<TaskCommentRecord>,
    events: Vec<TaskEventRecord>,
    links: Vec<StoredLink>,
    attachments: Vec<(String, tentaflow_protocol::project_studio::AttachmentWire)>,
}

fn source_copy(
    source_pool: &DbPool,
    task_ids: &[String],
    routes: &[TaskRelationRoute],
    source_project_id: &str,
) -> Result<SourceCopy> {
    let ids: HashSet<&str> = task_ids.iter().map(String::as_str).collect();
    let mut tasks = Vec::new();
    let mut attachments = Vec::new();
    for task_id in task_ids {
        let task = super::tasks::get_task(source_pool, task_id)?
            .ok_or_else(|| anyhow!("task disappeared during transfer"))?;
        for attachment in super::tasks::task_attachment_inventory(source_pool, task_id)? {
            attachments.push((task_id.clone(), attachment));
        }
        tasks.push(task);
    }
    let conn = source_pool
        .read()
        .map_err(|error| anyhow!("task source read: {error}"))?;
    let mut comments = Vec::new();
    let mut events = Vec::new();
    for task_id in task_ids {
        let mut stmt = conn.prepare(
            "SELECT comment_id,task_id,author_user_id,body_md,created_at,edited_at,mention_user_ids_json \
             FROM task_comments WHERE task_id=?1 ORDER BY created_at,comment_id",
        )?;
        comments.extend(
            stmt.query_map([task_id], |row| {
                Ok(TaskCommentRecord {
                    comment_id: row.get(0)?,
                    task_id: row.get(1)?,
                    author_user_id: row.get(2)?,
                    body_md: row.get(3)?,
                    created_at: row.get(4)?,
                    edited_at: row.get(5)?,
                    mention_user_ids_json: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        let mut stmt = conn.prepare(
            "SELECT event_id,task_id,at,actor_kind,actor_id,kind,before_json,after_json \
             FROM task_events WHERE task_id=?1 ORDER BY event_id",
        )?;
        events.extend(
            stmt.query_map([task_id], |row| {
                Ok(TaskEventRecord {
                    event_id: row.get(0)?,
                    task_id: row.get(1)?,
                    at: row.get(2)?,
                    actor_kind: row.get(3)?,
                    actor_id: row.get(4)?,
                    kind: row.get(5)?,
                    before_json: row.get(6)?,
                    after_json: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    let mut links = Vec::new();
    for route in routes {
        if route.owning_project_id != source_project_id
            || !ids.contains(route.source_task_id.as_str())
        {
            continue;
        }
        let stored = conn.query_row(
            "SELECT relation_id,source_task_id,target_task_id,source_project_id,target_project_id,\
             kind,lag_days,created_by,created_at FROM task_links WHERE link_id=?1",
            [route.link_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i32>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )?;
        if (
            stored.0.as_str(),
            stored.1.as_str(),
            stored.2.as_str(),
            stored.3.as_str(),
            stored.4.as_str(),
            stored.5.as_str(),
        ) != (
            route.relation_id.as_str(),
            route.source_task_id.as_str(),
            route.target_task_id.as_str(),
            route.source_project_id.as_str(),
            route.target_project_id.as_str(),
            route.kind.as_str(),
        ) {
            bail!("canonical task relation changed during transfer");
        }
        links.push(StoredLink {
            route: route.clone(),
            lag_days: stored.6,
            created_by: stored.7,
            created_at: stored.8,
        });
    }
    Ok(SourceCopy {
        tasks,
        comments,
        events,
        links,
        attachments,
    })
}

fn incident_routes(task_ids: &[String]) -> Result<Vec<TaskRelationRoute>> {
    let mut routes = HashMap::new();
    for task_id in task_ids {
        for route in super::repository::relation_routes_for_task(task_id)? {
            routes.insert(route.relation_id.clone(), route);
        }
    }
    let mut routes: Vec<_> = routes.into_values().collect();
    routes.sort_by(|left, right| left.relation_id.cmp(&right.relation_id));
    Ok(routes)
}

fn verify_hardlink(source: &Path, destination: &Path, sha: &str, size: u64) -> Result<()> {
    #[cfg(unix)]
    let mut created = false;
    if destination.exists() {
        let meta = std::fs::symlink_metadata(destination)?;
        if !meta.file_type().is_file() {
            bail!("transfer target is not a regular file");
        }
    } else {
        std::fs::hard_link(source, destination).with_context(|| {
            format!(
                "immutable transfer hardlink {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
        #[cfg(unix)]
        {
            created = true;
        }
    }
    let meta = std::fs::symlink_metadata(destination)?;
    if meta.len() != size || super::media::hash_file(destination, &|| Ok(()))? != sha {
        bail!("transfer target hash differs from immutable source");
    }
    #[cfg(unix)]
    if created {
        std::fs::File::open(destination.parent().expect("hardlink has a parent"))?.sync_all()?;
    }
    Ok(())
}

fn link_media(
    source: &ProjectRecord,
    destination: &ProjectRecord,
    previews: &[super::media::TransferPreview],
) -> Result<()> {
    let source_files = Path::new(&source.dir_path).join("files");
    let destination_files =
        super::media::safe_directory(Path::new(&destination.dir_path), "files")?;
    let destination_previews = super::media::safe_directory(&destination_files, ".previews")?;
    for preview in previews {
        if !super::media::is_sha256(&preview.sha256) {
            bail!("invalid transfer SHA-256");
        }
        verify_hardlink(
            &source_files.join(&preview.sha256),
            &destination_files.join(&preview.sha256),
            &preview.sha256,
            preview.original_size,
        )?;
        if let Some(ready) = &preview.ready {
            if !super::media::is_sha256(&ready.sha256) {
                bail!("invalid preview SHA-256");
            }
            verify_hardlink(
                &source_files
                    .join(".previews")
                    .join(format!("{}.mp4", preview.sha256)),
                &destination_previews.join(format!("{}.mp4", preview.sha256)),
                &ready.sha256,
                ready.size_bytes,
            )?;
        }
    }
    Ok(())
}

fn transfer_pools(
    source: &ProjectRecord,
    destination: &ProjectRecord,
    source_pool: &DbPool,
    destination_pool: &DbPool,
    routes: &[TaskRelationRoute],
    type_source_id: &str,
) -> Result<Vec<(String, DbPool)>> {
    let mut pools = HashMap::new();
    pools.insert(source.project_id.clone(), source_pool.clone());
    pools.insert(destination.project_id.clone(), destination_pool.clone());
    for project_id in routes
        .iter()
        .map(|route| route.owning_project_id.as_str())
        .chain(std::iter::once(type_source_id))
    {
        if !pools.contains_key(project_id) {
            pools.insert(project_id.to_string(), super::project_db::open(project_id)?);
        }
    }
    let mut pools: Vec<_> = pools.into_iter().collect();
    pools.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(pools)
}

fn copy_into_destination(
    source_project: &ProjectRecord,
    destination: &ProjectRecord,
    destination_conn: &rusqlite::Connection,
    catalogue: &rusqlite::Connection,
    source: &SourceCopy,
    routes: &[TaskRelationRoute],
    actor: &str,
    operation_id: &str,
) -> Result<(TransferCopy, HashMap<String, String>, Vec<TaskIndexEvent>)> {
    let moved: HashSet<&str> = source
        .tasks
        .iter()
        .map(|task| task.task_id.as_str())
        .collect();
    let tx = destination_conn.unchecked_transaction()?;
    for task in &source.tasks {
        tx.execute(
            "DELETE FROM task_links WHERE source_task_id=?1",
            [&task.task_id],
        )?;
        tx.execute(
            "DELETE FROM task_comments WHERE task_id=?1",
            [&task.task_id],
        )?;
        tx.execute("DELETE FROM task_events WHERE task_id=?1", [&task.task_id])?;
        tx.execute("DELETE FROM tasks WHERE task_id=?1", [&task.task_id])?;
    }
    let prefix: String = tx.query_row(
        "SELECT value FROM settings WHERE key='project_key_prefix'",
        [],
        |row| row.get(0),
    )?;
    if prefix != destination.key_prefix {
        bail!("destination key prefix differs from registry");
    }
    let mut next_no: i64 = tx.query_row(
        "SELECT CAST(value AS INTEGER) FROM settings WHERE key='task_next_no'",
        [],
        |row| row.get(0),
    )?;
    let mut locations = Vec::new();
    let mut key_map = HashMap::new();
    for task in &source.tasks {
        let active: Option<bool> = catalogue
            .query_row(
                "SELECT active FROM task_types WHERE type_id=?1",
                [&task.task_type],
                |row| row.get(0),
            )
            .optional()?;
        if active != Some(true) {
            bail!("destination task type is unavailable");
        }
        let (number, key) = loop {
            let number = next_no;
            next_no = next_no
                .checked_add(1)
                .ok_or_else(|| anyhow!("task number exhausted"))?;
            let key = format!("{prefix}-{number}");
            if !super::repository::task_key_reserved(&destination.org_id, &key)? {
                break (number, key);
            }
        };
        tx.execute(
            "INSERT INTO tasks(task_id,task_no,task_key,task_type,title,description_md,severity,\
             priority,status,assigned_to,due_date,parent_task_id,links_json,attachments_json,\
             created_by,created_at,updated_at,archived_at,resolution,resolution_reason) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            params![
                task.task_id,
                number,
                key,
                task.task_type,
                task.title,
                task.description_md,
                task.severity,
                task.priority,
                task.status,
                task.assigned_to,
                task.due_date,
                task.parent_task_id,
                task.links_json,
                task.attachments_json,
                task.created_by,
                task.created_at,
                task.updated_at,
                task.archived_at,
                task.resolution,
                task.resolution_reason
            ],
        )?;
        key_map.insert(task.task_id.clone(), key.clone());
        locations.push(TaskLocation {
            task_id: task.task_id.clone(),
            org_id: destination.org_id.clone(),
            project_id: destination.project_id.clone(),
            current_key: key,
        });
    }
    tx.execute(
        "UPDATE settings SET value=?1 WHERE key='task_next_no'",
        [next_no.to_string()],
    )?;
    tx.execute(
        "UPDATE settings SET value='1' WHERE key='project_key_prefix_locked'",
        [],
    )?;
    for comment in &source.comments {
        tx.execute(
            "INSERT INTO task_comments(comment_id,task_id,author_user_id,body_md,created_at,\
             edited_at,mention_user_ids_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                comment.comment_id,
                comment.task_id,
                comment.author_user_id,
                comment.body_md,
                comment.created_at,
                comment.edited_at,
                comment.mention_user_ids_json
            ],
        )?;
    }
    let mut event_aliases = Vec::new();
    for event in &source.events {
        tx.execute(
            "INSERT INTO task_events(task_id,at,actor_kind,actor_id,kind,before_json,after_json) \
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                event.task_id,
                event.at,
                event.actor_kind,
                event.actor_id,
                event.kind,
                event.before_json,
                event.after_json
            ],
        )?;
        event_aliases.push(TaskEventAlias {
            task_id: event.task_id.clone(),
            origin_project_id: source_project.project_id.clone(),
            origin_event_id: event.event_id,
            current_project_id: destination.project_id.clone(),
            current_event_id: tx.last_insert_rowid(),
        });
    }
    for task in &source.tasks {
        let new_key = key_map
            .get(&task.task_id)
            .expect("moved task has destination key");
        let before = serde_json::json!({
            "project_id":source_project.project_id,"project_name":source_project.name,
            "task_key":task.task_key,
        });
        let after = serde_json::json!({
            "project_id":destination.project_id,"project_name":destination.name,
            "task_key":new_key,"operation_id":operation_id,
        });
        tx.execute(
            "INSERT INTO task_events(task_id,actor_kind,actor_id,kind,before_json,after_json) \
             VALUES (?1,'user',?2,'transferred',?3,?4)",
            params![task.task_id, actor, before.to_string(), after.to_string()],
        )?;
    }
    let mut moved_link_ids = HashMap::new();
    let mut link_aliases = Vec::new();
    for link in &source.links {
        let target_project_id = if moved.contains(link.route.target_task_id.as_str()) {
            &destination.project_id
        } else {
            &link.route.target_project_id
        };
        tx.execute(
            "INSERT INTO task_links(relation_id,source_task_id,target_task_id,source_project_id,\
             target_project_id,kind,lag_days,created_by,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![link.route.relation_id,link.route.source_task_id,link.route.target_task_id,
                destination.project_id,target_project_id,link.route.kind,link.lag_days,
                link.created_by,link.created_at],
        )?;
        let new_link_id = tx.last_insert_rowid();
        moved_link_ids.insert(link.route.relation_id.clone(), new_link_id);
        link_aliases.push(TaskLinkAlias {
            relation_id: link.route.relation_id.clone(),
            origin_project_id: source_project.project_id.clone(),
            origin_link_id: link.route.link_id,
            current_project_id: destination.project_id.clone(),
            current_link_id: new_link_id,
        });
    }
    let relation_routes = routes
        .iter()
        .map(|old| {
            let mut route = old.clone();
            if moved.contains(route.source_task_id.as_str()) {
                route.source_project_id = destination.project_id.clone();
                route.owning_project_id = destination.project_id.clone();
                route.link_id = *moved_link_ids
                    .get(&route.relation_id)
                    .expect("moved source relation was copied");
            }
            if moved.contains(route.target_task_id.as_str()) {
                route.target_project_id = destination.project_id.clone();
            }
            route
        })
        .collect();
    let mut snapshots = Vec::new();
    for task in &source.tasks {
        let (task_id, snapshot_json): (String, String) = tx.query_row(
            "SELECT task_id,snapshot_json FROM task_index_snapshots WHERE task_id=?1",
            [&task.task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        snapshots.push(TaskIndexEvent {
            revision: 0,
            task_id,
            op: "upsert".into(),
            snapshot_json,
        });
    }
    tx.commit()?;
    Ok((
        TransferCopy {
            locations,
            event_aliases,
            link_aliases,
            relation_routes,
        },
        key_map,
        snapshots,
    ))
}

fn copy_prepared(
    state: &Arc<crate::dispatch::AppState>,
    operation: &TaskTransferJournal,
    source: &ProjectRecord,
    destination: &ProjectRecord,
    source_pool: &DbPool,
    destination_pool: &DbPool,
) -> Result<TransferCopy> {
    require_active_actor(&state.db, &operation.actor_user_id)?;
    let routes = incident_routes(&operation.task_ids)?;
    let source_data = source_copy(
        source_pool,
        &operation.task_ids,
        &routes,
        &source.project_id,
    )?;
    let previews = super::media::prepare_transfer(
        state,
        source,
        source_pool,
        &operation.operation_id,
        &operation.actor_user_id,
        &source_data.attachments,
    )?;
    link_media(source, destination, &previews)?;
    let (_, type_source_id) =
        super::repository::effective_project_settings(&destination.project_id)?;
    let pools = transfer_pools(
        source,
        destination,
        source_pool,
        destination_pool,
        &routes,
        &type_source_id,
    )?;
    let writers = pools
        .iter()
        .map(|(_, pool)| {
            pool.write()
                .map_err(|error| anyhow!("task transfer project write: {error}"))
        })
        .collect::<Result<Vec<_>>>()?;
    super::repository::project_write_admission(&source.project_id)?;
    super::repository::project_write_admission(&destination.project_id)?;
    let (_, current_type_source) =
        super::repository::effective_project_settings(&destination.project_id)?;
    if current_type_source != type_source_id {
        bail!("destination task type source changed during transfer");
    }
    let destination_index = pools
        .iter()
        .position(|(id, _)| id == &destination.project_id)
        .expect("destination pool is preopened");
    let catalogue_index = pools
        .iter()
        .position(|(id, _)| id == &type_source_id)
        .expect("catalogue pool is preopened");
    let current_source = super::repository::project_record(&source.project_id)?
        .ok_or_else(|| anyhow!("transfer source project disappeared"))?;
    let current_destination = super::repository::project_record(&destination.project_id)?
        .ok_or_else(|| anyhow!("transfer destination project disappeared"))?;
    if current_source.dir_path != source.dir_path
        || current_destination.dir_path != destination.dir_path
    {
        bail!("project storage path changed during transfer");
    }
    let (copy, key_map, snapshots) = copy_into_destination(
        &current_source,
        &current_destination,
        &writers[destination_index],
        &writers[catalogue_index],
        &source_data,
        &routes,
        &operation.actor_user_id,
        &operation.operation_id,
    )?;
    super::repository::stage_task_transfer_index(&operation.operation_id, &snapshots)?;
    super::repository::mark_task_transfer_copied(
        &operation.operation_id,
        &serde_json::to_string(&previews)?,
        &serde_json::to_string(&key_map)?,
        &serde_json::to_string(&copy)?,
    )?;
    publish_copied(&state.db, operation, &copy)?;
    Ok(copy)
}

fn publish_copied(
    core_db: &DbPool,
    operation: &TaskTransferJournal,
    copy: &TransferCopy,
) -> Result<()> {
    require_active_actor(core_db, &operation.actor_user_id)?;
    super::repository::publish_task_transfer(
        &operation.operation_id,
        &copy.locations,
        &[] as &[TaskKeyAlias],
        &copy.event_aliases,
        &copy.link_aliases,
        &copy.relation_routes,
    )
}

fn copied_manifest(operation: &TaskTransferJournal) -> Result<TransferCopy> {
    let copy: TransferCopy =
        serde_json::from_str(&operation.event_map_json).context("task transfer copy manifest")?;
    let expected: HashSet<&str> = operation.task_ids.iter().map(String::as_str).collect();
    let actual: HashSet<&str> = copy
        .locations
        .iter()
        .map(|location| location.task_id.as_str())
        .collect();
    if expected != actual || copy.locations.len() != actual.len() {
        bail!("task transfer copy manifest has a different task set");
    }
    Ok(copy)
}

pub fn transfer_task(
    state: &Arc<crate::dispatch::AppState>,
    source: &ProjectRecord,
    destination: &ProjectRecord,
    source_pool: &DbPool,
    destination_pool: &DbPool,
    task_id: &str,
    actor: &str,
    confirm_wider_access: bool,
) -> Result<TransferOutcome> {
    require_active_actor(&state.db, actor)?;
    let plan = preview_transfer(source, destination, source_pool, destination_pool, task_id)?;
    if !plan.blocking_reasons.is_empty() {
        bail!(
            "task transfer blocked: {}",
            plan.blocking_reasons.join(", ")
        );
    }
    if plan.widens_access && !confirm_wider_access {
        bail!("task transfer would widen access without consent");
    }
    let status = super::task_index::sync_project(source, source_pool, 4096)?;
    if status.lag != 0 {
        bail!("task source index has not caught up");
    }
    let operation = TaskTransferJournal {
        operation_id: uuid::Uuid::new_v4().to_string(),
        org_id: source.org_id.clone(),
        source_project_id: source.project_id.clone(),
        destination_project_id: destination.project_id.clone(),
        actor_user_id: actor.to_string(),
        task_ids: plan.task_ids.clone(),
        consent_wider_access: confirm_wider_access,
        phase: "prepared".into(),
        sha_manifest_json: "[]".into(),
        key_map_json: "{}".into(),
        event_map_json: "{}".into(),
    };
    {
        let source_writer = source_pool
            .write()
            .map_err(|error| anyhow!("task transfer source write: {error}"))?;
        let actual = closure_tasks_on(&source_writer, task_id)?;
        if actual
            .iter()
            .map(|row| (&row.0, &row.1))
            .collect::<Vec<_>>()
            != plan.task_ids.iter().zip(&plan.old_keys).collect::<Vec<_>>()
        {
            bail!("task hierarchy changed during transfer preparation");
        }
        super::repository::prepare_task_transfer(&operation)?;
    }
    let copy = match copy_prepared(
        state,
        &operation,
        source,
        destination,
        source_pool,
        destination_pool,
    ) {
        Ok(copy) => copy,
        Err(error) => {
            let current = super::repository::pending_task_transfers()?
                .into_iter()
                .find(|candidate| candidate.operation_id == operation.operation_id);
            if current
                .as_ref()
                .is_some_and(|candidate| candidate.phase == "prepared")
            {
                discard_prepared_copy(state, source, source_pool, &operation, destination_pool)
                    .context(
                        "task transfer failed and its prepared copy could not be rolled back",
                    )?;
            }
            return Err(error);
        }
    };
    let published = super::repository::pending_task_transfers()?
        .into_iter()
        .find(|pending| pending.operation_id == operation.operation_id)
        .ok_or_else(|| anyhow!("task transfer journal disappeared"))?;
    cleanup_published(
        state,
        &published,
        source,
        destination,
        source_pool,
        destination_pool,
    )?;
    let new_key = copy
        .locations
        .iter()
        .find(|location| location.task_id == task_id)
        .map(|location| location.current_key.clone())
        .ok_or_else(|| anyhow!("requested task is absent from transfer closure"))?;
    Ok(TransferOutcome {
        operation_id: operation.operation_id,
        task_id: task_id.to_string(),
        destination_project_id: destination.project_id.clone(),
        new_key,
        moved_task_ids: plan.task_ids,
    })
}

fn cleanup_published(
    state: &Arc<crate::dispatch::AppState>,
    operation: &TaskTransferJournal,
    source: &ProjectRecord,
    destination: &ProjectRecord,
    source_pool: &DbPool,
    destination_pool: &DbPool,
) -> Result<()> {
    let copy = copied_manifest(operation)?;
    let previews: Vec<super::media::TransferPreview> =
        serde_json::from_str(&operation.sha_manifest_json)?;
    link_media(source, destination, &previews)?;
    let pools = transfer_pools(
        source,
        destination,
        source_pool,
        destination_pool,
        &copy.relation_routes,
        &destination.project_id,
    )?;
    {
        let writers = pools
            .iter()
            .map(|(_, pool)| {
                pool.write()
                    .map_err(|error| anyhow!("task transfer cleanup write: {error}"))
            })
            .collect::<Result<Vec<_>>>()?;
        for location in &copy.locations {
            let current = super::repository::task_location(&location.task_id)?
                .ok_or_else(|| anyhow!("published task location missing"))?;
            if current != *location {
                bail!("published task location changed before cleanup");
            }
            let destination_index = pools
                .iter()
                .position(|(id, _)| id == &destination.project_id)
                .expect("destination pool is preopened");
            let exists: i64 = writers[destination_index].query_row(
                "SELECT COUNT(*) FROM tasks WHERE task_id=?1",
                [&location.task_id],
                |row| row.get(0),
            )?;
            if exists != 1 {
                bail!("published task content missing from destination");
            }
        }
        for route in &copy.relation_routes {
            let owner_index = pools
                .iter()
                .position(|(id, _)| id == &route.owning_project_id)
                .ok_or_else(|| anyhow!("task relation owner pool missing"))?;
            let updated = writers[owner_index].execute(
                "UPDATE task_links SET source_project_id=?1,target_project_id=?2 \
                 WHERE relation_id=?3 AND link_id=?4",
                params![
                    route.source_project_id,
                    route.target_project_id,
                    route.relation_id,
                    route.link_id
                ],
            )?;
            if updated != 1 {
                bail!("published canonical task relation row missing");
            }
        }
        let source_index = pools
            .iter()
            .position(|(id, _)| id == &source.project_id)
            .expect("source pool is preopened");
        let tx = writers[source_index].unchecked_transaction()?;
        for task_id in &operation.task_ids {
            tx.execute("DELETE FROM task_links WHERE source_task_id=?1", [task_id])?;
            tx.execute("DELETE FROM task_comments WHERE task_id=?1", [task_id])?;
            tx.execute("DELETE FROM task_events WHERE task_id=?1", [task_id])?;
            tx.execute("DELETE FROM tasks WHERE task_id=?1", [task_id])?;
        }
        tx.commit()?;
    }
    super::media::resume_transfer(
        state,
        destination,
        destination_pool,
        &operation.actor_user_id,
        &previews,
    )?;
    super::media::finish_transfer(state, source, &operation.operation_id)?;
    for (project_id, pool) in &pools {
        let project = super::repository::project_record(project_id)?
            .ok_or_else(|| anyhow!("task relation owner project missing"))?;
        loop {
            let status = super::task_index::sync_project(&project, pool, 4096)?;
            if status.lag == 0 {
                break;
            }
        }
        reconcile_project_relations(&project, pool)?;
    }
    super::repository::mark_task_transfer_cleaned(&operation.operation_id)?;
    Ok(())
}

fn discard_prepared_copy(
    state: &Arc<crate::dispatch::AppState>,
    source: &ProjectRecord,
    source_pool: &DbPool,
    operation: &TaskTransferJournal,
    destination_pool: &DbPool,
) -> Result<()> {
    super::media::abort_transfer(state, source, source_pool, &operation.operation_id)?;
    let conn = destination_pool
        .write()
        .map_err(|error| anyhow!("task transfer rollback write: {error}"))?;
    let tx = conn.unchecked_transaction()?;
    for task_id in &operation.task_ids {
        tx.execute("DELETE FROM task_links WHERE source_task_id=?1", [task_id])?;
        tx.execute("DELETE FROM task_comments WHERE task_id=?1", [task_id])?;
        tx.execute("DELETE FROM task_events WHERE task_id=?1", [task_id])?;
        tx.execute("DELETE FROM tasks WHERE task_id=?1", [task_id])?;
    }
    tx.commit()?;
    drop(conn);
    super::repository::abort_task_transfer(&operation.operation_id)?;
    super::media::finish_transfer(state, source, &operation.operation_id)?;
    Ok(())
}

pub fn recover_transfers(state: &Arc<crate::dispatch::AppState>) -> Result<()> {
    for operation in super::repository::pending_task_transfers()? {
        let source = super::repository::project_record(&operation.source_project_id)?
            .ok_or_else(|| anyhow!("task transfer source project missing"))?;
        let destination = super::repository::project_record(&operation.destination_project_id)?
            .ok_or_else(|| anyhow!("task transfer destination project missing"))?;
        let source_pool = super::project_db::open(&source.project_id)?;
        let destination_pool = super::project_db::open(&destination.project_id)?;
        match operation.phase.as_str() {
            "prepared" => {
                if let Err(error) = copy_prepared(
                    state,
                    &operation,
                    &source,
                    &destination,
                    &source_pool,
                    &destination_pool,
                ) {
                    let current = super::repository::pending_task_transfers()?
                        .into_iter()
                        .find(|candidate| candidate.operation_id == operation.operation_id)
                        .ok_or_else(|| anyhow!("task transfer disappeared during copy recovery"))?;
                    if current.phase == "prepared" {
                        discard_prepared_copy(
                            state,
                            &source,
                            &source_pool,
                            &operation,
                            &destination_pool,
                        )
                        .context("task transfer rollback after incomplete copy")?;
                        loop {
                            let status = super::task_index::sync_project(
                                &destination,
                                &destination_pool,
                                4096,
                            )?;
                            if status.lag == 0 {
                                break;
                            }
                        }
                        tracing::warn!(operation_id=%operation.operation_id,
                            "unpublished task transfer rolled back after copy failure: {error}");
                        continue;
                    }
                    return Err(error)
                        .context("copied task transfer needs recovery before serving requests");
                }
                let refreshed = super::repository::pending_task_transfers()?
                    .into_iter()
                    .find(|candidate| candidate.operation_id == operation.operation_id)
                    .ok_or_else(|| anyhow!("task transfer disappeared during recovery"))?;
                let _copy = copied_manifest(&refreshed)?;
                let published = super::repository::pending_task_transfers()?
                    .into_iter()
                    .find(|candidate| candidate.operation_id == operation.operation_id)
                    .ok_or_else(|| anyhow!("task transfer disappeared after publication"))?;
                cleanup_published(
                    state,
                    &published,
                    &source,
                    &destination,
                    &source_pool,
                    &destination_pool,
                )?;
            }
            "copied" => {
                let copy = copied_manifest(&operation)?;
                link_media(
                    &source,
                    &destination,
                    &serde_json::from_str::<Vec<super::media::TransferPreview>>(
                        &operation.sha_manifest_json,
                    )?,
                )?;
                publish_copied(&state.db, &operation, &copy)?;
                let published = super::repository::pending_task_transfers()?
                    .into_iter()
                    .find(|candidate| candidate.operation_id == operation.operation_id)
                    .ok_or_else(|| anyhow!("task transfer disappeared after publication"))?;
                cleanup_published(
                    state,
                    &published,
                    &source,
                    &destination,
                    &source_pool,
                    &destination_pool,
                )?;
            }
            "published" => cleanup_published(
                state,
                &operation,
                &source,
                &destination,
                &source_pool,
                &destination_pool,
            )?,
            other => bail!("unexpected task transfer phase: {other}"),
        }
    }
    Ok(())
}

fn recover_relation_deletions(project: &ProjectRecord, source_pool: &DbPool) -> Result<()> {
    for route in super::repository::pending_task_relation_deletions(&project.project_id)? {
        let mut pools = vec![(project.project_id.clone(), source_pool.clone())];
        if route.target_project_id != project.project_id {
            pools.push((
                route.target_project_id.clone(),
                super::project_db::open(&route.target_project_id)?,
            ));
        }
        pools.sort_by(|left, right| left.0.cmp(&right.0));
        let writers = pools
            .iter()
            .map(|(_, pool)| {
                pool.write()
                    .map_err(|error| anyhow!("task relation recovery write: {error}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let source_index = pools
            .iter()
            .position(|(id, _)| id == &project.project_id)
            .expect("source pool is preopened");
        let source = &writers[source_index];
        let row_exists: i64 = source.query_row(
            "SELECT COUNT(*) FROM task_links WHERE relation_id=?1 AND link_id=?2",
            params![route.relation_id, route.link_id],
            |row| row.get(0),
        )?;
        if row_exists != 0 {
            super::repository::abort_task_relation_deletion(&route.relation_id)?;
            continue;
        }
        let (before_json, actor): (String, String) = source
            .query_row(
                "SELECT before_json,actor_id FROM task_events WHERE task_id=?1 \
                 AND kind='link_deleted' AND json_extract(before_json,'$.relation_id')=?2 \
                 ORDER BY event_id DESC LIMIT 1",
                params![route.source_task_id, route.relation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("deleted relation lacks its atomic source event")?;
        let event: serde_json::Value = serde_json::from_str(&before_json)?;
        if event["source_task_id"].as_str() != Some(route.source_task_id.as_str())
            || event["target_task_id"].as_str() != Some(route.target_task_id.as_str())
            || event["source_project_id"].as_str() != Some(route.source_project_id.as_str())
            || event["target_project_id"].as_str() != Some(route.target_project_id.as_str())
            || event["kind"].as_str() != Some(route.kind.as_str())
        {
            bail!("deleted relation source event differs from admitted route");
        }
        let target_index = pools
            .iter()
            .position(|(id, _)| id == &route.target_project_id)
            .expect("target pool is preopened");
        let target = &writers[target_index];
        let target_event: i64 = target.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_events WHERE task_id=?1 AND kind='link_deleted' \
             AND json_extract(before_json,'$.relation_id')=?2)",
            params![route.target_task_id, route.relation_id],
            |row| row.get(0),
        )?;
        if target_event == 0 {
            if route.target_project_id == project.project_id {
                bail!("same-project relation deletion lost its atomic target event");
            }
            let tx = target.unchecked_transaction()?;
            let task_exists: i64 = tx.query_row(
                "SELECT COUNT(*) FROM tasks WHERE task_id=?1",
                [&route.target_task_id],
                |row| row.get(0),
            )?;
            if task_exists != 1 {
                bail!("relation target task missing during deletion recovery");
            }
            tx.execute(
                "INSERT INTO task_events(task_id,actor_kind,actor_id,kind,before_json,after_json) \
                 VALUES (?1,'user',?2,'link_deleted',?3,'null')",
                params![route.target_task_id, actor, before_json],
            )?;
            tx.commit()?;
        }
        super::repository::finish_task_relation_deletion(&route.relation_id)?;
    }
    Ok(())
}

pub fn reconcile_project_relations(project: &ProjectRecord, pool: &DbPool) -> Result<()> {
    recover_relation_deletions(project, pool)?;
    let source_rows: HashMap<String, TaskRelationRoute> = {
        let conn = pool
            .read()
            .map_err(|error| anyhow::anyhow!("task relation read: {error}"))?;
        let mut stmt = conn.prepare(
            "SELECT relation_id,link_id,source_task_id,target_task_id,source_project_id,\
             target_project_id,kind FROM task_links ORDER BY link_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TaskRelationRoute {
                relation_id: row.get(0)?,
                owning_project_id: project.project_id.clone(),
                link_id: row.get(1)?,
                source_task_id: row.get(2)?,
                target_task_id: row.get(3)?,
                source_project_id: row.get(4)?,
                target_project_id: row.get(5)?,
                kind: row.get(6)?,
            })
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let route = row?;
            if route.relation_id.is_empty()
                || route.source_project_id != project.project_id
                || map.insert(route.relation_id.clone(), route).is_some()
            {
                bail!("invalid canonical task relation row");
            }
        }
        map
    };
    let active = super::repository::relation_routes_owned_by(&project.project_id)?;
    let pending = super::repository::pending_task_relation_admissions()?;
    let pending_ids: HashSet<&str> = pending
        .iter()
        .map(|route| route.relation_id.as_str())
        .collect();
    let active_ids: HashSet<&str> = active
        .iter()
        .map(|route| route.relation_id.as_str())
        .collect();
    for route in &active {
        match source_rows.get(&route.relation_id) {
            Some(row) if row == route => {}
            Some(_) => bail!("task relation route differs from canonical row"),
            None => bail!("task relation route has no canonical row or deletion admission"),
        }
    }
    for row in source_rows.values() {
        if active_ids.contains(row.relation_id.as_str()) {
            continue;
        }
        if pending_ids.contains(row.relation_id.as_str())
            && row.target_project_id != project.project_id
        {
            replay_remote_link_event(pool, row)?;
        }
        let pending = TaskRelationRoute {
            link_id: 0,
            ..row.clone()
        };
        super::repository::prepare_task_relation_route(&pending)?;
        super::repository::publish_task_relation_route(row)?;
    }
    for pending in pending {
        if pending.owning_project_id == project.project_id
            && !source_rows.contains_key(&pending.relation_id)
        {
            super::repository::abort_task_relation_route(&pending.relation_id)?;
        }
    }
    Ok(())
}

fn replay_remote_link_event(source_pool: &DbPool, route: &TaskRelationRoute) -> Result<()> {
    let (lag_days, actor): (i32, String) = source_pool
        .read()
        .map_err(|error| anyhow!("task relation read: {error}"))?
        .query_row(
            "SELECT lag_days,created_by FROM task_links WHERE relation_id=?1 AND link_id=?2",
            params![route.relation_id, route.link_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    let target_pool = super::project_db::open(&route.target_project_id)?;
    let conn = target_pool
        .write()
        .map_err(|error| anyhow!("task link history write: {error}"))?;
    let tx = conn.unchecked_transaction()?;
    let exists: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_events WHERE task_id=?1 AND kind='link_added' \
         AND json_extract(after_json,'$.relation_id')=?2)",
        params![route.target_task_id, route.relation_id],
        |row| row.get(0),
    )?;
    if exists == 0 {
        let target_exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_id=?1",
            [&route.target_task_id],
            |row| row.get(0),
        )?;
        if target_exists != 1 {
            bail!("task relation target missing during history recovery");
        }
        let payload = serde_json::json!({
            "relation_id":route.relation_id,"link_id":route.link_id,
            "source_task_id":route.source_task_id,"target_task_id":route.target_task_id,
            "source_project_id":route.source_project_id,"target_project_id":route.target_project_id,
            "kind":route.kind,"lag_days":lag_days,
        });
        tx.execute(
            "INSERT INTO task_events(task_id,actor_kind,actor_id,kind,before_json,after_json) \
             VALUES (?1,'user',?2,'link_added','null',?3)",
            params![route.target_task_id, actor, payload.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tasks::{self, TaskInput};
    use super::*;

    #[test]
    fn immutable_media_transfer_links_original_and_ready_preview() {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let content = tempfile::tempdir().expect("content directory");
        let source_dir = content.path().join("source");
        let destination_dir = content.path().join("destination");
        for dir in [&source_dir, &destination_dir] {
            std::fs::create_dir_all(dir.join("files/.previews")).expect("media directory");
        }
        let original = source_dir.join("files/original.bin");
        let preview = source_dir.join("files/.previews/ready.mp4");
        std::fs::write(&original, b"immutable task attachment").expect("original content");
        std::fs::write(&preview, b"immutable fast-start preview").expect("ready preview");
        let original_sha =
            super::super::media::hash_file(&original, &|| Ok(())).expect("original SHA-256");
        let ready_sha =
            super::super::media::hash_file(&preview, &|| Ok(())).expect("preview SHA-256");
        std::fs::rename(&original, source_dir.join("files").join(&original_sha))
            .expect("content address original");
        std::fs::rename(
            &preview,
            source_dir
                .join("files/.previews")
                .join(format!("{original_sha}.mp4")),
        )
        .expect("content address preview");
        let source_metadata = source_dir
            .join("files/.previews")
            .join(format!("{original_sha}.json"));
        std::fs::write(
            &source_metadata,
            br#"{"status":"ready","duration_ms":1200}"#,
        )
        .expect("source preview metadata");
        let source = ProjectRecord {
            dir_path: source_dir.to_string_lossy().into_owned(),
            ..empty_media_project("source")
        };
        let destination = ProjectRecord {
            dir_path: destination_dir.to_string_lossy().into_owned(),
            ..empty_media_project("destination")
        };
        let manifest = super::super::media::TransferPreview {
            sha256: original_sha.clone(),
            original_size: b"immutable task attachment".len() as u64,
            owner_task_id: uuid::Uuid::new_v4().to_string(),
            ready: Some(super::super::media::ReadyPreview {
                sha256: ready_sha.clone(),
                size_bytes: b"immutable fast-start preview".len() as u64,
                duration_ms: 1200,
            }),
            requeue: false,
        };
        link_media(&source, &destination, &[manifest.clone()]).expect("durable hardlinks");
        let source_original = source_dir.join("files").join(&original_sha);
        let target_original = destination_dir.join("files").join(&original_sha);
        let source_preview = source_dir
            .join("files/.previews")
            .join(format!("{original_sha}.mp4"));
        let target_preview = destination_dir
            .join("files/.previews")
            .join(format!("{original_sha}.mp4"));
        assert_eq!(
            super::super::media::hash_file(&target_original, &|| Ok(())).expect("target hash"),
            original_sha
        );
        assert_eq!(
            super::super::media::hash_file(&target_preview, &|| Ok(())).expect("preview hash"),
            ready_sha
        );
        #[cfg(unix)]
        {
            let source_original_meta = std::fs::metadata(&source_original).expect("source inode");
            let target_original_meta = std::fs::metadata(&target_original).expect("target inode");
            assert_eq!(source_original_meta.dev(), target_original_meta.dev());
            assert_eq!(source_original_meta.ino(), target_original_meta.ino());
            let source_preview_meta =
                std::fs::metadata(&source_preview).expect("source preview inode");
            let target_preview_meta =
                std::fs::metadata(&target_preview).expect("target preview inode");
            assert_eq!(source_preview_meta.dev(), target_preview_meta.dev());
            assert_eq!(source_preview_meta.ino(), target_preview_meta.ino());
        }
        assert_eq!(
            std::fs::read(&source_metadata).expect("metadata preserved"),
            br#"{"status":"ready","duration_ms":1200}"#
        );
        assert!(!destination_dir
            .join("files/.previews")
            .join(format!("{original_sha}.json"))
            .exists());
        let blocked = destination_dir.join("files/.previews/blocked.mp4");
        std::fs::create_dir(&blocked).expect("preexisting unsupported destination");
        assert!(verify_hardlink(
            &source_preview,
            &blocked,
            &ready_sha,
            manifest.ready.unwrap().size_bytes
        )
        .is_err());
        assert!(source_original.exists());
        assert!(source_preview.exists());
    }

    fn empty_media_project(project_id: &str) -> ProjectRecord {
        ProjectRecord {
            project_id: project_id.into(),
            org_id: "organization".into(),
            key_prefix: "MED".into(),
            name: project_id.into(),
            description: String::new(),
            status: "active".into(),
            template: "custom".into(),
            modules_json: "[]".into(),
            owner_user_id: "owner".into(),
            dir_path: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            parent_id: None,
            path: String::new(),
            depth: 1,
            is_private: false,
            inherit_modules: false,
            inherit_task_types: false,
            module_disabled_json: "[]".into(),
            lifecycle: "active".into(),
            ended_at: None,
        }
    }

    #[test]
    fn prepared_transfer_recovery_rolls_back_when_actor_is_inactive() {
        let state = crate::dispatch::AppState::for_test();
        let queue_dir = tempfile::tempdir().expect("queue directory");
        crate::services::ingest_jobs::init(&queue_dir.path().join("jobs.db"))
            .expect("durable media queue");
        let central_dir = tempfile::tempdir().expect("central directory");
        let _ = super::super::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let content = tempfile::tempdir().expect("content directory");
        let org = uuid::Uuid::new_v4().to_string();
        let mut projects = Vec::new();
        for (index, prefix) in ["RCS", "RCD"].iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let dir = content.path().join(format!("project-{index}"));
            std::fs::create_dir_all(&dir).expect("project directory");
            super::super::repository::create_project(
                &id,
                &org,
                &format!("Recovery project {index}"),
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                dir.to_str().expect("path"),
                prefix,
                None,
                false,
                false,
                false,
                &[],
            )
            .expect("register project");
            projects.push((
                super::super::repository::project_record(&id)
                    .expect("record")
                    .expect("project exists"),
                super::super::project_db::open(&id).expect("project pool"),
            ));
        }
        let task = tasks::create_task(
            &projects[0].1,
            &TaskInput {
                task_type: "technical",
                title: "Source remains canonical",
                description_md: "",
                severity: "",
                priority: "medium",
                status: "todo",
                assigned_to: "",
                due_date: "",
                parent_task_id: None,
                links_json: "[]",
                attachments_json: "[]",
            },
            "owner",
        )
        .expect("source task");
        assert_eq!(
            super::super::task_index::sync_project(&projects[0].0, &projects[0].1, 128)
                .expect("publish source task location")
                .lag,
            0
        );
        let operation = TaskTransferJournal {
            operation_id: uuid::Uuid::new_v4().to_string(),
            org_id: org,
            source_project_id: projects[0].0.project_id.clone(),
            destination_project_id: projects[1].0.project_id.clone(),
            actor_user_id: "owner".into(),
            task_ids: vec![task.task_id.clone()],
            consent_wider_access: false,
            phase: "prepared".into(),
            sha_manifest_json: "[]".into(),
            key_map_json: "{}".into(),
            event_map_json: "{}".into(),
        };
        super::super::repository::prepare_task_transfer(&operation).expect("durable prepare");
        let source = projects.remove(0);
        let destination = projects.remove(0);
        drop(source.1);
        drop(destination.1);
        super::super::project_db::close(&source.0.project_id);
        super::super::project_db::close(&destination.0.project_id);
        recover_transfers(&state).expect("recovery rolls back revoked actor");
        assert!(super::super::repository::pending_task_transfers()
            .expect("journals")
            .is_empty());
        assert_eq!(
            super::super::repository::task_location(&task.task_id)
                .expect("location")
                .expect("source task")
                .project_id,
            source.0.project_id
        );
        let destination_pool =
            super::super::project_db::open(&destination.0.project_id).expect("destination reopen");
        assert_eq!(
            destination_pool
                .read()
                .expect("destination read")
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE task_id=?1",
                    [&task.task_id],
                    |row| row.get::<_, i64>(0)
                )
                .expect("no unpublished task"),
            0
        );
    }

    #[test]
    fn copied_and_published_transfer_recover_with_one_canonical_location() {
        let state = crate::dispatch::AppState::for_test();
        state
            .db
            .write()
            .expect("core writer")
            .execute(
                "INSERT INTO user_accounts(id,username,password_hash,display_name) \
                 VALUES ('owner','owner','','Owner')",
                [],
            )
            .expect("active transfer actor");
        let queue_dir = tempfile::tempdir().expect("queue directory");
        crate::services::ingest_jobs::init(&queue_dir.path().join("jobs.db"))
            .expect("durable media queue");
        let central_dir = tempfile::tempdir().expect("central directory");
        let _ = super::super::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let content = tempfile::tempdir().expect("content directory");
        for phase in ["copied", "published"] {
            let org = uuid::Uuid::new_v4().to_string();
            let mut projects = Vec::new();
            for (index, prefix) in ["RXS", "RXD"].iter().enumerate() {
                let id = uuid::Uuid::new_v4().to_string();
                let dir = content.path().join(&id);
                std::fs::create_dir_all(&dir).expect("project directory");
                super::super::repository::create_project(
                    &id,
                    &org,
                    &format!("{phase} project {index}"),
                    "",
                    "custom",
                    "[\"tasks\"]",
                    "owner",
                    dir.to_str().expect("path"),
                    prefix,
                    None,
                    false,
                    false,
                    false,
                    &[],
                )
                .expect("register project");
                projects.push((
                    super::super::repository::project_record(&id)
                        .expect("record")
                        .expect("project exists"),
                    super::super::project_db::open(&id).expect("project pool"),
                ));
            }
            let (source, source_pool) = &projects[0];
            let (destination, destination_pool) = &projects[1];
            let created = tasks::create_task(
                source_pool,
                &TaskInput {
                    task_type: "technical",
                    title: "Recover transferred content",
                    description_md: "Persisted description",
                    severity: "",
                    priority: "medium",
                    status: "review",
                    assigned_to: "",
                    due_date: "",
                    parent_task_id: None,
                    links_json: "[]",
                    attachments_json: "[]",
                },
                "owner",
            )
            .expect("source task");
            let original_comment = tasks::add_comment(
                source_pool,
                &created.task_id,
                "owner",
                "Preserved comment",
                &[],
            )
            .expect("source comment")
            .comment
            .expect("comment row");
            assert_eq!(
                super::super::task_index::sync_project(source, source_pool, 128)
                    .expect("source projection")
                    .lag,
                0
            );
            let operation = TaskTransferJournal {
                operation_id: uuid::Uuid::new_v4().to_string(),
                org_id: org,
                source_project_id: source.project_id.clone(),
                destination_project_id: destination.project_id.clone(),
                actor_user_id: "owner".into(),
                task_ids: vec![created.task_id.clone()],
                consent_wider_access: false,
                phase: "prepared".into(),
                sha_manifest_json: "[]".into(),
                key_map_json: "{}".into(),
                event_map_json: "{}".into(),
            };
            super::super::repository::prepare_task_transfer(&operation).expect("durable prepare");
            let data = source_copy(source_pool, &operation.task_ids, &[], &source.project_id)
                .expect("source snapshot");
            let (copy, keys, snapshots) = {
                let writer = destination_pool.write().expect("destination writer");
                copy_into_destination(
                    source,
                    destination,
                    &writer,
                    &writer,
                    &data,
                    &[],
                    "owner",
                    &operation.operation_id,
                )
                .expect("durable destination copy")
            };
            super::super::repository::stage_task_transfer_index(
                &operation.operation_id,
                &snapshots,
            )
            .expect("hidden staged index");
            super::super::repository::mark_task_transfer_copied(
                &operation.operation_id,
                "[]",
                &serde_json::to_string(&keys).expect("key map"),
                &serde_json::to_string(&copy).expect("copy map"),
            )
            .expect("copied journal");
            if phase == "published" {
                publish_copied(&state.db, &operation, &copy).expect("publish before restart");
            } else {
                assert_eq!(
                    super::super::repository::task_location(&created.task_id)
                        .expect("precutover location")
                        .expect("task")
                        .project_id,
                    source.project_id
                );
            }
            let source = source.clone();
            let destination = destination.clone();
            drop(projects);
            super::super::project_db::close(&source.project_id);
            super::super::project_db::close(&destination.project_id);
            recover_transfers(&state).expect("restart completes transfer");
            recover_transfers(&state).expect("cleaned restart is idempotent");
            let location = super::super::repository::task_location(&created.task_id)
                .expect("canonical location")
                .expect("moved task");
            assert_eq!(location.project_id, destination.project_id);
            assert_eq!(
                super::super::repository::resolve_task_key(&source.org_id, &created.task_key)
                    .expect("old key")
                    .expect("alias")
                    .current_key,
                location.current_key
            );
            let source_reopened =
                super::super::project_db::open(&source.project_id).expect("reopen source");
            let destination_reopened = super::super::project_db::open(&destination.project_id)
                .expect("reopen destination");
            assert!(tasks::get_task(&source_reopened, &created.task_id)
                .expect("source lookup")
                .is_none());
            assert!(tasks::get_task(&destination_reopened, &created.task_id)
                .expect("destination lookup")
                .is_some());
            assert!(
                tasks::list_comments(&destination_reopened, &created.task_id)
                    .expect("destination comments")
                    .iter()
                    .any(|comment| comment.comment_id == original_comment.comment_id)
            );
            assert!(super::super::repository::pending_task_transfers()
                .expect("journals")
                .iter()
                .all(|pending| pending.operation_id != operation.operation_id));
        }
    }

    #[test]
    fn opposing_content_transfers_use_one_order_and_keep_both_tasks() {
        use std::sync::{Arc, Barrier};

        let state = crate::dispatch::AppState::for_test();
        state
            .db
            .write()
            .expect("core writer")
            .execute(
                "INSERT INTO user_accounts(id,username,password_hash,display_name) \
                 VALUES ('owner','owner','','Owner')",
                [],
            )
            .expect("active transfer actor");
        let queue_dir = tempfile::tempdir().expect("queue directory");
        crate::services::ingest_jobs::init(&queue_dir.path().join("jobs.db"))
            .expect("durable media queue");
        let central_dir = tempfile::tempdir().expect("central directory");
        let _ = super::super::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let content = tempfile::tempdir().expect("content directory");
        let org = uuid::Uuid::new_v4().to_string();
        let mut projects = Vec::new();
        for (index, prefix) in ["OPS", "OPD"].iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let dir = content.path().join(&id);
            std::fs::create_dir_all(&dir).expect("project directory");
            super::super::repository::create_project(
                &id,
                &org,
                &format!("Opposing project {index}"),
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                dir.to_str().expect("path"),
                prefix,
                None,
                false,
                false,
                false,
                &[],
            )
            .expect("register project");
            projects.push((
                super::super::repository::project_record(&id)
                    .expect("record")
                    .expect("project exists"),
                super::super::project_db::open(&id).expect("project pool"),
            ));
        }
        let mut task_ids = Vec::new();
        for (project, pool) in &projects {
            let created = tasks::create_task(
                pool,
                &TaskInput {
                    task_type: "technical",
                    title: "Opposite direction",
                    description_md: "",
                    severity: "",
                    priority: "medium",
                    status: "todo",
                    assigned_to: "",
                    due_date: "",
                    parent_task_id: None,
                    links_json: "[]",
                    attachments_json: "[]",
                },
                "owner",
            )
            .expect("source task");
            task_ids.push(created.task_id);
            assert_eq!(
                super::super::task_index::sync_project(project, pool, 128)
                    .expect("source projection")
                    .lag,
                0
            );
        }
        let barrier = Arc::new(Barrier::new(3));
        std::thread::scope(|scope| {
            let left_barrier = barrier.clone();
            let left_state = state.clone();
            let left_projects = &projects;
            let left_task_ids = &task_ids;
            let left = scope.spawn(move || {
                left_barrier.wait();
                transfer_task(
                    &left_state,
                    &left_projects[0].0,
                    &left_projects[1].0,
                    &left_projects[0].1,
                    &left_projects[1].1,
                    &left_task_ids[0],
                    "owner",
                    false,
                )
            });
            let right_barrier = barrier.clone();
            let right_state = state.clone();
            let right_projects = &projects;
            let right_task_ids = &task_ids;
            let right = scope.spawn(move || {
                right_barrier.wait();
                transfer_task(
                    &right_state,
                    &right_projects[1].0,
                    &right_projects[0].0,
                    &right_projects[1].1,
                    &right_projects[0].1,
                    &right_task_ids[1],
                    "owner",
                    false,
                )
            });
            barrier.wait();
            left.join().expect("left thread").expect("left transfer");
            right.join().expect("right thread").expect("right transfer");
        });
        for index in 0..2 {
            let destination = 1 - index;
            assert_eq!(
                super::super::repository::task_location(&task_ids[index])
                    .expect("canonical location")
                    .expect("transferred task")
                    .project_id,
                projects[destination].0.project_id
            );
            assert!(tasks::get_task(&projects[index].1, &task_ids[index])
                .expect("old project")
                .is_none());
            assert!(tasks::get_task(&projects[destination].1, &task_ids[index])
                .expect("new project")
                .is_some());
        }
    }

    #[test]
    fn failed_remote_link_delete_recovers_after_target_end_admission() {
        let central_dir = tempfile::tempdir().expect("central directory");
        let _ = super::super::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let content = tempfile::tempdir().expect("content directory");
        let org_id = uuid::Uuid::new_v4().to_string();
        let mut projects = Vec::new();
        for (index, prefix) in ["RDA", "RDB"].iter().enumerate() {
            let project_id = uuid::Uuid::new_v4().to_string();
            let dir = content.path().join(format!("project-{index}"));
            std::fs::create_dir_all(&dir).expect("project directory");
            super::super::repository::create_project(
                &project_id,
                &org_id,
                &format!("Relation project {index}"),
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                dir.to_str().expect("path"),
                prefix,
                None,
                false,
                false,
                false,
                &[],
            )
            .expect("register project");
            let record = super::super::repository::project_record(&project_id)
                .expect("record")
                .expect("project exists");
            let pool = super::super::project_db::open(&project_id).expect("project pool");
            projects.push((record, pool));
        }
        let input = TaskInput {
            task_type: "technical",
            title: "Relation endpoint",
            description_md: "",
            severity: "",
            priority: "medium",
            status: "todo",
            assigned_to: "",
            due_date: "",
            parent_task_id: None,
            links_json: "[]",
            attachments_json: "[]",
        };
        let source_task = tasks::create_task(&projects[0].1, &input, "owner").expect("source task");
        let target_task = tasks::create_task(&projects[1].1, &input, "owner").expect("target task");
        let link = tasks::add_task_link(
            &projects[0].1,
            &projects[1].1,
            &projects[0].0.project_id,
            &projects[1].0.project_id,
            &source_task.task_id,
            &target_task.task_id,
            "related",
            0,
            "owner",
        )
        .expect("cross-project relation");
        projects[0]
            .1
            .write()
            .expect("source writer")
            .execute_batch(
                "CREATE TRIGGER reject_link_source_delete BEFORE DELETE ON task_links \
                 BEGIN SELECT RAISE(ABORT,'source transaction failed'); END;",
            )
            .expect("source failure trigger");
        assert!(tasks::delete_task_link(
            &projects[0].1,
            &projects[1].1,
            link.link.link_id,
            "owner"
        )
        .is_err());
        assert_eq!(
            projects[0]
                .1
                .read()
                .expect("source read")
                .query_row(
                    "SELECT COUNT(*) FROM task_links WHERE relation_id=?1",
                    [&link.link.relation_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("source row after rollback"),
            1
        );
        assert_eq!(
            projects[0]
                .1
                .read()
                .expect("source read")
                .query_row(
                    "SELECT COUNT(*) FROM task_events WHERE kind='link_deleted' AND task_id=?1",
                    [&source_task.task_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("source events after rollback"),
            0
        );
        assert!(super::super::repository::pending_task_relation_deletions(
            &projects[0].0.project_id
        )
        .expect("rolled-back admission")
        .is_empty());
        projects[0]
            .1
            .write()
            .expect("source writer")
            .execute_batch("DROP TRIGGER reject_link_source_delete")
            .expect("remove source failure trigger");
        let route = super::super::repository::relation_routes_for_task(&source_task.task_id)
            .expect("canonical relation")
            .into_iter()
            .next()
            .expect("active route");
        super::super::repository::prepare_task_relation_deletion(&route, "owner")
            .expect("interrupted admission before source commit");
        reconcile_project_relations(&projects[0].0, &projects[0].1)
            .expect("restart aborts deletion whose canonical row survived");
        assert!(super::super::repository::pending_task_relation_deletions(
            &projects[0].0.project_id
        )
        .expect("recovered admission")
        .is_empty());
        assert_eq!(
            super::super::repository::relation_routes_for_task(&source_task.task_id)
                .expect("visible relation after recovery")
                .len(),
            1
        );
        projects[1]
            .1
            .write()
            .expect("target writer")
            .execute_batch(
                "CREATE TRIGGER reject_link_delete BEFORE INSERT ON task_events \
                 WHEN NEW.kind='link_deleted' BEGIN SELECT RAISE(ABORT,'target write failed'); END;",
            )
            .expect("failure trigger");
        assert!(tasks::delete_task_link(
            &projects[0].1,
            &projects[1].1,
            link.link.link_id,
            "owner"
        )
        .is_err());
        let source_row: i64 = projects[0]
            .1
            .read()
            .expect("source read")
            .query_row(
                "SELECT COUNT(*) FROM task_links WHERE relation_id=?1",
                [&link.link.relation_id],
                |row| row.get(0),
            )
            .expect("canonical row");
        assert_eq!(source_row, 0);
        let source_events: i64 = projects[0]
            .1
            .read()
            .expect("source read")
            .query_row(
                "SELECT COUNT(*) FROM task_events WHERE task_id=?1 AND kind='link_deleted' \
                 AND json_extract(before_json,'$.relation_id')=?2",
                params![source_task.task_id, link.link.relation_id],
                |row| row.get(0),
            )
            .expect("source deletion event");
        assert_eq!(source_events, 1);
        assert_eq!(
            super::super::repository::pending_task_relation_deletions(&projects[0].0.project_id)
                .expect("pending deletion")
                .len(),
            1
        );
        assert!(
            super::super::repository::relation_routes_for_task(&source_task.task_id)
                .expect("visible relations")
                .is_empty()
        );
        for (source_index, task_id) in [
            (0, source_task.task_id.as_str()),
            (1, target_task.task_id.as_str()),
        ] {
            let destination_index = 1 - source_index;
            let operation = TaskTransferJournal {
                operation_id: uuid::Uuid::new_v4().to_string(),
                org_id: org_id.clone(),
                source_project_id: projects[source_index].0.project_id.clone(),
                destination_project_id: projects[destination_index].0.project_id.clone(),
                actor_user_id: "owner".into(),
                task_ids: vec![task_id.to_string()],
                consent_wider_access: false,
                phase: "prepared".into(),
                sha_manifest_json: "[]".into(),
                key_map_json: "{}".into(),
                event_map_json: "{}".into(),
            };
            let denied = super::super::repository::prepare_task_transfer(&operation)
                .expect_err("either relation endpoint is fenced from transfer");
            assert!(denied.to_string().contains("pending relation deletion"));
        }
        projects[1]
            .1
            .write()
            .expect("target writer")
            .execute_batch("DROP TRIGGER reject_link_delete")
            .expect("remove failure trigger");
        {
            let central = super::super::db::pool().expect("central pool");
            central
                .write()
                .expect("central writer")
                .execute(
                    "INSERT INTO project_admissions(project_id,operation_id,kind,actor_user_id,reason) \
                     VALUES (?1,?2,'end','owner','end already admitted')",
                    params![projects[1].0.project_id, uuid::Uuid::new_v4().to_string()],
                )
                .expect("target end admission after source commit");
        }
        let source_project = projects.remove(0);
        let target_project = projects.remove(0);
        drop(source_project.1);
        drop(target_project.1);
        super::super::project_db::close(&source_project.0.project_id);
        super::super::project_db::close(&target_project.0.project_id);
        let reopened = super::super::project_db::open(&source_project.0.project_id)
            .expect("reopen source after interrupted delete");
        reconcile_project_relations(&source_project.0, &reopened)
            .expect("complete admitted deletion after restart");
        reconcile_project_relations(&source_project.0, &reopened)
            .expect("replayed recovery remains idempotent");
        let target_reopened = super::super::project_db::open(&target_project.0.project_id)
            .expect("reopen ended target");
        let target_events: i64 = target_reopened
            .read()
            .expect("target read")
            .query_row(
                "SELECT COUNT(*) FROM task_events WHERE task_id=?1 AND kind='link_deleted' \
                 AND json_extract(before_json,'$.relation_id')=?2",
                params![target_task.task_id, link.link.relation_id],
                |row| row.get(0),
            )
            .expect("target history");
        assert_eq!(target_events, 1);
        let source_events_after_recovery: i64 = reopened
            .read()
            .expect("source read")
            .query_row(
                "SELECT COUNT(*) FROM task_events WHERE task_id=?1 AND kind='link_deleted' \
                 AND json_extract(before_json,'$.relation_id')=?2",
                params![source_task.task_id, link.link.relation_id],
                |row| row.get(0),
            )
            .expect("source history after repeated recovery");
        assert_eq!(source_events_after_recovery, 1);
        assert!(super::super::repository::pending_task_relation_deletions(
            &source_project.0.project_id
        )
        .expect("pending deletion after recovery")
        .is_empty());
        assert!(
            super::super::repository::relation_routes_owned_by(&source_project.0.project_id)
                .expect("canonical routes")
                .is_empty()
        );
    }

    #[test]
    fn copied_history_comment_and_original_event_survive_two_content_moves() {
        let central_dir = tempfile::tempdir().expect("central directory");
        let _ = super::super::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let core_conn = rusqlite::Connection::open_in_memory().expect("core database");
        core_conn
            .execute_batch(
                "CREATE TABLE user_accounts(id TEXT PRIMARY KEY,username TEXT NOT NULL,\
             password_hash TEXT NOT NULL,display_name TEXT NOT NULL,email TEXT NOT NULL,\
             is_active INTEGER NOT NULL,is_admin INTEGER NOT NULL,\
             must_change_password INTEGER NOT NULL,sso_provider TEXT,sso_subject TEXT,\
             last_login_at TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,\
             role TEXT NOT NULL);\
             INSERT INTO user_accounts VALUES ('owner','owner','','Owner','',1,0,0,\
             NULL,NULL,NULL,'2026-10-01','2026-10-01','user');",
            )
            .expect("active actor");
        let core_db = Arc::new(crate::db::Db::from_connection(core_conn));
        let content = tempfile::tempdir().expect("content directory");
        let org_id = uuid::Uuid::new_v4().to_string();
        let mut projects = Vec::new();
        for (index, prefix) in ["SRC", "DST", "THR"].iter().enumerate() {
            let project_id = uuid::Uuid::new_v4().to_string();
            let dir = content.path().join(format!("project-{index}"));
            std::fs::create_dir_all(&dir).expect("project directory");
            super::super::repository::create_project(
                &project_id,
                &org_id,
                &format!("Project {index}"),
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                dir.to_str().expect("path"),
                prefix,
                None,
                index == 0,
                false,
                false,
                &[],
            )
            .expect("project");
            let record = super::super::repository::project_record(&project_id)
                .expect("record")
                .expect("project exists");
            let pool = super::super::project_db::open(&project_id).expect("project pool");
            projects.push((record, pool));
        }
        {
            let central = super::super::db::pool().expect("central database");
            let conn = central.write().expect("central writer");
            for (project, _) in &projects {
                conn.execute(
                    "INSERT INTO project_members(project_id,user_id,project_admin,invited_by) \
                     VALUES (?1,'assignee',1,'owner')",
                    [&project.project_id],
                )
                .expect("assignee can read every project");
            }
        }
        let input = TaskInput {
            task_type: "technical",
            title: "Keep history",
            description_md: "Original",
            severity: "",
            priority: "medium",
            status: "todo",
            assigned_to: "assignee",
            due_date: "",
            parent_task_id: None,
            links_json: "[]",
            attachments_json: "[]",
        };
        let created = tasks::create_task(&projects[0].1, &input, "owner").expect("create task");
        let origin_event = created.event_ids[0];
        let added_comment =
            tasks::add_comment(&projects[0].1, &created.task_id, "owner", "A note", &[])
                .expect("comment");
        let comment_event_id = added_comment.event_id.expect("comment event");
        let comment = added_comment.comment.expect("comment row");
        assert_eq!(
            super::super::task_index::sync_project(&projects[0].0, &projects[0].1, 100)
                .expect("initial index")
                .lag,
            0
        );
        assert_eq!(
            super::super::repository::resolve_task_event(
                &created.task_id,
                &projects[0].0.project_id,
                comment_event_id,
            )
            .expect("current event route"),
            Some((projects[0].0.project_id.clone(), comment_event_id))
        );
        assert!(super::super::repository::resolve_task_event(
            &created.task_id,
            &projects[0].0.project_id,
            comment_event_id + 1000,
        )
        .expect("missing event route")
        .is_none());
        let target = tasks::create_task(
            &projects[2].1,
            &TaskInput {
                title: "Relation target",
                ..input
            },
            "owner",
        )
        .expect("target task");
        assert_eq!(
            super::super::task_index::sync_project(&projects[2].0, &projects[2].1, 100)
                .expect("target index")
                .lag,
            0
        );
        let link = tasks::add_task_link(
            &projects[0].1,
            &projects[2].1,
            &projects[0].0.project_id,
            &projects[2].0.project_id,
            &created.task_id,
            &target.task_id,
            "related",
            0,
            "owner",
        )
        .expect("cross-project relation");
        for index in 0..2 {
            let (source, source_pool) = &projects[index];
            let (destination, destination_pool) = &projects[index + 1];
            if index == 0 {
                let plan = preview_transfer(
                    source,
                    destination,
                    source_pool,
                    destination_pool,
                    &created.task_id,
                )
                .expect("private-to-public preview");
                assert!(!plan.widens_access);
                assert!(plan.blocking_reasons.is_empty());
            }
            let operation = TaskTransferJournal {
                operation_id: uuid::Uuid::new_v4().to_string(),
                org_id: org_id.clone(),
                source_project_id: source.project_id.clone(),
                destination_project_id: destination.project_id.clone(),
                actor_user_id: "owner".into(),
                task_ids: vec![created.task_id.clone()],
                consent_wider_access: false,
                phase: "prepared".into(),
                sha_manifest_json: "[]".into(),
                key_map_json: "{}".into(),
                event_map_json: "{}".into(),
            };
            if index == 0 {
                let central = super::super::db::pool().expect("central database");
                {
                    let conn = central.write().expect("central writer");
                    conn.execute(
                        "INSERT INTO project_members(project_id,user_id,project_admin,invited_by) \
                         VALUES (?1,'new-reader',1,'owner')",
                        [&destination.project_id],
                    )
                    .expect("destination-only reader");
                }
                assert!(super::super::repository::prepare_task_transfer(&operation).is_err());
                central
                    .write()
                    .expect("central writer")
                    .execute(
                        "DELETE FROM project_members WHERE project_id=?1 AND user_id='new-reader'",
                        [&destination.project_id],
                    )
                    .expect("remove destination-only reader");
            }
            super::super::repository::prepare_task_transfer(&operation).expect("prepare");
            let routes = incident_routes(&operation.task_ids).expect("canonical routes");
            let data = source_copy(
                source_pool,
                &operation.task_ids,
                &routes,
                &source.project_id,
            )
            .expect("copy source");
            let (copy, keys, snapshots) = {
                let target = destination_pool.write().expect("target writer");
                copy_into_destination(
                    source,
                    destination,
                    &target,
                    &target,
                    &data,
                    &routes,
                    "owner",
                    &operation.operation_id,
                )
                .expect("copy destination")
            };
            super::super::repository::stage_task_transfer_index(
                &operation.operation_id,
                &snapshots,
            )
            .expect("stage hidden destination");
            assert_eq!(
                super::super::repository::task_location(&created.task_id)
                    .expect("canonical location")
                    .expect("task location")
                    .project_id,
                source.project_id
            );
            super::super::repository::mark_task_transfer_copied(
                &operation.operation_id,
                "[]",
                &serde_json::to_string(&keys).expect("keys"),
                &serde_json::to_string(&copy).expect("copy manifest"),
            )
            .expect("durable copied phase");
            if index == 0 {
                let central = super::super::db::pool().expect("central database");
                {
                    let conn = central.write().expect("central writer");
                    conn.execute(
                        "UPDATE projects SET owner_user_id='replacement' WHERE project_id=?1",
                        [&source.project_id],
                    )
                    .expect("revoke source ownership");
                }
                assert!(publish_copied(&core_db, &operation, &copy).is_err());
                assert_eq!(
                    super::super::repository::task_location(&created.task_id)
                        .expect("location")
                        .expect("task")
                        .project_id,
                    source.project_id
                );
                {
                    let conn = central.write().expect("central writer");
                    conn.execute(
                        "UPDATE projects SET owner_user_id='owner' WHERE project_id=?1",
                        [&source.project_id],
                    )
                    .expect("restore ownership");
                    conn.execute(
                        "UPDATE projects SET modules_json='[]' WHERE project_id=?1",
                        [&destination.project_id],
                    )
                    .expect("disable destination Tasks");
                }
                assert!(publish_copied(&core_db, &operation, &copy).is_err());
                assert_eq!(
                    super::super::repository::task_location(&created.task_id)
                        .expect("location")
                        .expect("task")
                        .project_id,
                    source.project_id
                );
                {
                    let conn = central.write().expect("central writer");
                    conn.execute(
                        "UPDATE projects SET modules_json='[\"tasks\"]' WHERE project_id=?1",
                        [&destination.project_id],
                    )
                    .expect("restore destination Tasks");
                }
                core_db
                    .write()
                    .expect("core writer")
                    .execute("UPDATE user_accounts SET is_active=0 WHERE id='owner'", [])
                    .expect("deactivate actor");
                assert!(publish_copied(&core_db, &operation, &copy).is_err());
                assert_eq!(
                    super::super::repository::task_location(&created.task_id)
                        .expect("location")
                        .expect("task")
                        .project_id,
                    source.project_id
                );
                core_db
                    .write()
                    .expect("core writer")
                    .execute("UPDATE user_accounts SET is_active=1 WHERE id='owner'", [])
                    .expect("reactivate actor");
                {
                    let conn = central.write().expect("central writer");
                    conn.execute(
                        "INSERT INTO project_members(project_id,user_id,project_admin,invited_by) \
                         VALUES (?1,'new-reader',1,'owner')",
                        [&destination.project_id],
                    )
                    .expect("add destination-only reader");
                }
                assert!(publish_copied(&core_db, &operation, &copy).is_err());
                assert_eq!(
                    super::super::repository::task_location(&created.task_id)
                        .expect("location")
                        .expect("task")
                        .project_id,
                    source.project_id
                );
                {
                    let conn = central.write().expect("central writer");
                    conn.execute(
                        "DELETE FROM project_members WHERE project_id=?1 AND user_id='new-reader'",
                        [&destination.project_id],
                    )
                    .expect("remove destination-only reader");
                    conn.execute(
                        "DELETE FROM project_members WHERE project_id=?1 AND user_id='assignee'",
                        [&destination.project_id],
                    )
                    .expect("revoke assignee destination read");
                }
                assert!(publish_copied(&core_db, &operation, &copy).is_err());
                assert_eq!(
                    super::super::repository::task_location(&created.task_id)
                        .expect("location")
                        .expect("task")
                        .project_id,
                    source.project_id
                );
                central
                    .write()
                    .expect("central writer")
                    .execute(
                        "INSERT INTO project_members(project_id,user_id,project_admin,invited_by) \
                         VALUES (?1,'assignee',1,'owner')",
                        [&destination.project_id],
                    )
                    .expect("restore assignee read");
            }
            publish_copied(&core_db, &operation, &copy).expect("atomic cutover");
            assert_eq!(
                super::super::repository::task_location(&created.task_id)
                    .expect("canonical location")
                    .expect("task location")
                    .project_id,
                destination.project_id
            );
            assert!(tasks::list_comments(destination_pool, &created.task_id)
                .expect("comments")
                .iter()
                .any(|entry| entry.comment_id == comment.comment_id));
            let tx = source_pool.write().expect("source writer");
            tx.execute(
                "DELETE FROM task_links WHERE source_task_id=?1",
                [&created.task_id],
            )
            .expect("remove old relation row");
            tx.execute(
                "DELETE FROM task_comments WHERE task_id=?1",
                [&created.task_id],
            )
            .expect("remove old comments");
            tx.execute(
                "DELETE FROM task_events WHERE task_id=?1",
                [&created.task_id],
            )
            .expect("remove old events");
            tx.execute("DELETE FROM tasks WHERE task_id=?1", [&created.task_id])
                .expect("remove old task");
            drop(tx);
            super::super::repository::mark_task_transfer_cleaned(&operation.operation_id)
                .expect("cleaned phase");
            assert_eq!(
                super::super::task_index::sync_project(source, source_pool, 100)
                    .expect("source index")
                    .lag,
                0
            );
            assert_eq!(
                super::super::task_index::sync_project(destination, destination_pool, 100)
                    .expect("destination index")
                    .lag,
                0
            );
        }
        let final_location = super::super::repository::task_location(&created.task_id)
            .expect("location")
            .expect("task");
        assert_eq!(final_location.project_id, projects[2].0.project_id);
        let old_key = super::super::repository::resolve_task_key(&org_id, &created.task_key)
            .expect("old key")
            .expect("alias");
        assert_eq!(old_key.current_key, final_location.current_key);
        let old_event = super::super::repository::resolve_task_event(
            &created.task_id,
            &projects[0].0.project_id,
            origin_event,
        )
        .expect("event alias")
        .expect("original event");
        assert_eq!(old_event.0, projects[2].0.project_id);
        assert!(
            tasks::list_task_events(&projects[2].1, &created.task_id, None, 100)
                .expect("final history")
                .0
                .iter()
                .any(|event| event.event_id == old_event.1)
        );
        let routed_comment = super::super::repository::resolve_task_event(
            &created.task_id,
            &projects[0].0.project_id,
            comment_event_id,
        )
        .expect("comment event alias")
        .expect("original comment event");
        assert_eq!(routed_comment.0, projects[2].0.project_id);
        assert!(
            tasks::list_task_events(&projects[2].1, &created.task_id, None, 100)
                .expect("final comment history")
                .0
                .iter()
                .any(|event| event.event_id == routed_comment.1 && event.kind == "comment_added")
        );
        let route = super::super::repository::relation_routes_for_task(&created.task_id)
            .expect("relation route");
        assert_eq!(route.len(), 1);
        assert_eq!(route[0].owning_project_id, projects[2].0.project_id);
        assert_eq!(route[0].target_project_id, projects[2].0.project_id);
        let alias = super::super::repository::resolve_task_link(
            &projects[0].0.project_id,
            link.link.link_id,
        )
        .expect("link alias")
        .expect("old relation id");
        assert_eq!(alias.current_project_id, projects[2].0.project_id);
        assert_eq!(alias.relation_id, link.link.relation_id);
    }
}
