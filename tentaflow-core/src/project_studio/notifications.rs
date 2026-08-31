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
