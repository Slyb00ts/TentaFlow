// ============ File: project_studio/task_index.rs — project task projection into the central registry ============

use std::collections::HashSet;

use anyhow::{bail, Result};
use rusqlite::params;

use super::models::{ProjectRecord, TaskIndexEvent, TaskIndexStatus};
use crate::db::DbPool;

const BATCH_SIZE: u32 = 128;

pub fn source_revision(pool: &DbPool) -> Result<i64> {
    let conn = pool
        .read()
        .map_err(|error| anyhow::anyhow!("task source read: {error}"))?;
    conn.query_row(
        "SELECT COALESCE(MAX(revision),0) FROM task_index_outbox",
        [],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub fn sync_project(
    project: &ProjectRecord,
    pool: &DbPool,
    max_events: u32,
) -> Result<TaskIndexStatus> {
    if max_events == 0 {
        bail!("task index synchronization budget must be positive");
    }
    sync_catalogue(project, pool)?;
    let mut remaining = max_events;
    while remaining != 0 {
        let source = source_revision(pool)?;
        let status = super::repository::task_index_status(&project.project_id, source)?;
        if status.applied_revision > source {
            bail!("task index cursor is ahead of source; rebuild required");
        }
        if status.applied_revision == source {
            return Ok(status);
        }
        let batch_limit = remaining.min(BATCH_SIZE);
        let events: Vec<TaskIndexEvent> = {
            let conn = pool
                .read()
                .map_err(|error| anyhow::anyhow!("task source read: {error}"))?;
            let mut stmt = conn.prepare(
                "SELECT revision,task_id,op,snapshot_json FROM task_index_outbox \
                 WHERE revision>?1 ORDER BY revision LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![status.applied_revision, batch_limit], |row| {
                Ok(TaskIndexEvent {
                    revision: row.get(0)?,
                    task_id: row.get(1)?,
                    op: row.get(2)?,
                    snapshot_json: row.get(3)?,
                })
            })?;
            let events = rows.collect::<rusqlite::Result<_>>()?;
            events
        };
        if events.is_empty() || events[0].revision != status.applied_revision + 1 {
            return rebuild_project(project, pool);
        }
        let applied = super::repository::upsert_task_index_events(
            &project.project_id,
            &project.org_id,
            &events,
        )?;
        remaining -= events.len() as u32;
        if applied < events.last().map_or(0, |event| event.revision) {
            bail!("task index did not apply its complete source batch");
        }
    }
    let source = source_revision(pool)?;
    super::repository::record_task_index_observation(&project.project_id, source)?;
    super::repository::task_index_status(&project.project_id, source)
}

pub fn rebuild_project(project: &ProjectRecord, pool: &DbPool) -> Result<TaskIndexStatus> {
    sync_catalogue(project, pool)?;
    let (watermark, snapshots): (i64, Vec<TaskIndexEvent>) = {
        let conn = pool
            .read()
            .map_err(|error| anyhow::anyhow!("task source read: {error}"))?;
        let snapshot = conn.unchecked_transaction()?;
        let watermark = snapshot.query_row(
            "SELECT COALESCE(MAX(revision),0) FROM task_index_outbox",
            [],
            |row| row.get(0),
        )?;
        let mut stmt = snapshot
            .prepare("SELECT task_id,snapshot_json FROM task_index_snapshots ORDER BY task_id")?;
        let snapshots = stmt
            .query_map([], |row| {
                Ok(TaskIndexEvent {
                    revision: watermark,
                    task_id: row.get(0)?,
                    op: "upsert".into(),
                    snapshot_json: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        snapshot.commit()?;
        (watermark, snapshots)
    };
    super::repository::replace_project_task_index(
        &project.project_id,
        &project.org_id,
        watermark,
        &snapshots,
    )?;
    super::repository::task_index_status(&project.project_id, watermark)
}

pub fn sync_catalogue(project: &ProjectRecord, pool: &DbPool) -> Result<()> {
    let (_, source_project_id) =
        super::repository::effective_project_settings(&project.project_id)?;
    let source_pool = if source_project_id == project.project_id {
        pool.clone()
    } else {
        super::project_db::open(&source_project_id)?
    };
    let (revision, types) = {
        let conn = source_pool
            .read()
            .map_err(|error| anyhow::anyhow!("task catalogue source read: {error}"))?;
        let snapshot = conn.unchecked_transaction()?;
        let revision: i64 = snapshot.query_row(
            "SELECT revision FROM task_type_catalogue_revision WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let types = super::tasks::list_task_types_on(&snapshot)?;
        snapshot.commit()?;
        (revision, types)
    };
    super::repository::replace_projected_task_type_names(&source_project_id, revision, &types)?;
    Ok(())
}

pub fn reconcile_startup() -> Result<Vec<String>> {
    let pending = super::repository::pending_task_transfers()?;
    let mut deferred: HashSet<String> = pending
        .iter()
        .flat_map(|operation| {
            [
                operation.source_project_id.clone(),
                operation.destination_project_id.clone(),
            ]
        })
        .collect();
    for operation in &pending {
        for task_id in &operation.task_ids {
            for route in super::repository::relation_routes_for_task(task_id)? {
                deferred.insert(route.owning_project_id);
            }
        }
    }
    let records = super::repository::all_project_records()?;
    for project in &records {
        if deferred.contains(&project.project_id) {
            continue;
        }
        let pool = super::project_db::open(&project.project_id)?;
        loop {
            let status = sync_project(project, &pool, 4096)?;
            if status.lag == 0 {
                break;
            }
        }
    }
    for project in &records {
        if deferred.contains(&project.project_id) {
            continue;
        }
        let pool = super::project_db::open(&project.project_id)?;
        super::task_transfer::reconcile_project_relations(project, &pool)?;
    }
    let mut ids: Vec<String> = deferred.into_iter().collect();
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rusqlite::Connection;

    use super::*;

    #[test]
    fn catalogue_projection_preserves_newer_rename_against_delayed_snapshot() {
        let registry = tempfile::tempdir().expect("registry directory");
        let _ = super::super::db::init(&registry.path().join("projects.db"));
        std::mem::forget(registry);
        let content = tempfile::tempdir().expect("content directory");
        let project_id = uuid::Uuid::new_v4().to_string();
        let org_id = uuid::Uuid::new_v4().to_string();
        super::super::repository::create_project(
            &project_id,
            &org_id,
            "Catalogue projection",
            "",
            "custom",
            "[\"tasks\"]",
            "owner",
            content.path().to_str().expect("path"),
            "CAT",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("register project");
        let project = super::super::repository::project_record(&project_id)
            .expect("project record")
            .expect("project exists");
        let pool = super::super::project_db::open(&project_id).expect("content pool");
        sync_catalogue(&project, &pool).expect("seed actual built-ins");
        let seeded = super::super::repository::projected_task_type_names(&project_id)
            .expect("central type names");
        assert_eq!(
            seeded.get("technical").map(String::as_str),
            Some("Technical task")
        );
        super::super::tasks::save_task_type(&pool, "custom_work", "Before rename", "", 70, true)
            .expect("first source catalogue commit and projection");
        let stale_revision: i64 = pool
            .read()
            .expect("source read")
            .query_row(
                "SELECT revision FROM task_type_catalogue_revision WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .expect("source revision");
        let stale_types = super::super::tasks::list_task_types(&pool).expect("old type names");
        super::super::tasks::save_task_type(&pool, "custom_work", "After rename", "", 70, true)
            .expect("rename source catalogue and projection");
        assert!(
            !super::super::repository::replace_projected_task_type_names(
                &project_id,
                stale_revision,
                &stale_types,
            )
            .expect("delayed projection is ignored")
        );
        assert_eq!(
            super::super::repository::projected_task_type_names(&project_id)
                .expect("current central names")
                .get("custom_work")
                .map(String::as_str),
            Some("After rename")
        );
    }

    #[test]
    fn file_wal_read_transaction_keeps_watermark_and_rows_together() {
        let temporary = tempfile::tempdir().expect("temporary project directory");
        let (initial, _) =
            super::super::project_db::open_pool_at(temporary.path()).expect("migrate file project");
        drop(initial);
        let path = temporary.path().join("project.db");
        let writer = Connection::open(&path).expect("writer connection");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("WAL mode");
        let pool =
            Arc::new(crate::db::Db::with_read_pool(writer, &path).expect("file project read pool"));
        let conn = pool.read().expect("pooled reader");
        let snapshot = conn.unchecked_transaction().expect("reader snapshot");
        let watermark: i64 = snapshot
            .query_row(
                "SELECT COALESCE(MAX(revision),0) FROM task_index_outbox",
                [],
                |row| row.get(0),
            )
            .expect("initial watermark");
        assert_eq!(watermark, 0);
        pool.write()
            .expect("writer")
            .execute(
                "INSERT INTO tasks(task_id,task_no,task_key,task_type,title,created_by) \
             VALUES ('concurrent',1,'PR-1','technical','Concurrent insert','author')",
                [],
            )
            .expect("commit source mutation while reader is open");
        let count: i64 = snapshot
            .query_row("SELECT COUNT(*) FROM task_index_snapshots", [], |row| {
                row.get(0)
            })
            .expect("rows at watermark");
        assert_eq!(count, 0);
        snapshot.commit().expect("complete snapshot");
        drop(conn);
        assert_eq!(source_revision(&pool).expect("new source watermark"), 1);
        let latest: i64 = pool
            .read()
            .expect("new reader")
            .query_row("SELECT COUNT(*) FROM task_index_snapshots", [], |row| {
                row.get(0)
            })
            .expect("new source rows");
        assert_eq!(latest, 1);
    }

    #[test]
    fn committed_outbox_replays_once_and_rebuilds_snapshot_before_new_tail() {
        let registry = tempfile::tempdir().expect("registry directory");
        let _ = super::super::db::init(&registry.path().join("projects.db"));
        std::mem::forget(registry);
        let content = tempfile::tempdir().expect("content directory");
        let project_id = uuid::Uuid::new_v4().to_string();
        let org_id = uuid::Uuid::new_v4().to_string();
        super::super::repository::create_project(
            &project_id,
            &org_id,
            "Outbox replay",
            "",
            "custom",
            "[\"tasks\"]",
            "owner",
            content.path().to_str().expect("path"),
            "OUT",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("register project");
        let project = super::super::repository::project_record(&project_id)
            .expect("project record")
            .expect("project exists");
        let pool = super::super::project_db::open(&project_id).expect("content pool");
        let original = super::super::tasks::TaskInput {
            task_type: "technical",
            title: "Before restart",
            description_md: "Real source content",
            severity: "",
            priority: "medium",
            status: "todo",
            assigned_to: "owner",
            due_date: "",
            parent_task_id: None,
            links_json: "[]",
            attachments_json: "[]",
        };
        let task =
            super::super::tasks::create_task(&pool, &original, "owner").expect("committed create");
        let edited = super::super::tasks::TaskInput {
            title: "After source update",
            status: "in_progress",
            ..original
        };
        super::super::tasks::update_task(&pool, &task.task_id, &edited, "owner")
            .expect("committed update")
            .expect("existing task");
        let unapplied_source = source_revision(&pool).expect("durable outbox revision");
        assert_eq!(unapplied_source, 2);
        let allowed = vec![project_id.clone()];
        let filter = super::super::models::TaskIndexFilter {
            task_type: String::new(),
            status: String::new(),
            assigned_to: String::new(),
            search: String::new(),
            severity: String::new(),
            include_archived: false,
            offset: 0,
            limit: 20,
        };
        let (_, before_count) = super::super::repository::list_indexed_tasks(&allowed, &filter)
            .expect("unapplied index");
        assert_eq!(before_count, 0);
        assert!(super::super::repository::task_location(&task.task_id)
            .expect("unapplied canonical location")
            .is_none());
        super::super::project_db::close(&project_id);
        drop(pool);
        let reopened =
            super::super::project_db::open(&project_id).expect("reopen persisted source");
        assert_eq!(
            source_revision(&reopened).expect("reopened outbox"),
            unapplied_source
        );
        let first = sync_project(&project, &reopened, 1).expect("bounded first replay");
        assert_eq!(first.applied_revision, 1);
        assert_eq!(first.lag, 1);
        let (first_page, first_count) =
            super::super::repository::list_indexed_tasks(&allowed, &filter).expect("first page");
        assert_eq!(first_count, 1);
        assert_eq!(first_page[0].snapshot.title, "Before restart");
        let current = sync_project(&project, &reopened, 1).expect("second replay");
        assert_eq!(current.applied_revision, unapplied_source);
        assert_eq!(current.lag, 0);
        let (page, count) =
            super::super::repository::list_indexed_tasks(&allowed, &filter).expect("current page");
        assert_eq!(count, 1);
        assert_eq!(page[0].snapshot.title, "After source update");
        assert_eq!(page[0].snapshot.status, "in_progress");
        assert_eq!(page[0].snapshot.task_key, task.task_key);
        let location = super::super::repository::task_location(&task.task_id)
            .expect("canonical location")
            .expect("indexed task");
        assert_eq!(location.project_id, project_id);
        assert_eq!(location.current_key, task.task_key);
        assert_eq!(
            super::super::repository::task_aggregate_summary(&allowed, &project_id, &filter)
                .expect("indexed summary")
                .in_progress,
            1
        );
        let actual_events: Vec<TaskIndexEvent> = {
            let conn = reopened.read().expect("source reader");
            let mut stmt = conn
                .prepare(
                    "SELECT revision,task_id,op,snapshot_json FROM task_index_outbox ORDER BY revision",
                )
                .expect("source outbox query");
            let rows = stmt
                .query_map([], |row| {
                    Ok(TaskIndexEvent {
                        revision: row.get(0)?,
                        task_id: row.get(1)?,
                        op: row.get(2)?,
                        snapshot_json: row.get(3)?,
                    })
                })
                .expect("source outbox rows");
            rows.collect::<rusqlite::Result<_>>()
                .expect("persisted source events")
        };
        assert_eq!(actual_events.len(), 2);
        assert_eq!(
            super::super::repository::upsert_task_index_events(
                &project_id,
                &org_id,
                &actual_events
            )
            .expect("duplicate replay"),
            unapplied_source
        );
        assert_eq!(
            sync_project(&project, &reopened, 1)
                .expect("idempotent sync")
                .lag,
            0
        );
        assert_eq!(
            super::super::repository::list_indexed_tasks(&allowed, &filter)
                .expect("no duplicate index row")
                .1,
            1
        );
        {
            let central = super::super::db::pool().expect("central database");
            central
                .write()
                .expect("central writer")
                .execute(
                    "DELETE FROM task_index WHERE project_id=?1 AND task_id=?2",
                    rusqlite::params![project_id, task.task_id],
                )
                .expect("corrupt only the central projection");
        }
        assert_eq!(
            super::super::repository::list_indexed_tasks(&allowed, &filter)
                .expect("corrupted projection")
                .1,
            0
        );
        let rebuilt = rebuild_project(&project, &reopened).expect("rebuild from actual source");
        assert_eq!(rebuilt.applied_revision, unapplied_source);
        assert_eq!(rebuilt.lag, 0);
        let (rebuilt_page, rebuilt_count) =
            super::super::repository::list_indexed_tasks(&allowed, &filter).expect("rebuilt page");
        assert_eq!(rebuilt_count, 1);
        assert_eq!(rebuilt_page[0].snapshot.title, "After source update");
        let final_input = super::super::tasks::TaskInput {
            task_type: "technical",
            title: "Committed after snapshot",
            description_md: "Real source content",
            severity: "",
            priority: "medium",
            status: "review",
            assigned_to: "owner",
            due_date: "",
            parent_task_id: None,
            links_json: "[]",
            attachments_json: "[]",
        };
        super::super::tasks::update_task(&reopened, &task.task_id, &final_input, "owner")
            .expect("post-snapshot source commit")
            .expect("existing task");
        assert_eq!(
            source_revision(&reopened).expect("post-snapshot tail"),
            unapplied_source + 1
        );
        let (stale_page, stale_count) =
            super::super::repository::list_indexed_tasks(&allowed, &filter).expect("stale page");
        assert_eq!(stale_count, 1);
        assert_eq!(stale_page[0].snapshot.title, "After source update");
        let tailed = sync_project(&project, &reopened, 1).expect("apply post-snapshot tail");
        assert_eq!(tailed.lag, 0);
        assert_eq!(tailed.applied_revision, unapplied_source + 1);
        let (final_page, final_count) =
            super::super::repository::list_indexed_tasks(&allowed, &filter).expect("tail page");
        assert_eq!(final_count, 1);
        assert_eq!(final_page[0].snapshot.title, "Committed after snapshot");
        assert_eq!(final_page[0].snapshot.status, "review");
        let summary =
            super::super::repository::task_aggregate_summary(&allowed, &project_id, &filter)
                .expect("tail summary");
        assert_eq!(summary.review, 1);
        assert_eq!(summary.in_progress, 0);
        assert_eq!(summary.own_total, 1);
        assert_eq!(summary.applied_revision, tailed.applied_revision);
    }
}
