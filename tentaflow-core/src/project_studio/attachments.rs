// ============ File: project_studio/attachments.rs — canonical owners of stored attachment blobs ============

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use super::media::AttachmentOwner;
use crate::db::DbPool;
use tentaflow_protocol::project_studio::{AttachmentOwnerKind, AttachmentWire};

pub fn attachment_owners(
    pool: &DbPool,
    sha256: &str,
    excluded_task_ids: &HashSet<String>,
) -> Result<Vec<AttachmentOwner>> {
    let mut owners = Vec::new();
    let task_ids: Vec<String> = {
        let conn = pool
            .read()
            .map_err(|error| anyhow::anyhow!("attachment owner read: {error}"))?;
        let mut stmt = conn.prepare(
            "SELECT task_id FROM tasks UNION SELECT DISTINCT e.task_id FROM task_events e \
             WHERE e.kind IN ('created','attachments_json') ORDER BY task_id",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let task_ids = rows.collect::<rusqlite::Result<_>>()?;
        task_ids
    };
    for task_id in task_ids {
        if excluded_task_ids.contains(&task_id) {
            continue;
        }
        if super::tasks::task_attachment_inventory(pool, &task_id)?
            .iter()
            .any(|item| item.sha256 == sha256)
        {
            owners.push(AttachmentOwner {
                kind: AttachmentOwnerKind::Task,
                id: task_id,
                step_index: None,
            });
        }
    }
    let conn = pool
        .read()
        .map_err(|error| anyhow::anyhow!("attachment owner read: {error}"))?;
    for (kind, table, id_col) in [
        (AttachmentOwnerKind::Case, "test_cases", "case_id"),
        (AttachmentOwnerKind::RunItem, "test_run_items", "item_id"),
        (AttachmentOwnerKind::RunStep, "test_run_steps", "item_id"),
    ] {
        let sql = if kind == AttachmentOwnerKind::RunStep {
            format!("SELECT {id_col},step_index,attachments_json FROM {table}")
        } else {
            format!("SELECT {id_col},NULL,attachments_json FROM {table}")
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<u32>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, step_index, raw) = row?;
            let attachments: Vec<AttachmentWire> = serde_json::from_str(&raw)?;
            if attachments
                .iter()
                .any(|attachment| attachment.sha256 == sha256)
            {
                owners.push(AttachmentOwner {
                    kind,
                    id,
                    step_index,
                });
            }
        }
    }
    Ok(owners)
}
