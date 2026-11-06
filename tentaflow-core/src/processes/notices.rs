// ============ File: notices.rs — BPMN B4.3 incident notices: queued by the database, pushed as private live events ============

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::json;
use tentaflow_protocol::SystemEventPayload;

use crate::db::DbPool;

/// `UserNotification.kind` of a process incident. The dashboard builds the
/// sentence from the code in the link, so the title and body only carry the
/// untranslated fallback.
pub const INCIDENT_NOTICE_KIND: &str = "process_incident";

/// Notices settled per pass; a larger backlog is drained over the next passes.
const PASS_SIZE: u32 = 50;
const PROCESS_NAME_CHARS: usize = 120;

struct PendingNotice {
    incident_id: String,
    user_id: String,
    instance_id: String,
    org_id: String,
    node_id: Option<String>,
    code: String,
    resolved: bool,
    process_name: String,
}

/// Pushes every queued notice whose incident is still open to a recipient who
/// can read the instance, and settles the rest without a push. The notice text
/// is the incident code and the process name only: no variable value, no
/// worker failure text and no incident message ever reaches the event, because
/// the recipient may read the instance but the event travels outside the gate
/// that redacts for a monitor. A push is sent before its row is settled, so a
/// crash in between repeats it; the notification id is derived from the pair,
/// so the dashboard drops the repeat. Returns the number of pushes.
pub fn deliver_pending(
    db: &DbPool,
    now_ms: i64,
    publish: &dyn Fn(SystemEventPayload),
) -> Result<usize> {
    let mut conn = db.write()?;
    if is_private_simulation(&conn)? {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    let pending = pending_notices(&tx)?;
    let mut pushed = 0;
    for notice in &pending {
        let deliver = !notice.resolved
            && super::repository::incident_notice_reader_on(
                &tx,
                &notice.org_id,
                &notice.user_id,
                &notice.instance_id,
            )?;
        if deliver {
            publish(event(notice));
            pushed += 1;
        }
        tx.execute(
            "UPDATE bpmn_incident_notices SET notified_at_ms=?1, suppressed=?2 WHERE incident_id=?3 AND user_id=?4",
            params![now_ms, i64::from(!deliver), notice.incident_id, notice.user_id],
        )?;
    }
    // A notice whose incident is gone (the instance was purged) has nothing left to say.
    tx.execute(
        "DELETE FROM bpmn_incident_notices WHERE NOT EXISTS(SELECT 1 FROM bpmn_incidents x WHERE x.incident_id=bpmn_incident_notices.incident_id)",
        [],
    )?;
    tx.commit()?;
    Ok(pushed)
}

fn is_private_simulation(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='simulation_meta')",
        [],
        |row| row.get(0),
    )?)
}

fn pending_notices(conn: &Connection) -> Result<Vec<PendingNotice>> {
    let mut statement = conn.prepare(
        "SELECT n.incident_id,n.user_id,x.instance_id,i.org_id,x.node_id,x.code,x.resolved_at_ms IS NOT NULL,d.name \
         FROM bpmn_incident_notices n \
         JOIN bpmn_incidents x ON x.incident_id=n.incident_id \
         JOIN bpmn_instances i ON i.instance_id=x.instance_id \
         JOIN bpmn_definitions d ON d.definition_id=i.definition_id \
         WHERE n.notified_at_ms IS NULL ORDER BY n.queued_at_ms,n.incident_id,n.user_id LIMIT ?1",
    )?;
    let rows = statement
        .query_map([PASS_SIZE], |row| {
            Ok(PendingNotice {
                incident_id: row.get(0)?,
                user_id: row.get(1)?,
                instance_id: row.get(2)?,
                org_id: row.get(3)?,
                node_id: row.get(4)?,
                code: row.get(5)?,
                resolved: row.get(6)?,
                process_name: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn event(notice: &PendingNotice) -> SystemEventPayload {
    let name: String = notice
        .process_name
        .chars()
        .take(PROCESS_NAME_CHARS)
        .collect();
    SystemEventPayload::UserNotification {
        user_id: notice.user_id.clone(),
        notification_id: format!("process-incident:{}:{}", notice.incident_id, notice.user_id),
        project_id: String::new(),
        kind: INCIDENT_NOTICE_KIND.to_string(),
        title: name.clone(),
        body: notice.code.clone(),
        link_json: json!({
            "instanceId": notice.instance_id,
            "incidentId": notice.incident_id,
            "nodeId": notice.node_id,
            "code": notice.code,
            "processName": name,
        })
        .to_string(),
    }
}
