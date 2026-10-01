// ===== File: project_studio/notifications.rs — personal notifications (central projects.db) =====
//
// Rows live in the CENTRAL registry (`notifications` table, schema V1) —
// notifications outlive per-project databases and the bell endpoint has no
// project scope. PRIVACY INVARIANT: every read/write here filters by the
// authenticated `user_id`; the push event is additionally filtered per
// connection in ws_binary. Anti-spam (risk F.7): one aggregate notification
// per (user, run), sender skipped, and an UNREAD duplicate of the same
// (kind, link_json) is never inserted twice.

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use super::models::NotificationRecord;
use crate::dispatch::HandlerContext;
use tentaflow_protocol::project_studio::access::ProjectPermissionLevel;

pub fn task_reader(ctx: &HandlerContext, project_id: &str, user_id: &str) -> bool {
    if !crate::db::repository::get_user_account_by_id(&ctx.state.db, user_id)
        .ok()
        .flatten()
        .is_some_and(|user| user.is_active)
    {
        return false;
    }
    let Ok(org) = crate::services::rbac::resolve_org_context(
        &ctx.state.db,
        user_id,
        ctx.org_context.as_ref().map(|org| org.org_id.as_str()),
    ) else {
        return false;
    };
    let mut recipient = ctx.clone();
    recipient.org_context = Some(org);
    crate::dispatch::project_studio::require_read(&recipient)
        .and_then(|org| {
            crate::dispatch::project_studio::require_task_access(
                &recipient,
                org,
                project_id,
                ProjectPermissionLevel::Read,
            )
        })
        .is_ok()
}

pub fn notify_task_changes(
    ctx: &HandlerContext,
    project_id: &str,
    mutation: &super::tasks::TaskMutation,
) {
    if !mutation.changed {
        return;
    }
    let Some(org) = ctx.org_context.as_ref() else {
        return;
    };
    let result = (|| -> Result<()> {
        let pool = super::project_db::open(project_id).map_err(|e| anyhow!(e))?;
        let Some(task) = super::tasks::get_task(&pool, &mutation.task_id)? else {
            return Ok(());
        };
        let mut notified = std::collections::HashSet::new();
        let event_for = |kind: &str| -> Result<Option<i64>> {
            let conn = pool.read().map_err(read_err)?;
            for id in &mutation.event_ids {
                let event_kind: Option<String> = conn
                    .query_row(
                        "SELECT kind FROM task_events WHERE event_id = ?1 AND task_id = ?2",
                        params![id, mutation.task_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if event_kind.as_deref() == Some(kind) {
                    return Ok(Some(*id));
                }
            }
            Ok(None)
        };
        if let Some(previous) = mutation
            .previous_status
            .as_deref()
            .filter(|old| *old != mutation.current_status)
        {
            let event_id = mutation
                .status_event_id
                .ok_or_else(|| anyhow!("status mutation has no committed event"))?;
            for user in [&mutation.current_assignee, &task.created_by] {
                if user.is_empty()
                    || user == &org.user_id
                    || !notified.insert(user.clone())
                    || !task_reader(ctx, project_id, user)
                {
                    continue;
                }
                notify(&org.org_id, user, project_id, "task_status_changed", "Zmieniono status zadania",
                    &format!("{} „{}”: {} → {}", task.task_key, task.title, previous, mutation.current_status),
                    &serde_json::json!({"project_id":project_id,"task_id":task.task_id,"task_key":task.task_key,"task_title":task.title,"from_status":previous,"to_status":mutation.current_status,"event_id":event_id}).to_string());
            }
        }
        let assignment_event = event_for("assigned")?
            .or(event_for("reassigned")?)
            .or(event_for("unassigned")?)
            .or(event_for("created")?);
        if let Some(event_id) = assignment_event {
            let previous = mutation.previous_assignee.as_deref().unwrap_or("");
            let previous_kind = if mutation.current_assignee.is_empty() {
                "task_unassigned"
            } else {
                "task_reassigned"
            };
            let mut assignment_notified = std::collections::HashSet::new();
            for (recipient, kind, title) in [
                (
                    mutation.current_assignee.as_str(),
                    "task_assigned",
                    "Przypisano Ci zadanie",
                ),
                (previous, previous_kind, "Zmieniono przypisanie zadania"),
            ] {
                if recipient.is_empty()
                    || recipient == org.user_id
                    || !assignment_notified.insert(recipient.to_string())
                    || !task_reader(ctx, project_id, recipient)
                {
                    continue;
                }
                notify(&org.org_id, recipient, project_id, kind, title,
                    &format!("{} „{}”", task.task_key, task.title),
                    &serde_json::json!({"project_id":project_id,"task_id":task.task_id,"task_key":task.task_key,"task_title":task.title,"event_id":event_id,"from_user_id":mutation.previous_assignee,"to_user_id":mutation.current_assignee}).to_string());
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "task change notification failed");
    }
}

pub fn notify_mentions(
    ctx: &HandlerContext,
    project_id: &str,
    task_id: &str,
    mutation: &super::tasks::CommentMutation,
) {
    if !mutation.changed {
        return;
    }
    let (Some(org), Some(comment), Some(event_id)) = (
        ctx.org_context.as_ref(),
        mutation.comment.as_ref(),
        mutation.event_id,
    ) else {
        return;
    };
    let result = (|| -> Result<()> {
        let pool = super::project_db::open(project_id).map_err(|e| anyhow!(e))?;
        let Some(task) = super::tasks::get_task(&pool, task_id)? else {
            return Ok(());
        };
        for user in &mutation.added_mention_user_ids {
            if user == &org.user_id || !task_reader(ctx, project_id, user) {
                continue;
            }
            notify(&org.org_id, user, project_id, "task_mentioned", "Wspomniano Cię w zadaniu",
                &format!("{} „{}”", task.task_key, task.title),
                &serde_json::json!({"project_id":project_id,"task_id":task_id,"task_key":task.task_key,"task_title":task.title,"comment_id":comment.comment_id,"event_id":event_id}).to_string());
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "task mention notification failed");
    }
}

pub fn notify_task_handover(
    org_id: &str,
    actor: &str,
    project_id: &str,
    mutation: &super::tasks::TaskMutation,
    direction: super::tasks::TaskHandoverDirection,
    readable: &dyn Fn(&str) -> bool,
) {
    if !mutation.changed {
        return;
    }
    let result = (|| -> Result<()> {
        let pool = super::project_db::open(project_id).map_err(|e| anyhow!(e))?;
        let Some(task) = super::tasks::get_task(&pool, &mutation.task_id)? else {
            return Ok(());
        };
        let mut sent = std::collections::HashSet::new();
        let kind = match direction {
            super::tasks::TaskHandoverDirection::Over => "task_handed_over",
            super::tasks::TaskHandoverDirection::Back => "task_handed_back",
        };
        let title = match direction {
            super::tasks::TaskHandoverDirection::Over => "Przekazano zadanie",
            super::tasks::TaskHandoverDirection::Back => "Cofnięto przekazanie zadania",
        };
        let link = serde_json::json!({"project_id":project_id,"task_id":task.task_id,"task_key":task.task_key,"task_title":task.title,
            "event_id":mutation.event_ids.last(),"comment_id":mutation.handover_comment_id}).to_string();
        for user in [
            mutation.previous_assignee.as_deref().unwrap_or(""),
            mutation.current_assignee.as_str(),
        ] {
            if user.is_empty() || user == actor || !sent.insert(user.to_string()) || !readable(user)
            {
                continue;
            }
            notify(
                org_id,
                user,
                project_id,
                kind,
                title,
                &format!("{} „{}”", task.task_key, task.title),
                &link,
            );
        }
        if let Some(comment_id) = &mutation.handover_comment_id {
            if let Some(comment) = super::tasks::get_comment(&pool, comment_id)? {
                let mentions: Vec<String> = serde_json::from_str(&comment.mention_user_ids_json)?;
                for user in mentions {
                    if user == actor || !sent.insert(user.clone()) || !readable(&user) {
                        continue;
                    }
                    notify(
                        org_id,
                        &user,
                        project_id,
                        "task_mentioned",
                        "Wspomniano Cię w zadaniu",
                        &format!("{} „{}”", task.task_key, task.title),
                        &link,
                    );
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "task handover notification failed");
    }
}

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio notifications read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio notifications write: {e}")
}

/// Inserts one notification (unless an unread duplicate of the same user +
/// kind + link exists) and pushes the live `UserNotification` system event.
/// Best-effort by design: a failed notification must never fail the mutation
/// it announces, so errors are logged and swallowed.
#[allow(clippy::too_many_arguments)]
pub fn notify(
    org_id: &str,
    user_id: &str,
    project_id: &str,
    kind: &str,
    title: &str,
    body: &str,
    link_json: &str,
) {
    match insert_deduped(org_id, user_id, project_id, kind, title, body, link_json) {
        Ok(Some(notification_id)) => {
            crate::dispatch::system_event_broadcast::publish(
                tentaflow_protocol::SystemEventPayload::UserNotification {
                    user_id: user_id.to_string(),
                    notification_id,
                    project_id: project_id.to_string(),
                    kind: kind.to_string(),
                    title: title.to_string(),
                    body: body.to_string(),
                    link_json: link_json.to_string(),
                },
            );
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(kind, "notification insert failed: {e}"),
    }
}

/// Returns the new notification id, or `None` when an unread duplicate
/// (same user + kind + link) already exists.
fn insert_deduped(
    org_id: &str,
    user_id: &str,
    project_id: &str,
    kind: &str,
    title: &str,
    body: &str,
    link_json: &str,
) -> Result<Option<String>> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let duplicate: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM notifications \
             WHERE user_id = ?1 AND kind = ?2 AND link_json = ?3 AND read_at IS NULL LIMIT 1",
            params![user_id, kind, link_json],
            |row| row.get(0),
        )
        .optional()?;
    if duplicate.is_some() {
        return Ok(None);
    }
    let notification_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO notifications (notification_id, org_id, user_id, project_id, kind, \
            title, body, link_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            notification_id,
            org_id,
            user_id,
            project_id,
            kind,
            title,
            body,
            link_json
        ],
    )?;
    Ok(Some(notification_id))
}

/// Lists the caller's notifications newest-first with rowid keyset pagination
/// (`before_id` = notification_id of the previous page's last row). Returns
/// `(rows, unread_count, has_more)`. Project names resolve from the central
/// `projects` table in the same database.
pub fn list(
    user_id: &str,
    only_unread: bool,
    before_id: Option<&str>,
    limit: u32,
) -> Result<(Vec<NotificationRecord>, u32, bool)> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let before_rowid: Option<i64> = match before_id {
        Some(id) => conn
            .query_row(
                "SELECT rowid FROM notifications WHERE notification_id = ?1 AND user_id = ?2",
                params![id, user_id],
                |row| row.get(0),
            )
            .optional()?,
        None => None,
    };
    let unread_filter = if only_unread {
        "AND n.read_at IS NULL"
    } else {
        ""
    };
    let fetch = (limit as i64) + 1;
    let mut stmt = conn.prepare(&format!(
        "SELECT n.notification_id, n.project_id, COALESCE(p.name, ''), n.kind, n.title, \
                n.body, n.link_json, n.read_at, n.created_at \
         FROM notifications n LEFT JOIN projects p ON p.project_id = n.project_id \
         WHERE n.user_id = ?1 AND (?2 IS NULL OR n.rowid < ?2) {unread_filter} \
         ORDER BY n.rowid DESC LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![user_id, before_rowid, fetch], |row| {
        Ok(NotificationRecord {
            notification_id: row.get(0)?,
            project_id: row.get(1)?,
            project_name: row.get(2)?,
            kind: row.get(3)?,
            title: row.get(4)?,
            body: row.get(5)?,
            link_json: row.get(6)?,
            read_at: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    let mut entries = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = entries.len() as i64 > limit as i64;
    entries.truncate(limit as usize);
    let unread: i64 = conn.query_row(
        "SELECT COUNT(*) FROM notifications WHERE user_id = ?1 AND read_at IS NULL",
        params![user_id],
        |row| row.get(0),
    )?;
    Ok((entries, unread as u32, has_more))
}

/// Marks the given notifications read; an empty list marks ALL of the
/// caller's unread rows. Always caller-scoped.
pub fn mark_read(user_id: &str, notification_ids: &[String]) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    if notification_ids.is_empty() {
        conn.execute(
            "UPDATE notifications SET read_at = datetime('now') \
             WHERE user_id = ?1 AND read_at IS NULL",
            params![user_id],
        )?;
    } else {
        for id in notification_ids {
            conn.execute(
                "UPDATE notifications SET read_at = datetime('now') \
                 WHERE user_id = ?1 AND notification_id = ?2 AND read_at IS NULL",
                params![user_id, id],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    /// (f) Every query is caller-scoped: user B never sees user A's rows, and
    /// mark_read cannot cross users. Also covers the unread (kind, link) dedup.
    #[test]
    fn notifications_are_user_scoped_and_deduped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));

        let ua = format!("user-a-{}", uuid::Uuid::new_v4());
        let ub = format!("user-b-{}", uuid::Uuid::new_v4());
        let link = r#"{"run_id":"r1"}"#;
        let first = insert_deduped("org-t", &ua, "p1", "run_item_assigned", "T", "B", link)
            .expect("insert");
        assert!(first.is_some());
        // Unread duplicate of the same (user, kind, link) is suppressed.
        let dup = insert_deduped("org-t", &ua, "p1", "run_item_assigned", "T", "B", link)
            .expect("dup insert");
        assert!(dup.is_none());
        // A DIFFERENT user with the same kind+link still gets their own row.
        let other = insert_deduped("org-t", &ub, "p1", "run_item_assigned", "T", "B", link)
            .expect("other insert");
        assert!(other.is_some());

        let (rows_a, unread_a, _) = list(&ua, false, None, 50).expect("list a");
        assert_eq!(rows_a.len(), 1);
        assert_eq!(unread_a, 1);
        let (rows_b, unread_b, _) = list(&ub, false, None, 50).expect("list b");
        assert_eq!(rows_b.len(), 1);
        assert_eq!(unread_b, 1);
        assert_ne!(
            rows_a[0].notification_id, rows_b[0].notification_id,
            "rows are private per user"
        );

        // Marking B's id as A must not touch B's row.
        mark_read(&ua, &[rows_b[0].notification_id.clone()]).expect("cross mark");
        let (_, unread_b, _) = list(&ub, false, None, 50).expect("list b again");
        assert_eq!(unread_b, 1, "user A cannot mark user B's notification");

        // Marking all as A clears only A.
        mark_read(&ua, &[]).expect("mark all");
        let (_, unread_a, _) = list(&ua, false, None, 50).expect("list a again");
        assert_eq!(unread_a, 0);
        // After the read, the same (kind, link) may notify again.
        let again = insert_deduped("org-t", &ua, "p1", "run_item_assigned", "T", "B", link)
            .expect("re-insert");
        assert!(again.is_some());
    }
}
