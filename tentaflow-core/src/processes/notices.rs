// ============ File: notices.rs — BPMN B4.3 incident notices: queued by the database, pushed as private live events ============

use std::collections::HashMap;

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
pub(crate) const PASS_SIZE: u32 = 200;
/// A person gets at most this many individual notices per window; the rest
/// become summaries, so a mass failure is one toast and a slow trickle of
/// failures cannot fill the screen either.
pub(crate) const SINGLE_NOTICE_LIMIT: usize = 3;
pub(crate) const THROTTLE_WINDOW_MS: i64 = 30_000;
/// A notice this old is not pushed: the person was away for days and the
/// attention list is the record of what is still open.
pub(crate) const STALE_AFTER_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Most live people one pass looks for.
const LIVE_USERS_PER_PASS: usize = 500;
const PROCESS_NAME_CHARS: usize = 120;

/// What each person was sent individually lately, kept by the worker.
#[derive(Default)]
pub struct NoticeThrottle {
    sent: HashMap<String, Vec<i64>>,
}

impl NoticeThrottle {
    fn budget(&mut self, user_id: &str, now_ms: i64) -> usize {
        let recent = self.sent.entry(user_id.to_string()).or_default();
        recent.retain(|at| now_ms - *at < THROTTLE_WINDOW_MS);
        SINGLE_NOTICE_LIMIT.saturating_sub(recent.len())
    }

    fn record(&mut self, user_id: &str, now_ms: i64, count: usize) {
        self.sent
            .entry(user_id.to_string())
            .or_default()
            .extend(std::iter::repeat_n(now_ms, count));
    }

    fn forget_idle(&mut self, now_ms: i64) {
        self.sent
            .retain(|_, recent| recent.iter().any(|at| now_ms - *at < THROTTLE_WINDOW_MS));
    }
}

struct PendingNotice {
    incident_id: String,
    user_id: String,
    instance_id: String,
    org_id: String,
    node_id: Option<String>,
    code: String,
    resolved: bool,
    process_name: String,
    queued_at_ms: i64,
}

/// Pushes every queued notice whose incident is still open to a recipient who
/// can read the instance in the organization the dashboard opens for them, and
/// settles the rest without a push. The text is the incident code and the
/// process name only: no variable value, no worker failure text and no
/// incident message ever reaches the event, because the event travels outside
/// the gate that redacts for a monitor. A row whose check fails is settled as
/// suppressed, so one broken row never blocks the queue. A push is sent before
/// its rows are settled, so a crash in between repeats it; the notification
/// id is derived from the pair, so the dashboard drops the repeat.
///
/// Only people with a settled live socket are looked at: a notice for anyone
/// else stays queued and is pushed when they connect, because a push to nobody
/// is lost. A notice older than a week is settled without a push. An idle pass
/// reads one index and never takes the writer lock. Returns the number of
/// events pushed.
pub fn deliver_pending(
    db: &DbPool,
    now_ms: i64,
    live_users: &[String],
    throttle: &mut NoticeThrottle,
    publish: &dyn Fn(SystemEventPayload),
) -> Result<usize> {
    if live_users.is_empty() {
        return Ok(0);
    }
    let pending = {
        let conn = db.read()?;
        if is_private_simulation(&conn)? || !has_pending(&conn)? {
            return Ok(0);
        }
        pending_notices(&conn, live_users, now_ms)?
    };
    let mut default_orgs: HashMap<String, Option<String>> = HashMap::new();
    let mut deliverable: Vec<(usize, bool)> = Vec::with_capacity(pending.len());
    for (index, notice) in pending.iter().enumerate() {
        let verdict = if notice.resolved || now_ms - notice.queued_at_ms > STALE_AFTER_MS {
            Ok(false)
        } else {
            let in_default_org = default_orgs
                .entry(notice.user_id.clone())
                .or_insert_with(|| default_org(db, &notice.user_id))
                .as_deref()
                == Some(notice.org_id.as_str());
            if in_default_org {
                db.read().map_err(anyhow::Error::from).and_then(|conn| {
                    super::repository::incident_notice_reader_on(
                        &conn,
                        &notice.org_id,
                        &notice.user_id,
                        &notice.instance_id,
                    )
                })
            } else {
                Ok(false)
            }
        };
        match verdict {
            Ok(deliver) => deliverable.push((index, deliver)),
            Err(error) => {
                tracing::warn!(
                    error_len = error.to_string().len(),
                    "process incident notice check failed"
                );
                deliverable.push((index, false));
            }
        }
    }

    let mut groups: HashMap<(&str, &str), Vec<&PendingNotice>> = HashMap::new();
    for (index, deliver) in &deliverable {
        if *deliver {
            let notice = &pending[*index];
            groups
                .entry((notice.user_id.as_str(), notice.org_id.as_str()))
                .or_default()
                .push(notice);
        }
    }
    let mut pushed = 0;
    for ((user_id, org_id), notices) in &groups {
        let single = notices.len().min(throttle.budget(user_id, now_ms));
        for notice in &notices[..single] {
            publish(event(notice));
            pushed += 1;
        }
        throttle.record(user_id, now_ms, single);
        if notices.len() > single {
            publish(summary_event(user_id, org_id, &notices[single..]));
            pushed += 1;
        }
    }
    throttle.forget_idle(now_ms);

    let mut conn = db.write()?;
    let tx = conn.transaction()?;
    for (index, deliver) in &deliverable {
        let notice = &pending[*index];
        tx.execute(
            "UPDATE bpmn_incident_notices SET notified_at_ms=?1, suppressed=?2 WHERE incident_id=?3 AND user_id=?4",
            params![now_ms, i64::from(!*deliver), notice.incident_id, notice.user_id],
        )?;
    }
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

fn has_pending(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_incident_notices WHERE notified_at_ms IS NULL)",
        [],
        |row| row.get(0),
    )?)
}

/// The organization the dashboard opens for this person, which is where a toast
/// can lead them to the run. A person in several organizations is not paged
/// about the others; the attention list is the trace there.
fn default_org(db: &DbPool, user_id: &str) -> Option<String> {
    crate::services::rbac::middleware::resolve_org_context(db, user_id, None)
        .ok()
        .map(|context| context.org_id)
}

fn pending_notices(
    conn: &Connection,
    live_users: &[String],
    now_ms: i64,
) -> Result<Vec<PendingNotice>> {
    let live = &live_users[..live_users.len().min(LIVE_USERS_PER_PASS)];
    let marks = vec!["?"; live.len()].join(",");
    let mut statement = conn.prepare(&format!(
        "SELECT n.incident_id,n.user_id,x.instance_id,i.org_id,x.node_id,x.code,x.resolved_at_ms IS NOT NULL,d.name,n.queued_at_ms \
         FROM bpmn_incident_notices n \
         JOIN bpmn_incidents x ON x.incident_id=n.incident_id \
         JOIN bpmn_instances i ON i.instance_id=x.instance_id \
         JOIN bpmn_definitions d ON d.definition_id=i.definition_id \
         WHERE n.notified_at_ms IS NULL AND (n.user_id IN ({marks}) OR n.queued_at_ms < ?) \
         ORDER BY n.queued_at_ms,n.incident_id,n.user_id LIMIT {PASS_SIZE}"
    ))?;
    let mut values: Vec<rusqlite::types::Value> =
        live.iter().map(|user| user.clone().into()).collect();
    values.push((now_ms - STALE_AFTER_MS).into());
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(PendingNotice {
                incident_id: row.get(0)?,
                user_id: row.get(1)?,
                instance_id: row.get(2)?,
                org_id: row.get(3)?,
                node_id: row.get(4)?,
                code: row.get(5)?,
                resolved: row.get(6)?,
                process_name: row.get(7)?,
                queued_at_ms: row.get(8)?,
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
            "orgId": notice.org_id,
            "nodeId": notice.node_id,
            "code": notice.code,
            "processName": name,
        })
        .to_string(),
    }
}

fn summary_event(user_id: &str, org_id: &str, notices: &[&PendingNotice]) -> SystemEventPayload {
    SystemEventPayload::UserNotification {
        user_id: user_id.to_string(),
        notification_id: format!(
            "process-incident-batch:{}:{}",
            notices[0].incident_id, user_id
        ),
        project_id: String::new(),
        kind: INCIDENT_NOTICE_KIND.to_string(),
        title: String::new(),
        body: String::new(),
        link_json: json!({ "summary": true, "count": notices.len(), "orgId": org_id }).to_string(),
    }
}
